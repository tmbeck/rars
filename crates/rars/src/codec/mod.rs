//! RAR compression codecs, filters, PPMd, and RARVM components used by `rars`.

mod fast;
pub(crate) mod filters;
mod huffman;
mod match_finder;
mod ppmd;
pub mod rar13;
pub mod rar20;
pub mod rar29;
pub mod rar50;
pub mod rarvm;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    InvalidData(&'static str),
    NeedMoreInput,
    Cancelled,
    /// The caller's reader or writer failed; its error, unchanged.
    Io(crate::error::IoError),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidData(msg) => write!(f, "{msg}"),
            Self::NeedMoreInput => write!(f, "codec input is truncated"),
            Self::Cancelled => f.write_str("codec operation was cancelled"),
            Self::Io(e) => write!(f, "I/O error: {}", e.message),
        }
    }
}

impl std::error::Error for Error {}

/// Hands out one byte per `read`, so a decoder that refills its input
/// crosses every boundary a larger read could hide.
#[cfg(test)]
pub(crate) struct OneByteReader<'a>(pub(crate) &'a [u8]);

#[cfg(test)]
impl std::io::Read for OneByteReader<'_> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if self.0.is_empty() || out.is_empty() {
            return Ok(0);
        }
        out[0] = self.0[0];
        self.0 = &self.0[1..];
        Ok(1)
    }
}

/// Text-like bytes (4 bits of entropy) with `0xE8` opcodes sprinkled in, so
/// packed members are large and an E8 filter has work to do.
#[cfg(test)]
pub(crate) fn refill_test_bytes(n: usize) -> Vec<u8> {
    let mut x = 0x2545_f491_4f6c_dd1du64;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            if x.is_multiple_of(9) {
                0xe8
            } else {
                b"ACGTacgtNnRrYyKk"[(x >> 60) as usize]
            }
        })
        .collect()
}
