//! Reading source images from disk or memory, including SVG documents, which are rasterized
//! straight at the size the render needs instead of being resampled from a fixed bitmap.

use std::path::Path;

use anyhow::{Context, Result};
use image::RgbaImage;

use crate::preprocess::{FitMode, PreprocessOptions, plan_dimensions};

/// Whether `path` names an SVG file (by its `.svg` extension).
#[must_use]
pub fn is_svg_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("svg"))
}

/// Pixel size to rasterize an SVG whose natural size is `natural` so that `plan`'s resize step
/// has nothing left to resample: the planned output size, or the covering size for
/// `--fit cover` so the crop stays centred.
#[must_use]
pub fn svg_raster_size(natural: (u32, u32), plan: Option<&PreprocessOptions>) -> (u32, u32) {
    let Some(plan) = plan else {
        return natural;
    };
    let (nw, nh) = (natural.0.max(1) as f32, natural.1.max(1) as f32);
    if let (Some(tw), Some(th), FitMode::Cover) = (plan.width, plan.height, plan.fit) {
        let ratio = (tw as f32 / nw).max(th as f32 / nh);
        return (
            ((nw * ratio).ceil() as u32).max(tw),
            ((nh * ratio).ceil() as u32).max(th),
        );
    }
    plan_dimensions(natural.0, natural.1, plan)
}

#[cfg(feature = "svg")]
fn decode_svg(bytes: &[u8], plan: Option<&PreprocessOptions>) -> Result<RgbaImage> {
    let (w, h) = svg_raster_size(crate::svg::svg_size(bytes)?, plan);
    crate::svg::rasterize(bytes, w, h)
}

#[cfg(not(feature = "svg"))]
fn decode_svg(_bytes: &[u8], _plan: Option<&PreprocessOptions>) -> Result<RgbaImage> {
    anyhow::bail!("this build of dith-core has no SVG support (enable the \"svg\" feature)")
}

fn looks_like_svg(bytes: &[u8]) -> bool {
    #[cfg(feature = "svg")]
    {
        crate::svg::is_svg_data(bytes)
    }
    #[cfg(not(feature = "svg"))]
    {
        let head = String::from_utf8_lossy(&bytes[..bytes.len().min(1024)]);
        head.contains("<svg")
    }
}

/// Decode encoded image bytes (PNG, JPEG, WebP, GIF, BMP, TIFF or SVG). An SVG is rasterized at
/// the size `plan` asks for (its natural size when `plan` is `None`), keeping transparency.
///
/// # Errors
/// Returns an error if the bytes are not a supported image.
pub fn decode_image_bytes(bytes: &[u8], plan: Option<&PreprocessOptions>) -> Result<RgbaImage> {
    if looks_like_svg(bytes) {
        return decode_svg(bytes, plan);
    }
    Ok(image::load_from_memory(bytes)
        .context("decoding image bytes")?
        .into_rgba8())
}

/// Load an image file. SVG files (by extension or content) are rasterized at the size `plan`
/// asks for, so `--width`, `--height` and `--max-dim` give crisp vector edges.
///
/// # Errors
/// Returns an error if the file cannot be read or decoded.
pub fn load_image(path: &Path, plan: Option<&PreprocessOptions>) -> Result<RgbaImage> {
    if is_svg_path(path) {
        let bytes =
            std::fs::read(path).with_context(|| format!("reading SVG '{}'", path.display()))?;
        return decode_svg(&bytes, plan)
            .with_context(|| format!("rasterizing '{}'", path.display()));
    }
    Ok(image::open(path)
        .with_context(|| format!("opening input image '{}'", path.display()))?
        .into_rgba8())
}

/// Natural pixel dimensions of an image file (an SVG's `width`/`height` or `viewBox`).
///
/// # Errors
/// Returns an error if the file cannot be read or is not a supported image.
pub fn image_dimensions(path: &Path) -> Result<(u32, u32)> {
    if is_svg_path(path) {
        let bytes =
            std::fs::read(path).with_context(|| format!("reading SVG '{}'", path.display()))?;
        #[cfg(feature = "svg")]
        return crate::svg::svg_size(&bytes)
            .with_context(|| format!("reading the size of '{}'", path.display()));
        #[cfg(not(feature = "svg"))]
        return decode_svg(&bytes, None).map(|i| i.dimensions());
    }
    image::image_dimensions(path)
        .with_context(|| format!("reading dimensions of '{}'", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn svg_raster_size_follows_the_plan() {
        let plan = PreprocessOptions {
            width: Some(300),
            ..PreprocessOptions::default()
        };
        assert_eq!(svg_raster_size((100, 50), Some(&plan)), (300, 150));
        assert_eq!(svg_raster_size((100, 50), None), (100, 50));
        let cover = PreprocessOptions {
            width: Some(200),
            height: Some(200),
            fit: FitMode::Cover,
            ..PreprocessOptions::default()
        };
        assert_eq!(svg_raster_size((100, 50), Some(&cover)), (400, 200));
    }

    #[cfg(feature = "svg")]
    #[test]
    fn svg_files_rasterize_at_the_planned_size() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dot.svg");
        std::fs::write(
            &path,
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="40" height="20"><rect width="20" height="20" fill="#000"/></svg>"##,
        )
        .unwrap();
        assert_eq!(image_dimensions(&path).unwrap(), (40, 20));
        let plan = PreprocessOptions {
            width: Some(80),
            ..PreprocessOptions::default()
        };
        let img = load_image(&path, Some(&plan)).unwrap();
        assert_eq!(img.dimensions(), (80, 40));
        assert_eq!(img.get_pixel(10, 10).0, [0, 0, 0, 255]);
        assert_eq!(img.get_pixel(70, 10)[3], 0);
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(
            decode_image_bytes(&bytes, None).unwrap().dimensions(),
            (40, 20)
        );
    }
}
