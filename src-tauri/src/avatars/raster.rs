// SPDX-License-Identifier: MIT
//! Raster faces (PNG, WebP): read their size, decode them, and write the small
//! square PNG the agents list draws at 15–28px.

use std::io::Cursor;

use super::{FaceFormat, MAX_SIDE};

/// Pixels as 8-bit RGBA, row-major.
pub struct Rgba {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// The width and height a PNG or WebP file declares, without decoding it.
pub fn dimensions(format: FaceFormat, bytes: &[u8]) -> Result<(u32, u32), String> {
    match format {
        FaceFormat::Png => {
            let reader = png::Decoder::new(Cursor::new(bytes))
                .read_info()
                .map_err(|e| format!("not a readable PNG: {e}"))?;
            let info = reader.info();
            Ok((info.width, info.height))
        }
        FaceFormat::Webp => {
            let decoder = image_webp::WebPDecoder::new(Cursor::new(bytes))
                .map_err(|e| format!("not a readable WebP: {e}"))?;
            Ok(decoder.dimensions())
        }
        FaceFormat::Svg => Err("an SVG has no pixel size".to_string()),
    }
}

/// Decode a PNG or WebP (the first frame of an animated one) to RGBA. Refuses
/// anything larger than [`MAX_SIDE`] before allocating for it.
pub fn decode(format: FaceFormat, bytes: &[u8]) -> Result<Rgba, String> {
    let (width, height) = dimensions(format, bytes)?;
    if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
        return Err(format!(
            "{width}×{height}px is outside 1–{MAX_SIDE}px per side"
        ));
    }
    match format {
        FaceFormat::Png => decode_png(bytes),
        FaceFormat::Webp => decode_webp(bytes),
        FaceFormat::Svg => Err("an SVG is not a raster image".to_string()),
    }
}

fn decode_png(bytes: &[u8]) -> Result<Rgba, String> {
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder
        .read_info()
        .map_err(|e| format!("not a readable PNG: {e}"))?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let frame = reader
        .next_frame(&mut buf)
        .map_err(|e| format!("not a readable PNG: {e}"))?;
    buf.truncate(frame.buffer_size());
    let pixels = match frame.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => buf
            .chunks_exact(3)
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect(),
        png::ColorType::Grayscale => buf.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::GrayscaleAlpha => buf
            .chunks_exact(2)
            .flat_map(|p| [p[0], p[0], p[0], p[1]])
            .collect(),
        png::ColorType::Indexed => return Err("unexpected indexed PNG output".to_string()),
    };
    Ok(Rgba {
        width: frame.width,
        height: frame.height,
        pixels,
    })
}

fn decode_webp(bytes: &[u8]) -> Result<Rgba, String> {
    let mut decoder = image_webp::WebPDecoder::new(Cursor::new(bytes))
        .map_err(|e| format!("not a readable WebP: {e}"))?;
    let (width, height) = decoder.dimensions();
    let size = decoder
        .output_buffer_size()
        .ok_or_else(|| "WebP too large".to_string())?;
    let mut buf = vec![0; size];
    decoder
        .read_image(&mut buf)
        .map_err(|e| format!("not a readable WebP: {e}"))?;
    let pixels = if decoder.has_alpha() {
        buf
    } else {
        buf.chunks_exact(3)
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect()
    };
    Ok(Rgba {
        width,
        height,
        pixels,
    })
}

/// The centre square of `image`, scaled down to at most `side` px by averaging
/// (alpha-weighted, so transparent edges do not darken). Never scales up.
pub fn small_square(image: &Rgba, side: u32) -> Rgba {
    let crop = image.width.min(image.height);
    let x0 = (image.width - crop) / 2;
    let y0 = (image.height - crop) / 2;
    let out_side = crop.min(side).max(1);
    let scale = crop as f64 / out_side as f64;
    let mut pixels = Vec::with_capacity((out_side * out_side * 4) as usize);
    for oy in 0..out_side {
        let sy0 = (oy as f64 * scale).floor() as u32;
        let sy1 = (((oy + 1) as f64 * scale).ceil() as u32).clamp(sy0 + 1, crop);
        for ox in 0..out_side {
            let sx0 = (ox as f64 * scale).floor() as u32;
            let sx1 = (((ox + 1) as f64 * scale).ceil() as u32).clamp(sx0 + 1, crop);
            let (mut r, mut g, mut b, mut a, mut n) = (0u64, 0u64, 0u64, 0u64, 0u64);
            for sy in sy0..sy1 {
                for sx in sx0..sx1 {
                    let i = (((y0 + sy) * image.width + (x0 + sx)) * 4) as usize;
                    let alpha = image.pixels[i + 3] as u64;
                    r += image.pixels[i] as u64 * alpha;
                    g += image.pixels[i + 1] as u64 * alpha;
                    b += image.pixels[i + 2] as u64 * alpha;
                    a += alpha;
                    n += 1;
                }
            }
            if a == 0 {
                pixels.extend_from_slice(&[0, 0, 0, 0]);
            } else {
                pixels.extend_from_slice(&[
                    ((r + a / 2) / a) as u8,
                    ((g + a / 2) / a) as u8,
                    ((b + a / 2) / a) as u8,
                    ((a + n / 2) / n) as u8,
                ]);
            }
        }
    }
    Rgba {
        width: out_side,
        height: out_side,
        pixels,
    }
}

/// `image` as a PNG file.
pub fn encode_png(image: &Rgba) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, image.width, image.height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
        writer
            .write_image_data(&image.pixels)
            .map_err(|e| e.to_string())?;
    }
    Ok(out)
}
