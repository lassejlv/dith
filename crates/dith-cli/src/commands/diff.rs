//! The `dith diff` command: count, locate and highlight the pixels that differ between two images,
//! or between consecutive frames of an animation.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Args;
use image::RgbaImage;
use serde::Serialize;

use dith_core::animate::decode_animated_frames;
use dith_core::diff::{ImageDiff, changed_mask, diff_from_mask, highlight_changes};
use dith_core::input::{image_dimensions, load_image};
use dith_core::output::{encode_png_bytes, write_output_file};
use dith_core::recipe::RecipeError;

#[derive(Debug, Args)]
pub struct DiffArgs {
    /// First image (PNG, GIF, WebP, SVG, ...).
    #[arg(required_unless_present = "frames", conflicts_with = "frames")]
    pub a: Option<PathBuf>,

    /// Second image, the same size as the first.
    #[arg(required_unless_present = "frames")]
    pub b: Option<PathBuf>,

    /// Compare each frame of an animation (GIF, APNG, animated WebP, or a directory of frame
    /// images) with the frame before it.
    #[arg(long, value_name = "ANIM|DIR")]
    pub frames: Option<PathBuf>,

    /// With --frames, also compare the last frame with the first (the loop seam).
    #[arg(long = "loop", requires = "frames")]
    pub wrap: bool,

    /// Per-channel difference (0..=255) a pixel may have and still count as unchanged.
    #[arg(long, default_value_t = 0)]
    pub tolerance: u8,

    /// Write a PNG that fades the (last) image and paints every changed pixel red.
    #[arg(short = 'o', long, value_name = "PATH")]
    pub output: Option<PathBuf>,

