//! Agent-friendly Clap CLI definitions and command handlers.

use std::fs;
use std::io::{self, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use clap::{Args, Parser, Subcommand, ValueEnum};
use image::RgbaImage;
use rayon::prelude::*;
use serde::Serialize;

use crate::animate::{
    AnimDitherOptions, AnimFormat, AnimFrame, AnimatedOutput, Easing, FloatRange, ProceduralMotion,
    ScreenDrift, SweepMode, TemporalMode, U32Range, apply_holds, apply_ping_pong,
    compose_spritesheet, decode_animated_frames, dither_animation_frames, encode_apng, encode_gif,
    generate_procedural_frames, write_frames_directory,
};
use crate::color::{ColorSpace, Rgb};
use crate::compare::{CompareTile, compose_contact_sheet};
use crate::dither::Algorithm;
use crate::generate::{GenerateOptions, Pattern, generate_pattern};
use crate::inspect::inspect_image;
use crate::output::{
    ColorUsage, ExportOptions, KittyOptions, OutputFormat, SvgShape, encode_image,
    encode_png_bytes, render_ascii, render_braille, render_kitty, write_output_file,
};
use crate::palette::{Palette, list_builtin_palettes};
use crate::preprocess::{AlphaMode, FitMode, plan_dimensions};
use crate::preset::{
    OverwritePolicy, PresetScope, PresetSource, SavePresetRequest, delete_local_preset,
    list_all_presets, resolve_preset, save_local_preset,
};
use crate::recipe::{DitherRecipe, PaletteSpec, RecipeError, build_capabilities, run_pipeline};
use crate::update::{UpdateOptions, run_update};
use crate::viewer::{
    FullscreenAnimInput, FullscreenViewerInput, ViewerBackend, run_fullscreen_animation_viewer,
    run_fullscreen_viewer,
};

/// Top-level CLI entrypoint.
#[derive(Debug, Parser)]
#[command(
    name = "dith",
    version,
    about = "Agent-friendly CLI for crafting beautiful dithers, halftones, Risograph prints, procedural art, looping animations, and vector SVGs.",
    long_about = "Dith is a high-craft, deterministic image dithering CLI designed for both AI agents and humans.\n\nQuick discovery for agents:\n  dith capabilities\n  dith list presets --json\n  dith inspect photo.png --json\n  dith generate sphere -o orb.png --preset risograph --json"
)]
pub struct Cli {
    /// Emit stable machine-readable JSON output (and structured JSON errors on failure).
    #[arg(long, global = true)]
    pub json: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Dither an input image (or '-' for stdin) into PNG, WebP, SVG, Kitty graphics, Braille, or ASCII.
    Apply(ApplyArgs),
    /// Procedurally generate gradients, 3D spheres, meshes, waves, or noise and dither in one step.
    Generate(GenerateArgs),
    /// Create animated dithers from procedural loops, GIFs, frame sequences, or boiled photos (GIF, APNG, spritesheet, or live --view).
    #[command(alias = "anim")]
    Animate(Box<AnimateArgs>),
    /// Render a dithered (or raw) fullscreen preview in the terminal via Kitty graphics protocol (or --inline / Braille / ASCII).
    #[command(alias = "view")]
    Preview(PreviewArgs),
    /// Save, list, inspect, or delete persistent local presets (.dith/presets/ or ~/.config/dith/presets/).
    Preset(PresetArgs),
    /// Render a labeled contact sheet (or folder of variants, or fullscreen --view) comparing algorithms, presets, or palettes.
    Compare(CompareArgs),
    /// Dither multiple input images in parallel into an output directory.
    Batch(BatchArgs),
    /// Analyze an image (dimensions, luminance, dominant colors) and recommend dithering commands.
    Inspect(InspectArgs),
    /// List available algorithms, palettes, presets, generators, or formats.
    List(ListArgs),
    /// Update dith to the latest GitHub Release binary (and refresh agent skills).
    Update(UpdateArgs),
    /// Dump a complete self-describing JSON manifest of all commands, options, ranges, presets, and palettes.
    #[command(alias = "schema")]
    Capabilities,
}

/// Arguments for `dith update`.
#[derive(Debug, Args)]
pub struct UpdateArgs {
    /// Check if a newer release is available without modifying the installed binary.
    #[arg(long)]
    pub check: bool,

    /// Install a specific release tag (e.g. "v0.2.0") instead of the latest release.
    #[arg(long, value_name = "TAG")]
    pub version: Option<String>,

    /// Reinstall even when already running the target release version.
    #[arg(long)]
    pub force: bool,

    /// Skip refreshing installed agent skills after updating.
    #[arg(long)]
    pub no_skills: bool,
}

/// Shared recipe flags that compose cleanly: `--recipe` -> `--preset` -> explicit CLI overrides.
#[derive(Debug, Clone, Args, Default)]
pub struct DitherFlags {
    /// Load base settings from a JSON recipe file (or '-' to read JSON from stdin).
    #[arg(long, value_name = "PATH|-")]
    pub recipe: Option<PathBuf>,

