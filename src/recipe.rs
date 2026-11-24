//! Serializable dithering recipes, pipeline execution, and self-describing capabilities schema.

use std::path::PathBuf;
use std::time::Instant;

use anyhow::Result;
use image::RgbaImage;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::animate::AnimFormat;
use crate::color::{ColorSpace, Rgb};
use crate::dither::{Algorithm, AlgorithmInfo, DitherOptions, dither_image};
use crate::generate::{Pattern, PatternInfo};
use crate::output::{ColorUsage, ExportOptions, OutputFormat, color_histogram};
use crate::palette::{
    BuiltinPaletteInfo, Palette, PaletteError, builtin_palette_ids, extract_adaptive_palette,
    list_builtin_palettes,
};
use crate::plot::{PAPER_SIZES, PlotFormat};
use crate::preprocess::{PreprocessOptions, finalize_image, prepare_image};
use crate::preset::{
    PresetStoreError, PresetSummary, UnknownPresetError, all_preset_ids, list_all_presets,
    resolve_preset,
};

/// Structured domain error carrying an agent-friendly machine code and recovery hints.
#[derive(Debug, Error)]
pub enum RecipeError {
    #[error("{0}")]
    UnknownPreset(#[from] UnknownPresetError),
    #[error("{0}")]
    PresetStore(#[from] PresetStoreError),
    #[error("{0}")]
    Palette(#[from] PaletteError),
    #[error("invalid argument '{field}': {reason}")]
    Validation { field: &'static str, reason: String },
    #[error("output file already exists at '{path}' (pass --overwrite to replace it)")]
    OutputExists { path: String },
    #[error("failed to parse JSON recipe: {0}")]
    InvalidRecipeJson(String),
}

impl RecipeError {
    /// Machine-readable error code for JSON envelopes.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::UnknownPreset(_) => "UNKNOWN_PRESET",
            Self::PresetStore(PresetStoreError::InvalidName { .. }) => "INVALID_PRESET_NAME",
            Self::PresetStore(PresetStoreError::AlreadyExists { .. }) => "PRESET_ALREADY_EXISTS",
            Self::PresetStore(PresetStoreError::NotFound { .. }) => "PRESET_NOT_FOUND",
            Self::PresetStore(PresetStoreError::NoUserConfigDir) => "NO_USER_CONFIG_DIR",
            Self::PresetStore(PresetStoreError::Io { .. }) => "PRESET_IO_ERROR",
            Self::PresetStore(PresetStoreError::Parse { .. }) => "PRESET_PARSE_ERROR",
            Self::Palette(PaletteError::UnknownPalette { .. }) => "UNKNOWN_PALETTE",
            Self::Palette(PaletteError::TooFewColors { .. }) => "PALETTE_TOO_FEW_COLORS",
            Self::Palette(PaletteError::InvalidColor(_)) => "INVALID_HEX_COLOR",
            Self::Palette(PaletteError::Io { .. }) => "PALETTE_IO_ERROR",
            Self::Palette(PaletteError::ParseFile { .. }) => "PALETTE_PARSE_ERROR",
            Self::Validation { .. } => "INVALID_ARGUMENT",
            Self::OutputExists { .. } => "OUTPUT_EXISTS",
            Self::InvalidRecipeJson(_) => "INVALID_RECIPE_JSON",
        }
    }

    /// Suggested recovery hint for agents.
    #[must_use]
    pub fn suggestion(&self) -> String {
        match self {
            Self::UnknownPreset(_) => {
                "Run `dith list presets --json` (or `dith preset list --json`) to inspect built-in and saved local presets."
                    .to_string()
            }
            Self::PresetStore(PresetStoreError::InvalidName { .. }) => {
                "Use a slug like 'brand-hero' or 'editorial-v2' (lowercase letters, digits, hyphens)."
                    .to_string()
            }
            Self::PresetStore(PresetStoreError::AlreadyExists { .. }) => {
                "Omit --no-clobber to update the existing local preset, or choose a new name."
                    .to_string()
            }
            Self::PresetStore(PresetStoreError::NotFound { .. }) => {
                "Run `dith preset list --json` to see saved presets and their scope (project vs user)."
                    .to_string()
            }
            Self::PresetStore(
                PresetStoreError::NoUserConfigDir
                | PresetStoreError::Io { .. }
                | PresetStoreError::Parse { .. },
            ) => "Check directory permissions or pass --dir <PATH> / --scope project.".to_string(),
            Self::Palette(PaletteError::UnknownPalette { .. }) => {
                "Run `dith list palettes --json`, use `grayN` (e.g. gray4), or pass --colors \"#111,#eee\"."
                    .to_string()
            }
            Self::Palette(PaletteError::TooFewColors { .. }) => {
                "Provide at least 2 distinct colors.".to_string()
            }
            Self::Palette(PaletteError::InvalidColor(_)) => {
                "Use hex format like #111111, #f5efe6, or shorthand #000.".to_string()
            }
            Self::OutputExists { .. } => {
                "Pass --overwrite to replace the existing file or choose a new -o path.".to_string()
            }
            Self::Validation { .. } | Self::InvalidRecipeJson(_) => {
                "Run `dith capabilities` to inspect valid parameter names and value ranges."
                    .to_string()
            }
            Self::Palette(PaletteError::Io { .. } | PaletteError::ParseFile { .. }) => {
                "Supported palette files are .hex/.txt (hex colors), .gpl (GIMP Palette), and .json (array of hex strings)."
                    .to_string()
            }
        }
    }

    /// Valid enum/slug options when applicable.
    #[must_use]
    pub fn valid_values(&self) -> Option<Vec<String>> {
        match self {
            Self::UnknownPreset(_) => Some(all_preset_ids()),
            Self::Palette(PaletteError::UnknownPalette { .. }) => Some(
                builtin_palette_ids()
                    .into_iter()
                    .map(ToString::to_string)
                    .collect(),
            ),
            _ => None,
        }
    }
}

/// Declarative palette selector inside a [`DitherRecipe`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", content = "value", rename_all = "kebab-case")]
pub enum PaletteSpec {
    /// Named built-in palette (e.g. `"bw"`, `"newsprint"`, `"gameboy"`, `"gray6"`).
    Builtin(String),
    /// Explicit list of hex colors.
    Colors(Vec<Rgb>),
    /// Load colors from a `.hex`, `.gpl`, or `.json` file on disk.
    File(PathBuf),
    /// Adaptively extract `N` perceptual colors from the input image via Oklab k-means++.
    Adaptive(usize),
}

impl Default for PaletteSpec {
    fn default() -> Self {
        Self::Builtin("bw".to_string())
    }
}

/// Complete, self-contained, JSON-serializable dithering recipe.
///
/// Agents can emit this with `--emit-recipe` and replay or tweak it with `--recipe <path|->`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DitherRecipe {
    pub preset: Option<String>,
    pub palette: PaletteSpec,
    pub dither: DitherOptions,
    pub preprocess: PreprocessOptions,
    pub export: ExportOptions,
}

impl DitherRecipe {
    /// Drop photo-oriented preparation (auto-levels, contrast boost, sharpening) that presets tune
    /// for real photographs. Procedural sources are already clean and full-range, and those steps
    /// would clip their highlights into flat patches.
    pub const fn strip_photo_prep(&mut self) {
        self.preprocess.auto_levels = false;
        self.preprocess.contrast = 0.0;
        self.preprocess.sharpen = 0.0;
    }

