//! Serializable dithering recipes and pipeline execution.

use std::path::PathBuf;
use std::time::Instant;

use anyhow::Result;
use image::RgbaImage;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::color::Rgb;
use crate::dither::{DitherOptions, dither_image};
use crate::output::{ColorUsage, ExportOptions, color_histogram};
use crate::palette::{Palette, PaletteError, builtin_palette_ids, extract_adaptive_palette};
use crate::preprocess::{PreprocessOptions, finalize_image, prepare_image};
use crate::preset::{PresetStoreError, UnknownPresetError, all_preset_ids, resolve_preset};

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
