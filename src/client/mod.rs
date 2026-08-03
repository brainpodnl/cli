use std::fmt;

use anyhow::{Context, Result, anyhow};
use reqwest::{Method, StatusCode, Url};
use serde_json::Value;

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    endpoint: Url,
}

impl Client {
    pub fn try_new(endpoint: &str, api_key: &str) -> Result<Self> {
        if api_key.trim().is_empty() {
            return Err(anyhow!("API key cannot be empty"));
        }

        let endpoint = Url::parse(endpoint).context("invalid Brainpod API endpoint")?;
        let mut headers = reqwest::header::HeaderMap::new();
        let authorization = reqwest::header::HeaderValue::from_str(&format!("Bearer {api_key}"))
            .context("API key contains invalid header characters")?;
        headers.insert(reqwest::header::AUTHORIZATION, authorization);
        let http = reqwest::Client::builder()
            .default_headers(headers)
            .build()
            .context("failed to create HTTP client")?;

        Ok(Self { http, endpoint })
    }

    pub async fn get(&self, path: &[&str], query: &[(&str, String)]) -> Result<Value> {
        self.request(Method::GET, path, query, None).await
    }

    pub async fn post(
        &self,
        path: &[&str],
        query: &[(&str, String)],
        body: Option<&Value>,
    ) -> Result<Value> {
        self.request(Method::POST, path, query, body).await
    }

    pub async fn put(&self, path: &[&str], body: &Value) -> Result<Value> {
        self.request(Method::PUT, path, &[], Some(body)).await
    }

    pub async fn delete(&self, path: &[&str]) -> Result<Value> {
        self.request(Method::DELETE, path, &[], None).await
    }

    async fn request(
        &self,
        method: Method,
        path: &[&str],
        query: &[(&str, String)],
        body: Option<&Value>,
    ) -> Result<Value> {
        let mut url = self.endpoint.clone();
        {
            let mut segments = url
                .path_segments_mut()
                .map_err(|_| anyhow!("Brainpod API endpoint cannot be a base URL"))?;
            segments.pop_if_empty();
            segments.extend(path.iter().copied());
        }

        let mut request = self.http.request(method, url).query(query);
        if let Some(body) = body {
            request = request.json(body);
        }

        let response = request
            .send()
            .await
            .context("Brainpod API request failed")?;
        let status = response.status();
        let text = response
            .text()
            .await
            .context("failed to read Brainpod API response")?;
        let body = if text.trim().is_empty() {
            Value::Null
        } else {
            match serde_json::from_str(&text) {
                Ok(body) => body,
                Err(_) => Value::String(text),
            }
        };

        if !status.is_success() {
            return Err(ApiError { status, body }.into());
        }

        if body.is_string() {
            return Err(anyhow!("Brainpod API returned a non-JSON response"));
        }

        Ok(body)
    }
}

#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub body: Value,
}

impl fmt::Display for ApiError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let error = self.body.get("error");
        let code = error
            .and_then(|value| value.get("code"))
            .and_then(Value::as_str);
        let message = error
            .and_then(|value| value.get("message"))
            .and_then(Value::as_str);
        let request_id = error
            .and_then(|value| value.get("requestId"))
            .and_then(Value::as_str);

        write!(formatter, "Brainpod API returned {}", self.status)?;
        if let Some(code) = code {
            write!(formatter, " ({code})")?;
        }
        if let Some(message) = message {
            write!(formatter, ": {message}")?;
        }
        if let Some(request_id) = request_id {
            write!(formatter, " [request ID: {request_id}]")?;
        }
        Ok(())
    }
}

impl std::error::Error for ApiError {}
