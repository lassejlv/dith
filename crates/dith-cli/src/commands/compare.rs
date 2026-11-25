//! The `dith compare` command: contact sheets and variant folders comparing algorithms, presets, or palettes.

use std::io::{self, IsTerminal};
use std::path::PathBuf;

use anyhow::Result;
use clap::Args;
use rayon::prelude::*;
use serde::Serialize;

use crate::cli::DitherFlags;
use crate::viewer::{FullscreenViewerInput, ViewerBackend, run_fullscreen_viewer};
use dith_core::compare::{CompareTile, compose_contact_sheet};
use dith_core::dither::Algorithm;
use dith_core::generate::{GenerateOptions, Pattern, generate_pattern};
use dith_core::output::{ExportOptions, OutputFormat, encode_image, write_output_file};
use dith_core::palette::Palette;
use dith_core::recipe::{DitherRecipe, PaletteSpec, RecipeError, run_pipeline};

use super::shared::{emit_inline_kitty, load_input_image, palette_spec_label};

#[derive(Debug, Args)]
pub struct CompareArgs {
    /// Input image path (omit when using --pattern to compare procedural art).
    pub input: Option<PathBuf>,

    /// Generate a procedural pattern as the comparison source instead of an input file.
    #[arg(long, value_enum)]
    pub pattern: Option<Pattern>,

    /// Output path for the labeled contact sheet PNG/WebP.
    #[arg(short = 'o', long, value_name = "PATH")]
    pub output: Option<PathBuf>,

    /// Also (or instead) write each variant as its own individual image in this directory.
    #[arg(long, value_name = "DIR")]
    pub out_dir: Option<PathBuf>,

    /// Comma-separated algorithms to compare (e.g. "floyd-steinberg,atkinson,ostromoukhov,bayer4,blue-noise").
    #[arg(long, value_delimiter = ',')]
    pub algorithms: Vec<Algorithm>,

    /// Comma-separated preset slugs to compare (e.g. "mac-classic,editorial-print,risograph,film-stipple").
    #[arg(long, value_delimiter = ',')]
    pub presets: Vec<String>,

    /// Comma-separated built-in palette slugs to compare (e.g. "bw,newsprint,risograph-blue,gameboy").
    #[arg(long, value_delimiter = ',')]
    pub palettes: Vec<String>,

    /// Number of columns in the contact sheet (auto square grid if omitted).
    #[arg(long)]
    pub columns: Option<u32>,

    /// Maximum longest dimension per tile on the contact sheet (default 360).
    #[arg(long, default_value_t = 360)]
    pub tile_max_dim: u32,

    /// Use a light warm paper contact-sheet frame instead of dark studio matte.
    #[arg(long)]
    pub light: bool,

    /// Open the labeled contact sheet in a fullscreen terminal viewer (works with or without -o; add --inline for scrollback).
    #[arg(long)]
    pub view: bool,

    /// Render --view inline in terminal scrollback instead of fullscreen.
    #[arg(long, conflicts_with = "fullscreen")]
    pub inline: bool,

    /// Open the labeled contact sheet in a fullscreen terminal viewer (implies --view; fullscreen is default for --view).
    #[arg(short = 'F', long, conflicts_with = "inline")]
    pub fullscreen: bool,

    /// Validate comparison plan without writing files.
    #[arg(long)]
    pub dry_run: bool,

    #[command(flatten)]
    pub flags: DitherFlags,
}

#[derive(Debug, Serialize)]
struct CompareVariantReport {
    label: String,
    algorithm: &'static str,
    palette: String,
    preset: Option<String>,
    file: Option<String>,
    elapsed_ms: u128,
}

#[derive(Debug, Serialize)]
struct CompareReport {
    status: &'static str,
    dry_run: bool,
    source: String,
    sheet_output: Option<String>,
    sheet_dimensions: [u32; 2],
    out_dir: Option<String>,
    variants: Vec<CompareVariantReport>,
}

