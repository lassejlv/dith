//! The `dith plot` command: weighted Voronoi stippling for pen plotters, as SVG or G-code.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::{Args, ValueEnum};
use image::RgbaImage;
use serde::Serialize;

use crate::color::Rgb;
use crate::generate::{GenerateOptions, Pattern, generate_pattern};
use crate::output::write_output_file;
use crate::palette::Palette;
use crate::plot::{
    DotShape, MachineOptions, Orientation, Origin, PageLayout, Paper, PenPlot, PlotDocument,
    PlotDot, PlotFit, PlotFormat, draw_length, emphasize_edges, encode_gcode, encode_plot_svg,
    fmt_trim, plan_layout, raster_size, separate_inks,
};
use crate::preprocess::{AlphaMode, FitMode, PreprocessOptions, prepare_image};
use crate::recipe::RecipeError;
use crate::stipple::{StippleError, StippleOptions, stipple};
use crate::tour::{TourOptions, closed_length, plan_tour};

use super::shared::load_input_image;

#[derive(Debug, Args)]
pub struct PlotArgs {
    /// Input image path, '-' for stdin, or a procedural generator name (e.g. "sphere", "mesh").
    pub source: PathBuf,

    /// Output path (.svg or .gcode/.nc), or '-' for stdout.
    #[arg(short = 'o', long, value_name = "PATH|-")]
    pub output: Option<PathBuf>,

    /// Output format (inferred from the -o extension if omitted).
    #[arg(short = 'f', long, value_enum)]
    pub format: Option<PlotFormat>,

    /// Number of stipple points across all pens (default 5000).
    #[arg(short = 'n', long, conflicts_with = "density")]
    pub points: Option<usize>,

    /// Point budget as dots per cm² of solid ink (alternative to --points; scales with paper size).
    #[arg(long, value_name = "PER_CM2")]
    pub density: Option<f64>,

    /// Maximum Lloyd relaxation iterations (default 50; 0 = raw rejection samples).
    #[arg(long, default_value_t = 50)]
    pub iterations: u32,

    /// Stop relaxing once the mean point movement drops below this many raster pixels.
    #[arg(long, default_value_t = 0.02)]
    pub tolerance: f64,

    /// Longest side of the density raster in pixels (64..=8192, default 1200).
    #[arg(long, default_value_t = 1200)]
    pub resolution: u32,

    /// Deterministic seed for initial point sampling (and generator randomness).
    #[arg(long, default_value_t = 42)]
    pub seed: u64,

    /// Ink densities below this (0.0..1.0) are treated as bare paper.
    #[arg(long, default_value_t = 0.0)]
    pub cutoff: f32,

    /// Edge emphasis: adds this much Sobel edge strength to the ink density so eyes, lips, and
    /// outlines get extra dots (0.0..=4.0; 0.5..1.0 suits portraits).
    #[arg(long, default_value_t = 0.0)]
    pub edges: f32,

    /// Invert tones before stippling (light areas get ink).
    #[arg(long)]
    pub invert: bool,

    /// Brightness adjustment before stippling (-1.0..=1.0).
    #[arg(long, allow_hyphen_values = true)]
    pub brightness: Option<f32>,

    /// Contrast adjustment before stippling (-1.0..=1.0).
    #[arg(long, allow_hyphen_values = true)]
    pub contrast: Option<f32>,

    /// Gamma exponent before stippling (0.1..=5.0, <1 brightens midtones).
    #[arg(long)]
    pub gamma: Option<f32>,

    /// Gaussian blur sigma before stippling (0.0..=20.0).
    #[arg(long)]
    pub blur: Option<f32>,

    /// Unsharp mask before stippling (0.0..=5.0).
    #[arg(long)]
    pub sharpen: Option<f32>,

    /// Stretch the luminance histogram before stippling.
    #[arg(long)]
    pub auto_levels: bool,

    /// Built-in palette: the lightest color is the paper, every other color is one pen.
    #[arg(short = 'p', long, value_name = "NAME", conflicts_with = "colors")]
    pub palette: Option<String>,

    /// Custom comma-separated hex palette: lightest is the paper, the rest are pens (e.g. "#111111,#e11d48,#ffffff").
    #[arg(long, value_name = "HEX,HEX,...")]
    pub colors: Option<String>,

    /// Paper color; when set, every palette color is a pen (default: lightest palette color).
    #[arg(long, value_name = "HEX")]
    pub background: Option<Rgb>,

