use std::{io, os::fd::AsRawFd, os::unix::net::UnixStream};

use anyhow::{Context, Result};

/// Returns the effective UID associated with the connected Unix socket peer.
#[cfg(any(
    target_vendor = "apple",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
))]
pub(crate) fn uid(stream: &UnixStream) -> Result<libc::uid_t> {
    let mut uid = 0;
    let mut gid = 0;
    // SAFETY: `stream` owns a valid connected socket descriptor, and the output
    // pointers refer to writable values for the duration of the call.
    let rc = unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) };
    if rc == -1 {
        Err(io::Error::last_os_error()).context("failed to read Unix socket peer credentials")
    } else {
        Ok(uid)
    }
}

/// Returns the effective UID associated with the connected Unix socket peer.
#[cfg(any(target_os = "linux", target_os = "android"))]
pub(crate) fn uid(stream: &UnixStream) -> Result<libc::uid_t> {
    let mut credentials = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: `stream` owns a valid connected socket descriptor. The buffer and
    // length pointers are valid for writes and describe a `libc::ucred`.
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

/// Fails closed on platforms where peer credential lookup is unavailable.
#[cfg(not(any(
    target_vendor = "apple",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly",
    target_os = "linux",
    target_os = "android"
)))]
pub(crate) fn uid(_stream: &UnixStream) -> Result<libc::uid_t> {
    anyhow::bail!("Unix socket peer credential verification is unsupported on this platform")
}

#[cfg(test)]
mod tests {
    use std::os::unix::net::UnixStream;

    use super::uid;

    #[test]
    fn same_user_socket_peer_uid_matches_effective_uid() {
        let (stream, _) = UnixStream::pair().unwrap();

        assert_eq!(uid(&stream).unwrap(), unsafe { libc::geteuid() });
    }
}