    /// Construct a recipe seeded from a saved local preset or built-in preset.
    ///
    /// Local lookup order: `$DITH_PRESETS_DIR` -> `.dith/presets/` -> `~/.config/dith/presets/` -> built-in.
    ///
    /// # Errors
    /// Returns [`RecipeError::UnknownPreset`] if the preset slug is not recognized.
    pub fn from_preset(name: &str) -> Result<Self, RecipeError> {
        let resolved = resolve_preset(name)?;
        Ok(resolved.recipe)
    }

    /// Validate numeric parameter bounds before running expensive image work.
    ///
    /// # Errors
    /// Returns [`RecipeError::Validation`] if any field falls outside its legal domain.
    pub fn validate(&self) -> Result<(), RecipeError> {
        if !(0.0..=1.5).contains(&self.dither.strength) {
            return Err(RecipeError::Validation {
                field: "strength",
                reason: format!("must be in 0.0..=1.5 (got {})", self.dither.strength),
            });
        }
        if !(-0.5..=0.5).contains(&self.dither.bias) {
            return Err(RecipeError::Validation {
                field: "bias",
                reason: format!("must be in -0.5..=0.5 (got {})", self.dither.bias),
            });
        }
        if !(0.0..=0.5).contains(&self.dither.noise) {
            return Err(RecipeError::Validation {
                field: "noise",
                reason: format!("must be in 0.0..=0.5 (got {})", self.dither.noise),
            });
        }
        if self.dither.pattern_scale == 0 || self.dither.pattern_scale > 32 {
            return Err(RecipeError::Validation {
                field: "pattern-scale",
                reason: format!("must be in 1..=32 (got {})", self.dither.pattern_scale),
            });
        }
        if self.preprocess.pixel_size == 0 || self.preprocess.pixel_size > 64 {
            return Err(RecipeError::Validation {
                field: "pixel-size",
                reason: format!("must be in 1..=64 (got {})", self.preprocess.pixel_size),
            });
        }
        if !(-1.0..=1.0).contains(&self.preprocess.brightness) {
            return Err(RecipeError::Validation {
                field: "brightness",
                reason: format!("must be in -1.0..=1.0 (got {})", self.preprocess.brightness),
            });
        }
        if !(-1.0..=1.0).contains(&self.preprocess.contrast) {
            return Err(RecipeError::Validation {
                field: "contrast",
                reason: format!("must be in -1.0..=1.0 (got {})", self.preprocess.contrast),
            });
        }
        if !(0.1..=5.0).contains(&self.preprocess.gamma) {
            return Err(RecipeError::Validation {
                field: "gamma",
                reason: format!("must be in 0.1..=5.0 (got {})", self.preprocess.gamma),
            });
        }
        if !(0.0..=3.0).contains(&self.preprocess.saturation) {
            return Err(RecipeError::Validation {
                field: "saturation",
                reason: format!("must be in 0.0..=3.0 (got {})", self.preprocess.saturation),
            });
        }
        if !(0.0..=5.0).contains(&self.preprocess.sharpen) {
            return Err(RecipeError::Validation {
                field: "sharpen",
                reason: format!("must be in 0.0..=5.0 (got {})", self.preprocess.sharpen),
            });
        }
        if !(0.0..=20.0).contains(&self.preprocess.blur) {
            return Err(RecipeError::Validation {
                field: "blur",
                reason: format!("must be in 0.0..=20.0 (got {})", self.preprocess.blur),
            });
        }
        if let PaletteSpec::Adaptive(n) = self.palette
            && !(2..=64).contains(&n)
        {
            return Err(RecipeError::Validation {
                field: "auto-palette",
                reason: format!("must be in 2..=64 (got {n})"),
            });
        }
        Ok(())
    }

