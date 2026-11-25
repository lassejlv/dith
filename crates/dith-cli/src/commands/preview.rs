//! The `dith preview` command: fullscreen or inline terminal previews of dithered images.

use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Args;
use serde::Serialize;

use crate::animate::{AnimDitherOptions, decode_animated_frames, dither_animation_frames};
use crate::cli::{DitherFlags, PreviewMode};
use crate::color::Rgb;
use crate::generate::{GenerateOptions, Pattern, generate_pattern};
use crate::output::{ColorUsage, KittyOptions, render_ascii, render_braille, render_kitty};
use crate::palette::Palette;
use crate::recipe::run_pipeline;
use crate::viewer::{
    FullscreenAnimInput, FullscreenViewerInput, ViewerBackend, run_fullscreen_animation_viewer,
    run_fullscreen_viewer,
};

use super::shared::load_input_image;

#[derive(Debug, Args)]
pub struct PreviewArgs {
    /// Input image path, or '-' for stdin (omit when using --pattern).
    pub input: Option<PathBuf>,

    /// Generate a procedural pattern to preview instead of loading a file.
    #[arg(long, value_enum)]
    pub pattern: Option<Pattern>,

    /// Terminal preview renderer (default: kitty graphics protocol).
    #[arg(short = 'm', long, value_enum, default_value_t = PreviewMode::Kitty)]
    pub mode: PreviewMode,

    /// Preview the image directly without running dithering (useful for before/after comparison).
    #[arg(long)]
    pub raw: bool,

    /// Render inline in terminal scrollback instead of the default fullscreen alternate-screen viewer.
    #[arg(long, conflicts_with = "fullscreen")]
    pub inline: bool,

    /// Explicitly request fullscreen alternate-screen viewer (enabled by default on interactive TTYs).
    #[arg(short = 'F', long, conflicts_with = "inline")]
    pub fullscreen: bool,

    /// Display width in terminal columns (inline mode).
    #[arg(long)]
    pub cols: Option<u32>,

    /// Display height in terminal rows (inline Kitty mode).
    #[arg(long)]
    pub rows: Option<u32>,

    /// Canvas width in pixels when using --pattern (default 640).
    #[arg(long = "canvas-width", default_value_t = 640)]
    pub canvas_width: u32,

    /// Canvas height in pixels when using --pattern (default 480).
    #[arg(long = "canvas-height", default_value_t = 480)]
    pub canvas_height: u32,

    /// Direction / light angle in degrees when using --pattern (default 135).
    #[arg(long, default_value_t = 135.0, allow_hyphen_values = true)]
    pub angle: f32,

    /// Pattern frequency / scale factor when using --pattern (default 1.0).
    #[arg(long, default_value_t = 1.0)]
    pub frequency: f32,

    /// Comma-separated source gradient stops when using --pattern.
    #[arg(long, value_name = "HEX,HEX,...")]
    pub stops: Option<String>,

    #[command(flatten)]
    pub flags: DitherFlags,
}

#[derive(Debug, Serialize)]
struct PreviewReport {
    status: &'static str,
    mode: &'static str,
    fullscreen: bool,
    source: String,
    raw: bool,
    preset: Option<String>,
    algorithm: Option<&'static str>,
    palette_name: Option<String>,
    palette_colors: Option<Vec<String>>,
    dimensions: [u32; 2],
    elapsed_ms: u128,
    color_usage: Vec<ColorUsage>,
    preview_payload: String,
}

