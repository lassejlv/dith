//! Ink overlays: exact line work (eyes, outlines, lettering) composited over a finished dither.
//! Overlay pixels are never dithered and never diffuse error: each one snaps to the nearest palette
//! color (or one fixed color), with transparency cut hard at 50% or screened by an ordered 4x4 Bayer
//! matrix, and under `--pixel-size N` whole N x N blocks switch together so the ink lines up with
//! the chunky pixels.

use std::path::{Path, PathBuf};

use image::RgbaImage;
use image::imageops::{self, FilterType};
use serde::{Deserialize, Serialize};

use crate::color::Rgb;
use crate::palette::Palette;
use crate::recipe::RecipeError;

/// How an overlay's transparency turns into pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
#[serde(rename_all = "kebab-case")]
pub enum OverlayAlpha {
    /// Hard cut: pixels (or pixel-size blocks) at least 50% opaque are drawn, the rest are not.
    #[default]
    Threshold,
    /// Ordered 4x4 Bayer screen: partial transparency becomes an even stipple of ink.
    Dither,
}

impl OverlayAlpha {
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Threshold => "threshold",
            Self::Dither => "dither",
        }
    }
}

/// Overlay layers and how they are inked, stored in a [`crate::DitherRecipe`].
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct OverlayOptions {
    /// SVG or PNG files composited in order after dithering.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub layers: Vec<PathBuf>,
    /// Paint every drawn overlay pixel this color instead of snapping it to the palette.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<Rgb>,
    pub alpha: OverlayAlpha,
}

impl OverlayOptions {
    /// True when nothing is set, so recipes without overlays serialize exactly as before.
    #[must_use]
    pub fn is_off(&self) -> bool {
        *self == Self::default()
    }
}

fn asset_error(path: &Path, reason: impl std::fmt::Display) -> RecipeError {
    RecipeError::InvalidAsset {
        kind: "overlay",
        path: path.display().to_string(),
        reason: reason.to_string(),
    }
}

/// Check that an overlay file exists and decodes, without rasterizing it.
///
/// # Errors
/// Returns [`RecipeError::InvalidAsset`] when the file is missing or not an image.
pub fn check_overlay(path: &Path) -> Result<(), RecipeError> {
    crate::input::image_dimensions(path)
        .map(|_| ())
        .map_err(|e| asset_error(path, format!("{e:#}")))
}

/// Load an overlay at exactly `width` x `height`: SVGs are rasterized at that size, raster files
/// of another size are scaled with nearest-neighbour so their edges stay hard.
///
/// # Errors
/// Returns [`RecipeError::InvalidAsset`] when the file is missing or not an image.
pub fn load_overlay(path: &Path, width: u32, height: u32) -> Result<RgbaImage, RecipeError> {
    let img = if crate::input::is_svg_path(path) {
        #[cfg(feature = "svg")]
        {
            let bytes = std::fs::read(path).map_err(|e| asset_error(path, e))?;
            crate::svg::rasterize(&bytes, width, height)
                .map_err(|e| asset_error(path, format!("{e:#}")))?
        }
        #[cfg(not(feature = "svg"))]
        {
            return Err(asset_error(path, "this build has no SVG support"));
        }
    } else {
        crate::input::load_image(path, None).map_err(|e| asset_error(path, format!("{e:#}")))?
    };
    Ok(if img.dimensions() == (width, height) {
        img
    } else {
        imageops::resize(&img, width, height, FilterType::Nearest)
    })
}

const BAYER4: [u8; 16] = [0, 8, 2, 10, 12, 4, 14, 6, 3, 11, 1, 9, 15, 7, 13, 5];

/// Composite one overlay (already at `target`'s size) onto a finished dither. Each
/// `pixel_size` x `pixel_size` block is drawn or skipped as a whole, from the block's mean
/// opacity, in the palette color nearest the block's mean overlay color (or `opts.color`).
pub fn composite_overlay(
    target: &mut RgbaImage,
    overlay: &RgbaImage,
    palette: &Palette,
    opts: &OverlayOptions,
    pixel_size: u32,
) {
    let (w, h) = target.dimensions();
    let ps = pixel_size.max(1);
    for by in 0..h.div_ceil(ps) {
        for bx in 0..w.div_ceil(ps) {
            let (x0, y0) = (bx * ps, by * ps);
            let (x1, y1) = ((x0 + ps).min(w), (y0 + ps).min(h));
            let (mut alpha, mut rgb) = (0u64, [0u64; 3]);
            for y in y0..y1 {
                for x in x0..x1 {
                    let p = overlay.get_pixel(x, y);
                    let a = u64::from(p[3]);
                    alpha += a;
                    for c in 0..3 {
                        rgb[c] += u64::from(p[c]) * a;
                    }
                }
            }
            let count = u64::from((x1 - x0) * (y1 - y0));
            let coverage = alpha as f32 / (count * 255) as f32;
            let threshold = match opts.alpha {
                OverlayAlpha::Threshold => 0.5,
                OverlayAlpha::Dither => {
                    (f32::from(BAYER4[((by & 3) * 4 + (bx & 3)) as usize]) + 0.5) / 16.0
                }
            };
            if alpha == 0 || coverage < threshold {
                continue;
            }
            let ink = opts.color.unwrap_or_else(|| {
                let mean = rgb.map(|c| (c / alpha) as u8);
                palette.colors()[palette
                    .nearest_index_from_srgb(Rgb::new(mean[0], mean[1], mean[2]).to_srgb_f32())]
            });
            for y in y0..y1 {
                for x in x0..x1 {
                    target.put_pixel(x, y, image::Rgba([ink.r, ink.g, ink.b, 255]));
                }
            }
        }
    }
}