    /// Resolve the palette specification into a concrete [`Palette`] for a prepared image.
    ///
    /// # Errors
    /// Returns [`RecipeError::Palette`] if the palette cannot be loaded or parsed.
    pub fn resolve_palette(&self, img: &RgbaImage) -> Result<Palette, RecipeError> {
        let palette = match &self.palette {
            PaletteSpec::Builtin(name) => Palette::by_name(name)?,
            PaletteSpec::Colors(colors) => {
                Palette::new("custom", "Custom hex color palette", colors.clone())?
            }
            PaletteSpec::File(path) => Palette::from_file(path)?,
            PaletteSpec::Adaptive(count) => {
                extract_adaptive_palette(img, *count, self.dither.seed)?
            }
        };
        Ok(palette)
    }

    /// Validate a non-adaptive palette eagerly (used in `--dry-run`).
    ///
    /// # Errors
    /// Returns [`RecipeError::Palette`] if the static palette is invalid.
    pub fn validate_static_palette(&self) -> Result<Option<Vec<String>>, RecipeError> {
        match &self.palette {
            PaletteSpec::Builtin(name) => Ok(Some(Palette::by_name(name)?.hex_colors())),
            PaletteSpec::Colors(colors) => Ok(Some(
                Palette::new("custom", "Custom hex color palette", colors.clone())?.hex_colors(),
            )),
            PaletteSpec::File(path) => Ok(Some(Palette::from_file(path)?.hex_colors())),
            PaletteSpec::Adaptive(_) => Ok(None),
        }
    }
}

/// Output of running the core dithering pipeline on an in-memory image.
#[derive(Debug, Clone)]
pub struct PipelineOutput {
    pub image: RgbaImage,
    pub palette: Palette,
    pub source_width: u32,
    pub source_height: u32,
    pub working_width: u32,
    pub working_height: u32,
    pub output_width: u32,
    pub output_height: u32,
    pub histogram: Vec<ColorUsage>,
    pub elapsed_ms: u128,
}

/// Execute preprocessing -> palette resolution -> dithering -> finalization on `src`.
///
/// # Errors
/// Returns an error if recipe validation or palette resolution fails.
pub fn run_pipeline(src: &RgbaImage, recipe: &DitherRecipe) -> Result<PipelineOutput, RecipeError> {
    let started = Instant::now();
    recipe.validate()?;
    let source_width = src.width();
    let source_height = src.height();

    let prepared = prepare_image(src, &recipe.preprocess);
    let working_width = prepared.working.width();
    let working_height = prepared.working.height();

    let palette = recipe.resolve_palette(&prepared.working)?;
    let dithered = dither_image(&prepared.working, &palette, &recipe.dither);
    let final_img = finalize_image(dithered, &prepared, &recipe.preprocess);
    let histogram = color_histogram(&final_img);

    Ok(PipelineOutput {
        output_width: final_img.width(),
        output_height: final_img.height(),
        image: final_img,
        palette,
        source_width,
        source_height,
        working_width,
        working_height,
        histogram,
        elapsed_ms: started.elapsed().as_millis(),
    })
}

/// Machine-readable JSON schema returned by `dith capabilities` for zero-shot agent onboarding.
#[derive(Debug, Clone, Serialize)]
pub struct CapabilitiesManifest {
    pub tool: &'static str,
    pub version: &'static str,
    pub description: &'static str,
    pub agent_workflow_tips: &'static [&'static str],
    pub commands: Vec<CommandCapability>,
    pub algorithms: Vec<AlgorithmInfo>,
    pub presets: Vec<PresetSummary>,
    pub palettes: Vec<BuiltinPaletteInfo>,
    pub generators: Vec<PatternInfo>,
    pub color_spaces: Vec<NamedDescription>,
    pub output_formats: Vec<NamedDescription>,
    pub animation_formats: Vec<NamedDescription>,
    /// Pen-plotter file formats written by `dith plot`.
    pub plot_formats: Vec<NamedDescription>,
    /// Named paper sizes accepted by `dith plot --paper`.
    pub paper_sizes: Vec<PaperInfo>,
    pub parameter_ranges: Vec<ParameterRange>,
    pub default_recipe: DitherRecipe,
}

