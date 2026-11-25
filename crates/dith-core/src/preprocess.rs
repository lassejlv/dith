//! Image resizing, tonal adjustment, sharpening/blurring, and alpha handling before dithering.

use clap::ValueEnum;
use image::imageops::{self, FilterType};
use image::{Rgba, RgbaImage};
use serde::{Deserialize, Serialize};

use crate::color::{Rgb, linear_to_srgb_u8, srgb_u8_to_linear};

/// How to fit an image when both `--width` and `--height` are specified.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FitMode {
    /// Fit inside target dimensions while preserving aspect ratio.
    #[default]
    Contain,
    /// Fill target dimensions by center-cropping while preserving aspect ratio.
    Cover,
    /// Stretch directly to exact target dimensions.
    Stretch,
}

/// How to treat transparent pixels during dithering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AlphaMode {
    /// Keep original alpha channel intact; fully transparent pixels do not diffuse error.
    #[default]
    Preserve,
    /// Composite transparent pixels onto a solid background color before dithering.
    Flatten,
    /// Hard-threshold alpha at 50% (crisp 1-bit transparency mask).
    Threshold,
    /// Dither the alpha channel itself using an ordered Bayer 4x4 screen for stippled fadeouts!
    Dither,
}

/// Complete pre-processing configuration applied before the dithering pass.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PreprocessOptions {
    /// Target output width in pixels (before pixel-size quantization).
    pub width: Option<u32>,
    /// Target output height in pixels (before pixel-size quantization).
    pub height: Option<u32>,
    /// Cap the longest side to at most this many pixels while preserving aspect ratio.
    pub max_dim: Option<u32>,
    /// Uniform scale multiplier (e.g. `0.5` or `2.0`).
    pub scale: Option<f32>,
    /// Fit behavior when both `width` and `height` are specified.
    pub fit: FitMode,
    /// Chunky pixel scale factor (`1` = native pixels, `2..=32` = dither at 1/N then nearest upscale).
    pub pixel_size: u32,
    /// Brightness shift in `[-1.0, 1.0]`.
    pub brightness: f32,
    /// Contrast adjustment in `[-1.0, 1.0]`.
    pub contrast: f32,
    /// Gamma exponent (`> 0.0`, `1.0` = neutral, `< 1.0` brightens midtones, `> 1.0` darkens).
    pub gamma: f32,
    /// Saturation multiplier (`0.0` = grayscale, `1.0` = neutral, `2.0` = vivid).
    pub saturation: f32,
    /// Unsharp mask sharpening intensity (`0.0` = off, `0.5..=2.0` = crisp edges for 1-bit dithers).
    pub sharpen: f32,
    /// Pre-dither Gaussian blur sigma (`0.0` = off).
    pub blur: f32,
    /// Stretch luminance histogram (1st..99th percentile) so shadows and highlights breathe.
    pub auto_levels: bool,
    /// Convert to linear luminance grayscale before dithering.
    pub grayscale: bool,
    /// Invert tonal values before dithering.
    pub invert: bool,
    /// Alpha handling mode.
    pub alpha_mode: AlphaMode,
    /// Background color used when `alpha_mode == AlphaMode::Flatten`.
    pub background: Rgb,
}

impl Default for PreprocessOptions {
    fn default() -> Self {
        Self {
            width: None,
            height: None,
            max_dim: None,
            scale: None,
            fit: FitMode::Contain,
            pixel_size: 1,
            brightness: 0.0,
            contrast: 0.0,
            gamma: 1.0,
            saturation: 1.0,
            sharpen: 0.0,
            blur: 0.0,
            auto_levels: false,
            grayscale: false,
            invert: false,
            alpha_mode: AlphaMode::Preserve,
            background: Rgb::WHITE,
        }
    }
}

/// Output of pre-processing: the working image to dither plus final output dimensions.
#[derive(Debug, Clone)]
pub struct PreparedImage {
    /// Image ready for the dithering pass (possibly downscaled by `pixel_size`).
    pub working: RgbaImage,
    /// Final output dimensions after any `pixel_size` nearest-neighbor upscale.
    pub final_width: u32,
    pub final_height: u32,
}

