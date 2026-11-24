//! The `dith animate` command: procedural loops, GIF re-dithers, frame sequences, and boiled stills.

use std::io::{self, IsTerminal};
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};
use clap::{Args, ValueEnum};
use serde::Serialize;

use crate::animate::{
    AnimDitherOptions, AnimFormat, AnimFrame, AnimatedOutput, Easing, FloatRange, ProceduralMotion,
    ScreenDrift, SweepMode, TemporalMode, U32Range, apply_holds, apply_ping_pong,
    compose_spritesheet, decode_animated_frames, dither_animation_frames, encode_animated_webp,
    encode_apng, encode_gif, generate_procedural_frames, write_frames_directory,
};
use crate::cli::{DitherFlags, PreviewMode};
use crate::color::{ColorSpace, Rgb};
use crate::generate::{GenerateOptions, Pattern};
use crate::output::{
    ColorUsage, OutputFormat, encode_image, render_ascii, render_braille, write_output_file,
};
use crate::palette::Palette;
use crate::preprocess::{FitMode, plan_dimensions};
use crate::preset::{OverwritePolicy, PresetScope, SavePresetRequest, save_local_preset};
use crate::recipe::{DitherRecipe, RecipeError};
use crate::viewer::{FullscreenAnimInput, ViewerBackend, run_fullscreen_animation_viewer};

use super::shared::{emit_inline_kitty, load_input_image, palette_spec_label};

#[derive(Debug, Args)]
pub struct AnimateArgs {
    /// Input GIF, static photo (for animated film-boil), image sequence, or procedural pattern name (e.g. "sphere", "waves", "mesh").
    pub inputs: Vec<PathBuf>,

    /// Explicitly generate a seamless procedural loop instead of reading input files.
    #[arg(long, value_enum)]
    pub pattern: Option<Pattern>,

    /// Output path (.gif, .apng, .webp, or .png sprite sheet). Optional when using --view or --frames-dir.
    #[arg(short = 'o', long, value_name = "PATH")]
    pub output: Option<PathBuf>,

    /// Output animation container (auto-inferred from -o extension if omitted).
    #[arg(long, value_enum)]
    pub anim_format: Option<AnimFormat>,

    /// Also (or instead) write numbered lossless PNG frames (`frame_0001.png`, ...) into this directory.
    #[arg(long, value_name = "DIR")]
    pub frames_dir: Option<PathBuf>,

    /// Number of frames for procedural loops or static-photo film boil (2..=240, default 24).
    #[arg(long)]
    pub frames: Option<usize>,

    /// Target loop duration in seconds (alternative to --frames; computes frames = duration * fps).
    #[arg(long, conflicts_with = "frames")]
    pub duration: Option<f32>,

    /// Playback frame rate in FPS (1..=60, default 15; also overrides input GIF timing when passed explicitly).
    #[arg(long)]
    pub fps: Option<u32>,

    /// Loop count for GIF / APNG / WebP (0 = infinite loop, default 0).
    #[arg(long, default_value_t = 0)]
    pub loops: u16,

    /// Temporal dither mode across frames (locked = fixed screen, boil = Risograph film boil, grain = lively stipple).
    /// Locked is only flicker-free with per-pixel algorithms (ordered screens, blue-noise, ign, random); error
    /// diffusion propagates motion into static areas.
    #[arg(long, value_enum)]
    pub temporal: Option<TemporalMode>,

    /// Film-boil cadence: update boil pattern every N frames (1 = every frame, 2 = on twos, 3 = on threes).
    #[arg(long, default_value_t = 1)]
    pub boil_rate: u32,

    /// Explicit boil/grain jitter intensity in 0.0..=0.5 (overrides temporal default).
    #[arg(long)]
    pub boil_amount: Option<f32>,

    /// Per-frame ordered/halftone/scanline screen drift in cells, e.g. "1,0" or "0,-1".
    #[arg(long, value_name = "DX,DY", allow_hyphen_values = true)]
    pub screen_drift: Option<ScreenDrift>,

    /// Motion & sweep easing curve (linear, ease-in-out, ease-in, ease-out, smoothstep, bounce, steps).
    #[arg(long, value_enum, default_value_t = Easing::Linear)]
    pub easing: Easing,

    /// Quantized step count when --easing steps is selected (default 6).
    #[arg(long, default_value_t = 6)]
    pub easing_steps: u32,

