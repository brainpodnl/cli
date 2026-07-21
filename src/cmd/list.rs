use std::str::FromStr;

use anyhow::Result;
use brainpod_core::resource::ResourceKind;
use clap::Parser;

use crate::api::Client;
use crate::widgets::TableWidget;

#[derive(Parser, Debug)]
pub struct Opts {
    #[arg(value_parser = ResourceKind::from_str)]
    kind: ResourceKind,
}

pub async fn handle(client: Client, opts: Opts) -> Result<()> {
    let resources = client.list_resources(&opts.kind).await?;
    let widget = TableWidget(&resources);
    crate::draw::render_inline(widget)?;

    Ok(())
}