/// Compute target final dimensions without actually allocating or resizing pixels (used by `--dry-run`).
#[must_use]
pub fn plan_dimensions(src_w: u32, src_h: u32, opts: &PreprocessOptions) -> (u32, u32) {
    let (mut w, mut h) = (src_w.max(1), src_h.max(1));

    if let Some(scale) = opts.scale {
        let s = scale.clamp(0.01, 16.0);
        w = ((w as f32) * s).round().max(1.0) as u32;
        h = ((h as f32) * s).round().max(1.0) as u32;
    }

    match (opts.width, opts.height) {
        (Some(tw), Some(th)) => match opts.fit {
            FitMode::Stretch | FitMode::Cover => {
                w = tw.max(1);
                h = th.max(1);
            }
            FitMode::Contain => {
                let ratio = (tw as f32 / w as f32).min(th as f32 / h as f32);
                w = ((w as f32) * ratio).round().max(1.0) as u32;
                h = ((h as f32) * ratio).round().max(1.0) as u32;
            }
        },
        (Some(tw), None) => {
            let ratio = tw as f32 / w as f32;
            w = tw.max(1);
            h = ((h as f32) * ratio).round().max(1.0) as u32;
        }
        (None, Some(th)) => {
            let ratio = th as f32 / h as f32;
            h = th.max(1);
            w = ((w as f32) * ratio).round().max(1.0) as u32;
        }
        (None, None) => {}
    }

    if let Some(max_d) = opts.max_dim {
        let max_d = max_d.max(1);
        let longest = w.max(h);
        if longest > max_d {
            let ratio = max_d as f32 / longest as f32;
            w = ((w as f32) * ratio).round().max(1.0) as u32;
            h = ((h as f32) * ratio).round().max(1.0) as u32;
        }
    }

    (w.max(1), h.max(1))
}

/// Resize, apply alpha flattening, tonal adjustments, blur/sharpen, and downscale for `pixel_size`.
#[must_use]
pub fn prepare_image(src: &RgbaImage, opts: &PreprocessOptions) -> PreparedImage {
    let (final_w, final_h) = plan_dimensions(src.width(), src.height(), opts);

    // Resize to final target first (handling cover crop cleanly).
    let mut img = if final_w == src.width() && final_h == src.height() {
        src.clone()
    } else if matches!(
        (opts.width, opts.height, opts.fit),
        (Some(_), Some(_), FitMode::Cover)
    ) {
        resize_cover(src, final_w, final_h)
    } else {
        imageops::resize(src, final_w, final_h, FilterType::Lanczos3)
    };

    // If pixel_size > 1, downscale to the coarse dither grid before tonal adjustments & dithering.
    let ps = opts.pixel_size.max(1);
    if ps > 1 {
        let work_w = (final_w / ps).max(1);
        let work_h = (final_h / ps).max(1);
        img = imageops::resize(&img, work_w, work_h, FilterType::Lanczos3);
    }

    // Flatten alpha early so tonal/spatial filters don't bleed undefined RGB from transparent areas.
    if opts.alpha_mode == AlphaMode::Flatten {
        flatten_alpha(&mut img, opts.background);
    }

    if opts.blur > 0.01 {
        img = imageops::blur(&img, opts.blur.clamp(0.1, 20.0));
    }

    if opts.sharpen > 0.01 {
        apply_unsharp_mask(&mut img, opts.sharpen.clamp(0.0, 5.0));
    }

    if opts.auto_levels {
        apply_auto_levels(&mut img);
    }

    let needs_tonal = opts.brightness.abs() > 1e-4
        || opts.contrast.abs() > 1e-4
        || (opts.gamma - 1.0).abs() > 1e-4
        || (opts.saturation - 1.0).abs() > 1e-4
        || opts.grayscale
        || opts.invert;

    if needs_tonal {
        apply_tonal_adjustments(&mut img, opts);
    }

    PreparedImage {
        working: img,
        final_width: final_w,
        final_height: final_h,
    }
}

/// Apply post-dither alpha mode and nearest-neighbor upscale when `pixel_size > 1`.
#[must_use]
pub fn finalize_image(
    mut dithered: RgbaImage,
    prepared: &PreparedImage,
    opts: &PreprocessOptions,
) -> RgbaImage {
    match opts.alpha_mode {
        AlphaMode::Preserve | AlphaMode::Flatten => {}
        AlphaMode::Threshold => {
            for px in dithered.pixels_mut() {
                px[3] = if px[3] >= 128 { 255 } else { 0 };
            }
        }
        AlphaMode::Dither => {
            // Crisp 4x4 Bayer screen over the alpha channel.
            const BAYER4: [u8; 16] = [0, 8, 2, 10, 12, 4, 14, 6, 3, 11, 1, 9, 15, 7, 13, 5];
            for (x, y, px) in dithered.enumerate_pixels_mut() {
                let threshold = (f32::from(BAYER4[((y & 3) * 4 + (x & 3)) as usize]) + 0.5) / 16.0;
                let a = f32::from(px[3]) / 255.0;
                px[3] = if a >= threshold { 255 } else { 0 };
            }
        }
    }

    if dithered.width() != prepared.final_width || dithered.height() != prepared.final_height {
        imageops::resize(
            &dithered,
            prepared.final_width,
            prepared.final_height,
            FilterType::Nearest,
        )
    } else {
        dithered
    }
}

