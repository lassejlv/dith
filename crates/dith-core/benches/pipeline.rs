//! Stage timings for the dith-core pipeline, with no dependency beyond the crate itself.
//!
//! ```text
//! cargo bench -p dith-core --bench pipeline                    # synthetic 2048x2048 source
//! cargo bench -p dith-core --bench pipeline -- dither/floyd    # only stages containing a filter
//! DITH_BENCH_IMAGE=photo.jpg cargo bench -p dith-core --bench pipeline
//! ```
//!
//! Each stage runs `DITH_BENCH_RUNS` times (default 7) and the median wall clock is printed with
//! the throughput in megapixels per second. Set `RAYON_NUM_THREADS=1` to time a single thread.

use std::hint::black_box;
use std::time::{Duration, Instant};

use dith_core::color::ColorSpace;
use dith_core::dither::{Algorithm, DitherOptions, dither_image};
use dith_core::generate::{GenerateOptions, Pattern, generate_pattern};
use dith_core::output::color_histogram;
use dith_core::palette::{Palette, extract_adaptive_palette};
use dith_core::preprocess::{PreprocessOptions, prepare_image};
use dith_core::recipe::{DitherRecipe, run_pipeline};
use image::RgbaImage;

struct Bench {
    filters: Vec<String>,
    runs: usize,
}

impl Bench {
    fn run<T>(&self, name: &str, megapixels: f64, mut stage: impl FnMut() -> T) {
        if !self.filters.is_empty() && !self.filters.iter().any(|f| name.contains(f.as_str())) {
            return;
        }
        let mut times: Vec<Duration> = (0..self.runs)
            .map(|_| {
                let started = Instant::now();
                black_box(stage());
                started.elapsed()
            })
            .collect();
        times.sort();
        let median = times[times.len() / 2];
        println!(
            "{name:44} {:9.2} ms {:9.1} MP/s",
            median.as_secs_f64() * 1e3,
            megapixels / median.as_secs_f64()
        );
    }
}

fn source() -> RgbaImage {
    if let Some(path) = std::env::var_os("DITH_BENCH_IMAGE") {
        return image::open(&path)
            .unwrap_or_else(|err| panic!("opening {}: {err}", path.display()))
            .to_rgba8();
    }
    // Colorful fractal noise: photo-like tonal spread with detail at every scale.
    generate_pattern(&GenerateOptions {
        pattern: Pattern::Noise,
        width: 2048,
        height: 2048,
        frequency: 3.0,
        stops: ["#0b1026", "#7a3b69", "#e8a04c", "#f6f1e7"]
            .iter()
            .map(|hex| hex.parse().expect("valid hex stop"))
            .collect(),
        ..GenerateOptions::default()
    })
}

fn main() {
    let bench = Bench {
        filters: std::env::args()
            .skip(1)
            .filter(|arg| !arg.starts_with("--"))
            .collect(),
        runs: std::env::var("DITH_BENCH_RUNS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(7),
    };
    let src = source();
    let megapixels = f64::from(src.width()) * f64::from(src.height()) / 1e6;
    println!(
        "source {}x{} ({megapixels:.1} MP)",
        src.width(),
        src.height()
    );

    let palettes = ["bw", "pico8", "web216"].map(|id| Palette::by_name(id).expect("palette"));
    for algorithm in [
        Algorithm::FloydSteinberg,
        Algorithm::Atkinson,
        Algorithm::JarvisJudiceNinke,
        Algorithm::Ostromoukhov,
        Algorithm::Bayer8,
        Algorithm::Halftone,
        Algorithm::BlueNoise,
        Algorithm::Ign,
        Algorithm::Riemersma,
        Algorithm::Threshold,
    ] {
        for palette in &palettes {
            let opts = DitherOptions {
                algorithm,
                ..DitherOptions::default()
            };
            bench.run(
                &format!("dither/{}/{}/oklab", algorithm.id(), palette.name),
                megapixels,
                || dither_image(&src, palette, &opts),
            );
        }
    }
    for (space, label) in [(ColorSpace::Linear, "linear"), (ColorSpace::Srgb, "srgb")] {
        for algorithm in [Algorithm::FloydSteinberg, Algorithm::Bayer8] {
            let opts = DitherOptions {
                algorithm,
                color_space: space,
                ..DitherOptions::default()
            };
            bench.run(
                &format!("dither/{}/pico8/{label}", algorithm.id()),
                megapixels,
                || dither_image(&src, &palettes[1], &opts),
            );
        }
    }
    let noisy = DitherOptions {
        noise: 0.1,
        ..DitherOptions::default()
    };
    bench.run("dither/floyd-steinberg/pico8/noise", megapixels, || {
        dither_image(&src, &palettes[1], &noisy)
    });

    let preprocess = |name: &str, opts: PreprocessOptions| {
        bench.run(&format!("preprocess/{name}"), megapixels, || {
            prepare_image(&src, &opts)
        });
    };
    preprocess("none", PreprocessOptions::default());
    preprocess(
        "resize-half",
        PreprocessOptions {
            scale: Some(0.5),
            ..PreprocessOptions::default()
        },
    );
    preprocess(
        "pixel-size-4",
        PreprocessOptions {
            pixel_size: 4,
            ..PreprocessOptions::default()
        },
    );
    preprocess(
        "tone",
        PreprocessOptions {
            contrast: 0.2,
            gamma: 0.9,
            saturation: 1.2,
            ..PreprocessOptions::default()
        },
    );
    preprocess(
        "contrast-only",
        PreprocessOptions {
            contrast: 0.2,
            brightness: 0.05,
            ..PreprocessOptions::default()
        },
    );
    preprocess(
        "grayscale",
        PreprocessOptions {
            grayscale: true,
            ..PreprocessOptions::default()
        },
    );
    preprocess(
        "sharpen",
        PreprocessOptions {
            sharpen: 1.0,
            ..PreprocessOptions::default()
        },
    );
    preprocess(
        "blur",
        PreprocessOptions {
            blur: 1.5,
            ..PreprocessOptions::default()
        },
    );
    preprocess(
        "auto-levels",
        PreprocessOptions {
            auto_levels: true,
            ..PreprocessOptions::default()
        },
    );

    bench.run("palette/auto-16", megapixels, || {
        extract_adaptive_palette(&src, 16, 42)
    });
    let dithered = dither_image(&src, &palettes[1], &DitherOptions::default());
    bench.run("report/color-histogram", megapixels, || {
        color_histogram(&dithered)
    });
    for preset in ["mac-classic", "risograph", "manga", "pico8"] {
        let recipe = DitherRecipe::from_preset(preset).expect("preset");
        bench.run(&format!("pipeline/{preset}"), megapixels, || {
            run_pipeline(&src, &recipe).expect("pipeline")
        });
    }

    for pattern in Pattern::ALL {
        let opts = GenerateOptions {
            pattern: *pattern,
            width: 1024,
            height: 1024,
            ..GenerateOptions::default()
        };
        bench.run(&format!("generate/{}", pattern.id()), 1.048_576, || {
            generate_pattern(&opts)
        });
    }
}
