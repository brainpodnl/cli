use std::io::{self, Write};
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use axum::Router;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::client::Client;
use crate::config::Config;

const CALLBACK_PATH: &str = "/callback";
const AUTHENTICATION_TIMEOUT: Duration = Duration::from_secs(10 * 60);

#[derive(Clone, Copy, Debug, Default)]
pub struct LoginOptions {
    pub no_browser: bool,
    pub json: bool,
}

pub async fn login(
    dashboard_endpoint: &str,
    api_endpoint: &str,
    config: &mut Config,
    config_path: &Path,
    options: LoginOptions,
) -> Result<Value> {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .context("failed to start authentication callback server")?;
    let port = listener
        .local_addr()
        .context("failed to determine authentication callback address")?
        .port();
    let redirect_uri = format!("http://127.0.0.1:{port}{CALLBACK_PATH}");
    let state = generate_state()?;
    let authorize_url = authorization_url(dashboard_endpoint, &redirect_uri, &state)?;

    let (request_sender, mut requests) = mpsc::channel(1);
    let app = Router::new()
        .route(CALLBACK_PATH, get(handle_callback))
        .with_state(CallbackState {
            expected_state: state,
            requests: request_sender,
        });
    let (shutdown_sender, shutdown) = oneshot::channel();
    let mut server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = shutdown.await;
            })
            .await
            .context("authentication callback server failed")
    });

    write_authorization_notice(authorize_url.as_str(), options)?;

    if !options.no_browser
        && let Err(error) = webbrowser::open(authorize_url.as_str())
    {
        eprintln!(
            "warning: failed to open the browser automatically: {error}; open the URL above in a browser on this machine, or create an API token in the Brainpod dashboard and run `brainpod config set api-token <token>`"
        );
    }

    let request = tokio::select! {
        request = requests.recv() => request
            .ok_or_else(|| anyhow!("authentication callback server stopped unexpectedly"))?,
        result = &mut server => {
            result
                .context("authentication callback server task failed")??;
            return Err(anyhow!("authentication callback server stopped unexpectedly"));
        }
        _ = tokio::time::sleep(AUTHENTICATION_TIMEOUT) => {
            drop(requests);
            let _ = shutdown_sender.send(());
            server
                .await
                .context("authentication callback server task failed")??;
            return Err(anyhow!("authentication timed out after 10 minutes"));
        }
    };

    let token = match request.callback {
        Ok(Callback::Token(token)) => token,
        Ok(Callback::Cancelled) => {
            let _ =
                finish_callback(request.response, Page::cancelled(), shutdown_sender, server).await;
            return Err(anyhow!("authentication was cancelled"));
        }
        Err(error) => {
            let _ =
                finish_callback(request.response, Page::invalid(), shutdown_sender, server).await;
            return Err(error);
        }
    };

    config.api_token = Some(token.clone());
    if let Err(error) = config.save(config_path) {
        let _ = finish_callback(
            request.response,
            Page::storage_failed(),
            shutdown_sender,
            server,
        )
        .await;
        return Err(error);
    }

    let page = Page::success(handover().await);
    finish_callback(request.response, page, shutdown_sender, server).await?;

    Client::try_new(api_endpoint, &token)?
        .get(&["v1", "me"], &[])
        .await
        .context("failed to verify authentication with the Brainpod API")
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AuthorizeAnnouncement<'a> {
    event: &'static str,
    url: &'a str,
    expires_in_seconds: u64,
}

/// Writes the authorization URL to stdout before waiting for the callback.
///
/// The authorization URL redirects to a loopback address, so it only completes
/// in a browser running on the same machine as the CLI.
fn write_authorization_notice(authorize_url: &str, options: LoginOptions) -> Result<()> {
    let notice = authorization_notice(authorize_url, options)?;
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    writeln!(stdout, "{notice}").context("failed to write the authorization announcement")?;
    stdout
        .flush()
        .context("failed to flush the authorization announcement")
}

