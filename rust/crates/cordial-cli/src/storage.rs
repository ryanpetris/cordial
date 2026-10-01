//! Saving downloaded adapter files. A download is written to a private temporary file and
//! published only once complete, never replacing a local file unless asked to.
use crate::error::{Error, Result};
use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

/// The error for an existing destination that was not to be replaced.
pub const EXISTS: &str = "the local file already exists";

/// Checks a local destination before a download starts.
pub fn check_destination(local: &Path, overwrite: bool) -> Result<()> {
    if local.file_name().is_none() {
        return Err(Error::new("invalid local filename"));
    }
    match fs::metadata(local) {
        Ok(m) if m.is_dir() => Err(Error::new("the local destination is a directory")),
        Ok(_) if !overwrite => Err(Error::new(EXISTS)),
        _ => Ok(()),
    }
}

/// A private temporary file beside the destination, removed unless published.
struct Temp {
    path: PathBuf,
    file: Option<fs::File>,
}
impl Temp {
    fn create(local: &Path) -> Result<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let parent = local
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let name = local.file_name().unwrap().to_string_lossy();
        let path = parent.join(format!(
            ".{name}.{}.{}.part",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(&path)?;
        Ok(Self {
            path,
            file: Some(file),
        })
    }
    fn publish(mut self, local: &Path, overwrite: bool) -> Result<()> {
        let file = self.file.take().unwrap();
        file.sync_all()?;
        drop(file);
        if overwrite {
            fs::rename(&self.path, local)?;
        } else {
            publish_new(&self.path, local)?;
        }
        self.path = PathBuf::new();
        Ok(())
    }
}
/// Moves `from` to `local` only if `local` does not exist, atomically: a file
/// created meanwhile, even during the download, always survives. The
/// platform's no-replace rename is used; a hard link, which also never
/// replaces, only where that rename is unsupported. Otherwise the download
/// fails rather than risk replacing.
fn publish_new(from: &Path, local: &Path) -> Result<()> {
    let result = match rename_noreplace(from, local) {
        Err(Native::Unsupported) => link_noreplace(from, local),
        Err(Native::Failed(e)) => Err(e),
        Ok(()) => Ok(()),
    };
    result.map_err(|e| match e.kind() {
        io::ErrorKind::AlreadyExists => Error::new(EXISTS),
        _ => Error::new(format!(
            "couldn't save without risking replacing a local file: {e}"
        )),
    })
}

enum Native {
    /// This platform or filesystem has no atomic no-replace rename.
    #[cfg_attr(windows, allow(dead_code))]
    Unsupported,
    Failed(io::Error),
}

#[cfg(any(target_os = "linux", target_vendor = "apple"))]
fn rename_noreplace(from: &Path, local: &Path) -> std::result::Result<(), Native> {
    use rustix::fs::{CWD, RenameFlags, renameat_with};
    renameat_with(CWD, from, CWD, local, RenameFlags::NOREPLACE).map_err(|e| {
        if unsupported(e) {
            Native::Unsupported
        } else {
            Native::Failed(e.into())
        }
    })
}

/// renameat2 or renameatx_np errors meaning the flag isn't available here.
#[cfg(any(target_os = "linux", target_vendor = "apple"))]
fn unsupported(e: rustix::io::Errno) -> bool {
    use rustix::io::Errno;
    [Errno::INVAL, Errno::NOSYS, Errno::NOTSUP, Errno::OPNOTSUPP].contains(&e)
}

/// MoveFileExW without MOVEFILE_REPLACE_EXISTING or MOVEFILE_COPY_ALLOWED
/// renames within the directory and fails if the destination exists.
#[cfg(windows)]
fn rename_noreplace(from: &Path, local: &Path) -> std::result::Result<(), Native> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{MOVEFILE_WRITE_THROUGH, MoveFileExW};
    let wide = |p: &Path| {
        p.as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<u16>>()
    };
    let (from, local) = (wide(from), wide(local));
    // SAFETY: both are NUL-terminated wide strings that outlive the call.
    if unsafe { MoveFileExW(from.as_ptr(), local.as_ptr(), MOVEFILE_WRITE_THROUGH) } != 0 {
        Ok(())
    } else {
        Err(Native::Failed(io::Error::last_os_error()))
    }
}

#[cfg(not(any(target_os = "linux", target_vendor = "apple", windows)))]
fn rename_noreplace(_: &Path, _: &Path) -> std::result::Result<(), Native> {
    Err(Native::Unsupported)
}

