//! The `dith apply` command: dither an input image into any output format.

use std::io::{self, IsTerminal};
use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::Args;

use crate::cli::{DitherFlags, OverlayFlags};
use crate::viewer::{FullscreenViewerInput, ViewerBackend, run_fullscreen_viewer};
use dith_core::output::{OutputFormat, encode_image, render_braille};
use dith_core::preprocess::plan_dimensions;
use dith_core::preset::{OverwritePolicy, PresetScope, SavePresetRequest, save_local_preset};
use dith_core::recipe::{RecipeError, run_pipeline, run_pipeline_owned};

use super::shared::{
    RenderReport, emit_inline_kitty, emit_render_report, ensure_output_destination,
    load_input_image, palette_spec_label, resolve_output_format, write_destination,
};

#[derive(Debug, Args)]
pub struct ApplyArgs {
    /// Input image path, or '-' to read image bytes from stdin.
    pub input: PathBuf,

    /// Output path, or '-' to write encoded bytes to stdout. Optional for braille/ascii formats.
    #[arg(short = 'o', long, value_name = "PATH|-")]
    pub output: Option<PathBuf>,

    /// Replace an existing output file (by default, existing files are overwritten unless --no-clobber is set).
    #[arg(long, conflicts_with = "no_clobber")]
    pub overwrite: bool,

    /// Refuse to overwrite an existing output file (safe mode for automated pipelines).
    #[arg(long, conflicts_with = "overwrite")]
    pub no_clobber: bool,

    /// Validate settings and compute planned output dimensions without writing any file.
    #[arg(long)]
    pub dry_run: bool,

    /// Print the fully resolved JSON recipe (instead of or alongside execution).
    #[arg(long)]
    pub emit_recipe: bool,

    /// Save the resolved settings as a reusable local preset in `.dith/presets/<NAME>.json`.
    #[arg(long, value_name = "NAME")]
    pub save_preset: Option<String>,

    /// Target scope when using --save-preset (project = .dith/presets, user = ~/.config/dith/presets).
    #[arg(long, value_enum, default_value_t = PresetScope::Project)]
    pub preset_scope: PresetScope,

    /// Include a compact Unicode Braille visual preview in output (great for terminal agents).
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
    pub overlay: OverlayFlags,

    #[command(flatten)]
    pub flags: DitherFlags,
}

pub(crate) fn handle_apply(args: &ApplyArgs, as_json: bool) -> Result<()> {
    let reading_stdin = args.input.as_os_str() == "-";
    let mut recipe = args.flags.resolve_recipe(!reading_stdin)?;
    args.overlay.apply(&mut recipe)?;
    let wants_view = args.view || args.fullscreen;
    let wants_fullscreen = wants_view && !args.inline;

    let format = resolve_output_format(args.output.as_deref(), recipe.export.format, wants_view)?;
    ensure_output_destination(
        args.output.as_deref(),
        format,
        args.no_clobber,
        args.dry_run,
    )?;

    if args.dry_run {
        let palette_colors = recipe.validate_static_palette()?;
        let (src_w, src_h) = if reading_stdin {
            (0, 0)
        } else {
            dith_core::input::image_dimensions(&args.input)?
        };
        let (out_w, out_h) = if src_w > 0 && src_h > 0 {
            plan_dimensions(src_w, src_h, &recipe.preprocess)
        } else {
            (0, 0)
        };
        let ps = recipe.preprocess.pixel_size.max(1);
        let report = RenderReport {
            status: "ok",
            dry_run: true,
            source: args.input.display().to_string(),
            output: args.output.as_ref().map(|p| p.display().to_string()),
            format: format.id(),
            preset: recipe.preset.clone(),
            algorithm: recipe.dither.algorithm.id(),
            color_space: recipe.dither.color_space,
            palette_name: palette_spec_label(&recipe.palette),
            palette_colors,
            source_dimensions: [src_w, src_h],
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

    let src_img = load_input_image(&args.input, Some(&recipe.preprocess))?;
    let interactive_fullscreen = wants_fullscreen && !as_json && io::stdout().is_terminal();
    // Only the fullscreen viewer shows the source again; otherwise the pipeline can consume it.
    let (output_data, raw_source) = if interactive_fullscreen {
        (run_pipeline(&src_img, &recipe)?, Some(src_img))
    } else {
        (run_pipeline_owned(src_img, &recipe)?, None)
    };
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
        source: args.input.display().to_string(),
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
            args.input.display(),
            recipe
                .preset
                .as_deref()
                .unwrap_or_else(|| recipe.dither.algorithm.id()),
            output_data.palette.name
        );
        run_fullscreen_viewer(&FullscreenViewerInput {
            primary: &output_data.image,
            raw_source: raw_source.as_ref(),
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

    // When --view --inline is requested alongside a saved file (-o <path>), render inline via Kitty graphics.
    if wants_view && args.output.is_some() && format != OutputFormat::Kitty {
        emit_inline_kitty(&output_data.image, &recipe.export, as_json)?;
    }
    Ok(())
}
