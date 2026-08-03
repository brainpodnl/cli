use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use clap::{Args, Subcommand, ValueEnum};
use serde_json::{Value, json};

use crate::client::Client;
use crate::config::Config;
use crate::output::{CommandOutput, View};

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Manage local CLI configuration
    Config(ConfigArgs),
    /// Show the authenticated user
    Whoami,
    /// Inspect pods
    Pod(PodArgs),
    /// Inspect pod revisions
    Revision(RevisionArgs),
    /// Create and manage pod resources
    Resource(ResourceArgs),
    /// Deploy the current draft revision
    Deploy(DeployArgs),
    /// Redeploy the deployed revision
    Redeploy,
    /// Query pod events
    Events(EventsArgs),
}

#[derive(Debug, Args)]
pub struct ConfigArgs {
    #[command(subcommand)]
    command: ConfigCommand,
}

#[derive(Debug, Subcommand)]
enum ConfigCommand {
    /// Show configuration without revealing the API key
    Show,
    /// Print the configuration file path
    Path,
    /// Set a configuration value
    Set { key: ConfigKey, value: String },
    /// Remove a configuration value
    Unset { key: ConfigKey },
}

#[derive(Clone, Debug, ValueEnum)]
enum ConfigKey {
    Endpoint,
    ApiKey,
    Pod,
}

#[derive(Debug, Args)]
pub struct PodArgs {
    #[command(subcommand)]
    command: PodCommand,
}

#[derive(Debug, Subcommand)]
enum PodCommand {
    /// List pods
    List,
    /// Get a pod
    Get { pod: String },
}

#[derive(Debug, Args)]
pub struct RevisionArgs {
    #[command(subcommand)]
    command: RevisionCommand,
}

#[derive(Debug, Subcommand)]
enum RevisionCommand {
    /// List revisions
    List {
        #[arg(long)]
        cursor: Option<String>,
        #[arg(long, default_value_t = 10, value_parser = clap::value_parser!(u8).range(1..=50))]
        limit: u8,
    },
    /// Get a revision and its resources
    Get { revision: String },
    /// Compare a revision with its parent or another revision
    Diff {
        revision: String,
        #[arg(long)]
        base: Option<String>,
    },
}

#[derive(Debug, Args)]
pub struct ResourceArgs {
    #[command(subcommand)]
    command: ResourceCommand,
}

#[derive(Debug, Subcommand)]
enum ResourceCommand {
    /// List resources in the current or a historical revision
    List {
        #[arg(long, conflicts_with = "at")]
        revision: Option<String>,
        #[arg(long, conflicts_with = "revision")]
        at: Option<String>,
    },
    /// Get one resource
    Get {
        kind: ResourceKind,
        name: String,
        #[arg(long, conflicts_with = "at")]
        revision: Option<String>,
        #[arg(long, conflicts_with = "revision")]
        at: Option<String>,
    },
    /// Create one resource or an array of resources from JSON
    Create {
        #[arg(short, long, value_name = "PATH")]
        file: PathBuf,
        #[arg(long)]
        dry_run: bool,
    },
    /// Replace a resource from a JSON document
    Replace {
        kind: ResourceKind,
        name: String,
        #[arg(short, long, value_name = "PATH")]
        file: PathBuf,
    },
    /// Delete a resource
    Delete { kind: ResourceKind, name: String },
}

#[derive(Clone, Debug, ValueEnum)]
enum ResourceKind {
    App,
    Config,
    Route,
    Postgres,
    #[value(name = "mariadb")]
    MariaDb,
    Valkey,
    Disk,
}

impl ResourceKind {
    fn as_api_str(&self) -> &'static str {
        match self {
            Self::App => "App",
            Self::Config => "Config",
            Self::Route => "Route",
            Self::Postgres => "Postgres",
            Self::MariaDb => "MariaDB",
            Self::Valkey => "Valkey",
            Self::Disk => "Disk",
        }
    }
}

#[derive(Debug, Args)]
pub struct DeployArgs {
    /// Human-readable description of this deployment
    #[arg(long)]
    summary: Option<String>,
}

#[derive(Debug, Args)]
pub struct EventsArgs {
    /// Event source
    #[arg(long)]
    kind: EventKind,
    /// Resource name
    #[arg(long)]
    resource: String,
    /// Filter app events by level
    #[arg(long)]
    level: Option<EventLevel>,
    /// Full-text search query
    #[arg(long)]
    search: Option<String>,
    /// Time window
    #[arg(long, default_value = "15m")]
    range: EventRange,
    /// Pagination cursor from the previous response
    #[arg(long)]
    cursor: Option<String>,
}

#[derive(Clone, Debug, ValueEnum)]
enum EventKind {
    App,
    #[value(name = "http-access")]
    HttpAccess,
    K8s,
}

impl EventKind {
    fn as_api_str(&self) -> &'static str {
        match self {
            Self::App => "app",
            Self::HttpAccess => "httpAccess",
            Self::K8s => "k8s",
        }
    }
}

