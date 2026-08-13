use anyhow::{Context, Result, bail};
use std::{
    ffi::CString,
    fs::{self, OpenOptions},
    io::Write,
    os::unix::ffi::OsStrExt,
    path::Path,
};

pub fn write_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("destination has no parent")?;
    fs::create_dir_all(parent)?;
    let temp = crate::local::unique_temp_path(parent, "isaaccloud-write");
    let existing_permissions = fs::metadata(path)
        .ok()
        .map(|metadata| metadata.permissions());

    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp)?;
        if let Some(permissions) = existing_permissions {
            file.set_permissions(permissions)?;
        }
        file.write_all(bytes)?;
        file.flush()?;
        file.sync_all()?;

        fs::rename(&temp, path)
            .with_context(|| format!("atomic rename {} -> {}", temp.display(), path.display()))?;
        fsync_directory(parent)?;
        Ok(())
    })();

    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

pub fn replace_from_file(source: &Path, destination: &Path) -> Result<()> {
    let bytes = fs::read(source)?;
    write_bytes(destination, &bytes)
}

fn fsync_directory(path: &Path) -> Result<()> {
    let value = CString::new(path.as_os_str().as_bytes())?;
    let descriptor = unsafe { libc::open(value.as_ptr(), libc::O_RDONLY) };
    if descriptor < 0 {
        bail!("open directory for fsync failed: {}", path.display());
    }
    let result = unsafe { libc::fsync(descriptor) };
    unsafe { libc::close(descriptor) };
    if result != 0 {
        bail!("directory fsync failed: {}", path.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    #[test]
    fn replacement_is_complete_and_leaves_no_staging_file() {
        let root = std::env::temp_dir().join(format!("isaaccloud-atomic-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let destination = root.join("rep_persistentgamedata1.dat");
        fs::write(&destination, b"old-valid-save").unwrap();
        fs::set_permissions(&destination, fs::Permissions::from_mode(0o600)).unwrap();
        write_bytes(&destination, b"new-valid-save").unwrap();
        assert_eq!(fs::read(&destination).unwrap(), b"new-valid-save");
        assert_eq!(fs::metadata(&destination).unwrap().mode() & 0o777, 0o600);
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
        let _ = fs::remove_dir_all(root);
    }
}
