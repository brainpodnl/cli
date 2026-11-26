use std::collections::HashMap;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, CommandFactory as _, Subcommand, ValueEnum};
use serde_json::{Value, json};

use crate::auth::LoginOptions;
use crate::client::Client;
use crate::config::Config;
use crate::output::{CommandOutput, View};

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Describe CLI commands and their machine-readable contract
    Describe(DescribeArgs),
    /// Detect local projects, frameworks, languages, and version constraints
    Detect(crate::detect::DetectArgs),
    /// Drive the session console the user watches while a workflow runs
    Agent(crate::agent::AgentArgs),
    /// Authenticate through the Brainpod dashboard
    Login(LoginArgs),
    /// Manage local CLI configuration
    Config(ConfigArgs),
    /// Show the authenticated user
    Whoami,
    /// List available clusters
    Cluster(ClusterArgs),
    /// List object storage providers and regions for Bucket resources
    Storage(StorageArgs),
    /// Report GitHub access and list repositories that can be linked to a Pod
    Github(GithubArgs),
    /// Inspect pods
    Pod(PodArgs),
    /// Link a GitHub repository to the pod and show the link
    Repo(RepoArgs),
    /// Start the pod's repository onboarding, an agent run that deploys the pod from its linked GitHub repository
    Onboard(OnboardArgs),
    /// List, inspect, and answer the pod's agent runs
    Runs(RunsArgs),
    /// List and inspect the pod's agent sessions, such as its repository onboarding
    Sessions(SessionsArgs),
    /// Browse and install blueprints
    Blueprint(BlueprintArgs),
    /// Manage container images
    Image(ImageArgs),
    /// Transfer and list objects through the Pod-scoped S3 gateway
    S3(crate::s3::S3Args),
    /// Inspect pod revisions
    Revision(RevisionArgs),
    /// Create a local TCP tunnel to a deployed database
    Tunnel(TunnelArgs),
    /// Create and manage pod resources
    Resource(ResourceArgs),
    /// Deploy the current draft revision
    Deploy(DeployArgs),
    /// Retry the pod's current head after its deployment failed
    Redeploy,
    /// Restore an earlier revision as the pod's draft head, optionally deploying it
    Rollback(RollbackArgs),
    /// Query pod events
    Events(EventsArgs),
    /// Make an authenticated request to a path on the Brainpod API
    Api(ApiArgs),
}

#[derive(Debug, Args)]
pub struct DescribeArgs {
    /// Command path or resource kind to describe; omit to return the complete command tree
    #[arg(value_name = "COMMAND", num_args = 0..)]
    pub command: Vec<String>,
}

#[derive(Debug, Args)]
pub struct LoginArgs {
    /// Print the authorization URL without opening a browser
    #[arg(long)]
    pub no_browser: bool,
}

#[derive(Debug, Args)]
pub struct TunnelArgs {
    /// Database resource name, URN, or stable UUID
    pub resource: String,
    /// Local IP address and port; defaults to loopback and the database engine's standard port
    pub listen_address: Option<std::net::SocketAddr>,
    /// Skip database credential preflight and password output
    #[arg(long)]
    pub skip_preflight: bool,
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
    ControlPlaneEndpoint,
    RegistryEndpoint,
    S3Endpoint,
    ApiToken,
    Pod,
    Architecture,
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
pub struct StorageArgs {
    #[command(subcommand)]
    command: StorageCommand,
}

#[derive(Debug, Subcommand)]
enum StorageCommand {
    /// List storage providers and their regions; a Bucket's spec.provider is a KEY and spec.region one of its REGIONS
    Providers,
}

#[derive(Debug, Args)]
pub struct GithubArgs {
    #[command(subcommand)]
    command: GithubCommand,
}

#[derive(Debug, Subcommand)]
enum GithubCommand {
    /// Report whether GitHub is connected to Brainpod and with what access, like `gh auth status`; prints the link to connect GitHub in the console, it does not authenticate itself
    Auth,
    /// List and search the GitHub repositories the Brainpod GitHub App can access
    Repos {
        /// Case-insensitive text to match against OWNER/NAME
        query: Option<String>,
        /// Page of results to return
        #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..))]
        page: u32,
        /// Number of repositories per page
        #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u8).range(1..=100))]
        per_page: u8,
    },
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
pub struct RepoArgs {
    #[command(subcommand)]
    command: RepoCommand,
}

#[derive(Debug, Subcommand)]
enum RepoCommand {
    /// Link a GitHub repository the Brainpod GitHub App can access to the pod; a linked repository is never replaced
    Link {
        /// The repository as OWNER/NAME, matched case-insensitively
        #[arg(value_name = "OWNER/NAME", value_parser = parse_repository_name)]
        repository: String,
    },
    /// Show the GitHub repository linked to the pod
    Get,
}

#[derive(Debug, Args)]
pub struct OnboardArgs {
    /// Branch, tag, or commit of the linked repository to onboard; defaults to its default branch
    #[arg(long = "ref", value_name = "REF")]
    git_ref: Option<String>,
}

#[derive(Debug, Args)]
pub struct RunsArgs {
    #[command(subcommand)]
    command: RunsCommand,
}

#[derive(Debug, Subcommand)]
enum RunsCommand {
    /// List the pod's agent runs
    List,
    /// Get an agent run: its step, state, error, and open questions
    Get {
        /// Agent run ID
        run: String,
    },
    /// Cancel an agent run that has not finished
    Cancel {
        /// Agent run ID
        run: String,
    },
    /// Retry a failed, timed-out, or canceled run of a repository onboarding
    Retry {
        /// Agent run ID
        run: String,
    },
    /// Request changes to a reviewed repository onboarding; the run must be its latest, a succeeded `assemble`
    Reply {
        /// Agent run ID
        run: String,
        /// The change to the reviewed draft, in your own words
        message: String,
    },
    /// Answer every open question of a run awaiting input and resume it
    Answer {
        /// Agent run ID
        run: String,
        /// An answer as QUESTION_ID=ANSWER; repeat it for every open question
        #[arg(
            long = "answer",
            value_name = "QUESTION_ID=ANSWER",
            required = true,
            value_parser = parse_answer
        )]
        answers: Vec<(String, String)>,
    },
}

#[derive(Debug, Args)]
pub struct SessionsArgs {
    #[command(subcommand)]
    command: SessionsCommand,
}

#[derive(Debug, Subcommand)]
enum SessionsCommand {
    /// List the pod's agent sessions, newest first
    List {
        /// Only sessions with this label, such as purpose=repo-onboarding
        #[arg(long, value_name = "KEY=VALUE", value_parser = parse_label)]
        label: Option<String>,
        /// Only sessions with this status
        #[arg(long)]
        status: Option<SessionStatus>,
    },
    /// Get an agent session: its runs per step, open questions, and continuous-deploys pull request
    Get {
        /// Agent session ID, the `sessionId` of its runs
        session: String,
    },
}

