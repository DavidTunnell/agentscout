use anyhow::{Context, Result};
use image::{ImageBuffer, ImageEncoder, Rgba, RgbaImage};
use std::collections::HashSet;
use xcap::Monitor;

#[derive(Debug, Clone)]
pub struct MonitorCapture {
    pub monitor_id: u32,
    pub image: RgbaImage,
    pub x: i32,
    pub y: i32,
}

/// Abstraction over monitor enumeration and capture. Production uses
/// [`XcapScreenshotter`]; tests inject [`FakeScreenshotter`] to exercise
/// the full pipeline without a real display.
pub trait Screenshotter: Send + Sync {
    fn list_monitors(&self) -> Result<Vec<MonitorInfo>>;
    fn capture_enabled(&self, enabled_ids: &[u32]) -> Result<Vec<MonitorCapture>>;
}

/// Stable signature describing the current monitor topology — used by
/// the scheduler to detect hot-plug events between ticks. Two monitor
/// sets with the same signature are equivalent for capture purposes.
pub fn monitor_topology_signature(monitors: &[MonitorInfo]) -> String {
    let mut sorted: Vec<&MonitorInfo> = monitors.iter().collect();
    sorted.sort_by_key(|m| m.id);
    sorted
        .iter()
        .map(|m| format!("{}:{}x{}@{},{}", m.id, m.width, m.height, m.x, m.y))
        .collect::<Vec<_>>()
        .join("|")
}

pub struct XcapScreenshotter;

impl XcapScreenshotter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for XcapScreenshotter {
    fn default() -> Self {
        Self::new()
    }
}

impl Screenshotter for XcapScreenshotter {
    fn list_monitors(&self) -> Result<Vec<MonitorInfo>> {
        list_monitors()
    }

    fn capture_enabled(&self, enabled_ids: &[u32]) -> Result<Vec<MonitorCapture>> {
        capture_enabled(enabled_ids)
    }
}

/// Returns pre-baked synthetic captures. For integration tests that need
/// to exercise the post-capture pipeline (encryption, OCR, thumbnail) on
/// machines without a display.
pub struct FakeScreenshotter {
    pub monitors: Vec<MonitorInfo>,
    pub captures: Vec<MonitorCapture>,
}

impl FakeScreenshotter {
    pub fn single(width: u32, height: u32, fill: [u8; 4]) -> Self {
        let img: RgbaImage = ImageBuffer::from_pixel(width, height, Rgba(fill));
        let info = MonitorInfo {
            id: 0,
            name: "fake-primary".into(),
            x: 0,
            y: 0,
            width,
            height,
        };
        let cap = MonitorCapture {
            monitor_id: 0,
            image: img,
            x: 0,
            y: 0,
        };
        Self {
            monitors: vec![info],
            captures: vec![cap],
        }
    }
}

impl Screenshotter for FakeScreenshotter {
    fn list_monitors(&self) -> Result<Vec<MonitorInfo>> {
        Ok(self.monitors.clone())
    }

    fn capture_enabled(&self, enabled_ids: &[u32]) -> Result<Vec<MonitorCapture>> {
        let enabled: HashSet<u32> = enabled_ids.iter().copied().collect();
        Ok(self
            .captures
            .iter()
            .filter(|c| enabled.contains(&c.monitor_id))
            .cloned()
            .collect())
    }
}

pub fn list_monitors() -> Result<Vec<MonitorInfo>> {
    let monitors = Monitor::all().context("enumerating monitors via xcap")?;
    Ok(monitors
        .into_iter()
        .enumerate()
        .map(|(idx, m)| MonitorInfo {
            id: idx as u32,
            name: m.name().unwrap_or_else(|_| format!("Monitor {idx}")),
            x: m.x().unwrap_or(0),
            y: m.y().unwrap_or(0),
            width: m.width().unwrap_or(0),
            height: m.height().unwrap_or(0),
        })
        .collect())
}