    /// Number of full procedural motion cycles per loop (default 1.0).
    #[arg(long, default_value_t = 1.0)]
    pub cycles: f32,

    /// Starting loop phase offset in 0.0..=1.0 (default 0.0, useful to sync multiple animations).
    #[arg(long, default_value_t = 0.0)]
    pub phase_offset: f32,

    /// Procedural motion travel/orbit amplitude multiplier (0.0 = still, 1.0 = default, 2.0 = dramatic).
    #[arg(long, default_value_t = 1.0)]
    pub motion_amplitude: f32,

    /// Play procedural motion or input frames in reverse.
    #[arg(long)]
    pub reverse: bool,

    /// Append reversed interior frames for a seamless boomerang ping-pong loop.
    #[arg(long)]
    pub ping_pong: bool,

    /// Extra dwell frames holding on the first frame (e.g. dramatic opening pause).
    #[arg(long, default_value_t = 0)]
    pub hold_start: usize,

    /// Extra dwell frames holding on the final frame (e.g. pause on finished reveal).
    #[arg(long, default_value_t = 0)]
    pub hold_end: usize,

    /// How --sweep-* parameters evolve over frames (linear = start->end, pulse = start->end->start).
    #[arg(long, value_enum, default_value_t = SweepMode::Linear)]
    pub sweep_mode: SweepMode,

    /// Animate dither threshold bias across frames, e.g. "-0.45..0.0" (ink-wash dissolve reveal!).
    #[arg(long, value_name = "START..END", allow_hyphen_values = true)]
    pub sweep_bias: Option<FloatRange>,

    /// Animate contrast across frames, e.g. "-0.2..0.35".
    #[arg(long, value_name = "START..END", allow_hyphen_values = true)]
    pub sweep_contrast: Option<FloatRange>,

    /// Animate brightness across frames, e.g. "-0.7..0.0" (dithered fade-in!).
    #[arg(long, value_name = "START..END", allow_hyphen_values = true)]
    pub sweep_brightness: Option<FloatRange>,

    /// Animate gamma across frames, e.g. "2.2..0.8".
    #[arg(long, value_name = "START..END")]
    pub sweep_gamma: Option<FloatRange>,

    /// Animate dither strength across frames, e.g. "0.0..1.2" (stencil posterize -> full stipple!).
    #[arg(long, value_name = "START..END")]
    pub sweep_strength: Option<FloatRange>,

    /// Animate chunky pixel-size across frames, e.g. "12..1" (mosaic focus-in reveal!).
    #[arg(long, value_name = "START..END")]
    pub sweep_pixel_size: Option<U32Range>,

    /// Classic 90s VGA/Amiga palette color-cycling across frames!
    #[arg(long)]
    pub cycle_palette: bool,

    /// Extract adaptive palettes per frame instead of locking one shared global palette.
    #[arg(long)]
    pub per_frame_palette: bool,

    /// Cap the number of decoded frames when reading an input GIF.
    #[arg(long)]
    pub max_frames: Option<usize>,

    /// Number of columns when exporting --anim-format spritesheet.
    #[arg(long)]
    pub sheet_cols: Option<u32>,

    /// Maximum output file size, e.g. "2MB", "500KB", "1.5MiB", or plain bytes. Shrinks frame
    /// dimensions (keeping frame count, fps, and look) until the GIF / APNG / WebP / spritesheet fits.
    #[arg(long, value_name = "SIZE", value_parser = parse_byte_size)]
    pub max_size: Option<u64>,

    /// Canvas width in pixels for procedural animations (default 480).
    #[arg(long = "canvas-width", default_value_t = 480)]
    pub canvas_width: u32,

    /// Canvas height in pixels for procedural animations (default 360).
    #[arg(long = "canvas-height", default_value_t = 360)]
    pub canvas_height: u32,

    /// Base direction / light angle in degrees for procedural animations (default 135).
    #[arg(long, default_value_t = 135.0, allow_hyphen_values = true)]
    pub angle: f32,

    /// Pattern frequency / scale factor (default 1.0).
    #[arg(long, default_value_t = 1.0)]
    pub frequency: f32,

    /// Comma-separated Oklab gradient stops for procedural animations.
    #[arg(long, value_name = "HEX,HEX,...")]
    pub stops: Option<String>,

