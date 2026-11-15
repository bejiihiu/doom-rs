//! Shared bounds-checked helpers for the map decoders.
//!
//! Every read goes through [`crate::swap`]; this module only re-stamps
//! the failures with the lump name so the errors stay actionable.
//! Callers pre-validate the lump size, so [`MapError::Truncated`] is
//! a defensive arm, not a hot path.

use crate::errors::EngineError;
use crate::swap;

use super::error::{MapError, MapResult};

/// Number of whole records in `data`, or an error when the size is
/// not a multiple of `stride`. Lump sizes are small enough that the
/// division itself cannot overflow.
pub(crate) fn record_count(lump: &'static str, data: &[u8], stride: usize) -> MapResult<usize> {
    if data.len() % stride != 0 {
        return Err(MapError::BadSize {
            lump,
            size: data.len(),
            reason: "lump size is not a multiple of the record size",
        });
    }
    Ok(data.len() / stride)
}

/// Reads a signed 16-bit little-endian integer at `offset`,
/// re-stamping bounds failures with the lump name.
pub(crate) fn rd_i16(lump: &'static str, data: &[u8], offset: usize) -> MapResult<i16> {
    swap::read_i16(data, offset).map_err(|err| restamp(lump, err))
}

/// Reads an unsigned 16-bit little-endian integer at `offset`,
/// re-stamping bounds failures with the lump name.
pub(crate) fn rd_u16(lump: &'static str, data: &[u8], offset: usize) -> MapResult<u16> {
    swap::read_u16(data, offset).map_err(|err| restamp(lump, err))
}

/// Validates an on-disk reference index against the number of entries
/// it points at. `raw` is taken as `i64` so negative on-disk values
/// are reported verbatim instead of wrapping.
pub(crate) fn check_index(
    lump: &'static str,
    what: &'static str,
    raw: i64,
    count: usize,
) -> MapResult<usize> {
    if raw < 0 || raw >= count as i64 {
        return Err(MapError::OutOfRange {
            lump,
            what,
            index: raw,
            count,
        });
    }
    Ok(raw as usize)
}

/// Converts a shared-swap bounds failure into a lump-named error.
fn restamp(lump: &'static str, err: EngineError) -> MapError {
    match err {
        EngineError::LumpRead {
            offset,
            needed,
            available,
        } => MapError::Truncated {
            lump,
            offset,
            needed,
            available,
        },
        other => MapError::Engine(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_count_rejects_ragged_lumps() {
        let err = record_count("VERTEXES", &[0u8; 5], 4).unwrap_err();
        assert!(matches!(err, MapError::BadSize { size: 5, .. }));
    }

    #[test]
    fn record_count_divides_evenly() {
        assert_eq!(record_count("VERTEXES", &[0u8; 16], 4).unwrap(), 4);
        assert_eq!(record_count("VERTEXES", &[], 4).unwrap(), 0);
    }

    #[test]
    fn check_index_reports_the_offending_value() {
        let err = check_index("LINEDEFS", "vertex", -1, 4).unwrap_err();
        assert!(matches!(
            err,
            MapError::OutOfRange {
                what: "vertex",
                index: -1,
                count: 4,
                ..
            }
        ));

        let err = check_index("LINEDEFS", "vertex", 4, 4).unwrap_err();
        assert!(matches!(err, MapError::OutOfRange { index: 4, .. }));
    }
}
