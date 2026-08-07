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

    finish_callback(request.response, Page::success(), shutdown_sender, server).await?;

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

struct Page {
    status: StatusCode,
    lead: &'static str,
    emphasis: &'static str,
    message: &'static str,
    tone: Tone,
    /// Where control goes back to, drawn as the second tile.
    handoff: Option<Handoff>,
    /// Steps the session still has ahead of it, for the agent-driven page.
    next: Vec<String>,
}

#[derive(Clone, Copy)]
enum Tone {
    Signed,
    Neutral,
    Failed,
}

/// What the user is being handed back to.
#[derive(Clone, Copy)]
enum Handoff {
    Console,
    Terminal,
}

impl Page {
    /// The page the user lands on after approving.
    ///
    /// An agent that set up a session console leaves steps behind it, and those
    /// turn this from a dead end into a handover: the user is told what happens
    /// next and where it is happening. Someone who ran `login` themselves has no
    /// such context, so they are pointed back at the terminal instead.
    fn success() -> Self {
        let next = crate::agent::upcoming();
        let handed_to_agent = !next.is_empty();
        Self {
            status: StatusCode::OK,
            lead: "You're",
            emphasis: "signed in",
            message: if handed_to_agent {
                "Your deploy is carrying on where you started it."
            } else {
                "Head back to the Brainpod CLI to pick up where you left off."
            },
            tone: Tone::Signed,
            handoff: Some(if handed_to_agent {
                Handoff::Console
            } else {
                Handoff::Terminal
            }),
            next,
        }
    }

    fn cancelled() -> Self {
        Self {
            status: StatusCode::OK,
            lead: "Sign-in",
            emphasis: "cancelled",
            message: "Nothing was changed. You can close this tab.",
            tone: Tone::Neutral,
            handoff: None,
            next: Vec::new(),
        }
    }

    fn invalid() -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            lead: "Sign-in",
            emphasis: "failed",
            message: "That response was not valid. Head back to the CLI and start again.",
            tone: Tone::Failed,
            handoff: None,
            next: Vec::new(),
        }
    }

    fn storage_failed() -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            lead: "Sign-in",
            emphasis: "failed",
            message: "Your token could not be saved. The CLI has the details.",
            tone: Tone::Failed,
            handoff: None,
            next: Vec::new(),
        }
    }

    fn unavailable() -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            lead: "Sign-in",
            emphasis: "failed",
            message: "The CLI stopped waiting for this. Head back and start again.",
            tone: Tone::Failed,
            handoff: None,
            next: Vec::new(),
        }
    }
}

const BRAINPOD_GLYPH: &str = concat!(
    r#"<svg viewBox="0 0 19.89 21.47" class="glyph" aria-hidden="true">"#,
    r#"<path fill="currentColor" d="M6.14451 14.0293C6.40601 10.6845 9.10699 7.98496 12.4518 7.72417C13.9056 7.61047 15.2721 7.95227 16.4275 8.6181C19.3971 3.77042 16.8525 0 11.8044 0H0.974935C0.436304 0 0 0.436305 0 0.974936V19.7588C0 20.2377 0.348192 20.6485 0.821449 20.7217C3.05485 21.0677 5.18734 20.5604 7.5785 18.8087C6.56306 17.5091 6.00382 15.8363 6.14451 14.0293Z"/>"#,
    r##"<path fill="#003399" d="M16.4282 8.61755C16.2939 8.83642 16.1497 9.05812 15.9926 9.28125C12.6742 13.9989 9.9938 17.0395 7.57849 18.8096C8.83767 20.422 10.7982 21.4601 13.0018 21.4601C16.8006 21.4601 19.8803 18.3804 19.8803 14.5816C19.8803 12.0305 18.4904 9.80567 16.4282 8.61755Z"/>"##,
    "</svg>"
);