    /// Refuse to overwrite an existing output file.
    #[arg(long)]
    pub no_clobber: bool,

    /// Validate animation settings and plan dimensions/frames without writing files.
    #[arg(long)]
    pub dry_run: bool,

    /// Print the fully resolved JSON recipe.
    #[arg(long)]
    pub emit_recipe: bool,

    /// Save the resolved dithering settings as a reusable local preset.
    #[arg(long, value_name = "NAME")]
    pub save_preset: Option<String>,

    /// Target scope when using --save-preset.
    #[arg(long, value_enum, default_value_t = PresetScope::Project)]
    pub preset_scope: PresetScope,

    /// Play the dithered animation live in the fullscreen terminal player (Kitty graphics protocol by default).
    #[arg(long)]
    pub view: bool,

    /// Terminal renderer for --view playback (kitty, braille, or ascii).
    #[arg(long, value_enum, default_value_t = PreviewMode::Kitty)]
    pub view_mode: PreviewMode,

    /// Print first frame inline in scrollback instead of live fullscreen playback when --view is set.
    #[arg(long, conflicts_with = "fullscreen")]
    pub inline: bool,

    /// Open the live fullscreen animation player (implies --view; fullscreen is default for --view).
    #[arg(short = 'F', long, conflicts_with = "inline")]
    pub fullscreen: bool,

    #[command(flatten)]
    pub flags: DitherFlags,
}

#[derive(Debug, Serialize)]
struct AnimateReport {
    status: &'static str,
    dry_run: bool,
    source: String,
    output: Option<String>,
    frames_dir: Option<String>,
    anim_format: &'static str,
    temporal: &'static str,
    frame_count: usize,
    fps: u32,
    total_duration_ms: u64,
    loops: u16,
    preset: Option<String>,
    algorithm: &'static str,
    color_space: ColorSpace,
    palette_name: String,
    palette_colors: Option<Vec<String>>,
    source_dimensions: [u32; 2],
    frame_dimensions: [u32; 2],
    #[serde(skip_serializing_if = "Option::is_none")]
    spritesheet_grid: Option<[u32; 2]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_size_bytes: Option<u64>,
    /// Linear scale applied to frame dimensions to satisfy `--max-size` (1.0 = untouched).
    #[serde(skip_serializing_if = "Option::is_none")]
    size_fit_scale: Option<f32>,
    bytes_written: usize,
    elapsed_ms: u128,
    color_usage: Vec<ColorUsage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    saved_preset_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    recipe: Option<DitherRecipe>,
}

enum AnimateSource {
    Procedural(Pattern),
    Gif(PathBuf),
    StaticBoil(PathBuf),
    Sequence(Vec<PathBuf>),
}

fn resolve_animate_source(args: &AnimateArgs) -> AnimateSource {
    if let Some(pat) = args.pattern {
        return AnimateSource::Procedural(pat);
    }
    if args.inputs.is_empty() {
        return AnimateSource::Procedural(Pattern::Sphere);
    }
    if args.inputs.len() == 1 {
        let first = &args.inputs[0];
        if !first.exists()
            && let Some(name) = first.to_str()
            && let Ok(pat) = Pattern::from_str(name, true)
        {
            return AnimateSource::Procedural(pat);
        }
        if decode_animated_frames(first, None, Some(2))
            .ok()
            .flatten()
            .is_some()
        {
            return AnimateSource::Gif(first.clone());
        }
        return AnimateSource::StaticBoil(first.clone());
    }
    AnimateSource::Sequence(args.inputs.clone())
}

