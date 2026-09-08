//! Reader and decoder for Wallpaper Engine `.tex` texture containers.
//!
//! Two container generations exist in the wild:
//!
//! - **`TEXV0001`/`TEXB####` (old, direct):** 8-byte magic immediately followed
//!   by `format_id`, `width`, `height` and pixel/block data. This is the layout
//!   the earliest parser targeted.
//! - **`TEXV0005` (modern, wrapped):** a `TEXV0005` magic, then a `TEXI0001`
//!   info sub-block, then one `TEXB####` payload block. The `TEXB` revision
//!   selects the payload encoding:
//!   - `TEXB0003` / `TEXB0004`: the payload is an embedded PNG or JPEG image
//!     (the dominant case on the reference library: 376/997 files).
//!   - `TEXB0002`: raw block-compressed (BC/DXT) pixel data.
//!
//! This module decodes straight to 32-bit RGBA8888 for upload to OpenGL ES
//! textures. An embedded PNG/JPEG is handed to the `image` crate; raw BC data
//! is decompressed with the built-in DXT1/3/5 decoders.

use anyhow::{bail, Context, Result};
use image::RgbaImage;
use std::path::Path;

/// Texture compression formats supported by Wallpaper Engine `.tex` files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TexFormat {
    Rgba8,
    Rgb8,
    R8,
    Dxt1,
    Dxt3,
    Dxt5,
    /// Embedded PNG/JPEG payload (no raw BC decode needed).
    Image,
    Unknown(u32),
}

impl TexFormat {
    pub fn from_u32(val: u32) -> Self {
        match val {
            0 | 1 | 28 => TexFormat::Rgba8,
            2 | 29 => TexFormat::Rgb8,
            3 | 61 => TexFormat::R8,
            4 | 10 | 71 => TexFormat::Dxt1,
            5 | 11 | 74 => TexFormat::Dxt3,
            6 | 12 | 77 => TexFormat::Dxt5,
            other => TexFormat::Unknown(other),
        }
    }
}

/// A parsed texture from a `.tex` file.
#[derive(Debug, Clone)]
pub struct TexImage {
    pub width: u32,
    pub height: u32,
    pub format: TexFormat,
    /// Encoded payload: raw BC blocks, or the bytes of an embedded PNG/JPEG.
    pub data: Vec<u8>,
}

/// Parse a length-prefixed or bare `TEXV####`/`TEXB####` magic at the start of
/// `bytes`, returning the offset just past it.
fn skip_tex_magic(bytes: &[u8]) -> Result<usize> {
    if bytes.starts_with(b"TEXV") || bytes.starts_with(b"TEXB") {
        return Ok(8);
    }
    if bytes.len() >= 12 && (&bytes[4..8] == b"TEXV" || &bytes[4..8] == b"TEXB") {
        let len = u32::from_le_bytes(bytes[0..4].try_into().unwrap()) as usize;
        return Ok(4 + len);
    }
    bail!("unknown .tex magic; expected TEXV#### or TEXB####");
}

/// Locate an embedded image (PNG `\x89PNG` or JPEG `\xff\xd8\xff`) in `bytes`,
/// preferring one that starts after any `TEXB` block magic.
fn find_embedded_image(bytes: &[u8]) -> Option<(usize, bool)> {
    let png = find_sig(bytes, b"\x89PNG\r\n\x1a\n");
    let jpg = find_sig(bytes, b"\xff\xd8\xff");
    match (png, jpg) {
        (Some(p), Some(j)) => {
            // Prefer whichever comes first, but if one sits inside a TEXB
            // payload region prefer that one; both are inside the payload here.
            if p <= j {
                Some((p, true))
            } else {
                Some((j, false))
            }
        }
        (Some(p), None) => Some((p, true)),
        (None, Some(j)) => Some((j, false)),
        (None, None) => None,
    }
}

fn find_sig(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|w| w == needle)
}

