use anyhow::Result;
use clap::{Parser, Subcommand};

use crate::api::Client;
use crate::widgets::TableWidget;

#[derive(Subcommand, Debug)]
enum Command {
    /// List pods
    List,
}

#[derive(Parser, Debug)]
pub struct Opts {
    #[command(subcommand)]
    command: Command,
}

pub async fn handle(client: Client, _opts: Opts) -> Result<()> {
    let pods = client.list_pods().await?;
    crate::draw::render_inline(TableWidget(&pods))?;

    Ok(())
}