#[derive(Clone, Debug, ValueEnum)]
enum EventLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl EventLevel {
    fn as_api_str(&self) -> &'static str {
        match self {
            Self::Trace => "trace",
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }
}

#[derive(Clone, Debug, ValueEnum)]
enum EventRange {
    #[value(name = "5m")]
    FiveMinutes,
    #[value(name = "15m")]
    FifteenMinutes,
    #[value(name = "30m")]
    ThirtyMinutes,
    #[value(name = "1h")]
    OneHour,
    #[value(name = "24h")]
    OneDay,
    #[value(name = "7d")]
    SevenDays,
}

impl EventRange {
    fn as_api_str(&self) -> &'static str {
        match self {
            Self::FiveMinutes => "5m",
            Self::FifteenMinutes => "15m",
            Self::ThirtyMinutes => "30m",
            Self::OneHour => "1h",
            Self::OneDay => "24h",
            Self::SevenDays => "7d",
        }
    }
}

pub fn needs_client(command: &Command) -> bool {
    !matches!(command, Command::Config(_))
}

pub async fn handle(
    command: Command,
    client: Option<&Client>,
    pod: Option<&str>,
    config: &mut Config,
    config_path: &Path,
) -> Result<CommandOutput> {
    match command {
        Command::Config(args) => handle_config(args, config, config_path),
        Command::Whoami => Ok(CommandOutput::new(
            client_required(client)?.get(&["v1", "me"], &[]).await?,
            View::Whoami,
        )),
        Command::Pod(args) => handle_pod(client_required(client)?, args).await,
        Command::Revision(args) => {
            handle_revision(client_required(client)?, pod_required(pod)?, args).await
        }
        Command::Resource(args) => {
            handle_resource(client_required(client)?, pod_required(pod)?, args).await
        }
        Command::Deploy(args) => {
            let body = match args.summary {
                Some(summary) => json!({ "summary": summary }),
                None => json!({}),
            };
            Ok(CommandOutput::new(
                client_required(client)?
                    .post(
                        &["v1", "pods", pod_required(pod)?, "deploy"],
                        &[],
                        Some(&body),
                    )
                    .await?,
                View::Deploy,
            ))
        }
        Command::Redeploy => Ok(CommandOutput::new(
            client_required(client)?
                .post(&["v1", "pods", pod_required(pod)?, "redeploy"], &[], None)
                .await?,
            View::Redeploy,
        )),
        Command::Events(args) => {
            handle_events(client_required(client)?, pod_required(pod)?, args).await
        }
    }
}

fn handle_config(args: ConfigArgs, config: &mut Config, path: &Path) -> Result<CommandOutput> {
    match args.command {
        ConfigCommand::Show => Ok(CommandOutput::new(
            json!({
                "path": path,
                "endpoint": config.endpoint,
                "apiKeyConfigured": config.api_key.is_some(),
                "pod": config.pod,
            }),
            View::ConfigShow,
        )),
        ConfigCommand::Path => Ok(CommandOutput::new(
            json!({ "path": path }),
            View::ConfigPath,
        )),
        ConfigCommand::Set { key, value } => {
            if value.trim().is_empty() {
                return Err(anyhow!("configuration value cannot be empty"));
            }
            let key_name = match key {
                ConfigKey::Endpoint => {
                    config.endpoint = Some(value);
                    "endpoint"
                }
                ConfigKey::ApiKey => {
                    config.api_key = Some(value);
                    "apiKey"
                }
                ConfigKey::Pod => {
                    config.pod = Some(value);
                    "pod"
                }
            };
            config.save(path)?;
            Ok(CommandOutput::new(
                json!({ "updated": key_name, "path": path }),
                View::ConfigChange,
            ))
        }
        ConfigCommand::Unset { key } => {
            let key_name = match key {
                ConfigKey::Endpoint => {
                    config.endpoint = None;
                    "endpoint"
                }
                ConfigKey::ApiKey => {
                    config.api_key = None;
                    "apiKey"
                }
                ConfigKey::Pod => {
                    config.pod = None;
                    "pod"
                }
            };
            config.save(path)?;
            Ok(CommandOutput::new(
                json!({ "removed": key_name, "path": path }),
                View::ConfigChange,
            ))
        }
    }
}

async fn handle_pod(client: &Client, args: PodArgs) -> Result<CommandOutput> {
    match args.command {
        PodCommand::List => Ok(CommandOutput::new(
            client.get(&["v1", "pods"], &[]).await?,
            View::PodList,
        )),
        PodCommand::Get { pod } => Ok(CommandOutput::new(
            client.get(&["v1", "pods", &pod], &[]).await?,
            View::PodGet,
        )),
    }
}

