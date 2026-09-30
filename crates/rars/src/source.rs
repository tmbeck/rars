use crate::detect::{detect_archive_family, find_archive_start, ArchiveSignature, SFX_SCAN_LIMIT};
use crate::error::{Error, Result};
use crate::io_util::read_exact_at;
use crate::read_at::ReadAt;
use crate::version::ArchiveFamily;
use std::io::{Cursor, Read, Write};
use std::ops::Range;
use std::sync::Arc;

#[derive(Clone)]
pub(crate) enum ArchiveSource {
    Memory(Arc<[u8]>),
    Positioned(Arc<dyn ReadAt>),
}

impl std::fmt::Debug for ArchiveSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Memory(data) => write!(f, "Memory({} bytes)", data.len()),
            Self::Positioned(_) => f.write_str("Positioned"),
        }
    }
}

/// Finds the archive signature: at offset 0 from the first 8 bytes, else
/// within the first `SFX_SCAN_LIMIT` bytes (an SFX stub precedes it).
pub(crate) fn scan_signature(src: &dyn ReadAt) -> Result<ArchiveSignature> {
    let size = src.size()?;
    let head = read_exact_at(src, 0, size.min(8) as usize)?;
    // A RAR 1.3 signature at 0 can still lose to a later 1.5+ one in
    // `find_archive_start`, so only the other families short-circuit.
    if let Some(sig) = detect_archive_family(&head) {
        if sig.family != ArchiveFamily::Rar13 {
            return Ok(sig);
        }
    }
    let len = size.min(SFX_SCAN_LIMIT as u64) as usize;
    let scan = read_exact_at(src, 0, len)?;
    find_archive_start(&scan, SFX_SCAN_LIMIT).ok_or(Error::UnsupportedSignature)
}

/// `Read` over a byte range of a positioned source; unbuffered. A source
/// that ends before the range does is `UnexpectedEof`.
struct RangeReader {
    src: Arc<dyn ReadAt>,
    pos: u64,
    end: u64,
}

impl Read for RangeReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let remaining = self.end.saturating_sub(self.pos);
        let want = usize::try_from(remaining).map_or(buf.len(), |r| buf.len().min(r));
        if want == 0 {
            return Ok(0);
        }
        let n = self.src.read_at(&mut buf[..want], self.pos)?;
        if n == 0 {
            // The source ended inside the range: a short member, not a
            // shorter one.
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        self.pos += n as u64;
        Ok(n)
    }
}

impl ArchiveSource {
    pub(crate) fn read_range(&self, range: Range<usize>) -> Result<Vec<u8>> {
        match self {
            Self::Memory(data) => data
                .get(range)
                .map(|data| data.to_vec())
                .ok_or(Error::TooShort),
            Self::Positioned(src) => read_exact_at(src.as_ref(), range.start, range.len()),
        }
    }

    pub(crate) fn copy_range_to(&self, range: Range<usize>, writer: &mut dyn Write) -> Result<()> {
        let mut reader = self.range_reader(range)?;
        std::io::copy(&mut reader, writer)?;
        Ok(())
    }

    pub(crate) fn range_reader(&self, range: Range<usize>) -> Result<Box<dyn Read + '_>> {
        match self {
            Self::Memory(data) => {
                let data = data.get(range).ok_or(Error::TooShort)?;
                Ok(Box::new(Cursor::new(data)))
            }
            Self::Positioned(src) => Ok(Box::new(RangeReader {
                src: Arc::clone(src),
                pos: range.start as u64,
                end: range.end as u64,
            })),
        }
    }

    pub(crate) fn len(&self) -> Result<usize> {
        match self {
            Self::Memory(data) => Ok(data.len()),
            Self::Positioned(src) => usize::try_from(src.size()?)
                .map_err(|_| Error::InvalidHeader("archive size overflows host address size")),
        }
    }

    pub(crate) fn bytes(&self) -> Result<Vec<u8>> {
        match self {
            Self::Memory(data) => Ok(data.to_vec()),
            Self::Positioned(src) => read_exact_at(src.as_ref(), 0, self.len()?),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::read_at::SeekReader;

    #[test]
    fn a_range_past_the_end_of_the_source_is_an_error() {
        let source =
            ArchiveSource::Positioned(Arc::new(SeekReader::new(Cursor::new(vec![7u8; 10]))));
        let mut out = Vec::new();
        let err = source.copy_range_to(4..20, &mut out).unwrap_err();
        assert!(
            matches!(&err, Error::Io(e) if e.kind == std::io::ErrorKind::UnexpectedEof),
            "{err:?}"
        );
        let mut out = Vec::new();
        source.copy_range_to(4..10, &mut out).unwrap();
        assert_eq!(out, [7u8; 6]);
    }
}
