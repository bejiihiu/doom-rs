//! Minimal RGBA image buffer and PNG writer.
//!
//! The engine speaks palette indices; PNG output is always 8-bit RGBA.
//! Palette applications happen in the command layer, this module only
//! knows how to turn a finished RGBA buffer into a PNG file.

use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

/// A plain 8-bit RGBA pixel buffer, rows top to bottom.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RgbaImage {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl RgbaImage {
    /// A zeroed (fully transparent black) image of `width` x `height`.
    ///
    /// Panics never: allocations here are tiny (largest dump is a
    /// 512x544 colormap stack).
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            pixels: vec![0; (width as usize) * (height as usize) * 4],
        }
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// The raw RGBA bytes, row major, 4 bytes per pixel.
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// Sets the pixel at `(x, y)`. Out-of-range coordinates are ignored
    /// (callers clip to the image bounds themselves; silently dropping
    /// a stray pixel is better than a panic in a dump tool).
    pub fn set_pixel(&mut self, x: u32, y: u32, rgba: [u8; 4]) {
        if x >= self.width || y >= self.height {
            return;
        }
        let offset = ((y * self.width + x) * 4) as usize;
        self.pixels[offset..offset + 4].copy_from_slice(&rgba);
    }

    /// Fills the `width` x `height` rectangle rooted at `(x, y)` with
    /// one color. Out-of-range areas are clipped.
    pub fn fill_rect(&mut self, x: u32, y: u32, width: u32, height: u32, rgba: [u8; 4]) {
        let x0 = x.min(self.width);
        let y0 = y.min(self.height);
        let x1 = x.saturating_add(width).min(self.width);
        let y1 = y.saturating_add(height).min(self.height);
        for row in y0..y1 {
            for col in x0..x1 {
                self.set_pixel(col, row, rgba);
            }
        }
    }
}

/// Writes `image` to `path` as an 8-bit RGBA PNG.
///
/// Fails (with a descriptive `Err`, never a panic) when the file cannot
/// be created or the encoder rejects the buffer — the caller maps that
/// to exit code 2.
pub fn write_png(path: &Path, image: &RgbaImage) -> Result<(), String> {
    let file =
        File::create(path).map_err(|e| format!("could not create {}: {e}", path.display()))?;
    let writer = BufWriter::new(file);

    let mut encoder = png::Encoder::new(writer, image.width(), image.height());
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder
        .write_header()
        .map_err(|e| format!("could not write PNG header to {}: {e}", path.display()))?;
    writer
        .write_image_data(image.pixels())
        .map_err(|e| format!("could not write PNG image data to {}: {e}", path.display()))?;
    Ok(())
}

/// The RGB triplet of `palette_color` as opaque RGBA.
pub fn opaque(color: [u8; 3]) -> [u8; 4] {
    [color[0], color[1], color[2], 255]
}

/// The PNG file signature; used by tests to smoke-check written files.
#[cfg(test)]
pub const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_starts_fully_transparent() {
        let image = RgbaImage::new(4, 3);
        assert_eq!(image.pixels().len(), 4 * 3 * 4);
        assert!(image.pixels().iter().all(|&b| b == 0));
    }

    #[test]
    fn set_pixel_roundtrips() {
        let mut image = RgbaImage::new(2, 2);
        image.set_pixel(1, 1, [1, 2, 3, 4]);
        // pixel (1, 1) in a 2x2 image lives at byte offset (1 * 2 + 1) * 4
        let offset = 12;
        assert_eq!(&image.pixels()[offset..offset + 4], &[1, 2, 3, 4]);
        // out-of-range writes are dropped, not fatal
        image.set_pixel(2, 0, [9, 9, 9, 9]);
        image.set_pixel(0, 2, [9, 9, 9, 9]);
        assert_eq!(image.pixels()[4..8], [0, 0, 0, 0]);
    }

    #[test]
    fn fill_rect_clips_to_bounds() {
        let mut image = RgbaImage::new(4, 4);
        image.fill_rect(2, 2, 10, 10, [7, 8, 9, 10]);
        let px = |x: u32, y: u32| {
            let offset = ((y * 4 + x) * 4) as usize;
            &image.pixels()[offset..offset + 4]
        };
        assert_eq!(px(3, 3), &[7, 8, 9, 10]);
        assert_eq!(px(2, 2), &[7, 8, 9, 10]);
        assert_eq!(px(1, 3), &[0, 0, 0, 0]);
    }

    #[test]
    fn write_png_produces_a_png_signature() {
        let mut image = RgbaImage::new(2, 1);
        image.set_pixel(0, 0, [255, 0, 0, 255]);
        image.set_pixel(1, 0, [0, 255, 0, 255]);

        let mut path = std::env::temp_dir();
        path.push(format!(
            "doom-asset-inspector-image-test-{}-{}.png",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        write_png(&path, &image).expect("write png");

        let bytes = std::fs::read(&path).expect("read back png");
        assert!(bytes.len() > PNG_SIGNATURE.len());
        assert_eq!(&bytes[..8], &PNG_SIGNATURE);
        // the IHDR chunk must follow the signature
        assert_eq!(&bytes[12..16], b"IHDR");
        let _ = std::fs::remove_file(&path);
    }
}
