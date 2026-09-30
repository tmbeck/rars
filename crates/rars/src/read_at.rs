//! Positioned reads, so one open handle serves every range of an archive
//! without a shared cursor, and callers can supply their own storage.

use std::io::{Read, Seek, SeekFrom};
use std::sync::{Arc, Mutex};

/// A random-access byte source. `read_at` may return fewer bytes than asked
/// (0 only at or past the end), like `Read::read`.
pub trait ReadAt: Send + Sync {
    /// Reads into `buf` starting at `offset`, returning how many bytes were
    /// read. May read fewer than `buf.len()`; 0 means `offset` is at or past
    /// the end.
    fn read_at(&self, buf: &mut [u8], offset: u64) -> std::io::Result<usize>;
    /// Total length in bytes.
    fn size(&self) -> std::io::Result<u64>;
}

#[cfg(unix)]
impl ReadAt for std::fs::File {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> std::io::Result<usize> {
        std::os::unix::fs::FileExt::read_at(self, buf, offset)
    }
    fn size(&self) -> std::io::Result<u64> {
        Ok(self.metadata()?.len())
    }
}

#[cfg(windows)]
impl ReadAt for std::fs::File {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> std::io::Result<usize> {
        // Moves the file cursor, which rars never uses.
        std::os::windows::fs::FileExt::seek_read(self, buf, offset)
    }
    fn size(&self) -> std::io::Result<u64> {
        Ok(self.metadata()?.len())
    }
}

/// Any `Read + Seek` behind a lock, for targets without positioned file
/// reads (and for callers whose storage only seeks).
pub struct SeekReader<R>(Mutex<R>);

impl<R> SeekReader<R> {
    /// Wraps `inner`; each read seeks it to the requested offset first.
    pub fn new(inner: R) -> Self {
        Self(Mutex::new(inner))
    }
    /// Returns the wrapped reader, at whatever position the last read left it.
    pub fn into_inner(self) -> R {
        self.0.into_inner().unwrap_or_else(|e| e.into_inner())
    }
}

impl<R: Read + Seek + Send> ReadAt for SeekReader<R> {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> std::io::Result<usize> {
        let mut r = self.0.lock().unwrap_or_else(|e| e.into_inner());
        r.seek(SeekFrom::Start(offset))?;
        r.read(buf)
    }
    fn size(&self) -> std::io::Result<u64> {
        let mut r = self.0.lock().unwrap_or_else(|e| e.into_inner());
        r.seek(SeekFrom::End(0))
    }
}

/// Wraps an open file for positioned reads: directly where the platform has
/// them, behind a [`SeekReader`] elsewhere.
pub(crate) fn file_source(file: std::fs::File) -> Arc<dyn ReadAt> {
    #[cfg(any(unix, windows))]
    {
        Arc::new(file)
    }
    #[cfg(not(any(unix, windows)))]
    {
        Arc::new(SeekReader::new(file))
    }
}
