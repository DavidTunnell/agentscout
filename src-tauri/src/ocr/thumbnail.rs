use anyhow::{Context, Result};
use image::{codecs::webp::WebPEncoder, ExtendedColorType, ImageEncoder, RgbaImage};
use std::io::Cursor;

pub const DEFAULT_MAX_DIMENSION: u32 = 400;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThumbnailFormat {
    WebP,
    Png,
}

impl ThumbnailFormat {
    pub fn extension(&self) -> &'static str {
        match self {
            ThumbnailFormat::WebP => "webp",
            ThumbnailFormat::Png => "png",
        }
    }
}

/// Generate a thumbnail with the longest edge clamped to `max_dimension`.
/// Aspect ratio is preserved. WebP output is significantly smaller than
/// PNG for screenshot-like content.
pub fn generate_thumbnail(
    source_png: &[u8],
    max_dimension: u32,
    format: ThumbnailFormat,
) -> Result<Vec<u8>> {
    let img = image::load_from_memory(source_png).context("decoding source image")?;
    let (w, h) = (img.width(), img.height());

    let resized = if w <= max_dimension && h <= max_dimension {
        img
    } else {
        let scale = max_dimension as f32 / w.max(h) as f32;
        let new_w = (w as f32 * scale).round() as u32;
        let new_h = (h as f32 * scale).round() as u32;
        img.resize_exact(new_w, new_h, image::imageops::FilterType::Triangle)
    };

    let rgba: RgbaImage = resized.into_rgba8();
    let (w, h) = rgba.dimensions();

    let mut out = Vec::new();
    match format {
        ThumbnailFormat::WebP => {
            let encoder = WebPEncoder::new_lossless(Cursor::new(&mut out));
            encoder
                .write_image(rgba.as_raw(), w, h, ExtendedColorType::Rgba8)
                .context("encoding thumbnail as WebP")?;
        }
        ThumbnailFormat::Png => {
            image::codecs::png::PngEncoder::new_with_quality(
                Cursor::new(&mut out),
                image::codecs::png::CompressionType::Best,
                image::codecs::png::FilterType::Adaptive,
            )
            .write_image(rgba.as_raw(), w, h, ExtendedColorType::Rgba8)
            .context("encoding thumbnail as PNG")?;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, Rgba};

    fn synth_png(w: u32, h: u32) -> Vec<u8> {
        let img: RgbaImage = ImageBuffer::from_pixel(w, h, Rgba([10, 20, 30, 255]));
        let mut buf = Vec::new();
        image::codecs::png::PngEncoder::new_with_quality(
            Cursor::new(&mut buf),
            image::codecs::png::CompressionType::Fast,
            image::codecs::png::FilterType::NoFilter,
        )
        .write_image(img.as_raw(), w, h, ExtendedColorType::Rgba8)
        .unwrap();
        buf
    }

    #[test]
    fn downscales_landscape_to_max_dimension() {
        let src = synth_png(2000, 1000);
        let thumb = generate_thumbnail(&src, 400, ThumbnailFormat::Png).unwrap();
        let decoded = image::load_from_memory(&thumb).unwrap();
        assert_eq!(decoded.width(), 400);
        assert_eq!(decoded.height(), 200);
    }

    #[test]
    fn downscales_portrait_to_max_dimension() {
        let src = synth_png(800, 2000);
        let thumb = generate_thumbnail(&src, 400, ThumbnailFormat::Png).unwrap();
        let decoded = image::load_from_memory(&thumb).unwrap();
        assert_eq!(decoded.width(), 160);
        assert_eq!(decoded.height(), 400);
    }

    #[test]
    fn does_not_upscale_smaller_images() {
        let src = synth_png(100, 80);
        let thumb = generate_thumbnail(&src, 400, ThumbnailFormat::Png).unwrap();
        let decoded = image::load_from_memory(&thumb).unwrap();
        assert_eq!(decoded.width(), 100);
        assert_eq!(decoded.height(), 80);
    }

    #[test]
    fn webp_output_is_smaller_than_png_for_screenshots() {
        let src = synth_png(1920, 1080);
        let png_thumb = generate_thumbnail(&src, 400, ThumbnailFormat::Png).unwrap();
        let webp_thumb = generate_thumbnail(&src, 400, ThumbnailFormat::WebP).unwrap();
        assert!(
            webp_thumb.len() < png_thumb.len(),
            "expected WebP {} < PNG {}",
            webp_thumb.len(),
            png_thumb.len()
        );
    }
}
