use std::{
    collections::VecDeque,
    ffi::CString,
    fs,
    io::{self, Read, Write},
    os::{
        fd::{AsRawFd, OwnedFd},
        unix::net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    process,
    sync::{Arc, Mutex},
    thread,
};

use anyhow::{Context, Result, bail};
use nix::{
    pty::{ForkptyResult, Winsize, forkpty},
    sys::wait::{WaitPidFlag, WaitStatus, waitpid},
    unistd::{ForkResult, Pid, execvp, fork},
};

use crate::protocol::{Request, WindowSize};
use crate::{
    attach::read_window_size,
    registry::{restrict_socket_permissions, socket_path, verify_peer_uid},
};

/// Starts a detached session using the caller's terminal size when available.
pub(crate) fn run_new(name: String, buffer_lines: usize, command: Vec<String>) -> Result<()> {
    let initial_window_size = read_window_size(io::stdin().as_raw_fd()).ok();
    start_session(name, buffer_lines, command, initial_window_size)
}

/// Validates and binds a session socket, then forks the background server.
pub(crate) fn start_session(
    name: String,
    buffer_lines: usize,
    command: Vec<String>,
    initial_window_size: Option<WindowSize>,
) -> Result<()> {
    let socket = socket_path(&name)?;
    if socket.exists() {
        bail!("session already exists: {name} ({})", socket.display());
    }

    let listener = UnixListener::bind(&socket)
        .with_context(|| format!("failed to bind {}", socket.display()))?;
    let socket_cleanup = SocketCleanup::new(socket);
    restrict_socket_permissions(socket_cleanup.path())?;
    let argv = to_cstrings(&command)?;

    println!(
        "dtch: session `{name}` started at {}",
        socket_cleanup.path().display()
    );

    match unsafe { fork() }.context("failed to fork session server")? {
        ForkResult::Parent { .. } => {
            socket_cleanup.disarm();
            Ok(())
        }
        ForkResult::Child => {
            if let Err(err) = run_session_server(
                listener,
                socket_cleanup,
                argv,
                buffer_lines,
                initial_window_size,
            ) {
                eprintln!("dtch: {err:#}");
                process::exit(1);
            }

            process::exit(0);
        }
    }
}

/// Daemonizes, starts the command under a pty, and serves its parent process.
fn run_session_server(
    listener: UnixListener,
    socket_cleanup: SocketCleanup,
    argv: Vec<CString>,
    buffer_lines: usize,
    initial_window_size: Option<WindowSize>,
) -> Result<()> {
    daemonize().context("failed to daemonize session server")?;
    let initial_window_size = initial_window_size.map(to_pty_window_size);
    let forked =
        unsafe { forkpty(initial_window_size.as_ref(), None) }.context("failed to create pty")?;
    match forked {
        ForkptyResult::Child => {
            let err = execvp(&argv[0], &argv).expect_err("execvp unexpectedly returned");
            eprintln!("dtch: failed to exec {:?}: {err}", argv[0]);
            process::exit(127);
        }
        ForkptyResult::Parent { child, master } => {
            serve_session(listener, socket_cleanup, master, child, buffer_lines)
        }
    }
}

/// Detaches the server process while preserving its current working directory.
fn daemonize() -> io::Result<()> {
    #[allow(deprecated)]
    let rc = unsafe { libc::daemon(1, 0) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Accepts attach and resize requests while worker threads handle pty I/O.
fn serve_session(
    listener: UnixListener,
    socket_cleanup: SocketCleanup,
    master: OwnedFd,
    child: Pid,
    buffer_lines: usize,
) -> Result<()> {
    let master: fs::File = master.into();
    let input_master = Arc::new(Mutex::new(
        master.try_clone().context("failed to clone pty master")?,
    ));
    let clients = Arc::new(Mutex::new(Vec::<UnixStream>::new()));
    let line_buffer = Arc::new(Mutex::new(LineBuffer::new(buffer_lines)));

    let output_clients = Arc::clone(&clients);
    let output_buffer = Arc::clone(&line_buffer);
    thread::spawn(move || copy_pty_to_clients(master, output_clients, output_buffer));

    let reap_socket = socket_cleanup.path().to_owned();
    thread::spawn(move || {
        let _ = wait_for_child(child);
        let _ = fs::remove_file(reap_socket);
        process::exit(0);
    });

    for stream in listener.incoming() {
        let mut stream = match stream {
            Ok(stream) => stream,
            Err(err) => {
                eprintln!("dtch: accept failed: {err}");
                continue;
            }
        };
        if let Err(err) = verify_peer_uid(&stream) {
            eprintln!("dtch: {err}");
            continue;
        }
        let request = match Request::read_from(&mut stream) {
            Ok(request) => request,
            Err(err) => {
                eprintln!("dtch: invalid client request: {err}");
                continue;
            }
        };
        let size = match request {
            Request::Attach(size) => size,
            Request::Resize(size) => {
                if let Err(err) = set_window_size(&input_master, size) {
                    eprintln!("dtch: failed to resize pty: {err}");
                }
                continue;
            }
        };
        if let Err(err) = set_window_size(&input_master, size) {
            eprintln!("dtch: failed to resize pty: {err}");
        }

        let mut stream_for_output = stream
            .try_clone()
            .context("failed to clone client stream for output")?;

        let replay = line_buffer
            .lock()
            .expect("line buffer mutex poisoned")
            .bytes();
        if !replay.is_empty() {
            stream_for_output.write_all(&replay)?;
        }

        clients
            .lock()
            .expect("clients mutex poisoned")
            .push(stream_for_output.try_clone()?);
        force_redraw(&input_master, child);

        let input_master = Arc::clone(&input_master);
        thread::spawn(move || {
            let _ = copy_client_to_pty(stream, input_master);
        });
    }

    Ok(())
}

/// Removes a bound session socket unless ownership has moved to the daemon.
struct SocketCleanup {
    path: PathBuf,
    armed: bool,
}

impl SocketCleanup {
    fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn disarm(mut self) {
        self.armed = false;
    }
}

impl Drop for SocketCleanup {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, process};

    use super::SocketCleanup;

    fn cleanup_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("dtch-{name}-{}", process::id()))
    }

    #[test]
    fn socket_cleanup_removes_armed_path() {
        let path = cleanup_path("armed-cleanup-test");
        fs::write(&path, []).unwrap();

        {
            let _cleanup = SocketCleanup::new(path.clone());
        }

        assert!(!path.exists());
    }

    #[test]
    fn socket_cleanup_preserves_disarmed_path() {
        let path = cleanup_path("disarmed-cleanup-test");
        fs::write(&path, []).unwrap();

        SocketCleanup::new(path.clone()).disarm();

        assert!(path.exists());
        fs::remove_file(path).unwrap();
    }
}

