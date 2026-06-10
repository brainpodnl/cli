use anyhow::Result;
use clap::{Parser, Subcommand};

mod cmd;

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

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    Auth(cmd::auth::Opts),
}

fn main() -> Result<()> {
    let opts = Opts::parse();
    dbg!(&opts);

    Ok(())
}