fn authorization_notice(authorize_url: &str, options: LoginOptions) -> Result<String> {
    if options.json {
        return serde_json::to_string(&AuthorizeAnnouncement {
            event: "authorize",
            url: authorize_url,
            expires_in_seconds: AUTHENTICATION_TIMEOUT.as_secs(),
        })
        .context("failed to serialize the authorization announcement");
    }

    Ok(format!(
        "Open this URL in a browser on this machine to authenticate: {authorize_url}"
    ))
}

fn generate_state() -> Result<String> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|error| anyhow!("failed to generate authentication state: {error}"))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn authorization_url(dashboard_endpoint: &str, redirect_uri: &str, state: &str) -> Result<Url> {
    let mut url = Url::parse(dashboard_endpoint).context("invalid Brainpod dashboard endpoint")?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(anyhow!(
            "Brainpod dashboard endpoint must use http or https"
        ));
    }
    if url.host_str().is_none() {
        return Err(anyhow!("Brainpod dashboard endpoint has no host"));
    }
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(anyhow!(
            "Brainpod dashboard endpoint must not contain credentials, a query, or a fragment"
        ));
    }
    {
        let mut segments = url
            .path_segments_mut()
            .map_err(|_| anyhow!("Brainpod dashboard endpoint cannot be a base URL"))?;
        segments.pop_if_empty();
        segments.extend(["cli", "authorize"]);
    }
    url.query_pairs_mut()
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("state", state);
    Ok(url)
}

#[derive(Clone)]
struct CallbackState {
    expected_state: String,
    requests: mpsc::Sender<CallbackRequest>,
}

struct CallbackRequest {
    callback: Result<Callback>,
    response: oneshot::Sender<Page>,
}

#[derive(Deserialize)]
struct CallbackQuery {
    token: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

enum Callback {
    Token(String),
    Cancelled,
}

async fn handle_callback(
    State(state): State<CallbackState>,
    query: std::result::Result<Query<CallbackQuery>, QueryRejection>,
) -> Response {
    let callback = match query {
        Ok(Query(query)) => parse_callback_query(query, &state.expected_state),
        Err(error) => Err(anyhow!(
            "authentication callback query was invalid: {error}"
        )),
    };
    let (response, page) = oneshot::channel();
    if state
        .requests
        .send(CallbackRequest { callback, response })
        .await
        .is_err()
    {
        return Page::unavailable().into_response();
    }

    match page.await {
        Ok(page) => page.into_response(),
        Err(_) => Page::unavailable().into_response(),
    }
}

fn parse_callback_query(query: CallbackQuery, expected_state: &str) -> Result<Callback> {
    if query.state.as_deref() != Some(expected_state) {
        return Err(anyhow!("authentication callback state did not match"));
    }

    match (query.token, query.error) {
        (Some(token), None) if token.starts_with("brain_") && token.len() > "brain_".len() => {
            Ok(Callback::Token(token))
        }
        (None, Some(error)) if error == "access_denied" => Ok(Callback::Cancelled),
        (None, Some(error)) => Err(anyhow!("dashboard returned authentication error `{error}`")),
        _ => Err(anyhow!("authentication callback had an invalid result")),
    }
}

async fn finish_callback(
    response: oneshot::Sender<Page>,
    page: Page,
    shutdown: oneshot::Sender<()>,
    server: JoinHandle<Result<()>>,
) -> Result<()> {
    let _ = response.send(page);
    let _ = shutdown.send(());
    server
        .await
        .context("authentication callback server task failed")??;
    Ok(())
}

/// What the callback page is for.
///
/// Not a styling choice: an agent-driven session has a plan behind it and gets
/// shown where the workflow has reached, while someone who ran `login` in a
/// terminal has none and is handed the prompt that starts one instead.
enum Shape {
    Agent(Vec<crate::agent::RailStep>),
    Human,
    Stopped,
}

struct Page {
    status: StatusCode,
    headline: &'static str,
    lead: &'static str,
    badge: &'static str,
    badge_tone: &'static str,
    foot: &'static str,
    shape: Shape,
    handover: Option<String>,
}

impl Page {
    fn success(handover: Option<String>) -> Self {
        let rail = crate::agent::rail();
        if rail.is_empty() {
            return Self {
                status: StatusCode::OK,
                headline: "Signed in.",
                lead: "The token is stored. The terminal you started this from is already carrying on.",
                badge: "signed in",
                badge_tone: "",
                foot: "Token stored in your Brainpod config",
                shape: Shape::Human,
                handover: None,
            };
        }

        let mut page = Self {
            status: StatusCode::OK,
            headline: "Signed in.",
            lead: "Your agent has the session and is carrying on.",
            badge: "agent session",
            badge_tone: "",
            foot: "Waiting on your agent, not on you.",
            shape: Shape::Agent(rail),
            handover: None,
        };
        if handover.is_some() {
            page.lead =
                "Your agent has the session and is carrying on. This tab is about to show it live.";
            page.foot = "Opening the session console\u{2026}";
            page.handover = handover;
        }
        page
    }

