//! Input loading, output destination, and render report helpers shared by several commands.

use std::cell::RefCell;
use std::io::{self, IsTerminal, Read, Write};
use std::path::Path;

use anyhow::{Context, Result};
use image::RgbaImage;
use serde::Serialize;

use crate::color::ColorSpace;
use crate::output::{
    ColorUsage, ExportOptions, KittyOptions, OutputFormat, render_kitty, write_output_file,
};
use crate::recipe::{DitherRecipe, PaletteSpec, RecipeError};

/// Longest side, in pixels, of the preview image an in-process [`capture`] keeps.
pub(crate) const PREVIEW_MAX_DIM: u32 = 512;

/// What a command handler produced while running under [`capture`] instead of printing.
#[derive(Debug, Default)]
pub(crate) struct Capture {
    /// The handler's JSON report, serialized compactly.
    pub(crate) report: Option<String>,
    /// The rendered result, box-averaged by a whole factor to at most [`PREVIEW_MAX_DIM`] on its
    /// longest side, so every preview pixel covers the same source area and dither screens do not
    /// beat into moiré.
    pub(crate) preview: Option<RgbaImage>,
}

thread_local! {
    static CAPTURE: RefCell<Option<Capture>> = const { RefCell::new(None) };
}

/// Run a command handler in-process with its JSON report and preview image captured instead of
/// written to stdout, so `dith mcp` can reuse the exact CLI code paths on its protocol channel.
pub(crate) fn capture(run: impl FnOnce() -> Result<()>) -> (Result<()>, Capture) {
    CAPTURE.set(Some(Capture::default()));
    let result = run();
    (result, CAPTURE.take().unwrap_or_default())
}

/// Print a JSON report to stdout (pretty), or hand it to the active [`capture`] (compact).
pub(crate) fn emit_json<T: Serialize + ?Sized>(value: &T) -> Result<()> {
    let capturing = CAPTURE.with_borrow(Option::is_some);
    if !capturing {
        println!("{}", serde_json::to_string_pretty(value)?);
        return Ok(());
    }
    let json = serde_json::to_string(value)?;
    CAPTURE.with_borrow_mut(|slot| {
        if let Some(capture) = slot {
            capture.report = Some(json);
        }
    });
    Ok(())
}

/// Offer the rendered image as the preview of an active [`capture`]; a no-op for the CLI.
///
/// Only the first offer is kept, and only a downscaled copy is stored.
pub(crate) fn offer_preview(img: &RgbaImage) {
    CAPTURE.with_borrow_mut(|slot| {
        let Some(capture) = slot else { return };
        if capture.preview.is_some() {
            return;
        }
        let (w, h) = img.dimensions();
        let factor = w.max(h).div_ceil(PREVIEW_MAX_DIM);
        capture.preview = Some(if factor <= 1 {
            img.clone()
        } else {
            image::imageops::thumbnail(img, (w / factor).max(1), (h / factor).max(1))
        });
    });
}

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

pub(crate) fn load_input_image(path: &Path) -> Result<RgbaImage> {
    if path.as_os_str() == "-" {
        let mut buf = Vec::new();
        io::stdin()
            .read_to_end(&mut buf)
            .context("reading input image bytes from stdin")?;
        let img = image::load_from_memory(&buf).context("decoding input image from stdin")?;
        Ok(img.to_rgba8())
    } else {
        let img = image::open(path)
            .with_context(|| format!("opening input image '{}'", path.display()))?;
        Ok(img.to_rgba8())
    }
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
        return emit_json(report);
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