/// Load and composite every layer of `opts` onto `target`, in order.
///
/// # Errors
/// Returns [`RecipeError::InvalidAsset`] when a layer cannot be loaded.
pub fn apply_overlays(
    target: &mut RgbaImage,
    opts: &OverlayOptions,
    palette: &Palette,
    pixel_size: u32,
) -> Result<(), RecipeError> {
    let (w, h) = target.dimensions();
    for layer in &opts.layers {
        let overlay = load_overlay(layer, w, h)?;
        composite_overlay(target, &overlay, palette, opts, pixel_size);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use image::Rgba;

    use super::*;

    fn palette() -> Palette {
        Palette::from_hex_list("#F5F1EA,#171717").unwrap()
    }

    fn overlay() -> RgbaImage {
        RgbaImage::from_fn(8, 8, |x, y| match (x, y) {
            (0..=1, 0..=1) => Rgba([0, 0, 0, 255]),
            (4, 4) => Rgba([255, 255, 255, 255]),
            (6, 6) => Rgba([0, 0, 0, 100]),
            _ => Rgba([0, 0, 0, 0]),
        })
    }

    #[test]
    fn snaps_to_the_palette_and_cuts_alpha_at_half() {
        let mut img = RgbaImage::from_pixel(8, 8, Rgba([1, 2, 3, 255]));
        composite_overlay(
            &mut img,
            &overlay(),
            &palette(),
            &OverlayOptions::default(),
            1,
        );
        assert_eq!(img.get_pixel(1, 1).0, [0x17, 0x17, 0x17, 255]);
        assert_eq!(img.get_pixel(4, 4).0, [0xF5, 0xF1, 0xEA, 255]);
        assert_eq!(
            img.get_pixel(6, 6).0,
            [1, 2, 3, 255],
            "39% opaque stays under the cut"
        );
        assert_eq!(img.get_pixel(3, 3).0, [1, 2, 3, 255]);

        let mut fixed = RgbaImage::from_pixel(8, 8, Rgba([1, 2, 3, 255]));
        let opts = OverlayOptions {
            color: Some(Rgb::new(200, 0, 0)),
            ..OverlayOptions::default()
        };
        composite_overlay(&mut fixed, &overlay(), &palette(), &opts, 1);
        assert_eq!(fixed.get_pixel(4, 4).0, [200, 0, 0, 255]);
    }

    #[test]
    fn pixel_size_switches_whole_blocks() {
        let mut img = RgbaImage::from_pixel(8, 8, Rgba([1, 2, 3, 255]));
        composite_overlay(
            &mut img,
            &overlay(),
            &palette(),
            &OverlayOptions::default(),
            4,
        );
        // Top-left 4x4 block is 25% covered: below the cut, so untouched.
        assert_eq!(img.get_pixel(0, 0).0, [1, 2, 3, 255]);
        let mut img = RgbaImage::from_pixel(8, 8, Rgba([1, 2, 3, 255]));
        composite_overlay(
            &mut img,
            &overlay(),
            &palette(),
            &OverlayOptions::default(),
            2,
        );
        for (x, y) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
            assert_eq!(img.get_pixel(x, y).0, [0x17, 0x17, 0x17, 255]);
        }
        // The lone white pixel covers 25% of its 2x2 block.
        assert_eq!(img.get_pixel(4, 4).0, [1, 2, 3, 255]);
    }

    #[test]
    fn dither_alpha_stipples_partial_coverage_evenly() {
        let half = RgbaImage::from_pixel(16, 16, Rgba([0, 0, 0, 128]));
        let mut img = RgbaImage::from_pixel(16, 16, Rgba([255, 255, 255, 255]));
        let opts = OverlayOptions {
            alpha: OverlayAlpha::Dither,
            ..OverlayOptions::default()
        };
        composite_overlay(&mut img, &half, &palette(), &opts, 1);
        let inked = img.pixels().filter(|p| p.0[0] == 0x17).count();
        assert_eq!(inked, 128);
        assert!(OverlayOptions::default().is_off());
        assert!(!opts.is_off());
    }
}
