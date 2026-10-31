//! Compatibility positioned-read API used by callers that must retain one
//! source across parsing and fallback readers.

use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom};
use std::sync::{Arc, Mutex};

pub trait ReadAt: Send + Sync {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> std::io::Result<usize>;
    fn size(&self) -> std::io::Result<u64>;
}

impl<T: ?Sized + ReadAt> ReadAt for Arc<T> {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> std::io::Result<usize> {
        (**self).read_at(buf, offset)
    }

    fn size(&self) -> std::io::Result<u64> {
        (**self).size()
    }
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

/// Applies sparse patches to a buffer read at `offset`.
pub fn apply_overlay(patches: &BTreeMap<u64, Vec<u8>>, offset: u64, buf: &mut [u8]) {
    if patches.is_empty() || buf.is_empty() {
        return;
    }
    let range_start = offset;
    let range_end = offset.saturating_add(buf.len() as u64);

    for (&patch_offset, patch_data) in patches.iter() {
        let patch_start = patch_offset;
        let patch_end = patch_start.saturating_add(patch_data.len() as u64);
        if patch_start >= range_end {
            break;
        }
        if patch_end <= range_start {
            continue;
        }
        let overlap_start = patch_start.max(range_start);
        let overlap_end = patch_end.min(range_end);
        if overlap_start < overlap_end {
            let buf_slice_start = (overlap_start - range_start) as usize;
            let buf_slice_end = (overlap_end - range_start) as usize;
            let patch_slice_start = (overlap_start - patch_start) as usize;
            let patch_slice_end = (overlap_end - patch_start) as usize;
            buf[buf_slice_start..buf_slice_end]
                .copy_from_slice(&patch_data[patch_slice_start..patch_slice_end]);
        }
    }
}

/// A positioned-read wrapper that applies sparse in-memory repair patches over an underlying source.
#[derive(Clone, Debug)]
pub struct RepairedReadAt<T = Arc<dyn ReadAt>> {
    inner: T,
    patches: Arc<BTreeMap<u64, Vec<u8>>>,
}

impl<T> RepairedReadAt<T> {
    pub fn new(inner: T, patches: BTreeMap<u64, Vec<u8>>) -> Self {
        Self {
            inner,
            patches: Arc::new(patches),
        }
    }

    pub fn with_arc_patches(inner: T, patches: Arc<BTreeMap<u64, Vec<u8>>>) -> Self {
        Self { inner, patches }
    }

    pub fn inner(&self) -> &T {
        &self.inner
    }

    pub fn into_inner(self) -> T {
        self.inner
    }

    pub fn patches(&self) -> &BTreeMap<u64, Vec<u8>> {
        &self.patches
    }

    pub fn arc_patches(&self) -> &Arc<BTreeMap<u64, Vec<u8>>> {
        &self.patches
    }

    pub fn add_patch(&mut self, offset: u64, data: Vec<u8>) {
        Arc::make_mut(&mut self.patches).insert(offset, data);
    }
}

impl<T: ReadAt> ReadAt for RepairedReadAt<T> {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> std::io::Result<usize> {
        let n = self.inner.read_at(buf, offset)?;
        if n > 0 {
            apply_overlay(&self.patches, offset, &mut buf[..n]);
        }
        Ok(n)
    }

    fn size(&self) -> std::io::Result<u64> {
        self.inner.size()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn repaired_read_at_applies_sparse_patches() {
        let original = b"0123456789abcdef".to_vec();
        let reader = SeekReader::new(Cursor::new(original));
        let mut patches = BTreeMap::new();
        patches.insert(4, b"XXXX".to_vec());
        patches.insert(12, b"YYYY".to_vec());

        let repaired = RepairedReadAt::new(reader, patches);
        assert_eq!(repaired.size().unwrap(), 16);

        let mut buf = [0u8; 16];
        assert_eq!(repaired.read_at(&mut buf, 0).unwrap(), 16);
        assert_eq!(&buf, b"0123XXXX89abYYYY");

        let mut partial = [0u8; 6];
        assert_eq!(repaired.read_at(&mut partial, 2).unwrap(), 6);
        assert_eq!(&partial, b"23XXXX");

        let mut partial2 = [0u8; 4];
        assert_eq!(repaired.read_at(&mut partial2, 5).unwrap(), 4);
        assert_eq!(&partial2, b"XXX8");
    }

    #[test]
    fn repaired_read_at_works_with_arc_dyn_read_at() {
        let original = b"hello world from rars".to_vec();
        let reader: Arc<dyn ReadAt> = Arc::new(SeekReader::new(Cursor::new(original)));
        let mut patches = BTreeMap::new();
        patches.insert(6, b"THERE".to_vec());
        let repaired = RepairedReadAt::new(reader, patches);

        let mut buf = [0u8; 11];
        assert_eq!(repaired.read_at(&mut buf, 0).unwrap(), 11);
        assert_eq!(&buf, b"hello THERE");
    }
}
