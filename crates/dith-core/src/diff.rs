//! Pixel-exact image and animation frame comparison: how many pixels changed, where, and a
//! highlight image that marks them.

use image::{Rgba, RgbaImage};
use serde::Serialize;

use crate::recipe::RecipeError;

/// Axis-aligned pixel rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PixelRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// Summary of the pixels that differ between two equally sized images.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ImageDiff {
    pub width: u32,
    pub height: u32,
    pub changed_pixels: u64,
    pub total_pixels: u64,
    /// Share of changed pixels in percent, rounded to four decimals.
    pub changed_percent: f64,
    /// Smallest rectangle holding every changed pixel (`None` when nothing changed).
    pub bbox: Option<PixelRect>,
    /// Inclusive `[first, last]` runs of rows that hold at least one changed pixel.
    pub changed_rows: Vec<[u32; 2]>,
    /// Inclusive `[first, last]` runs of columns that hold at least one changed pixel.
    pub changed_columns: Vec<[u32; 2]>,
}

/// Whether two pixels differ by more than `tolerance` on any channel. Two fully transparent
/// pixels are always equal, whatever RGB they carry.
#[must_use]
pub fn pixels_differ(a: Rgba<u8>, b: Rgba<u8>, tolerance: u8) -> bool {
    if a[3] == 0 && b[3] == 0 {
        return false;
    }
    a.0.iter()
        .zip(b.0.iter())
        .any(|(&x, &y)| x.abs_diff(y) > tolerance)
}

fn check_same_size(a: &RgbaImage, b: &RgbaImage) -> Result<(), RecipeError> {
    if a.dimensions() == b.dimensions() {
        return Ok(());
    }
    Err(RecipeError::Validation {
        field: "input",
        reason: format!(
            "images must share one size to diff, got {}x{} and {}x{}",
            a.width(),
            a.height(),
            b.width(),
            b.height()
        ),
    })
}

/// Per-pixel change mask (row-major, `true` = changed).
///
/// # Errors
/// Returns [`RecipeError::Validation`] when the images differ in size.
pub fn changed_mask(a: &RgbaImage, b: &RgbaImage, tolerance: u8) -> Result<Vec<bool>, RecipeError> {
    check_same_size(a, b)?;
    Ok(a.pixels()
        .zip(b.pixels())
        .map(|(&pa, &pb)| pixels_differ(pa, pb, tolerance))
        .collect())
}

/// Count pixels that differ at all between two images (every pixel of `b` when sizes differ).
#[must_use]
pub fn count_changed_pixels(a: &RgbaImage, b: &RgbaImage) -> u64 {
    if a.dimensions() != b.dimensions() {
        return u64::from(b.width()) * u64::from(b.height());
    }
    a.pixels()
        .zip(b.pixels())
        .filter(|&(&pa, &pb)| pixels_differ(pa, pb, 0))
        .count() as u64
}

/// Compare two equally sized images.
///
/// # Errors
/// Returns [`RecipeError::Validation`] when the images differ in size.
pub fn diff_images(a: &RgbaImage, b: &RgbaImage, tolerance: u8) -> Result<ImageDiff, RecipeError> {
    let mask = changed_mask(a, b, tolerance)?;
    Ok(diff_from_mask(a.width(), a.height(), &mask))
}

/// Summarize a row-major change mask of a `width` x `height` canvas.
#[must_use]
pub fn diff_from_mask(width: u32, height: u32, mask: &[bool]) -> ImageDiff {
    let w = width as usize;
    let mut rows = vec![false; height as usize];
    let mut cols = vec![false; w];
    let mut changed = 0u64;
    for (i, _) in mask.iter().enumerate().filter(|&(_, &c)| c) {
        changed += 1;
        rows[i / w] = true;
        cols[i % w] = true;
    }
    let changed_rows = runs(&rows);
    let changed_columns = runs(&cols);
    let bbox = match (
        changed_rows.first(),
        changed_rows.last(),
        changed_columns.first(),
        changed_columns.last(),
    ) {
        (Some(r0), Some(r1), Some(c0), Some(c1)) => Some(PixelRect {
            x: c0[0],
            y: r0[0],
            width: c1[1] - c0[0] + 1,
            height: r1[1] - r0[0] + 1,
        }),
        _ => None,
    };
    let total = u64::from(width) * u64::from(height);
    let percent = if total == 0 {
        0.0
    } else {
        (changed as f64 * 100.0 / total as f64 * 10_000.0).round() / 10_000.0
    };
    ImageDiff {
        width,
        height,
        changed_pixels: changed,
        total_pixels: total,
        changed_percent: percent,
        bbox,
        changed_rows,
        changed_columns,
    }
}

