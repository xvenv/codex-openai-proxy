mod app;
mod auth_cmd;
mod backend;
mod config;
mod execution;
mod http;
mod models;
mod routing;

use anyhow::Result;
use clap::{Parser, Subcommand};

use crate::config::Args;

/// Codex OpenAI Proxy — multi-account Codex backend proxy.
#[derive(Parser, Debug)]
#[command(author, version, about)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Proxy server options (used when no subcommand is given)
    #[command(flatten)]
    args: Args,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Manage Codex accounts
    Auth {
        #[command(subcommand)]
        action: AuthAction,
    },
}

#[derive(Subcommand, Debug)]
enum AuthAction {
    /// Add or update a Codex account
    Login {
        /// Human-readable label for this account (e.g. "work", "personal")
        #[arg(long, default_value = "")]
        label: String,
    },
    /// List all saved accounts
    List,
    /// Remove an account by label
    Remove {
        /// Label of the account to remove
        #[arg(long)]
        label: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Command::Auth { action }) => {
            // Auth commands only need the auth_path, resolve it from args/config.
            let auth_path = resolve_auth_path(&cli.args);
            match action {
                AuthAction::Login { label } => auth_cmd::run_login(&label, &auth_path).await?,
                AuthAction::List => auth_cmd::run_list(&auth_path)?,
                AuthAction::Remove { label } => auth_cmd::run_remove(&label, &auth_path)?,
            }
        }
        None => {
            app::run(cli.args).await?;
        }
    }

    Ok(())
}

/// Resolve the auth path from CLI args without fully loading the config
/// (avoids requiring a config.json to exist for auth subcommands).
fn resolve_auth_path(args: &Args) -> String {
    if let Some(ref path) = args.auth_path {
        return expand_home(path);
    }
    expand_home("~/.config/codex-proxy/auth.json")
}

fn expand_home(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return format!("{home}/{rest}");
        }
    }
    path.to_string()
}
