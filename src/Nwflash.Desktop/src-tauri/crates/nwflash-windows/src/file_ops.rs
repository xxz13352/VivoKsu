//! Local file operations that preserve a no-replace publication boundary.
//!
//! A transfer must never expose a partially written final path or replace a
//! user-selected file behind their back. The helper below publishes a
//! complete regular file only when the destination does not already exist.

use std::{fs, io, path::Path};

/// Verifies that `path` and every existing ancestor are ordinary directories
/// reached without a symlink/reparse hop.  Callers should perform this check
/// immediately before creating a sibling temporary file; the exclusive
/// create/open helpers below repeat the final-component check at the handle
/// boundary.
pub fn ensure_safe_directory(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path must be an ordinary directory",
        ));
    }

    for ancestor in path.ancestors() {
        if ancestor.as_os_str().is_empty() {
            continue;
        }
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) => {
                if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "directory ancestry must contain only ordinary directories",
                    ));
                }
                reject_reparse_point(ancestor)?;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// Creates one transaction-owned regular file without following a reparse
/// point at the final path component.  `create_new` is retained so a UUID
/// collision cannot truncate another operation's temporary file.
pub fn create_exclusive_regular_file(path: &Path) -> io::Result<fs::File> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, FILE_SHARE_WRITE,
        };

        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)?;
        if let Err(error) = ensure_regular_file_handle(&file) {
            drop(file);
            let _ = fs::remove_file(path);
            return Err(error);
        }
        Ok(file)
    }

    #[cfg(not(windows))]
    {
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)?;
        if let Err(error) = ensure_regular_file_handle(&file) {
            drop(file);
            let _ = fs::remove_file(path);
            return Err(error);
        }
        Ok(file)
    }
}

/// Opens a completed transfer file for a durability flush while rejecting a
/// symlink/reparse final component.
pub fn open_regular_file_no_follow(path: &Path) -> io::Result<fs::File> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, FILE_SHARE_WRITE,
        };

        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)?;
        ensure_regular_file_handle(&file)?;
        Ok(file)
    }

    #[cfg(not(windows))]
    {
        let file = fs::OpenOptions::new().read(true).write(true).open(path)?;
        ensure_regular_file_handle(&file)?;
        Ok(file)
    }
}

/// Performs a no-follow regular-file check on an existing path.
pub fn ensure_safe_regular_file(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path must be an ordinary file",
        ));
    }
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        ensure_safe_directory(parent)?;
    }
    reject_reparse_point(path)
}

fn ensure_regular_file_handle(file: &fs::File) -> io::Result<()> {
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "file handle must refer to an ordinary file",
        ));
    }

    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "file handle must not refer to a reparse point",
            ));
        }
    }
    Ok(())
}

/// Publishes `source` at `destination` without replacing an existing target.
///
/// The source and destination are expected to be regular files in the same
/// directory/volume. Callers should sync and validate the source before this
/// function is called. An existing file, directory, symlink, or reparse point
/// is reported as an error and is never removed.
pub fn promote_without_replace(source: &Path, destination: &Path) -> io::Result<()> {
    if source == destination {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "source and destination must differ",
        ));
    }

    validate_promotion_paths(source, destination)?;

    #[cfg(windows)]
    {
        promote_without_replace_windows(source, destination)?;
        validate_promoted_destination(source, destination)
    }

    #[cfg(not(windows))]
    {
        promote_without_replace_link(source, destination)?;
        validate_promoted_destination(source, destination)
    }
}

fn validate_promotion_paths(source: &Path, destination: &Path) -> io::Result<()> {
    let source_metadata = fs::symlink_metadata(source)?;
    if !source_metadata.file_type().is_file() || source_metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "promotion source must be a regular non-symlink file",
        ));
    }
    reject_reparse_point(source)?;
    if let Some(parent) = source
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        ensure_safe_directory(parent)?;
    }

    match fs::symlink_metadata(destination) {
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "promotion destination already exists",
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    let parent = destination.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "promotion destination must have a parent",
        )
    })?;
    ensure_safe_directory(parent)?;
    Ok(())
}

fn reject_reparse_point(path: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::{
            GetFileAttributesW, FILE_ATTRIBUTE_REPARSE_POINT, INVALID_FILE_ATTRIBUTES,
        };

        let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
        if wide.contains(&0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "path contains an embedded NUL",
            ));
        }
        wide.push(0);
        let attributes = unsafe { GetFileAttributesW(wide.as_ptr()) };
        if attributes == INVALID_FILE_ATTRIBUTES {
            return Err(io::Error::last_os_error());
        }
        if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "reparse points are not valid promotion paths",
            ));
        }
    }

    #[cfg(not(windows))]
    {
        let metadata = fs::symlink_metadata(path)?;
        if metadata.file_type().is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "symlink paths are not valid promotion paths",
            ));
        }
    }
    Ok(())
}

fn validate_promoted_destination(source: &Path, destination: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(destination)?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "promoted destination is not a regular file",
        ));
    }
    reject_reparse_point(destination)?;
    match fs::symlink_metadata(source) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "promotion source still exists",
        )),
        Err(error) => Err(error),
    }
}

