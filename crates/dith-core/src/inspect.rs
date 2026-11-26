//! Image inspection and smart dithering recommendation engine for agents.

use std::path::Path;

use anyhow::{Context, Result};
use serde::Serialize;

use crate::color::Rgb;
use crate::palette::extract_adaptive_palette;

/// Structured analysis returned by `dith inspect`.
#[derive(Debug, Clone, Serialize)]
pub struct ImageInspection {
    pub path: String,
    pub width: u32,
    pub height: u32,
    pub aspect_ratio: f64,
    pub color_type: String,
    pub has_transparency: bool,
    pub transparent_percent: f64,
    pub is_grayscale: bool,
    pub mean_luminance: f64,
    pub dynamic_range: f64,
    pub p5_luminance: f64,
    pub p50_luminance: f64,
    pub p95_luminance: f64,
    pub dominant_colors: Vec<String>,
    pub recommendations: Vec<DitherRecommendation>,
}

/// Actionable suggestion returned alongside image inspection.
#[derive(Debug, Clone, Serialize)]
pub struct DitherRecommendation {
    pub style: String,
    pub preset: Option<String>,
    pub rationale: String,
    pub example_command: String,
}

/// Inspect an image file and generate structured diagnostics + tailored recommendations.
///
/// # Errors
/// Returns an error if the image cannot be opened or decoded.
pub fn inspect_image(path: &Path, dominant_count: usize) -> Result<ImageInspection> {
    let (color_type, rgba) = if crate::input::is_svg_path(path) {
        ("Svg".to_string(), crate::input::load_image(path, None)?)
    } else {
        let dyn_img = image::open(path)
            .with_context(|| format!("opening image '{}' for inspection", path.display()))?;
        (format!("{:?}", dyn_img.color()), dyn_img.to_rgba8())
    };
    let (width, height) = rgba.dimensions();

    let total = u64::from(width) * u64::from(height);
    let mut transparent = 0u64;
    let mut chroma_sum = 0.0f64;
    let mut opaque = 0u64;
    let mut luma_hist = [0u64; 256];
    let mut luma_sum = 0.0f64;

    for px in rgba.pixels() {
        if px[3] < 250 {
            transparent += 1;
        }
        if px[3] < 16 {
            continue;
        }
        opaque += 1;
        let max_c = px[0].max(px[1]).max(px[2]);
        let min_c = px[0].min(px[1]).min(px[2]);
        chroma_sum += f64::from(max_c - min_c);

        let luma =
            (0.2126 * f64::from(px[0]) + 0.7152 * f64::from(px[1]) + 0.0722 * f64::from(px[2]))
                .clamp(0.0, 255.0);
        luma_sum += luma / 255.0;
        luma_hist[luma.round() as usize] += 1;
    }

    let opaque_denom = opaque.max(1) as f64;
    let mean_luminance = round3(luma_sum / opaque_denom);
    let mean_chroma = chroma_sum / opaque_denom;
    let is_grayscale = mean_chroma < 4.0;
    let has_transparency = transparent > 0;
    let transparent_percent = round2((transparent as f64 / total.max(1) as f64) * 100.0);

    let p5 = percentile_from_hist(&luma_hist, opaque, 0.05);
    let p50 = percentile_from_hist(&luma_hist, opaque, 0.50);
    let p95 = percentile_from_hist(&luma_hist, opaque, 0.95);
    let dynamic_range = round3((p95 - p5).max(0.0));

    let dominant_colors = extract_adaptive_palette(&rgba, dominant_count.clamp(2, 16), 42)
        .map_or_else(
            |_| vec![Rgb::BLACK.to_hex(), Rgb::WHITE.to_hex()],
            |p| p.hex_colors(),
        );

    let path_str = path.display().to_string();
    let recommendations = build_recommendations(
        &path_str,
        width,
        height,
        is_grayscale,
        has_transparency,
        dynamic_range,
        mean_luminance,
    );

    Ok(ImageInspection {
        path: path_str,
        width,
        height,
        aspect_ratio: round3(f64::from(width) / f64::from(height.max(1))),
        color_type,
        has_transparency,
        transparent_percent,
        is_grayscale,
        mean_luminance,
        dynamic_range,
        p5_luminance: round3(p5),
        p50_luminance: round3(p50),
        p95_luminance: round3(p95),
        dominant_colors,
        recommendations,
    })
}

