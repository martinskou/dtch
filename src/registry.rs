use std::{
    ffi::OsStr,
    fs, io,
    os::{
        fd::AsRawFd,
        unix::{
            fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt},
            net::UnixStream,
        },
    },
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};

/// Finds session sockets in the current user's private runtime directory.
pub(crate) fn run_list() -> Result<()> {
    let mut sessions = Vec::new();
    let session_dir = session_dir()?;

    for entry in fs::read_dir(&session_dir)
        .with_context(|| format!("failed to read {}", session_dir.display()))?
    {
        let entry =
            entry.with_context(|| format!("failed to read {} entry", session_dir.display()))?;
        let file_type = entry
            .file_type()
            .with_context(|| format!("failed to read file type for {}", entry.path().display()))?;

        if !file_type.is_socket() {
            continue;
        }

        let Some(name) = session_name_from_file_name(&entry.file_name()) else {
            continue;
        };

        sessions.push((name, entry.path()));
    }

    sessions.sort_by(|left, right| left.0.cmp(&right.0));

    if sessions.is_empty() {
        println!("no active sessions");
        return Ok(());
    }

    println!("{:<24} SOCKET", "NAME");
    for (name, socket) in sessions {
        println!("{:<24} {}", name, socket.display());
    }

    Ok(())
}

/// Validates a session name and maps it to its Unix socket path.
pub(crate) fn socket_path(name: &str) -> Result<PathBuf> {
    if name.is_empty() {
        bail!("session name cannot be empty");
    }

    if !name
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        bail!("session name may only contain letters, numbers, '.', '_', and '-'");
    }

    Ok(session_dir()?.join(format!("{name}.sock")))
}

/// Restricts a newly bound socket to its owner.
pub(crate) fn restrict_socket_permissions(socket: &Path) -> Result<()> {
    fs::set_permissions(socket, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("failed to restrict permissions for {}", socket.display()))
}

/// Connects to a session and rejects sockets served by another user.
pub(crate) fn connect_to_session(socket: &Path) -> Result<UnixStream> {
    let stream = UnixStream::connect(socket)
        .with_context(|| format!("failed to connect to {}", socket.display()))?;
    verify_peer_uid(&stream)?;
    Ok(stream)
}

/// Rejects clients that are not running as the session owner's effective user.
pub(crate) fn verify_peer_uid(stream: &UnixStream) -> Result<()> {
    let expected = unsafe { libc::geteuid() };
    let actual = peer_uid(stream)?;
    if actual != expected {
        bail!("rejected Unix socket peer with uid {actual}; expected uid {expected}");
    }

    Ok(())
}

/// Creates and validates the private directory that prevents socket pre-creation attacks.
fn session_dir() -> Result<PathBuf> {
    let uid = unsafe { libc::geteuid() };
    let path = PathBuf::from(format!("/tmp/dtch-{uid}"));

    match fs::DirBuilder::new().mode(0o700).create(&path) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
        Err(err) => {
            return Err(err).with_context(|| format!("failed to create {}", path.display()));
        }
    }

    let metadata = fs::symlink_metadata(&path)
        .with_context(|| format!("failed to read metadata for {}", path.display()))?;
    if !metadata.file_type().is_dir() {
        bail!("session directory is not a directory: {}", path.display());
    }
    if metadata.uid() != uid {
        bail!(
            "session directory {} is owned by uid {}, expected uid {uid}",
            path.display(),
            metadata.uid()
        );
    }

    fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
        .with_context(|| format!("failed to restrict permissions for {}", path.display()))?;
    Ok(path)
}

#[cfg(any(
    target_vendor = "apple",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
))]
fn peer_uid(stream: &UnixStream) -> Result<libc::uid_t> {
    let mut uid = 0;
    let mut gid = 0;
    let rc = unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) };
    if rc == -1 {
        Err(io::Error::last_os_error()).context("failed to read Unix socket peer credentials")
    } else {
        Ok(uid)
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn peer_uid(stream: &UnixStream) -> Result<libc::uid_t> {
    let mut credentials = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut credentials as *mut libc::ucred).cast(),
            &mut len,
        )
    };
    if rc == -1 {
        Err(io::Error::last_os_error()).context("failed to read Unix socket peer credentials")
    } else {
        Ok(credentials.uid)
    }
}

#[cfg(not(any(
    target_vendor = "apple",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly",
    target_os = "linux",
    target_os = "android"
)))]
fn peer_uid(_stream: &UnixStream) -> Result<libc::uid_t> {
    bail!("Unix socket peer credential verification is unsupported on this platform")
}

/// Extracts a session name from a socket file name that follows the dtch convention.
fn session_name_from_file_name(file_name: &OsStr) -> Option<String> {
    let file_name = file_name.to_str()?;
    let name = file_name.strip_suffix(".sock")?;

    if name.is_empty() {
        return None;
    }

    Some(name.to_owned())
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        os::unix::{
            fs::{MetadataExt, PermissionsExt},
            net::UnixStream,
        },
    };

    use super::{socket_path, verify_peer_uid};

    #[test]
    fn session_directory_is_private_and_owned_by_current_user() {
        let socket = socket_path("permissions_test").unwrap();
        let metadata = fs::metadata(socket.parent().unwrap()).unwrap();

        assert_eq!(metadata.permissions().mode() & 0o777, 0o700);
        assert_eq!(metadata.uid(), unsafe { libc::geteuid() });
    }

    #[test]
    fn same_user_socket_peer_is_accepted() {
        let (stream, _) = UnixStream::pair().unwrap();

        verify_peer_uid(&stream).unwrap();
    }
}
