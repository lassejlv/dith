//! Curated built-in presets and persistent local preset storage (project-local and user-global).

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::color::ColorSpace;
use crate::dither::{Algorithm, DitherOptions};
use crate::preprocess::PreprocessOptions;
use crate::recipe::{DitherRecipe, PaletteSpec};

/// Environment variable allowing agents/tests to specify an extra highest-priority preset directory.
pub const PRESETS_DIR_ENV: &str = "DITH_PRESETS_DIR";

/// Error returned when an unknown preset name is requested.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("unknown preset '{name}'")]
pub struct UnknownPresetError {
    pub name: String,
}

/// Error returned when saving, loading, or deleting local presets fails.
#[derive(Debug, Error)]
pub enum PresetStoreError {
    #[error("invalid preset name '{name}': use 1..=64 ASCII letters, digits, or hyphens")]
    InvalidName { name: String },
    #[error("local preset '{name}' already exists at '{path}' (omit --no-clobber to replace it)")]
    AlreadyExists { name: String, path: String },
    #[error("local preset '{name}' not found in {scope}")]
    NotFound { name: String, scope: String },
    #[error("cannot determine user config directory (set XDG_CONFIG_HOME or HOME)")]
    NoUserConfigDir,
    #[error("failed to read/write preset '{path}': {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse saved preset '{path}': {reason}")]
    Parse { path: String, reason: String },
}

/// Where to store or manage a local preset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
#[serde(rename_all = "kebab-case")]
pub enum PresetScope {
    /// Save inside the current workspace at `.dith/presets/<name>.json`.
    #[default]
    Project,
    /// Save globally for the current user at `~/.config/dith/presets/<name>.json`.
    User,
}

/// Origin of a discovered preset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
#[serde(rename_all = "kebab-case")]
pub enum PresetSource {
    /// Loaded from `$DITH_PRESETS_DIR`.
    Env,
    /// Loaded from `.dith/presets/` in the current project.
    Project,
    /// Loaded from the user's config directory (`~/.config/dith/presets/`).
    User,
    /// Built into `dith`.
    Builtin,
}

impl PresetSource {
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Env => "env",
            Self::Project => "project",
            Self::User => "user",
            Self::Builtin => "builtin",
        }
    }
}

/// How a built-in preset specifies its default color palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "kebab-case")]
pub enum PresetPalette {
    Builtin(&'static str),
    Adaptive(usize),
}

/// Complete curated built-in artistic preset.
#[derive(Debug, Clone, Serialize)]
pub struct Preset {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub tags: &'static [&'static str],
    pub algorithm: Algorithm,
    pub palette: PresetPalette,
    pub color_space: ColorSpace,
    pub serpentine: bool,
    pub strength: f32,
    pub pixel_size: u32,
    pub pattern_scale: u32,
    pub contrast: f32,
    pub sharpen: f32,
    pub auto_levels: bool,
    pub grayscale: bool,
}

impl Preset {
    /// Convert this built-in preset into a full [`DitherRecipe`].
    #[must_use]
    pub fn to_recipe(&self) -> DitherRecipe {
        let palette = match self.palette {
            PresetPalette::Builtin(id) => PaletteSpec::Builtin(id.to_string()),
            PresetPalette::Adaptive(n) => PaletteSpec::Adaptive(n),
        };
        DitherRecipe {
            preset: Some(self.id.to_string()),
            palette,
            dither: DitherOptions {
                algorithm: self.algorithm,
                color_space: self.color_space,
                strength: self.strength,
                serpentine: self.serpentine,
                pattern_scale: self.pattern_scale,
                ..DitherOptions::default()
            },
            preprocess: PreprocessOptions {
                pixel_size: self.pixel_size,
                contrast: self.contrast,
                sharpen: self.sharpen,
                auto_levels: self.auto_levels,
                grayscale: self.grayscale,
                ..PreprocessOptions::default()
            },
            export: crate::output::ExportOptions::default(),
        }
    }
}