pub(crate) fn handle_compare(args: &CompareArgs, as_json: bool) -> Result<()> {
    let wants_view = args.view || args.fullscreen;
    if args.output.is_none() && args.out_dir.is_none() && !wants_view && !args.dry_run {
        return Err(RecipeError::Validation {
            field: "output",
            reason:
                "specify -o <sheet.png>, --out-dir <dir>, --view, --fullscreen, or a combination"
                    .to_string(),
        }
        .into());
    }

    let base_recipe = args
        .flags
        .resolve_recipe_for(true, args.input.is_none() && args.pattern.is_some())?;
    let variant_specs = build_compare_variants(args, &base_recipe)?;

    let source_label = match (&args.input, args.pattern) {
        (Some(p), _) => p.display().to_string(),
        (None, Some(pat)) => format!("generate:{}", pat.id()),
        (None, None) => "generate:sphere".to_string(),
    };

    if args.dry_run {
        let variants = variant_specs
            .iter()
            .map(|(label, rec)| CompareVariantReport {
                label: label.clone(),
                algorithm: rec.dither.algorithm.id(),
                palette: palette_spec_label(&rec.palette),
                preset: rec.preset.clone(),
                file: args
                    .out_dir
                    .as_ref()
                    .map(|d| d.join(format!("{label}.png")).display().to_string()),
                elapsed_ms: 0,
            })
            .collect();
        let report = CompareReport {
            status: "ok",
            dry_run: true,
            source: source_label,
            sheet_output: args.output.as_ref().map(|p| p.display().to_string()),
            sheet_dimensions: [0, 0],
            out_dir: args.out_dir.as_ref().map(|p| p.display().to_string()),
            variants,
        };
        if as_json {
            println!("{}", serde_json::to_string_pretty(&report)?);
        } else {
            println!(
                "Dry run: would render {} comparison variants from {}",
                report.variants.len(),
                report.source
            );
        }
        return Ok(());
    }

    let src_img = match (&args.input, args.pattern) {
        (Some(path), _) => load_input_image(path)?,
        (None, pat) => generate_pattern(&GenerateOptions {
            pattern: pat.unwrap_or(Pattern::Sphere),
            width: args.tile_max_dim.max(240),
            height: args.tile_max_dim.max(240),
            seed: base_recipe.dither.seed,
            ..GenerateOptions::default()
        }),
    };

    // Render all variants in parallel via rayon.
    let rendered: Result<
        Vec<(String, DitherRecipe, dith_core::recipe::PipelineOutput)>,
        RecipeError,
    > = variant_specs
        .into_par_iter()
        .map(|(label, mut rec)| {
            if rec.preprocess.max_dim.is_none()
                && rec.preprocess.width.is_none()
                && rec.preprocess.height.is_none()
            {
                rec.preprocess.max_dim = Some(args.tile_max_dim.max(32));
            }
            let out = run_pipeline(&src_img, &rec)?;
            Ok((label, rec, out))
        })
        .collect();
    let rendered = rendered?;

    let mut variant_reports = Vec::with_capacity(rendered.len());
    let mut tiles = Vec::with_capacity(rendered.len());

    for (label, rec, out) in rendered {
        let mut written_file = None;
        if let Some(dir) = &args.out_dir {
            let safe_name = label.replace(|c: char| !c.is_ascii_alphanumeric() && c != '-', "_");
            let fmt = rec.export.format.unwrap_or(OutputFormat::Png);
            let file_path = dir.join(format!("{safe_name}.{}", fmt.id()));
            let bytes = encode_image(&out.image, fmt, &rec.export)?;
            write_output_file(&file_path, &bytes)?;
            written_file = Some(file_path.display().to_string());
        }
        variant_reports.push(CompareVariantReport {
            label: label.clone(),
            algorithm: rec.dither.algorithm.id(),
            palette: out.palette.name.clone(),
            preset: rec.preset.clone(),
            file: written_file,
            elapsed_ms: out.elapsed_ms,
        });
        tiles.push(CompareTile {
            label,
            image: out.image,
        });
    }

    let mut sheet_dims = [0u32; 2];
    let composed_sheet = if args.output.is_some() || wants_view {
        let sheet = compose_contact_sheet(&tiles, args.columns, !args.light);
        sheet_dims = [sheet.width(), sheet.height()];
        if let Some(sheet_path) = &args.output {
            let fmt = OutputFormat::infer_from_path(sheet_path);
            let export = ExportOptions {
                optimize: base_recipe.export.optimize,
                ..ExportOptions::default()
            };
            let bytes = encode_image(&sheet, fmt, &export)?;
            write_output_file(sheet_path, &bytes)?;
        }
        Some(sheet)
    } else {
        None
    };

    let report = CompareReport {
        status: "ok",
        dry_run: false,
        source: source_label.clone(),
        sheet_output: args.output.as_ref().map(|p| p.display().to_string()),
        sheet_dimensions: sheet_dims,
        out_dir: args.out_dir.as_ref().map(|p| p.display().to_string()),
        variants: variant_reports,
    };

    if wants_view
        && !args.inline
        && !as_json
        && io::stdout().is_terminal()
        && let Some(sheet) = &composed_sheet
    {
        run_fullscreen_viewer(&FullscreenViewerInput {
            primary: sheet,
            raw_source: Some(&src_img),
            title: format!(
                "compare · {source_label} ({} variants)",
                report.variants.len()
            ),
            start_raw: false,
            backend: ViewerBackend::Kitty,
        })?;
        if let Some(sheet_out) = &report.sheet_output {
            println!(
                "Wrote contact sheet {} ({}x{}, {} variants)",
                sheet_out,
                sheet_dims[0],
                sheet_dims[1],
                report.variants.len()
            );
        }
        return Ok(());
    }

    if as_json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        if let Some(sheet) = &report.sheet_output {
            println!(
                "Wrote contact sheet {} ({}x{}, {} variants)",
                sheet,
                sheet_dims[0],
                sheet_dims[1],
                report.variants.len()
            );
        }
        if let Some(dir) = &report.out_dir {
            println!(
                "Wrote {} individual variants to {}",
                report.variants.len(),
                dir
            );
        }
    }

    if wants_view && let Some(sheet) = &composed_sheet {
        emit_inline_kitty(sheet, &base_recipe.export, as_json)?;
    }

    Ok(())
}

