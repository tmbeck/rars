#![no_main]

use std::hint::black_box;
use std::io::{sink, Write};

use libfuzzer_sys::fuzz_target;
use rars::{rar15_40, rar50, Archive, ArchiveFamily, ArchiveReadOptions, ArchiveReader, Result};

// Parse arbitrary bytes and extract every member into a sink through the
// per-family entry points an embedding application drives, reading every
// metadata field on the way. Any panic here aborts the host process.
//
// Each input is parsed twice: in memory (`read`) and from a file
// (`read_path`), which walks headers and reads payloads independently. The
// files go to per-process names under the temp dir (set TMPDIR to a ramdisk).
//
// Volume path: byte 0 picks where the rest splits into two "volumes". To seed
// it from a two-volume fixture pair A, B: write `[x] + A + B`, choosing x (and
// zero padding after B) so that `x * (len(A) + len(B) + pad) / 256 == len(A)`.
fuzz_target!(|data: &[u8]| {
    for archive in parse(data, "whole.rar").into_iter().flatten() {
        extract(&[archive]);
    }
    if data.len() > 1 {
        let rest = &data[1..];
        let (first, second) = rest.split_at(usize::from(data[0]) * rest.len() / 256);
        let pairs = parse(first, "vol.part1.rar")
            .into_iter()
            .zip(parse(second, "vol.part2.rar"));
        for (a, b) in pairs {
            if let (Some(a), Some(b)) = (a, b) {
                if a.family() == b.family() {
                    extract(&[a, b]);
                }
            }
        }
    }
});

/// Parses `bytes` in memory and from a file: two independent header walks.
fn parse(bytes: &[u8], name: &str) -> [Option<Archive>; 2] {
    let path = std::env::temp_dir().join(format!("extract_sink-{}-{name}", std::process::id()));
    std::fs::write(&path, bytes).expect("write fuzz input file");
    [
        ArchiveReader::read(bytes).ok(),
        ArchiveReader::read_path(&path).ok(),
    ]
}

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