    /// Start from a curated artistic preset (individual flags override preset defaults).
    #[arg(short = 'P', long, value_name = "PRESET")]
    pub preset: Option<String>,

    /// Dithering algorithm (e.g. floyd-steinberg, atkinson, ostromoukhov, bayer4, halftone, blue-noise, riemersma).
    #[arg(short = 'a', long, value_enum)]
    pub algorithm: Option<Algorithm>,

    /// Built-in palette slug (e.g. bw, newsprint, risograph-blue, gameboy, pico8, gray6).
    #[arg(short = 'p', long, value_name = "NAME")]
    pub palette: Option<String>,

    /// Custom comma-separated hex palette (e.g. "#111111,#f5efe6" or "#0f172a,#2563eb,#f8fafc").
    #[arg(long, value_name = "HEX,HEX,...")]
    pub colors: Option<String>,

    /// Load custom palette from a .hex, .txt, .gpl (GIMP), or .json file.
    #[arg(long, value_name = "PATH")]
    pub palette_file: Option<PathBuf>,

    /// Adaptively extract N perceptual colors from the image using Oklab k-means++ (2..=64).
    #[arg(long, value_name = "N")]
    pub auto_palette: Option<usize>,

    /// Color space for error diffusion and nearest-color matching.
    #[arg(long, value_enum)]
    pub color_space: Option<ColorSpace>,

    /// Dither / diffusion strength multiplier (0.0..=1.5, default 1.0).
    #[arg(long)]
    pub strength: Option<f32>,

    /// Enable serpentine scanline alternation for error diffusion (default true).
    #[arg(long, conflicts_with = "no_serpentine")]
    pub serpentine: bool,

    /// Disable serpentine scanning (classic left-to-right diffusion).
    #[arg(long, conflicts_with = "serpentine")]
    pub no_serpentine: bool,

    /// Midtone threshold bias (-0.5..=0.5, negative = darker, positive = lighter).
    #[arg(long, allow_hyphen_values = true)]
    pub bias: Option<f32>,

    /// Deterministic jitter amplitude (0.0..=0.5) to soften grid patterns or break banding.
    #[arg(long)]
    pub noise: Option<f32>,

    /// Scale multiplier for ordered/halftone/blue-noise screen cells (1..=32).
    #[arg(long)]
    pub pattern_scale: Option<u32>,

    /// Deterministic PRNG seed (for blue-noise/random/ign/noise/auto-palette).
    #[arg(long)]
    pub seed: Option<u64>,

    /// Chunky pixel block size (1..=64: downscales by N, dithers, then nearest-upscales).
    #[arg(long)]
    pub pixel_size: Option<u32>,

    /// Target output width in pixels.
    #[arg(long)]
    pub width: Option<u32>,

    /// Target output height in pixels.
    #[arg(long)]
    pub height: Option<u32>,

    /// Cap the longest side to at most N pixels (preserving aspect ratio).
    #[arg(long)]
    pub max_dim: Option<u32>,

    /// Uniform resize multiplier (e.g. 0.5 or 2.0).
    #[arg(long)]
    pub scale: Option<f32>,

    /// Fit mode when both --width and --height are set.
    #[arg(long, value_enum)]
    pub fit: Option<FitMode>,

    /// Brightness adjustment before dithering (-1.0..=1.0).
    #[arg(long, allow_hyphen_values = true)]
    pub brightness: Option<f32>,

    /// Contrast adjustment before dithering (-1.0..=1.0).
    #[arg(long, allow_hyphen_values = true)]
    pub contrast: Option<f32>,

    /// Gamma exponent before dithering (0.1..=5.0, <1 brightens midtones).
    #[arg(long)]
    pub gamma: Option<f32>,

    /// Saturation multiplier (0.0 = grayscale, 1.0 = neutral, 2.0 = vivid).
    #[arg(long)]
    pub saturation: Option<f32>,

    /// Unsharp mask edge sharpening before dithering (0.0..=5.0).
    #[arg(long)]
    pub sharpen: Option<f32>,

    /// Pre-dither Gaussian blur sigma (0.0..=20.0).
    #[arg(long)]
    pub blur: Option<f32>,

    /// Auto-stretch luminance histogram (1st..99th percentile) before dithering.
    #[arg(long, conflicts_with = "no_auto_levels")]
    pub auto_levels: bool,

    /// Disable auto-levels (useful to override a preset).
    #[arg(long, conflicts_with = "auto_levels")]
    pub no_auto_levels: bool,

    /// Convert to perceptual grayscale before dithering.
    #[arg(long, conflicts_with = "no_grayscale")]
    pub grayscale: bool,

    /// Keep full color (useful to override a monochrome preset).
    #[arg(long, conflicts_with = "grayscale")]
    pub no_grayscale: bool,

    /// Invert tonal values before dithering.
    #[arg(long)]
    pub invert: bool,

    /// How to handle transparency (preserve, flatten, threshold, dither).
    #[arg(long, value_enum)]
    pub alpha_mode: Option<AlphaMode>,

