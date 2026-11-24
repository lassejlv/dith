//! The `dith generate` command: procedurally generate and dither an image in one step.

use std::io::{self, IsTerminal};
use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::Args;

use crate::cli::DitherFlags;
use crate::color::Rgb;
use crate::generate::{GenerateOptions, Pattern, generate_pattern};
use crate::output::{OutputFormat, encode_image, render_braille};
use crate::palette::Palette;
use crate::preprocess::plan_dimensions;
use crate::preset::{OverwritePolicy, PresetScope, SavePresetRequest, save_local_preset};
use crate::recipe::{RecipeError, run_pipeline};
use crate::viewer::{FullscreenViewerInput, ViewerBackend, run_fullscreen_viewer};

use super::shared::{
    RenderReport, emit_inline_kitty, emit_render_report, ensure_output_destination,
    palette_spec_label, resolve_output_format, write_destination,
};

#[derive(Debug, Args)]
pub struct GenerateArgs {
    /// Procedural source pattern to generate before dithering.
    #[arg(value_enum)]
    pub pattern: Pattern,

    /// Output path, or '-' for stdout. Optional for --view or text/terminal formats.
    #[arg(short = 'o', long, value_name = "PATH|-")]
    pub output: Option<PathBuf>,

    /// Canvas width in pixels (default 640).
    #[arg(long = "canvas-width", default_value_t = 640)]
    pub canvas_width: u32,

    /// Canvas height in pixels (default 480).
    #[arg(long = "canvas-height", default_value_t = 480)]
    pub canvas_height: u32,

    /// Direction / light angle in degrees (default 135).
    #[arg(long, default_value_t = 135.0, allow_hyphen_values = true)]
    pub angle: f32,

    /// Pattern frequency / scale factor (default 1.0).
    #[arg(long, default_value_t = 1.0)]
    pub frequency: f32,

    /// Comma-separated source gradient stops interpolated in Oklab (e.g. "#0f172a,#38bdf8,#f8fafc").
    #[arg(long, value_name = "HEX,HEX,...")]
    pub stops: Option<String>,

    /// Refuse to overwrite an existing output file.
    #[arg(long)]
    pub no_clobber: bool,

    /// Validate settings and plan output without writing files.
    #[arg(long)]
    pub dry_run: bool,

    /// Print the fully resolved JSON recipe.
    #[arg(long)]
    pub emit_recipe: bool,

    /// Save the resolved settings as a reusable local preset in `.dith/presets/<NAME>.json`.
    #[arg(long, value_name = "NAME")]
    pub save_preset: Option<String>,

    /// Target scope when using --save-preset (project = .dith/presets, user = ~/.config/dith/presets).
    #[arg(long, value_enum, default_value_t = PresetScope::Project)]
    pub preset_scope: PresetScope,

    /// Include a compact Unicode Braille visual preview in output.
    #[arg(long)]
    pub preview: bool,

    /// Open a fullscreen terminal preview via Kitty graphics protocol (works with or without -o; add --inline for scrollback).
    #[arg(long)]
    pub view: bool,

    /// Render --view inline in terminal scrollback instead of fullscreen.
    #[arg(long, conflicts_with = "fullscreen")]
    pub inline: bool,

    /// Open a fullscreen terminal preview via Kitty graphics protocol (implies --view; fullscreen is already the default for --view).
    #[arg(short = 'F', long, conflicts_with = "inline")]
    pub fullscreen: bool,

    #[command(flatten)]
    pub flags: DitherFlags,
}