impl TexImage {
    /// Read a `.tex` file from disk.
    pub fn open(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path).with_context(|| format!("reading {0}", path.display()))?;
        Self::parse(&bytes).with_context(|| format!("parsing {0}", path.display()))
    }

    /// Parse from memory buffer.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < 16 {
            bail!("file too small to be a .tex texture");
        }

        // Bare TEXV0005-style: outer magic, then optional TEXI info, then an
        // embedded image or a TEXB payload.
        if bytes.starts_with(b"TEXV0005") {
            return Self::parse_v5(bytes);
        }

        // Fall back to the original direct layout: magic, format, w, h, data.
        let pos = skip_tex_magic(bytes)?;
        if bytes.len() < pos + 12 {
            bail!("truncated .tex header");
        }

        let format_id = u32::from_le_bytes(bytes[pos..pos + 4].try_into().unwrap());
        let width = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().unwrap());
        let height = u32::from_le_bytes(bytes[pos + 8..pos + 12].try_into().unwrap());
        let mut pos = pos + 12;

        // Skip optional mipmap/extra count if present.
        if pos + 4 <= bytes.len() {
            let _extra = u32::from_le_bytes(bytes[pos..pos + 4].try_into().unwrap());
            pos += 4;
        }

        // A direct-layout .tex may itself embed a PNG/JPEG; prefer decoding
        // that rather than trying to interpret the raw blob.
        if let Some((start, _)) = find_embedded_image(&bytes[pos..]) {
            let image_start = pos + start;
            return Ok(TexImage {
                width,
                height,
                format: TexFormat::Image,
                data: bytes[image_start..].to_vec(),
            });
        }

        let format = TexFormat::from_u32(format_id);
        let data = bytes[pos..].to_vec();

        Ok(TexImage {
            width,
            height,
            format,
            data,
        })
    }

    /// Parse the modern `TEXV0005` container.
    ///
    /// Layout observed on the reference corpus (997 files, all `TEXV0005`):
    ///
    /// ```text
    /// char[8]  magic                "TEXV0005"
    /// char[1]  0x00
    /// char[8]  sub-magic            "TEXI0001"
    /// u32      flags                usually 0
    /// u32      ???                  often 512
    /// u32      width  (fixed 8.24?  width  * 256)
    /// u32      height (fixed 8.24?  height * 256)
    /// u32      ???                  often width * 256 again
    /// u32      ???                  often height * 256 again
    /// char[8]  payload magic        "TEXB0003" / "TEXB0004" / "TEXB0002"
    /// ...
    /// ```
    ///
    /// The first reliable signal is the embedded PNG/JPEG: when present, the
    /// texture decodes through the `image` crate regardless of the exact field
    /// layout. Raw-BC payloads (`TEXB0002`) are parsed with a best-effort
    /// header of `u32[2]=256` (fixed 1.0), `u32[2]=width<<8`, `u32[3]=height<<8`,
    /// then pixel data follows.
    fn parse_v5(bytes: &[u8]) -> Result<Self> {
        let _magic = &bytes[0..8];

        // Embedded image is authoritative for dimensions and pixels.
        if let Some((start, _)) = find_embedded_image(bytes) {
            let img = image::load_from_memory(&bytes[start..])
                .context("decoding embedded image in .tex")?;
            let rgba = img.to_rgba8();
            let dims = rgba.dimensions();
            drop(rgba);
            return Ok(TexImage {
                width: dims.0,
                height: dims.1,
                format: TexFormat::Image,
                data: bytes[start..].to_vec(),
            });
        }

        // No embedded image: raw pixel/block data. Wallpaper Engine TEX headers
        // store the size in more than one place and the layout differs between
        // revisions (some store w/h * 256, some raw). The only consistent
        // ground truth is the payload bytes themselves: for block-compressed
        // data, `ceil(w/4)*ceil(h/4)` blocks occupy a known byte count. So we
        // collect candidate (w, h) pairs from every plausible header field and
        // keep the one whose payload length exactly matches a known format.
        let (width, height, data_start, format) = dims_from_payload(bytes)?;

        Ok(TexImage {
            width,
            height,
            format,
            data: bytes[data_start..].to_vec(),
        })
    }

    /// Decode the texture into standard 32-bit RGBA8 pixels (`width * height * 4` bytes).
        pub fn to_rgba8(&self) -> Result<Vec<u8>> {
            let w = self.width as usize;
            let h = self.height as usize;
            let expected_pixels = w * h;

            match self.format {
                // Embedded image payloads decode through the image crate.
                TexFormat::Image => {
                    let img = image::load_from_memory(&self.data)
                        .context("decoding embedded image payload")?;
                    Ok(img.to_rgba8().into_raw())
                }
                TexFormat::Rgba8 => {
                    if self.data.len() >= expected_pixels * 4 {
                        Ok(self.data[..expected_pixels * 4].to_vec())
                    } else {
                        bail!("insufficient data for RGBA8 texture");
                    }
                }
                TexFormat::Rgb8 => {
                    if self.data.len() < expected_pixels * 3 {
                        bail!("insufficient data for RGB8 texture");
                    }
                    let mut out = Vec::with_capacity(expected_pixels * 4);
                    for rgb in self.data[..expected_pixels * 3].chunks_exact(3) {
                        out.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
                    }
                    Ok(out)
                }
                TexFormat::R8 => {
                    if self.data.len() < expected_pixels {
                        bail!("insufficient data for R8 texture");
                    }
                    let mut out = Vec::with_capacity(expected_pixels * 4);
                    for &r in &self.data[..expected_pixels] {
                        out.extend_from_slice(&[r, r, r, 255]);
                    }
                    Ok(out)
                }
                TexFormat::Dxt1 => decompress_dxt1(self.width, self.height, &self.data),
                TexFormat::Dxt3 => decompress_dxt3(self.width, self.height, &self.data),
                TexFormat::Dxt5 => decompress_dxt5(self.width, self.height, &self.data),
                TexFormat::Unknown(fmt) => {
                    bail!("unsupported texture format {fmt}");
                }
            }
        }

    /// Decode to an RGBA image, using the image crate for embedded payloads
    /// and the DXT decoders for raw block data.
    pub fn to_rgba_image(&self) -> Result<RgbaImage> {
        let raw = self.to_rgba8()?;
        RgbaImage::from_raw(self.width, self.height, raw)
            .context("decoded dimensions do not match declared size")
    }
}