pub(crate) fn handle_preview(args: &PreviewArgs, as_json: bool) -> Result<()> {
    let reading_stdin = args.input.as_deref() == Some(Path::new("-"));
    let procedural = args.input.is_none() && args.pattern.is_some();
    let mut recipe = args.flags.resolve_recipe_for(!reading_stdin, procedural)?;
    let use_fullscreen = !args.inline;
    let interactive_fullscreen = use_fullscreen && !as_json && io::stdout().is_terminal();

    // If previewing a multi-frame GIF / APNG / animated WebP interactively, play ALL frames live!
    if interactive_fullscreen
        && let Some(path) = &args.input
        && path.as_os_str() != "-"
        && let Ok(Some(raw_frames)) = decode_animated_frames(path, None, Some(180))
    {
        let backend = match args.mode {
            PreviewMode::Kitty => ViewerBackend::Kitty,
            PreviewMode::Braille => ViewerBackend::Braille,
            PreviewMode::Ascii => ViewerBackend::Ascii,
        };
        if args.raw {
            run_fullscreen_animation_viewer(&FullscreenAnimInput {
                frames: &raw_frames,
                raw_frames: None,
                title: format!("{} · raw", path.display()),
                backend,
            })?;
        } else {
            let anim =
                dither_animation_frames(&raw_frames, &recipe, &AnimDitherOptions::default())?;
            let style = recipe
                .preset
                .as_deref()
                .unwrap_or_else(|| recipe.dither.algorithm.id());
            run_fullscreen_animation_viewer(&FullscreenAnimInput {
                frames: &anim.frames,
                raw_frames: Some(&raw_frames),
                title: format!("{} · {style} · {}", path.display(), anim.palette.name),
                backend,
            })?;
        }
        return Ok(());
    }

    let (source_label, src_img) = match (&args.input, args.pattern) {
        (Some(path), _) => (path.display().to_string(), load_input_image(path)?),
        (None, pat) => {
            let pattern = pat.unwrap_or(Pattern::Sphere);
            let stops = if let Some(spec) = &args.stops {
                Palette::from_hex_list(spec)?.colors().to_vec()
            } else {
                vec![Rgb::from_u32(0x0A0A0D), Rgb::from_u32(0xF7F7F9)]
            };
            let gen_w = recipe.preprocess.width.unwrap_or(args.canvas_width).max(1);
            let gen_h = recipe
                .preprocess
                .height
                .unwrap_or(args.canvas_height)
                .max(1);
            recipe.preprocess.width = None;
            recipe.preprocess.height = None;
            let img = generate_pattern(&GenerateOptions {
                pattern,
                width: gen_w,
                height: gen_h,
                angle: args.angle,
                frequency: args.frequency,
                stops,
                seed: recipe.dither.seed,
                ..GenerateOptions::default()
            });
            (format!("generate:{}", pattern.id()), img)
        }
    };

    let dithered_out = if args.raw && !interactive_fullscreen {
        None
    } else {
        Some(run_pipeline(&src_img, &recipe)?)
    };

    // Fullscreen alternate-screen viewer is enabled by default on interactive TTYs (never blocks agents/pipes).
    if interactive_fullscreen {
        let primary = dithered_out.as_ref().map_or(&src_img, |out| &out.image);
        let style_label = if let Some(out) = &dithered_out {
            format!(
                "{} · {}",
                recipe
                    .preset
                    .as_deref()
                    .unwrap_or_else(|| recipe.dither.algorithm.id()),
                out.palette.name
            )
        } else {
            "raw".to_string()
        };
        let backend = match args.mode {
            PreviewMode::Kitty => ViewerBackend::Kitty,
            PreviewMode::Braille => ViewerBackend::Braille,
            PreviewMode::Ascii => ViewerBackend::Ascii,
        };
        run_fullscreen_viewer(&FullscreenViewerInput {
            primary,
            raw_source: Some(&src_img),
            title: format!("{source_label} · {style_label}"),
            start_raw: args.raw,
            backend,
        })?;
        return Ok(());
    }

    let (img, algorithm, palette_name, palette_colors, elapsed_ms, color_usage) = if args.raw {
        let hist = crate::output::color_histogram(&src_img);
        (src_img, None, None, None, 0, hist)
    } else {
        let out = dithered_out.expect("dithered pipeline ran for non-raw preview");
        (
            out.image,
            Some(recipe.dither.algorithm.id()),
            Some(out.palette.name.clone()),
            Some(out.palette.hex_colors()),
            out.elapsed_ms,
            out.histogram,
        )
    };

    let cols = args
        .cols
        .or(recipe.export.view_cols)
        .or(recipe.export.text_width);
    let rows = args.rows.or(recipe.export.view_rows);

    let payload = match args.mode {
        PreviewMode::Kitty => render_kitty(
            &img,
            KittyOptions {
                cols,
                rows,
                tmux_passthrough: !as_json && KittyOptions::should_use_tmux_passthrough(),
                trailing_newline: true,
                no_cursor_move: false,
                image_id: None,
            },
        )?,
        PreviewMode::Braille => render_braille(&img, cols),
        PreviewMode::Ascii => render_ascii(&img, cols),
    };

    if as_json {
        let report = PreviewReport {
            status: "ok",
            mode: args.mode.id(),
            fullscreen: use_fullscreen,
            source: source_label,
            raw: args.raw,
            preset: (!args.raw).then(|| recipe.preset.clone()).flatten(),
            algorithm,
            palette_name,
            palette_colors,
            dimensions: [img.width(), img.height()],
            elapsed_ms,
            color_usage,
            preview_payload: payload,
        };
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        let mut stdout = io::stdout().lock();
        stdout
            .write_all(payload.as_bytes())
            .context("writing preview output to stdout")?;
        stdout.flush().context("flushing stdout")?;
    }

    Ok(())
}