async fn handle_revision(client: &Client, pod: &str, args: RevisionArgs) -> Result<CommandOutput> {
    match args.command {
        RevisionCommand::List { cursor, limit } => {
            let mut query = vec![("limit", limit.to_string())];
            push_query(&mut query, "cursor", cursor);
            Ok(CommandOutput::new(
                client
                    .get(&["v1", "pods", pod, "revisions"], &query)
                    .await?,
                View::RevisionList,
            ))
        }
        RevisionCommand::Get { revision } => Ok(CommandOutput::new(
            client
                .get(&["v1", "pods", pod, "revisions", &revision], &[])
                .await?,
            View::RevisionGet,
        )),
        RevisionCommand::Diff { revision, base } => {
            let mut query = Vec::new();
            push_query(&mut query, "base", base);
            Ok(CommandOutput::new(
                client
                    .get(&["v1", "pods", pod, "revisions", &revision, "diff"], &query)
                    .await?,
                View::RevisionDiff,
            ))
        }
    }
}

async fn handle_resource(client: &Client, pod: &str, args: ResourceArgs) -> Result<CommandOutput> {
    match args.command {
        ResourceCommand::List { revision, at } => {
            let query = historical_query(revision, at);
            Ok(CommandOutput::new(
                client
                    .get(&["v1", "pods", pod, "resources"], &query)
                    .await?,
                View::ResourceList,
            ))
        }
        ResourceCommand::Get {
            kind,
            name,
            revision,
            at,
        } => {
            let query = historical_query(revision, at);
            Ok(CommandOutput::new(
                client
                    .get(
                        &[
                            "v1",
                            "pods",
                            pod,
                            "resources",
                            kind.as_api_str(),
                            "default",
                            &name,
                        ],
                        &query,
                    )
                    .await?,
                View::ResourceGet,
            ))
        }
        ResourceCommand::Create { file, dry_run } => {
            let input = read_json(&file)?;
            let body = if input.is_object() {
                Value::Array(vec![input])
            } else if input.is_array() {
                input
            } else {
                return Err(anyhow!("resource input must be a JSON object or array"));
            };
            let query = if dry_run {
                vec![("dryRun", "true".to_owned())]
            } else {
                Vec::new()
            };
            let view = if dry_run {
                View::ResourceValidation
            } else {
                View::ResourceMutation
            };
            Ok(CommandOutput::new(
                client
                    .post(&["v1", "pods", pod, "resources"], &query, Some(&body))
                    .await?,
                view,
            ))
        }
        ResourceCommand::Replace { kind, name, file } => {
            let body = read_json(&file)?;
            if !body.is_object() {
                return Err(anyhow!("replacement input must be a JSON object"));
            }
            Ok(CommandOutput::new(
                client
                    .put(
                        &[
                            "v1",
                            "pods",
                            pod,
                            "resources",
                            kind.as_api_str(),
                            "default",
                            &name,
                        ],
                        &body,
                    )
                    .await?,
                View::ResourceMutation,
            ))
        }
        ResourceCommand::Delete { kind, name } => Ok(CommandOutput::new(
            client
                .delete(&[
                    "v1",
                    "pods",
                    pod,
                    "resources",
                    kind.as_api_str(),
                    "default",
                    &name,
                ])
                .await?,
            View::ResourceMutation,
        )),
    }
}

async fn handle_events(client: &Client, pod: &str, args: EventsArgs) -> Result<CommandOutput> {
    let mut query = vec![
        ("kind", args.kind.as_api_str().to_owned()),
        ("resource", args.resource),
        ("namespace", "default".to_owned()),
        ("range", args.range.as_api_str().to_owned()),
    ];
    push_query(
        &mut query,
        "level",
        args.level.map(|level| level.as_api_str().to_owned()),
    );
    push_query(&mut query, "search", args.search);
    push_query(&mut query, "cursor", args.cursor);
    Ok(CommandOutput::new(
        client.get(&["v1", "pods", pod, "events"], &query).await?,
        View::Events,
    ))
}

fn client_required(client: Option<&Client>) -> Result<&Client> {
    client.ok_or_else(|| anyhow!("API client is not configured"))
}

fn pod_required(pod: Option<&str>) -> Result<&str> {
    pod.ok_or_else(|| {
        anyhow!(
            "pod is required; pass --pod, set BRAINPOD_POD, or run `brainpod config set pod <name>`"
        )
    })
}

fn historical_query(revision: Option<String>, at: Option<String>) -> Vec<(&'static str, String)> {
    let mut query = Vec::new();
    push_query(&mut query, "revision", revision);
    push_query(&mut query, "at", at);
    query
}

fn push_query(query: &mut Vec<(&'static str, String)>, key: &'static str, value: Option<String>) {
    if let Some(value) = value {
        query.push((key, value));
    }
}

fn read_json(path: &Path) -> Result<Value> {
    let contents = if path == Path::new("-") {
        let mut contents = String::new();
        io::stdin()
            .read_to_string(&mut contents)
            .context("failed to read JSON from stdin")?;
        contents
    } else {
        fs::read_to_string(path)
            .with_context(|| format!("failed to read JSON from {}", path.display()))?
    };

    serde_json::from_str(&contents).with_context(|| format!("invalid JSON in {}", path.display()))
}