#[derive(Clone, Debug, ValueEnum)]
enum SessionStatus {
    Active,
    Completed,
    Canceled,
}

impl SessionStatus {
    fn as_api_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Completed => "completed",
            Self::Canceled => "canceled",
        }
    }
}

fn parse_repository_name(value: &str) -> Result<String, String> {
    match value.split_once('/') {
        Some((owner, name)) if !owner.is_empty() && !name.is_empty() && !name.contains('/') => {
            Ok(value.to_owned())
        }
        _ => Err("expected OWNER/NAME, such as acme/web".to_owned()),
    }
}

fn parse_answer(value: &str) -> Result<(String, String), String> {
    match value.split_once('=') {
        Some((id, answer)) if !id.is_empty() => Ok((id.to_owned(), answer.to_owned())),
        _ => Err("expected QUESTION_ID=ANSWER".to_owned()),
    }
}

fn parse_label(value: &str) -> Result<String, String> {
    match value.split_once('=') {
        Some((key, _)) if !key.is_empty() => Ok(value.to_owned()),
        _ => Err("expected KEY=VALUE, such as purpose=repo-onboarding".to_owned()),
    }
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
        /// Target platform; defaults to the best architecture supported by the API
        #[arg(long, value_name = "PLATFORM")]
        platform: Option<String>,
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
    /// Wait until every resource in a revision is healthy
    Wait {
        revision: String,
        /// Maximum number of seconds to wait
        #[arg(
            long,
            default_value_t = 90,
            value_parser = clap::value_parser!(u64).range(1..)
        )]
        timeout: u64,
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
    /// Show the variables exported by every resource, or by one resource
    Variables {
        /// Resource kind; omit both KIND and NAME for every resource in the pod
        #[arg(requires = "name")]
        kind: Option<ResourceKind>,
        /// Resource name
        name: Option<String>,
        #[arg(long, conflicts_with = "at")]
        revision: Option<String>,
        #[arg(long, conflicts_with = "revision")]
        at: Option<String>,
    },
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
    /// Wait until every resource in the deployed revision is healthy
    #[arg(long)]
    wait: bool,
    /// Maximum number of seconds to wait
    #[arg(
        long,
        default_value_t = 90,
        requires = "wait",
        value_parser = clap::value_parser!(u64).range(1..)
    )]
    timeout: u64,
}

#[derive(Debug, Args)]
pub struct RollbackArgs {
    /// ID of the revision to restore; it must not be a draft
    revision: String,
    /// Deploy the restored revision instead of leaving it as the draft head
    #[arg(long)]
    deploy: bool,
    /// Wait until every resource in the deployed revision is healthy
    #[arg(long, requires = "deploy")]
    wait: bool,
    /// Maximum number of seconds to wait
    #[arg(
        long,
        default_value_t = 90,
        requires = "wait",
        value_parser = clap::value_parser!(u64).range(1..)
    )]
    timeout: u64,
}

