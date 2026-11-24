//! The `dith list` command: algorithms, palettes, presets, generators, and formats.

use anyhow::Result;
use clap::{Args, ValueEnum};

use crate::dither::Algorithm;
use crate::generate::Pattern;
use crate::palette::list_builtin_palettes;
use crate::preset::list_all_presets;
use crate::recipe::{build_capabilities, paper_sizes};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ListKind {
    Algorithms,
    Palettes,
    Presets,
    Generators,
    Formats,
    Papers,
    AnimFormats,
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

pub(crate) fn handle_list(args: &ListArgs, as_json: bool) -> Result<()> {
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
            let formats: Vec<_> = caps
                .output_formats
                .into_iter()
                .chain(caps.plot_formats)
                .collect();
            if as_json {
                println!("{}", serde_json::to_string_pretty(&formats)?);
            } else {
                for f in formats {
                    println!("{:<10} {}", f.id, f.description);
                }
            }
        }
        ListKind::Papers => {
            let items = paper_sizes();
            if as_json {
                println!("{}", serde_json::to_string_pretty(&items)?);
            } else {
                for p in items {
                    println!("{:<10} {} x {} mm", p.id, p.width_mm, p.height_mm);
                }
            }
        }
        ListKind::AnimFormats => {
            let caps = build_capabilities();
            if as_json {
                println!("{}", serde_json::to_string_pretty(&caps.animation_formats)?);
            } else {
                for f in caps.animation_formats {
                    println!("{:<12} {}", f.id, f.description);
                }
            }
        }
    }
    Ok(())
}
