use std::{ffi::OsStr, fs, os::unix::fs::FileTypeExt, path::PathBuf};

use anyhow::{Context, Result, bail};

/// Finds session sockets in `/tmp` and prints them in name order.
pub(crate) fn run_list() -> Result<()> {
    let mut sessions = Vec::new();

    for entry in fs::read_dir("/tmp").context("failed to read /tmp")? {
        let entry = entry.context("failed to read /tmp entry")?;
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

    Ok(PathBuf::from(format!("/tmp/dtch_{name}.sock")))
}

/// Extracts a session name from a socket file name that follows the dtch convention.
fn session_name_from_file_name(file_name: &OsStr) -> Option<String> {
    let file_name = file_name.to_str()?;
    let name = file_name.strip_prefix("dtch_")?.strip_suffix(".sock")?;

    if name.is_empty() {
        return None;
    }

    Some(name.to_owned())
}
