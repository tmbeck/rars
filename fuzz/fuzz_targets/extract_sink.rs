#![no_main]

use std::hint::black_box;
use std::io::{sink, Write};

use libfuzzer_sys::fuzz_target;
use rars::{rar15_40, rar50, Archive, ArchiveFamily, ArchiveReadOptions, ArchiveReader, Result};

// Parse arbitrary bytes and extract every member into a sink through the
// per-family entry points an embedding application drives, reading every
// metadata field on the way. Any panic here aborts the host process.
fuzz_target!(|data: &[u8]| {
    if let Ok(archive) = ArchiveReader::read(data) {
        extract(std::slice::from_ref(&archive));
    }
    // Volume path: split at an input-chosen offset into two "volumes".
    if data.len() > 1 {
        let rest = &data[1..];
        let (first, second) = rest.split_at(usize::from(data[0]) * rest.len() / 256);
        if let (Ok(a), Ok(b)) = (ArchiveReader::read(first), ArchiveReader::read(second)) {
            if a.family() == b.family() {
                extract(&[a, b]);
            }
        }
    }
});

fn open<M: std::fmt::Debug>(meta: &M) -> Result<Box<dyn Write>> {
    black_box(format!("{meta:?}"));
    Ok(Box::new(sink()))
}

fn extract(volumes: &[Archive]) {
    let opts = ArchiveReadOptions::new;
    match volumes[0].family() {
        ArchiveFamily::Rar50Plus => {
            let link = |meta: &rar50::ExtractedEntryMeta, redirection: &rar50::FileRedirection| {
                black_box(format!("{meta:?} {redirection:?}"));
                Ok(())
            };
            if let [single] = volumes {
                let _ = single
                    .as_rar50()
                    .unwrap()
                    .extract_to_with_redirections(opts(), open, link);
            } else {
                let vols: Vec<_> = volumes
                    .iter()
                    .filter_map(|a| a.as_rar50().cloned())
                    .collect();
                let _ = rar50::extract_volumes_to_with_redirections(&vols, opts(), open, link);
            }
        }
        ArchiveFamily::Rar15To40 => {
            if let [single] = volumes {
                let _ = single.as_rar15_40().unwrap().extract_to(opts(), open);
            } else {
                let vols: Vec<_> = volumes
                    .iter()
                    .filter_map(|a| a.as_rar15_40().cloned())
                    .collect();
                let _ = rar15_40::extract_volumes_to(&vols, opts(), open);
            }
        }
        _ => {}
    }
}
