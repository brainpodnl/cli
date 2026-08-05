use std::process::ExitCode;

use anyhow::{Result, anyhow};
use clap::{CommandFactory as _, Parser};
use serde_json::{Value, json};

mod auth;
mod client;
mod cmd;
mod config;
mod describe;
mod image;
mod openapi;
mod output;

use client::{ApiError, Client};
use cmd::Command;
use config::{Config, DEFAULT_DASHBOARD_ENDPOINT, DEFAULT_ENDPOINT, DEFAULT_REGISTRY_ENDPOINT};

const UPGRADE_URL: &str = "https://brainpod.io/onboarding?upgrade=1";

#[derive(Debug, Parser)]
#[command(
    name = "brainpod",
    version,
    about = "Manage Brainpod deployments, images, and resources",
    after_help = "For machine-readable command metadata, run `brainpod describe --json`."
)]
pub(crate) struct Opts {
    /// Emit JSON instead of line-oriented text (NDJSON for event watches)
    #[arg(long, global = true)]
    json: bool,

    /// Brainpod API endpoint (overrides environment and config)
    #[arg(long, global = true)]
    endpoint: Option<String>,

    /// Brainpod API token (overrides environment and config)
    #[arg(long, global = true)]
    api_token: Option<String>,

    /// Brainpod registry endpoint (overrides environment and config)
    #[arg(long, global = true)]
    registry_endpoint: Option<String>,

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
    if let Command::Describe(args) = &opts.command {
        if openapi::is_resource_path(&args.command) {
            let resource_description =
                openapi::describe(&args.command, opts.endpoint.as_deref()).await?;
            if args.command.len() == 1 {
                let mut description = describe::generate(Opts::command(), &args.command)?;
                description
                    .as_object_mut()
                    .ok_or_else(|| anyhow!("CLI description is not a JSON object"))?
                    .insert("resourceSchemas".to_owned(), resource_description);
                return Ok(output::CommandOutput::new(
                    description,
                    output::View::Describe,
                ));
            }

            return Ok(output::CommandOutput::new(
                resource_description,
                output::View::ResourceSchema,
            ));
        }

        return Ok(output::CommandOutput::new(
            describe::generate(Opts::command(), &args.command)?,
            output::View::Describe,
        ));
    }

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
    let api_token = opts
        .api_token
        .or_else(|| environment("BRAINPOD_API_TOKEN"))
        .or_else(|| config.api_token.clone());
    let dashboard_endpoint = environment("BRAINPOD_DASHBOARD_ENDPOINT")
        .unwrap_or_else(|| DEFAULT_DASHBOARD_ENDPOINT.to_owned());
    let registry_endpoint = opts
        .registry_endpoint
        .or_else(|| environment("BRAINPOD_REGISTRY_ENDPOINT"))
        .or_else(|| config.registry_endpoint.clone())
        .unwrap_or_else(|| DEFAULT_REGISTRY_ENDPOINT.to_owned());
    let pod = opts
        .pod
        .or_else(|| environment("BRAINPOD_POD"))
        .or_else(|| config.pod.clone());

    if cmd::needs_api_token(&opts.command) && api_token.is_none() {
        return Err(anyhow!(
            "API token is required; run `brainpod login`, pass --api-token, set BRAINPOD_API_TOKEN, or run `brainpod config set api-token <token>`"
        ));
    }
    let client = if cmd::needs_client(&opts.command) {
        Some(Client::try_new(
            &endpoint,
            api_token
                .as_deref()
                .ok_or_else(|| anyhow!("API token is required"))?,
        )?)
    } else {
        None
    };

    cmd::handle(
        opts.command,
        client.as_ref(),
        &endpoint,
        &dashboard_endpoint,
        pod.as_deref(),
        api_token.as_deref(),
        &registry_endpoint,
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
    fn parses_describe_path() {
        let opts = Opts::try_parse_from(["brainpod", "describe", "resource", "create"]).unwrap();

        let Command::Describe(args) = opts.command else {
            panic!("expected describe command");
        };
        assert_eq!(
            args.command,
            vec!["resource".to_owned(), "create".to_owned()]
        );
    }

    #[test]
    fn parses_api_token() {
        let opts = Opts::try_parse_from([
            "brainpod",
            "--api-token",
            "brain_example",
            "whoami",
        ])
        .unwrap();

        assert_eq!(opts.api_token.as_deref(), Some("brain_example"));
    }

    #[test]
    fn rejects_api_key_flag() {
        let result = Opts::try_parse_from([
            "brainpod",
            "--api-key",
            "brain_example",
            "whoami",
        ]);

        assert!(result.is_err());
    }

    #[test]
    fn parses_cluster_list() {
        let opts = Opts::try_parse_from(["brainpod", "cluster", "list"]).unwrap();

        assert!(matches!(opts.command, Command::Cluster(_)));
        assert!(super::cmd::needs_client(&opts.command));
    }

    #[test]
    fn parses_image_build() {
        let opts = Opts::try_parse_from([
            "brainpod",
            "--pod",
            "my-pod",
            "image",
            "build",
            "api",
            "./service",
            "--tag",
            "v1",
            "--builder",
            "dockerfile",
            "--output",
            "api.oci",
        ])
        .unwrap();

        assert!(matches!(opts.command, Command::Image(_)));
        assert!(!super::cmd::needs_client(&opts.command));
    }

    #[test]
    fn parses_image_list() {
        let opts = Opts::try_parse_from([
            "brainpod",
            "--pod",
            "my-pod",
            "image",
            "list",
            "--search",
            "worker",
            "--visibility",
            "pod",
            "--limit",
            "10",
            "--offset",
            "20",
        ])
        .unwrap();

        assert!(matches!(opts.command, Command::Image(_)));
        assert!(super::cmd::needs_client(&opts.command));
    }

    #[test]
    fn parses_image_inspect() {
        let opts = Opts::try_parse_from([
            "brainpod",
            "image",
            "inspect",
            "ubuntu",
            "latest",
            "--visibility",
            "public",
        ])
        .unwrap();

        assert!(matches!(opts.command, Command::Image(_)));
    }

    #[test]
    fn parses_image_inspect_with_default_visibility() {
        let opts = Opts::try_parse_from(["brainpod", "image", "inspect", "api", "latest"])
            .unwrap();

        assert!(matches!(opts.command, Command::Image(_)));
    }

    #[test]
    fn rejects_invalid_image_list_limit() {
        let result = Opts::try_parse_from([
            "brainpod",
            "image",
            "list",
            "--limit",
            "101",
        ]);

        assert!(result.is_err());
    }

    #[test]
    fn rejects_image_platform_override() {
        let result = Opts::try_parse_from([
            "brainpod",
            "image",
            "build",
            "api",
            "--platform",
            "linux/arm64",
        ]);

        assert!(result.is_err());
    }

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
