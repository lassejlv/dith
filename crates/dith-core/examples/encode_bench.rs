//! Size and time benchmark for the still-image encoders.
//!
//! Reads every `.png` in a directory of already dithered images, encodes each one in the
//! requested formats and prints one tab-separated line per result: image, format, bytes and
//! the fastest encode time in milliseconds. With an output directory, the encoded files are
//! written there so they can be decoded and compared against the inputs.
//!
//! ```text
//! cargo run --release -p dith-core --example encode_bench -- <stills-dir> [--out <dir>]
//!     [--formats png,webp,gif,svg] [--filter <substring>] [--reps <n>]
//! ```

use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use dith_core::output::SvgShape;
use dith_core::{ExportOptions, OutputFormat, encode_image};

const ALL_FORMATS: &str = "png,png-opt,webp,gif,bmp,tiff,jpeg,jpeg-opt,svg,svg-circle";

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let stills = PathBuf::from(
        args.next()
            .context("usage: encode_bench <stills-dir> [...]")?,
    );
    let mut out_dir: Option<PathBuf> = None;
    let mut formats = String::from("png,webp,gif,bmp,tiff,jpeg,svg");
    let mut filter = String::new();
    let mut reps = 1usize;
    while let Some(flag) = args.next() {
        let value = args
            .next()
            .with_context(|| format!("{flag} needs a value"))?;
        match flag.as_str() {
            "--out" => out_dir = Some(PathBuf::from(value)),
            "--formats" if value == "all" => formats = ALL_FORMATS.to_string(),
            "--formats" => formats = value,
            "--filter" => filter = value,
            "--reps" => reps = value.parse().context("--reps expects a number")?,
            other => bail!("unknown flag {other}"),
        }
    }
    if let Some(dir) = &out_dir {
        std::fs::create_dir_all(dir)?;
    }

    let mut inputs: Vec<PathBuf> = std::fs::read_dir(&stills)
        .with_context(|| format!("reading {}", stills.display()))?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "png"))
        .filter(|p| p.to_string_lossy().contains(&filter))
        .collect();
    inputs.sort();

    println!("image\tformat\tbytes\tms");
    for path in &inputs {
        let stem = path.file_stem().unwrap_or_default().to_string_lossy();
        let img = dith_core::image::open(path)
            .with_context(|| format!("decoding {}", path.display()))?
            .to_rgba8();
        for name in formats.split(',') {
            let mut export = ExportOptions::default();
            let (format, extension) = match name {
                "png" => (OutputFormat::Png, "png"),
                "png-opt" => {
                    export.optimize = true;
                    (OutputFormat::Png, "opt.png")
                }
                "webp" => (OutputFormat::Webp, "webp"),
                "gif" => (OutputFormat::Gif, "gif"),
                "bmp" => (OutputFormat::Bmp, "bmp"),
                "tiff" => (OutputFormat::Tiff, "tiff"),
                "jpeg" => (OutputFormat::Jpeg, "jpg"),
                "jpeg-opt" => {
                    export.optimize = true;
                    (OutputFormat::Jpeg, "opt.jpg")
                }
                "svg" => (OutputFormat::Svg, "svg"),
                "svg-circle" => {
                    export.svg_shape = SvgShape::Circle;
                    (OutputFormat::Svg, "circle.svg")
                }
                other => bail!("unknown format {other}"),
            };
            let mut best_ms = f64::INFINITY;
            let mut bytes = Vec::new();
            for _ in 0..reps.max(1) {
                let start = Instant::now();
                bytes = encode_image(&img, format, &export)?;
                best_ms = best_ms.min(start.elapsed().as_secs_f64() * 1000.0);
            }
            println!("{stem}\t{name}\t{}\t{best_ms:.1}", bytes.len());
            if let Some(dir) = &out_dir {
                std::fs::write(dir.join(format!("{stem}.{extension}")), &bytes)?;
            }
        }
    }
    Ok(())
}