/// Determine dimensions and payload layout for a `TEXV0005` with no embedded
/// image, by validating candidate (width, height) pairs against the actual
/// payload byte count.
///
/// Header layouts in the corpus are inconsistent: some revisions store
/// dimensions as 16.16 fixed-point (`value * 256`), some store them raw, and
/// the TEXB sub-block can precede the pixels with a header of varying length.
/// The one invariant is the payload itself: for a block-compressed texture,
/// `ceil(w/4) * ceil(h/4)` blocks occupy `bytes_per_block` each, and for
/// uncompressed RGBA/R8 the bytes are `w * h * bpp`. So every candidate pair
/// whose remaining bytes (from some plausible payload start) exactly match a
/// known encoding is accepted.
fn dims_from_payload(bytes: &[u8]) -> Result<(u32, u32, usize, TexFormat)> {
    let Some(tb) = find_texb(bytes) else {
        bail!("TEXV0005 with no TEXB payload");
    };
    // Search the header region after the TEXB magic for (w, h) candidates.
    let header_end = (tb + 8 + 96).min(bytes.len());

    let mut candidates: Vec<(u32, u32)> = Vec::new();
    let mut i = 17usize;
    while i + 8 <= header_end {
        let a = u32::from_le_bytes(bytes[i..i + 4].try_into().unwrap());
        let b = u32::from_le_bytes(bytes[i + 4..i + 8].try_into().unwrap());
        // Raw and 16.16 forms.
        for (w, h) in [(a, b), (a >> 8, b >> 8), (b, a), (b >> 8, a >> 8)] {
            if (1..=1 << 15).contains(&w) && (1..=1 << 15).contains(&h) {
                candidates.push((w, h));
            }
        }
        i += 4;
    }

    // Payload starts: any 4-byte boundary shortly after the TEXB magic.
    let starts: Vec<usize> = (0..header_end - (tb + 8))
        .step_by(4)
        .map(|o| tb + 8 + o)
        .collect();

    let encodings: [(TexFormat, u64); 5] = [
        (TexFormat::Dxt1, 8),
        (TexFormat::Dxt5, 16),
        (TexFormat::Dxt3, 16),
        // RGBA8 / R8 raw: bpp 4 / 1
        (TexFormat::Rgba8, 4),
        (TexFormat::R8, 1),
    ];

    // Prefer block formats first (they are the common case), then raw.
    for (fmt, bpp) in encodings.iter() {
        for &(w, h) in &candidates {
            let want = if matches!(fmt, TexFormat::Rgba8 | TexFormat::R8) {
                (w as u64) * (h as u64) * bpp
            } else {
                blocks_for(w, h) * bpp
            };
            for &start in &starts {
                let rest = bytes.len() - start;
                if rest as u64 == want {
                    return Ok((w, h, start, *fmt));
                }
            }
        }
    }

    // No exact match: fall back to the first plausible header dims and leave
    // the whole TEXB payload as-is; to_rgba8 will report what is wrong.
    if let Some(&(w, h)) = candidates.first() {
        return Ok((w, h, tb + 8, TexFormat::Dxt5));
    }
    bail!("could not determine .tex dimensions from payload")
}