    /// Background hex color used when --alpha-mode flatten is set (default #ffffff).
    #[arg(long, value_name = "HEX")]
    pub background: Option<Rgb>,

    /// Output format (auto-inferred from output extension if omitted).
    #[arg(short = 'f', long, value_enum)]
    pub format: Option<OutputFormat>,

    /// Vector shape primitive when exporting SVG (rect, circle, dot).
    #[arg(long, value_enum)]
    pub svg_shape: Option<SvgShape>,

    /// Omit the dominant background color in SVG output for transparent overlays.
    #[arg(long)]
    pub svg_skip_bg: bool,

    /// Maximum character columns when exporting braille or ascii.
    #[arg(long)]
    pub text_width: Option<u32>,

    /// Display width in terminal columns when using --view or --format kitty.
    #[arg(long)]
    pub view_cols: Option<u32>,

    /// Display height in terminal rows when using --view or --format kitty.
    #[arg(long)]
    pub view_rows: Option<u32>,
}

impl DitherFlags {
    /// Resolve layered configuration: JSON recipe -> preset -> explicit CLI flags.
    ///
    /// # Errors
    /// Returns a [`RecipeError`] if the recipe JSON, preset, or custom colors are invalid.
    pub fn resolve_recipe(&self, allow_stdin_recipe: bool) -> Result<DitherRecipe, RecipeError> {
        self.resolve_recipe_for(allow_stdin_recipe, false)
    }

    /// Like [`Self::resolve_recipe`], but for `procedural` sources a preset's photo preparation
    /// (auto-levels, contrast, sharpen) is dropped unless those flags are passed explicitly.
    ///
    /// # Errors
    /// Returns a [`RecipeError`] if the recipe JSON, preset, or custom colors are invalid.
    pub fn resolve_recipe_for(
        &self,
        allow_stdin_recipe: bool,
        procedural: bool,
    ) -> Result<DitherRecipe, RecipeError> {
        let mut recipe = if let Some(path) = &self.recipe {
            let raw = if path.as_os_str() == "-" {
                if !allow_stdin_recipe {
                    return Err(RecipeError::Validation {
                        field: "recipe",
                        reason: "cannot read both image input and --recipe from stdin ('-')"
                            .to_string(),
                    });
                }
                let mut buf = String::new();
                io::stdin()
                    .read_to_string(&mut buf)
                    .map_err(|e| RecipeError::InvalidRecipeJson(e.to_string()))?;
                buf
            } else {
                fs::read_to_string(path).map_err(|e| {
                    RecipeError::InvalidRecipeJson(format!("reading '{}': {e}", path.display()))
                })?
            };
            serde_json::from_str::<DitherRecipe>(&raw)
                .map_err(|e| RecipeError::InvalidRecipeJson(e.to_string()))?
        } else {
            DitherRecipe::default()
        };

        if let Some(preset_name) = &self.preset {
            recipe = DitherRecipe::from_preset(preset_name)?;
            if procedural {
                recipe.strip_photo_prep();
            }
        }

        // Apply explicit palette overrides (priority: --colors > --palette-file > --auto-palette > --palette).
        if let Some(hex_spec) = &self.colors {
            let parsed = Palette::from_hex_list(hex_spec)?;
            recipe.palette = PaletteSpec::Colors(parsed.colors().to_vec());
        } else if let Some(pfile) = &self.palette_file {
            recipe.palette = PaletteSpec::File(pfile.clone());
        } else if let Some(n) = self.auto_palette {
            recipe.palette = PaletteSpec::Adaptive(n);
        } else if let Some(name) = &self.palette {
            recipe.palette = PaletteSpec::Builtin(name.clone());
        }

        // Apply explicit dither overrides.
        if let Some(a) = self.algorithm {
            recipe.dither.algorithm = a;
        }
        if let Some(cs) = self.color_space {
            recipe.dither.color_space = cs;
        }
        if let Some(s) = self.strength {
            recipe.dither.strength = s;
        }
        if self.serpentine {
            recipe.dither.serpentine = true;
        } else if self.no_serpentine {
            recipe.dither.serpentine = false;
        }
        if let Some(b) = self.bias {
            recipe.dither.bias = b;
        }
        if let Some(n) = self.noise {
            recipe.dither.noise = n;
        }
        if let Some(ps) = self.pattern_scale {
            recipe.dither.pattern_scale = ps;
        }
        if let Some(seed) = self.seed {
            recipe.dither.seed = seed;
        }

        // Apply explicit preprocess overrides.
        if let Some(ps) = self.pixel_size {
            recipe.preprocess.pixel_size = ps;
        }
        if let Some(w) = self.width {
            recipe.preprocess.width = Some(w);
        }
        if let Some(h) = self.height {
            recipe.preprocess.height = Some(h);
        }
        if let Some(md) = self.max_dim {
            recipe.preprocess.max_dim = Some(md);
        }
        if let Some(sc) = self.scale {
            recipe.preprocess.scale = Some(sc);
        }
        if let Some(fit) = self.fit {
            recipe.preprocess.fit = fit;
        }
        if let Some(b) = self.brightness {
            recipe.preprocess.brightness = b;
        }
        if let Some(c) = self.contrast {
            recipe.preprocess.contrast = c;
        }
        if let Some(g) = self.gamma {
            recipe.preprocess.gamma = g;
        }
        if let Some(s) = self.saturation {
            recipe.preprocess.saturation = s;
        }
        if let Some(sh) = self.sharpen {
            recipe.preprocess.sharpen = sh;
        }
        if let Some(bl) = self.blur {
            recipe.preprocess.blur = bl;
        }
        if self.auto_levels {
            recipe.preprocess.auto_levels = true;
        } else if self.no_auto_levels {
            recipe.preprocess.auto_levels = false;
        }
        if self.grayscale {
            recipe.preprocess.grayscale = true;
        } else if self.no_grayscale {
            recipe.preprocess.grayscale = false;
        }
        if self.invert {
            recipe.preprocess.invert = true;
        }
        if let Some(am) = self.alpha_mode {
            recipe.preprocess.alpha_mode = am;
        }
        if let Some(bg) = self.background {
            recipe.preprocess.background = bg;
        }

        // Export overrides.
        if let Some(fmt) = self.format {
            recipe.export.format = Some(fmt);
        }
        if let Some(shape) = self.svg_shape {
            recipe.export.svg_shape = shape;
        }
        if self.svg_skip_bg {
            recipe.export.svg_skip_bg = true;
        }
        if let Some(tw) = self.text_width {
            recipe.export.text_width = Some(tw);
        }
        if let Some(vc) = self.view_cols {
            recipe.export.view_cols = Some(vc);
        }
        if let Some(vr) = self.view_rows {
            recipe.export.view_rows = Some(vr);
        }

        recipe.validate()?;
        Ok(recipe)
    }
}

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
    pub flags: DitherFlags,
}

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