/// Persistent JSON representation of a locally saved preset.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedPreset {
    pub id: String,
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub recipe: DitherRecipe,
}

/// Unified summary for both built-in and saved local presets (used in `list`, `preset list`, `capabilities`).
#[derive(Debug, Clone, Serialize)]
pub struct PresetSummary {
    pub id: String,
    pub name: String,
    pub description: String,
    pub tags: Vec<String>,
    pub source: PresetSource,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub algorithm: &'static str,
    pub recipe: DitherRecipe,
}

/// Normalize and validate a preset slug (`[a-z0-9-]`, 1..=64 chars, no leading/trailing hyphens).
///
/// # Errors
/// Returns [`PresetStoreError::InvalidName`] if the slug is empty, too long, or contains illegal characters.
pub fn normalize_preset_slug(raw: &str) -> Result<String, PresetStoreError> {
    let slug = raw.trim().to_ascii_lowercase().replace(['_', ' '], "-");
    let valid = !slug.is_empty()
        && slug.len() <= 64
        && !slug.starts_with('-')
        && !slug.ends_with('-')
        && slug
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if !valid {
        return Err(PresetStoreError::InvalidName {
            name: raw.to_string(),
        });
    }
    Ok(slug)
}

/// Return the project-local preset directory (`.dith/presets` in CWD).
#[must_use]
pub fn project_presets_dir() -> PathBuf {
    PathBuf::from(".dith").join("presets")
}

/// Return the user-global preset directory (`$XDG_CONFIG_HOME/dith/presets` or `~/.config/dith/presets`).
#[must_use]
pub fn user_presets_dir() -> Option<PathBuf> {
    if let Some(xdg) = env::var_os("XDG_CONFIG_HOME")
        && !xdg.is_empty()
    {
        return Some(PathBuf::from(xdg).join("dith").join("presets"));
    }
    if let Some(home) = env::var_os("HOME")
        && !home.is_empty()
    {
        return Some(
            PathBuf::from(home)
                .join(".config")
                .join("dith")
                .join("presets"),
        );
    }
    if let Some(appdata) = env::var_os("APPDATA")
        && !appdata.is_empty()
    {
        return Some(PathBuf::from(appdata).join("dith").join("presets"));
    }
    None
}

/// Return the target directory for saving/deleting a preset given scope or explicit `--dir`.
///
/// # Errors
/// Returns [`PresetStoreError::NoUserConfigDir`] when `PresetScope::User` is chosen and no config root is set.
pub fn target_preset_dir(
    scope: PresetScope,
    custom_dir: Option<&Path>,
) -> Result<PathBuf, PresetStoreError> {
    if let Some(dir) = custom_dir {
        return Ok(dir.to_path_buf());
    }
    if let Some(env_dir) = env::var_os(PRESETS_DIR_ENV)
        && !env_dir.is_empty()
        && scope == PresetScope::Project
    {
        return Ok(PathBuf::from(env_dir));
    }
    match scope {
        PresetScope::Project => Ok(project_presets_dir()),
        PresetScope::User => user_presets_dir().ok_or(PresetStoreError::NoUserConfigDir),
    }
}

/// Ordered search directories for local presets (highest priority first).
#[must_use]
pub fn search_preset_dirs() -> Vec<(PresetSource, PathBuf)> {
    let mut dirs = Vec::new();
    if let Some(env_dir) = env::var_os(PRESETS_DIR_ENV)
        && !env_dir.is_empty()
    {
        dirs.push((PresetSource::Env, PathBuf::from(env_dir)));
    }
    dirs.push((PresetSource::Project, project_presets_dir()));
    if let Some(user_dir) = user_presets_dir() {
        dirs.push((PresetSource::User, user_dir));
    }
    dirs
}

/// Overwrite behavior when saving a local preset file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OverwritePolicy {
    #[default]
    Replace,
    NoClobber,
}

