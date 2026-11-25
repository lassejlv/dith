//! The `dith preset` command: save, list, show, and delete persistent local presets.

use std::path::PathBuf;

use anyhow::Result;
use clap::{Args, Subcommand, ValueEnum};
use serde::Serialize;

use crate::cli::DitherFlags;
use crate::preset::{
    OverwritePolicy, PresetScope, PresetSource, SavePresetRequest, delete_local_preset,
    list_all_presets, resolve_preset, save_local_preset,
};
use crate::recipe::{DitherRecipe, RecipeError};

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

pub(crate) fn handle_preset(args: &PresetArgs, as_json: bool) -> Result<()> {
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