#[derive(Debug, Args)]
pub struct EventsArgs {
    /// Event stream; omit to return all streams for the resource
    #[arg(long)]
    kind: Option<EventKind>,
    /// Event-capable resource name, URN, or stable UUID
    #[arg(long, value_name = "RESOURCE")]
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

#[derive(Debug, Args)]
pub struct ApiArgs {
    /// API path relative to the configured endpoint, such as /v1/pods/{pod}/builds; `{pod}` is replaced by the selected pod
    #[arg(value_name = "PATH")]
    path: String,
    /// HTTP method; defaults to GET, or POST when --input is given
    #[arg(short = 'X', long, ignore_case = true)]
    method: Option<ApiMethod>,
    /// JSON request body read from a file, or `-` for stdin
    #[arg(long, value_name = "FILE")]
    input: Option<PathBuf>,
    /// Query parameter; repeat for more parameters
    #[arg(short, long, value_name = "KEY=VALUE", value_parser = parse_query_parameter)]
    query: Vec<(String, String)>,
}

impl ApiArgs {
    pub fn method(&self) -> reqwest::Method {
        match (&self.method, &self.input) {
            (Some(method), _) => method.as_method(),
            (None, Some(_)) => reqwest::Method::POST,
            (None, None) => reqwest::Method::GET,
        }
    }
}

#[derive(Clone, Debug, ValueEnum)]
enum ApiMethod {
    #[value(name = "GET")]
    Get,
    #[value(name = "POST")]
    Post,
    #[value(name = "PUT")]
    Put,
    #[value(name = "PATCH")]
    Patch,
    #[value(name = "DELETE")]
    Delete,
}

impl ApiMethod {
    fn as_method(&self) -> reqwest::Method {
        match self {
            Self::Get => reqwest::Method::GET,
            Self::Post => reqwest::Method::POST,
            Self::Put => reqwest::Method::PUT,
            Self::Patch => reqwest::Method::PATCH,
            Self::Delete => reqwest::Method::DELETE,
        }
    }
}

fn parse_query_parameter(value: &str) -> Result<(String, String), String> {
    match value.split_once('=') {
        Some((key, value)) if !key.is_empty() => Ok((key.to_owned(), value.to_owned())),
        _ => Err("expected KEY=VALUE".to_owned()),
    }
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

fn is_event_resource_urn(value: &str) -> bool {
    let mut parts = value.split(':');
    parts.next() == Some("urn")
        && parts.next() == Some("brain")
        && parts.next().is_some_and(|kind| {
            matches!(
                kind,
                "app" | "postgres" | "mariadb" | "valkey" | "mssql" | "route"
            )
        })
        && parts.next() == Some("default")
        && parts.next().is_some_and(|name| !name.is_empty())
        && parts.next().is_none()
}

pub fn needs_api_token(command: &Command) -> bool {
    !matches!(
        command,
        Command::Describe(_)
            | Command::Detect(_)
            | Command::Agent(_)
            | Command::Login(_)
            | Command::Config(_)
    )
}

pub fn needs_client(command: &Command) -> bool {
    !matches!(
        command,
        Command::S3(_)
            | Command::Describe(_)
            | Command::Detect(_)
            | Command::Agent(_)
            | Command::Login(_)
            | Command::Config(_)
    )
}

pub struct CommandContext<'a> {
    pub client: Option<&'a Client>,
    pub endpoint: &'a str,
    pub control_plane_endpoint: &'a str,
    pub dashboard_endpoint: &'a str,
    pub pod: Option<&'a str>,
    pub api_token: Option<&'a str>,
    pub registry_endpoint: &'a str,
    pub s3_endpoint: &'a str,
    pub config: &'a mut Config,
    pub config_path: &'a Path,
    pub show_progress: bool,
    pub json: bool,
}

pub async fn handle(command: Command, context: CommandContext<'_>) -> Result<CommandOutput> {
    let CommandContext {
        client,
        endpoint,
        control_plane_endpoint,
        dashboard_endpoint,
        pod,
        api_token,
        registry_endpoint,
        s3_endpoint,
        config,
        config_path,
        show_progress,
        json,
    } = context;
    match command {
        Command::Describe(args) => Ok(CommandOutput::new(
            crate::describe::generate(crate::Opts::command(), &args.command)?,
            View::Describe,
        )),
        Command::Detect(args) => crate::detect::handle(args),
        Command::Agent(args) => crate::agent::handle(args, pod, dashboard_endpoint, json).await,
        Command::Login(args) => Ok(CommandOutput::new(
            crate::auth::login(
                dashboard_endpoint,
                endpoint,
                config,
                config_path,
                LoginOptions {
                    no_browser: args.no_browser,
                    json,
                },
            )
            .await?,
            View::Login,
        )),
        Command::Config(args) => handle_config(args, config, config_path),
        Command::S3(args) => {
            crate::s3::handle(
                args,
                s3_endpoint,
                pod_required(pod)?,
                api_token.ok_or_else(|| anyhow!("API token is required"))?,
            )
            .await
        }
        Command::Whoami => Ok(CommandOutput::new(
            client_required(client)?.get(&["v1", "me"], &[]).await?,
            View::Whoami,
        )),
        Command::Cluster(args) => handle_cluster(client_required(client)?, args).await,
        Command::Storage(args) => handle_storage(client_required(client)?, args).await,
        Command::Github(args) => {
            handle_github(client_required(client)?, dashboard_endpoint, args).await
        }
        Command::Pod(args) => handle_pod(client_required(client)?, args).await,
        Command::Repo(args) => {
            handle_repo(
                client_required(client)?,
                pod_required(pod)?,
                dashboard_endpoint,
                args,
            )
            .await
        }
        Command::Onboard(args) => {
            let mut body = json!({ "agentKey": REPO_ONBOARDING_AGENT_KEY });
            if let Some(git_ref) = args.git_ref {
                body["ref"] = json!(git_ref);
            }
            Ok(CommandOutput::new(
                client_required(client)?
                    .post(
                        &["v1", "pods", pod_required(pod)?, "agent-runs"],
                        &[],
                        Some(&body),
                    )
                    .await?,
                View::OnboardStarted,
            ))
        }
        Command::Runs(args) => {
            handle_runs(client_required(client)?, pod_required(pod)?, args).await
        }
        Command::Sessions(args) => {
            handle_sessions(client_required(client)?, pod_required(pod)?, args).await
        }
        Command::Tunnel(args) => {
            crate::tunnel::handle(
                client_required(client)?,
                pod_required(pod)?,
                args,
                control_plane_endpoint,
                api_token.ok_or_else(|| anyhow!("API token is required"))?,
                json,
            )
            .await
        }
        Command::Blueprint(args) => handle_blueprint(client_required(client)?, pod, args).await,
        Command::Image(args) => {
            handle_image(
                client,
                args,
                pod_required(pod)?,
                api_token.ok_or_else(|| anyhow!("API token is required"))?,
                registry_endpoint,
                config,
                config_path,
            )
            .await
        }
        Command::Revision(args) => {
            handle_revision(
                client_required(client)?,
                pod_required(pod)?,
                args,
                show_progress,
            )
            .await
        }
        Command::Resource(args) => {
            handle_resource(client_required(client)?, pod_required(pod)?, args).await
        }
        Command::Deploy(args) => {
            let client = client_required(client)?;
            let pod = pod_required(pod)?;
            let body = match args.summary {
                Some(summary) => json!({ "summary": summary }),
                None => json!({}),
            };
            let deployment = client
                .post(&["v1", "pods", pod, "deploy"], &[], Some(&body))
                .await?;
            crate::agent::note(
                "deploy",
                "Deploy accepted",
                "done",
                deployment.get("revisionId").and_then(Value::as_str),
            );
            if !args.wait {
                return Ok(CommandOutput::new(deployment, View::Deploy));
            }

            let revision = deployment_revision(&deployment)?;
            Ok(CommandOutput::new(
                wait_for_revision(client, pod, revision, args.timeout, show_progress).await?,
                View::DeployWait,
            ))
        }
        Command::Redeploy => Ok(CommandOutput::new(
            client_required(client)?
                .post(&["v1", "pods", pod_required(pod)?, "redeploy"], &[], None)
                .await?,
            View::Redeploy,
        )),
        Command::Rollback(args) => {
            let client = client_required(client)?;
            let pod = pod_required(pod)?;
            let query = if args.deploy {
                vec![("deploy", "true".to_owned())]
            } else {
                Vec::new()
            };
            let rollback = client
                .post(
                    &["v1", "pods", pod, "rollback", &args.revision],
                    &query,
                    None,
                )
                .await?;
            if !args.wait {
                return Ok(CommandOutput::new(rollback, View::Rollback));
            }

            let revision = deployment_revision(&rollback)?;
            Ok(CommandOutput::new(
                wait_for_revision(client, pod, revision, args.timeout, show_progress).await?,
                View::DeployWait,
            ))
        }
        Command::Events(args) => {
            handle_events(client_required(client)?, pod_required(pod)?, args).await
        }
        Command::Api(args) => handle_api(client_required(client)?, pod, args).await,
    }
}

fn deployment_revision(deployment: &Value) -> Result<&str> {
    deployment
        .get("revisionId")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("Brainpod API returned a deployment without a revision ID"))
}

async fn handle_cluster(client: &Client, args: ClusterArgs) -> Result<CommandOutput> {
    match args.command {
        ClusterCommand::List => Ok(CommandOutput::new(
            client.get(&["v1", "clusters"], &[]).await?,
            View::ClusterList,
        )),
    }
}

async fn handle_storage(client: &Client, args: StorageArgs) -> Result<CommandOutput> {
    match args.command {
        StorageCommand::Providers => Ok(CommandOutput::new(
            client.get(&["v1", "storage", "providers"], &[]).await?,
            View::StorageProviderList,
        )),
    }
}

async fn handle_github(
    client: &Client,
    dashboard_endpoint: &str,
    args: GithubArgs,
) -> Result<CommandOutput> {
    let (response, view) = match args.command {
        GithubCommand::Auth => (
            client.get(&["v1", "github", "installations"], &[]).await?,
            View::GithubAuth,
        ),
        GithubCommand::Repos {
            query,
            page,
            per_page,
        } => {
            let mut parameters = vec![
                ("page", page.to_string()),
                ("perPage", per_page.to_string()),
            ];
            push_query(&mut parameters, "query", query);
            (
                client
                    .get(&["v1", "github", "repositories"], &parameters)
                    .await?,
                View::GithubRepos,
            )
        }
    };
    Ok(CommandOutput::new(
        with_connect_link(response, dashboard_endpoint),
        view,
    ))
}

