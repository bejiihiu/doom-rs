//! Little-endian reads and writes on byte slices.
//!
//! DOOM's WAD archives store every multi-byte field little-endian, and the
//! lumps map straight onto flat C structs. This module is the safe boundary
//! between raw lump bytes and typed values: every access is bounds-checked
//! and returns [`crate::errors::EngineResult`] instead of panicking on corrupt input.
//!
//! Reads copy out of the slice; no references into it are kept.

use crate::errors::{EngineError, EngineResult};

/// Size of `read_i16`/`read_u16`/`write_i16`/`write_u16` in bytes.
const U16_SIZE: usize = 2;
/// Size of `read_i32`/`read_u32`/`write_i32`/`write_u32` in bytes.
const U32_SIZE: usize = 4;

/// Checks that `[offset, offset + size)` lies inside `buf`.
///
/// Does the arithmetic with checked ops so a bogus huge `offset` cannot
/// wrap around and sneak past the bounds check.
fn check_bounds(buf: &[u8], offset: usize, size: usize) -> EngineResult<()> {
    let end = offset.checked_add(size).ok_or(EngineError::LumpRead {
        offset,
        needed: size,
        available: buf.len().saturating_sub(offset),
    })?;
    if end > buf.len() {
        return Err(EngineError::LumpRead {
            offset,
            needed: size,
            available: buf.len().saturating_sub(offset),
        });
    }
    Ok(())
}

/// Reads a signed 16-bit little-endian integer at `offset`.
pub fn read_i16(buf: &[u8], offset: usize) -> EngineResult<i16> {
    Ok(read_u16(buf, offset)? as i16)
}

/// Reads an unsigned 16-bit little-endian integer at `offset`.
pub fn read_u16(buf: &[u8], offset: usize) -> EngineResult<u16> {
    check_bounds(buf, offset, U16_SIZE)?;
    Ok(u16::from_le_bytes([buf[offset], buf[offset + 1]]))
}

/// Reads a signed 32-bit little-endian integer at `offset`.
pub fn read_i32(buf: &[u8], offset: usize) -> EngineResult<i32> {
    Ok(read_u32(buf, offset)? as i32)
}

/// Reads an unsigned 32-bit little-endian integer at `offset`.
pub fn read_u32(buf: &[u8], offset: usize) -> EngineResult<u32> {
    check_bounds(buf, offset, U32_SIZE)?;
    Ok(u32::from_le_bytes([
        buf[offset],
        buf[offset + 1],
        buf[offset + 2],
        buf[offset + 3],
    ]))
}

/// Writes a signed 16-bit little-endian integer at `offset`.
pub fn write_i16(buf: &mut [u8], offset: usize, value: i16) -> EngineResult<()> {
    write_u16(buf, offset, value as u16)
}

/// Writes an unsigned 16-bit little-endian integer at `offset`.
pub fn write_u16(buf: &mut [u8], offset: usize, value: u16) -> EngineResult<()> {
    check_bounds(buf, offset, U16_SIZE)?;
    buf[offset..offset + U16_SIZE].copy_from_slice(&value.to_le_bytes());
    Ok(())
}

/// Writes a signed 32-bit little-endian integer at `offset`.
pub fn write_i32(buf: &mut [u8], offset: usize, value: i32) -> EngineResult<()> {
    write_u32(buf, offset, value as u32)
}

