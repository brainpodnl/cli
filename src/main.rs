use std::process::ExitCode;

use anyhow::{Result, anyhow};
use clap::Parser;
use serde_json::{Value, json};

mod client;
mod cmd;
mod config;
mod output;

use client::{ApiError, Client};
use cmd::Command;
use config::{Config, DEFAULT_ENDPOINT};

const UPGRADE_URL: &str = "https://brainpod.io/onboarding?upgrade=1";

#[derive(Debug, Parser)]
#[command(
    name = "brainpod",
    version,
    about = "Manage Brainpod deployments and resources"
)]
struct Opts {
    /// Emit JSON instead of line-oriented text (NDJSON for event watches)
    #[arg(long, global = true)]
    json: bool,

    /// Brainpod API endpoint (overrides environment and config)
    #[arg(long, global = true)]
    endpoint: Option<String>,

    /// Brainpod API key (overrides environment and config)
    #[arg(long, global = true)]
    api_key: Option<String>,

    /// Default pod for pod-scoped commands
    #[arg(long, global = true)]
    pod: Option<String>,

    #[command(subcommand)]
    command: Command,
}

#[tokio::main]
async fn main() -> ExitCode {
    let opts = Opts::parse();
    let json_output = opts.json;

    match run(opts).await {
        Ok(value) => match output::write(value, json_output).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) if is_broken_pipe(&error) => ExitCode::SUCCESS,
            Err(error) => {
                write_error(&error, json_output);
                ExitCode::FAILURE
            }
        },
        Err(error) => {
            write_error(&error, json_output);
            ExitCode::FAILURE
        }
    }
}

async fn run(opts: Opts) -> Result<output::CommandOutput> {
    let config_path = Config::path()?;
    let mut config = Config::load(&config_path)?;

    let endpoint = match opts
        .endpoint
        .or_else(|| environment("BRAINPOD_API_ENDPOINT"))
        .or_else(|| config.endpoint.clone())
    {
        Some(endpoint) => endpoint,
        None => DEFAULT_ENDPOINT.to_owned(),
    };
    let api_key = opts
        .api_key
        .or_else(|| environment("BRAINPOD_API_KEY"))
        .or_else(|| config.api_key.clone());
    let pod = opts
        .pod
        .or_else(|| environment("BRAINPOD_POD"))
        .or_else(|| config.pod.clone());

    let client = if cmd::needs_client(&opts.command) {
        let api_key = api_key.ok_or_else(|| {
            anyhow!(
                "API key is required; pass --api-key, set BRAINPOD_API_KEY, or run `brainpod config set api-key <key>`"
            )
        })?;
        Some(Client::try_new(&endpoint, &api_key)?)
    } else {
        None
    };

    cmd::handle(
        opts.command,
        client.as_ref(),
        pod.as_deref(),
        &mut config,
        &config_path,
    )
    .await
}

fn environment(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn write_error(error: &anyhow::Error, json_output: bool) {
    let api_error = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<ApiError>());

    if json_output {
        let value = match api_error {
            Some(api_error) => api_error_json(api_error),
            None => json!({
                "error": {
                    "code": "CLI_ERROR",
                    "message": format!("{error:#}"),
                }
            }),
        };
        match serde_json::to_string(&value) {
            Ok(value) => eprintln!("{value}"),
            Err(_) => eprintln!(
                "{{\"error\":{{\"code\":\"CLI_ERROR\",\"message\":\"failed to serialize error\"}}}}"
            ),
        }
    } else {
        eprintln!("error: {error:#}");
        if api_error.is_some_and(ApiError::is_account_limit_error) {
            eprintln!("Add your payment details to increase your account limits: {UPGRADE_URL}");
        }
    }
}

fn api_error_json(api_error: &ApiError) -> Value {
    let mut body = api_error.body.clone();
    if let Some(error) = body.get_mut("error").and_then(Value::as_object_mut) {
        error.insert("httpStatus".to_owned(), json!(api_error.status.as_u16()));
        if api_error.is_account_limit_error() {
            error.insert(
                "resolution".to_owned(),
                json!("Add your payment details to increase your account limits"),
            );
            error.insert("upgradeUrl".to_owned(), json!(UPGRADE_URL));
        }
        body
    } else {
        json!({
            "error": {
                "code": "API_ERROR",
                "message": api_error.to_string(),
                "httpStatus": api_error.status.as_u16(),
                "response": body,
            }
        })
    }
}

fn is_broken_pipe(error: &anyhow::Error) -> bool {
    error
        .chain()
        .filter_map(|cause| cause.downcast_ref::<std::io::Error>())
        .any(|error| error.kind() == std::io::ErrorKind::BrokenPipe)
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::{Command, Opts};

    #[test]
    fn parses_event_watch() {
        let opts = Opts::try_parse_from([
            "brainpod",
            "events",
            "--watch",
            "--kind",
            "platform",
            "--resource",
            "urn:brain:app:default:worker",
            "--duration",
            "20",
            "--last-event-id",
            "event-1",
        ])
        .unwrap();

        assert!(matches!(opts.command, Command::Events(_)));
    }

    #[test]
    fn parses_events_without_kind() {
        let opts = Opts::try_parse_from([
            "brainpod",
            "events",
            "--resource",
            "urn:brain:route:default:public",
        ])
        .unwrap();

        assert!(matches!(opts.command, Command::Events(_)));
    }

    #[test]
    fn rejects_invalid_event_resource_urn() {
        let result = Opts::try_parse_from(["brainpod", "events", "--resource", "api"]);

        assert!(result.is_err());
    }

    #[test]
    fn rejects_invalid_event_watch_duration() {
        let result = Opts::try_parse_from([
            "brainpod",
            "events",
            "--watch",
            "--kind",
            "app",
            "--resource",
            "urn:brain:app:default:api",
            "--duration",
            "21",
        ]);

        assert!(result.is_err());
    }

    #[test]
    fn rejects_watch_options_without_watch() {
        let result = Opts::try_parse_from([
            "brainpod",
            "events",
            "--kind",
            "app",
            "--resource",
            "urn:brain:app:default:api",
            "--duration",
            "10",
        ]);

        assert!(result.is_err());
    }
}
