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
    title: &'static str,
    message: &'static str,
    tone: Tone,
    /// Steps the session still has ahead of it, for the agent-driven page.
    next: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tone {
    Signed,
    Neutral,
    Failed,
}

impl Page {
    /// The page the user lands on after approving.
    ///
    /// An agent that set up a session console leaves steps behind it, and those
    /// turn this from a dead end into a handover: the user is told what happens
    /// next and where it is happening. Someone who ran `login` themselves has
    /// no such context, so they get told to go back to the terminal instead.
    fn success() -> Self {
        let next = crate::agent::upcoming();
        Self {
            status: StatusCode::OK,
            title: "You're signed in",
            message: if next.is_empty() {
                "Head back to the Brainpod CLI. You can close this tab."
            } else {
                "Your deploy is carrying on where you started it. You can close this tab."
            },
            tone: Tone::Signed,
            next,
        }
    }

    fn cancelled() -> Self {
        Self {
            status: StatusCode::OK,
            title: "Sign-in cancelled",
            message: "Nothing was changed. You can close this tab.",
            tone: Tone::Neutral,
            next: Vec::new(),
        }
    }

    fn invalid() -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            title: "Sign-in failed",
            message: "That response was not valid. Head back to the CLI and start again.",
            tone: Tone::Failed,
            next: Vec::new(),
        }
    }

    fn storage_failed() -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            title: "Sign-in failed",
            message: "Your token could not be saved. The CLI has the details.",
            tone: Tone::Failed,
            next: Vec::new(),
        }
    }

    fn unavailable() -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            title: "Sign-in failed",
            message: "The CLI stopped waiting for this. Head back and start again.",
            tone: Tone::Failed,
            next: Vec::new(),
        }
    }
}

const MARK: &str = concat!(
    r#"<svg class="mark" viewBox="0 0 19.89 21.47" aria-hidden="true">"#,
    r#"<path fill="currentColor" d="M6.14451 14.0293C6.40601 10.6845 9.10699 7.98496 12.4518 7.72417C13.9056 7.61047 15.2721 7.95227 16.4275 8.6181C19.3971 3.77042 16.8525 0 11.8044 0H0.974935C0.436304 0 0 0.436305 0 0.974936V19.7588C0 20.2377 0.348192 20.6485 0.821449 20.7217C3.05485 21.0677 5.18734 20.5604 7.5785 18.8087C6.56306 17.5091 6.00382 15.8363 6.14451 14.0293Z"/>"#,
    r##"<path fill="#003399" d="M16.4282 8.61755C16.2939 8.83642 16.1497 9.05812 15.9926 9.28125C12.6742 13.9989 9.9938 17.0395 7.57849 18.8096C8.83767 20.422 10.7982 21.4601 13.0018 21.4601C16.8006 21.4601 19.8803 18.3804 19.8803 14.5816C19.8803 12.0305 18.4904 9.80567 16.4282 8.61755Z"/>"##,
    "</svg>"
);

const STYLE: &str = r#"
:root{--bg:#fbfaf8;--fg:#16140f;--card:#fff;--border:#e5e0d7;--muted:#6b6660;--faint:#a19b92;--brand:#003399;--ok:#2f7d55;--fail:#b4402f}
@media(prefers-color-scheme:dark){:root{--bg:#16140f;--fg:#fbfaf8;--card:#1d1b16;--border:#2d2a25;--muted:#a19b92;--faint:#78736c;--brand:#7da4ff;--ok:#5fbd8a;--fail:#d4614c}}
*{box-sizing:border-box}
body{margin:0;min-height:100vh;display:grid;place-items:center;padding:32px;background:var(--bg);color:var(--fg);font:400 15px/1.6 ui-sans-serif,system-ui,-apple-system,"Segoe UI",sans-serif;-webkit-font-smoothing:antialiased}
main{width:100%;max-width:30rem;display:flex;flex-direction:column;gap:22px}
.brand{display:flex;align-items:center;gap:9px;justify-content:center;color:var(--fg)}
.mark{height:20px;width:auto}
.word{font-weight:600;font-size:15px;letter-spacing:-.015em}
.panel{background:color-mix(in srgb,var(--brand) 8%,transparent);border-radius:18px;padding:30px 26px;text-align:center;display:flex;flex-direction:column;align-items:center;gap:12px}
.seal{width:42px;height:42px;border-radius:999px;display:grid;place-items:center;background:var(--ok)}
.seal.failed{background:var(--fail)}
.seal.neutral{background:var(--faint)}
.seal svg{width:20px;height:20px;display:block}
.seal path{stroke:var(--bg);stroke-width:2.6;fill:none;stroke-linecap:round;stroke-linejoin:round}
h1{margin:0;font-size:26px;line-height:1.12;letter-spacing:-.028em;font-weight:600;text-wrap:balance}
p{margin:0;color:var(--muted);text-wrap:balance}
.next{background:var(--card);border:1px solid var(--border);border-radius:14px;overflow:hidden}
.next h2{margin:0;padding:11px 16px;border-bottom:1px solid var(--border);font:500 11px/1.4 ui-monospace,SFMono-Regular,Menlo,monospace;letter-spacing:.09em;text-transform:uppercase;color:var(--faint)}
ol{margin:0;padding:12px 16px 14px;list-style:none;display:flex;flex-direction:column;gap:9px}
li{display:flex;align-items:baseline;gap:11px;font-size:14.5px}
li span{flex:none;width:17px;height:17px;border-radius:999px;border:1.5px solid var(--border);font:500 10px/16px ui-monospace,SFMono-Regular,Menlo,monospace;text-align:center;color:var(--faint);align-self:center}
li:first-child span{border-color:var(--brand);color:var(--brand)}
.note{text-align:center;font-size:13px;color:var(--faint)}
"#;

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

impl IntoResponse for Page {
    fn into_response(self) -> Response {
        let (seal, glyph) = match self.tone {
            Tone::Signed => ("seal", "M5 10.4 8.4 13.8 15 6.2"),
            Tone::Neutral => ("seal neutral", "M5 10h10"),
            Tone::Failed => ("seal failed", "M6 6l8 8M14 6l-8 8"),
        };

        let next = if self.next.is_empty() {
            String::new()
        } else {
            let items = self
                .next
                .iter()
                .enumerate()
                .map(|(index, label)| {
                    format!("<li><span>{}</span>{}</li>", index + 1, escape(label))
                })
                .collect::<String>();
            format!("<section class=\"next\"><h2>What happens next</h2><ol>{items}</ol></section>")
        };

        let body = format!(
            concat!(
                "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">",
                "<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">",
                "<title>{title} · Brainpod</title><style>{style}</style></head><body><main>",
                "<div class=\"brand\">{mark}<span class=\"word\">brainpod</span></div>",
                "<div class=\"panel\"><div class=\"{seal}\">",
                "<svg viewBox=\"0 0 20 20\" aria-hidden=\"true\"><path d=\"{glyph}\"/></svg></div>",
                "<h1>{title}</h1><p>{message}</p></div>{next}",
                "<p class=\"note\">Nothing on this page needs saving.</p>",
                "</main></body></html>"
            ),
            title = self.title,
            style = STYLE,
            mark = MARK,
            seal = seal,
            glyph = glyph,
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
