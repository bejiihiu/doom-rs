//! Synthetic WAD builder for tests. Test-only, on purpose: if a real
//! packing tool is ever needed it can grow into one, but fixtures stay
//! close to the tests that use them for now.

/// Builds a well-formed WAD blob.
///
/// Lump payloads are laid out sequentially right after the 12-byte header;
/// the directory goes right after the last lump. Names are written verbatim
/// (truncated to eight bytes, NUL-padded) so tests can exercise lowercase
/// and mixed-case entries.
pub(crate) fn build_wad(magic: &[u8; 4], lumps: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(magic);
    out.extend_from_slice(&(lumps.len() as i32).to_le_bytes());
    let data_start = 12 + lumps.iter().map(|(_, data)| data.len()).sum::<usize>();
    out.extend_from_slice(&(data_start as i32).to_le_bytes());

    let mut records = Vec::with_capacity(lumps.len());
    for (name, data) in lumps {
        let file_pos = out.len();
        out.extend_from_slice(data);
        let mut name_bytes = [0u8; 8];
        for (dst, src) in name_bytes.iter_mut().zip(name.as_bytes().iter().take(8)) {
            *dst = *src;
        }
        records.push((file_pos as i32, data.len() as i32, name_bytes));
    }

    for (file_pos, size, name) in records {
        out.extend_from_slice(&file_pos.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&name);
    }
    out
}

/// Overwrites the `i32` at `offset` with `value`, for hand-crafting corrupt
/// fixtures in tests.
pub(crate) fn poke_i32(buf: &mut [u8], offset: usize, value: i32) {
    buf[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}