    /// Paper size: a2, a3, a4, a5, a6, letter, legal, tabloid, or `WxH[mm|cm|in]` like `300x200mm`.
    #[arg(long, default_value = "a4")]
    pub paper: String,

    /// Sheet orientation (auto = landscape for wide sources).
    #[arg(long, value_enum, default_value_t = Orientation::Auto)]
    pub orientation: Orientation,

    /// Margin on every side in mm.
    #[arg(long, default_value_t = 15.0)]
    pub margin: f64,

    /// Fit the source inside the margins (contain) or fill them by centre-cropping (cover).
    #[arg(long, value_enum, default_value_t = PlotFit::Contain)]
    pub fit: PlotFit,

    /// How each stipple is drawn: point (pen tap), circle, or stroke (short dash).
    #[arg(long, value_enum, default_value_t = DotShape::Point)]
    pub dot: DotShape,

    /// Pen tip width in mm (stroke width in SVG previews).
    #[arg(long, default_value_t = 0.5)]
    pub pen_width: f64,

    /// Largest dot diameter in mm for circle/stroke dots (darkest areas).
    #[arg(long, default_value_t = 1.2)]
    pub max_dot: f64,

    /// Smallest dot diameter in mm for circle/stroke dots (defaults to --max-dot = constant size).
    #[arg(long)]
    pub min_dot: Option<f64>,

    /// G-code lines that raise the pen ('\n' separates several lines).
    #[arg(long, default_value = "G0 Z2", allow_hyphen_values = true)]
    pub pen_up: String,

    /// G-code lines that lower the pen ('\n' separates several lines), e.g. "M3 S30".
    #[arg(long, default_value = "G1 Z0 F1000", allow_hyphen_values = true)]
    pub pen_down: String,

    /// G-code lines emitted at home before each additional pen.
    #[arg(long, default_value = "M0 ; change pen", allow_hyphen_values = true)]
    pub pen_change: String,

    /// Pause after each pen-up/pen-down in seconds (G4 P<s>; 0 = none).
    #[arg(long, default_value_t = 0.0)]
    pub dwell: f64,

    /// Drawing feed rate in mm/min.
    #[arg(long, default_value_t = 3000.0)]
    pub draw_feed: f64,

    /// Travel feed rate in mm/min (also used for the plot-time estimate).
    #[arg(long, default_value_t = 6000.0)]
    pub travel_feed: f64,

    /// Where G-code X0 Y0 sits on the sheet (also the pen's home).
    #[arg(long, value_enum, default_value_t = Origin::BottomLeft)]
    pub origin: Origin,

    /// Skip 2-opt and keep the greedy nearest-neighbour tour.
    #[arg(long)]
    pub no_optimize: bool,

    /// Wall-time cap for 2-opt in seconds (0 = no cap). Hitting it makes output timing-dependent.
    #[arg(long, default_value_t = 10.0)]
    pub tour_time_limit: f64,

    /// Generator canvas direction / light angle in degrees.
    #[arg(long, default_value_t = 135.0, allow_hyphen_values = true)]
    pub angle: f32,

    /// Generator frequency / scale factor.
    #[arg(long, default_value_t = 1.0)]
    pub frequency: f32,

    /// Generator gradient stops in Oklab (e.g. "#0f172a,#f8fafc").
    #[arg(long, value_name = "HEX,HEX,...")]
    pub stops: Option<String>,

    /// Refuse to overwrite an existing output file.
    #[arg(long)]
    pub no_clobber: bool,

    /// Validate settings and plan the layout without stippling or writing files.
    #[arg(long)]
    pub dry_run: bool,
}

/// Per-pen statistics in the JSON report.
#[derive(Debug, Serialize)]
struct PenReport {
    index: usize,
    color: String,
    points: usize,
    iterations: u32,
    converged: bool,
    mean_movement_px: f64,
    dropped: usize,
    travel_unordered_mm: f64,
    travel_nearest_mm: f64,
    travel_mm: f64,
    draw_mm: f64,
    two_opt_moves: usize,
    estimated_seconds: f64,
}

#[derive(Debug, Serialize)]
struct TimingReport {
    prepare: u128,
    stipple: u128,
    tour: u128,
    encode: u128,
}