fn resize_cover(src: &RgbaImage, target_w: u32, target_h: u32) -> RgbaImage {
    let ratio = (target_w as f32 / src.width() as f32).max(target_h as f32 / src.height() as f32);
    let scaled_w = ((src.width() as f32) * ratio).ceil().max(target_w as f32) as u32;
    let scaled_h = ((src.height() as f32) * ratio).ceil().max(target_h as f32) as u32;
    let scaled = imageops::resize(src, scaled_w, scaled_h, FilterType::Lanczos3);
    let x = (scaled_w - target_w) / 2;
    let y = (scaled_h - target_h) / 2;
    imageops::crop_imm(&scaled, x, y, target_w, target_h).to_image()
}

fn flatten_alpha(img: &mut RgbaImage, bg: Rgb) {
    let bg_lin = bg.to_linear();
    for px in img.pixels_mut() {
        let a = f32::from(px[3]) / 255.0;
        if a >= 0.999 {
            px[3] = 255;
            continue;
        }
        let inv = 1.0 - a;
        let r = srgb_u8_to_linear(px[0]) * a + bg_lin.r * inv;
        let g = srgb_u8_to_linear(px[1]) * a + bg_lin.g * inv;
        let b = srgb_u8_to_linear(px[2]) * a + bg_lin.b * inv;
        *px = Rgba([
            linear_to_srgb_u8(r),
            linear_to_srgb_u8(g),
            linear_to_srgb_u8(b),
            255,
        ]);
    }
}

fn apply_unsharp_mask(img: &mut RgbaImage, amount: f32) {
    // Fast separable 3x3 Gaussian-like tent blur ([1, 2, 1] / 4) in integer math (~20x faster than float blur).
    let w = img.width() as usize;
    let h = img.height() as usize;
    if w < 3 || h < 3 {
        return;
    }
    let src = img.as_raw();
    let mut horiz = vec![0u8; w * h * 3];

    for y in 0..h {
        let row_src = y * w * 4;
        let row_dst = y * w * 3;
        for x in 0..w {
            let xm = x.saturating_sub(1);
            let xp = (x + 1).min(w - 1);
            let i0 = row_src + xm * 4;
            let i1 = row_src + x * 4;
            let i2 = row_src + xp * 4;
            let o = row_dst + x * 3;
            for c in 0..3 {
                let sum =
                    u16::from(src[i0 + c]) + u16::from(src[i1 + c]) * 2 + u16::from(src[i2 + c]);
                horiz[o + c] = ((sum + 2) >> 2) as u8;
            }
        }
    }

    let dst = img.as_mut();
    for y in 0..h {
        let ym = y.saturating_sub(1);
        let yp = (y + 1).min(h - 1);
        let r0 = ym * w * 3;
        let r1 = y * w * 3;
        let r2 = yp * w * 3;
        let rd = y * w * 4;
        for x in 0..w {
            let h0 = r0 + x * 3;
            let h1 = r1 + x * 3;
            let h2 = r2 + x * 3;
            let d = rd + x * 4;
            for c in 0..3 {
                let blurred = f32::from(
                    (u16::from(horiz[h0 + c])
                        + u16::from(horiz[h1 + c]) * 2
                        + u16::from(horiz[h2 + c])
                        + 2)
                        >> 2,
                );
                let orig = f32::from(dst[d + c]);
                let sharp = orig + (orig - blurred) * amount;
                dst[d + c] = sharp.clamp(0.0, 255.0).round() as u8;
            }
        }
    }
}