/// Parameters for persisting a local preset to disk.
#[derive(Debug, Clone)]
pub struct SavePresetRequest<'a> {
    pub slug: &'a str,
    pub title: Option<&'a str>,
    pub description: Option<&'a str>,
    pub tags: Vec<String>,
    pub recipe: DitherRecipe,
    pub scope: PresetScope,
    pub custom_dir: Option<&'a Path>,
    pub overwrite: OverwritePolicy,
}

/// Save a local preset as pretty JSON to disk, creating parent directories automatically.
///
/// # Errors
/// Returns an error if the slug is invalid, `--no-clobber` blocks overwrite, or disk I/O fails.
pub fn save_local_preset(
    req: SavePresetRequest<'_>,
) -> Result<(SavedPreset, PathBuf, PresetSource), PresetStoreError> {
    let id = normalize_preset_slug(req.slug)?;
    let dir = target_preset_dir(req.scope, req.custom_dir)?;
    let path = dir.join(format!("{id}.json"));

    if req.overwrite == OverwritePolicy::NoClobber && path.exists() {
        return Err(PresetStoreError::AlreadyExists {
            name: id,
            path: path.display().to_string(),
        });
    }

    let mut recipe = req.recipe;
    recipe.preset = Some(id.clone());
    let name = req
        .title
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map_or_else(|| title_case_slug(&id), ToString::to_string);
    let description = req
        .description
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map_or_else(
            || {
                format!(
                    "Saved local preset ({}, {})",
                    recipe.dither.algorithm.id(),
                    match &recipe.palette {
                        PaletteSpec::Builtin(p) => p.clone(),
                        PaletteSpec::Colors(c) => format!("{} custom colors", c.len()),
                        PaletteSpec::File(p) => p.display().to_string(),
                        PaletteSpec::Adaptive(n) => format!("auto-{n}"),
                    }
                )
            },
            ToString::to_string,
        );

    let saved = SavedPreset {
        id,
        name,
        description,
        tags: req.tags,
        recipe,
    };

    fs::create_dir_all(&dir).map_err(|source| PresetStoreError::Io {
        path: dir.display().to_string(),
        source,
    })?;
    let json = serde_json::to_string_pretty(&saved).map_err(|e| PresetStoreError::Parse {
        path: path.display().to_string(),
        reason: e.to_string(),
    })?;
    fs::write(&path, format!("{json}\n")).map_err(|source| PresetStoreError::Io {
        path: path.display().to_string(),
        source,
    })?;

    let source = if req.custom_dir.is_some()
        || (req.scope == PresetScope::Project && env::var_os(PRESETS_DIR_ENV).is_some())
    {
        PresetSource::Env
    } else {
        match req.scope {
            PresetScope::Project => PresetSource::Project,
            PresetScope::User => PresetSource::User,
        }
    };

    Ok((saved, path, source))
}

/// Delete a saved local preset file.
///
/// # Errors
/// Returns an error if the slug is invalid, the preset file does not exist, or deletion fails.
pub fn delete_local_preset(
    slug_raw: &str,
    scope: PresetScope,
    custom_dir: Option<&Path>,
) -> Result<(String, PathBuf), PresetStoreError> {
    let id = normalize_preset_slug(slug_raw)?;
    let dir = target_preset_dir(scope, custom_dir)?;
    let path = dir.join(format!("{id}.json"));
    if !path.exists() {
        return Err(PresetStoreError::NotFound {
            name: id,
            scope: dir.display().to_string(),
        });
    }
    fs::remove_file(&path).map_err(|source| PresetStoreError::Io {
        path: path.display().to_string(),
        source,
    })?;
    Ok((id, path))
}

