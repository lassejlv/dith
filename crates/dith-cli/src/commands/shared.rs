//! Input loading, output destination, and render report helpers shared by several commands.

use std::io::{self, IsTerminal, Read, Write};
use std::path::Path;

use anyhow::{Context, Result};
use image::RgbaImage;
use serde::Serialize;

use dith_core::color::ColorSpace;
use dith_core::input::{decode_image_bytes, load_image};
use dith_core::output::{
    ColorUsage, ExportOptions, KittyOptions, OutputFormat, render_kitty, write_output_file,
};
use dith_core::preprocess::PreprocessOptions;
use dith_core::recipe::{DitherRecipe, PaletteSpec, RecipeError};

/// Structured result emitted for `apply` and `generate`.
#[derive(Debug, Serialize)]
pub(crate) struct RenderReport {
    pub(crate) status: &'static str,
    pub(crate) dry_run: bool,
    pub(crate) source: String,
    pub(crate) output: Option<String>,
    pub(crate) format: &'static str,
    pub(crate) preset: Option<String>,
    pub(crate) algorithm: &'static str,
    pub(crate) color_space: ColorSpace,
    pub(crate) palette_name: String,
    pub(crate) palette_colors: Option<Vec<String>>,
    pub(crate) source_dimensions: [u32; 2],
    pub(crate) working_dimensions: [u32; 2],
    pub(crate) output_dimensions: [u32; 2],
    pub(crate) bytes_written: usize,
    pub(crate) elapsed_ms: u128,
    pub(crate) color_usage: Vec<ColorUsage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) preview_braille: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) text_output: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) saved_preset_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) recipe: Option<DitherRecipe>,
}

/// Load an input image (or '-' for stdin). SVG input is rasterized at the size `plan` asks for.
pub(crate) fn load_input_image(path: &Path, plan: Option<&PreprocessOptions>) -> Result<RgbaImage> {
    if path.as_os_str() == "-" {
        let mut buf = Vec::new();
        io::stdin()
            .read_to_end(&mut buf)
            .context("reading input image bytes from stdin")?;
        decode_image_bytes(&buf, plan).context("decoding input image from stdin")
    } else {
        load_image(path, plan)
    }
}

/// Image files (PNG, GIF, JPEG, WebP, BMP, TIFF, SVG) directly inside `dir`, in name order.
pub(crate) fn list_image_files(dir: &Path) -> Result<Vec<std::path::PathBuf>> {
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .with_context(|| format!("reading directory '{}'", dir.display()))?
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
            reason: format!("'{}' holds no image files", dir.display()),
        }
        .into());
    }
    Ok(files)
}

pub(crate) fn resolve_output_format(
    output: Option<&Path>,
    explicit: Option<OutputFormat>,
    view: bool,
) -> Result<OutputFormat, RecipeError> {
    if let Some(fmt) = explicit {
        return Ok(fmt);
    }
    match output {
        Some(path) if path.as_os_str() != "-" => Ok(OutputFormat::infer_from_path(path)),
        Some(_) => Ok(OutputFormat::Png),
        None if view => Ok(OutputFormat::Kitty),
        None => Err(RecipeError::Validation {
            field: "output",
            reason: "pass -o <path>, -o - (stdout), --view, or --format kitty|braille|ascii|svg"
                .to_string(),
        }),
    }
}