fn apply_auto_levels(img: &mut RgbaImage) {
    let mut hist = [0u32; 256];
    let mut total = 0u32;
    for px in img.pixels() {
        if px[3] < 16 {
            continue;
        }
        let luma =
            (0.2126 * f32::from(px[0]) + 0.7152 * f32::from(px[1]) + 0.0722 * f32::from(px[2]))
                .round()
                .clamp(0.0, 255.0) as usize;
        hist[luma] += 1;
        total += 1;
    }
    if total == 0 {
        return;
    }
    let low_cut = (total / 100).max(1);
    let high_cut = total.saturating_sub(low_cut);

    let mut acc = 0u32;
    let mut lo = 0u8;
    let mut hi = 255u8;
    for (i, &count) in hist.iter().enumerate() {
        acc += count;
        if acc >= low_cut {
            lo = i as u8;
            break;
        }
    }
    acc = 0;
    for (i, &count) in hist.iter().enumerate() {
        acc += count;
        if acc >= high_cut {
            hi = i as u8;
            break;
        }
    }
    if hi <= lo.saturating_add(4) {
        return;
    }

    let lo_f = f32::from(lo) / 255.0;
    let inv_span = 255.0 / f32::from(hi - lo);
    for px in img.pixels_mut() {
        for c in 0..3 {
            let v = (f32::from(px[c]) / 255.0 - lo_f) * inv_span;
            px[c] = (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        }
    }
}

fn apply_tonal_adjustments(img: &mut RgbaImage, opts: &PreprocessOptions) {
    // Contrast curve factor: smooth perceptual contrast in [-1.0, 1.0].
    let c = opts.contrast.clamp(-1.0, 1.0);
    let contrast_factor = if c >= 0.0 {
        1.0 + c * 2.5
    } else {
        1.0 / (1.0 - c * 2.5)
    };
    let brightness = opts.brightness.clamp(-1.0, 1.0);
    let gamma = opts.gamma.clamp(0.1, 5.0);
    let sat = if opts.grayscale {
        0.0
    } else {
        opts.saturation.clamp(0.0, 3.0)
    };

    for px in img.pixels_mut() {
        let mut r = f32::from(px[0]) / 255.0;
        let mut g = f32::from(px[1]) / 255.0;
        let mut b = f32::from(px[2]) / 255.0;

        // Brightness + contrast around 0.5 midpoint.
        r = ((r - 0.5) * contrast_factor + 0.5 + brightness).clamp(0.0, 1.0);
        g = ((g - 0.5) * contrast_factor + 0.5 + brightness).clamp(0.0, 1.0);
        b = ((b - 0.5) * contrast_factor + 0.5 + brightness).clamp(0.0, 1.0);

        // Gamma.
        if (gamma - 1.0).abs() > 1e-4 {
            r = r.powf(gamma);
            g = g.powf(gamma);
            b = b.powf(gamma);
        }

        // Saturation / grayscale in linear light.
        if (sat - 1.0).abs() > 1e-4 {
            let lr = crate::color::srgb_f32_to_linear(r);
            let lg = crate::color::srgb_f32_to_linear(g);
            let lb = crate::color::srgb_f32_to_linear(b);
            let y = 0.2126 * lr + 0.7152 * lg + 0.0722 * lb;
            r = crate::color::linear_to_srgb_f32(y + (lr - y) * sat);
            g = crate::color::linear_to_srgb_f32(y + (lg - y) * sat);
            b = crate::color::linear_to_srgb_f32(y + (lb - y) * sat);
        }

        if opts.invert {
            r = 1.0 - r;
            g = 1.0 - g;
            b = 1.0 - b;
        }

        px[0] = (r.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        px[1] = (g.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        px[2] = (b.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_dimensions_respects_aspect_and_max_dim() {
        let opts = PreprocessOptions {
            max_dim: Some(200),
            ..Default::default()
        };
        assert_eq!(plan_dimensions(800, 400, &opts), (200, 100));

        let opts_w = PreprocessOptions {
            width: Some(300),
            ..Default::default()
        };
        assert_eq!(plan_dimensions(600, 400, &opts_w), (300, 200));
    }

    #[test]
    fn pixel_size_downscales_and_restores_dimensions() {
        let src = RgbaImage::from_pixel(64, 48, Rgba([120, 130, 140, 255]));
        let opts = PreprocessOptions {
            pixel_size: 4,
            ..Default::default()
        };
        let prep = prepare_image(&src, &opts);
        assert_eq!(prep.working.dimensions(), (16, 12));
        let out = finalize_image(prep.working.clone(), &prep, &opts);
        assert_eq!(out.dimensions(), (64, 48));
    }
}
