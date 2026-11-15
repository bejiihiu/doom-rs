//! Sprite graphics.
//!
//! A sprite is not a separate format: it is an ordinary patch lump whose
//! *name* carries the actor, frame and rotation information. The naming
//! convention is `SSSSFFFF[S]`:
//!
//! - `SSSS` — four-character actor name, e.g. `POSS`;
//! - `F`   — frame letter;
//! - rotation — the viewing angle, `1`..`8`, or `0` meaning "the same
//!   picture is used for all angles";
//! - optionally a second frame/rotation pair, marking the picture that
//!   doubles as the mirrored half of the sprite.
//!
//! Decoding is exactly [`decode_patch`](crate::gfx::patch::decode_patch); there is nothing sprite-specific
//! in the byte format. The frame/rotation tables (`spritedef_t` in the
//! original) belong to the sprite rendering work and are intentionally not
//! part of this module. Sprite namespace handling (`S_START`/`S_END`)
//! lives in the `wad` module's combined namespace.
//!
//! Sprites are loaded as raw patches through the resource cache like
//! everything else; the re-exports below give sprite call sites a natural
//! import point.

pub use super::patch::{Patch, PatchColumn, PatchHeader, Post, decode_patch as decode_sprite};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sprite_decodes_as_a_patch() {
        // minimal valid patch: 1x1, one pixel
        let mut data = Vec::new();
        data.extend_from_slice(&1i16.to_le_bytes()); // width
        data.extend_from_slice(&1i16.to_le_bytes()); // height
        data.extend_from_slice(&0i16.to_le_bytes()); // left offset
        data.extend_from_slice(&0i16.to_le_bytes()); // top offset
        data.extend_from_slice(&12i32.to_le_bytes()); // column 0 offset
        data.push(0); // topdelta
        data.push(1); // length
        data.push(0); // pad
        data.push(200); // pixel
        data.push(0); // pad
        data.push(0xff); // end of column

        let patch = decode_sprite(&data).unwrap();
        assert_eq!(patch.header.width, 1);
        assert_eq!(patch.pixels().next().unwrap().value, 200);
    }
}