/// Structured result emitted for `dith plot`.
#[derive(Debug, Serialize)]
struct PlotReport {
    status: &'static str,
    dry_run: bool,
    source: String,
    output: Option<String>,
    format: &'static str,
    paper: Paper,
    layout: PageLayout,
    margin_mm: f64,
    fit: PlotFit,
    origin: Origin,
    dot: DotShape,
    pen_width_mm: f64,
    paper_color: String,
    raster_dimensions: [u32; 2],
    points_requested: usize,
    points: usize,
    iterations: u32,
    converged: bool,
    travel_unordered_mm: f64,
    travel_nearest_mm: f64,
    travel_mm: f64,
    travel_reduction_pct: f64,
    draw_mm: f64,
    pen_lifts: usize,
    draw_feed_mm_min: f64,
    travel_feed_mm_min: f64,
    estimated_plot_seconds: f64,
    tour_capped: bool,
    pens: Vec<PenReport>,
    bytes_written: usize,
    timings_ms: Option<TimingReport>,
    elapsed_ms: u128,
}

fn invalid(field: &'static str, reason: impl Into<String>) -> RecipeError {
    RecipeError::Validation {
        field,
        reason: reason.into(),
    }
}

fn check_range(field: &'static str, v: f64, lo: f64, hi: f64) -> Result<(), RecipeError> {
    if v.is_finite() && (lo..=hi).contains(&v) {
        Ok(())
    } else {
        Err(invalid(field, format!("{v} must be within {lo}..={hi}")))
    }
}