/// A hard link never replaces; the temporary name is removed afterwards.
#[cfg(unix)]
fn link_noreplace(from: &Path, local: &Path) -> io::Result<()> {
    fs::hard_link(from, local)?;
    let _ = fs::remove_file(from);
    Ok(())
}

#[cfg(not(unix))]
fn link_noreplace(_: &Path, _: &Path) -> io::Result<()> {
    Err(io::Error::from(io::ErrorKind::Unsupported))
}
impl Drop for Temp {
    fn drop(&mut self) {
        self.file.take();
        if !self.path.as_os_str().is_empty() {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// Writes a downloaded file to `local` and returns its length.
pub fn save(local: &Path, overwrite: bool, data: &[u8]) -> Result<u64> {
    check_destination(local, overwrite)?;
    let mut temp = Temp::create(local)?;
    temp.file.as_mut().unwrap().write_all(data)?;
    temp.publish(local, overwrite)?;
    Ok(data.len() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("cordial-publish-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn publishing_new_never_replaces_a_file_created_meanwhile() {
        let d = dir("race");
        let (from, local) = (d.join("part"), d.join("out"));
        fs::write(&from, b"new").unwrap();
        fs::write(&local, b"appeared").unwrap();
        assert_eq!(publish_new(&from, &local).unwrap_err().message, EXISTS);
        assert_eq!(fs::read(&local).unwrap(), b"appeared");
        fs::remove_file(&local).unwrap();
        publish_new(&from, &local).unwrap();
        assert_eq!(fs::read(&local).unwrap(), b"new");
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn failed_publication_never_replaces_and_reports_it() {
        // A move that cannot be made (here, a missing source) fails and leaves
        // no destination; nothing falls back to a replacing rename.
        let d = dir("nolink");
        let local = d.join("out");
        let error = publish_new(&d.join("missing"), &local).unwrap_err();
        assert!(
            error.message.starts_with("couldn't save without risking"),
            "{}",
            error.message
        );
        assert!(!local.exists());
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn a_refused_download_keeps_the_destination_and_removes_its_temporary() {
        let d = dir("temp");
        let local = d.join("out");
        let mut temp = Temp::create(&local).unwrap();
        temp.file.as_mut().unwrap().write_all(b"new").unwrap();
        fs::write(&local, b"appeared").unwrap();
        assert_eq!(temp.publish(&local, false).unwrap_err().message, EXISTS);
        assert_eq!(fs::read(&local).unwrap(), b"appeared");
        assert_eq!(fs::read_dir(&d).unwrap().count(), 1, "temporary removed");
        let mut temp = Temp::create(&local).unwrap();
        temp.file.as_mut().unwrap().write_all(b"new").unwrap();
        temp.publish(&local, true).unwrap();
        assert_eq!(fs::read(&local).unwrap(), b"new");
        assert_eq!(fs::read_dir(&d).unwrap().count(), 1);
        fs::remove_dir_all(&d).unwrap();
    }

    #[cfg(any(target_os = "linux", target_vendor = "apple"))]
    #[test]
    fn only_missing_flag_support_falls_back_to_a_link() {
        use rustix::io::Errno;
        for e in [Errno::INVAL, Errno::NOSYS, Errno::NOTSUP, Errno::OPNOTSUPP] {
            assert!(unsupported(e), "{e:?}");
        }
        for e in [Errno::EXIST, Errno::NOENT, Errno::ACCESS, Errno::XDEV] {
            assert!(!unsupported(e), "{e:?}");
        }
    }

    #[cfg(any(target_os = "linux", target_vendor = "apple"))]
    #[test]
    fn native_rename_publishes_without_leaving_the_temporary() {
        let d = dir("native");
        let (from, local) = (d.join("part"), d.join("out"));
        fs::write(&from, b"new").unwrap();
        assert!(rename_noreplace(&from, &local).is_ok());
        assert!(!from.exists());
        fs::write(&from, b"newer").unwrap();
        assert!(matches!(
            rename_noreplace(&from, &local),
            Err(Native::Failed(e)) if e.kind() == io::ErrorKind::AlreadyExists
        ));
        assert_eq!(fs::read(&local).unwrap(), b"new");
        // The link fallback is equally non-replacing.
        assert_eq!(
            link_noreplace(&from, &local).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        fs::remove_file(&local).unwrap();
        link_noreplace(&from, &local).unwrap();
        assert!(!from.exists() && fs::read(&local).unwrap() == b"newer");
        fs::remove_dir_all(&d).unwrap();
    }
}
