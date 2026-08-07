use std::fs;
use std::io::{self, BufRead as _, Write as _};
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::process::Command as Process;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow};
use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use clap::{Args, Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::net::TcpListener;

use crate::output::{CommandOutput, View};

const CONSOLE_TEMPLATE: &str = include_str!("console.html");
/// Shared with the sign-in callback page so the two stay one design.
const SHELL_CSS: &str = include_str!("../callback.css");
const CONSOLE_CSS: &str = include_str!("console.css");
const CONFETTI: &str = include_str!("../callback-confetti.html");

/// The console page with its shared assets folded in.
fn console_page() -> String {
    CONSOLE_TEMPLATE
        .replace("/*@@SHELL@@*/", SHELL_CSS)
        .replace("/*@@CONSOLE@@*/", CONSOLE_CSS)
        .replace("<!--@@CONFETTI@@-->", CONFETTI)
}

const DIRECTORY: &str = ".brainpod";
const CONSOLE_FILE: &str = "console.html";
const SESSION_FILE: &str = "session.json";
const IGNORE_ENTRY: &str = ".brainpod/";
const IGNORE_COMMENT: &str = "# Brainpod session console";

const SCHEMA: u32 = 2;
const LOG_FILE: &str = "session.log";
/// Enough for the user to see where this is going without becoming a wall.
const RAIL_SHOWN: usize = 6;

#[derive(Debug, Args)]
pub struct AgentArgs {
    #[command(subcommand)]
    command: AgentCommand,
}

#[derive(Debug, Subcommand)]
enum AgentCommand {
    /// Start a session console and print the page to put in front of the user
    Start(StartArgs),
    /// Serve the session console over loopback for a browser outside this machine's agent
    Serve(ServeArgs),
    /// Record a step's state in the running session
    Step(StepArgs),
    /// Append stdin to one of the session's output streams
    Log(LogArgs),
    /// Close the running session
    Finish(FinishArgs),
    /// Remove the session console from the project
    Clear(ScopeArgs),
}

#[derive(Debug, Args)]
struct StartArgs {
    /// Planned step as <id>=<label>; repeat in the order they will run
    #[arg(long = "step", value_name = "ID=LABEL", value_parser = parse_planned_step)]
    steps: Vec<(String, String)>,
    /// Directory to write the console into; defaults to the repository root
    #[arg(long, value_name = "DIR")]
    path: Option<PathBuf>,
    /// Leave the repository's .gitignore untouched
    #[arg(long)]
    no_ignore: bool,
}

fn parse_planned_step(value: &str) -> std::result::Result<(String, String), String> {
    let (id, label) = value
        .split_once('=')
        .ok_or_else(|| "must be <id>=<label>".to_owned())?;
    if id.trim().is_empty() || label.trim().is_empty() {
        return Err("both <id> and <label> must be present".to_owned());
    }
    Ok((id.trim().to_owned(), label.trim().to_owned()))
}

#[derive(Debug, Args)]
struct ServeArgs {
    /// Directory holding the console; defaults to the repository root
    #[arg(long, value_name = "DIR")]
    path: Option<PathBuf>,
    /// Port to bind; defaults to an ephemeral port
    #[arg(long)]
    port: Option<u16>,
}

#[derive(Debug, Args)]
struct ScopeArgs {
    /// Directory holding the console; defaults to the repository root
    #[arg(long, value_name = "DIR")]
    path: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct LogArgs {
    /// Name each line is tagged with, so one log can carry several sources
    #[arg(long, default_value = "output")]
    stream: String,
    /// Directory holding the console; defaults to the repository root
    #[arg(long, value_name = "DIR")]
    path: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct StepArgs {
    /// Stable identifier for the step
    #[arg(value_name = "ID")]
    id: String,
    /// Text shown to the user; required the first time a step is recorded
    #[arg(long)]
    label: Option<String>,
    /// State to move the step into
    #[arg(long, value_enum, default_value_t = StepState::Running)]
    state: StepState,
    /// One short line shown under the label
    #[arg(long)]
    detail: Option<String>,
    /// Directory holding the console; defaults to the repository root
    #[arg(long, value_name = "DIR")]
    path: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct FinishArgs {
    /// How the session ended
    #[arg(long, value_enum, default_value_t = Outcome::Done)]
    state: Outcome,
    /// One line explaining the outcome
    #[arg(long)]
    message: Option<String>,
    /// Directory holding the console; defaults to the repository root
    #[arg(long, value_name = "DIR")]
    path: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum StepState {
    Pending,
    Running,
    Done,
    Failed,
}

impl StepState {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Done => "done",
            Self::Failed => "failed",
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum Outcome {
    Done,
    Failed,
}

impl Outcome {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Session {
    schema: u32,
    session: String,
    state: String,
    started_at: u64,
    updated_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pod: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pod_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    message: Option<String>,
    #[serde(default)]
    steps: Vec<Step>,
    /// Where a running `agent serve` will push change events, when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    events: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Step {
    id: String,
    label: String,
    state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    started_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ended_at: Option<u64>,
}

pub async fn handle(
    args: AgentArgs,
    pod: Option<&str>,
    dashboard_endpoint: &str,
    json: bool,
) -> Result<CommandOutput> {
    match args.command {
        AgentCommand::Start(args) => start(args, pod, dashboard_endpoint),
        AgentCommand::Serve(args) => serve(args, json).await,
        AgentCommand::Step(args) => step(args),
        AgentCommand::Log(args) => log(args),
        AgentCommand::Finish(args) => finish(args),
        AgentCommand::Clear(args) => clear(args),
    }
}

/// Serves the console over loopback until the process is killed.
///
/// A browser outside the agent will not read `session.json` from a `file://`
/// page — same-directory reads are blocked there — so the console is served
/// instead, which puts the page and its session on one origin. Announces the
/// URL on stdout before blocking, the way `login` announces its authorization
/// URL, so a caller can background this and read the first line.
///
/// The URL carries a random path because loopback is reachable by anything
/// else running on this machine, including pages in the user's browser.
async fn serve(args: ServeArgs, json: bool) -> Result<CommandOutput> {
    let directory = session_directory(args.path)?;
    let secret = identifier()?;

    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, args.port.unwrap_or_default()))
        .await
        .context("failed to bind the session console server")?;
    let port = listener
        .local_addr()
        .context("failed to determine the session console address")?
        .port();

    // 127.0.0.1 rather than localhost: localhost resolves to ::1 first on some
    // machines, and only the IPv4 loopback is bound here.
    let url = format!("http://127.0.0.1:{port}/{secret}/");

    let app = Router::new()
        .route(&format!("/{secret}/"), get(console))
        .route(&format!("/{secret}/{SESSION_FILE}"), get(session_state))
        .route(&format!("/{secret}/{LOG_FILE}"), get(log_file))
        .route(&format!("/{secret}/events"), get(events))
        .with_state(directory.clone());

    // Advertise the endpoint so a page opened as a local file can find it and
    // upgrade from polling to push. Never cleared: the page falls back to
    // polling when the stream drops, and no guard survives the kill that
    // actually ends this process.
    let advertised = format!("{url}events");
    let _ = amend_at(&directory, |session| session.events = Some(advertised));

    announce(&url, json)?;

    axum::serve(listener, app)
        .await
        .context("session console server failed")?;

    Ok(CommandOutput::new(
        json!({ "url": url, "port": port }),
        View::AgentServe,
    ))
}

fn announce(url: &str, json: bool) -> Result<()> {
    let notice = if json {
        serde_json::to_string(&json!({ "event": "console", "url": url }))
            .context("failed to serialize the console announcement")?
    } else {
        format!("Session console: {url}")
    };

    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    writeln!(stdout, "{notice}").context("failed to write the console announcement")?;
    stdout
        .flush()
        .context("failed to flush the console announcement")
}

async fn console() -> Response {
    (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        console_page(),
    )
        .into_response()
}

/// Emits a change token whenever the session or any of its logs moves.
///
/// Deliberately dumb: it says something changed and the page re-reads what it
/// already knows how to read, so the polling path and the push path run exactly
/// the same code.
async fn events(
    State(directory): State<PathBuf>,
) -> Sse<impl tokio_stream::Stream<Item = std::result::Result<Event, std::convert::Infallible>>> {
    use tokio_stream::StreamExt as _;

    let mut last = String::new();
    let ticks = tokio_stream::wrappers::IntervalStream::new(tokio::time::interval(
        std::time::Duration::from_millis(200),
    ));

    Sse::new(ticks.filter_map(move |_| {
        let current = fingerprint(&directory);
        if current == last {
            return None;
        }
        last = current.clone();
        Some(Ok(Event::default().data(current)))
    }))
    .keep_alive(KeepAlive::default())
}

/// A cheap summary of everything the page reads, used to notice changes.
fn fingerprint(directory: &Path) -> String {
    let mut parts = vec![
        fs::metadata(directory.join(SESSION_FILE))
            .and_then(|meta| Ok((meta.len(), meta.modified()?)))
            .map(|(len, time)| {
                let stamp = time
                    .duration_since(UNIX_EPOCH)
                    .map(|since| since.as_millis())
                    .unwrap_or_default();
                format!("{len}:{stamp}")
            })
            .unwrap_or_default(),
    ];
    parts.push(
        fs::metadata(directory.join(LOG_FILE))
            .map(|meta| meta.len().to_string())
            .unwrap_or_default(),
    );
    parts.join("|")
}

/// Serves the session log, honouring a byte range so the page can tail it.
async fn log_file(State(directory): State<PathBuf>, headers: HeaderMap) -> Response {
    let Ok(contents) = tokio::fs::read(directory.join(LOG_FILE)).await else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let total = contents.len() as u64;
    let range = headers
        .get(header::RANGE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| parse_range(value, total));

    let common = [
        (header::CONTENT_TYPE, "text/plain; charset=utf-8".to_owned()),
        (header::CACHE_CONTROL, "no-store".to_owned()),
        (header::ACCEPT_RANGES, "bytes".to_owned()),
    ];

    match range {
        Some((start, end)) => {
            let slice = contents[start as usize..=end as usize].to_vec();
            let mut response = (StatusCode::PARTIAL_CONTENT, common, slice).into_response();
            if let Ok(value) = format!("bytes {start}-{end}/{total}").parse() {
                response.headers_mut().insert(header::CONTENT_RANGE, value);
            }
            response
        }
        None => (common, contents).into_response(),
    }
}

/// Parses the one range form a tailing reader needs: `bytes=start-` or a suffix.
fn parse_range(value: &str, total: u64) -> Option<(u64, u64)> {
    let spec = value.strip_prefix("bytes=")?;
    if spec.contains(',') || total == 0 {
        return None;
    }
    let (start, end) = spec.split_once('-')?;

    let (start, end) = match (start.trim(), end.trim()) {
        ("", last) => (total.saturating_sub(last.parse::<u64>().ok()?), total - 1),
        (first, "") => (first.parse().ok()?, total - 1),
        (first, last) => (
            first.parse().ok()?,
            last.parse::<u64>().ok()?.min(total - 1),
        ),
    };

    (start <= end && start < total).then_some((start, end))
}

async fn session_state(State(directory): State<PathBuf>) -> Response {
    match tokio::fs::read(directory.join(SESSION_FILE)).await {
        Ok(contents) => (
            [
                (header::CONTENT_TYPE, "application/json"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            contents,
        )
            .into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Prepares `.brainpod/` and mints a fresh session.
///
/// The session file is always replaced rather than merged, so a second run
/// cannot leave the previous deploy's steps showing underneath the new one.
fn start(args: StartArgs, pod: Option<&str>, dashboard_endpoint: &str) -> Result<CommandOutput> {
    let root = project_root(args.path)?;
    let directory = root.join(DIRECTORY);
    fs::create_dir_all(&directory)
        .with_context(|| format!("failed to create {}", directory.display()))?;

    let ignored = if args.no_ignore {
        false
    } else {
        ensure_ignored(&root)?
    };

    // The session is minted fresh, so its log goes with it. Leaving it would
    // show the previous run's output under the new run's steps, the exact
    // confusion truncating the session file exists to prevent.
    let _ = fs::remove_file(directory.join(LOG_FILE));

    let console = directory.join(CONSOLE_FILE);
    replace(&console, console_page().as_bytes())?;

    let now = now();
    let session = Session {
        schema: SCHEMA,
        session: identifier()?,
        state: "running".to_owned(),
        started_at: now,
        updated_at: now,
        pod: pod.map(str::to_owned),
        pod_url: pod.map(|pod| pod_url(dashboard_endpoint, pod)),
        steps: args
            .steps
            .into_iter()
            .map(|(id, label)| Step {
                id,
                label,
                state: "pending".to_owned(),
                detail: None,
                started_at: None,
                ended_at: None,
            })
            .collect(),
        ..Session::default()
    };
    write_session(&directory, &session)?;

    Ok(CommandOutput::new(
        json!({
            "console": console.display().to_string(),
            "session": session.session,
            "directory": directory.display().to_string(),
            "gitignoreUpdated": ignored,
        }),
        View::AgentStart,
    ))
}

fn step(args: StepArgs) -> Result<CommandOutput> {
    let directory = session_directory(args.path)?;
    let mut session = read_session(&directory)?;
    let now = now();

    let state = args.state.as_str().to_owned();
    match session.steps.iter_mut().find(|step| step.id == args.id) {
        Some(existing) => {
            if let Some(label) = args.label {
                existing.label = label;
            }
            if args.detail.is_some() {
                existing.detail = args.detail;
            }
            if args.state == StepState::Running && existing.started_at.is_none() {
                existing.started_at = Some(now);
            }
            if matches!(args.state, StepState::Done | StepState::Failed) {
                existing.ended_at = Some(now);
            }
            existing.state = state;
        }
        None => {
            let label = args.label.ok_or_else(|| {
                anyhow!(
                    "--label is required the first time step `{}` is recorded",
                    args.id
                )
            })?;
            session.steps.push(Step {
                id: args.id,
                label,
                state,
                detail: args.detail,
                started_at: (args.state != StepState::Pending).then_some(now),
                ended_at: matches!(args.state, StepState::Done | StepState::Failed).then_some(now),
            });
        }
    }

    session.updated_at = now;
    write_session(&directory, &session)?;
    Ok(summary(&session))
}

/// Appends stdin to the session's bounded output tail.
///
/// The tail is capped so the file stays a snapshot the page can re-read whole;
/// `logDropped` is what lets the page say lines were dropped rather than imply
/// it showed everything.
fn log(args: LogArgs) -> Result<CommandOutput> {
    let directory = session_directory(args.path)?;
    let mut sink = open_log(&directory, &args.stream)?;
    for line in io::stdin().lock().lines() {
        sink.write(&line.context("failed to read output from stdin")?);
    }
    Ok(summary(&read_session(&directory)?))
}

/// An appender that tags every line with where it came from.
///
/// One file rather than one per source: separate files carry no timestamps, so
/// nothing could put them back in order. Append order is the ordering.
pub struct Sink {
    file: fs::File,
    prefix: String,
}

impl Sink {
    pub fn write(&mut self, line: &str) {
        let _ = writeln!(self.file, "{}{line}", self.prefix);
    }
}

fn open_log(directory: &Path, stream: &str) -> Result<Sink> {
    if !is_stream_name(stream) {
        return Err(anyhow!(
            "stream name may only contain letters, numbers, dashes, and underscores"
        ));
    }

    let path = directory.join(LOG_FILE);
    let file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("failed to open {}", path.display()))?;

    Ok(Sink {
        file,
        prefix: format!("[{stream}] "),
    })
}

/// Stream names are shown on every line they tag, so keep them boring.
fn is_stream_name(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 40
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn finish(args: FinishArgs) -> Result<CommandOutput> {
    let directory = session_directory(args.path)?;
    let mut session = read_session(&directory)?;
    let now = now();

    for step in &mut session.steps {
        if step.state == "running" {
            step.state = match args.state {
                Outcome::Done => "done".to_owned(),
                Outcome::Failed => "failed".to_owned(),
            };
            step.ended_at = Some(now);
        }
    }

    session.state = args.state.as_str().to_owned();
    session.message = args.message;
    session.updated_at = now;
    write_session(&directory, &session)?;
    Ok(summary(&session))
}

fn clear(args: ScopeArgs) -> Result<CommandOutput> {
    let root = project_root(args.path)?;
    let directory = root.join(DIRECTORY);
    let existed = directory.exists();
    if existed {
        fs::remove_dir_all(&directory)
            .with_context(|| format!("failed to remove {}", directory.display()))?;
    }

    Ok(CommandOutput::new(
        json!({
            "directory": directory.display().to_string(),
            "removed": existed,
        }),
        View::AgentClear,
    ))
}

/// One step as a page outside the console needs to draw it.
pub struct RailStep {
    pub state: String,
    pub label: String,
    pub detail: Option<String>,
}

/// The running session's steps, for a page outside the console to render.
///
/// Lets the sign-in page the user is looking at while `login` waits show where
/// the workflow has got to and what is still ahead, rather than being a dead
/// end. Empty when there is no session, which is also how the caller knows
/// nobody is driving this but the user.
pub fn rail() -> Vec<RailStep> {
    let Ok(root) = project_root(None) else {
        return Vec::new();
    };
    let Ok(session) = read_session(&root.join(DIRECTORY)) else {
        return Vec::new();
    };
    if session.state != "running" {
        return Vec::new();
    }

    let mut steps: Vec<RailStep> = session
        .steps
        .into_iter()
        .map(|step| RailStep {
            state: step.state,
            label: step.label,
            detail: step.detail,
        })
        .collect();

    // Keep the current step and what follows it. A long plan that has already
    // run for a while would otherwise push the interesting part off the card.
    if steps.len() > RAIL_SHOWN {
        let current = steps
            .iter()
            .position(|step| step.state == "running")
            .unwrap_or(0);
        let from = current.saturating_sub(1).min(steps.len() - RAIL_SHOWN);
        steps.drain(..from);
        steps.truncate(RAIL_SHOWN);
    }
    steps
}

/// Whether this project has a session console worth reporting into.
///
/// Callers use it to decide whether capturing a command's output is worth its
/// cost, so it checks only that the file is there rather than parsing it.
pub fn is_active() -> bool {
    project_root(None)
        .map(|root| root.join(DIRECTORY).join(SESSION_FILE).exists())
        .unwrap_or(false)
}

/// An appender for the session log, if this project has a console.
///
/// Best-effort like the rest: a caller that gets `None` simply has nowhere to
/// mirror its output, which must never be a reason to fail their command.
pub fn sink(stream: &str) -> Option<Sink> {
    let directory = project_root(None).ok()?.join(DIRECTORY);
    if !directory.join(SESSION_FILE).exists() {
        return None;
    }
    open_log(&directory, stream).ok()
}

/// Records a step in the session console, if this project has one.
///
/// Every call is best-effort and silent. A read-only checkout, a full disk, or
/// no console at all must never be able to fail the command the user actually
/// ran — they would be worse off than if the page had never existed.
pub fn note(id: &str, label: &str, state: &str, detail: Option<&str>) {
    let _ = amend(|session| {
        let now = now();
        match session.steps.iter_mut().find(|step| step.id == id) {
            Some(existing) => {
                if detail.is_some() {
                    existing.detail = detail.map(str::to_owned);
                }
                if state == "running" && existing.started_at.is_none() {
                    existing.started_at = Some(now);
                }
                if matches!(state, "done" | "failed") {
                    existing.ended_at = Some(now);
                }
                existing.state = state.to_owned();
            }
            None => session.steps.push(Step {
                id: id.to_owned(),
                label: label.to_owned(),
                state: state.to_owned(),
                detail: detail.map(str::to_owned),
                started_at: Some(now),
                ended_at: matches!(state, "done" | "failed").then_some(now),
            }),
        }
    });
}

/// Applies a change to the running session and stamps it as still alive.
///
/// Callers in a polling loop double as the heartbeat: without a recent
/// `updatedAt` the page cannot tell a slow deploy from an abandoned one.
fn amend(change: impl FnOnce(&mut Session)) -> Result<()> {
    amend_at(&project_root(None)?.join(DIRECTORY), change)
}

fn amend_at(directory: &Path, change: impl FnOnce(&mut Session)) -> Result<()> {
    if !directory.join(SESSION_FILE).exists() {
        return Ok(());
    }

    let mut session = read_session(directory)?;
    if session.state != "running" {
        return Ok(());
    }

    change(&mut session);
    session.updated_at = now();
    write_session(directory, &session)
}

fn summary(session: &Session) -> CommandOutput {
    CommandOutput::new(
        json!({
            "session": session.session,
            "state": session.state,
            "steps": session.steps.len(),
            "state": session.state,
        }),
        View::AgentUpdate,
    )
}

/// Resolves the directory the console lives in.
///
/// The repository root rather than the working directory, so a command run from
/// a subdirectory reaches the same console instead of creating a second one the
/// open page will never read.
fn project_root(explicit: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return Ok(path);
    }

    let toplevel = Process::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|path| PathBuf::from(path.trim()))
        .filter(|path| path.is_dir());

    match toplevel {
        Some(path) => Ok(path),
        None => std::env::current_dir().context("failed to determine the working directory"),
    }
}

fn session_directory(explicit: Option<PathBuf>) -> Result<PathBuf> {
    let directory = project_root(explicit)?.join(DIRECTORY);
    if !directory.join(SESSION_FILE).exists() {
        return Err(anyhow!(
            "no session console in {}; run `brainpod agent start` first",
            directory.display()
        ));
    }
    Ok(directory)
}

fn read_session(directory: &Path) -> Result<Session> {
    let path = directory.join(SESSION_FILE);
    let contents =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let session: Session = serde_json::from_str(&contents)
        .with_context(|| format!("failed to parse {}", path.display()))?;

    if session.schema != SCHEMA {
        return Err(anyhow!(
            "session console at {} was written by a different CLI version; run `brainpod agent start` again",
            path.display()
        ));
    }
    Ok(session)
}

fn write_session(directory: &Path, session: &Session) -> Result<()> {
    let contents = serde_json::to_vec_pretty(session).context("failed to serialize the session")?;
    replace(&directory.join(SESSION_FILE), &contents)
}

/// Writes through a temporary file so the page never reads a half-written file.
fn replace(path: &Path, contents: &[u8]) -> Result<()> {
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, contents)
        .with_context(|| format!("failed to write {}", temporary.display()))?;
    fs::rename(&temporary, path)
        .with_context(|| format!("failed to replace {}", path.display()))?;
    Ok(())
}

/// Adds `.brainpod/` to the repository's `.gitignore`, once.
///
/// Returns whether the file was changed. Existing content is never rewritten or
/// reordered — only appended to, and only when no entry already covers it.
fn ensure_ignored(root: &Path) -> Result<bool> {
    if !root.join(".git").exists() {
        return Ok(false);
    }

    let path = root.join(".gitignore");
    let current = match fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", path.display()));
        }
    };

    if current.lines().any(covers_console) {
        return Ok(false);
    }

    let mut next = current;
    if !next.is_empty() && !next.ends_with('\n') {
        next.push('\n');
    }
    if !next.is_empty() {
        next.push('\n');
    }
    next.push_str(IGNORE_COMMENT);
    next.push('\n');
    next.push_str(IGNORE_ENTRY);
    next.push('\n');

    fs::write(&path, next).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(true)
}

fn covers_console(line: &str) -> bool {
    matches!(
        line.trim(),
        ".brainpod" | ".brainpod/" | "/.brainpod" | "/.brainpod/"
    )
}

fn pod_url(dashboard_endpoint: &str, pod: &str) -> String {
    format!("{}/pods/{pod}", dashboard_endpoint.trim_end_matches('/'))
}

fn identifier() -> Result<String> {
    let mut bytes = [0_u8; 8];
    getrandom::fill(&mut bytes)
        .map_err(|error| anyhow!("failed to generate a session identifier: {error}"))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default()
}

pub fn render_start(value: &Value) -> Vec<String> {
    let mut lines = vec![format!(
        "Session console: {}",
        crate::output::field(value, "console")
    )];
    if value.get("gitignoreUpdated") == Some(&Value::Bool(true)) {
        lines.push("Added .brainpod/ to .gitignore".to_owned());
    }
    lines
}

pub fn render_update(value: &Value) -> Vec<String> {
    let steps = value
        .get("steps")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    vec![format!(
        "Session {}: {steps} step{}",
        crate::output::field(value, "state"),
        if steps == 1 { "" } else { "s" }
    )]
}

pub fn render_clear(value: &Value) -> Vec<String> {
    let directory = crate::output::field(value, "directory");
    if value.get("removed") == Some(&Value::Bool(true)) {
        vec![format!("Removed {directory}")]
    } else {
        vec![format!("Nothing to remove at {directory}")]
    }
}

#[cfg(test)]
mod tests {
    use super::{covers_console, pod_url};

    #[test]
    fn recognises_existing_ignore_entries() {
        assert!(covers_console(".brainpod/"));
        assert!(covers_console("  .brainpod  "));
        assert!(covers_console("/.brainpod/"));
        assert!(!covers_console(".brainpod/session.json"));
        assert!(!covers_console("# .brainpod/"));
    }

    #[test]
    fn builds_pod_url_without_doubling_the_separator() {
        assert_eq!(
            pod_url("https://brainpod.io/", "imagination"),
            "https://brainpod.io/pods/imagination"
        );
        assert_eq!(
            pod_url("https://brainpod.io", "imagination"),
            "https://brainpod.io/pods/imagination"
        );
    }
}