fn blocks_for(width: u32, height: u32) -> u64 {
    ((width as u64 + 3) / 4) * ((height as u64 + 3) / 4)
}

fn find_texb(bytes: &[u8]) -> Option<usize> {
    bytes
        .windows(8)
        .position(|w| w.starts_with(b"TEXB") && w[4..8].iter().all(|b| b.is_ascii_digit()))
}

trait FormatFallback {
    fn or_else<F: FnOnce() -> Self>(self, f: F) -> Self;
}
impl FormatFallback for TexFormat {
    fn or_else<F: FnOnce() -> Self>(self, f: F) -> Self {
        match self {
            TexFormat::Unknown(_) => f(),
            other => other,
        }
    }
}

/// Convert 16-bit RGB565 to 24-bit RGB888.
fn decode_rgb565(c: u16) -> [u8; 3] {
    let r = (((c >> 11) & 0x1F) * 527 + 23) >> 6;
    let g = (((c >> 5) & 0x3F) * 259 + 33) >> 6;
    let b = ((c & 0x1F) * 527 + 23) >> 6;
    [r as u8, g as u8, b as u8]
}

/// Decompress DXT1 / BC1 texture to RGBA8888.
pub fn decompress_dxt1(width: u32, height: u32, data: &[u8]) -> Result<Vec<u8>> {
    let bw = (width as usize + 3) / 4;
    let bh = (height as usize + 3) / 4;
    let block_count = bw * bh;
    if data.len() < block_count * 8 {
        bail!(
            "insufficient data for DXT1: needed {} bytes, got {}",
            block_count * 8,
            data.len()
        );
    }

    let mut out = vec![0u8; (width * height * 4) as usize];
    let mut offset = 0;

    for by in 0..bh {
        for bx in 0..bw {
            let c0 = u16::from_le_bytes(data[offset..offset + 2].try_into().unwrap());
            let c1 = u16::from_le_bytes(data[offset + 2..offset + 4].try_into().unwrap());
            let lookup = u32::from_le_bytes(data[offset + 4..offset + 8].try_into().unwrap());
            offset += 8;

            let rgb0 = decode_rgb565(c0);
            let rgb1 = decode_rgb565(c1);

            let mut palette = [[0u8; 4]; 4];
            palette[0] = [rgb0[0], rgb0[1], rgb0[2], 255];
            palette[1] = [rgb1[0], rgb1[1], rgb1[2], 255];

            if c0 > c1 {
                palette[2] = [
                    ((2 * rgb0[0] as u16 + rgb1[0] as u16) / 3) as u8,
                    ((2 * rgb0[1] as u16 + rgb1[1] as u16) / 3) as u8,
                    ((2 * rgb0[2] as u16 + rgb1[2] as u16) / 3) as u8,
                    255,
                ];
                palette[3] = [
                    ((rgb0[0] as u16 + 2 * rgb1[0] as u16) / 3) as u8,
                    ((rgb0[1] as u16 + 2 * rgb1[1] as u16) / 3) as u8,
                    ((rgb0[2] as u16 + 2 * rgb1[2] as u16) / 3) as u8,
                    255,
                ];
            } else {
                palette[2] = [
                    ((rgb0[0] as u16 + rgb1[0] as u16) / 2) as u8,
                    ((rgb0[1] as u16 + rgb1[1] as u16) / 2) as u8,
                    ((rgb0[2] as u16 + rgb1[2] as u16) / 2) as u8,
                    255,
                ];
                palette[3] = [0, 0, 0, 0];
            }

            for py in 0..4 {
                let y = by * 4 + py;
                if y >= height as usize {
                    continue;
                }
                for px in 0..4 {
                    let x = bx * 4 + px;
                    if x >= width as usize {
                        continue;
                    }
                    let bit_idx = (py * 4 + px) * 2;
                    let code = ((lookup >> bit_idx) & 0x03) as usize;
                    let pixel = palette[code];
                    let out_idx = (y * width as usize + x) * 4;
                    out[out_idx..out_idx + 4].copy_from_slice(&pixel);
                }
            }
        }
    }

    Ok(out)
}