fn split_commands(raw: &str) -> Vec<String> {
    raw.replace("\\n", "\n")
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

fn validate(args: &PlotArgs) -> Result<(), RecipeError> {
    if let Some(n) = args.points
        && !(1..=1_000_000).contains(&n)
    {
        return Err(invalid("points", format!("{n} must be within 1..=1000000")));
    }
    if let Some(d) = args.density {
        check_range("density", d, 0.001, 10_000.0)?;
    }
    if args.iterations > 1000 {
        return Err(invalid("iterations", "must be at most 1000"));
    }
    check_range("tolerance", args.tolerance, 0.0, 100.0)?;
    if !(64..=8192).contains(&args.resolution) {
        return Err(invalid("resolution", "must be within 64..=8192"));
    }
    check_range("cutoff", f64::from(args.cutoff), 0.0, 0.99)?;
    check_range("edges", f64::from(args.edges), 0.0, 4.0)?;
    for (field, v) in [("brightness", args.brightness), ("contrast", args.contrast)] {
        if let Some(v) = v {
            check_range(field, f64::from(v), -1.0, 1.0)?;
        }
    }
    if let Some(g) = args.gamma {
        check_range("gamma", f64::from(g), 0.1, 5.0)?;
    }
    if let Some(b) = args.blur {
        check_range("blur", f64::from(b), 0.0, 20.0)?;
    }
    if let Some(s) = args.sharpen {
        check_range("sharpen", f64::from(s), 0.0, 5.0)?;
    }
    check_range("margin", args.margin, 0.0, 1000.0)?;
    check_range("pen-width", args.pen_width, 0.05, 10.0)?;
    check_range("max-dot", args.max_dot, 0.05, 50.0)?;
    if let Some(min) = args.min_dot {
        check_range("min-dot", min, 0.0, args.max_dot)?;
    }
    check_range("dwell", args.dwell, 0.0, 10.0)?;
    check_range("draw-feed", args.draw_feed, 1.0, 1_000_000.0)?;
    check_range("travel-feed", args.travel_feed, 1.0, 1_000_000.0)?;
    check_range("tour-time-limit", args.tour_time_limit, 0.0, 3600.0)?;
    if split_commands(&args.pen_up).is_empty() {
        return Err(invalid("pen-up", "needs at least one G-code line"));
    }
    if split_commands(&args.pen_down).is_empty() {
        return Err(invalid("pen-down", "needs at least one G-code line"));
    }
    Ok(())
}

/// Resolve `(paper color, pens)` from --palette / --colors / --background.
fn resolve_inks(args: &PlotArgs) -> Result<(Rgb, Vec<Rgb>), RecipeError> {
    let colors = if let Some(spec) = &args.colors {
        Some(Palette::from_hex_list(spec)?.colors().to_vec())
    } else if let Some(name) = &args.palette {
        Some(Palette::by_name(name)?.colors().to_vec())
    } else {
        None
    };
    let Some(colors) = colors else {
        return Ok((args.background.unwrap_or(Rgb::WHITE), vec![Rgb::BLACK]));
    };
    let paper = args.background.unwrap_or_else(|| {
        colors
            .iter()
            .copied()
            .max_by(|a, b| a.lightness().total_cmp(&b.lightness()))
            .unwrap_or(Rgb::WHITE)
    });
    let mut pens: Vec<Rgb> = Vec::new();
    for c in colors {
        if c != paper && !pens.contains(&c) {
            pens.push(c);
        }
    }
    if pens.is_empty() {
        return Err(invalid(
            "colors",
            "palette needs at least one pen color besides the paper",
        ));
    }
    if pens.len() > 16 {
        return Err(invalid(
            "colors",
            format!("{} pens given; at most 16 are supported", pens.len()),
        ));
    }
    Ok((paper, pens))
}

enum Source {
    Image(RgbaImage),
    Generator(Pattern),
}

fn resolve_source(path: &Path) -> Result<Source> {
    if path.as_os_str() != "-"
        && !path.exists()
        && let Some(name) = path.to_str()
        && let Ok(pattern) = Pattern::from_str(name, true)
    {
        return Ok(Source::Generator(pattern));
    }
    Ok(Source::Image(load_input_image(path)?))
}

/// Split `total` points across pens in proportion to ink mass (largest remainder, ties by index).
fn allocate_points(total: usize, masses: &[f64]) -> Vec<usize> {
    let sum: f64 = masses.iter().sum();
    if sum <= 0.0 {
        return vec![0; masses.len()];
    }
    let exact: Vec<f64> = masses.iter().map(|m| total as f64 * m / sum).collect();
    let mut counts: Vec<usize> = exact.iter().map(|e| e.floor() as usize).collect();
    let mut order: Vec<usize> = (0..masses.len()).collect();
    order.sort_by(|&a, &b| {
        (exact[b] - exact[b].floor())
            .total_cmp(&(exact[a] - exact[a].floor()))
            .then(a.cmp(&b))
    });
    let mut left = total - counts.iter().sum::<usize>();
    for i in order {
        if left == 0 {
            break;
        }
        counts[i] += 1;
        left -= 1;
    }
    counts
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

pub(crate) fn handle_plot(args: &PlotArgs, as_json: bool) -> Result<()> {
    let started = Instant::now();
    validate(args)?;
    let format = match (args.format, args.output.as_deref()) {
        (Some(f), _) => f,
        (None, Some(p)) if p.as_os_str() != "-" => PlotFormat::infer_from_path(p).ok_or_else(|| {
            invalid(
                "format",
                format!(
                    "cannot infer a plot format from '{}': use a .svg or .gcode extension or pass --format svg|gcode",
                    p.display()
                ),
            )
        })?,
        (None, _) => PlotFormat::Svg,
    };
    if args.output.is_none() && !args.dry_run {
        return Err(invalid("output", "pass -o <path.svg|path.gcode> or -o - for stdout").into());
    }
    if args.no_clobber
        && !args.dry_run
        && let Some(path) = &args.output
        && path.as_os_str() != "-"
        && path.exists()
    {
        return Err(RecipeError::OutputExists {
            path: path.display().to_string(),
        }
        .into());
    }
    let paper: Paper = args
        .paper
        .parse()
        .map_err(|e: String| invalid("paper", e))?;
    let (paper_color, pens) = resolve_inks(args)?;
    let stops = match &args.stops {
        Some(spec) => Palette::from_hex_list(spec)?.colors().to_vec(),
        None => vec![Rgb::from_u32(0x0A0A0D), Rgb::from_u32(0xF7F7F9)],
    };

    let prepare_started = Instant::now();
    let source = resolve_source(&args.source)?;
    let (source_label, aspect) = match &source {
        Source::Image(img) => (
            args.source.display().to_string(),
            f64::from(img.width()) / f64::from(img.height().max(1)),
        ),
        Source::Generator(p) => (format!("generate:{}", p.id()), 4.0 / 3.0),
    };
    let fit = if matches!(source, Source::Generator(_)) {
        PlotFit::Cover
    } else {
        args.fit
    };
    let layout = plan_layout(&paper, args.orientation, args.margin, fit, aspect)
        .map_err(|e| invalid("margin", e))?;
    let [area_x, area_y, area_w, area_h] = layout.plot_area_mm;
    let (raster_w, raster_h) = raster_size(area_w, area_h, args.resolution);

    let base = match source {
        Source::Image(img) => img,
        Source::Generator(pattern) => generate_pattern(&GenerateOptions {
            pattern,
            width: raster_w,
            height: raster_h,
            angle: args.angle,
            frequency: args.frequency,
            stops,
            seed: args.seed,
            ..GenerateOptions::default()
        }),
    };
    let prep = PreprocessOptions {
        width: Some(raster_w),
        height: Some(raster_h),
        fit: match fit {
            PlotFit::Contain => FitMode::Stretch,
            PlotFit::Cover => FitMode::Cover,
        },
        brightness: args.brightness.unwrap_or(0.0),
        contrast: args.contrast.unwrap_or(0.0),
        gamma: args.gamma.unwrap_or(1.0),
        blur: args.blur.unwrap_or(0.0),
        sharpen: args.sharpen.unwrap_or(0.0),
        auto_levels: args.auto_levels,
        invert: args.invert,
        alpha_mode: AlphaMode::Flatten,
        background: paper_color,
        ..PreprocessOptions::default()
    };
    let working = prepare_image(&base, &prep).working;
    let mut densities = separate_inks(&working, paper_color, &pens, args.cutoff);
    if args.edges > 0.0 {
        for d in &mut densities {
            *d = emphasize_edges(d, args.edges);
        }
    }
    let masses: Vec<f64> = densities
        .iter()
        .map(crate::stipple::DensityMap::mass)
        .collect();
    let mm_per_px = [area_w / f64::from(raster_w), area_h / f64::from(raster_h)];
    let total_points = match (args.points, args.density) {
        (Some(n), _) => n,
        (None, Some(per_cm2)) => {
            let ink_cm2 = masses.iter().sum::<f64>() * mm_per_px[0] * mm_per_px[1] / 100.0;
            ((per_cm2 * ink_cm2).round() as usize).clamp(1, 1_000_000)
        }
        (None, None) => 5000,
    };
    let counts = allocate_points(total_points, &masses);
    let prepare_ms = prepare_started.elapsed().as_millis();

    let home = args.origin.home(layout.width_mm, layout.height_mm);
    let min_dot = args.min_dot.unwrap_or(args.max_dot);
    let tour_opts = TourOptions {
        two_opt: !args.no_optimize,
        time_limit: (args.tour_time_limit > 0.0)
            .then(|| Duration::from_secs_f64(args.tour_time_limit)),
    };
    let machine = MachineOptions {
        pen_up: split_commands(&args.pen_up),
        pen_down: split_commands(&args.pen_down),
        pen_change: split_commands(&args.pen_change),
        dwell_s: args.dwell,
        draw_feed: args.draw_feed,
        travel_feed: args.travel_feed,
        origin: args.origin,
    };

    let mut pen_plots = Vec::new();
    let mut pen_reports = Vec::new();
    let (mut stipple_ms, mut tour_ms) = (0, 0);
    let mut tour_capped = false;
    if !args.dry_run {
        if masses.iter().all(|&m| m <= 0.0) {
            return Err(invalid("source", StippleError::Blank.to_string()).into());
        }
        for (k, (density, &count)) in densities.iter().zip(&counts).enumerate() {
            if count == 0 {
                continue;
            }
            let t0 = Instant::now();
            let result = stipple(
                density,
                &StippleOptions {
                    points: count,
                    max_iterations: args.iterations,
                    tolerance: args.tolerance,
                    seed: args.seed.wrapping_add(k as u64),
                },
            )
            .map_err(|e| invalid("source", e.to_string()))?;
            stipple_ms += t0.elapsed().as_millis();

            let t1 = Instant::now();
            let dots: Vec<PlotDot> = result
                .stipples
                .iter()
                .map(|s| PlotDot {
                    x: area_x + s.x * mm_per_px[0],
                    y: area_y + s.y * mm_per_px[1],
                    diameter: min_dot + (args.max_dot - min_dot) * f64::from(s.darkness),
                })
                .collect();
            let centres: Vec<[f64; 2]> = dots.iter().map(|d| [d.x, d.y]).collect();
            let tour = plan_tour(&centres, home, &tour_opts);
            debug_assert!((closed_length(&centres, home, &tour.order) - tour.length).abs() < 1e-6);
            tour_ms += t1.elapsed().as_millis();
            tour_capped |= tour.capped;

            let ordered: Vec<PlotDot> = tour.order.iter().map(|&i| dots[i]).collect();
            let draw_mm: f64 = ordered
                .iter()
                .map(|d| draw_length(d, args.dot, args.pen_width))
                .sum();
            let estimated = tour.length / args.travel_feed * 60.0
                + draw_mm / args.draw_feed * 60.0
                + ordered.len() as f64 * 2.0 * args.dwell;
            pen_reports.push(PenReport {
                index: k + 1,
                color: pens[k].to_hex(),
                points: ordered.len(),
                iterations: result.iterations,
                converged: result.converged,
                mean_movement_px: (result.final_movement * 10_000.0).round() / 10_000.0,
                dropped: result.dropped,
                travel_unordered_mm: round2(tour.unordered_length),
                travel_nearest_mm: round2(tour.nearest_length),
                travel_mm: round2(tour.length),
                draw_mm: round2(draw_mm),
                two_opt_moves: tour.moves,
                estimated_seconds: round2(estimated),
            });
            pen_plots.push(PenPlot {
                color: pens[k],
                dots: ordered,
            });
        }
    }

    let sum = |f: fn(&PenReport) -> f64| round2(pen_reports.iter().map(f).sum());
    let travel_nearest_mm = sum(|p| p.travel_nearest_mm);
    let travel_mm = sum(|p| p.travel_mm);
    let draw_mm = sum(|p| p.draw_mm);
    let points: usize = pen_reports.iter().map(|p| p.points).sum();
    let estimated_plot_seconds = sum(|p| p.estimated_seconds);

    let mut header = vec![
        format!(
            "dith {} plot: weighted Voronoi stippling",
            env!("CARGO_PKG_VERSION")
        ),
        format!(
            "source: {}",
            std::path::Path::new(&source_label).file_name().map_or_else(
                || source_label.clone(),
                |name| name.to_string_lossy().into_owned()
            )
        ),
        format!(
            "paper: {} {}x{}mm {}, margin {}mm, fit {}",
            paper.name,
            fmt_trim(layout.width_mm, 2),
            fmt_trim(layout.height_mm, 2),
            if layout.orientation == Orientation::Landscape {
                "landscape"
            } else {
                "portrait"
            },
            fmt_trim(args.margin, 2),
            fit.to_possible_value()
                .map_or_else(String::new, |v| v.get_name().to_string()),
        ),
        format!(
            "plot area: x {} y {} w {} h {} mm, raster {raster_w}x{raster_h}px",
            fmt_trim(area_x, 2),
            fmt_trim(area_y, 2),
            fmt_trim(area_w, 2),
            fmt_trim(area_h, 2)
        ),
        format!(
            "stipples: {points} points, {} iterations max, tolerance {}px, seed {}, edges {}",
            args.iterations,
            fmt_trim(args.tolerance, 4),
            args.seed,
            fmt_trim(f64::from(args.edges), 3)
        ),
        if args.dot == DotShape::Point {
            format!("dots: point, pen width {}mm", fmt_trim(args.pen_width, 3))
        } else {
            format!(
                "dots: {}, pen width {}mm, diameter {}..{}mm",
                args.dot
                    .to_possible_value()
                    .map_or_else(String::new, |v| v.get_name().to_string()),
                fmt_trim(args.pen_width, 3),
                fmt_trim(min_dot, 3),
                fmt_trim(args.max_dot, 3)
            )
        },
        format!(
            "travel: {}mm after 2-opt ({}mm nearest-neighbour), draw {}mm, est {}s",
            fmt_trim(travel_mm, 1),
            fmt_trim(travel_nearest_mm, 1),
            fmt_trim(draw_mm, 1),
            fmt_trim(estimated_plot_seconds, 0)
        ),
    ];
    if format == PlotFormat::Gcode {
        header.push(format!(
            "units mm, origin {}, pen-up \"{}\", pen-down \"{}\", dwell {}s",
            args.origin
                .to_possible_value()
                .map_or_else(String::new, |v| v.get_name().to_string()),
            machine.pen_up.join(" / "),
            machine.pen_down.join(" / "),
            fmt_trim(args.dwell, 3)
        ));
        header.push(format!(
            "feeds: draw {} mm/min, travel {} mm/min",
            fmt_trim(args.draw_feed, 1),
            fmt_trim(args.travel_feed, 1)
        ));
    }

    let encode_started = Instant::now();
    let bytes_written = if args.dry_run {
        0
    } else {
        let doc = PlotDocument {
            layout: &layout,
            pens: &pen_plots,
            shape: args.dot,
            pen_width_mm: args.pen_width,
            header: &header,
        };
        let encoded = match format {
            PlotFormat::Svg => encode_plot_svg(&doc),
            PlotFormat::Gcode => encode_gcode(&doc, &machine),
        };
        match args.output.as_deref() {
            Some(p) if p.as_os_str() == "-" => {
                if !as_json {
                    let mut stdout = io::stdout().lock();
                    stdout
                        .write_all(encoded.as_bytes())
                        .context("writing plot to stdout")?;
                    stdout.flush().context("flushing stdout")?;
                }
            }
            Some(p) => write_output_file(p, encoded.as_bytes())?,
            None => {}
        }
        encoded.len()
    };
    let encode_ms = encode_started.elapsed().as_millis();

    let report = PlotReport {
        status: "ok",
        dry_run: args.dry_run,
        source: source_label,
        output: args.output.as_ref().map(|p| p.display().to_string()),
        format: format.id(),
        paper,
        layout,
        margin_mm: args.margin,
        fit,
        origin: args.origin,
        dot: args.dot,
        pen_width_mm: args.pen_width,
        paper_color: paper_color.to_hex(),
        raster_dimensions: [raster_w, raster_h],
        points_requested: total_points,
        points,
        iterations: pen_reports.iter().map(|p| p.iterations).max().unwrap_or(0),
        converged: pen_reports.iter().all(|p| p.converged),
        travel_unordered_mm: sum(|p| p.travel_unordered_mm),
        travel_nearest_mm,
        travel_mm,
        travel_reduction_pct: if travel_nearest_mm > 0.0 {
            round2((1.0 - travel_mm / travel_nearest_mm) * 100.0)
        } else {
            0.0
        },
        draw_mm,
        pen_lifts: points,
        draw_feed_mm_min: args.draw_feed,
        travel_feed_mm_min: args.travel_feed,
        estimated_plot_seconds,
        tour_capped,
        pens: pen_reports,
        bytes_written,
        timings_ms: (!args.dry_run).then_some(TimingReport {
            prepare: prepare_ms,
            stipple: stipple_ms,
            tour: tour_ms,
            encode: encode_ms,
        }),
        elapsed_ms: started.elapsed().as_millis(),
    };

    if as_json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else if report.dry_run {
        println!(
            "Dry run: {} -> {} ({}, {} points requested, {} pens, {}x{}mm {}, raster {}x{})",
            report.source,
            report.output.as_deref().unwrap_or("<none>"),
            report.format,
            report.points_requested,
            pens.len(),
            fmt_trim(report.layout.width_mm, 1),
            fmt_trim(report.layout.height_mm, 1),
            report.paper.name,
            raster_w,
            raster_h,
        );
    } else if args.output.as_deref() != Some(Path::new("-")) {
        println!(
            "Wrote {} ({}, {} points, {} pens, travel {}mm (nearest-neighbour {}mm), est {}s, {} bytes, {}ms)",
            report.output.as_deref().unwrap_or("<none>"),
            report.format,
            report.points,
            report.pens.len(),
            fmt_trim(report.travel_mm, 0),
            fmt_trim(report.travel_nearest_mm, 0),
            fmt_trim(report.estimated_plot_seconds, 0),
            report.bytes_written,
            report.elapsed_ms,
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn points_are_allocated_by_ink_mass() {
        assert_eq!(allocate_points(10, &[1.0, 1.0, 2.0]), vec![3, 2, 5]);
        assert_eq!(allocate_points(7, &[0.0, 5.0]), vec![0, 7]);
        assert_eq!(allocate_points(5, &[0.0, 0.0]), vec![0, 0]);
        assert_eq!(
            allocate_points(1000, &[0.3, 0.7]).iter().sum::<usize>(),
            1000
        );
    }

    #[test]
    fn pen_commands_split_on_escaped_newlines() {
        assert_eq!(
            split_commands("M3 S30\\nG4 P0.2"),
            vec!["M3 S30", "G4 P0.2"]
        );
        assert_eq!(split_commands("  "), Vec::<String>::new());
    }
}
