use anyhow::Result;
use clap::{Parser, Subcommand};

/// Manage credentials and authentication to the Brainpod platform
#[derive(Parser, Debug)]
pub struct Opts {
    #[command(subcommand)]
    command: Command,
}

fn login() -> Result<()> {

    Ok(())
}

#[derive(Subcommand, Debug)]
enum Command {
    Login {
        #[arg(short, long, env = "BRAINPOD_USERNAME")]
        username: String,

        #[arg(short, long, env = "BRAINPOD_PASSWORD")]
        password: String,
    },
}

pub fn handle(opts: Opts) -> Result<()> {
    match opts.command {
        Command::Login {} => Ok(()),
    }
}