/// Decompress DXT3 / BC2 texture to RGBA8888.
pub fn decompress_dxt3(width: u32, height: u32, data: &[u8]) -> Result<Vec<u8>> {
    let bw = (width as usize + 3) / 4;
    let bh = (height as usize + 3) / 4;
    let block_count = bw * bh;
    if data.len() < block_count * 16 {
        bail!("insufficient data for DXT3");
    }

    let mut out = vec![0u8; (width * height * 4) as usize];
    let mut offset = 0;

    for by in 0..bh {
        for bx in 0..bw {
            let alpha_bytes = &data[offset..offset + 8];
            let c0 = u16::from_le_bytes(data[offset + 8..offset + 10].try_into().unwrap());
            let c1 = u16::from_le_bytes(data[offset + 10..offset + 12].try_into().unwrap());
            let lookup = u32::from_le_bytes(data[offset + 12..offset + 16].try_into().unwrap());
            offset += 16;

            let rgb0 = decode_rgb565(c0);
            let rgb1 = decode_rgb565(c1);

            let mut palette = [[0u8; 3]; 4];
            palette[0] = rgb0;
            palette[1] = rgb1;
            palette[2] = [
                ((2 * rgb0[0] as u16 + rgb1[0] as u16) / 3) as u8,
                ((2 * rgb0[1] as u16 + rgb1[1] as u16) / 3) as u8,
                ((2 * rgb0[2] as u16 + rgb1[2] as u16) / 3) as u8,
            ];
            palette[3] = [
                ((rgb0[0] as u16 + 2 * rgb1[0] as u16) / 3) as u8,
                ((rgb0[1] as u16 + 2 * rgb1[1] as u16) / 3) as u8,
                ((rgb0[2] as u16 + 2 * rgb1[2] as u16) / 3) as u8,
            ];

            for py in 0..4 {
                let y = by * 4 + py;
                if y >= height as usize {
                    continue;
                }
                for px in 0..4 {
                    let x = bx * 4 + px;
                    if x >= width as usize {
                        continue;
                    }
                    let pixel_idx = py * 4 + px;
                    let alpha_nibble = (alpha_bytes[pixel_idx / 2] >> ((pixel_idx % 2) * 4)) & 0x0F;
                    let alpha = alpha_nibble * 17; // 0..15 -> 0..255

                    let code = ((lookup >> (pixel_idx * 2)) & 0x03) as usize;
                    let rgb = palette[code];

                    let out_idx = (y * width as usize + x) * 4;
                    out[out_idx] = rgb[0];
                    out[out_idx + 1] = rgb[1];
                    out[out_idx + 2] = rgb[2];
                    out[out_idx + 3] = alpha;
                }
            }
        }
    }

    Ok(out)
}

