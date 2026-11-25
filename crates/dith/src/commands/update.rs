//! The `dith update` command: self-update from GitHub Releases.

use anyhow::Result;
use clap::Args;

use crate::update::{UpdateOptions, run_update};

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

pub(crate) fn handle_update(args: &UpdateArgs, as_json: bool) -> Result<()> {
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