pub(crate) fn emit_inline_kitty(
    img: &RgbaImage,
    export: &ExportOptions,
    as_json: bool,
) -> Result<()> {
    if as_json && !io::stderr().is_terminal() {
        return Ok(());
    }
    let kitty_opts = KittyOptions {
        cols: export.view_cols.or(export.text_width),
        rows: export.view_rows,
        tmux_passthrough: if as_json {
            std::env::var_os("TMUX").is_some() && io::stderr().is_terminal()
        } else {
            KittyOptions::should_use_tmux_passthrough()
        },
        trailing_newline: true,
        no_cursor_move: false,
        image_id: None,
    };
    let seq = render_kitty(img, kitty_opts)?;
    if as_json {
        let mut stderr = io::stderr().lock();
        stderr
            .write_all(seq.as_bytes())
            .context("writing Kitty graphics preview to stderr")?;
        stderr.flush().context("flushing stderr")?;
    } else {
        let mut stdout = io::stdout().lock();
        stdout
            .write_all(seq.as_bytes())
            .context("writing Kitty graphics preview to stdout")?;
        stdout.flush().context("flushing stdout")?;
    }
    Ok(())
}

pub(crate) fn ensure_output_destination(
    output: Option<&Path>,
    format: OutputFormat,
    no_clobber: bool,
    dry_run: bool,
) -> Result<(), RecipeError> {
    if output.is_none() && !format.is_text() {
        return Err(RecipeError::Validation {
            field: "output",
            reason: format!(
                "binary format '{}' requires -o <path> (or -o - for stdout)",
                format.id()
            ),
        });
    }
    if no_clobber
        && !dry_run
        && let Some(path) = output
        && path.as_os_str() != "-"
        && path.exists()
    {
        return Err(RecipeError::OutputExists {
            path: path.display().to_string(),
        });
    }
    Ok(())
}

pub(crate) fn write_destination(
    output: Option<&Path>,
    bytes: &[u8],
    format: OutputFormat,
    as_json: bool,
) -> Result<usize> {
    match output {
        Some(path) if path.as_os_str() == "-" => {
            if !as_json {
                let mut stdout = io::stdout().lock();
                stdout
                    .write_all(bytes)
                    .context("writing output to stdout")?;
                stdout.flush().context("flushing stdout")?;
            }
            Ok(bytes.len())
        }
        Some(path) => {
            write_output_file(path, bytes)?;
            Ok(bytes.len())
        }
        None => {
            if format.is_text() && !as_json {
                let mut stdout = io::stdout().lock();
                stdout
                    .write_all(bytes)
                    .context("writing text output to stdout")?;
                stdout.flush().context("flushing stdout")?;
            }
            Ok(bytes.len())
        }
    }
}

pub(crate) fn emit_render_report(
    report: &RenderReport,
    as_json: bool,
    wrote_stdout_payload: bool,
) -> Result<()> {
    if as_json {
        println!("{}", serde_json::to_string_pretty(report)?);
        return Ok(());
    }
    if wrote_stdout_payload {
        return Ok(());
    }
    if report.dry_run {
        println!(
            "Dry run: {} -> {} ({}x{}, algo={}, palette={}, format={})",
            report.source,
            report.output.as_deref().unwrap_or("<stdout>"),
            report.output_dimensions[0],
            report.output_dimensions[1],
            report.algorithm,
            report.palette_name,
            report.format,
        );
    } else {
        println!(
            "Wrote {} ({}x{}, {} bytes, algo={}, palette={}, {}ms)",
            report.output.as_deref().unwrap_or("<stdout>"),
            report.output_dimensions[0],
            report.output_dimensions[1],
            report.bytes_written,
            report.algorithm,
            report.palette_name,
            report.elapsed_ms,
        );
    }
    if let Some(preview) = &report.preview_braille {
        println!("\n{preview}");
    }
    if let Some(recipe) = &report.recipe {
        println!("\nRecipe JSON:\n{}", serde_json::to_string_pretty(recipe)?);
    }
    Ok(())
}

pub(crate) fn palette_spec_label(spec: &PaletteSpec) -> String {
    match spec {
        PaletteSpec::Builtin(name) => name.clone(),
        PaletteSpec::Colors(c) => format!("custom({} colors)", c.len()),
        PaletteSpec::File(p) => format!("file:{}", p.display()),
        PaletteSpec::Adaptive(n) => format!("auto-{n}"),
    }
}