#[derive(Debug, Args)]
pub struct AnimateArgs {
    /// Input GIF, static photo (for animated film-boil), image sequence, or procedural pattern name (e.g. "sphere", "waves", "mesh").
    pub inputs: Vec<PathBuf>,

    /// Explicitly generate a seamless procedural loop instead of reading input files.
    #[arg(long, value_enum)]
    pub pattern: Option<Pattern>,

    /// Output path (.gif, .apng, or .png sprite sheet). Optional when using --view or --frames-dir.
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

    /// Loop count for GIF / APNG (0 = infinite loop, default 0).
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
    /// dimensions (keeping frame count, fps, and look) until the GIF / APNG / spritesheet fits.
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

/// Terminal renderer mode for `dith preview`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PreviewMode {
    /// Pixel-exact inline graphics via the Kitty graphics protocol (Kitty, Ghostty, `WezTerm`).
    #[default]
    Kitty,
    /// High-density Unicode Braille text preview (works in any UTF-8 terminal).
    Braille,
    /// Classic shaded ASCII text preview.
    Ascii,
}

impl PreviewMode {
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Kitty => "kitty",
            Self::Braille => "braille",
            Self::Ascii => "ascii",
        }
    }
}

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

#[derive(Debug, Args)]
pub struct PresetArgs {
    #[command(subcommand)]
    pub action: PresetCommand,
}

#[derive(Debug, Subcommand)]
pub enum PresetCommand {
    /// Save a custom dithering configuration as a reusable local preset.
    Save(Box<PresetSaveArgs>),
    /// List saved local presets (and optionally built-in presets).
    List(PresetListArgs),
    /// Show the full JSON definition and recipe of any local or built-in preset.
    Show(PresetShowArgs),
    /// Delete a saved local preset file.
    Delete(PresetDeleteArgs),
}

#[derive(Debug, Args)]
pub struct PresetSaveArgs {
    /// Preset slug name (e.g. "brand-hero" or "zine-v2"; lowercase letters, digits, hyphens).
    pub name: String,

    /// Human-readable display title (auto-generated from slug if omitted).
    #[arg(long)]
    pub title: Option<String>,

    /// Short artistic description of what this preset is for.
    #[arg(long)]
    pub description: Option<String>,

    /// Comma-separated discovery tags (e.g. "brand,hero,risograph").
    #[arg(long, value_delimiter = ',')]
    pub tags: Vec<String>,

    /// Where to store the preset (project = .dith/presets, user = ~/.config/dith/presets).
    #[arg(long, value_enum, default_value_t = PresetScope::Project)]
    pub scope: PresetScope,

    /// Explicit directory path to store the preset JSON file (overrides --scope).
    #[arg(long, value_name = "DIR")]
    pub dir: Option<PathBuf>,

    /// Refuse to overwrite an existing saved preset with the same name.
    #[arg(long)]
    pub no_clobber: bool,

    #[command(flatten)]
    pub flags: DitherFlags,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum PresetSourceFilter {
    /// Both saved local presets and built-in presets.
    #[default]
    All,
    /// Only locally saved presets (env, project, user).
    Local,
    /// Only project-local presets (.dith/presets/).
    Project,
    /// Only user-global presets (~/.config/dith/presets/).
    User,
    /// Only built-in presets.
    Builtin,
}

#[derive(Debug, Args)]
pub struct PresetListArgs {
    /// Filter presets by storage origin.
    #[arg(long, value_enum, default_value_t = PresetSourceFilter::All)]
    pub source: PresetSourceFilter,

