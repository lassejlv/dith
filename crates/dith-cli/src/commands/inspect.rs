//! The `dith inspect` command: image analysis with recommended dithering commands.

use std::path::PathBuf;

use anyhow::Result;
use clap::Args;

use dith_core::inspect::inspect_image;

#[derive(Debug, Args)]
pub struct InspectArgs {
    /// Path to the image file to analyze.
    pub input: PathBuf,

    /// Number of dominant colors to extract via Oklab k-means++ (2..=16, default 6).
    #[arg(long, default_value_t = 6)]
    pub colors: usize,
}

pub(crate) fn handle_inspect(args: &InspectArgs, as_json: bool) -> Result<()> {
    let report = inspect_image(&args.input, args.colors)?;
    if as_json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!(
            "Image: {} ({}x{}, {})",
            report.path, report.width, report.height, report.color_type
        );
        println!(
            "Tone:  mean={:.2}  p5={:.2}  median={:.2}  p95={:.2}  dynamic_range={:.2}  grayscale={}",
            report.mean_luminance,
            report.p5_luminance,
            report.p50_luminance,
            report.p95_luminance,
            report.dynamic_range,
            report.is_grayscale
        );
        println!(
            "Alpha: transparent={} ({:.1}%)",
            report.has_transparency, report.transparent_percent
        );
        println!("Dominant colors: {}", report.dominant_colors.join(" "));
        println!("\nRecommended commands:");
        for rec in &report.recommendations {
            println!("  * {} — {}", rec.style, rec.rationale);
            println!("    {}", rec.example_command);
        }
    }
    Ok(())
}
