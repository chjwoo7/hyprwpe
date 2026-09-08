//! Reader and decoder for Wallpaper Engine `.tex` texture containers.
//!
//! `.tex` files store textures used by scene materials, effects, and sprites.
//! They can contain uncompressed pixel data (RGBA8) or block-compressed
//! textures (DXT1/BC1, DXT3/BC2, DXT5/BC3).
//!
//! Decodes directly to 32-bit RGBA8888 for portable upload to OpenGL ES textures.

use anyhow::{bail, Context, Result};
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
    pub data: Vec<u8>,
}

impl TexImage {
    /// Read a `.tex` file from disk.
    pub fn open(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        Self::parse(&bytes).with_context(|| format!("parsing {}", path.display()))
    }

    /// Parse from memory buffer.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < 16 {
            bail!("file too small to be a .tex texture");
        }

        let mut pos = 0;

        // Check for length-prefixed version (e.g. 0x08, 0x00, 0x00, 0x00, "TEXV0001")
        // or bare 8-byte magic ("TEXV0001" or "TEXB0001")
        if bytes.starts_with(b"TEXV") || bytes.starts_with(b"TEXB") {
            pos += 8;
        } else if bytes.len() >= 12 && (&bytes[4..8] == b"TEXV" || &bytes[4..8] == b"TEXB") {
            let len = u32::from_le_bytes(bytes[0..4].try_into().unwrap()) as usize;
            pos += 4 + len;
        } else {
            bail!("unknown .tex magic; expected TEXV#### or TEXB####");
        }

        if bytes.len() < pos + 12 {
            bail!("truncated .tex header");
        }

        let format_id = u32::from_le_bytes(bytes[pos..pos + 4].try_into().unwrap());
        let width = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().unwrap());
        let height = u32::from_le_bytes(bytes[pos + 8..pos + 12].try_into().unwrap());
        pos += 12;

        // Skip optional mipmap/extra count if present
        if pos + 4 <= bytes.len() {
            let _extra = u32::from_le_bytes(bytes[pos..pos + 4].try_into().unwrap());
            pos += 4;
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

    /// Decode the texture into standard 32-bit RGBA8 pixels (`width * height * 4` bytes).
    pub fn to_rgba8(&self) -> Result<Vec<u8>> {
        let w = self.width as usize;
        let h = self.height as usize;
        let expected_pixels = w * h;

        match self.format {
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
        bail!("insufficient data for DXT1: needed {} bytes, got {}", block_count * 8, data.len());
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
                    alpha_palette[i + 1] = (((7 - i) as u16 * a0 as u16 + i as u16 * a1 as u16) / 7) as u8;
                }
            } else {
                for i in 1..5 {
                    alpha_palette[i + 1] = (((5 - i) as u16 * a0 as u16 + i as u16 * a1 as u16) / 5) as u8;
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
            255, 0, 0, 255,
            0, 255, 0, 255,
            0, 0, 255, 255,
            255, 255, 255, 255,
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
}
