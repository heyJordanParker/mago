//! The `mago server` command: starts, stops, inspects and verifies the analysis server that keeps
//! every worktree's analysis warm for `mago analyze`.

use std::process::ExitCode;

use clap::ColorChoice;
use clap::Parser;
use clap::Subcommand;

use crate::config::Configuration;
use crate::error::Error;
use crate::server;
use crate::server::Check;
use crate::utils::should_use_colors;

/// Manage the analysis server that answers `mago analyze`.
///
/// One server runs per Mago build and keeps the analysis of every worktree it served warm, so a
/// check re-analyzes only what changed. `mago analyze` starts it on first use.
#[derive(Parser, Debug)]
#[command(name = "server")]
pub struct ServerCommand {
    #[command(subcommand)]
    action: ServerAction,
}

#[derive(Subcommand, Debug)]
enum ServerAction {
    /// Start the analysis server of this build unless one runs.
    Start {
        /// Run the server in this process instead of starting it in the background.
        #[arg(long, hide = true)]
        foreground: bool,
    },
    /// Stop the analysis server after it persists every worktree's analysis.
    Stop,
    /// Print the analysis server's process and the worktrees it keeps warm, as JSON.
    Status,
    /// Analyze this worktree from scratch and compare the result with the server's warm state.
    Verify,
}

impl ServerCommand {
    pub fn execute(self, configuration: Configuration, color_choice: ColorChoice) -> Result<ExitCode, Error> {
        match self.action {
            ServerAction::Start { foreground: true } => server::run().map(|()| ExitCode::SUCCESS),
            ServerAction::Start { foreground: false } => server::start().map(|()| ExitCode::SUCCESS),
            ServerAction::Stop => {
                if !server::stop()? {
                    tracing::info!("No analysis server runs for this build.");
                }

                Ok(ExitCode::SUCCESS)
            }
            ServerAction::Status => match server::status()? {
                Some(status) => {
                    println!("{}", serde_json::to_string_pretty(&status)?);
                    Ok(ExitCode::SUCCESS)
                }
                None => {
                    tracing::info!("No analysis server runs for this build.");
                    Ok(ExitCode::FAILURE)
                }
            },
            ServerAction::Verify => {
                let colors = should_use_colors(color_choice);
                let verification = server::verify(Check::new(configuration, colors, true, Vec::new()))?;

                if verification.only_warm.is_empty() && verification.only_fresh.is_empty() {
                    println!("The warm analysis equals a fresh one: {} issue(s).", verification.issues);
                    return Ok(ExitCode::SUCCESS);
                }

                println!("The warm analysis differs from a fresh one.");
                for issue in &verification.only_warm {
                    println!("only warm:  {issue}");
                }
                for issue in &verification.only_fresh {
                    println!("only fresh: {issue}");
                }

                Ok(ExitCode::FAILURE)
            }
        }
    }
}