    fn cancelled() -> Self {
        Self {
            status: StatusCode::OK,
            headline: "Sign-in was cancelled.",
            lead: "Nothing changed, and no token was stored. The CLI is still waiting, so you can start again from there.",
            badge: "not signed in",
            badge_tone: " is-warn",
            foot: "You can close this tab.",
            shape: Shape::Stopped,
            handover: None,
        }
    }

    fn invalid() -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            headline: "Sign-in failed.",
            lead: "That response was not valid, so no token was stored. Start again from the CLI.",
            badge: "not signed in",
            badge_tone: " is-fail",
            foot: "You can close this tab.",
            shape: Shape::Stopped,
            handover: None,
        }
    }

    fn storage_failed() -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            headline: "Sign-in failed.",
            lead: "You approved it, but the token could not be saved. The CLI has the details.",
            badge: "not signed in",
            badge_tone: " is-fail",
            foot: "You can close this tab.",
            shape: Shape::Stopped,
            handover: None,
        }
    }

    fn unavailable() -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            headline: "Sign-in failed.",
            lead: "The CLI stopped waiting for this response. Start again from the terminal.",
            badge: "not signed in",
            badge_tone: " is-fail",
            foot: "You can close this tab.",
            shape: Shape::Stopped,
            handover: None,
        }
    }
}

const STYLE: &str = include_str!("callback.css");
const CONFETTI: &str = include_str!("callback-confetti.html");

const MARK: &str = concat!(
    r#"<svg viewBox="0 0 19.89 21.47" aria-hidden><path fill="currentColor" d="M6.14451 14.0293C6.40601 10.6845 9.10699 7.98496 12.4518 7.72417C13.9056 7.61047 15.2721 7.95227 16.4275 8.6181C19.3971 3.77042 16.8525 0 11.8044 0H0.974935C0.436304 0 0 0.436305 0 0.974936V19.7588C0 20.2377 0.348192 20.6485 0.821449 20.7217C3.05485 21.0677 5.18734 20.5604 7.5785 18.8087C6.56306 17.5091 6.00382 15.8363 6.14451 14.0293Z"/>"#,
    r##"<path fill="#003399" d="M16.4282 8.61755C16.2939 8.83642 16.1497 9.05812 15.9926 9.28125C12.6742 13.9989 9.9938 17.0395 7.57849 18.8096C8.83767 20.422 10.7982 21.4601 13.0018 21.4601C16.8006 21.4601 19.8803 18.3804 19.8803 14.5816C19.8803 12.0305 18.4904 9.80567 16.4282 8.61755Z"/></svg>"##
);

const TICK: &str = concat!(
    r#"<svg class="tick" viewBox="0 0 16 16" aria-hidden><circle cx="8" cy="8" r="8"/>"#,
    r#"<path d="M4.6 8.2 6.9 10.5 11.4 6"/></svg>"#
);

/// The prompt handed to someone who signed in without an agent driving it.
///
/// Brainpod deploys run through an agent, so the useful thing to give a person
/// at this point is the sentence that starts one — not a congratulation.
const PROMPT: &str = "Deploy this project to Brainpod using the Brainpod skill from github.com/brainpodnl/skills.\n\nI'm already signed in with the brainpod CLI. Work out what the project needs and hand me the live URL when it's up.";

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The live session console, if one answers.
///
/// Probed rather than trusted: `agent serve` advertises its URL in the session
/// file and never withdraws it, since no guard survives the kill that ends that
/// process. Sending the browser to an address nobody is listening on would turn
/// the one page confirming the sign-in into a connection error.
async fn handover() -> Option<String> {
    let url = crate::agent::console_url()?;
    let http = reqwest::Client::builder()
        .timeout(HANDOVER_PROBE)
        .build()
        .ok()?;
    let answered = http.get(&url).send().await.ok()?.status().is_success();
    answered.then_some(url)
}