pub(crate) fn handle_animate(args: &AnimateArgs, as_json: bool) -> Result<()> {
    let procedural = matches!(resolve_animate_source(args), AnimateSource::Procedural(_));
    let mut recipe = args.flags.resolve_recipe_for(true, procedural)?;
    let wants_view = args.view || args.fullscreen;
    let wants_fullscreen = wants_view && !args.inline;

    if args.output.is_none() && args.frames_dir.is_none() && !wants_view && !args.dry_run {
        return Err(RecipeError::Validation {
            field: "output",
            reason: "pass -o <out.gif|out.apng|out.webp|sheet.png>, --frames-dir <DIR>, or --view"
                .to_string(),
        }
        .into());
    }
    let fps = args.fps.unwrap_or(15);
    let requested_frames = if let Some(dur) = args.duration {
        if !(0.1..=30.0).contains(&dur) {
            return Err(RecipeError::Validation {
                field: "duration",
                reason: format!("must be in 0.1..=30.0 seconds (got {dur})"),
            }
            .into());
        }
        ((dur * fps as f32).round() as usize).clamp(2, 240)
    } else {
        args.frames.unwrap_or(24)
    };

    if !(2..=240).contains(&requested_frames) {
        return Err(RecipeError::Validation {
            field: "frames",
            reason: format!("must be in 2..=240 (got {requested_frames})"),
        }
        .into());
    }
    if let Some(f) = args.fps
        && !(1..=60).contains(&f)
    {
        return Err(RecipeError::Validation {
            field: "fps",
            reason: format!("must be in 1..=60 (got {f})"),
        }
        .into());
    }

    let anim_format = args.anim_format.unwrap_or_else(|| {
        args.output.as_deref().map_or_else(
            || {
                if args.frames_dir.is_some() {
                    AnimFormat::Frames
                } else {
                    AnimFormat::Gif
                }
            },
            AnimFormat::infer_from_path,
        )
    });

    if args.no_clobber
        && !args.dry_run
        && let Some(path) = &args.output
        && path.exists()
    {
        return Err(RecipeError::OutputExists {
            path: path.display().to_string(),
        }
        .into());
    }

    if args.max_size.is_some() && (args.output.is_none() || anim_format == AnimFormat::Frames) {
        return Err(RecipeError::Validation {
            field: "max-size",
            reason: "needs -o with a .gif, .apng, .webp, or spritesheet .png output".to_string(),
        }
        .into());
    }

    let source = resolve_animate_source(args);
    let has_any_sweep = args.sweep_bias.is_some()
        || args.sweep_contrast.is_some()
        || args.sweep_brightness.is_some()
        || args.sweep_gamma.is_some()
        || args.sweep_strength.is_some()
        || args.sweep_pixel_size.is_some()
        || args.screen_drift.is_some()
        || args.cycle_palette;
    let temporal = args.temporal.unwrap_or(match &source {
        // Static photos default to Risograph film boil unless explicit sweeps/cycling or --temporal are set.
        AnimateSource::StaticBoil(_) if !has_any_sweep => TemporalMode::Boil,
        _ => TemporalMode::Locked,
    });

    let anim_dither_opts = AnimDitherOptions {
        temporal,
        boil_rate: args.boil_rate,
        boil_amount: args.boil_amount,
        screen_drift: args.screen_drift,
        easing: args.easing,
        easing_steps: args.easing_steps,
        sweep_mode: args.sweep_mode,
        sweep_bias: args.sweep_bias,
        sweep_contrast: args.sweep_contrast,
        sweep_brightness: args.sweep_brightness,
        sweep_gamma: args.sweep_gamma,
        sweep_strength: args.sweep_strength,
        sweep_pixel_size: args.sweep_pixel_size,
        cycle_palette: args.cycle_palette,
        per_frame_palette: args.per_frame_palette,
    };

    let source_label = match &source {
        AnimateSource::Procedural(p) => format!("animate:{}", p.id()),
        AnimateSource::Gif(p) | AnimateSource::StaticBoil(p) => p.display().to_string(),
        AnimateSource::Sequence(list) => format!("sequence({} files)", list.len()),
    };

    if args.dry_run {
        let palette_colors = recipe.validate_static_palette()?;
        let (src_w, src_h, planned_frames) = match &source {
            AnimateSource::Procedural(_) => {
                let w = recipe.preprocess.width.unwrap_or(args.canvas_width).max(1);
                let h = recipe
                    .preprocess
                    .height
                    .unwrap_or(args.canvas_height)
                    .max(1);
                recipe.preprocess.width = None;
                recipe.preprocess.height = None;
                (w, h, requested_frames)
            }
            AnimateSource::Gif(p) | AnimateSource::StaticBoil(p) => {
                let (w, h) = image::image_dimensions(p)
                    .with_context(|| format!("reading dimensions of '{}'", p.display()))?;
                (w, h, requested_frames)
            }
            AnimateSource::Sequence(list) => {
                let (w, h) = image::image_dimensions(&list[0])
                    .with_context(|| format!("reading dimensions of '{}'", list[0].display()))?;
                (w, h, list.len())
            }
        };
        let (out_w, out_h) = plan_dimensions(src_w, src_h, &recipe.preprocess);
        let mut total_frames = if args.ping_pong && planned_frames > 2 {
            planned_frames * 2 - 2
        } else {
            planned_frames
        };
        total_frames += args.hold_start + args.hold_end;
        let report = AnimateReport {
            status: "ok",
            dry_run: true,
            source: source_label,
            output: args.output.as_ref().map(|p| p.display().to_string()),
            frames_dir: args.frames_dir.as_ref().map(|p| p.display().to_string()),
            anim_format: anim_format.id(),
            temporal: temporal.id(),
            frame_count: total_frames,
            fps,
            total_duration_ms: (total_frames as u64) * u64::from((1000 / fps).max(16)),
            loops: args.loops,
            preset: recipe.preset.clone(),
            algorithm: recipe.dither.algorithm.id(),
            color_space: recipe.dither.color_space,
            palette_name: palette_spec_label(&recipe.palette),
            palette_colors,
            source_dimensions: [src_w, src_h],
            frame_dimensions: [out_w, out_h],
            spritesheet_grid: None,
            max_size_bytes: args.max_size,
            size_fit_scale: None,
            bytes_written: 0,
            elapsed_ms: 0,
            color_usage: Vec::new(),
            saved_preset_path: None,
            recipe: Some(recipe),
        };
        if as_json {
            println!("{}", serde_json::to_string_pretty(&report)?);
        } else {
            println!(
                "Dry run: {} ({} frames @ {}fps, {}x{}, algo={}, temporal={}, format={})",
                report.source,
                report.frame_count,
                report.fps,
                out_w,
                out_h,
                report.algorithm,
                report.temporal,
                report.anim_format,
            );
        }
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

    let mut raw_frames = load_source_frames(source, args, &mut recipe, requested_frames, fps)?;

    if args.ping_pong {
        apply_ping_pong(&mut raw_frames);
    }
    apply_holds(&mut raw_frames, args.hold_start, args.hold_end);

    let mut anim = dither_animation_frames(&raw_frames, &recipe, &anim_dither_opts)?;
    let encode = |anim: &AnimatedOutput| -> Result<(Vec<u8>, Option<[u32; 2]>)> {
        Ok(match anim_format {
            AnimFormat::Gif => (
                encode_gif(
                    &anim.frames,
                    &anim.palette,
                    args.loops,
                    args.per_frame_palette,
                )?,
                None,
            ),
            AnimFormat::Apng => (
                encode_apng(&anim.frames, u32::from(args.loops), recipe.export.optimize)?,
                None,
            ),
            AnimFormat::Webp => (encode_animated_webp(&anim.frames, args.loops)?, None),
            AnimFormat::Spritesheet => {
                let (sheet, cols, rows) = compose_spritesheet(&anim.frames, args.sheet_cols);
                (
                    encode_image(&sheet, OutputFormat::Png, &recipe.export)?,
                    Some([cols, rows]),
                )
            }
            AnimFormat::Frames => (Vec::new(), None),
        })
    };
    let mut encoded = if args.output.is_some() {
        Some(encode(&anim)?)
    } else {
        None
    };

    let mut size_fit_scale = None;
    if let (Some(limit), Some((bytes, _))) = (args.max_size, encoded.as_ref()) {
        let limit_f = limit as f64;
        let mut size = bytes.len() as u64;
        let base = anim.output_dimensions;
        let mut scale = 1.0f64;
        // File size of dithered frames scales roughly with pixel area, so shrink both sides by
        // sqrt(limit / size) with a small safety margin and re-render until it fits.
        for _ in 0..8 {
            if size <= limit {
                break;
            }
            scale *= ((limit_f / size as f64).sqrt() * 0.95).clamp(0.3, 0.95);
            let fit_w = ((f64::from(base[0]) * scale).round() as u32).max(1);
            let fit_h = ((f64::from(base[1]) * scale).round() as u32).max(1);
            if fit_w.min(fit_h) < 8 {
                break;
            }
            let pre = &mut recipe.preprocess;
            if pre.width.is_none() || pre.height.is_none() {
                pre.fit = FitMode::Stretch;
            }
            pre.width = Some(fit_w);
            pre.height = Some(fit_h);
            pre.max_dim = None;
            pre.scale = None;
            anim = dither_animation_frames(&raw_frames, &recipe, &anim_dither_opts)?;
            let next = encode(&anim)?;
            size = next.0.len() as u64;
            encoded = Some(next);
        }
        if size > limit {
            return Err(RecipeError::Validation {
                field: "max-size",
                reason: format!(
                    "could not fit under {limit} bytes; the smallest attempt was {size} bytes at {}x{}. Reduce --frames or --duration, use an ordered algorithm (e.g. bayer4) with --temporal locked, use fewer palette colors, or raise --max-size",
                    anim.output_dimensions[0], anim.output_dimensions[1]
                ),
            }
            .into());
        }
        size_fit_scale = Some(((scale * 1000.0).round() / 1000.0) as f32);
    }

    let total_duration_ms: u64 = anim.frames.iter().map(|f| u64::from(f.delay_ms)).sum();
    let effective_fps = if total_duration_ms > 0 {
        ((anim.frames.len() as f64 * 1000.0) / total_duration_ms as f64)
            .round()
            .clamp(1.0, 60.0) as u32
    } else {
        fps
    };

    let mut bytes_written = 0usize;
    let mut spritesheet_grid = None;

    if let Some(dir) = &args.frames_dir {
        write_frames_directory(&anim.frames, dir, recipe.export.optimize)?;
    }

    if let Some(out_path) = &args.output {
        if anim_format == AnimFormat::Frames {
            write_frames_directory(&anim.frames, out_path, recipe.export.optimize)?;
        } else if let Some((bytes, grid)) = encoded {
            spritesheet_grid = grid;
            write_output_file(out_path, &bytes)?;
            bytes_written = bytes.len();
        }
    }

    let report = AnimateReport {
        status: "ok",
        dry_run: false,
        source: source_label.clone(),
        output: args.output.as_ref().map(|p| p.display().to_string()),
        frames_dir: args.frames_dir.as_ref().map(|p| p.display().to_string()),
        anim_format: anim_format.id(),
        temporal: temporal.id(),
        frame_count: anim.frames.len(),
        fps: effective_fps,
        total_duration_ms,
        loops: args.loops,
        preset: recipe.preset.clone(),
        algorithm: recipe.dither.algorithm.id(),
        color_space: recipe.dither.color_space,
        palette_name: anim.palette.name.clone(),
        palette_colors: Some(anim.palette.hex_colors()),
        source_dimensions: anim.source_dimensions,
        frame_dimensions: anim.output_dimensions,
        spritesheet_grid,
        max_size_bytes: args.max_size,
        size_fit_scale,
        bytes_written,
        elapsed_ms: anim.elapsed_ms,
        color_usage: anim.histogram,
        saved_preset_path,
        recipe: args.emit_recipe.then_some(recipe.clone()),
    };

    if wants_fullscreen && !as_json && io::stdout().is_terminal() {
        let backend = match args.view_mode {
            PreviewMode::Kitty => ViewerBackend::Kitty,
            PreviewMode::Braille => ViewerBackend::Braille,
            PreviewMode::Ascii => ViewerBackend::Ascii,
        };
        let style = recipe
            .preset
            .as_deref()
            .unwrap_or_else(|| recipe.dither.algorithm.id());
        run_fullscreen_animation_viewer(&FullscreenAnimInput {
            frames: &anim.frames,
            raw_frames: Some(&raw_frames),
            title: format!("{source_label} · {style} · {}", anim.palette.name),
            backend,
        })?;
        if args.output.is_some() || args.frames_dir.is_some() {
            print_animate_summary(&report);
        }
        return Ok(());
    }

    if as_json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else if wants_view && args.output.is_none() && args.frames_dir.is_none() {
        // Non-TTY or --inline fallback: emit first frame via Kitty graphics / Braille / ASCII.
        let first = &anim.frames[0].image;
        match args.view_mode {
            PreviewMode::Kitty => emit_inline_kitty(first, &recipe.export, false)?,
            PreviewMode::Braille => {
                print!("{}", render_braille(first, recipe.export.text_width));
            }
            PreviewMode::Ascii => {
                print!("{}", render_ascii(first, recipe.export.text_width));
            }
        }
    } else {
        print_animate_summary(&report);
        if wants_view && !as_json {
            emit_inline_kitty(&anim.frames[0].image, &recipe.export, false)?;
        }
    }

    Ok(())
}

/// Decode, generate, or load the raw (undithered) frames for an animation source.
fn load_source_frames(
    source: AnimateSource,
    args: &AnimateArgs,
    recipe: &mut DitherRecipe,
    requested_frames: usize,
    fps: u32,
) -> Result<Vec<AnimFrame>> {
    let delay_ms = (1000 / fps).max(16);
    Ok(match source {
        AnimateSource::Procedural(pattern) => {
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

            let gen_opts = GenerateOptions {
                pattern,
                width: gen_w,
                height: gen_h,
                angle: args.angle,
                frequency: args.frequency,
                stops,
                seed: recipe.dither.seed,
                phase: 0.0,
                motion_amplitude: args.motion_amplitude,
            };
            generate_procedural_frames(
                &gen_opts,
                ProceduralMotion {
                    frame_count: requested_frames,
                    fps,
                    cycles: args.cycles,
                    phase_offset: args.phase_offset,
                    easing: args.easing,
                    easing_steps: args.easing_steps,
                    reverse: args.reverse,
                },
            )
        }
        AnimateSource::Gif(path) => {
            let mut decoded = decode_animated_frames(&path, args.fps, args.max_frames)?
                .ok_or_else(|| {
                    anyhow!(
                        "'{}' does not contain multiple animation frames",
                        path.display()
                    )
                })?;
            if args.reverse {
                decoded.reverse();
            }
            decoded
        }
        AnimateSource::StaticBoil(path) => {
            let img = load_input_image(&path)?;
            (0..requested_frames)
                .map(|_| AnimFrame {
                    image: img.clone(),
                    delay_ms,
                })
                .collect()
        }
        AnimateSource::Sequence(paths) => {
            let mut loaded = paths
                .iter()
                .map(|p| {
                    let img = load_input_image(p)?;
                    Ok(AnimFrame {
                        image: img,
                        delay_ms,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            if args.reverse {
                loaded.reverse();
            }
            loaded
        }
    })
}

/// Parse a human byte size such as `2MB`, `500KB`, `1.5MiB`, or `750000` (decimal units are 1000-based).
fn parse_byte_size(raw: &str) -> Result<u64, String> {
    let trimmed = raw.trim();
    let split = trimmed
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(trimmed.len());
    let (num, unit) = trimmed.split_at(split);
    let value: f64 = num
        .parse()
        .map_err(|_| format!("invalid size '{raw}' (expected e.g. 2MB, 500KB, or 750000)"))?;
    let multiplier: f64 = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1.0,
        "k" | "kb" => 1e3,
        "kib" => 1024.0,
        "m" | "mb" => 1e6,
        "mib" => 1024.0 * 1024.0,
        "g" | "gb" => 1e9,
        "gib" => 1024.0 * 1024.0 * 1024.0,
        other => {
            return Err(format!(
                "unknown size unit '{other}' (use B, KB, MB, GB, KiB, MiB, or GiB)"
            ));
        }
    };
    let bytes = (value * multiplier).round();
    if bytes < 1.0 {
        return Err(format!("size '{raw}' must be at least 1 byte"));
    }
    Ok(bytes as u64)
}

fn print_animate_summary(report: &AnimateReport) {
    let dest = report
        .output
        .as_deref()
        .or(report.frames_dir.as_deref())
        .unwrap_or("<viewer>");
    println!(
        "Animated {} -> {} ({} frames @ {}fps, {}x{}, {} bytes, algo={}, palette={}, temporal={}, {}ms)",
        report.source,
        dest,
        report.frame_count,
        report.fps,
        report.frame_dimensions[0],
        report.frame_dimensions[1],
        report.bytes_written,
        report.algorithm,
        report.palette_name,
        report.temporal,
        report.elapsed_ms,
    );
}

#[cfg(test)]
mod tests {
    use super::parse_byte_size;

    #[test]
    fn byte_sizes_parse_with_decimal_and_binary_units() {
        assert_eq!(parse_byte_size("750000"), Ok(750_000));
        assert_eq!(parse_byte_size("500KB"), Ok(500_000));
        assert_eq!(parse_byte_size("2mb"), Ok(2_000_000));
        assert_eq!(parse_byte_size("1.5 MiB"), Ok(1_572_864));
        assert_eq!(parse_byte_size("1KiB"), Ok(1024));
        assert!(parse_byte_size("2XB").is_err());
        assert!(parse_byte_size("MB").is_err());
        assert!(parse_byte_size("0").is_err());
    }
}