/// The pane the agent is reporting into.
const CONSOLE_GLYPH: &str = concat!(
    r#"<svg viewBox="0 0 24 24" class="glyph line" aria-hidden="true">"#,
    r#"<rect x="2.6" y="4.2" width="18.8" height="15.6" rx="3.2"/>"#,
    r#"<path d="M2.6 9.1h18.8"/><circle cx="5.9" cy="6.7" r=".85" fill="currentColor" stroke="none"/>"#,
    r#"<path d="M6.4 12.9h6.4M6.4 16h9.6"/></svg>"#
);

const TERMINAL_GLYPH: &str = concat!(
    r#"<svg viewBox="0 0 24 24" class="glyph line" aria-hidden="true">"#,
    r#"<rect x="2.6" y="4.2" width="18.8" height="15.6" rx="3.2"/>"#,
    r#"<path d="M7.2 9.6l3.2 2.6-3.2 2.6M13 15.4h4.2"/></svg>"#
);

const ARROW: &str = concat!(
    r#"<svg viewBox="0 0 40 24" class="arrow" aria-hidden="true">"#,
    r#"<path d="M4 12h28M24 4.5 32.5 12 24 19.5"/></svg>"#
);

const STYLE: &str = r#"
:root{--bg:#fbfaf8;--fg:#16140f;--card:#fff;--sec:#f3f0ea;--border:#e5e0d7;--muted:#6b6660;--faint:#a19b92;--brand:#003399;--ink:#003399;--ok:#2f7d55;--fail:#b4402f;--dot:rgba(22,20,15,.09);--stage:color-mix(in srgb,#003399 8%,transparent);--veil:55%;--tileshadow:0 14px 34px -10px rgba(22,20,15,.34);--ring:rgba(22,20,15,.1)}
@media(prefers-color-scheme:dark){:root{--bg:#16140f;--fg:#fbfaf8;--card:#1d1b16;--sec:#262320;--border:#2d2a25;--muted:#a19b92;--faint:#78736c;--ink:#7da4ff;--ok:#5fbd8a;--fail:#d4614c;--dot:rgba(251,250,248,.07);--stage:color-mix(in srgb,#7da4ff 11%,transparent);--veil:30%;--tileshadow:0 14px 34px -10px rgba(0,0,0,.7);--ring:rgba(251,250,248,.12)}}
*{box-sizing:border-box}
html{-webkit-text-size-adjust:100%}
body{margin:0;min-height:100vh;display:grid;place-items:center;padding:40px 24px;color:var(--fg);background:var(--bg);background-image:radial-gradient(var(--dot) 1px,transparent 1px);background-size:22px 22px;font:400 15px/1.6 ui-sans-serif,system-ui,-apple-system,"Segoe UI",sans-serif;-webkit-font-smoothing:antialiased}
main{width:100%;max-width:31rem;display:flex;flex-direction:column;gap:26px}
.brand{display:flex;align-items:center;gap:9px;justify-content:center}
.brand .glyph{height:19px}
.word{font-weight:600;font-size:15px;letter-spacing:-.015em}
.stage{position:relative;display:flex;align-items:center;justify-content:center;gap:22px;padding:38px 24px;border-radius:22px;background:var(--stage);overflow:hidden}
.stage::after{content:"";position:absolute;inset:0;background:radial-gradient(circle 150px at 50% 50%,transparent 40%,color-mix(in srgb,var(--bg) var(--veil),transparent) 100%);pointer-events:none}
.tile{position:relative;width:84px;height:84px;border-radius:22px;display:grid;place-items:center;background:linear-gradient(145deg,var(--card),var(--sec));box-shadow:var(--tileshadow),inset 0 0 0 1px var(--ring)}
.tile .glyph{height:34px;width:auto;color:var(--fg)}
.tile .glyph.line{fill:none;stroke:currentColor;stroke-width:1.7;stroke-linecap:round;stroke-linejoin:round}
.arrow{width:34px;height:20px;fill:none;stroke:var(--fg);stroke-width:2.1;stroke-linecap:round;stroke-linejoin:round;opacity:.32}
.badge{position:absolute;right:-7px;bottom:-7px;width:27px;height:27px;border-radius:999px;display:grid;place-items:center;background:var(--ok);box-shadow:0 0 0 3.5px var(--bg)}
.badge.failed{background:var(--fail)}
.badge.neutral{background:var(--faint)}
.badge svg{width:14px;height:14px;fill:none;stroke:var(--bg);stroke-width:2.8;stroke-linecap:round;stroke-linejoin:round}
.say{display:flex;flex-direction:column;gap:9px;text-align:center}
h1{margin:0;font-size:clamp(30px,6vw,40px);line-height:1.04;letter-spacing:-.035em;font-weight:400;text-wrap:balance}
h1 b{font-weight:700}
p{margin:0;color:var(--muted);text-wrap:balance}
.next{background:var(--card);border:1px solid var(--border);border-radius:16px;overflow:hidden}
.next h2{margin:0;padding:12px 18px;border-bottom:1px solid var(--border);font:500 11px/1.4 ui-monospace,SFMono-Regular,Menlo,monospace;letter-spacing:.1em;text-transform:uppercase;color:var(--faint)}
ol{margin:0;padding:14px 18px 16px;list-style:none;display:flex;flex-direction:column;gap:10px}
li{display:flex;align-items:center;gap:12px;font-size:14.5px}
li i{flex:none;width:19px;height:19px;border-radius:999px;border:1.5px solid var(--border);font:500 10px/17px ui-monospace,SFMono-Regular,Menlo,monospace;text-align:center;color:var(--faint);font-style:normal}
li:first-child i{border-color:var(--ink);color:var(--ink)}
.note{text-align:center;font-size:13px;color:var(--faint)}
@media(prefers-reduced-motion:no-preference){main{animation:rise .5s cubic-bezier(.16,.84,.3,1)}}
@keyframes rise{from{opacity:0;transform:translateY(10px)}}
"#;

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

impl IntoResponse for Page {
    fn into_response(self) -> Response {
        let (badge, glyph) = match self.tone {
            Tone::Signed => ("badge", "M4.6 8.4 7.2 11 11.4 5.4"),
            Tone::Neutral => ("badge neutral", "M4.5 8h7"),
            Tone::Failed => ("badge failed", "M5 5l6 6M11 5l-6 6"),
        };
        let seal = format!(
            "<span class=\"{badge}\"><svg viewBox=\"0 0 16 16\" aria-hidden=\"true\"><path d=\"{glyph}\"/></svg></span>"
        );

        let stage = match self.handoff {
            Some(handoff) => format!(
                "<div class=\"tile\">{BRAINPOD_GLYPH}</div>{ARROW}<div class=\"tile\">{}{seal}</div>",
                match handoff {
                    Handoff::Console => CONSOLE_GLYPH,
                    Handoff::Terminal => TERMINAL_GLYPH,
                }
            ),
            None => format!("<div class=\"tile\">{BRAINPOD_GLYPH}{seal}</div>"),
        };

        let next = if self.next.is_empty() {
            String::new()
        } else {
            let items = self
                .next
                .iter()
                .enumerate()
                .map(|(index, label)| format!("<li><i>{}</i>{}</li>", index + 1, escape(label)))
                .collect::<String>();
            format!("<section class=\"next\"><h2>What happens next</h2><ol>{items}</ol></section>")
        };

        let body = format!(
            concat!(
                "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">",
                "<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">",
                "<title>{lead} {emphasis} · Brainpod</title><style>{style}</style></head><body><main>",
                "<div class=\"brand\">{mark}<span class=\"word\">brainpod</span></div>",
                "<div class=\"stage\">{stage}</div>",
                "<div class=\"say\"><h1>{lead} <b>{emphasis}</b></h1><p>{message}</p></div>",
                "{next}<p class=\"note\">You can close this tab.</p>",
                "</main></body></html>"
            ),
            lead = self.lead,
            emphasis = self.emphasis,
            style = STYLE,
            mark = BRAINPOD_GLYPH,
            stage = stage,
            message = self.message,
            next = next,
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
        parse_callback_query,
    };

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