/// Decompress DXT5 / BC3 texture to RGBA8888.
pub fn decompress_dxt5(width: u32, height: u32, data: &[u8]) -> Result<Vec<u8>> {
    let bw = (width as usize + 3) / 4;
    let bh = (height as usize + 3) / 4;
    let block_count = bw * bh;
    if data.len() < block_count * 16 {
        bail!("insufficient data for DXT5");
    }

    let mut out = vec![0u8; (width * height * 4) as usize];
    let mut offset = 0;

    for by in 0..bh {
        for bx in 0..bw {
            let a0 = data[offset];
            let a1 = data[offset + 1];
            let alpha_bits = u64::from_le_bytes([
                data[offset + 2],
                data[offset + 3],
                data[offset + 4],
                data[offset + 5],
                data[offset + 6],
                data[offset + 7],
                0,
                0,
            ]);

            let c0 = u16::from_le_bytes(data[offset + 8..offset + 10].try_into().unwrap());
            let c1 = u16::from_le_bytes(data[offset + 10..offset + 12].try_into().unwrap());
            let lookup = u32::from_le_bytes(data[offset + 12..offset + 16].try_into().unwrap());
            offset += 16;

            let mut alpha_palette = [0u8; 8];
            alpha_palette[0] = a0;
            alpha_palette[1] = a1;
            if a0 > a1 {
                for i in 1..7 {
                    alpha_palette[i + 1] =
                        (((7 - i) as u16 * a0 as u16 + i as u16 * a1 as u16) / 7) as u8;
                }
            } else {
                for i in 1..5 {
                    alpha_palette[i + 1] =
                        (((5 - i) as u16 * a0 as u16 + i as u16 * a1 as u16) / 5) as u8;
                }
                alpha_palette[6] = 0;
                alpha_palette[7] = 255;
            }

            let rgb0 = decode_rgb565(c0);
            let rgb1 = decode_rgb565(c1);

            let mut palette = [[0u8; 3]; 4];
            palette[0] = rgb0;
            palette[1] = rgb1;
            palette[2] = [
                ((2 * rgb0[0] as u16 + rgb1[0] as u16) / 3) as u8,
                ((2 * rgb0[1] as u16 + rgb1[1] as u16) / 3) as u8,
                ((2 * rgb0[2] as u16 + rgb1[2] as u16) / 3) as u8,
            ];
            palette[3] = [
                ((rgb0[0] as u16 + 2 * rgb1[0] as u16) / 3) as u8,
                ((rgb0[1] as u16 + 2 * rgb1[1] as u16) / 3) as u8,
                ((rgb0[2] as u16 + 2 * rgb1[2] as u16) / 3) as u8,
            ];

            for py in 0..4 {
                let y = by * 4 + py;
                if y >= height as usize {
                    continue;
                }
                for px in 0..4 {
                    let x = bx * 4 + px;
                    if x >= width as usize {
                        continue;
                    }
                    let pixel_idx = py * 4 + px;
                    let alpha_code = ((alpha_bits >> (pixel_idx * 3)) & 0x07) as usize;
                    let alpha = alpha_palette[alpha_code];

                    let code = ((lookup >> (pixel_idx * 2)) & 0x03) as usize;
                    let rgb = palette[code];

                    let out_idx = (y * width as usize + x) * 4;
                    out[out_idx] = rgb[0];
                    out[out_idx + 1] = rgb[1];
                    out[out_idx + 2] = rgb[2];
                    out[out_idx + 3] = alpha;
                }
            }
        }
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_raw_rgba8_tex() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"TEXV0001");
        bytes.extend_from_slice(&0u32.to_le_bytes()); // Format: Rgba8
        bytes.extend_from_slice(&2u32.to_le_bytes()); // Width: 2
        bytes.extend_from_slice(&2u32.to_le_bytes()); // Height: 2
        bytes.extend_from_slice(&0u32.to_le_bytes()); // Extra

        // 4 pixels RGBA8
        let pixel_data = [
            255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
        ];
        bytes.extend_from_slice(&pixel_data);

        let tex = TexImage::parse(&bytes).expect("valid tex header");
        assert_eq!(tex.width, 2);
        assert_eq!(tex.height, 2);
        assert_eq!(tex.format, TexFormat::Rgba8);

        let rgba = tex.to_rgba8().expect("decodes to rgba");
        assert_eq!(rgba, pixel_data);
    }

    #[test]
    fn decompress_simple_dxt1_block() {
        // Red color (RGB565: 0xF800), Green color (RGB565: 0x07E0)
        let c0: u16 = 0xF800; // Red
        let c1: u16 = 0x07E0; // Green
        let lookup: u32 = 0x00000000; // All pixels use color 0

        let mut block = Vec::new();
        block.extend_from_slice(&c0.to_le_bytes());
        block.extend_from_slice(&c1.to_le_bytes());
        block.extend_from_slice(&lookup.to_le_bytes());

        let decoded = decompress_dxt1(4, 4, &block).expect("dxt1 decompress");
        assert_eq!(decoded.len(), 4 * 4 * 4);

        // Every pixel should be pure red (255, 0, 0, 255)
        for px in decoded.chunks_exact(4) {
            assert_eq!(px[0], 255);
            assert_eq!(px[1], 0);
            assert_eq!(px[2], 0);
            assert_eq!(px[3], 255);
        }
    }

    #[test]
    fn parse_dims_from_v5_payload() {
        // A TEXV0005 containing a raw DXT5 payload: the pixel data is exactly
        // ceil(32/4)*ceil(32/4) blocks * 16 bytes = 8*8*16 = 1024 bytes.
        let mut header = Vec::new();
        header.extend_from_slice(b"TEXV0005");
        header.push(0);
        header.extend_from_slice(b"TEXI0001");
        header.extend_from_slice(&0u32.to_le_bytes());
        header.extend_from_slice(&512u32.to_le_bytes());
        header.extend_from_slice(&(32u32 << 8).to_le_bytes()); // w * 256
        header.extend_from_slice(&(32u32 << 8).to_le_bytes()); // h * 256
        // Inner sub-block with a plausible header + DXT5 payload.
        header.extend_from_slice(b"TEXB0003");
        header.extend_from_slice(&0x100u32.to_le_bytes());
        header.extend_from_slice(&0xffffffffu32.to_le_bytes());
        header.extend_from_slice(&1u32.to_le_bytes());
        header.extend_from_slice(&32u32.to_le_bytes()); // w
        header.extend_from_slice(&32u32.to_le_bytes()); // h
        header.extend_from_slice(&1u32.to_le_bytes()); // mip count
        header.extend_from_slice(&1024u32.to_le_bytes()); // data length
        header.extend_from_slice(&[0u8; 1024]); // DXT5 pixels (all opaque)

        let tex = TexImage::parse(&header).expect("parse");
        assert_eq!((tex.width, tex.height), (32, 32));
        assert_eq!(tex.format, TexFormat::Dxt5);
        let rgba = tex.to_rgba8().expect("decode dxt5");
        assert_eq!(rgba.len(), 32 * 32 * 4);
    }
}
