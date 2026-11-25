//! Agent-friendly Clap CLI definitions, shared recipe flags, and command dispatch.

use std::fs;
use std::io::{self, Read};
use std::path::PathBuf;

use anyhow::Result;
use clap::{Args, Parser, Subcommand, ValueEnum};
use serde::Serialize;

use dith_core::color::{ColorSpace, Rgb};
use dith_core::dither::Algorithm;
use dith_core::output::{OutputFormat, SvgShape};
use dith_core::palette::Palette;
use dith_core::preprocess::{AlphaMode, FitMode};
use dith_core::recipe::{DitherRecipe, PaletteSpec, RecipeError};

pub use crate::commands::animate::AnimateArgs;
pub use crate::commands::apply::ApplyArgs;
pub use crate::commands::batch::BatchArgs;
pub use crate::commands::compare::CompareArgs;
pub use crate::commands::generate::GenerateArgs;
pub use crate::commands::inspect::InspectArgs;
pub use crate::commands::list::ListArgs;
pub use crate::commands::preset::PresetArgs;
pub use crate::commands::preview::PreviewArgs;
pub use crate::commands::update::UpdateArgs;
use crate::commands::{
    animate::handle_animate, apply::handle_apply, batch::handle_batch,
    capabilities::handle_capabilities, compare::handle_compare, generate::handle_generate,
    inspect::handle_inspect, list::handle_list, preset::handle_preset, preview::handle_preview,
    update::handle_update,
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

    /// Spend extra CPU for the smallest lossless PNG/APNG (palette, filter, and Zopfli search).
    #[arg(long)]
    pub optimize: bool,
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
        if self.optimize {
            recipe.export.optimize = true;
        }

        recipe.validate()?;
        Ok(recipe)
    }
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
