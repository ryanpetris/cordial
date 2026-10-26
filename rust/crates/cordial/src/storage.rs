//! Saving downloaded adapter files. A download is written to a private temporary file and
//! published only once complete, never replacing a local file unless asked to.
use crate::error::{Error, Result};
use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    os::unix::fs::OpenOptionsExt,
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
        options.write(true).create_new(true).mode(0o600);
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
/// Publishes `from` without replacing an existing destination. A filesystem without
/// no-replace rename uses a hard link. Other failures leave the destination untouched.
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
    /// The filesystem has no atomic no-replace rename.
    Unsupported,
    Failed(io::Error),
}

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

/// renameat2 errors meaning the flag is unavailable.
fn unsupported(e: rustix::io::Errno) -> bool {
    use rustix::io::Errno;
    [Errno::INVAL, Errno::NOSYS, Errno::NOTSUP, Errno::OPNOTSUPP].contains(&e)
}

/// A hard link never replaces; the temporary name is removed afterwards.
fn link_noreplace(from: &Path, local: &Path) -> io::Result<()> {
    fs::hard_link(from, local)?;
    let _ = fs::remove_file(from);
    Ok(())
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
