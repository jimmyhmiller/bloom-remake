use crate::{StoreError, read_all};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::Mutex,
};

/// Explicit file creation and truncation policy.
#[derive(Clone, Copy, Debug, Default)]
pub struct OpenOpts {
    pub create: bool,
    pub create_new: bool,
    pub truncate: bool,
}
/// Filesystem seam used by all durable operations.
pub trait Vfs: Send + Sync {
    fn open(&self, path: &Path, opts: OpenOpts) -> Result<Box<dyn VfsFile>, StoreError>;
    fn rename(&self, from: &Path, to: &Path) -> Result<(), StoreError>;
    fn remove(&self, path: &Path) -> Result<(), StoreError>;
    /// Remove an empty directory. The caller syncs the parent to make the removal durable.
    fn remove_dir(&self, path: &Path) -> Result<(), StoreError>;
    fn list(&self, dir: &Path) -> Result<Vec<PathBuf>, StoreError>;
    fn sync_dir(&self, dir: &Path) -> Result<(), StoreError>;
    fn lock_exclusive(&self, path: &Path) -> Result<Box<dyn VfsLock>, StoreError>;
    /// Create a directory and its parents. The caller syncs parent directories before acknowledging their entries.
    fn create_dir_all(&self, path: &Path) -> Result<(), StoreError>;
}
/// Positional reads and sequential appends; a successful append writes all requested bytes.
pub trait VfsFile: Send {
    fn pread(&self, off: u64, buf: &mut [u8]) -> Result<usize, StoreError>;
    fn append(&mut self, data: &[u8]) -> Result<(), StoreError>;
    fn sync_data(&mut self) -> Result<(), StoreError>;
    fn len(&self) -> Result<u64, StoreError>;
    fn is_empty(&self) -> Result<bool, StoreError> {
        Ok(self.len()? == 0)
    }
    fn truncate(&mut self, len: u64) -> Result<(), StoreError>;
}
/// Lifetime guard for an exclusive store lock.
pub trait VfsLock: Send {}
/// Operating-system filesystem implementation.
#[derive(Debug, Default)]
pub struct RealFs;
struct RealFile(Mutex<File>);
impl VfsFile for RealFile {
    #[cfg(unix)]
    fn pread(&self, off: u64, buf: &mut [u8]) -> Result<usize, StoreError> {
        use std::os::unix::fs::FileExt;
        Ok(self
            .0
            .lock()
            .map_err(|_| crate::invalid("file mutex poisoned"))?
            .read_at(buf, off)?)
    }
    /// Without a positional read, a seek and a read under the file's lock (appends seek to the end themselves).
    #[cfg(not(unix))]
    fn pread(&self, off: u64, buf: &mut [u8]) -> Result<usize, StoreError> {
        let mut f = self.0.lock().map_err(|_| crate::invalid("file mutex poisoned"))?;
        f.seek(SeekFrom::Start(off))?;
        Ok(f.read(buf)?)
    }
    fn append(&mut self, data: &[u8]) -> Result<(), StoreError> {
        let f = self.0.get_mut().map_err(|_| crate::invalid("file mutex poisoned"))?;
        f.seek(SeekFrom::End(0))?;
        f.write_all(data)?;
        Ok(())
    }
    fn sync_data(&mut self) -> Result<(), StoreError> {
        // std issues F_FULLFSYNC on Apple targets; no unsafe platform syscall is necessary.
        self.0
            .get_mut()
            .map_err(|_| crate::invalid("file mutex poisoned"))?
            .sync_data()?;
        Ok(())
    }
    fn len(&self) -> Result<u64, StoreError> {
        Ok(self
            .0
            .lock()
            .map_err(|_| crate::invalid("file mutex poisoned"))?
            .metadata()?
            .len())
    }
    fn truncate(&mut self, len: u64) -> Result<(), StoreError> {
        self.0
            .get_mut()
            .map_err(|_| crate::invalid("file mutex poisoned"))?
            .set_len(len)?;
        Ok(())
    }
}
struct RealLock {
    _file: File,
}
impl VfsLock for RealLock {}
impl Vfs for RealFs {
    fn open(&self, path: &Path, opts: OpenOpts) -> Result<Box<dyn VfsFile>, StoreError> {
        Ok(Box::new(RealFile(Mutex::new(
            OpenOptions::new()
                .read(true)
                .write(true)
                .create(opts.create)
                .create_new(opts.create_new)
                .truncate(opts.truncate)
                .open(path)?,
        ))))
    }
    fn rename(&self, from: &Path, to: &Path) -> Result<(), StoreError> {
        std::fs::rename(from, to)?;
        Ok(())
    }
    fn remove(&self, path: &Path) -> Result<(), StoreError> {
        std::fs::remove_file(path)?;
        Ok(())
    }
    fn remove_dir(&self, path: &Path) -> Result<(), StoreError> {
        std::fs::remove_dir(path)?;
        Ok(())
    }
    fn list(&self, dir: &Path) -> Result<Vec<PathBuf>, StoreError> {
        let mut paths = std::fs::read_dir(dir)?
            .map(|e| e.map(|e| e.path()))
            .collect::<Result<Vec<_>, _>>()?;
        paths.sort();
        Ok(paths)
    }
    fn sync_dir(&self, dir: &Path) -> Result<(), StoreError> {
        File::open(dir)?.sync_all()?;
        Ok(())
    }
    fn create_dir_all(&self, path: &Path) -> Result<(), StoreError> {
        std::fs::create_dir_all(path)?;
        Ok(())
    }
    fn lock_exclusive(&self, path: &Path) -> Result<Box<dyn VfsLock>, StoreError> {
        let mut f = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        if let Err(e) = f.try_lock() {
            if matches!(e, std::fs::TryLockError::WouldBlock) {
                let mut pid = String::new();
                f.read_to_string(&mut pid)?;
                return Err(StoreError::Locked { path: path.into(), pid });
            }
            return Err(StoreError::Io(e.into()));
        }
        f.set_len(0)?;
        write!(f, "{}", std::process::id())?;
        f.sync_data()?;
        Ok(Box::new(RealLock { _file: f }))
    }
}

pub(crate) fn atomic_write(fs: &dyn Vfs, path: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    let tmp = path.with_extension("tmp");
    let mut f = fs.open(
        &tmp,
        OpenOpts {
            create: true,
            truncate: true,
            ..OpenOpts::default()
        },
    )?;
    f.append(bytes)?;
    f.sync_data()?;
    fs.rename(&tmp, path)?;
    fs.sync_dir(path.parent().unwrap_or(Path::new(".")))?;
    Ok(())
}
pub(crate) fn read_path(fs: &dyn Vfs, path: &Path) -> Result<Vec<u8>, StoreError> {
    read_all(&*fs.open(path, OpenOpts::default())?)
}

/// Create a directory tree and persist every new directory entry up to the filesystem root.
pub fn durable_dir(fs: &dyn Vfs, path: &Path) -> Result<(), StoreError> {
    fs.create_dir_all(path)?;
    for ancestor in path.ancestors() {
        if let Some(parent) = ancestor.parent() {
            // The parent of a relative path's first component is the empty path: the current directory.
            fs.sync_dir(if parent.as_os_str().is_empty() {
                Path::new(".")
            } else {
                parent
            })?;
        }
    }
    Ok(())
}