/// Adds the console page that connects a GitHub account, which the API cannot link to.
fn with_connect_link(mut response: Value, dashboard_endpoint: &str) -> Value {
    if let Some(links) = response.as_object_mut().and_then(|response| {
        response
            .entry("_links")
            .or_insert_with(|| json!({}))
            .as_object_mut()
    }) {
        links.insert(
            "connect".to_owned(),
            json!({ "href": crate::output::github_connect_url(dashboard_endpoint) }),
        );
    }
    response
}

/// The agent whose run starts a Pod's repository onboarding.
const REPO_ONBOARDING_AGENT_KEY: &str = "repo-onboarding";

async fn handle_repo(
    client: &Client,
    pod: &str,
    dashboard_endpoint: &str,
    args: RepoArgs,
) -> Result<CommandOutput> {
    match args.command {
        RepoCommand::Link { repository } => {
            let found = find_github_repository(client, dashboard_endpoint, &repository).await?;
            let body = json!({
                "installationId": found.get("installationId"),
                "repositoryId": found.get("id"),
            });
            Ok(CommandOutput::new(
                client
                    .put(&["v1", "pods", pod, "repository"], &body)
                    .await?,
                View::RepositoryLinked,
            ))
        }
        RepoCommand::Get => match client.get(&["v1", "pods", pod, "repository"], &[]).await {
            Ok(repository) => Ok(CommandOutput::new(repository, View::Repository)),
            Err(error) if is_unlinked_repository(&error) => Err(anyhow!(
                "no repository linked; run `brainpod --pod {pod} repo link OWNER/NAME`"
            )),
            Err(error) => Err(error),
        },
    }
}

/// Pages through the repositories the GitHub App can access for the one named `full_name`.
async fn find_github_repository(
    client: &Client,
    dashboard_endpoint: &str,
    full_name: &str,
) -> Result<Value> {
    let mut page: u32 = 1;
    loop {
        let listing = client
            .get(
                &["v1", "github", "repositories"],
                &[
                    ("query", full_name.to_owned()),
                    ("page", page.to_string()),
                    ("perPage", "100".to_owned()),
                ],
            )
            .await?;
        if let Some(repository) = matching_repository(&listing, full_name) {
            return Ok(repository.clone());
        }
        if !value_is_link(listing.pointer("/_links/next")) {
            return Err(repository_not_accessible(
                full_name,
                &listing,
                &crate::output::github_connect_url(dashboard_endpoint),
            ));
        }
        page += 1;
    }
}

fn value_is_link(link: Option<&Value>) -> bool {
    link.and_then(|link| link.get("href"))
        .is_some_and(Value::is_string)
}

fn matching_repository<'a>(listing: &'a Value, full_name: &str) -> Option<&'a Value> {
    listing.get("items")?.as_array()?.iter().find(|repository| {
        repository
            .get("fullName")
            .and_then(Value::as_str)
            .is_some_and(|name| name.eq_ignore_ascii_case(full_name))
    })
}

fn repository_not_accessible(full_name: &str, listing: &Value, connect_url: &str) -> anyhow::Error {
    let mut message =
        format!("GitHub repository {full_name} is not accessible to the Brainpod GitHub App");
    for hint in crate::output::github_access_hints(listing.get("installations"), connect_url) {
        message.push('\n');
        message.push_str(&hint);
    }
    anyhow!(message)
}

fn is_unlinked_repository(error: &anyhow::Error) -> bool {
    error
        .chain()
        .filter_map(|cause| cause.downcast_ref::<crate::client::ApiError>())
        .any(|error| {
            error.status.as_u16() == 404
                && error
                    .body
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .is_some_and(|message| message.contains("no linked repository"))
        })
}

async fn handle_runs(client: &Client, pod: &str, args: RunsArgs) -> Result<CommandOutput> {
    let (response, view) = match args.command {
        RunsCommand::List => (
            client.get(&["v1", "pods", pod, "agent-runs"], &[]).await?,
            View::RunList,
        ),
        RunsCommand::Get { run } => (
            client
                .get(&["v1", "pods", pod, "agent-runs", &run], &[])
                .await?,
            View::Run,
        ),
        RunsCommand::Cancel { run } => (
            client
                .post(
                    &["v1", "pods", pod, "agent-runs", &run, "cancel"],
                    &[],
                    None,
                )
                .await?,
            View::RunChanged("Run canceled"),
        ),
        RunsCommand::Retry { run } => (
            client
                .post(&["v1", "pods", pod, "agent-runs", &run, "retry"], &[], None)
                .await?,
            View::RunChanged("Retry queued"),
        ),
        RunsCommand::Reply { run, message } => {
            let body = json!({
                "requestId": uuid::Uuid::new_v4().to_string(),
                "prompt": message,
            });
            (
                client
                    .post(
                        &["v1", "pods", pod, "agent-runs", &run, "reply"],
                        &[],
                        Some(&body),
                    )
                    .await?,
                View::RunChanged("Reply queued"),
            )
        }
        RunsCommand::Answer { run, answers } => {
            let current = client
                .get(&["v1", "pods", pod, "agent-runs", &run], &[])
                .await?;
            let body = resume_input(&current, answers)?;
            (
                client
                    .post(
                        &["v1", "pods", pod, "agent-runs", &run, "resume"],
                        &[],
                        Some(&body),
                    )
                    .await?,
                View::RunChanged("Run resumed"),
            )
        }
    };
    Ok(CommandOutput::new(response, view))
}

/// The resume input of a run awaiting input: exactly one answer to each of its open questions.
fn resume_input(run: &Value, answers: Vec<(String, String)>) -> Result<Value> {
    let id = run.get("id").and_then(Value::as_str).unwrap_or("-");
    let state = run.get("state").and_then(Value::as_str).unwrap_or("-");
    if state != "awaiting_input" {
        bail!("run {id} is {state}, not awaiting input; it has no open questions to answer");
    }
    let questions = run
        .get("questions")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(|question| question.get("id").and_then(Value::as_str))
        .collect::<Vec<_>>();
    let open = questions.join(", ");
    let mut answered = Vec::with_capacity(answers.len());
    for (question, _) in &answers {
        if !questions.contains(&question.as_str()) {
            bail!("run {id} has no open question `{question}`; its open questions: {open}");
        }
        if answered.contains(&question.as_str()) {
            bail!("question `{question}` is answered more than once");
        }
        answered.push(question.as_str());
    }
    let missing = questions
        .iter()
        .filter(|question| !answered.contains(question))
        .copied()
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        bail!(
            "answer every open question of run {id}; missing: {}",
            missing.join(", ")
        );
    }
    Ok(json!({
        "answers": answers
            .into_iter()
            .map(|(id, answer)| json!({ "id": id, "answer": answer }))
            .collect::<Vec<_>>(),
    }))
}