#[derive(Debug, Clone)]
pub struct MonitorInfo {
    pub id: u32,
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

pub fn capture_enabled(enabled_ids: &[u32]) -> Result<Vec<MonitorCapture>> {
    let enabled: HashSet<u32> = enabled_ids.iter().copied().collect();
    let monitors = Monitor::all().context("enumerating monitors via xcap")?;
    let mut out = Vec::new();
    for (idx, monitor) in monitors.into_iter().enumerate() {
        let id = idx as u32;
        if !enabled.contains(&id) {
            continue;
        }
        let image = monitor
            .capture_image()
            .with_context(|| format!("capturing monitor {}", id))?;
        out.push(MonitorCapture {
            monitor_id: id,
            x: monitor.x().unwrap_or(0),
            y: monitor.y().unwrap_or(0),
            image,
        });
    }
    Ok(out)
}

pub fn tile_horizontally(mut captures: Vec<MonitorCapture>) -> Option<RgbaImage> {
    if captures.is_empty() {
        return None;
    }
    if captures.len() == 1 {
        return captures.pop().map(|c| c.image);
    }
    captures.sort_by_key(|c| c.x);
    let total_width: u32 = captures.iter().map(|c| c.image.width()).sum();
    let max_height: u32 = captures.iter().map(|c| c.image.height()).max().unwrap_or(0);
    let mut tiled: RgbaImage =
        ImageBuffer::from_pixel(total_width, max_height, Rgba([0, 0, 0, 255]));
    let mut cursor_x: u32 = 0;
    for cap in captures {
        image::imageops::overlay(&mut tiled, &cap.image, cursor_x as i64, 0);
        cursor_x += cap.image.width();
    }
    Some(tiled)
}

pub fn encode_png(image: &RgbaImage) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    image::codecs::png::PngEncoder::new_with_quality(
        &mut buf,
        image::codecs::png::CompressionType::Fast,
        image::codecs::png::FilterType::Adaptive,
    )
    .write_image(
        image.as_raw(),
        image.width(),
        image.height(),
        image::ExtendedColorType::Rgba8,
    )?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tile_single_returns_same_dims() {
        let img: RgbaImage = ImageBuffer::from_pixel(100, 50, Rgba([255, 0, 0, 255]));
        let cap = MonitorCapture {
            monitor_id: 0,
            image: img,
            x: 0,
            y: 0,
        };
        let tiled = tile_horizontally(vec![cap]).unwrap();
        assert_eq!(tiled.dimensions(), (100, 50));
    }

    #[test]
    fn tile_two_sums_widths_uses_max_height() {
        let a: RgbaImage = ImageBuffer::from_pixel(100, 60, Rgba([255, 0, 0, 255]));
        let b: RgbaImage = ImageBuffer::from_pixel(80, 90, Rgba([0, 255, 0, 255]));
        let tiled = tile_horizontally(vec![
            MonitorCapture {
                monitor_id: 0,
                image: a,
                x: 0,
                y: 0,
            },
            MonitorCapture {
                monitor_id: 1,
                image: b,
                x: 100,
                y: 0,
            },
        ])
        .unwrap();
        assert_eq!(tiled.dimensions(), (180, 90));
    }

    #[test]
    fn tile_sorts_by_x_coordinate() {
        let left: RgbaImage = ImageBuffer::from_pixel(10, 10, Rgba([1, 0, 0, 255]));
        let right: RgbaImage = ImageBuffer::from_pixel(10, 10, Rgba([2, 0, 0, 255]));
        let tiled = tile_horizontally(vec![
            MonitorCapture {
                monitor_id: 0,
                image: right.clone(),
                x: 100,
                y: 0,
            },
            MonitorCapture {
                monitor_id: 1,
                image: left.clone(),
                x: -100,
                y: 0,
            },
        ])
        .unwrap();
        assert_eq!(tiled.get_pixel(0, 0)[0], 1);
        assert_eq!(tiled.get_pixel(10, 0)[0], 2);
    }

    #[test]
    fn encode_png_roundtrips() {
        let img: RgbaImage = ImageBuffer::from_pixel(20, 20, Rgba([10, 20, 30, 255]));
        let bytes = encode_png(&img).unwrap();
        let decoded = image::load_from_memory(&bytes).unwrap().to_rgba8();
        assert_eq!(decoded.dimensions(), (20, 20));
        assert_eq!(decoded.get_pixel(0, 0)[0], 10);
    }

    fn info(id: u32, w: u32, h: u32, x: i32, y: i32) -> MonitorInfo {
        MonitorInfo {
            id,
            name: format!("M{id}"),
            width: w,
            height: h,
            x,
            y,
        }
    }

    #[test]
    fn topology_signature_stable_across_reorderings() {
        let a = vec![info(0, 1920, 1080, 0, 0), info(1, 2560, 1440, 1920, 0)];
        let b = vec![info(1, 2560, 1440, 1920, 0), info(0, 1920, 1080, 0, 0)];
        assert_eq!(
            monitor_topology_signature(&a),
            monitor_topology_signature(&b)
        );
    }

    #[test]
    fn topology_signature_changes_on_hotplug() {
        let before = vec![info(0, 1920, 1080, 0, 0)];
        let after = vec![info(0, 1920, 1080, 0, 0), info(1, 2560, 1440, 1920, 0)];
        assert_ne!(
            monitor_topology_signature(&before),
            monitor_topology_signature(&after)
        );
    }

    #[test]
    fn topology_signature_changes_on_resolution_change() {
        let before = vec![info(0, 1920, 1080, 0, 0)];
        let after = vec![info(0, 3840, 2160, 0, 0)];
        assert_ne!(
            monitor_topology_signature(&before),
            monitor_topology_signature(&after)
        );
    }
}
