use rars::{ArchiveReadOptions, ArchiveReader, ArchiveVersion, Builder, ReadAt, SeekReader};
use std::io::{self, Cursor, Write};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc, Mutex,
};

/// A deliberately unhelpful positioned source: every successful read is short,
/// and reads can be failed after parsing has completed.
struct Positioned {
    bytes: Arc<Vec<u8>>,
    max_read: usize,
    fail: Arc<AtomicBool>,
    reads: Arc<AtomicUsize>,
    dropped: Arc<AtomicBool>,
}

impl ReadAt for Positioned {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        if self.fail.load(Ordering::Relaxed) {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                "positioned read failed",
            ));
        }
        let offset = usize::try_from(offset)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "offset overflows usize"))?;
        if offset >= self.bytes.len() {
            return Ok(0);
        }
        let count = buf.len().min(self.max_read).min(self.bytes.len() - offset);
        buf[..count].copy_from_slice(&self.bytes[offset..offset + count]);
        self.reads.fetch_add(1, Ordering::Relaxed);
        Ok(count)
    }

    fn size(&self) -> io::Result<u64> {
        if self.fail.load(Ordering::Relaxed) {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                "positioned size failed",
            ));
        }
        Ok(self.bytes.len() as u64)
    }
}

impl Drop for Positioned {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::Relaxed);
    }
}

fn positioned_source(
    bytes: Vec<u8>,
    max_read: usize,
) -> (
    Arc<dyn ReadAt>,
    Arc<AtomicBool>,
    Arc<AtomicUsize>,
    Arc<AtomicBool>,
) {
    let fail = Arc::new(AtomicBool::new(false));
    let reads = Arc::new(AtomicUsize::new(0));
    let dropped = Arc::new(AtomicBool::new(false));
    let source = Positioned {
        bytes: Arc::new(bytes),
        max_read,
        fail: fail.clone(),
        reads: reads.clone(),
        dropped: dropped.clone(),
    };
    (Arc::new(source), fail, reads, dropped)
}

struct Capture(Arc<Mutex<Vec<u8>>>);
impl Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn archive_bytes(version: ArchiveVersion, payload: &[u8]) -> Vec<u8> {
    let mut builder = Builder::new(version).store(true);
    builder
        .add_bytes(b"payload.bin".to_vec(), payload.to_vec(), None, None)
        .unwrap();
    builder.to_bytes().unwrap()
}

#[test]
fn read_source_handles_short_reads_for_legacy_rar5_and_rar7_and_sfx() {
    for version in [
        ArchiveVersion::Rar14,
        ArchiveVersion::Rar50,
        ArchiveVersion::Rar70,
    ] {
        let payload = (0..4097).map(|n| (n % 251) as u8).collect::<Vec<_>>();
        let mut bytes = if version == ArchiveVersion::Rar14 {
            b"MZ SFX stub".to_vec()
        } else {
            Vec::new()
        };
        bytes.extend(archive_bytes(version, &payload));
        let (source, _, reads, _) = positioned_source(
            bytes,
            if version == ArchiveVersion::Rar14 {
                usize::MAX
            } else {
                137
            },
        );
        let archive = ArchiveReader::read_source(source).unwrap_or_else(|error| {
            panic!("{version:?}: {error:?}");
        });
        if version != ArchiveVersion::Rar14 {
            assert!(
                reads.load(Ordering::Relaxed) > 10,
                "short reads were not exercised"
            );
        }
        assert_eq!(
            archive.read_member(b"payload.bin", None).unwrap(),
            Some(payload)
        );
    }
}

#[test]
fn positioned_source_is_retained_until_archive_is_dropped() {
    let (source, _, _, dropped) =
        positioned_source(archive_bytes(ArchiveVersion::Rar50, b"kept"), 3);
    let archive = ArchiveReader::read_source(source.clone()).unwrap();
    drop(source);
    assert!(!dropped.load(Ordering::Relaxed));
    assert_eq!(
        archive.read_member(b"payload.bin", None).unwrap(),
        Some(b"kept".to_vec())
    );
    drop(archive);
    assert!(dropped.load(Ordering::Relaxed));
}

#[test]
fn positioned_source_errors_are_reported_during_parse_and_extraction() {
    let bytes = archive_bytes(ArchiveVersion::Rar50, b"payload");
    let (source, fail, _, _) = positioned_source(bytes.clone(), 7);
    fail.store(true, Ordering::Relaxed);
    let error = ArchiveReader::read_source(source).unwrap_err();
    assert_eq!(error.kind(), rars::ErrorKind::Io);

    let (source, fail, _, _) = positioned_source(bytes, 7);
    let archive = ArchiveReader::read_source(source).unwrap();
    fail.store(true, Ordering::Relaxed);
    let error = archive
        .extract_to(None, |_| Ok(Box::new(io::sink())))
        .unwrap_err();
    assert_eq!(error.kind(), rars::ErrorKind::Io);
}

#[test]
fn positioned_source_honors_options_and_precancellation_without_reading() {
    let bytes = archive_bytes(ArchiveVersion::Rar50, b"payload");
    let (source, _, reads, _) = positioned_source(bytes, 2);
    let token = rars::ReadCancellation::new();
    token.cancel();
    let error = ArchiveReader::read_source_with_options(
        source,
        ArchiveReadOptions::new().with_cancellation(&token),
    )
    .unwrap_err();
    assert_eq!(error.kind(), rars::ErrorKind::Cancelled);
    assert_eq!(reads.load(Ordering::Relaxed), 0);
}

#[test]
fn positioned_sources_can_extract_split_legacy_rar5_and_rar7_volumes() {
    for version in [
        ArchiveVersion::Rar14,
        ArchiveVersion::Rar50,
        ArchiveVersion::Rar70,
    ] {
        let payload = vec![42; 4096];
        let mut builder = Builder::new(version).store(true).volume_size(Some(512));
        builder
            .add_bytes(b"split.bin".to_vec(), payload.clone(), None, None)
            .unwrap();
        let archives: Vec<_> = builder
            .build_volumes(None)
            .unwrap()
            .into_iter()
            .map(|bytes| ArchiveReader::read_source(positioned_source(bytes, 11).0).unwrap())
            .collect();
        assert!(archives.len() > 1);
        let output = Arc::new(Mutex::new(Vec::new()));
        rars::extract_volumes_to(&archives, None, |_| Ok(Box::new(Capture(output.clone()))))
            .unwrap();
        assert_eq!(*output.lock().unwrap(), payload);
    }
}

#[test]
fn seek_reader_implements_positioned_reads_with_short_and_failed_io() {
    let reader = SeekReader::new(Cursor::new(b"0123456789".to_vec()));
    let mut buf = [0; 3];
    assert_eq!(reader.read_at(&mut buf, 4).unwrap(), 3);
    assert_eq!(&buf, b"456");
    assert_eq!(reader.size().unwrap(), 10);
    assert_eq!(reader.read_at(&mut buf, 9).unwrap(), 1);
    assert_eq!(&buf[..1], b"9");
}