async fn handle_sessions(client: &Client, pod: &str, args: SessionsArgs) -> Result<CommandOutput> {
    match args.command {
        SessionsCommand::List { label, status } => {
            let mut query = Vec::new();
            push_query(&mut query, "label", label);
            push_query(
                &mut query,
                "status",
                status.map(|status| status.as_api_str().to_owned()),
            );
            Ok(CommandOutput::new(
                client
                    .get(&["v1", "pods", pod, "agent-sessions"], &query)
                    .await?,
                View::SessionList,
            ))
        }
        SessionsCommand::Get { session } => Ok(CommandOutput::new(
            client
                .get(&["v1", "pods", pod, "agent-sessions", &session], &[])
                .await?,
            View::Session,
        )),
    }
}

fn handle_config(args: ConfigArgs, config: &mut Config, path: &Path) -> Result<CommandOutput> {
    match args.command {
        ConfigCommand::Show => Ok(CommandOutput::new(
            json!({
                "path": path,
                "endpoint": config.endpoint,
                "controlPlaneEndpoint": config.control_plane_endpoint,
                "registryEndpoint": config.registry_endpoint,
                "s3Endpoint": config.s3_endpoint,
                "apiTokenConfigured": config.api_token.is_some(),
                "pod": config.pod,
                "architecture": config.architecture,
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
                ConfigKey::ControlPlaneEndpoint => {
                    config.control_plane_endpoint = Some(value);
                    "controlPlaneEndpoint"
                }
                ConfigKey::S3Endpoint => {
                    config.s3_endpoint = Some(value);
                    "s3Endpoint"
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
                ConfigKey::Architecture => {
                    config.architecture = Some(value);
                    "architecture"
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
                ConfigKey::ControlPlaneEndpoint => {
                    config.control_plane_endpoint = None;
                    "controlPlaneEndpoint"
                }
                ConfigKey::S3Endpoint => {
                    config.s3_endpoint = None;
                    "s3Endpoint"
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
                ConfigKey::Architecture => {
                    config.architecture = None;
                    "architecture"
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

const PREFERRED_ARCHITECTURES: &[&str] = &["amd64", "arm64"];

async fn resolve_platform(
    client: &Client,
    requested: Option<String>,
    config: &mut Config,
    config_path: &Path,
) -> Result<String> {
    let response = client.get(&["v1", "clusters"], &[]).await?;
    let supported = supported_architectures(&response)?;

    if let Some(platform) = requested {
        let architecture = platform_architecture(&platform)?;
        if !supported
            .iter()
            .any(|supported| supported.as_str() == architecture)
        {
            return Err(anyhow!(
                "platform {platform} is not supported by the available clusters"
            ));
        }
        return Ok(platform);
    }

    let configured = config.architecture.as_deref().map(normalize_architecture);
    let architecture = configured
        .filter(|architecture| supported.iter().any(|item| item == *architecture))
        .map(str::to_owned)
        .or_else(|| {
            PREFERRED_ARCHITECTURES
                .iter()
                .copied()
                .find(|architecture| supported.iter().any(|item| item == *architecture))
                .map(str::to_owned)
        })
        .ok_or_else(|| {
            anyhow!(
                "none of the preferred architectures ({}) are supported by the available clusters",
                PREFERRED_ARCHITECTURES.join(", ")
            )
        })?;

    if config.architecture.as_deref() != Some(architecture.as_str()) {
        config.architecture = Some(architecture.clone());
        config.save(config_path)?;
    }

    Ok(format!("linux/{architecture}"))
}

fn supported_architectures(value: &Value) -> Result<Vec<String>> {
    let clusters = value
        .as_array()
        .ok_or_else(|| anyhow!("Brainpod API returned an invalid cluster list"))?;
    let mut architectures = Vec::new();
    for cluster in clusters {
        let Some(values) = cluster.get("architectures").and_then(Value::as_array) else {
            continue;
        };
        for architecture in values.iter().filter_map(Value::as_str) {
            if !architectures.iter().any(|item| item == architecture) {
                architectures.push(architecture.to_owned());
            }
        }
    }
    if architectures.is_empty() {
        return Err(anyhow!(
            "Brainpod API returned no supported cluster architectures"
        ));
    }
    Ok(architectures)
}

fn platform_architecture(platform: &str) -> Result<&str> {
    let (os, architecture) = platform.split_once('/').ok_or_else(|| {
        anyhow!("platform must use the OS/architecture form, such as linux/amd64")
    })?;
    if os != "linux" || architecture.is_empty() || architecture.contains('/') {
        return Err(anyhow!(
            "platform must use the OS/architecture form, such as linux/amd64"
        ));
    }
    Ok(architecture)
}

fn normalize_architecture(architecture: &str) -> &str {
    architecture.strip_prefix("linux/").unwrap_or(architecture)
}

async fn handle_image(
    client: Option<&Client>,
    args: ImageArgs,
    pod: &str,
    api_token: &str,
    registry_endpoint: &str,
    config: &mut Config,
    config_path: &Path,
) -> Result<CommandOutput> {
    match args.command {
        ImageCommand::List {
            search,
            visibility,
            limit,
            offset,
        } => {
            let mut query = vec![("limit", limit.to_string()), ("offset", offset.to_string())];
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
            platform,
            output,
        } => {
            let platform =
                resolve_platform(client_required(client)?, platform, config, config_path).await?;
            crate::agent::note("image", "Packaging your app", "running", None);
            let built = crate::image::build(crate::image::BuildRequest {
                image,
                context,
                tag,
                method: builder,
                output,
                platform,
                pod,
                api_token,
                registry_endpoint,
            })
            .await;
            match &built {
                Ok(value) => crate::agent::note(
                    "image",
                    "Packaging your app",
                    "done",
                    image_summary(value).as_deref(),
                ),
                Err(_) => crate::agent::note("image", "Packaging your app", "failed", None),
            }
            Ok(CommandOutput::new(built?, View::ImageBuild))
        }
    }
}

/// The part of a build result worth showing on one line of the console.
fn image_summary(value: &Value) -> Option<String> {
    let text = value.get("builder").and_then(Value::as_str)?;
    let mut summary = text.to_owned();
    if let Some(platform) = value.get("platform").and_then(Value::as_str) {
        summary.push_str(" · ");
        summary.push_str(platform);
    }
    if let Some(digest) = value.get("digest").and_then(Value::as_str) {
        summary.push_str(" · ");
        summary.push_str(&digest.chars().take(19).collect::<String>());
    }
    Some(summary)
}

async fn handle_revision(
    client: &Client,
    pod: &str,
    args: RevisionArgs,
    show_progress: bool,
) -> Result<CommandOutput> {
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
        RevisionCommand::Wait { revision, timeout } => Ok(CommandOutput::new(
            wait_for_revision(client, pod, &revision, timeout, show_progress).await?,
            View::RevisionWait,
        )),
    }
}

async fn wait_for_revision(
    client: &Client,
    pod: &str,
    revision: &str,
    timeout_seconds: u64,
    show_progress: bool,
) -> Result<Value> {
    let timeout = Duration::from_secs(timeout_seconds);
    let deadline = tokio::time::Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| anyhow!("timeout is too large"))?;
    let mut previous_health = HashMap::new();
    let mut unhealthy = Vec::new();

    loop {
        let value = tokio::time::timeout_at(
            deadline,
            client.get(&["v1", "pods", pod, "revisions", revision], &[]),
        )
        .await
        .map_err(|_| wait_timeout(revision, timeout_seconds, &unhealthy))??;
        let state = value
            .get("state")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("Brainpod API returned a revision without a state"))?;
        if matches!(state, "failed" | "canceled") {
            let error = value
                .get("error")
                .and_then(Value::as_str)
                .map(|error| format!(": {error}"))
                .unwrap_or_default();
            return Err(anyhow!(
                "revision {revision} became {state} while waiting for its resources to become healthy{error}"
            ));
        }

        let resources = value
            .get("resources")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("Brainpod API returned a revision without resources"))?;
        unhealthy.clear();
        let mut current_health = HashMap::new();
        for resource in resources {
            let name = resource_name(resource);
            let healthy = resource
                .get("healthy")
                .and_then(Value::as_bool)
                .ok_or_else(|| {
                    anyhow!("Brainpod API returned a resource without a healthy flag")
                })?;
            if healthy {
                if show_progress && previous_health.get(&name) == Some(&false) {
                    eprintln!("Healthy: {}", resource_label(resource));
                }
            } else {
                unhealthy.push(name.clone());
            }
            current_health.insert(name, healthy);
        }
        previous_health = current_health;
        let healthy = previous_health.len() - unhealthy.len();
        crate::agent::note(
            "healthy",
            "Serving traffic",
            if unhealthy.is_empty() {
                "done"
            } else {
                "running"
            },
            Some(&format!(
                "{healthy} of {} resources healthy",
                previous_health.len()
            )),
        );
        if unhealthy.is_empty() {
            return Ok(value);
        }

        let now = tokio::time::Instant::now();
        if now >= deadline {
            return Err(wait_timeout(revision, timeout_seconds, &unhealthy));
        }
        tokio::time::sleep_until(std::cmp::min(deadline, now + Duration::from_secs(1))).await;
    }
}

fn resource_name(resource: &Value) -> String {
    resource
        .get("urn")
        .and_then(Value::as_str)
        .unwrap_or("unknown resource")
        .to_owned()
}

fn resource_label(resource: &Value) -> String {
    let kind = resource.pointer("/content/kind").and_then(Value::as_str);
    let name = resource
        .pointer("/content/metadata/name")
        .and_then(Value::as_str);
    match (kind, name) {
        (Some(kind), Some(name)) => format!("{kind}/{name}"),
        _ => resource_name(resource),
    }
}

fn wait_timeout(revision: &str, timeout_seconds: u64, unhealthy: &[String]) -> anyhow::Error {
    let resources = if unhealthy.is_empty() {
        String::new()
    } else {
        format!("; unhealthy resources: {}", unhealthy.join(", "))
    };
    anyhow!(
        "timed out after {timeout_seconds}s waiting for revision {revision} resources to become healthy{resources}"
    )
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
        ResourceCommand::Variables {
            kind,
            name,
            revision,
            at,
        } => {
            let query = historical_query(revision, at);
            let path = match (&kind, &name) {
                (Some(kind), Some(name)) => vec![
                    "v1",
                    "pods",
                    pod,
                    "resources",
                    kind.as_api_str(),
                    "default",
                    name.as_str(),
                ],
                _ => vec!["v1", "pods", pod, "variables"],
            };
            let response = client.get(&path, &query).await?;
            Ok(CommandOutput::new(
                Value::Array(variable_entries(&response)),
                View::ResourceVariables,
            ))
        }
    }
}

/// Reads the catalog off a single resource's `variables` or off the pod-wide
/// `items` envelope, so both invocations of `resource variables` emit one shape.
fn variable_entries(response: &Value) -> Vec<Value> {
    for key in ["variables", "items"] {
        if let Some(entries) = response.get(key).and_then(Value::as_array) {
            return entries.clone();
        }
    }
    Vec::new()
}

async fn handle_events(client: &Client, pod: &str, args: EventsArgs) -> Result<CommandOutput> {
    if args.level.is_some() && !matches!(args.kind.as_ref(), Some(EventKind::App)) {
        return Err(anyhow!("--level requires --kind app"));
    }

    let resource = if is_event_resource_urn(&args.resource) {
        args.resource
    } else {
        client.resolve_resource(pod, &args.resource).await?.urn
    };
    let mut query = vec![
        ("resource", resource),
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

async fn handle_api(client: &Client, pod: Option<&str>, args: ApiArgs) -> Result<CommandOutput> {
    let method = args.method();
    let segments = api_path_segments(&args.path, pod)?;
    let body = args.input.as_deref().map(read_json).transpose()?;
    let path = segments.iter().map(String::as_str).collect::<Vec<_>>();
    let query = args
        .query
        .iter()
        .map(|(key, value)| (key.as_str(), value.clone()))
        .collect::<Vec<_>>();

    let response = client.send(method, &path, &query, body.as_ref()).await?;
    if crate::client::is_event_stream(&response) {
        return Err(anyhow!(
            "`brainpod api` does not stream server-sent events; use `brainpod events --watch` instead"
        ));
    }
    let text = response
        .text()
        .await
        .context("failed to read Brainpod API response")?;
    if text.trim().is_empty() {
        return Ok(CommandOutput::Silent);
    }
    Ok(match serde_json::from_str(&text) {
        Ok(value) => CommandOutput::new(value, View::Api),
        Err(_) => CommandOutput::Text(text),
    })
}

/// Splits a `brainpod api` PATH into segments below the configured endpoint, so the
/// request (and the API token it carries) can never leave that endpoint. `{pod}` is
/// replaced by the selected pod inside its segment.
fn api_path_segments(path: &str, pod: Option<&str>) -> Result<Vec<String>> {
    if path.chars().any(char::is_control) {
        return Err(anyhow!("API path must not contain control characters"));
    }
    if path.contains('\\') {
        return Err(anyhow!("API path must not contain backslashes"));
    }
    if path.contains(['?', '#']) {
        return Err(anyhow!(
            "API path must not contain `?` or `#`; pass query parameters with --query KEY=VALUE"
        ));
    }
    if has_scheme(path) || path.starts_with("//") {
        return Err(anyhow!(
            "API path must be relative to the configured endpoint, such as /v1/me; URLs are not accepted"
        ));
    }

    let relative = path.strip_prefix('/').unwrap_or(path);
    let mut segments = Vec::new();
    for segment in relative.split('/') {
        if segment.is_empty() {
            return Err(anyhow!("API path must not contain empty segments"));
        }
        if is_dot_segment(segment) {
            return Err(anyhow!("API path must not contain `.` or `..` segments"));
        }
        if !segment.contains("{pod}") {
            segments.push(segment.to_owned());
            continue;
        }

        let pod = pod.ok_or_else(|| {
            anyhow!(
                "API path contains {{pod}} but no pod is selected; pass --pod, set BRAINPOD_POD, or run `brainpod config set pod <name>`"
            )
        })?;
        let segment = segment.replace("{pod}", pod);
        if segment.is_empty() || is_dot_segment(&segment) || segment.chars().any(char::is_control) {
            return Err(anyhow!(
                "selected pod `{pod}` cannot be used as an API path segment"
            ));
        }
        segments.push(segment);
    }
    Ok(segments)
}

/// Whether `path` starts like an absolute URL (`scheme:`), as in `https://host` or `http:host`.
fn has_scheme(path: &str) -> bool {
    path.split_once(':').is_some_and(|(scheme, _)| {
        scheme.starts_with(|character: char| character.is_ascii_alphabetic())
            && scheme
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || "+-.".contains(character))
    })
}

fn is_dot_segment(segment: &str) -> bool {
    let decoded = segment.to_ascii_lowercase().replace("%2e", ".");
    matches!(decoded.as_str(), "." | "..")
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        api_path_segments, matching_repository, repository_not_accessible, resume_input,
        variable_entries, with_connect_link,
    };

    #[test]
    fn adds_the_connect_link_next_to_the_api_links() {
        let response = with_connect_link(
            json!({"items": [], "_links": {"self": {"href": "/v1/github/installations"}}}),
            "https://console.example.test/",
        );

        assert_eq!(
            response["_links"],
            json!({
                "self": {"href": "/v1/github/installations"},
                "connect": {"href": "https://console.example.test/github/signin"}
            })
        );
    }

    #[test]
    fn reads_variables_off_a_single_resource() {
        let response = json!({
            "urn": "urn:brain:postgres:default:db",
            "healthy": true,
            "variables": [{"name": "host", "ref": "${db.host}"}]
        });

        assert_eq!(
            variable_entries(&response),
            vec![json!({"name": "host", "ref": "${db.host}"})]
        );
    }

    #[test]
    fn reads_the_pod_wide_catalog_out_of_its_envelope() {
        let entry = json!({
            "urn": "urn:brain:postgres:default:db",
            "name": "host",
            "ref": "${db.host}"
        });
        let response = json!({
            "items": [entry],
            "_links": {"self": {"href": "/v1/pods/my-pod/variables"}}
        });

        assert_eq!(variable_entries(&response), vec![entry]);
    }

    #[test]
    fn reports_no_entries_for_an_unexpected_shape() {
        assert!(variable_entries(&json!({"error": "nope"})).is_empty());
    }

    fn rejection(path: &str) -> String {
        api_path_segments(path, Some("my-pod"))
            .expect_err(path)
            .to_string()
    }

    #[test]
    fn accepts_paths_relative_to_the_endpoint() {
        assert_eq!(api_path_segments("/v1/me", None).unwrap(), ["v1", "me"]);
        assert_eq!(api_path_segments("v1/me", None).unwrap(), ["v1", "me"]);
        assert_eq!(
            api_path_segments(
                "/v1/pods/p/resources/resolve/urn:brain:app:default:api",
                None
            )
            .unwrap(),
            [
                "v1",
                "pods",
                "p",
                "resources",
                "resolve",
                "urn:brain:app:default:api"
            ]
        );
        assert_eq!(
            api_path_segments("/v1/pods/{pod}/builds", Some("my-pod")).unwrap(),
            ["v1", "pods", "my-pod", "builds"]
        );
    }

    #[test]
    fn keeps_a_pod_with_a_slash_in_one_segment() {
        assert_eq!(
            api_path_segments("/v1/pods/{pod}", Some("a/b")).unwrap(),
            ["v1", "pods", "a/b"]
        );
    }

    #[test]
    fn requires_a_pod_only_for_the_placeholder() {
        let error = api_path_segments("/v1/pods/{pod}", None)
            .unwrap_err()
            .to_string();
        assert!(error.contains("no pod is selected"), "{error}");
        assert!(api_path_segments("/v1/onboarding", None).is_ok());
    }

    #[test]
    fn rejects_dot_pods() {
        for pod in [".", "..", "%2e%2E", ""] {
            assert!(
                api_path_segments("/v1/pods/{pod}", Some(pod)).is_err(),
                "{pod}"
            );
        }
    }

    #[test]
    fn rejects_absolute_urls() {
        for path in [
            "https://example.com/v1/me",
            "http://example.com/v1/me",
            "http:example.com/v1/me",
            "HTTPS://example.com",
            "localhost:8080/v1/me",
        ] {
            assert!(rejection(path).contains("relative"), "{path}");
        }
    }

    #[test]
    fn rejects_scheme_relative_paths() {
        assert!(rejection("//example.com/v1/me").contains("relative"));
    }

    #[test]
    fn rejects_backslashes() {
        assert!(rejection("/v1\\me").contains("backslash"));
        assert!(rejection("\\\\example.com/v1").contains("backslash"));
    }

    #[test]
    fn rejects_dot_segments() {
        for path in [
            "/v1/../me",
            "/v1/./me",
            "../v1/me",
            "/v1/..",
            "/v1/%2e%2e/me",
            "/v1/.%2E/me",
        ] {
            assert!(rejection(path).contains("`.` or `..`"), "{path}");
        }
    }

    #[test]
    fn rejects_empty_segments() {
        for path in ["", "/", "/v1//me", "/v1/me/"] {
            assert!(rejection(path).contains("empty segments"), "{path}");
        }
    }

    #[test]
    fn rejects_query_and_fragment() {
        assert!(rejection("/v1/me?x=1").contains("--query"));
        assert!(rejection("/v1/me#top").contains("--query"));
    }

    #[test]
    fn rejects_control_characters() {
        for path in ["/v1/me\n", "/v1/\r\nme", "/v1/m\0e", "/v1/\u{7f}"] {
            assert!(rejection(path).contains("control characters"), "{path:?}");
        }
    }

    #[test]
    fn parses_the_github_commands() {
        use clap::Parser as _;

        let opts = crate::Opts::try_parse_from([
            "brainpod",
            "github",
            "repos",
            "acme",
            "--page",
            "2",
            "--per-page",
            "100",
        ])
        .unwrap();
        assert!(matches!(
            opts.command,
            super::Command::Github(super::GithubArgs {
                command: super::GithubCommand::Repos {
                    query: Some(_),
                    page: 2,
                    per_page: 100
                }
            })
        ));
        assert!(
            crate::Opts::try_parse_from(["brainpod", "github", "repos", "--per-page", "101"])
                .is_err()
        );
    }

    #[test]
    fn matches_a_repository_by_its_full_name_ignoring_case() {
        let listing = json!({
            "items": [
                {"id": 1, "installationId": 7, "fullName": "acme/web-admin"},
                {"id": 2, "installationId": 7, "fullName": "Acme/Web"}
            ]
        });

        assert_eq!(
            matching_repository(&listing, "acme/web").map(|repository| &repository["id"]),
            Some(&json!(2))
        );
        assert!(matching_repository(&listing, "acme/we").is_none());
        assert!(matching_repository(&json!({"items": []}), "acme/web").is_none());
    }

    #[test]
    fn names_an_inaccessible_repository_with_the_grant_access_hints() {
        let message = repository_not_accessible(
            "acme/web",
            &json!({
                "items": [],
                "installations": [{
                    "account": {"login": "acme"},
                    "repositorySelection": "selected",
                    "_links": {"settings": {"href": "https://github.com/settings/installations/7"}}
                }]
            }),
            "https://console.example.test/github/signin",
        )
        .to_string();

        assert_eq!(
            message,
            "GitHub repository acme/web is not accessible to the Brainpod GitHub App\n\
             Missing a repository? Grant access at https://github.com/settings/installations/7 (acme)\n\
             Add another GitHub account or organization: https://console.example.test/github/signin"
        );
    }

    fn awaiting_run() -> serde_json::Value {
        json!({
            "id": "run-1",
            "state": "awaiting_input",
            "questions": [{"id": "database", "question": "Which database?"}, {"id": "domain", "question": "Which domain?"}]
        })
    }

    fn answers(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(id, answer)| ((*id).to_owned(), (*answer).to_owned()))
            .collect()
    }

    #[test]
    fn resumes_with_one_answer_per_open_question() {
        let input = resume_input(
            &awaiting_run(),
            answers(&[("domain", "shop.example.com"), ("database", "postgres=16")]),
        )
        .unwrap();

        assert_eq!(
            input,
            json!({"answers": [
                {"id": "domain", "answer": "shop.example.com"},
                {"id": "database", "answer": "postgres=16"}
            ]})
        );
    }

    #[test]
    fn refuses_missing_unknown_or_repeated_answers() {
        let error = |pairs: &[(&str, &str)]| {
            resume_input(&awaiting_run(), answers(pairs))
                .unwrap_err()
                .to_string()
        };

        assert_eq!(
            error(&[("database", "postgres")]),
            "answer every open question of run run-1; missing: domain"
        );
        assert_eq!(
            error(&[("region", "eu")]),
            "run run-1 has no open question `region`; its open questions: database, domain"
        );
        assert_eq!(
            error(&[("database", "a"), ("database", "b")]),
            "question `database` is answered more than once"
        );
        assert_eq!(
            resume_input(
                &json!({"id": "run-1", "state": "running", "questions": null}),
                answers(&[("database", "postgres")])
            )
            .unwrap_err()
            .to_string(),
            "run run-1 is running, not awaiting input; it has no open questions to answer"
        );
    }

    #[test]
    fn parses_the_onboarding_commands() {
        use clap::Parser as _;

        use super::{
            Command, OnboardArgs, RepoArgs, RepoCommand, RunsArgs, RunsCommand, SessionStatus,
            SessionsArgs, SessionsCommand,
        };

        let parse = |args: &[&str]| {
            crate::Opts::try_parse_from(["brainpod", "--pod", "shop"].iter().chain(args))
        };

        assert!(matches!(
            parse(&["repo", "link", "acme/web"]).unwrap().command,
            Command::Repo(RepoArgs { command: RepoCommand::Link { repository } }) if repository == "acme/web"
        ));
        for invalid in ["acme", "acme/", "/web", "acme/web/extra"] {
            assert!(parse(&["repo", "link", invalid]).is_err(), "{invalid}");
        }
        assert!(matches!(
            parse(&["onboard", "--ref", "release"]).unwrap().command,
            Command::Onboard(OnboardArgs { git_ref: Some(git_ref) }) if git_ref == "release"
        ));
        assert!(matches!(
            parse(&["onboard"]).unwrap().command,
            Command::Onboard(OnboardArgs { git_ref: None })
        ));
        assert!(matches!(
            parse(&["runs", "reply", "run-1", "Use Valkey"]).unwrap().command,
            Command::Runs(RunsArgs { command: RunsCommand::Reply { run, message } })
                if run == "run-1" && message == "Use Valkey"
        ));
        let Command::Runs(RunsArgs {
            command:
                RunsCommand::Answer {
                    run,
                    answers: parsed,
                },
        }) = parse(&[
            "runs",
            "answer",
            "run-1",
            "--answer",
            "database=postgres",
            "--answer",
            "domain=a=b",
        ])
        .unwrap()
        .command
        else {
            panic!("expected runs answer");
        };
        assert_eq!(run, "run-1");
        assert_eq!(
            parsed,
            answers(&[("database", "postgres"), ("domain", "a=b")])
        );
        assert!(parse(&["runs", "answer", "run-1"]).is_err());
        assert!(parse(&["runs", "answer", "run-1", "--answer", "=postgres"]).is_err());
        assert!(matches!(
            parse(&[
                "sessions",
                "list",
                "--label",
                "purpose=repo-onboarding",
                "--status",
                "active"
            ])
            .unwrap()
            .command,
            Command::Sessions(SessionsArgs {
                command: SessionsCommand::List {
                    label: Some(_),
                    status: Some(SessionStatus::Active)
                }
            })
        ));
        assert!(parse(&["sessions", "list", "--label", "purpose"]).is_err());
        assert!(parse(&["sessions", "list", "--status", "done"]).is_err());
    }
}
