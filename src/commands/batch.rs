//! The `dith batch` command: dither many input images in parallel.

use std::path::PathBuf;

use anyhow::{Result, anyhow};
use clap::Args;
use rayon::prelude::*;
use serde::Serialize;

use crate::cli::DitherFlags;
use crate::output::{OutputFormat, encode_image, write_output_file};
use crate::preprocess::plan_dimensions;
use crate::recipe::run_pipeline;

use super::shared::load_input_image;

#[derive(Debug, Args)]
pub struct BatchArgs {
    /// One or more input image paths to process in parallel.
    #[arg(required = true)]
    pub inputs: Vec<PathBuf>,

    /// Output directory where dithered files will be written.
    #[arg(short = 'd', long, value_name = "DIR")]
    pub out_dir: PathBuf,

    /// Suffix appended to each output filename stem (default "-dithered").
    #[arg(long, default_value = "-dithered")]
    pub suffix: String,

    /// Refuse to overwrite existing files in --out-dir.
    #[arg(long)]
    pub no_clobber: bool,

    /// Validate and plan all batch jobs without writing files.
    #[arg(long)]
    pub dry_run: bool,

    #[command(flatten)]
    pub flags: DitherFlags,
}

#[derive(Debug, Serialize)]
struct BatchItemReport {
    input: String,
    output: String,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    output_dimensions: Option<[u32; 2]>,
    bytes_written: usize,
    elapsed_ms: u128,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct BatchReport {
    status: &'static str,
    dry_run: bool,
    out_dir: String,
    total: usize,
    succeeded: usize,
    failed: usize,
    items: Vec<BatchItemReport>,
}

pub(crate) fn handle_batch(args: &BatchArgs, as_json: bool) -> Result<()> {
    let recipe = args.flags.resolve_recipe(true)?;
    let _ = recipe.validate_static_palette()?;
    let format = recipe.export.format.unwrap_or(OutputFormat::Png);

    let items: Vec<BatchItemReport> = args
        .inputs
        .par_iter()
        .map(|input| {
            let stem = input
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("image");
            let out_path = args
                .out_dir
                .join(format!("{stem}{}.{}", args.suffix, format.id()));

            if args.no_clobber && out_path.exists() {
                return BatchItemReport {
                    input: input.display().to_string(),
                    output: out_path.display().to_string(),
                    status: "error",
                    output_dimensions: None,
                    bytes_written: 0,
                    elapsed_ms: 0,
                    error: Some(format!("output already exists: {}", out_path.display())),
                };
            }

            if args.dry_run {
                let dims = image::image_dimensions(input).ok().map(|(w, h)| {
                    let (ow, oh) = plan_dimensions(w, h, &recipe.preprocess);
                    [ow, oh]
                });
                return BatchItemReport {
                    input: input.display().to_string(),
                    output: out_path.display().to_string(),
                    status: "ok",
                    output_dimensions: dims,
                    bytes_written: 0,
                    elapsed_ms: 0,
                    error: None,
                };
            }

            let process = || -> Result<([u32; 2], usize, u128)> {
                let img = load_input_image(input)?;
                let out = run_pipeline(&img, &recipe)?;
                let bytes = encode_image(&out.image, format, &recipe.export)?;
                write_output_file(&out_path, &bytes)?;
                Ok((
                    [out.output_width, out.output_height],
                    bytes.len(),
                    out.elapsed_ms,
                ))
            };

            match process() {
                Ok((dims, bytes, ms)) => BatchItemReport {
                    input: input.display().to_string(),
                    output: out_path.display().to_string(),
                    status: "ok",
                    output_dimensions: Some(dims),
                    bytes_written: bytes,
                    elapsed_ms: ms,
                    error: None,
                },
                Err(err) => BatchItemReport {
                    input: input.display().to_string(),
                    output: out_path.display().to_string(),
                    status: "error",
                    output_dimensions: None,
                    bytes_written: 0,
                    elapsed_ms: 0,
                    error: Some(format!("{err:#}")),
                },
            }
        })
        .collect();

    let succeeded = items.iter().filter(|i| i.status == "ok").count();
    let failed = items.len() - succeeded;
    let report = BatchReport {
        status: if failed == 0 { "ok" } else { "partial_failure" },
        dry_run: args.dry_run,
        out_dir: args.out_dir.display().to_string(),
        total: items.len(),
        succeeded,
        failed,
        items,
    };

    if as_json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!(
            "Batch complete: {}/{} succeeded into {}",
            report.succeeded, report.total, report.out_dir
        );
        for item in &report.items {
            if let Some(err) = &item.error {
                eprintln!("  FAILED {}: {}", item.input, err);
            }
        }
    }

    if failed > 0 {
        return Err(anyhow!("{failed} batch item(s) failed"));
    }
    Ok(())
}