fn build_compare_variants(
    args: &CompareArgs,
    base: &DitherRecipe,
) -> Result<Vec<(String, DitherRecipe)>, RecipeError> {
    if !args.presets.is_empty() {
        return args
            .presets
            .iter()
            .map(|name| {
                let mut rec = DitherRecipe::from_preset(name)?;
                if args.input.is_none() && args.pattern.is_some() {
                    rec.strip_photo_prep();
                }
                rec.export.optimize |= base.export.optimize;
                Ok((rec.preset.clone().unwrap_or_else(|| name.clone()), rec))
            })
            .collect();
    }
    if !args.algorithms.is_empty() {
        return Ok(args
            .algorithms
            .iter()
            .map(|&algo| {
                let mut rec = base.clone();
                rec.dither.algorithm = algo;
                (algo.id().to_string(), rec)
            })
            .collect());
    }
    if !args.palettes.is_empty() {
        return args
            .palettes
            .iter()
            .map(|pal| {
                let _ = Palette::by_name(pal)?;
                let mut rec = base.clone();
                rec.palette = PaletteSpec::Builtin(pal.clone());
                Ok((pal.clone(), rec))
            })
            .collect();
    }

    // Default curated showcase comparison when no explicit axis is passed!
    let default_algos = [
        Algorithm::FloydSteinberg,
        Algorithm::Atkinson,
        Algorithm::Ostromoukhov,
        Algorithm::Bayer4,
        Algorithm::Halftone,
        Algorithm::BlueNoise,
    ];
    Ok(default_algos
        .into_iter()
        .map(|algo| {
            let mut rec = base.clone();
            rec.dither.algorithm = algo;
            (algo.id().to_string(), rec)
        })
        .collect())
}
