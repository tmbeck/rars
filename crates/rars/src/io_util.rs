use crate::read_at::ReadAt;
use crate::{Error, Result};

pub(crate) fn read_exact_at(src: &dyn ReadAt, offset: usize, len: usize) -> Result<Vec<u8>> {
    let mut data = vec![0; len];
    let mut filled = 0;
    while filled < len {
        match src.read_at(&mut data[filled..], (offset + filled) as u64) {
            Ok(0) => return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof).into()),
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(data)
}

pub(crate) fn read_u16(input: &[u8], offset: usize) -> Result<u16> {
    let end = offset.checked_add(2).ok_or(Error::TooShort)?;
    let bytes = input.get(offset..end).ok_or(Error::TooShort)?;
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}

pub(crate) fn read_u32(input: &[u8], offset: usize) -> Result<u32> {
    let end = offset.checked_add(4).ok_or(Error::TooShort)?;
    let bytes = input.get(offset..end).ok_or(Error::TooShort)?;
    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

pub(crate) fn align16(value: usize, overflow_message: &'static str) -> Result<usize> {
    value
        .checked_add(15)
        .map(|value| value & !15)
        .ok_or(Error::InvalidHeader(overflow_message))
}