fn build_recommendations(
    path: &str,
    width: u32,
    height: u32,
    is_grayscale: bool,
    has_transparency: bool,
    dynamic_range: f64,
    mean_luminance: f64,
) -> Vec<DitherRecommendation> {
    let mut recs = Vec::new();
    let resize_hint = if width.max(height) > 1200 {
        " --max-dim 960"
    } else {
        ""
    };
    let levels_hint = if dynamic_range < 0.65 {
        " --auto-levels"
    } else {
        ""
    };
    let alpha_hint = if has_transparency {
        " --alpha-mode preserve"
    } else {
        ""
    };

    recs.push(DitherRecommendation {
        style: "Editorial Print".to_string(),
        preset: Some("editorial-print".to_string()),
        rationale: if dynamic_range < 0.65 {
            "Low dynamic range image benefits from --auto-levels and Ostromoukhov variable diffusion."
                .to_string()
        } else {
            "Smooth tonal range pairs cleanly with Ostromoukhov diffusion on archival newsprint."
                .to_string()
        },
        example_command: format!(
            "dith apply \"{path}\" -o out-editorial.png --preset editorial-print{resize_hint}{levels_hint}{alpha_hint}"
        ),
    });

    recs.push(DitherRecommendation {
        style: "Crisp 1-Bit Macintosh".to_string(),
        preset: Some("mac-classic".to_string()),
        rationale: "Atkinson diffusion keeps highlights airy and preserves facial/edge structure."
            .to_string(),
        example_command: format!(
            "dith apply \"{path}\" -o out-mac.png --preset mac-classic{resize_hint}{alpha_hint}"
        ),
    });

    if !is_grayscale {
        recs.push(DitherRecommendation {
            style: "Adaptive Full-Color Lo-Fi".to_string(),
            preset: Some("lo-fi-photo".to_string()),
            rationale: "Colorful source preserves its mood best with an Oklab-extracted 8-color adaptive palette."
                .to_string(),
            example_command: format!(
                "dith apply \"{path}\" -o out-lofi.png --preset lo-fi-photo{resize_hint}{alpha_hint}"
            ),
        });
    }

    if mean_luminance < 0.35 {
        recs.push(DitherRecommendation {
            style: "Moody CRT Glow".to_string(),
            preset: Some("crt-amber".to_string()),
            rationale: "Dark-key image works naturally on a dark phosphor CRT palette.".to_string(),
            example_command: format!(
                "dith apply \"{path}\" -o out-crt.png --preset crt-amber{resize_hint}"
            ),
        });
    } else {
        recs.push(DitherRecommendation {
            style: "Risograph Print".to_string(),
            preset: Some("risograph".to_string()),
            rationale: "Bright/balanced tones print boldly in soy cobalt blue on warm cream paper."
                .to_string(),
            example_command: format!(
                "dith apply \"{path}\" -o out-riso.png --preset risograph{resize_hint}{alpha_hint}"
            ),
        });
    }

    recs
}

fn percentile_from_hist(hist: &[u64; 256], total: u64, frac: f64) -> f64 {
    if total == 0 {
        return 0.0;
    }
    let target = ((total as f64) * frac).ceil() as u64;
    let mut acc = 0u64;
    for (i, &c) in hist.iter().enumerate() {
        acc += c;
        if acc >= target {
            return i as f64 / 255.0;
        }
    }
    1.0
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

fn round3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}