/// Look up a preset across local directories first (env -> project -> user), then built-ins.
///
/// # Errors
/// Returns [`UnknownPresetError`] if no local or built-in preset matches `name`.
pub fn resolve_preset(name: &str) -> Result<PresetSummary, UnknownPresetError> {
    let key = name.trim().to_ascii_lowercase().replace(['_', ' '], "-");

    for (source, dir) in search_preset_dirs() {
        let candidate = dir.join(format!("{key}.json"));
        if candidate.is_file()
            && let Ok(saved) = load_saved_preset_file(&candidate)
        {
            return Ok(PresetSummary {
                id: saved.id,
                name: saved.name,
                description: saved.description,
                tags: saved.tags,
                source,
                path: Some(candidate.display().to_string()),
                algorithm: saved.recipe.dither.algorithm.id(),
                recipe: saved.recipe,
            });
        }
    }

    let builtin = preset_by_name(&key)?;
    Ok(builtin_to_summary(builtin))
}

/// List all available presets (merging local presets over built-ins of the same slug).
#[must_use]
pub fn list_all_presets() -> Vec<PresetSummary> {
    let mut by_id: BTreeMap<String, PresetSummary> = BTreeMap::new();

    // Insert built-ins first so local overrides replace them cleanly while preserving ordering.
    for p in PRESETS {
        let summary = builtin_to_summary(p);
        by_id.insert(summary.id.clone(), summary);
    }

    // Walk search dirs from lowest priority (User) to highest priority (Env).
    for (source, dir) in search_preset_dirs().into_iter().rev() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        let mut paths: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
            .collect();
        paths.sort();

        for path in paths {
            if let Ok(saved) = load_saved_preset_file(&path) {
                let summary = PresetSummary {
                    id: saved.id.clone(),
                    name: saved.name,
                    description: saved.description,
                    tags: saved.tags,
                    source,
                    path: Some(path.display().to_string()),
                    algorithm: saved.recipe.dither.algorithm.id(),
                    recipe: saved.recipe,
                };
                by_id.insert(saved.id, summary);
            }
        }
    }

    // Sort local presets before built-ins so agent-created presets appear prominently.
    let mut list: Vec<PresetSummary> = by_id.into_values().collect();
    list.sort_by(|a, b| {
        let rank = |s: PresetSource| match s {
            PresetSource::Env => 0,
            PresetSource::Project => 1,
            PresetSource::User => 2,
            PresetSource::Builtin => 3,
        };
        rank(a.source)
            .cmp(&rank(b.source))
            .then_with(|| a.id.cmp(&b.id))
    });
    list
}

fn load_saved_preset_file(path: &Path) -> Result<SavedPreset, PresetStoreError> {
    let raw = fs::read_to_string(path).map_err(|source| PresetStoreError::Io {
        path: path.display().to_string(),
        source,
    })?;
    let mut saved: SavedPreset =
        serde_json::from_str(&raw).map_err(|e| PresetStoreError::Parse {
            path: path.display().to_string(),
            reason: e.to_string(),
        })?;
    if saved.id.trim().is_empty()
        && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
    {
        saved.id = stem.to_string();
    }
    saved.recipe.preset = Some(saved.id.clone());
    Ok(saved)
}

fn builtin_to_summary(p: &Preset) -> PresetSummary {
    PresetSummary {
        id: p.id.to_string(),
        name: p.name.to_string(),
        description: p.description.to_string(),
        tags: p.tags.iter().map(|t| (*t).to_string()).collect(),
        source: PresetSource::Builtin,
        path: None,
        algorithm: p.algorithm.id(),
        recipe: p.to_recipe(),
    }
}

fn title_case_slug(slug: &str) -> String {
    slug.split('-')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => {
                    let mut s = first.to_uppercase().to_string();
                    s.push_str(chars.as_str());
                    s
                }
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Look up a built-in preset by slug (case-insensitive, underscores allowed).
///
/// # Errors
/// Returns [`UnknownPresetError`] if no built-in preset matches `name`.
pub fn preset_by_name(name: &str) -> Result<&'static Preset, UnknownPresetError> {
    let key = name.trim().to_ascii_lowercase().replace('_', "-");
    PRESETS
        .iter()
        .find(|p| p.id == key)
        .ok_or_else(|| UnknownPresetError {
            name: name.to_string(),
        })
}

/// Return all curated built-in presets.
#[must_use]
pub const fn all_presets() -> &'static [Preset] {
    PRESETS
}

