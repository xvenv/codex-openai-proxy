mod app;
mod backend;
mod config;
mod execution;
mod http;
mod models;
mod routing;

use anyhow::Result;
use clap::Parser;

use crate::config::Args;

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    app::run(args).await
}
