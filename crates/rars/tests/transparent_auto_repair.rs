#![cfg(feature = "write")]
#![allow(clippy::type_complexity)]

use rars::crc32::crc32;
use rars::rar50::{self, Archive as Rar50Archive};
use rars::{
    Archive, ArchiveReadOptions, ArchiveReader, ArchiveVersion, ExtractionErrorAction, FeatureSet,
    ReadAt, RepairedReadAt, SeekReader,
};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io::{Cursor, Result as IoResult, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn entry(name: &[u8], data: &[u8]) -> rar50::ArchiveEntry {
    rar50::ArchiveEntry::new(
        name.to_vec(),
        rars::EntrySource::from_bytes(Arc::<[u8]>::from(data.to_vec())),
    )
}

fn write_stored_archive_with_recovery(
    entries: &[rar50::ArchiveEntry],
    recovery_percent: u64,
) -> Vec<u8> {
    rar50::Rar50Writer::new(
        rar50::WriterOptions::new(ArchiveVersion::Rar50, FeatureSet::store_only())
            .with_compression_level(0),
    )
    .entries(entries.to_vec())
    .recovery_percent(Some(recovery_percent))
    .finish()
    .unwrap()
}

struct CollectWriter {
    data: Rc<RefCell<Vec<u8>>>,
}

impl Write for CollectWriter {
    fn write(&mut self, buf: &[u8]) -> IoResult<usize> {
        self.data.borrow_mut().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> IoResult<()> {
        Ok(())
    }
}

fn collect_archive(
    archive: &Archive,
    options: ArchiveReadOptions<'_>,
) -> rars::Result<BTreeMap<Vec<u8>, Vec<u8>>> {
    let collected: Rc<RefCell<BTreeMap<Vec<u8>, Rc<RefCell<Vec<u8>>>>>> =
        Rc::new(RefCell::new(BTreeMap::new()));
    let collected_sink = Rc::clone(&collected);

    archive.extract_to_with_options(options, move |meta| {
        let file_data = Rc::new(RefCell::new(Vec::new()));
        collected_sink
            .borrow_mut()
            .insert(meta.name.clone(), Rc::clone(&file_data));
        Ok(Box::new(CollectWriter { data: file_data }))
    })?;

    let map = collected
        .borrow()
        .iter()
        .map(|(k, v)| (k.clone(), v.borrow().clone()))
        .collect();
    Ok(map)
}

#[test]
fn test_rar50_auto_repair_on_read() {
    let payload1 = b"first file payload protected by recovery records\n".repeat(32);
    let payload2 = b"second file payload also intact and protected\n".repeat(32);

    let entries = [
        entry(b"file1.txt", &payload1),
        entry(b"file2.txt", &payload2),
    ];
    let bytes = write_stored_archive_with_recovery(&entries, 20);

    let clean_rar50 = Rar50Archive::parse(&bytes).unwrap();
    let file1_range = clean_rar50.files().next().unwrap().block.data_range.clone();

    let mut damaged = bytes.clone();
    damaged[file1_range.start + 10..file1_range.start + 50].fill(0xa5);

    // Without auto-repair, reading and extracting fails with CRC mismatch.
    let damaged_archive = ArchiveReader::read_reader(Cursor::new(damaged.clone())).unwrap();
    assert!(!damaged_archive.is_overlaid());
    assert!(damaged_archive.has_recovery_record());
    assert!(collect_archive(&damaged_archive, ArchiveReadOptions::default()).is_err());

    // With auto-repair on read, transparent sparse overlay repairs damaged shards.
    let repaired_archive = ArchiveReader::read_source_with_options(
        Arc::new(SeekReader::new(Cursor::new(damaged))),
        ArchiveReadOptions::default().with_auto_repair(true),
    )
    .unwrap();

    assert!(repaired_archive.is_overlaid());
    let extracted = collect_archive(&repaired_archive, ArchiveReadOptions::default()).unwrap();
    assert_eq!(extracted.get(b"file1.txt".as_slice()).unwrap(), &payload1);
    assert_eq!(extracted.get(b"file2.txt".as_slice()).unwrap(), &payload2);
}

#[test]
fn test_rar50_auto_repair_on_extract() {
    let payload = b"critical data with structural recovery\n".repeat(64);
    let entries = [entry(b"document.bin", &payload)];
    let bytes = write_stored_archive_with_recovery(&entries, 20);

    let clean = Rar50Archive::parse(&bytes).unwrap();
    let data_range = clean.files().next().unwrap().block.data_range.clone();

    let mut damaged = bytes;
    damaged[data_range.start + 15..data_range.start + 70].fill(0xff);

    // Read archive without auto-repair
    let archive = ArchiveReader::read_reader(Cursor::new(damaged)).unwrap();
    assert!(!archive.is_overlaid());

    // Extracting with auto_repair = false fails
    assert!(collect_archive(&archive, ArchiveReadOptions::default()).is_err());

    // Extracting with auto_repair = true succeeds transparently!
    let extracted = collect_archive(
        &archive,
        ArchiveReadOptions::default().with_auto_repair(true),
    )
    .unwrap();
    assert_eq!(extracted.get(b"document.bin".as_slice()).unwrap(), &payload);
}

#[test]
fn test_rar50_repair_source_overlay() {
    let payload = b"testing repair_source helper for Borg/RARS\n".repeat(40);
    let entries = [entry(b"test.dat", &payload)];
    let bytes = write_stored_archive_with_recovery(&entries, 20);

    let clean = Rar50Archive::parse(&bytes).unwrap();
    let data_range = clean.files().next().unwrap().block.data_range.clone();

    let mut damaged = bytes;
    damaged[data_range.start + 20..data_range.start + 80].fill(0x33);

    let source: Arc<dyn ReadAt> = Arc::new(SeekReader::new(Cursor::new(damaged)));

    // Generate repaired source using Archive::repair_source
    let repaired_source =
        Archive::repair_source(source.clone(), ArchiveReadOptions::default()).unwrap();

    // Verify it is a RepairedReadAt
    let repaired_read_at = RepairedReadAt::new(source, BTreeMap::new());
    assert!(repaired_read_at.patches().is_empty());

    // The returned source is a RepairedReadAt overlay
    let archive = ArchiveReader::read_source(repaired_source).unwrap();
    let extracted = collect_archive(&archive, ArchiveReadOptions::default()).unwrap();
    assert_eq!(extracted.get(b"test.dat".as_slice()).unwrap(), &payload);
}

#[test]
fn test_rar250_protect_head_auto_repair() {
    let original = std::fs::read(fixture("rar15_40/rar250_protect_head_rr5.rar")).unwrap();
    let damage_offset = 512 + 16;
    let mut damaged = original.clone();
    damaged[damage_offset..damage_offset + 64].fill(0xa5);

    // Without auto-repair, reading and extracting fails
    let archive_unrepaired = ArchiveReader::read_reader(Cursor::new(damaged.clone())).unwrap();
    assert!(collect_archive(&archive_unrepaired, ArchiveReadOptions::default()).is_err());

    // With auto-repair on read:
    let archive_read = ArchiveReader::read_reader_with_options(
        Cursor::new(damaged.clone()),
        ArchiveReadOptions::default().with_auto_repair(true),
    )
    .unwrap();
    assert!(archive_read.is_overlaid());
    let extracted_read = collect_archive(&archive_read, ArchiveReadOptions::default()).unwrap();
    let file_data = extracted_read.get(b"BIG.BIN".as_slice()).unwrap();
    assert_eq!(crc32(file_data), 0x9a0e0c8c);

    // With auto-repair on extract:
    let extracted_on_extract = collect_archive(
        &archive_unrepaired,
        ArchiveReadOptions::default().with_auto_repair(true),
    )
    .unwrap();
    let file_data2 = extracted_on_extract.get(b"BIG.BIN".as_slice()).unwrap();
    assert_eq!(crc32(file_data2), 0x9a0e0c8c);
}

#[test]
fn test_rar300_newsub_auto_repair() {
    let original = std::fs::read(fixture("rar15_40/rar300/with_recovery_rar300.rar")).unwrap();
    let mut damaged = original.clone();
    damaged[512 + 16..512 + 80].fill(0xa5);

    // Without auto-repair, reading and extracting fails
    let archive_unrepaired = ArchiveReader::read_reader(Cursor::new(damaged.clone())).unwrap();
    assert!(collect_archive(&archive_unrepaired, ArchiveReadOptions::default()).is_err());

    // With auto-repair on read:
    let archive_read = ArchiveReader::read_reader_with_options(
        Cursor::new(damaged.clone()),
        ArchiveReadOptions::default().with_auto_repair(true),
    )
    .unwrap();
    assert!(archive_read.is_overlaid());
    let extracted_read = collect_archive(&archive_read, ArchiveReadOptions::default()).unwrap();
    let file_data = extracted_read.get(b"bigtext_64k.bin".as_slice()).unwrap();
    assert_eq!(crc32(file_data), 0xddc95682);

    // With auto-repair on extract:
    let extracted_on_extract = collect_archive(
        &archive_unrepaired,
        ArchiveReadOptions::default().with_auto_repair(true),
    )
    .unwrap();
    let file_data2 = extracted_on_extract
        .get(b"bigtext_64k.bin".as_slice())
        .unwrap();
    assert_eq!(crc32(file_data2), 0xddc95682);
}

#[test]
fn test_unrepairable_damage_and_error_callback_continuation() {
    let payload1 = b"first file will be destroyed beyond parity repair".repeat(16);
    let payload2 = b"second file is intact and should extract successfully".repeat(16);

    let entries = [
        entry(b"destroyed.txt", &payload1),
        entry(b"intact.txt", &payload2),
    ];
    let bytes = write_stored_archive_with_recovery(&entries, 10);

    let clean = Rar50Archive::parse(&bytes).unwrap();
    let file1_range = clean.files().next().unwrap().block.data_range.clone();
    let recovery_range = clean
        .services()
        .find(|service| service.name != b"RARS")
        .unwrap()
        .block
        .data_range
        .clone();

    let mut damaged = bytes;
    // Destroy file1 completely
    damaged[file1_range.clone()].fill(0xee);
    // Destroy recovery record so it cannot repair file1
    damaged[recovery_range].fill(0x00);

    let archive = ArchiveReader::read_reader(Cursor::new(damaged)).unwrap();

    // Verify recovery patches fail because recovery chunk is bad
    assert!(archive
        .recovery_patches_with_options(ArchiveReadOptions::default())
        .is_err());

    // Extract with error callback, choosing Continue on error
    let extracted: Rc<RefCell<BTreeMap<Vec<u8>, Rc<RefCell<Vec<u8>>>>>> =
        Rc::new(RefCell::new(BTreeMap::new()));
    let extracted_sink = Rc::clone(&extracted);

    let errors: Rc<RefCell<Vec<(Vec<u8>, String)>>> = Rc::new(RefCell::new(Vec::new()));
    let errors_sink = Rc::clone(&errors);

    let result = archive.extract_to_with_errors(
        ArchiveReadOptions::default().with_auto_repair(true),
        move |meta| {
            let data = Rc::new(RefCell::new(Vec::new()));
            extracted_sink
                .borrow_mut()
                .insert(meta.name.clone(), Rc::clone(&data));
            Ok(Box::new(CollectWriter { data }))
        },
        move |meta, err| {
            errors_sink
                .borrow_mut()
                .push((meta.meta.name.clone(), err.to_string()));
            Ok(ExtractionErrorAction::Continue)
        },
    );

    assert!(result.is_ok());

    let recorded_errors = errors.borrow().clone();
    assert_eq!(recorded_errors.len(), 1);
    assert_eq!(recorded_errors[0].0, b"destroyed.txt");

    let extracted_files = extracted.borrow();
    assert_eq!(
        extracted_files
            .get(b"intact.txt".as_slice())
            .unwrap()
            .borrow()
            .as_slice(),
        &payload2
    );
}

#[test]
fn test_multivolume_error_callback_continuation() {
    let mut sink = rar50::CollectedVolumes::new();
    let payload1 = b"file 1 corrupted in multivolume\n".repeat(20);
    let payload2 = b"file 2 intact in multivolume\n".repeat(20);

    let entries = [
        entry(b"corrupt.txt", &payload1),
        entry(b"intact.txt", &payload2),
    ];

    rar50::write_streaming_volumes_to(
        &entries,
        rar50::WriterOptions::new(ArchiveVersion::Rar50, FeatureSet::store_only())
            .with_compression_level(0),
        rar50::ArchiveExtras::default(),
        512,
        &mut sink,
        &rars::WriterResources::default(),
    )
    .unwrap();

    let mut parts = sink.take();
    assert!(parts.len() >= 2);

    // Corrupt payload of corrupt.txt in the first volume
    let range = Rar50Archive::parse(&parts[0])
        .unwrap()
        .files()
        .next()
        .unwrap()
        .block
        .data_range
        .clone();
    parts[0][range.start + 5..range.start + 25].fill(0xff);

    let archives: Vec<Rar50Archive> = parts
        .iter()
        .map(|part| Rar50Archive::parse(part).unwrap())
        .collect();

    let extracted: Rc<RefCell<BTreeMap<Vec<u8>, Rc<RefCell<Vec<u8>>>>>> =
        Rc::new(RefCell::new(BTreeMap::new()));
    let extracted_sink = Rc::clone(&extracted);

    let errors: Rc<RefCell<Vec<(Vec<u8>, String)>>> = Rc::new(RefCell::new(Vec::new()));
    let errors_sink = Rc::clone(&errors);

    let result = rar50::extract_volumes_to_with_errors(
        &archives,
        ArchiveReadOptions::default(),
        move |meta| {
            let data = Rc::new(RefCell::new(Vec::new()));
            extracted_sink
                .borrow_mut()
                .insert(meta.name.clone(), Rc::clone(&data));
            Ok(Box::new(CollectWriter { data }))
        },
        move |meta, err| {
            errors_sink
                .borrow_mut()
                .push((meta.meta.name.clone(), err.to_string()));
            Ok(ExtractionErrorAction::Continue)
        },
    );

    assert!(result.is_ok());

    let recorded_errors = errors.borrow().clone();
    assert_eq!(recorded_errors.len(), 1);
    assert_eq!(recorded_errors[0].0, b"corrupt.txt");

    let extracted_files = extracted.borrow();
    assert_eq!(
        extracted_files
            .get(b"intact.txt".as_slice())
            .unwrap()
            .borrow()
            .as_slice(),
        &payload2
    );
}

#[test]
fn test_rar300_newsub_recovery_streams_without_full_archive_read() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct BoundedReadAt {
        bytes: Vec<u8>,
        max_single_read: AtomicUsize,
    }

    impl ReadAt for BoundedReadAt {
        fn size(&self) -> std::io::Result<u64> {
            Ok(self.bytes.len() as u64)
        }

        fn read_at(&self, buf: &mut [u8], offset: u64) -> std::io::Result<usize> {
            self.max_single_read.fetch_max(buf.len(), Ordering::Relaxed);
            let start = usize::try_from(offset).unwrap_or(self.bytes.len());
            if start >= self.bytes.len() {
                return Ok(0);
            }
            let available = &self.bytes[start..];
            let n = available.len().min(buf.len());
            buf[..n].copy_from_slice(&available[..n]);
            Ok(n)
        }
    }

    let mut damaged = std::fs::read(fixture("rar15_40/rar300/with_recovery_rar300.rar")).unwrap();
    damaged[512 + 16..512 + 80].fill(0xa5);

    let source = Arc::new(BoundedReadAt {
        bytes: damaged,
        max_single_read: AtomicUsize::new(0),
    });
    let total_len = source.bytes.len();

    let archive = ArchiveReader::read_source(source.clone()).unwrap();
    source.max_single_read.store(0, Ordering::Relaxed);

    let patches = archive
        .recovery_patches_with_options(ArchiveReadOptions::new().with_auto_repair(true))
        .unwrap();
    assert!(!patches.is_empty());
    // Must stream in small chunks rather than reading the entire archive into a single Vec.
    let largest_read = source.max_single_read.load(Ordering::Relaxed);
    assert!(
        largest_read < total_len,
        "expected streaming reads < archive len {total_len}, got {largest_read}"
    );
}