    /// Filter presets by tag substring (case-insensitive).
    #[arg(long)]
    pub tag: Option<String>,
}

#[derive(Debug, Args)]
pub struct PresetShowArgs {
    /// Preset slug to inspect (checks local presets first, then built-ins).
    pub name: String,
}

#[derive(Debug, Args)]
pub struct PresetDeleteArgs {
    /// Saved local preset slug to delete.
    pub name: String,

    /// Storage scope to delete from (project = .dith/presets, user = ~/.config/dith/presets).
    #[arg(long, value_enum, default_value_t = PresetScope::Project)]
    pub scope: PresetScope,

    /// Explicit preset directory path (overrides --scope).
    #[arg(long, value_name = "DIR")]
    pub dir: Option<PathBuf>,
}

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

#[derive(Debug, Args)]
pub struct InspectArgs {
    /// Path to the image file to analyze.
    pub input: PathBuf,

    /// Number of dominant colors to extract via Oklab k-means++ (2..=16, default 6).
    #[arg(long, default_value_t = 6)]
    pub colors: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ListKind {
    Algorithms,
    Palettes,
    Presets,
    Generators,
    Formats,
}

#[derive(Debug, Args)]
pub struct ListArgs {
    /// Catalog section to inspect.
    #[arg(value_enum)]
    pub kind: ListKind,

    /// Filter entries by tag substring (case-insensitive).
    #[arg(long)]
    pub tag: Option<String>,

