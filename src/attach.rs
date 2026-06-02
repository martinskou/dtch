use std::{
    ffi::CStr,
    fs,
    io::{self, Read, Write},
    net::Shutdown,
    os::fd::{AsFd, AsRawFd, OwnedFd, RawFd},
    os::unix::fs::MetadataExt,
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use nix::sys::termios::{SetArg, Termios, cfmakeraw, tcgetattr, tcsetattr};

use crate::{
    protocol::{Request, WindowSize},
    registry::socket_path,
    session::start_session,
};

const DETACH_BYTE: u8 = 0x05; // Ctrl-E
const RESTORE_TERMINAL_MODES: &[u8] =
    b"\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1004l\x1b[?1006l\x1b[?2004l\x1b[?1l\x1b>\x1b[0m\x1b[?25h";

/// Attaches to a session, creating it first when requested and necessary.
pub(crate) fn run_attach_or_create(
    name: String,
    create: bool,
    buffer_lines: usize,
    command: Vec<String>,
) -> Result<()> {
    let socket = socket_path(&name)?;

    if !command.is_empty() && !create {
        bail!("a command can only be provided with `attach -c`");
    }

    if !socket.exists() {
        if !create {
            bail!("session does not exist: {}", socket.display());
        }

        if command.is_empty() {
            bail!("attach -c requires a command when creating a session");
        }

        let window_size = read_window_size(io::stdin().as_raw_fd())?;
        start_session(
            name.clone(),
            socket.clone(),
            buffer_lines,
            command,
            Some(window_size),
        )?;
    }

    print_socket_mtime(&socket)?;
    println!("dtch: attach with `dtch attach {name}`");
    println!("dtch: detach from clients with Ctrl-E");

    run_attach(socket)
}

/// Prints the socket modification time as a local datetime with timezone.
fn print_socket_mtime(socket: &Path) -> Result<()> {
    let timestamp = fs::metadata(socket)
        .with_context(|| format!("failed to read metadata for {}", socket.display()))?
        .mtime();
    let mut local_time = unsafe { std::mem::zeroed() };
    let local_time = unsafe { libc::localtime_r(&timestamp, &mut local_time) };
    if local_time.is_null() {
        return Err(io::Error::last_os_error()).context("failed to format socket mtime");
    }

    let mut formatted = [0_i8; 64];
    let format = c"%Y-%m-%d %H:%M:%S %z";
    let len = unsafe {
        libc::strftime(
            formatted.as_mut_ptr(),
            formatted.len(),
            format.as_ptr(),
            local_time,
        )
    };
    if len == 0 {
        bail!("failed to format socket mtime");
    }

    let formatted = unsafe { CStr::from_ptr(formatted.as_ptr()) };
    println!("dtch: socket mtime {}", formatted.to_string_lossy());
    Ok(())
}

/// Connects this terminal to the session socket and streams bytes in both directions.
fn run_attach(socket: PathBuf) -> Result<()> {
    let mut stream = UnixStream::connect(&socket)
        .with_context(|| format!("failed to connect to {}", socket.display()))?;
    let window_size = read_window_size(io::stdin().as_raw_fd())?;
    Request::Attach(window_size)
        .write_to(&mut stream)
        .context("failed to send terminal size")?;
    let _raw_mode = RawMode::enable(io::stdin().as_fd())?;
    let resize_monitor = ResizeMonitor::start(socket, window_size);

    let mut input_stream = stream
        .try_clone()
        .context("failed to clone socket for input")?;
    let _stdin_thread = thread::spawn(move || copy_stdin_to_socket(&mut input_stream));

    let mut output_stream = stream;
    let result = copy_socket_to_stdout(&mut output_stream);
    drop(resize_monitor);
    result
}

/// Reads the terminal dimensions reported by the kernel for a tty file descriptor.
pub(crate) fn read_window_size(fd: RawFd) -> Result<WindowSize> {
    let mut size = libc::winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let rc = unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut size) };
    if rc == -1 {
        return Err(io::Error::last_os_error()).context("failed to read terminal size");
    }

    Ok(WindowSize {
        rows: size.ws_row,
        cols: size.ws_col,
        xpixel: size.ws_xpixel,
        ypixel: size.ws_ypixel,
    })
}

struct ResizeMonitor {
    running: Arc<AtomicBool>,
}

impl ResizeMonitor {
    /// Starts a polling thread that forwards terminal size changes to the session.
    fn start(socket: PathBuf, initial_size: WindowSize) -> Self {
        let running = Arc::new(AtomicBool::new(true));
        let monitor_running = Arc::clone(&running);
        thread::spawn(move || {
            let mut last_size = initial_size;
            while monitor_running.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_millis(100));
                let Ok(size) = read_window_size(libc::STDIN_FILENO) else {
                    continue;
                };
                if size == last_size {
                    continue;
                }
                last_size = size;

                let Ok(mut stream) = UnixStream::connect(&socket) else {
                    continue;
                };
                let _ = Request::Resize(size).write_to(&mut stream);
            }
        });

        Self { running }
    }
}

impl Drop for ResizeMonitor {
    /// Signals the polling thread to stop after the current sleep interval.
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
    }
}

/// Copies local keystrokes to the session until EOF or the detach byte is received.
fn copy_stdin_to_socket(stream: &mut UnixStream) -> io::Result<()> {
    let mut stdin = io::stdin().lock();
    let mut buf = [0_u8; 1024];

    loop {
        let len = stdin.read(&mut buf)?;
        if len == 0 {
            break;
        }

        if let Some(pos) = buf[..len].iter().position(|byte| *byte == DETACH_BYTE) {
            stream.write_all(&buf[..pos])?;
            let _ = stream.shutdown(Shutdown::Both);
            break;
        }

        stream.write_all(&buf[..len])?;
    }

    Ok(())
}

/// Writes session output to the local terminal until the socket closes.
fn copy_socket_to_stdout(stream: &mut UnixStream) -> Result<()> {
    let mut stdout = io::stdout().lock();
    let mut buf = [0_u8; 8192];

    loop {
        let len = match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(len) => len,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err).context("failed to read session output"),
        };

        stdout
            .write_all(&buf[..len])
            .context("failed to write terminal output")?;
        stdout.flush().context("failed to flush terminal output")?;
    }

    Ok(())
}

struct RawMode {
    fd: OwnedFd,
    original: Termios,
}

impl RawMode {
    /// Saves the current tty configuration and switches stdin to raw mode.
    fn enable<Fd: AsFd>(fd: Fd) -> Result<Self> {
        let fd = fd
            .as_fd()
            .try_clone_to_owned()
            .context("failed to clone tty fd")?;
        let original = tcgetattr(&fd).context("failed to read terminal settings")?;
        let mut raw = original.clone();
        cfmakeraw(&mut raw);
        tcsetattr(&fd, SetArg::TCSAFLUSH, &raw).context("failed to set raw terminal mode")?;

        Ok(Self { fd, original })
    }
}

impl Drop for RawMode {
    /// Restores tty settings and disables terminal modes a detached program may leave active.
    fn drop(&mut self) {
        let _ = tcsetattr(&self.fd, SetArg::TCSAFLUSH, &self.original);
        let _ = io::stdout().write_all(RESTORE_TERMINAL_MODES);
        let _ = io::stdout().flush();
    }
}
