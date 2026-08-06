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

    eprintln!("{}", authorization_notice(authorize_url.as_str(), options)?);
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

/// Renders the single stderr line that tells the caller where to authenticate.
///
/// The authorization URL redirects to a loopback address, so it only completes
/// in a browser running on the same machine as the CLI.
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
}

impl Page {
    const fn success() -> Self {
        Self {
            status: StatusCode::OK,
            title: "Authentication successful",
            message: "You can close this window and return to the Brainpod CLI.",
        }
    }

    const fn cancelled() -> Self {
        Self {
            status: StatusCode::OK,
            title: "Authentication cancelled",
            message: "No changes were made. You can close this window.",
        }
    }

    const fn invalid() -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            title: "Authentication failed",
            message: "The authentication response was invalid. Return to the CLI and try again.",
        }
    }

    const fn storage_failed() -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            title: "Authentication failed",
            message: "The CLI could not store the API token. Return to the CLI for details.",
        }
    }

    const fn unavailable() -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            title: "Authentication failed",
            message: "The CLI is no longer waiting for an authentication response.",
        }
    }
}

impl IntoResponse for Page {
    fn into_response(self) -> Response {
        let body = format!(
            "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>{}</title></head><body><main><h1>{}</h1><p>{}</p></main></body></html>",
            self.title, self.title, self.message
        );
        (
            self.status,
            [
                ("cache-control", "no-store"),
                ("content-security-policy", "default-src 'none'"),
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