    /// Validate inputs and report what would be compared without diffing or writing anything.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Serialize)]
struct PairReport {
    status: &'static str,
    mode: &'static str,
    dry_run: bool,
    a: String,
    b: String,
    tolerance: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    output: Option<String>,
    #[serde(flatten)]
    diff: Option<ImageDiff>,
}

#[derive(Debug, Serialize)]
struct FrameReport {
    index: usize,
    previous: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    delay_ms: Option<u32>,
    #[serde(flatten)]
    diff: ImageDiff,
}

#[derive(Debug, Serialize)]
struct FramesReport {
    status: &'static str,
    mode: &'static str,
    dry_run: bool,
    source: String,
    frame_count: usize,
    width: u32,
    height: u32,
    tolerance: u8,
    #[serde(rename = "loop")]
    wrap: bool,
    /// Frames identical to the frame before them.
    unchanged_frames: usize,
    changed_pixels_total: u64,
    frames: Vec<FrameReport>,
    /// Every pixel that changes in any compared pair.
    #[serde(skip_serializing_if = "Option::is_none")]
    union: Option<ImageDiff>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output: Option<String>,
}

pub(crate) fn handle_diff(args: &DiffArgs, as_json: bool) -> Result<()> {
    if let Some(anim) = &args.frames {
        return diff_frames(args, anim, as_json);
    }
    let (Some(a), Some(b)) = (&args.a, &args.b) else {
        return Err(RecipeError::Validation {
            field: "input",
            reason: "pass two images (dith diff a.png b.png) or --frames <anim>".to_string(),
        }
        .into());
    };
    let mut report = PairReport {
        status: "ok",
        mode: "images",
        dry_run: args.dry_run,
        a: a.display().to_string(),
        b: b.display().to_string(),
        tolerance: args.tolerance,
        output: args.output.as_ref().map(|p| p.display().to_string()),
        diff: None,
    };
    if args.dry_run {
        let (da, db) = (image_dimensions(a)?, image_dimensions(b)?);
        if da != db {
            return Err(size_mismatch(da, db).into());
        }
        return emit(&report, as_json, || {
            format!(
                "Dry run: would diff {} and {} ({}x{})",
                report.a, report.b, da.0, da.1
            )
        });
    }
    let (img_a, img_b) = (load_image(a, None)?, load_image(b, None)?);
    let mask = changed_mask(&img_a, &img_b, args.tolerance)?;
    if let Some(path) = &args.output {
        write_highlight(path, &img_b, &mask)?;
    }
    let diff = diff_from_mask(img_b.width(), img_b.height(), &mask);
    let text = summary_line(&diff);
    report.diff = Some(diff);
    emit(&report, as_json, || text)
}

fn diff_frames(args: &DiffArgs, anim: &Path, as_json: bool) -> Result<()> {
    let frames = load_frames(anim)?;
    let (width, height) = frames[0].0.dimensions();
    if let Some((i, (img, _))) = frames
        .iter()
        .enumerate()
        .find(|(_, (f, _))| f.dimensions() != (width, height))
    {
        return Err(RecipeError::Validation {
            field: "frames",
            reason: format!(
                "frame {i} is {}x{} but frame 0 is {width}x{height}",
                img.width(),
                img.height()
            ),
        }
        .into());
    }
    let mut report = FramesReport {
        status: "ok",
        mode: "frames",
        dry_run: args.dry_run,
        source: anim.display().to_string(),
        frame_count: frames.len(),
        width,
        height,
        tolerance: args.tolerance,
        wrap: args.wrap,
        unchanged_frames: 0,
        changed_pixels_total: 0,
        frames: Vec::new(),
        union: None,
        output: args.output.as_ref().map(|p| p.display().to_string()),
    };
    if args.dry_run {
        return emit(&report, as_json, || {
            format!(
                "Dry run: would diff {} consecutive frame pairs of {} ({width}x{height})",
                frames.len().saturating_sub(1),
                report.source
            )
        });
    }

    let mut pairs: Vec<(usize, usize)> = (1..frames.len()).map(|i| (i - 1, i)).collect();
    if args.wrap && frames.len() > 1 {
        pairs.push((frames.len() - 1, 0));
    }
    let mut union_mask = vec![false; (width as usize) * (height as usize)];
    for (prev, index) in pairs {
        let mask = changed_mask(&frames[prev].0, &frames[index].0, args.tolerance)?;
        for (u, &m) in union_mask.iter_mut().zip(&mask) {
            *u |= m;
        }
        let diff = diff_from_mask(width, height, &mask);
        report.changed_pixels_total += diff.changed_pixels;
        if diff.changed_pixels == 0 {
            report.unchanged_frames += 1;
        }
        report.frames.push(FrameReport {
            index,
            previous: prev,
            delay_ms: frames[index].1,
            diff,
        });
    }
    if let Some(path) = &args.output {
        write_highlight(path, &frames[0].0, &union_mask)?;
    }
    let union = diff_from_mask(width, height, &union_mask);
    let text = {
        let mut lines: Vec<String> = report
            .frames
            .iter()
            .map(|f| {
                format!(
                    "frame {} vs {}: {}",
                    f.index,
                    f.previous,
                    summary_line(&f.diff)
                )
            })
            .collect();
        lines.push(format!("any frame: {}", summary_line(&union)));
        lines.join("\n")
    };
    report.union = Some(union);
    emit(&report, as_json, || text)
}

/// Decode animation frames with their delays, or read a directory of frame images in name order.
fn load_frames(path: &Path) -> Result<Vec<(RgbaImage, Option<u32>)>> {
    if path.is_dir() {
        let mut files: Vec<PathBuf> = std::fs::read_dir(path)
            .with_context(|| format!("reading frames directory '{}'", path.display()))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                p.extension().and_then(|e| e.to_str()).is_some_and(|e| {
                    [
                        "png", "gif", "jpg", "jpeg", "webp", "bmp", "tif", "tiff", "svg",
                    ]
                    .contains(&e.to_ascii_lowercase().as_str())
                })
            })
            .collect();
        files.sort();
        if files.is_empty() {
            return Err(RecipeError::Validation {
                field: "frames",
                reason: format!("'{}' holds no frame images", path.display()),
            }
            .into());
        }
        return files
            .iter()
            .map(|f| Ok((load_image(f, None)?, None)))
            .collect();
    }
    let frames = decode_animated_frames(path, None, None)?.ok_or_else(|| RecipeError::Validation {
        field: "frames",
        reason: format!(
            "'{}' is not an animation with several frames (expected GIF, APNG, animated WebP, or a frames directory)",
            path.display()
        ),
    })?;
    Ok(frames
        .into_iter()
        .map(|f| (f.image, Some(f.delay_ms)))
        .collect())
}

fn write_highlight(path: &Path, base: &RgbaImage, mask: &[bool]) -> Result<()> {
    let bytes = encode_png_bytes(&highlight_changes(base, mask))?;
    write_output_file(path, &bytes)
}

fn size_mismatch(a: (u32, u32), b: (u32, u32)) -> RecipeError {
    RecipeError::Validation {
        field: "input",
        reason: format!(
            "images must share one size to diff, got {}x{} and {}x{}",
            a.0, a.1, b.0, b.1
        ),
    }
}

fn summary_line(d: &ImageDiff) -> String {
    let Some(bbox) = d.bbox else {
        return format!("no changes ({} pixels compared)", d.total_pixels);
    };
    let fmt_runs = |runs: &[[u32; 2]]| {
        runs.iter()
            .map(|[a, b]| {
                if a == b {
                    a.to_string()
                } else {
                    format!("{a}-{b}")
                }
            })
            .collect::<Vec<_>>()
            .join(",")
    };
    format!(
        "{} of {} pixels changed ({}%), box {}x{} at ({},{}), rows {}, columns {}",
        d.changed_pixels,
        d.total_pixels,
        d.changed_percent,
        bbox.width,
        bbox.height,
        bbox.x,
        bbox.y,
        fmt_runs(&d.changed_rows),
        fmt_runs(&d.changed_columns)
    )
}

fn emit<T: Serialize>(report: &T, as_json: bool, text: impl FnOnce() -> String) -> Result<()> {
    if as_json {
        println!("{}", serde_json::to_string_pretty(report)?);
    } else {
        println!("{}", text());
    }
    Ok(())
}
