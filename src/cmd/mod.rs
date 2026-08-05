use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use clap::{Args, CommandFactory as _, Subcommand, ValueEnum};
use serde_json::{Value, json};

use crate::client::Client;
use crate::config::Config;
use crate::output::{CommandOutput, View};

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Describe CLI commands and their machine-readable contract
    Describe(DescribeArgs),
    /// Authenticate through the Brainpod dashboard
    Login,
    /// Manage local CLI configuration
    Config(ConfigArgs),
    /// Show the authenticated user
    Whoami,
    /// List available clusters
    Cluster(ClusterArgs),
    /// Inspect pods
    Pod(PodArgs),
    /// Browse and install blueprints
    Blueprint(BlueprintArgs),
    /// Manage container images
    Image(ImageArgs),
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
pub struct DescribeArgs {
    /// Command path to describe; omit to return the complete command tree
    #[arg(value_name = "COMMAND", num_args = 0..)]
    pub command: Vec<String>,
}

#[derive(Debug, Args)]
pub struct ConfigArgs {
    #[command(subcommand)]
    command: ConfigCommand,
}

#[derive(Debug, Subcommand)]
enum ConfigCommand {
    /// Show configuration without revealing the API token
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
    RegistryEndpoint,
    ApiToken,
    Pod,
}

#[derive(Debug, Args)]
pub struct ClusterArgs {
    #[command(subcommand)]
    command: ClusterCommand,
}

#[derive(Debug, Subcommand)]
enum ClusterCommand {
    /// List active clusters and their supported architectures
    List,
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
    /// Create an empty pod with a mutable draft head
    Create {
        #[arg(long)]
        display_name: Option<String>,
    },
    /// Get a pod
    Get { pod: String },
}

#[derive(Debug, Args)]
pub struct BlueprintArgs {
    #[command(subcommand)]
    command: BlueprintCommand,
}

#[derive(Debug, Subcommand)]
enum BlueprintCommand {
    /// List available blueprints
    List,
    /// Get blueprint metadata, documentation, defaults, and input schema
    Get { blueprint: String },
    /// Install a blueprint on the pod's mutable head without deploying it
    Install {
        blueprint: String,
        /// JSON object containing blueprint input; omit to use the defaults
        #[arg(short, long, value_name = "PATH")]
        file: Option<PathBuf>,
    },
}

#[derive(Debug, Args)]
pub struct ImageArgs {
    #[command(subcommand)]
    command: ImageCommand,
}

#[derive(Debug, Subcommand)]
enum ImageCommand {
    /// List active public and pod images visible from the selected pod
    List {
        /// Text to match against image metadata
        #[arg(long)]
        search: Option<String>,
        /// Limit results by visibility
        #[arg(long)]
        visibility: Option<ImageListVisibility>,
        /// Maximum number of images to return
        #[arg(long, default_value_t = 25, value_parser = clap::value_parser!(u16).range(1..=100))]
        limit: u16,
        /// Number of images to skip
        #[arg(long, default_value_t = 0)]
        offset: u32,
    },
    /// Inspect all active architecture variants for an exact image
    Inspect {
        /// Image repository
        repository: String,
        /// Image tag
        tag: String,
        /// Image visibility; defaults to pod
        #[arg(long, default_value = "pod")]
        visibility: ImageInspectVisibility,
    },
    /// Build an image from Dockerfile or Railpack and push it to the Brainpod registry
    Build {
        /// Repository name within the selected pod
        image: String,
        /// Application source directory
        #[arg(default_value = ".")]
        context: PathBuf,
        /// Image tag
        #[arg(long, default_value = "latest")]
        tag: String,
        /// Image builder; auto uses Dockerfile when present, otherwise Railpack
        #[arg(long, default_value_t = crate::image::BuildMethod::Auto)]
        builder: crate::image::BuildMethod,
        /// Retain the built OCI image layout at this path
        #[arg(long, value_name = "PATH")]
        output: Option<PathBuf>,
    },
}

#[derive(Clone, Debug, ValueEnum)]
enum ImageListVisibility {
    All,
    Public,
    Pod,
}

impl ImageListVisibility {
    fn as_api_str(&self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Public => "public",
            Self::Pod => "pod",
        }
    }
}

#[derive(Clone, Debug, ValueEnum)]
enum ImageInspectVisibility {
    Public,
    Pod,
}

