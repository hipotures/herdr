//! Unix filesystem checks for the private client control channel.

use std::fs;
use std::io;
use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _, PermissionsExt as _};
use std::path::Path;

pub(crate) fn ensure_private_directory(path: &Path, mode: u32) -> io::Result<()> {
    fs::create_dir_all(path)?;
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "client control path is not a private directory: {}",
                path.display()
            ),
        ));
    }
    if metadata.uid() != unsafe { libc::geteuid() } {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "client control directory is not owned by the current user: {}",
                path.display()
            ),
        ));
    }
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

pub(crate) fn is_socket_path(path: &Path, prefix: &str, suffix: &str) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    if !name.starts_with(prefix) || !name.ends_with(suffix) {
        return false;
    }
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return false;
    };
    !metadata.file_type().is_symlink() && metadata.file_type().is_socket()
}