pub(crate) fn handle_generate(args: &GenerateArgs, as_json: bool) -> Result<()> {
    let mut recipe = args.flags.resolve_recipe_for(true, true)?;
    let wants_view = args.view || args.fullscreen;
    let wants_fullscreen = wants_view && !args.inline;
    let format = resolve_output_format(args.output.as_deref(), recipe.export.format, wants_view)?;
    ensure_output_destination(
        args.output.as_deref(),
        format,
        args.no_clobber,
        args.dry_run,
    )?;

    let stops = if let Some(spec) = &args.stops {
        Palette::from_hex_list(spec)?.colors().to_vec()
    } else {
        // Default smooth dark -> light stops so every palette spans its full tonal range cleanly.
        vec![Rgb::from_u32(0x0A0A0D), Rgb::from_u32(0xF7F7F9)]
    };

    let gen_w = recipe.preprocess.width.unwrap_or(args.canvas_width).max(1);
    let gen_h = recipe
        .preprocess
        .height
        .unwrap_or(args.canvas_height)
        .max(1);
    // Avoid double-resizing when canvas already matches requested width/height.
    recipe.preprocess.width = None;
    recipe.preprocess.height = None;

    let gen_opts = GenerateOptions {
        pattern: args.pattern,
        width: gen_w,
        height: gen_h,
        angle: args.angle,
        frequency: args.frequency,
        stops,
        seed: recipe.dither.seed,
        ..GenerateOptions::default()
    };

    if args.dry_run {
        let palette_colors = recipe.validate_static_palette()?;
        let (out_w, out_h) = plan_dimensions(gen_w, gen_h, &recipe.preprocess);
        let ps = recipe.preprocess.pixel_size.max(1);
        let report = RenderReport {
            status: "ok",
            dry_run: true,
            source: format!("generate:{}", args.pattern.id()),
            output: args.output.as_ref().map(|p| p.display().to_string()),
            format: format.id(),
            preset: recipe.preset.clone(),
            algorithm: recipe.dither.algorithm.id(),
            color_space: recipe.dither.color_space,
            palette_name: palette_spec_label(&recipe.palette),
            palette_colors,
            source_dimensions: [gen_w, gen_h],
            working_dimensions: [(out_w / ps).max(1), (out_h / ps).max(1)],
            output_dimensions: [out_w, out_h],
            bytes_written: 0,
            elapsed_ms: 0,
            color_usage: Vec::new(),
            preview_braille: None,
            text_output: None,
            saved_preset_path: None,
            recipe: Some(recipe),
        };
        emit_render_report(&report, as_json, false)?;
        return Ok(());
    }

    let saved_preset_path = if let Some(preset_name) = &args.save_preset {
        let (_, path, _) = save_local_preset(SavePresetRequest {
            slug: preset_name,
            title: None,
            description: None,
            tags: Vec::new(),
            recipe: recipe.clone(),
            scope: args.preset_scope,
            custom_dir: None,
            overwrite: OverwritePolicy::Replace,
        })
        .map_err(RecipeError::from)?;
        Some(path.display().to_string())
    } else {
        None
    };

    let src_img = generate_pattern(&gen_opts);
    let output_data = run_pipeline(&src_img, &recipe)?;

    let interactive_fullscreen = wants_fullscreen && !as_json && io::stdout().is_terminal();
    let encoded = encode_image(&output_data.image, format, &recipe.export)?;
    let bytes_written = if interactive_fullscreen && args.output.is_none() {
        0
    } else {
        write_destination(args.output.as_deref(), &encoded, format, as_json)?
    };

    let preview_braille = if args.preview {
        Some(render_braille(&output_data.image, Some(48)))
    } else {
        None
    };
    let text_output = if as_json && args.output.is_none() && format.is_text() {
        String::from_utf8(encoded).ok()
    } else {
        None
    };

    let report = RenderReport {
        status: "ok",
        dry_run: false,
        source: format!("generate:{}", args.pattern.id()),
        output: args.output.as_ref().map(|p| p.display().to_string()),
        format: format.id(),
        preset: recipe.preset.clone(),
        algorithm: recipe.dither.algorithm.id(),
        color_space: recipe.dither.color_space,
        palette_name: output_data.palette.name.clone(),
        palette_colors: Some(output_data.palette.hex_colors()),
        source_dimensions: [output_data.source_width, output_data.source_height],
        working_dimensions: [output_data.working_width, output_data.working_height],
        output_dimensions: [output_data.output_width, output_data.output_height],
        bytes_written,
        elapsed_ms: output_data.elapsed_ms,
        color_usage: output_data.histogram,
        preview_braille,
        text_output,
        saved_preset_path,
        recipe: args.emit_recipe.then_some(recipe.clone()),
    };

    if interactive_fullscreen {
        let title = format!(
            "{} · {} · {}",
            args.pattern.id(),
            recipe
                .preset
                .as_deref()
                .unwrap_or_else(|| recipe.dither.algorithm.id()),
            output_data.palette.name
        );
        run_fullscreen_viewer(&FullscreenViewerInput {
            primary: &output_data.image,
            raw_source: Some(&src_img),
            title,
            start_raw: false,
            backend: ViewerBackend::Kitty,
        })?;
        if args.output.is_some() {
            emit_render_report(&report, false, false)?;
        }
        return Ok(());
    }

    let wrote_stdout_payload =
        !as_json && (args.output.as_deref() == Some(Path::new("-")) || args.output.is_none());
    emit_render_report(&report, as_json, wrote_stdout_payload)?;

    if wants_view && args.output.is_some() && format != OutputFormat::Kitty {
        emit_inline_kitty(&output_data.image, &recipe.export, as_json)?;
    }
    Ok(())
}