    /// Filter entries by category (e.g. error-diffusion, ordered, stochastic, print, retro).
    #[arg(long)]
    pub category: Option<String>,
}

/// Structured result emitted for `apply` and `generate`.
#[derive(Debug, Serialize)]
struct RenderReport {
    status: &'static str,
    dry_run: bool,
    source: String,
    output: Option<String>,
    format: &'static str,
    preset: Option<String>,
    algorithm: &'static str,
    color_space: ColorSpace,
    palette_name: String,
    palette_colors: Option<Vec<String>>,
    source_dimensions: [u32; 2],
    working_dimensions: [u32; 2],
    output_dimensions: [u32; 2],
    bytes_written: usize,
    elapsed_ms: u128,
    color_usage: Vec<ColorUsage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    preview_braille: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    text_output: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    saved_preset_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    recipe: Option<DitherRecipe>,
}

/// Execute the parsed CLI command.
///
/// # Errors
/// Returns an error if validation, I/O, or image processing fails.
pub fn dispatch(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Capabilities => handle_capabilities(),
        Command::List(args) => handle_list(&args, cli.json),
        Command::Inspect(args) => handle_inspect(&args, cli.json),
        Command::Apply(args) => handle_apply(&args, cli.json),
        Command::Generate(args) => handle_generate(&args, cli.json),
        Command::Animate(args) => handle_animate(&args, cli.json),
        Command::Preview(args) => handle_preview(&args, cli.json),
        Command::Preset(args) => handle_preset(&args, cli.json),
        Command::Compare(args) => handle_compare(&args, cli.json),
        Command::Batch(args) => handle_batch(&args, cli.json),
        Command::Update(args) => handle_update(&args, cli.json),
    }
}

fn handle_update(args: &UpdateArgs, as_json: bool) -> Result<()> {
    let report = run_update(&UpdateOptions {
        check_only: args.check,
        version: args.version.clone(),
        force: args.force,
        no_skills: args.no_skills,
    })?;

    if as_json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    if report.updated {
        println!(
            "Updated dith {} -> {} ({}) at {}",
            report.current_version, report.latest_version, report.target, report.install_path
        );
        if report.skills_updated {
            println!(
                "Refreshed dith agent skill for {}",
                report.skills_agents.join(", ")
            );
        }
    } else if report.update_available {
        println!(
            "Update available: dith {} -> {} ({})",
            report.current_version, report.latest_version, report.release_url
        );
        println!("Run `dith update` to install.");
    } else {
        println!(
            "dith {} is already up to date (latest: {})",
            report.current_version, report.latest_version
        );
    }
    Ok(())
}

fn handle_capabilities() -> Result<()> {
    let manifest = build_capabilities();
    println!("{}", serde_json::to_string_pretty(&manifest)?);
    Ok(())
}

fn handle_list(args: &ListArgs, as_json: bool) -> Result<()> {
    let tag_filter = args.tag.as_deref().map(str::to_ascii_lowercase);
    let cat_filter = args.category.as_deref().map(str::to_ascii_lowercase);

    match args.kind {
        ListKind::Algorithms => {
            let items: Vec<_> = Algorithm::ALL
                .iter()
                .map(|a| a.info())
                .filter(|a| {
                    cat_filter
                        .as_deref()
                        .is_none_or(|c| a.category.to_ascii_lowercase().contains(c))
                })
                .filter(|a| {
                    tag_filter.as_deref().is_none_or(|t| {
                        a.description.to_ascii_lowercase().contains(t)
                            || a.best_for.to_ascii_lowercase().contains(t)
                            || a.category.to_ascii_lowercase().contains(t)
                    })
                })
                .collect();
            if as_json {
                println!("{}", serde_json::to_string_pretty(&items)?);
            } else {
                for a in items {
                    println!(
                        "{:<20} [{:<15}]  {} — Best for: {}",
                        a.id, a.category, a.description, a.best_for
                    );
                }
            }
        }
        ListKind::Palettes => {
            let items: Vec<_> = list_builtin_palettes()
                .into_iter()
                .filter(|p| {
                    cat_filter
                        .as_deref()
                        .is_none_or(|c| p.category.to_ascii_lowercase().contains(c))
                })
                .filter(|p| {
                    tag_filter.as_deref().is_none_or(|t| {
                        p.tags.iter().any(|tg| tg.to_ascii_lowercase().contains(t))
                            || p.id.contains(t)
                    })
                })
                .collect();
            if as_json {
                println!("{}", serde_json::to_string_pretty(&items)?);
            } else {
                for p in items {
                    println!(
                        "{:<18} [{:<10}] ({} colors)  {}",
                        p.id,
                        p.category,
                        p.colors.len(),
                        p.description
                    );
                }
            }
        }
        ListKind::Presets => {
            let items: Vec<_> = list_all_presets()
                .into_iter()
                .filter(|p| {
                    tag_filter
                        .as_deref()
                        .is_none_or(|t| p.tags.iter().any(|tg| tg.to_ascii_lowercase().contains(t)))
                })
                .collect();
            if as_json {
                println!("{}", serde_json::to_string_pretty(&items)?);
            } else {
                for p in items {
                    println!(
                        "{:<18} [{:<7}] [algo: {:<16}]  {}",
                        p.id,
                        p.source.id(),
                        p.algorithm,
                        p.description
                    );
                }
            }
        }
        ListKind::Generators => {
            let items: Vec<_> = Pattern::ALL.iter().map(|p| p.info()).collect();
            if as_json {
                println!("{}", serde_json::to_string_pretty(&items)?);
            } else {
                for g in items {
                    println!("{:<18} {} — Best for: {}", g.id, g.description, g.best_for);
                }
            }
        }
        ListKind::Formats => {
            let caps = build_capabilities();
            if as_json {
                println!("{}", serde_json::to_string_pretty(&caps.output_formats)?);
            } else {
                for f in caps.output_formats {
                    println!("{:<10} {}", f.id, f.description);
                }
            }
        }
    }
    Ok(())
}

fn handle_inspect(args: &InspectArgs, as_json: bool) -> Result<()> {
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

fn handle_apply(args: &ApplyArgs, as_json: bool) -> Result<()> {
    let reading_stdin = args.input.as_os_str() == "-";
    let recipe = args.flags.resolve_recipe(!reading_stdin)?;
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
            image::image_dimensions(&args.input)
                .with_context(|| format!("reading dimensions of '{}'", args.input.display()))?
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

    let src_img = load_input_image(&args.input)?;
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

    // When --view --inline is requested alongside a saved file (-o <path>), render inline via Kitty graphics.
    if wants_view && args.output.is_some() && format != OutputFormat::Kitty {
        emit_inline_kitty(&output_data.image, &recipe.export, as_json)?;
    }
    Ok(())
}

fn handle_generate(args: &GenerateArgs, as_json: bool) -> Result<()> {
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

fn handle_animate(args: &AnimateArgs, as_json: bool) -> Result<()> {
    let procedural = matches!(resolve_animate_source(args), AnimateSource::Procedural(_));
    let mut recipe = args.flags.resolve_recipe_for(true, procedural)?;
    let wants_view = args.view || args.fullscreen;
    let wants_fullscreen = wants_view && !args.inline;

    if args.output.is_none() && args.frames_dir.is_none() && !wants_view && !args.dry_run {
        return Err(RecipeError::Validation {
            field: "output",
            reason: "pass -o <out.gif|out.apng|sheet.png>, --frames-dir <DIR>, or --view"
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
            reason: "needs -o with a .gif, .apng, or spritesheet .png output".to_string(),
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

    let delay_ms = (1000 / fps).max(16);
    let mut raw_frames: Vec<AnimFrame> = match source {
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
    };

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
            AnimFormat::Apng => (encode_apng(&anim.frames, u32::from(args.loops))?, None),
            AnimFormat::Spritesheet => {
                let (sheet, cols, rows) = compose_spritesheet(&anim.frames, args.sheet_cols);
                (encode_png_bytes(&sheet)?, Some([cols, rows]))
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
        write_frames_directory(&anim.frames, dir)?;
    }

    if let Some(out_path) = &args.output {
        if anim_format == AnimFormat::Frames {
            write_frames_directory(&anim.frames, out_path)?;
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

fn handle_preview(args: &PreviewArgs, as_json: bool) -> Result<()> {
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

#[derive(Debug, Serialize)]
struct PresetSaveReport {
    status: &'static str,
    id: String,
    name: String,
    description: String,
    tags: Vec<String>,
    source: PresetSource,
    path: String,
    recipe: DitherRecipe,
}

#[derive(Debug, Serialize)]
struct PresetDeleteReport {
    status: &'static str,
    id: String,
    deleted_path: String,
}

fn handle_preset(args: &PresetArgs, as_json: bool) -> Result<()> {
    match &args.action {
        PresetCommand::Save(save_args) => {
            let recipe = save_args.flags.resolve_recipe(true)?;
            let _ = recipe.validate_static_palette()?;
            let (saved, path, source) = save_local_preset(SavePresetRequest {
                slug: &save_args.name,
                title: save_args.title.as_deref(),
                description: save_args.description.as_deref(),
                tags: save_args.tags.clone(),
                recipe,
                scope: save_args.scope,
                custom_dir: save_args.dir.as_deref(),
                overwrite: if save_args.no_clobber {
                    OverwritePolicy::NoClobber
                } else {
                    OverwritePolicy::Replace
                },
            })
            .map_err(RecipeError::from)?;

            let report = PresetSaveReport {
                status: "ok",
                id: saved.id,
                name: saved.name,
                description: saved.description,
                tags: saved.tags,
                source,
                path: path.display().to_string(),
                recipe: saved.recipe,
            };

            if as_json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!(
                    "Saved local preset '{}' [{}] -> {}",
                    report.id,
                    report.source.id(),
                    report.path
                );
                println!(
                    "Use it anywhere with: dith apply <input> -o <out> --preset {}",
                    report.id
                );
            }
        }
        PresetCommand::List(list_args) => {
            let tag_filter = list_args.tag.as_deref().map(str::to_ascii_lowercase);
            let items: Vec<_> = list_all_presets()
                .into_iter()
                .filter(|p| match list_args.source {
                    PresetSourceFilter::All => true,
                    PresetSourceFilter::Local => p.source != PresetSource::Builtin,
                    PresetSourceFilter::Project => {
                        matches!(p.source, PresetSource::Project | PresetSource::Env)
                    }
                    PresetSourceFilter::User => p.source == PresetSource::User,
                    PresetSourceFilter::Builtin => p.source == PresetSource::Builtin,
                })
                .filter(|p| {
                    tag_filter
                        .as_deref()
                        .is_none_or(|t| p.tags.iter().any(|tg| tg.to_ascii_lowercase().contains(t)))
                })
                .collect();

            if as_json {
                println!("{}", serde_json::to_string_pretty(&items)?);
            } else if items.is_empty() {
                println!("No presets matched.");
            } else {
                for p in items {
                    let loc = p.path.as_deref().unwrap_or("built-in");
                    println!(
                        "{:<18} [{:<7}] [algo: {:<16}]  {} ({})",
                        p.id,
                        p.source.id(),
                        p.algorithm,
                        p.description,
                        loc
                    );
                }
            }
        }
        PresetCommand::Show(show_args) => {
            let resolved = resolve_preset(&show_args.name).map_err(RecipeError::from)?;
            if as_json {
                println!("{}", serde_json::to_string_pretty(&resolved)?);
            } else {
                println!(
                    "Preset: {} ({}) [{}]",
                    resolved.id,
                    resolved.name,
                    resolved.source.id()
                );
                if let Some(path) = &resolved.path {
                    println!("Path:   {path}");
                }
                println!("Desc:   {}", resolved.description);
                if !resolved.tags.is_empty() {
                    println!("Tags:   {}", resolved.tags.join(", "));
                }
                println!("\n{}", serde_json::to_string_pretty(&resolved.recipe)?);
            }
        }
        PresetCommand::Delete(del_args) => {
            let (id, path) =
                delete_local_preset(&del_args.name, del_args.scope, del_args.dir.as_deref())
                    .map_err(RecipeError::from)?;
            let report = PresetDeleteReport {
                status: "ok",
                id,
                deleted_path: path.display().to_string(),
            };
            if as_json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!(
                    "Deleted local preset '{}' ({})",
                    report.id, report.deleted_path
                );
            }
        }
    }
    Ok(())
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

fn handle_compare(args: &CompareArgs, as_json: bool) -> Result<()> {
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
    let rendered: Result<Vec<(String, DitherRecipe, crate::recipe::PipelineOutput)>, RecipeError> =
        variant_specs
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
            let bytes = encode_image(&sheet, fmt, &ExportOptions::default())?;
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

fn handle_batch(args: &BatchArgs, as_json: bool) -> Result<()> {
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

fn load_input_image(path: &Path) -> Result<RgbaImage> {
    if path.as_os_str() == "-" {
        let mut buf = Vec::new();
        io::stdin()
            .read_to_end(&mut buf)
            .context("reading input image bytes from stdin")?;
        let img = image::load_from_memory(&buf).context("decoding input image from stdin")?;
        Ok(img.to_rgba8())
    } else {
        let img = image::open(path)
            .with_context(|| format!("opening input image '{}'", path.display()))?;
        Ok(img.to_rgba8())
    }
}

fn resolve_output_format(
    output: Option<&Path>,
    explicit: Option<OutputFormat>,
    view: bool,
) -> Result<OutputFormat, RecipeError> {
    if let Some(fmt) = explicit {
        return Ok(fmt);
    }
    match output {
        Some(path) if path.as_os_str() != "-" => Ok(OutputFormat::infer_from_path(path)),
        Some(_) => Ok(OutputFormat::Png),
        None if view => Ok(OutputFormat::Kitty),
        None => Err(RecipeError::Validation {
            field: "output",
            reason: "pass -o <path>, -o - (stdout), --view, or --format kitty|braille|ascii|svg"
                .to_string(),
        }),
    }
}

fn emit_inline_kitty(img: &RgbaImage, export: &ExportOptions, as_json: bool) -> Result<()> {
    if as_json && !io::stderr().is_terminal() {
        return Ok(());
    }
    let kitty_opts = KittyOptions {
        cols: export.view_cols.or(export.text_width),
        rows: export.view_rows,
        tmux_passthrough: if as_json {
            std::env::var_os("TMUX").is_some() && io::stderr().is_terminal()
        } else {
            KittyOptions::should_use_tmux_passthrough()
        },
        trailing_newline: true,
        no_cursor_move: false,
        image_id: None,
    };
    let seq = render_kitty(img, kitty_opts)?;
    if as_json {
        let mut stderr = io::stderr().lock();
        stderr
            .write_all(seq.as_bytes())
            .context("writing Kitty graphics preview to stderr")?;
        stderr.flush().context("flushing stderr")?;
    } else {
        let mut stdout = io::stdout().lock();
        stdout
            .write_all(seq.as_bytes())
            .context("writing Kitty graphics preview to stdout")?;
        stdout.flush().context("flushing stdout")?;
    }
    Ok(())
}

fn ensure_output_destination(
    output: Option<&Path>,
    format: OutputFormat,
    no_clobber: bool,
    dry_run: bool,
) -> Result<(), RecipeError> {
    if output.is_none() && !format.is_text() {
        return Err(RecipeError::Validation {
            field: "output",
            reason: format!(
                "binary format '{}' requires -o <path> (or -o - for stdout)",
                format.id()
            ),
        });
    }
    if no_clobber
        && !dry_run
        && let Some(path) = output
        && path.as_os_str() != "-"
        && path.exists()
    {
        return Err(RecipeError::OutputExists {
            path: path.display().to_string(),
        });
    }
    Ok(())
}

fn write_destination(
    output: Option<&Path>,
    bytes: &[u8],
    format: OutputFormat,
    as_json: bool,
) -> Result<usize> {
    match output {
        Some(path) if path.as_os_str() == "-" => {
            if !as_json {
                let mut stdout = io::stdout().lock();
                stdout
                    .write_all(bytes)
                    .context("writing output to stdout")?;
                stdout.flush().context("flushing stdout")?;
            }
            Ok(bytes.len())
        }
        Some(path) => {
            write_output_file(path, bytes)?;
            Ok(bytes.len())
        }
        None => {
            if format.is_text() && !as_json {
                let mut stdout = io::stdout().lock();
                stdout
                    .write_all(bytes)
                    .context("writing text output to stdout")?;
                stdout.flush().context("flushing stdout")?;
            }
            Ok(bytes.len())
        }
    }
}

fn emit_render_report(
    report: &RenderReport,
    as_json: bool,
    wrote_stdout_payload: bool,
) -> Result<()> {
    if as_json {
        println!("{}", serde_json::to_string_pretty(report)?);
        return Ok(());
    }
    if wrote_stdout_payload {
        return Ok(());
    }
    if report.dry_run {
        println!(
            "Dry run: {} -> {} ({}x{}, algo={}, palette={}, format={})",
            report.source,
            report.output.as_deref().unwrap_or("<stdout>"),
            report.output_dimensions[0],
            report.output_dimensions[1],
            report.algorithm,
            report.palette_name,
            report.format,
        );
    } else {
        println!(
            "Wrote {} ({}x{}, {} bytes, algo={}, palette={}, {}ms)",
            report.output.as_deref().unwrap_or("<stdout>"),
            report.output_dimensions[0],
            report.output_dimensions[1],
            report.bytes_written,
            report.algorithm,
            report.palette_name,
            report.elapsed_ms,
        );
    }
    if let Some(preview) = &report.preview_braille {
        println!("\n{preview}");
    }
    if let Some(recipe) = &report.recipe {
        println!("\nRecipe JSON:\n{}", serde_json::to_string_pretty(recipe)?);
    }
    Ok(())
}

fn palette_spec_label(spec: &PaletteSpec) -> String {
    match spec {
        PaletteSpec::Builtin(name) => name.clone(),
        PaletteSpec::Colors(c) => format!("custom({} colors)", c.len()),
        PaletteSpec::File(p) => format!("file:{}", p.display()),
        PaletteSpec::Adaptive(n) => format!("auto-{n}"),
    }
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