fn runs(flags: &[bool]) -> Vec<[u32; 2]> {
    let mut out: Vec<[u32; 2]> = Vec::new();
    for (i, _) in flags.iter().enumerate().filter(|&(_, &f)| f) {
        let i = i as u32;
        match out.last_mut() {
            Some(run) if run[1] + 1 == i => run[1] = i,
            _ => out.push([i, i]),
        }
    }
    out
}

/// Color used to mark changed pixels in [`highlight_changes`].
pub const HIGHLIGHT: Rgba<u8> = Rgba([230, 40, 70, 255]);

/// Render `after` faded towards white with every changed pixel painted [`HIGHLIGHT`], so the
/// changes read at a glance while the picture stays recognisable.
#[must_use]
pub fn highlight_changes(after: &RgbaImage, mask: &[bool]) -> RgbaImage {
    let mut out = after.clone();
    for (px, &changed) in out.pixels_mut().zip(mask) {
        if changed {
            *px = HIGHLIGHT;
            continue;
        }
        let a = u32::from(px[3]);
        let luma =
            (u32::from(px[0]) * 2126 + u32::from(px[1]) * 7152 + u32::from(px[2]) * 722) / 10_000;
        let faded = 255 - (255 - (luma * a + 255 * (255 - a)) / 255) / 4;
        *px = Rgba([faded as u8, faded as u8, faded as u8, 255]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canvas() -> RgbaImage {
        RgbaImage::from_pixel(10, 8, Rgba([255, 255, 255, 255]))
    }

    #[test]
    fn reports_counts_box_and_row_column_runs() {
        let a = canvas();
        let mut b = canvas();
        for (x, y) in [(2, 1), (3, 1), (7, 5), (7, 6)] {
            b.put_pixel(x, y, Rgba([0, 0, 0, 255]));
        }
        let d = diff_images(&a, &b, 0).unwrap();
        assert_eq!(d.changed_pixels, 4);
        assert_eq!(d.total_pixels, 80);
        assert!((d.changed_percent - 5.0).abs() < 1e-9);
        assert_eq!(
            d.bbox,
            Some(PixelRect {
                x: 2,
                y: 1,
                width: 6,
                height: 6
            })
        );
        assert_eq!(d.changed_rows, vec![[1, 1], [5, 6]]);
        assert_eq!(d.changed_columns, vec![[2, 3], [7, 7]]);

        let mask = changed_mask(&a, &b, 0).unwrap();
        let hl = highlight_changes(&b, &mask);
        assert_eq!(*hl.get_pixel(2, 1), HIGHLIGHT);
        assert_eq!(hl.get_pixel(0, 0).0, [255, 255, 255, 255]);
    }

    #[test]
    fn identical_images_tolerance_and_transparency() {
        let a = canvas();
        let d = diff_images(&a, &a, 0).unwrap();
        assert_eq!((d.changed_pixels, d.bbox), (0, None));
        assert_eq!(d.changed_rows, Vec::<[u32; 2]>::new());

        let mut b = a.clone();
        b.put_pixel(0, 0, Rgba([250, 255, 255, 255]));
        assert_eq!(diff_images(&a, &b, 5).unwrap().changed_pixels, 0);
        assert_eq!(diff_images(&a, &b, 4).unwrap().changed_pixels, 1);

        assert!(!pixels_differ(Rgba([1, 2, 3, 0]), Rgba([9, 9, 9, 0]), 0));
        assert_eq!(count_changed_pixels(&a, &b), 1);
    }

    #[test]
    fn size_mismatch_is_a_validation_error() {
        let err = diff_images(&canvas(), &RgbaImage::new(3, 3), 0).unwrap_err();
        assert_eq!(err.code(), "INVALID_ARGUMENT");
        assert!(err.to_string().contains("10x8 and 3x3"), "{err}");
    }
}
