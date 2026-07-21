use anyhow::Result;
use clap::{Parser, Subcommand};

mod api;
mod cmd;
mod draw;
mod widgets;

use api::{ApiKey, Client};

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Opts {
    /// Brainpod API endpoint to use
    #[arg(
        long,
        env = "BRAINPOD_API_ENDPOINT",
        default_value = "https://api.brainpod.io"
    )]
    endpoint: String,

    #[arg(long, env = "BRAINPOD_API_KEY")]
    api_key: ApiKey,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    Pod(cmd::pod::Opts),
    List(cmd::list::Opts),
}

async fn handle(client: Client, command: Command) -> Result<()> {
    match command {
        Command::Pod(opts) => cmd::pod::handle(client, opts).await,
        Command::List(opts) => cmd::list::handle(client, opts).await,
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let opts = Opts::parse();
    let client = Client::try_new(&opts.endpoint, &opts.api_key)?;

    handle(client, opts.command).await
}