/// A named paper size for `dith plot --paper`, in portrait millimetres.
#[derive(Debug, Clone, Serialize)]
pub struct PaperInfo {
    pub id: &'static str,
    pub width_mm: f64,
    pub height_mm: f64,
}

/// Every named paper size accepted by `dith plot --paper`.
#[must_use]
pub fn paper_sizes() -> Vec<PaperInfo> {
    PAPER_SIZES
        .iter()
        .map(|&(id, width_mm, height_mm)| PaperInfo {
            id,
            width_mm,
            height_mm,
        })
        .collect()
}

#[derive(Debug, Clone, Serialize)]
pub struct CommandCapability {
    pub name: &'static str,
    pub summary: &'static str,
    pub example: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct NamedDescription {
    pub id: &'static str,
    pub description: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct ParameterRange {
    pub flag: &'static str,
    pub value_type: &'static str,
    pub range: &'static str,
    pub default: &'static str,
    pub effect: &'static str,
}

/// Build the full self-describing capabilities manifest.
#[must_use]
pub fn build_capabilities() -> CapabilitiesManifest {
    CapabilitiesManifest {
        tool: "dith",
        version: env!("CARGO_PKG_VERSION"),
        description: "Agent-friendly dithering toolkit CLI for creating crisp 1-bit, duotone, Risograph, halftone, blue-noise, retro, animated, and vector SVG dithers.",
        agent_workflow_tips: &[
            "Pass --json on any command for stable, machine-parseable output and structured errors.",
            "Run `dith inspect <image> --json` first to measure dynamic range, transparency, dominant colors, and get ready-to-run preset recommendations.",
            "Use `dith compare <input> -o sheet.png --presets mac-classic,editorial-print,risograph,film-stipple` to visually compare multiple aesthetics in one grid.",
            "Use `dith generate sphere -o orb.png --preset editorial-print` to craft procedural dither artwork from scratch with zero input files.",
            "Use `dith animate sphere -o orb.gif --preset risograph --frames 24 --fps 16` (or `dith animate photo.png -o boil.gif --temporal boil`) to build seamless animated dithers, or `--view` for live fullscreen terminal playback.",
            "Use `--emit-recipe` and `--recipe <file|->` to save, mutate, and replay exact JSON dithering pipelines.",
            "Save reusable project-local (`.dith/presets/`) or user-global presets with `dith preset save <name> ...` or `--save-preset <name>` on apply/generate.",
            "Use `--dry-run --json` to validate arguments and preview planned dimensions without writing files.",
            "Use `--format svg --svg-shape circle --svg-skip-bg` for crisp transparent-ready web SVGs, or `dith preview` / `--view` for fullscreen Kitty graphics previews (pass `--inline` to print in scrollback instead).",
            "Use `dith plot photo.jpg -o plot.svg -n 20000 --edges 0.8 --json` for pen-plotter stipples (mm SVG or .gcode) with an optimised pen-up tour.",
        ],
        commands: vec![
            CommandCapability {
                name: "apply",
                summary: "Dither an input image (or '-' stdin) with a preset, algorithm, custom/adaptive palette, and tonal controls.",
                example: "dith apply photo.png -o out.png --preset editorial-print --max-dim 960 --json",
            },
            CommandCapability {
                name: "generate",
                summary: "Procedurally generate gradients, 3D spheres, aurora meshes, waves, or fBm noise and dither in one step.",
                example: "dith generate sphere -o sphere.png --preset risograph --width 800 --height 600 --json",
            },
            CommandCapability {
                name: "animate",
                summary: "Create animated dithers (GIF, APNG, animated WebP, spritesheet, PNG frames, or live fullscreen --view) from procedural loops, GIFs, sequences, or boiled photos.",
                example: "dith animate sphere -o orb.gif --preset risograph --frames 24 --fps 16 --temporal locked --json",
            },
            CommandCapability {
                name: "preview",
                summary: "Open a fullscreen terminal preview (default on TTY, or --inline) via Kitty graphics protocol, Braille, or ASCII without writing a file.",
                example: "dith preview photo.png --preset risograph",
            },
            CommandCapability {
                name: "preset",
                summary: "Save, list, inspect, or delete persistent local presets in .dith/presets/ (project) or ~/.config/dith/presets/ (user).",
                example: "dith preset save brand-hero --algorithm blue-noise --colors \"#0f172a,#2563eb,#f5efe6\" --json",
            },
            CommandCapability {
                name: "compare",
                summary: "Render a labeled contact sheet (to -o, --out-dir, or fullscreen with --view) comparing algorithms, presets, or palettes.",
                example: "dith compare photo.png -o sheet.png --algorithms atkinson,ostromoukhov,bayer4,blue-noise --json",
            },
            CommandCapability {
                name: "batch",
                summary: "Dither many images in parallel into an output directory with structured per-file reporting.",
                example: "dith batch img1.png img2.png --out-dir ./dithered --preset mac-classic --json",
            },
            CommandCapability {
                name: "plot",
                summary: "Stipple an image or generator for pen plotters (weighted Voronoi stippling, nearest-neighbour + 2-opt tour) as physical-size mm SVG or G-code, one layer per pen.",
                example: "dith plot photo.jpg -o portrait.gcode -n 20000 --paper a4 --edges 0.8 --pen-down \"M3 S30\" --pen-up M5 --json",
            },
            CommandCapability {
                name: "inspect",
                summary: "Analyze dimensions, luminance percentiles, transparency, dominant hex colors, and recommended dithering commands.",
                example: "dith inspect photo.png --json",
            },
            CommandCapability {
                name: "list",
                summary: "List available algorithms, palettes, presets (including saved local presets), generators, formats, or plotter paper sizes.",
                example: "dith list palettes --tag risograph --json",
            },
            CommandCapability {
                name: "update",
                summary: "Update dith from GitHub Releases (macOS ARM & Linux x86_64) and silently refresh agent skills.",
                example: "dith update --check --json",
            },
            CommandCapability {
                name: "capabilities",
                summary: "Emit this complete JSON discovery manifest of all commands, options, ranges, presets, and palettes.",
                example: "dith capabilities",
            },
        ],
        algorithms: Algorithm::ALL.iter().map(|a| a.info()).collect(),
        presets: list_all_presets(),
        palettes: list_builtin_palettes(),
        generators: Pattern::ALL.iter().map(|p| p.info()).collect(),
        color_spaces: vec![
            NamedDescription {
                id: "oklab",
                description: ColorSpace::Oklab.description(),
            },
            NamedDescription {
                id: "linear",
                description: ColorSpace::Linear.description(),
            },
            NamedDescription {
                id: "srgb",
                description: ColorSpace::Srgb.description(),
            },
        ],
        output_formats: vec![
            NamedDescription {
                id: OutputFormat::Png.id(),
                description: "Lossless pixel-exact PNG (default)",
            },
            NamedDescription {
                id: OutputFormat::Webp.id(),
                description: "Lossless palette-coded WebP, usually the smallest lossless file",
            },
            NamedDescription {
                id: OutputFormat::Jpeg.id(),
                description: "JPEG (flattens alpha)",
            },
            NamedDescription {
                id: OutputFormat::Gif.id(),
                description: "Indexed GIF",
            },
            NamedDescription {
                id: OutputFormat::Bmp.id(),
                description: "BMP (1/4/8-bit palette when opaque with <=256 colors, else 24/32-bit)",
            },
            NamedDescription {
                id: OutputFormat::Tiff.id(),
                description: "Deflate-compressed lossless TIFF",
            },
            NamedDescription {
                id: OutputFormat::Svg.id(),
                description: "Crisp RLE vector SVG or circular halftone dots",
            },
            NamedDescription {
                id: OutputFormat::Kitty.id(),
                description: "Pixel-exact inline terminal preview via Kitty graphics protocol (Kitty, Ghostty, WezTerm)",
            },
            NamedDescription {
                id: OutputFormat::Braille.id(),
                description: "High-density Unicode Braille text preview",
            },
            NamedDescription {
                id: OutputFormat::Ascii.id(),
                description: "Shaded ASCII text art",
            },
        ],
        animation_formats: AnimFormat::ALL
            .iter()
            .map(|format| NamedDescription {
                id: format.id(),
                description: format.description(),
            })
            .collect(),
        plot_formats: PlotFormat::ALL
            .iter()
            .map(|f| NamedDescription {
                id: f.id(),
                description: f.description(),
            })
            .collect(),
        paper_sizes: paper_sizes(),
        parameter_ranges: vec![
            ParameterRange {
                flag: "--strength",
                value_type: "f32",
                range: "0.0..=1.5",
                default: "1.0",
                effect: "Scales error diffusion or ordered screen amplitude",
            },
            ParameterRange {
                flag: "--bias",
                value_type: "f32",
                range: "-0.5..=0.5",
                default: "0.0",
                effect: "Shifts dither threshold darker (<0) or lighter (>0)",
            },
            ParameterRange {
                flag: "--noise",
                value_type: "f32",
                range: "0.0..=0.5",
                default: "0.0",
                effect: "Injects seeded jitter to break regular patterns or banding",
            },
            ParameterRange {
                flag: "--pattern-scale",
                value_type: "u32",
                range: "1..=32",
                default: "1",
                effect: "Enlarges ordered/halftone/blue-noise screen cells",
            },
            ParameterRange {
                flag: "--pixel-size",
                value_type: "u32",
                range: "1..=64",
                default: "1",
                effect: "Downscales before dithering then nearest-upscales for crisp chunky pixels",
            },
            ParameterRange {
                flag: "--brightness",
                value_type: "f32",
                range: "-1.0..=1.0",
                default: "0.0",
                effect: "Linear brightness shift before dithering",
            },
            ParameterRange {
                flag: "--contrast",
                value_type: "f32",
                range: "-1.0..=1.0",
                default: "0.0",
                effect: "Contrast curve around midtones before dithering",
            },
            ParameterRange {
                flag: "--gamma",
                value_type: "f32",
                range: "0.1..=5.0",
                default: "1.0",
                effect: "Midtone gamma (<1 brightens, >1 darkens)",
            },
            ParameterRange {
                flag: "--saturation",
                value_type: "f32",
                range: "0.0..=3.0",
                default: "1.0",
                effect: "Color saturation multiplier in linear light",
            },
            ParameterRange {
                flag: "--sharpen",
                value_type: "f32",
                range: "0.0..=5.0",
                default: "0.0",
                effect: "Unsharp mask intensity before dithering (great for 1-bit edges)",
            },
            ParameterRange {
                flag: "--blur",
                value_type: "f32",
                range: "0.0..=20.0",
                default: "0.0",
                effect: "Pre-dither Gaussian blur sigma to calm sensor noise",
            },
            ParameterRange {
                flag: "--auto-palette",
                value_type: "usize",
                range: "2..=64",
                default: "none",
                effect: "Extracts N perceptual colors from the image using Oklab k-means++",
            },
            ParameterRange {
                flag: "--temporal",
                value_type: "enum",
                range: "locked | boil | grain",
                default: "locked (boil for static photos)",
                effect: "Controls how the dither pattern evolves across animation frames",
            },
            ParameterRange {
                flag: "--boil-rate",
                value_type: "u32",
                range: "1..=60",
                default: "1",
                effect: "Updates film boil every N frames (2 = animated on twos, 3 = on threes)",
            },
            ParameterRange {
                flag: "--boil-amount",
                value_type: "f32",
                range: "0.0..=0.5",
                default: "temporal default",
                effect: "Explicit boil/grain jitter intensity per animation frame",
            },
            ParameterRange {
                flag: "--screen-drift",
                value_type: "DX,DY",
                range: "integers",
                default: "none",
                effect: "Scrolls ordered/halftone/scanline screen cells smoothly across frames",
            },
            ParameterRange {
                flag: "--easing",
                value_type: "enum",
                range: "linear | ease-in-out | ease-in | ease-out | smoothstep | bounce | steps",
                default: "linear",
                effect: "Easing curve applied to procedural motion and --sweep-* animations",
            },
            ParameterRange {
                flag: "--sweep-bias / --sweep-contrast / --sweep-brightness / --sweep-gamma / --sweep-strength / --sweep-pixel-size",
                value_type: "START..END",
                range: "parameter domain",
                default: "none",
                effect: "Animates dithering or tonal knobs across frames (dissolves, mosaic focus-in, pulse breathing)",
            },
            ParameterRange {
                flag: "--cycle-palette",
                value_type: "bool",
                range: "true | false",
                default: "false",
                effect: "Classic Amiga/VGA palette color cycling across animation frames",
            },
            ParameterRange {
                flag: "--optimize",
                value_type: "bool",
                range: "true | false",
                default: "false",
                effect: "Smallest lossless PNG/APNG/spritesheet via palette-order and filter search plus Zopfli (encodes several times slower)",
            },
            ParameterRange {
                flag: "plot --points / --density",
                value_type: "usize / f64",
                range: "1..=1000000 / 0.001..=10000 dots per cm² of solid ink",
                default: "5000",
                effect: "Stipple budget across all pens (density scales with paper size and image darkness)",
            },
            ParameterRange {
                flag: "plot --iterations / --tolerance",
                value_type: "u32 / f64",
                range: "0..=1000 / 0.0..=100.0 raster px",
                default: "50 / 0.02",
                effect: "Lloyd relaxation budget; stops early once the mean point movement falls below the tolerance",
            },
            ParameterRange {
                flag: "plot --resolution",
                value_type: "u32",
                range: "64..=8192",
                default: "1200",
                effect: "Longest side of the density raster that stipples are relaxed on",
            },
            ParameterRange {
                flag: "plot --edges",
                value_type: "f32",
                range: "0.0..=4.0",
                default: "0.0",
                effect: "Adds Sobel edge strength to the ink density so eyes, lips, and outlines get extra dots (0.5..1.0 for portraits)",
            },
            ParameterRange {
                flag: "plot --cutoff",
                value_type: "f32",
                range: "0.0..=0.99",
                default: "0.0",
                effect: "Ink densities below this are bare paper (cleans near-white backgrounds)",
            },
            ParameterRange {
                flag: "plot --paper / --orientation / --margin / --fit",
                value_type: "size / enum / mm / enum",
                range: "a2..a6 | letter | legal | tabloid | WxH[mm|cm|in] / auto | portrait | landscape / 0..=1000 / contain | cover",
                default: "a4 / auto / 15 / contain",
                effect: "Physical sheet, orientation, margins, and how the source fills the drawable area",
            },
            ParameterRange {
                flag: "plot --dot / --pen-width / --min-dot / --max-dot",
                value_type: "enum / mm / mm / mm",
                range: "point | circle | stroke / 0.05..=10 / 0..=max-dot / 0.05..=50",
                default: "point / 0.5 / max-dot / 1.2",
                effect: "Dot primitive and size; circle/stroke dots scale with local darkness when min-dot < max-dot",
            },
            ParameterRange {
                flag: "plot --pen-up / --pen-down / --pen-change / --dwell",
                value_type: "G-code lines ('\\n' separated) / seconds",
                range: "any / 0.0..=10.0",
                default: "G0 Z2 / G1 Z0 F1000 / M0 ; change pen / 0",
                effect: "G-code pen lift, drop, between-pen pause, and settle time (G4 P<s>)",
            },
            ParameterRange {
                flag: "plot --draw-feed / --travel-feed / --origin",
                value_type: "mm/min / mm/min / enum",
                range: "1..=1000000 / 1..=1000000 / bottom-left | top-left | center",
                default: "3000 / 6000 / bottom-left",
                effect: "G-code feed rates (also drive the plot-time estimate) and where X0 Y0 sits",
            },
            ParameterRange {
                flag: "plot --no-optimize / --tour-time-limit",
                value_type: "bool / seconds",
                range: "true | false / 0.0..=3600 (0 = no cap)",
                default: "false / 10",
                effect: "Skip 2-opt or cap its wall time (reported as tour_capped; output is byte-deterministic unless capped)",
            },
        ],
        default_recipe: DitherRecipe::default(),
    }
}
