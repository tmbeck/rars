//! Compatibility positioned-read API used by callers that must retain one
//! source across parsing and fallback readers.

use std::io::{Read, Seek, SeekFrom};
use std::sync::{Arc, Mutex};

pub trait ReadAt: Send + Sync {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> std::io::Result<usize>;
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
        std::os::windows::fs::FileExt::seek_read(self, buf, offset)
    }

    fn size(&self) -> std::io::Result<u64> {
        Ok(self.metadata()?.len())
    }
}

pub struct SeekReader<R>(Mutex<R>);

impl<R> SeekReader<R> {
    pub fn new(inner: R) -> Self {
        Self(Mutex::new(inner))
    }

    pub fn into_inner(self) -> R {
        self.0.into_inner().unwrap_or_else(|e| e.into_inner())
    }
}

impl<R: Read + Seek + Send> ReadAt for SeekReader<R> {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> std::io::Result<usize> {
        let mut reader = self.0.lock().unwrap_or_else(|e| e.into_inner());
        reader.seek(SeekFrom::Start(offset))?;
        reader.read(buf)
    }

    fn size(&self) -> std::io::Result<u64> {
        let mut reader = self.0.lock().unwrap_or_else(|e| e.into_inner());
        reader.seek(SeekFrom::End(0))
    }
}

pub(crate) struct ReadAtReader {
    source: Arc<dyn ReadAt>,
    position: u64,
}

impl Read for ReadAtReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let count = self.source.read_at(buf, self.position)?;
        self.position = self.position.saturating_add(count as u64);
        Ok(count)
    }
}

impl Seek for ReadAtReader {
    fn seek(&mut self, from: SeekFrom) -> std::io::Result<u64> {
        let end = self.source.size()?;
        let next = match from {
            SeekFrom::Start(n) => i128::from(n),
            SeekFrom::Current(n) => i128::from(self.position) + i128::from(n),
            SeekFrom::End(n) => i128::from(end) + i128::from(n),
        };
        self.position = u64::try_from(next)
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid seek"))?;
        Ok(self.position)
    }
}

pub(crate) fn as_reader(source: Arc<dyn ReadAt>) -> ReadAtReader {
    ReadAtReader {
        source,
        position: 0,
    }
}