#[cfg(windows)]
fn promote_without_replace_windows(source: &Path, destination: &Path) -> io::Result<()> {
    use windows_sys::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_WRITE_THROUGH};

    let source = nul_terminated_wide(source)?;
    let destination = nul_terminated_wide(destination)?;
    // Omitting MOVEFILE_REPLACE_EXISTING is intentional: Windows then fails
    // when the destination exists, preserving the previous user file.
    let moved = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(windows)]
fn nul_terminated_wide(path: &Path) -> io::Result<Vec<u16>> {
    use std::os::windows::ffi::OsStrExt;

    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    if wide.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path contains an embedded NUL",
        ));
    }
    wide.push(0);
    Ok(wide)
}

#[cfg(not(windows))]
fn promote_without_replace_link(source: &Path, destination: &Path) -> io::Result<()> {
    // A hard link creates the destination atomically and fails with
    // AlreadyExists when a target (including a symlink) is present. Removing
    // the source afterwards leaves a complete file at the destination.
    fs::hard_link(source, destination)?;
    if let Err(error) = fs::remove_file(source) {
        let _ = fs::remove_file(destination);
        return Err(error);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    fn temporary_directory(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be available")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("nwflash-file-ops-{label}-{nonce}"));
        fs::create_dir_all(&path).expect("temporary directory should be created");
        path
    }

    #[test]
    fn promotion_publishes_complete_source_and_removes_temp() {
        let root = temporary_directory("success");
        let source = root.join(".source.partial");
        let destination = root.join("result.bin");
        fs::write(&source, b"complete").expect("source should be written");

        promote_without_replace(&source, &destination).expect("promotion should succeed");

        assert_eq!(
            fs::read(&destination).expect("destination should exist"),
            b"complete"
        );
        assert!(!source.exists());
        fs::remove_dir_all(root).expect("temporary directory should be removed");
    }

    #[test]
    fn promotion_rejects_existing_destination_without_changing_either_file() {
        let root = temporary_directory("existing");
        let source = root.join(".source.partial");
        let destination = root.join("result.bin");
        fs::write(&source, b"new").expect("source should be written");
        fs::write(&destination, b"old").expect("destination should be written");

        let error = promote_without_replace(&source, &destination)
            .expect_err("an existing destination must be rejected");

        assert!(matches!(
            error.kind(),
            io::ErrorKind::AlreadyExists | io::ErrorKind::PermissionDenied
        ));
        assert_eq!(fs::read(&source).expect("source should remain"), b"new");
        assert_eq!(
            fs::read(&destination).expect("destination should remain"),
            b"old"
        );
        fs::remove_dir_all(root).expect("temporary directory should be removed");
    }

    #[test]
    fn promotion_rejects_a_missing_source_without_creating_destination() {
        let root = temporary_directory("missing");
        let source = root.join(".missing.partial");
        let destination = root.join("result.bin");

        assert!(promote_without_replace(&source, &destination).is_err());
        assert!(!destination.exists());
        fs::remove_dir_all(root).expect("temporary directory should be removed");
    }

    #[test]
    fn promotion_rejects_identical_paths() {
        let root = temporary_directory("same");
        let source = root.join("result.bin");
        fs::write(&source, b"same").expect("source should be written");

        let error = promote_without_replace(&source, &source)
            .expect_err("identical paths must be rejected");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        fs::remove_dir_all(root).expect("temporary directory should be removed");
    }

    #[test]
    fn promotion_rejects_a_directory_source() {
        let root = temporary_directory("directory-source");
        let source = root.join("source-directory");
        let destination = root.join("result.bin");
        fs::create_dir(&source).expect("source directory should be created");

        let error = promote_without_replace(&source, &destination)
            .expect_err("directory sources must not be promoted");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(!destination.exists());
        fs::remove_dir_all(root).expect("temporary directory should be removed");
    }

    #[test]
    fn promotion_rejects_an_existing_destination_directory() {
        let root = temporary_directory("directory-destination");
        let source = root.join(".source.partial");
        let destination = root.join("result-directory");
        fs::write(&source, b"new").expect("source should be written");
        fs::create_dir(&destination).expect("destination directory should be created");

        let error = promote_without_replace(&source, &destination)
            .expect_err("directory destinations must not be replaced");
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert!(destination.is_dir());
        assert!(source.exists());
        fs::remove_dir_all(root).expect("temporary directory should be removed");
    }

    #[test]
    fn promotion_rejects_a_broken_symlink_destination_when_supported() {
        let root = temporary_directory("broken-symlink");
        let source = root.join(".source.partial");
        let destination = root.join("result.bin");
        let missing = root.join("missing-target.bin");
        fs::write(&source, b"new").expect("source should be written");

        #[cfg(unix)]
        std::os::unix::fs::symlink(&missing, &destination)
            .expect("unix should create the broken symlink fixture");
        #[cfg(windows)]
        if std::os::windows::fs::symlink_file(&missing, &destination).is_err() {
            eprintln!("skipping broken symlink fixture: symlink privilege unavailable");
            fs::remove_dir_all(root).expect("temporary directory should be removed");
            return;
        }

        let error = promote_without_replace(&source, &destination)
            .expect_err("broken symlink destinations must not be replaced");
        assert!(matches!(
            error.kind(),
            io::ErrorKind::AlreadyExists | io::ErrorKind::InvalidInput
        ));
        assert!(fs::symlink_metadata(&destination).is_ok());
        assert!(source.exists());
        fs::remove_dir_all(root).expect("temporary directory should be removed");
    }
}