/// Return all valid preset slugs (both saved local presets and built-ins) for error hints.
#[must_use]
pub fn all_preset_ids() -> Vec<String> {
    list_all_presets().into_iter().map(|p| p.id).collect()
}

/// Return all valid built-in preset slugs.
#[must_use]
pub fn preset_ids() -> Vec<&'static str> {
    PRESETS.iter().map(|p| p.id).collect()
}

const PRESETS: &[Preset] = &[
    Preset {
        id: "mac-classic",
        name: "1984 Macintosh Classic",
        description: "Crisp Atkinson 1-bit dither on warm off-white CRT glass with edge sharpening",
        tags: &["1-bit", "retro", "editorial", "mac"],
        algorithm: Algorithm::Atkinson,
        palette: PresetPalette::Builtin("mac-1bit"),
        color_space: ColorSpace::Oklab,
        serpentine: false,
        strength: 1.0,
        pixel_size: 1,
        pattern_scale: 1,
        contrast: 0.08,
        sharpen: 0.6,
        auto_levels: true,
        grayscale: true,
    },
    Preset {
        id: "editorial-print",
        name: "Editorial Gallery Print",
        description: "Silky Ostromoukhov variable-coefficient diffusion on warm archival newsprint",
        tags: &["editorial", "print", "monochrome", "refined"],
        algorithm: Algorithm::Ostromoukhov,
        palette: PresetPalette::Builtin("newsprint"),
        color_space: ColorSpace::Oklab,
        serpentine: true,
        strength: 1.0,
        pixel_size: 1,
        pattern_scale: 1,
        contrast: 0.06,
        sharpen: 0.25,
        auto_levels: true,
        grayscale: true,
    },
    Preset {
        id: "film-stipple",
        name: "Filmic Blue-Noise Stipple",
        description: "Artifact-free 64x64 Void-and-Cluster blue noise in India ink and cream",
        tags: &["stipple", "modern", "editorial", "organic"],
        algorithm: Algorithm::BlueNoise,
        palette: PresetPalette::Builtin("ink-cream"),
        color_space: ColorSpace::Oklab,
        serpentine: false,
        strength: 1.0,
        pixel_size: 1,
        pattern_scale: 1,
        contrast: 0.08,
        sharpen: 0.2,
        auto_levels: true,
        grayscale: true,
    },
    Preset {
        id: "risograph",
        name: "Risograph Federal Blue",
        description: "Crisp Stucki error diffusion in soy cobalt blue on warm cream paper stock",
        tags: &["risograph", "print", "duotone", "poster"],
        algorithm: Algorithm::Stucki,
        palette: PresetPalette::Builtin("risograph-blue"),
        color_space: ColorSpace::Oklab,
        serpentine: true,
        strength: 1.0,
        pixel_size: 1,
        pattern_scale: 1,
        contrast: 0.1,
        sharpen: 0.35,
        auto_levels: true,
        grayscale: true,
    },
    Preset {
        id: "risograph-red",
        name: "Risograph Vermilion",
        description: "High-impact red Risograph print with Stucki grain on warm uncoated stock",
        tags: &["risograph", "print", "duotone", "bold"],
        algorithm: Algorithm::Stucki,
        palette: PresetPalette::Builtin("risograph-red"),
        color_space: ColorSpace::Oklab,
        serpentine: true,
        strength: 1.0,
        pixel_size: 1,
        pattern_scale: 1,
        contrast: 0.12,
        sharpen: 0.4,
        auto_levels: true,
        grayscale: true,
    },
    Preset {
        id: "risograph-duo",
        name: "Risograph Two-Drum Overprint",
        description: "Cobalt blue and fluorescent pink two-color Risograph zine look",
        tags: &["risograph", "print", "vivid", "poster"],
        algorithm: Algorithm::FloydSteinberg,
        palette: PresetPalette::Builtin("risograph-duo"),
        color_space: ColorSpace::Oklab,
        serpentine: true,
        strength: 1.0,
        pixel_size: 2,
        pattern_scale: 1,
        contrast: 0.1,
        sharpen: 0.3,
        auto_levels: true,
        grayscale: false,
    },
    Preset {
        id: "gameboy",
        name: "Game Boy DMG",
        description: "Authentic 4-shade olive LCD with Bayer 4x4 crosshatch and chunky 3px pixels",
        tags: &["retro", "handheld", "pixel-art", "4-color"],
        algorithm: Algorithm::Bayer4,
        palette: PresetPalette::Builtin("gameboy"),
        color_space: ColorSpace::Oklab,
        serpentine: false,
        strength: 1.0,
        pixel_size: 3,
        pattern_scale: 1,
        contrast: 0.12,
        sharpen: 0.25,
        auto_levels: true,
        grayscale: true,
    },
    Preset {
        id: "crt-amber",
        name: "Amber Phosphor CRT",
        description: "Warm P3 amber terminal monitor with 8x8 Bayer screen and chunky pixels",
        tags: &["crt", "retro", "dark", "terminal"],
        algorithm: Algorithm::Bayer8,
        palette: PresetPalette::Builtin("amber-crt"),
        color_space: ColorSpace::Oklab,
        serpentine: false,
        strength: 1.0,
        pixel_size: 2,
        pattern_scale: 1,
        contrast: 0.1,
        sharpen: 0.2,
        auto_levels: true,
        grayscale: true,
    },
    Preset {
        id: "phosphor",
        name: "Green Phosphor Terminal",
        description: "Horizontal scanline etching in glowing P1 green phosphor",
        tags: &["crt", "retro", "dark", "scanlines"],
        algorithm: Algorithm::HorizontalLines,
        palette: PresetPalette::Builtin("phosphor-green"),
        color_space: ColorSpace::Oklab,
        serpentine: false,
        strength: 1.0,
        pixel_size: 2,
        pattern_scale: 1,
        contrast: 0.12,
        sharpen: 0.2,
        auto_levels: true,
        grayscale: true,
    },
    Preset {
        id: "blueprint",
        name: "Architectural Blueprint",
        description: "Sharp Atkinson cyanotype blueprint in Prussian blue and chalk white",
        tags: &["technical", "duotone", "editorial", "blue"],
        algorithm: Algorithm::Atkinson,
        palette: PresetPalette::Builtin("blueprint"),
        color_space: ColorSpace::Oklab,
        serpentine: false,
        strength: 1.0,
        pixel_size: 1,
        pattern_scale: 1,
        contrast: 0.1,
        sharpen: 0.7,
        auto_levels: true,
        grayscale: true,
    },
    Preset {
        id: "halftone-zine",
        name: "Halftone Punk Zine",
        description: "Bold 45-degree clustered-dot halftone screen on high-contrast newsprint",
        tags: &["halftone", "print", "zine", "bold"],
        algorithm: Algorithm::Halftone,
        palette: PresetPalette::Builtin("newsprint"),
        color_space: ColorSpace::Oklab,
        serpentine: false,
        strength: 1.05,
        pixel_size: 1,
        pattern_scale: 1,
        contrast: 0.18,
        sharpen: 0.4,
        auto_levels: true,
        grayscale: true,
    },
    Preset {
        id: "woodcut",
        name: "Woodcut Engraving",
        description: "Diagonal engraving lines in Japanese sumi ink, vermilion, and washi paper",
        tags: &["engraving", "print", "artistic", "lines"],
        algorithm: Algorithm::DiagonalLines,
        palette: PresetPalette::Builtin("vermilion-paper"),
        color_space: ColorSpace::Oklab,
        serpentine: false,
        strength: 1.0,
        pixel_size: 1,
        pattern_scale: 1,
        contrast: 0.14,
        sharpen: 0.4,
        auto_levels: true,
        grayscale: false,
    },
    Preset {
        id: "lo-fi-photo",
        name: "Lo-Fi Adaptive Photo",
        description: "Automatically extracts 8 perceptual colors from the image and dithers cleanly",
        tags: &["photo", "adaptive", "color", "web"],
        algorithm: Algorithm::FloydSteinberg,
        palette: PresetPalette::Adaptive(8),
        color_space: ColorSpace::Oklab,
        serpentine: true,
        strength: 0.95,
        pixel_size: 2,
        pattern_scale: 1,
        contrast: 0.05,
        sharpen: 0.2,
        auto_levels: false,
        grayscale: false,
    },
    Preset {
        id: "vapor-mesh",
        name: "Vaporwave Synth Mesh",
        description: "Neon cyber-magenta Bayer 4x4 crosshatch built for hero gradients and dark UIs",
        tags: &["neon", "web", "dark", "gradient"],
        algorithm: Algorithm::Bayer4,
        palette: PresetPalette::Builtin("cyber-magenta"),
        color_space: ColorSpace::Oklab,
        serpentine: false,
        strength: 1.0,
        pixel_size: 2,
        pattern_scale: 1,
        contrast: 0.08,
        sharpen: 0.0,
        auto_levels: false,
        grayscale: false,
    },
    Preset {
        id: "pico8",
        name: "PICO-8 Fantasy Console",
        description: "Vibrant 16-color PICO-8 pixel art with Bayer 4x4 dithering",
        tags: &["retro", "pixel-art", "16-color", "vibrant"],
        algorithm: Algorithm::Bayer4,
        palette: PresetPalette::Builtin("pico8"),
        color_space: ColorSpace::Oklab,
        serpentine: false,
        strength: 0.95,
        pixel_size: 3,
        pattern_scale: 1,
        contrast: 0.08,
        sharpen: 0.2,
        auto_levels: false,
        grayscale: false,
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::Rgb;
    use tempfile::TempDir;

    #[test]
    fn slug_validation_blocks_path_traversal() {
        assert_eq!(
            normalize_preset_slug("Brand_Hero 1").unwrap(),
            "brand-hero-1"
        );
        assert!(normalize_preset_slug("../escape").is_err());
        assert!(normalize_preset_slug("a/b").is_err());
        assert!(normalize_preset_slug("").is_err());
        assert!(normalize_preset_slug("-leading").is_err());
    }

    #[test]
    fn save_load_and_delete_local_preset_round_trip() {
        let dir = TempDir::new().unwrap();
        let mut recipe = DitherRecipe::default();
        recipe.dither.algorithm = Algorithm::BlueNoise;
        recipe.palette =
            PaletteSpec::Colors(vec![Rgb::from_u32(0x111111), Rgb::from_u32(0xF5EFE6)]);

        let (saved, path, _) = save_local_preset(SavePresetRequest {
            slug: "brand-stipple",
            title: Some("Brand Stipple"),
            description: Some("Hero texture"),
            tags: vec!["brand".into(), "web".into()],
            recipe: recipe.clone(),
            scope: PresetScope::Project,
            custom_dir: Some(dir.path()),
            overwrite: OverwritePolicy::NoClobber,
        })
        .unwrap();
        assert!(path.exists());
        assert_eq!(saved.id, "brand-stipple");

        // No-clobber rejects duplicate save.
        assert!(
            save_local_preset(SavePresetRequest {
                slug: "brand-stipple",
                title: None,
                description: None,
                tags: vec![],
                recipe,
                scope: PresetScope::Project,
                custom_dir: Some(dir.path()),
                overwrite: OverwritePolicy::NoClobber,
            })
            .is_err()
        );

        let loaded = load_saved_preset_file(&path).unwrap();
        assert_eq!(loaded.recipe.dither.algorithm, Algorithm::BlueNoise);
        assert_eq!(loaded.name, "Brand Stipple");

        let (deleted_id, deleted_path) =
            delete_local_preset("brand-stipple", PresetScope::Project, Some(dir.path())).unwrap();
        assert_eq!(deleted_id, "brand-stipple");
        assert!(!deleted_path.exists());
    }
}