/// A loopback server either answers immediately or is not there.
const HANDOVER_PROBE: Duration = Duration::from_millis(500);

/// Hands this tab to the live console, after a beat.
///
/// The beat is the point. This page is the only confirmation the user gets that
/// signing in worked, and a navigation they never see reads as the click having
/// done nothing.
fn refresh(handover: Option<&str>) -> String {
    handover
        .map(|url| {
            format!(
                r#"<meta http-equiv="refresh" content="2;url={}">"#,
                escape(url)
            )
        })
        .unwrap_or_default()
}

fn rail_html(steps: &[crate::agent::RailStep]) -> String {
    let rows = steps
        .iter()
        .map(|step| {
            let (class, mark) = match step.state.as_str() {
                "done" => ("is-done", TICK.to_owned()),
                "running" => ("is-now", r#"<span class="pip"></span>"#.to_owned()),
                _ => ("is-todo", r#"<span class="pip"></span>"#.to_owned()),
            };
            let when = if step.state == "running" {
                r#" <span class="when">now</span>"#
            } else {
                ""
            };
            let detail = step
                .detail
                .as_deref()
                .filter(|detail| !detail.is_empty())
                .map(|detail| format!("<p>{}</p>", escape(detail)))
                .unwrap_or_default();
            format!(
                r#"<div class="step {class}"><span class="mark">{mark}</span><div><h2>{}{when}</h2>{detail}</div></div>"#,
                escape(&step.label)
            )
        })
        .collect::<String>();
    format!(r#"<div class="rail">{rows}</div>"#)
}

fn human_html() -> String {
    format!(
        concat!(
            r#"<p class="section-label">Brainpod deploys are agent-driven. Hand this to yours, inside your project.</p>"#,
            r#"<div class="prompt">{prompt}</div>"#,
            r#"<div style="margin-top:1.5rem"><p class="section-label">Or stay in the terminal.</p><div class="cmds">"#,
            r#"<div class="cmd"><code>brainpod whoami</code><span>confirm the session</span></div>"#,
            r#"<div class="cmd"><code>brainpod pod list</code><span>see what you already run</span></div>"#,
            r#"<div class="cmd"><code>brainpod describe resource</code><span>the resource kinds you can compose</span></div>"#,
            "</div></div>"
        ),
        prompt = escape(PROMPT)
    )
}

fn stopped_html() -> &'static str {
    concat!(
        r#"<div class="cmds">"#,
        r#"<div class="cmd"><code>brainpod login</code><span>try again in this browser</span></div>"#,
        r#"<div class="cmd"><code>brainpod config set api-token &lt;token&gt;</code><span>no browser? use a dashboard token</span></div>"#,
        "</div>"
    )
}

impl IntoResponse for Page {
    fn into_response(self) -> Response {
        let celebrate = !matches!(self.shape, Shape::Stopped);
        let burst = if celebrate {
            format!(r#"<div class="confetti" aria-hidden>{CONFETTI}</div>"#)
        } else {
            String::new()
        };

        let content = match &self.shape {
            Shape::Agent(steps) => rail_html(steps),
            Shape::Human => human_html(),
            Shape::Stopped => stopped_html().to_owned(),
        };

        let body = format!(
            concat!(
                r#"<!doctype html><html lang="en"><head><meta charset="utf-8">"#,
                r#"<meta name="viewport" content="width=device-width,initial-scale=1">"#,
                "{refresh}",
                "<title>{headline} — Brainpod</title><style>{style}</style></head><body>",
                "{burst}",
                r#"<main class="card"><div class="chrome">{mark}<span class="wordmark">Brainpod</span>"#,
                r#"<span class="badge{badge_tone}"><span class="dot"></span>{badge}</span></div>"#,
                r#"<div class="body"><h1>{headline}</h1><p class="lead">{lead}</p>{content}</div>"#,
                r#"<div class="foot"><span>{foot}</span>"#,
                r#"<a class="sep" href="https://brainpod.io">brainpod.io &rarr;</a></div></main>"#,
                "</body></html>"
            ),
            refresh = refresh(self.handover.as_deref()),
            headline = self.headline,
            style = STYLE,
            burst = burst,
            mark = MARK,
            badge_tone = self.badge_tone,
            badge = self.badge,
            lead = self.lead,
            content = content,
            foot = self.foot,
        );

        (
            self.status,
            [
                ("cache-control", "no-store"),
                (
                    "content-security-policy",
                    "default-src 'none'; style-src 'unsafe-inline'",
                ),
                ("x-content-type-options", "nosniff"),
            ],
            Html(body),
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Callback, CallbackQuery, LoginOptions, authorization_notice, authorization_url,
        parse_callback_query, refresh,
    };

    #[test]
    fn hands_the_tab_over_only_when_a_console_answers() {
        assert!(refresh(None).is_empty());
        assert_eq!(
            refresh(Some("http://127.0.0.1:5173/9f2c/")),
            r#"<meta http-equiv="refresh" content="2;url=http://127.0.0.1:5173/9f2c/">"#
        );
    }

    /// The URL reaches this page from a file in the user's checkout.
    #[test]
    fn cannot_be_talked_out_of_the_refresh_attribute() {
        let escaped = refresh(Some(r#"http://127.0.0.1:1/" onload="x"#));
        assert!(!escaped.contains(r#"" onload"#), "{escaped}");
    }

    #[test]
    fn announces_the_authorization_url_as_a_single_json_line() {
        let notice = authorization_notice(
            "https://brainpod.io/cli/authorize?redirect_uri=http%3A%2F%2F127.0.0.1%3A1234%2Fcallback&state=state-value",
            LoginOptions {
                no_browser: true,
                json: true,
            },
        )
        .unwrap();

        assert_eq!(
            notice,
            "{\"event\":\"authorize\",\"url\":\"https://brainpod.io/cli/authorize?redirect_uri=http%3A%2F%2F127.0.0.1%3A1234%2Fcallback&state=state-value\",\"expiresInSeconds\":600}"
        );
    }

    #[test]
    fn announces_the_authorization_url_as_prose_without_json() {
        let notice =
            authorization_notice("https://brainpod.io/cli/authorize", LoginOptions::default())
                .unwrap();

        assert_eq!(
            notice,
            "Open this URL in a browser on this machine to authenticate: https://brainpod.io/cli/authorize"
        );
    }

    #[test]
    fn builds_authorization_url_with_encoded_callback() {
        let url = authorization_url(
            "https://brainpod.io",
            "http://127.0.0.1:1234/callback",
            "state-value",
        )
        .unwrap();

        assert_eq!(url.path(), "/cli/authorize");
        assert_eq!(
            url.query_pairs().collect::<Vec<_>>(),
            vec![
                (
                    "redirect_uri".into(),
                    "http://127.0.0.1:1234/callback".into()
                ),
                ("state".into(), "state-value".into()),
            ]
        );
    }

    #[test]
    fn accepts_token_with_matching_state() {
        let callback = parse_callback_query(
            CallbackQuery {
                token: Some("brain_example".to_owned()),
                state: Some("expected".to_owned()),
                error: None,
            },
            "expected",
        )
        .unwrap();

        assert!(matches!(callback, Callback::Token(token) if token == "brain_example"));
    }

    #[test]
    fn accepts_cancellation_with_matching_state() {
        let callback = parse_callback_query(
            CallbackQuery {
                token: None,
                state: Some("expected".to_owned()),
                error: Some("access_denied".to_owned()),
            },
            "expected",
        )
        .unwrap();

        assert!(matches!(callback, Callback::Cancelled));
    }

    #[test]
    fn rejects_mismatched_state() {
        let callback = parse_callback_query(
            CallbackQuery {
                token: Some("brain_example".to_owned()),
                state: Some("wrong".to_owned()),
                error: None,
            },
            "expected",
        );

        assert!(callback.is_err());
    }
}
