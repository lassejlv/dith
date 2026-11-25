//! Binary entrypoint for `dith`, with structured JSON error envelopes for agents.

use std::process::ExitCode;

use clap::Parser;
use serde::Serialize;

mod cli;
mod commands;
mod update;
mod viewer;

use dith_core::RecipeError;

use crate::cli::{Cli, dispatch};

#[derive(Serialize)]
struct ErrorEnvelope {
    status: &'static str,
    code: &'static str,
    message: String,
    suggestion: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    valid_values: Option<Vec<String>>,
}

fn main() -> ExitCode {
    let raw_args: Vec<String> = std::env::args().collect();
    let wants_json = raw_args.iter().any(|a| a == "--json");

    let cli = match Cli::try_parse_from(&raw_args) {
        Ok(parsed) => parsed,
        Err(err) => {
            if matches!(
                err.kind(),
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
            ) {
                let _ = err.print();
                return ExitCode::SUCCESS;
            }
            if wants_json {
                let env = ErrorEnvelope {
                    status: "error",
                    code: "CLI_USAGE_ERROR",
                    message: err.to_string().trim().to_string(),
                    suggestion: "Run `dith capabilities` or `dith --help` to inspect valid subcommands and flags."
                        .to_string(),
                    valid_values: None,
                };
                if let Ok(text) = serde_json::to_string_pretty(&env) {
                    eprintln!("{text}");
                }
            } else {
                let _ = err.print();
            }
            return ExitCode::from(2);
        }
    };

    let json_mode = cli.json;
    match dispatch(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            let (exit_code, env) = if let Some(re) = err.downcast_ref::<RecipeError>() {
                (
                    ExitCode::from(2),
                    ErrorEnvelope {
                        status: "error",
                        code: re.code(),
                        message: re.to_string(),
                        suggestion: re.suggestion(),
                        valid_values: re.valid_values(),
                    },
                )
            } else {
                (
                    ExitCode::from(1),
                    ErrorEnvelope {
                        status: "error",
                        code: "RUNTIME_ERROR",
                        message: format!("{err:#}"),
                        suggestion: "Verify input/output paths and permissions, or retry with --dry-run --json."
                            .to_string(),
                        valid_values: None,
                    },
                )
            };

            if json_mode {
                if let Ok(text) = serde_json::to_string_pretty(&env) {
                    eprintln!("{text}");
                }
            } else {
                eprintln!("error [{}]: {}", env.code, env.message);
                eprintln!("hint: {}", env.suggestion);
            }
            exit_code
        }
    }
}