/// Applies a client terminal size to the session pty.
fn set_window_size(master: &Mutex<fs::File>, size: WindowSize) -> io::Result<()> {
    let size = to_pty_window_size(size);
    let master = master.lock().expect("pty mutex poisoned");
    let rc = unsafe { libc::ioctl(master.as_raw_fd(), libc::TIOCSWINSZ, &size) };
    if rc == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Converts the wire representation into the type expected by `forkpty` and `ioctl`.
fn to_pty_window_size(size: WindowSize) -> Winsize {
    Winsize {
        ws_row: size.rows,
        ws_col: size.cols,
        ws_xpixel: size.xpixel,
        ws_ypixel: size.ypixel,
    }
}

/// Sends `SIGWINCH` to the foreground pty process group so interactive apps redraw.
fn force_redraw(master: &Mutex<fs::File>, child: Pid) {
    let master = master.lock().expect("pty mutex poisoned");
    let mut pgrp = 0;
    let rc = unsafe { libc::ioctl(master.as_raw_fd(), libc::TIOCGPGRP, &mut pgrp) };
    if rc == 0 && pgrp > 0 && unsafe { libc::kill(-pgrp, libc::SIGWINCH) } == 0 {
        return;
    }

    // forkpty creates a new session led by the child, which is also the
    // process group to notify when the platform cannot query the pty master.
    let _ = unsafe { libc::kill(-child.as_raw(), libc::SIGWINCH) };
}

/// Broadcasts pty output to attached clients and stores the configured line history.
fn copy_pty_to_clients(
    mut master: fs::File,
    clients: Arc<Mutex<Vec<UnixStream>>>,
    line_buffer: Arc<Mutex<LineBuffer>>,
) {
    let mut buf = [0_u8; 8192];

    loop {
        let len = match master.read(&mut buf) {
            Ok(0) => break,
            Ok(len) => len,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };

        line_buffer
            .lock()
            .expect("line buffer mutex poisoned")
            .push(&buf[..len]);

        let mut clients = clients.lock().expect("clients mutex poisoned");
        let mut i = 0;
        while i < clients.len() {
            if clients[i].write_all(&buf[..len]).is_err() {
                clients.remove(i);
            } else {
                i += 1;
            }
        }
    }
}

/// Copies input from one attached client into the shared pty master.
fn copy_client_to_pty(mut stream: UnixStream, master: Arc<Mutex<fs::File>>) -> io::Result<()> {
    let mut buf = [0_u8; 4096];

    loop {
        let len = match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(len) => len,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        };

        let mut pty = master.lock().expect("pty mutex poisoned");
        pty.write_all(&buf[..len])?;
    }

    Ok(())
}

struct LineBuffer {
    max_lines: usize,
    lines: VecDeque<Vec<u8>>,
    current: Vec<u8>,
}

impl LineBuffer {
    /// Creates a replay buffer capped at the requested number of complete lines.
    fn new(max_lines: usize) -> Self {
        Self {
            max_lines,
            lines: VecDeque::new(),
            current: Vec::new(),
        }
    }

    /// Appends output bytes, committing each newline-terminated line to history.
    fn push(&mut self, bytes: &[u8]) {
        if self.max_lines == 0 {
            return;
        }

        for byte in bytes {
            self.current.push(*byte);
            if *byte == b'\n' {
                self.push_current_line();
            }
        }
    }

    /// Returns complete retained lines followed by the current partial line.
    fn bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        for line in &self.lines {
            bytes.extend_from_slice(line);
        }
        bytes.extend_from_slice(&self.current);
        bytes
    }

    /// Moves the current line into history and removes lines beyond the configured cap.
    fn push_current_line(&mut self) {
        let line = std::mem::take(&mut self.current);
        self.lines.push_back(line);
        while self.lines.len() > self.max_lines {
            self.lines.pop_front();
        }
    }
}

/// Converts command arguments to the NUL-terminated representation required by `execvp`.
fn to_cstrings(args: &[String]) -> Result<Vec<CString>> {
    args.iter()
        .map(|arg| {
            CString::new(arg.as_str())
                .with_context(|| format!("argument contains an interior NUL byte: {arg:?}"))
        })
        .collect()
}

/// Reaps the command process and reports whether it exited or was terminated by a signal.
fn wait_for_child(child: Pid) -> Result<()> {
    loop {
        match waitpid(child, Some(WaitPidFlag::WUNTRACED)).context("failed to wait for child")? {
            WaitStatus::Exited(_, code) => {
                eprintln!("dtch: child exited with status {code}");
                return Ok(());
            }
            WaitStatus::Signaled(_, signal, _) => {
                eprintln!("dtch: child terminated by {signal:?}");
                return Ok(());
            }
            _ => {}
        }
    }
}