impl ImageInspectVisibility {
    fn as_api_str(&self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Pod => "pod",
        }
    }
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
    /// Event stream; omit to return all streams for the resource
    #[arg(long)]
    kind: Option<EventKind>,
    /// Event-capable resource URN
    #[arg(long, value_name = "URN", value_parser = parse_event_resource_urn)]
    resource: String,
    /// Filter app events by level
    #[arg(long, requires = "kind")]
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
    /// Stream events instead of returning one page
    #[arg(long)]
    watch: bool,
    /// Seconds per event-stream request
    #[arg(long, requires = "watch", value_parser = clap::value_parser!(u8).range(1..=20))]
    duration: Option<u8>,
    /// Resume a stream after this SSE event ID
    #[arg(long, requires = "watch")]
    last_event_id: Option<String>,
}

#[derive(Clone, Debug, ValueEnum)]
enum EventKind {
    App,
    #[value(name = "http-access")]
    HttpAccess,
    Platform,
}

impl EventKind {
    fn as_api_str(&self) -> &'static str {
        match self {
            Self::App => "app",
            Self::HttpAccess => "httpAccess",
            Self::Platform => "platform",
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

fn parse_event_resource_urn(value: &str) -> std::result::Result<String, String> {
    let mut parts = value.split(':');
    let valid = parts.next() == Some("urn")
        && parts.next() == Some("brain")
        && parts.next().is_some_and(|kind| {
            matches!(kind, "app" | "postgres" | "mariadb" | "valkey" | "route")
        })
        && parts.next() == Some("default")
        && parts.next().is_some_and(|name| !name.is_empty())
        && parts.next().is_none();

    if valid {
        Ok(value.to_owned())
    } else {
        Err("must match urn:brain:<app|postgres|mariadb|valkey|route>:default:<name>".to_owned())
    }
}

pub fn needs_api_token(command: &Command) -> bool {
    !matches!(
        command,
        Command::Describe(_) | Command::Login | Command::Config(_)
    )
}

pub fn needs_client(command: &Command) -> bool {
    !matches!(
        command,
        Command::Describe(_)
            | Command::Login
            | Command::Config(_)
            | Command::Image(ImageArgs {
                command: ImageCommand::Build { .. },
            })
    )
}

pub async fn handle(
    command: Command,
    client: Option<&Client>,
    endpoint: &str,
    dashboard_endpoint: &str,
    pod: Option<&str>,
    api_token: Option<&str>,
    registry_endpoint: &str,
    config: &mut Config,
    config_path: &Path,
) -> Result<CommandOutput> {
    match command {
        Command::Describe(args) => Ok(CommandOutput::new(
            crate::describe::generate(crate::Opts::command(), &args.command)?,
            View::Describe,
        )),
        Command::Login => Ok(CommandOutput::new(
            crate::auth::login(dashboard_endpoint, endpoint, config, config_path).await?,
            View::Login,
        )),
        Command::Config(args) => handle_config(args, config, config_path),
        Command::Whoami => Ok(CommandOutput::new(
            client_required(client)?.get(&["v1", "me"], &[]).await?,
            View::Whoami,
        )),
        Command::Cluster(args) => handle_cluster(client_required(client)?, args).await,
        Command::Pod(args) => handle_pod(client_required(client)?, args).await,
        Command::Blueprint(args) => {
            handle_blueprint(client_required(client)?, pod, args).await
        }
        Command::Image(args) => {
            handle_image(
                client,
                args,
                pod_required(pod)?,
                api_token.ok_or_else(|| anyhow!("API token is required"))?,
                registry_endpoint,
            )
            .await
        }
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

async fn handle_cluster(client: &Client, args: ClusterArgs) -> Result<CommandOutput> {
    match args.command {
        ClusterCommand::List => Ok(CommandOutput::new(
            client.get(&["v1", "clusters"], &[]).await?,
            View::ClusterList,
        )),
    }
}

fn handle_config(args: ConfigArgs, config: &mut Config, path: &Path) -> Result<CommandOutput> {
    match args.command {
        ConfigCommand::Show => Ok(CommandOutput::new(
            json!({
                "path": path,
                "endpoint": config.endpoint,
                "registryEndpoint": config.registry_endpoint,
                "apiTokenConfigured": config.api_token.is_some(),
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
                ConfigKey::RegistryEndpoint => {
                    config.registry_endpoint = Some(value);
                    "registryEndpoint"
                }
                ConfigKey::ApiToken => {
                    config.api_token = Some(value);
                    "apiToken"
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
                ConfigKey::RegistryEndpoint => {
                    config.registry_endpoint = None;
                    "registryEndpoint"
                }
                ConfigKey::ApiToken => {
                    config.api_token = None;
                    "apiToken"
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
        PodCommand::Create { display_name } => {
            let body = display_name.map(|display_name| json!({ "displayName": display_name }));
            Ok(CommandOutput::new(
                client.post(&["v1", "pods"], &[], body.as_ref()).await?,
                View::PodCreated,
            ))
        }
        PodCommand::Get { pod } => Ok(CommandOutput::new(
            client.get(&["v1", "pods", &pod], &[]).await?,
            View::PodGet,
        )),
    }
}

async fn handle_blueprint(
    client: &Client,
    pod: Option<&str>,
    args: BlueprintArgs,
) -> Result<CommandOutput> {
    match args.command {
        BlueprintCommand::List => Ok(CommandOutput::new(
            client.get(&["v1", "blueprints"], &[]).await?,
            View::BlueprintList,
        )),
        BlueprintCommand::Get { blueprint } => Ok(CommandOutput::new(
            client.get(&["v1", "blueprints", &blueprint], &[]).await?,
            View::BlueprintGet,
        )),
        BlueprintCommand::Install { blueprint, file } => {
            let input = match file {
                Some(file) => read_json(&file)?,
                None => json!({}),
            };
            if !input.is_object() {
                return Err(anyhow!("blueprint input must be a JSON object"));
            }
            let body = json!({ "input": input });
            Ok(CommandOutput::new(
                client
                    .post(
                        &[
                            "v1",
                            "pods",
                            pod_required(pod)?,
                            "blueprints",
                            &blueprint,
                            "install",
                        ],
                        &[],
                        Some(&body),
                    )
                    .await?,
                View::ResourceMutation,
            ))
        }
    }
}

async fn handle_image(
    client: Option<&Client>,
    args: ImageArgs,
    pod: &str,
    api_token: &str,
    registry_endpoint: &str,
) -> Result<CommandOutput> {
    match args.command {
        ImageCommand::List {
            search,
            visibility,
            limit,
            offset,
        } => {
            let mut query = vec![
                ("limit", limit.to_string()),
                ("offset", offset.to_string()),
            ];
            push_query(&mut query, "search", search);
            push_query(
                &mut query,
                "visibility",
                visibility.map(|visibility| visibility.as_api_str().to_owned()),
            );
            Ok(CommandOutput::new(
                client_required(client)?
                    .get(&["v1", "pods", pod, "images"], &query)
                    .await?,
                View::ImageList,
            ))
        }
        ImageCommand::Inspect {
            repository,
            tag,
            visibility,
        } => {
            let query = vec![
                ("visibility", visibility.as_api_str().to_owned()),
                ("repository", repository),
                ("tag", tag),
            ];
            Ok(CommandOutput::new(
                client_required(client)?
                    .get(&["v1", "pods", pod, "images", "inspect"], &query)
                    .await?,
                View::ImageInspect,
            ))
        }
        ImageCommand::Build {
            image,
            context,
            tag,
            builder,
            output,
        } => Ok(CommandOutput::new(
            crate::image::build(
                image,
                context,
                tag,
                builder,
                output,
                pod,
                api_token,
                registry_endpoint,
            )
            .await?,
            View::ImageBuild,
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
    if args.level.is_some() && !matches!(args.kind.as_ref(), Some(EventKind::App)) {
        return Err(anyhow!("--level requires --kind app"));
    }

    let mut query = vec![
        ("resource", args.resource),
        ("range", args.range.as_api_str().to_owned()),
    ];
    push_query(
        &mut query,
        "kind",
        args.kind.map(|kind| kind.as_api_str().to_owned()),
    );
    push_query(
        &mut query,
        "level",
        args.level.map(|level| level.as_api_str().to_owned()),
    );
    push_query(&mut query, "search", args.search);
    push_query(&mut query, "cursor", args.cursor);

    if args.watch {
        query.push(("duration", args.duration.unwrap_or(10).to_string()));
        return Ok(CommandOutput::event_watch(
            client
                .get_event_watch(
                    &["v1", "pods", pod, "events", "watch"],
                    &query,
                    args.last_event_id.as_deref(),
                )
                .await?,
        ));
    }

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
