use anyhow::Result;
use clap::{Parser, Subcommand};

use crate::client::Client;
use crate::widgets::{PodMetaWidget, TableWidget};
use crate::draw;

#[derive(Subcommand, Debug)]
enum Command {
    /// List pods
    List,
    /// Describe pod
    Describe { name: String },
}

#[derive(Parser, Debug)]
pub struct Opts {
    #[command(subcommand)]
    command: Command,
}

pub async fn handle(client: Client, opts: Opts) -> Result<()> {
    match opts.command {
        Command::List => {
            let pods = client.pods().list().await?;
            draw::render_inline(TableWidget(&pods))?;
        }
        Command::Describe { name } => {
            let pod = client.pods().by_name(&name).describe().await?;
            draw::render_inline(PodMetaWidget(&pod))?;
        }
    }

    Ok(())
}
