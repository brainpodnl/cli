use std::str::FromStr;

use anyhow::{Context, Result};
use brainpod_core::resource::{self, ResourceKind};
use clap::{Parser, Subcommand};

use crate::{api::Client };
use crate::widgets::ResourceTable;

fn parse_kind(s: &str) -> Result<ResourceKind> {
    ResourceKind::from_lowercase_str(&s.to_lowercase()).context("invalid resource kind")
}

#[derive(Parser, Debug)]
pub struct Opts {
    #[arg(value_parser = parse_kind)]
    kind: ResourceKind,
}

// #[derive(Subcommand, Debug)]
// enum Command {
//     Login {
//         #[arg(long, env = "BRAINPOD_API_KEY")]
//         api_key: String,
//     },
// }
//
pub async fn handle(client: Client, opts: Opts) -> Result<()> {
    let resources = client.list_resources(&opts.kind).await?;
    let widget = ResourceTable(&resources);
    crate::draw::render_inline(widget)?;

    Ok(())
}