/// Writes an unsigned 32-bit little-endian integer at `offset`.
pub fn write_u32(buf: &mut [u8], offset: usize, value: u32) -> EngineResult<()> {
    check_bounds(buf, offset, U32_SIZE)?;
    buf[offset..offset + U32_SIZE].copy_from_slice(&value.to_le_bytes());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_u16_decodes_little_endian() {
        let buf = [0x34, 0x12];
        assert_eq!(read_u16(&buf, 0).unwrap(), 0x1234);
    }

    #[test]
    fn read_i16_decodes_negative_values() {
        // 0xFF9C little-endian is -100
        let buf = [0x9C, 0xFF];
        assert_eq!(read_i16(&buf, 0).unwrap(), -100);
    }

    #[test]
    fn read_u32_decodes_little_endian() {
        let buf = [0x78, 0x56, 0x34, 0x12];
        assert_eq!(read_u32(&buf, 0).unwrap(), 0x1234_5678);
    }

    #[test]
    fn read_i32_decodes_negative_values() {
        // 0xFFFF_FF9C little-endian is -100
        let buf = [0x9C, 0xFF, 0xFF, 0xFF];
        assert_eq!(read_i32(&buf, 0).unwrap(), -100);
    }

    #[test]
    fn reads_at_a_nonzero_offset() {
        let buf = [0xAA, 0x34, 0x12, 0xBB];
        assert_eq!(read_u16(&buf, 1).unwrap(), 0x1234);
    }

    #[test]
    fn truncated_slice_returns_err_not_panic() {
        let buf = [0x01];
        let err = read_u16(&buf, 0).unwrap_err();
        assert!(matches!(err, EngineError::LumpRead { .. }));

        let buf = [0x01, 0x02, 0x03];
        let err = read_u32(&buf, 0).unwrap_err();
        assert!(matches!(err, EngineError::LumpRead { .. }));
    }

    #[test]
    fn read_at_the_exact_end_boundary_works() {
        let buf = [0xAA, 0xBB, 0x34, 0x12];
        assert_eq!(read_u16(&buf, 2).unwrap(), 0x1234);
    }

    #[test]
    fn read_one_byte_past_the_end_fails() {
        let buf = [0xAA, 0xBB, 0x34];
        assert!(read_u16(&buf, 2).is_err());

        let buf = [0xAA, 0xBB, 0x34, 0x12, 0x56];
        assert!(read_u32(&buf, 2).is_err());
    }

    #[test]
    fn huge_offset_does_not_wrap_around() {
        let buf = [0u8; 8];
        assert!(read_u32(&buf, usize::MAX).is_err());
        assert!(read_u16(&buf, usize::MAX - 1).is_err());
        assert!(write_u32(&mut [0u8; 8], usize::MAX, 0).is_err());
    }

    #[test]
    fn error_reports_offset_needed_and_available() {
        let buf = [0u8; 6];
        let err = read_u32(&buf, 4).unwrap_err();
        match err {
            EngineError::LumpRead {
                offset,
                needed,
                available,
            } => {
                assert_eq!(offset, 4);
                assert_eq!(needed, 4);
                assert_eq!(available, 2);
            }
            other => panic!("expected LumpRead, got {other:?}"),
        }
    }

    #[test]
    fn write_u16_roundtrips_with_read() {
        let mut buf = [0u8; 4];
        write_u16(&mut buf, 1, 0xBEEF).unwrap();
        assert_eq!(read_u16(&buf, 1).unwrap(), 0xBEEF);
        assert_eq!(buf, [0x00, 0xEF, 0xBE, 0x00]);
    }

    #[test]
    fn write_i16_roundtrips_negative_values() {
        let mut buf = [0u8; 2];
        write_i16(&mut buf, 0, -1234).unwrap();
        assert_eq!(read_i16(&buf, 0).unwrap(), -1234);
    }

    #[test]
    fn write_u32_roundtrips_with_read() {
        let mut buf = [0u8; 6];
        write_u32(&mut buf, 1, 0xDEAD_BEEF).unwrap();
        assert_eq!(read_u32(&buf, 1).unwrap(), 0xDEAD_BEEF);
        assert_eq!(buf, [0x00, 0xEF, 0xBE, 0xAD, 0xDE, 0x00]);
    }

    #[test]
    fn write_i32_roundtrips_negative_values() {
        let mut buf = [0u8; 4];
        write_i32(&mut buf, 0, -100_000).unwrap();
        assert_eq!(read_i32(&buf, 0).unwrap(), -100_000);
    }

    #[test]
    fn write_past_the_end_returns_err_and_does_not_touch_memory() {
        let mut buf = [0xAAu8; 3];
        assert!(write_u16(&mut buf, 2, 0x1234).is_err());
        assert_eq!(buf, [0xAA, 0xAA, 0xAA]);

        let mut buf = [0xAAu8; 3];
        assert!(write_u32(&mut buf, 0, 0x1234_5678).is_err());
        assert_eq!(buf, [0xAA, 0xAA, 0xAA]);
    }

    #[test]
    fn wad_header_shaped_read() {
        // a minimal fake "IWAD" header field set, all little-endian
        let buf = b"IWAD\x03\x00\x00\x00\x0c\x00\x00\x00";
        assert_eq!(&buf[..4], b"IWAD");
        assert_eq!(read_u32(buf, 4).unwrap(), 3);
        assert_eq!(read_u32(buf, 8).unwrap(), 12);
    }
}
