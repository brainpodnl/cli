use std::fmt;

use anyhow::{Context, Result, anyhow};
use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderValue};
use reqwest::{Method, Response, StatusCode, Url};
use serde_json::Value;

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    endpoint: Url,
}

pub struct EventWatch {
    client: Client,
    path: Vec<String>,
    query: Vec<(String, String)>,
    stream: EventStream,
}

struct EventStream {
    response: Response,
    buffer: Vec<u8>,
    last_event_id: Option<String>,
    finished: bool,
}

#[derive(Debug)]
pub struct EventStreamMessage {
    pub event: String,
    pub id: Option<String>,
    pub data: String,
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

    pub async fn get_event_watch(
        &self,
        path: &[&str],
        query: &[(&str, String)],
        last_event_id: Option<&str>,
    ) -> Result<EventWatch> {
        let stream = self.get_event_stream(path, query, last_event_id).await?;
        Ok(EventWatch {
            client: self.clone(),
            path: path.iter().map(|segment| (*segment).to_owned()).collect(),
            query: query
                .iter()
                .map(|(key, value)| ((*key).to_owned(), value.clone()))
                .collect(),
            stream,
        })
    }

    async fn get_event_stream(
        &self,
        path: &[&str],
        query: &[(&str, String)],
        last_event_id: Option<&str>,
    ) -> Result<EventStream> {
        let mut request = self
            .http
            .get(self.url(path)?)
            .query(query)
            .header(ACCEPT, "text/event-stream");
        if let Some(last_event_id) = last_event_id {
            let value = HeaderValue::from_str(last_event_id)
                .context("last event ID contains invalid header characters")?;
            request = request.header("Last-Event-ID", value);
        }

        let response = request
            .send()
            .await
            .context("Brainpod API request failed")?;
        let status = response.status();
        if !status.is_success() {
            let body = response_body(response).await?;
            return Err(ApiError { status, body }.into());
        }

        let is_event_stream = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .is_some_and(|value| value.trim().eq_ignore_ascii_case("text/event-stream"));
        if !is_event_stream {
            return Err(anyhow!("Brainpod API returned a non-event-stream response"));
        }

        Ok(EventStream {
            response,
            buffer: Vec::new(),
            last_event_id: last_event_id.map(str::to_owned),
            finished: false,
        })
    }

    async fn request(
        &self,
        method: Method,
        path: &[&str],
        query: &[(&str, String)],
        body: Option<&Value>,
    ) -> Result<Value> {
        let mut request = self.http.request(method, self.url(path)?).query(query);
        if let Some(body) = body {
            request = request.json(body);
        }

        let response = request
            .send()
            .await
            .context("Brainpod API request failed")?;
        let status = response.status();
        let body = response_body(response).await?;

        if !status.is_success() {
            return Err(ApiError { status, body }.into());
        }

        if body.is_string() {
            return Err(anyhow!("Brainpod API returned a non-JSON response"));
        }

        Ok(body)
    }

    fn url(&self, path: &[&str]) -> Result<Url> {
        let mut url = self.endpoint.clone();
        {
            let mut segments = url
                .path_segments_mut()
                .map_err(|_| anyhow!("Brainpod API endpoint cannot be a base URL"))?;
            segments.pop_if_empty();
            segments.extend(path.iter().copied());
        }
        Ok(url)
    }
}

impl EventWatch {
    pub async fn next(&mut self) -> Result<Option<EventStreamMessage>> {
        loop {
            match self.stream.next().await? {
                Some(message) if message.event == "end" => self.reconnect().await?,
                message => return Ok(message),
            }
        }
    }

    async fn reconnect(&mut self) -> Result<()> {
        let last_event_id = self.stream.last_event_id.clone();
        let path = self.path.iter().map(String::as_str).collect::<Vec<_>>();
        let query = self
            .query
            .iter()
            .map(|(key, value)| (key.as_str(), value.clone()))
            .collect::<Vec<_>>();
        self.stream = self
            .client
            .get_event_stream(&path, &query, last_event_id.as_deref())
            .await?;
        Ok(())
    }
}

impl EventStream {
    async fn next(&mut self) -> Result<Option<EventStreamMessage>> {
        loop {
            if let Some(record) = take_sse_record(&mut self.buffer) {
                if let Some(message) = self.parse_record(&record)? {
                    return Ok(Some(message));
                }
                continue;
            }

            if self.finished {
                if self.buffer.is_empty() {
                    return Ok(None);
                }
                let record = std::mem::take(&mut self.buffer);
                return self.parse_record(&record);
            }

            match self
                .response
                .chunk()
                .await
                .context("failed to read Brainpod event stream")?
            {
                Some(chunk) => self.buffer.extend_from_slice(&chunk),
                None => self.finished = true,
            }
        }
    }

    fn parse_record(&mut self, record: &[u8]) -> Result<Option<EventStreamMessage>> {
        parse_sse_record(record, &mut self.last_event_id)
    }
}

async fn response_body(response: Response) -> Result<Value> {
    let text = response
        .text()
        .await
        .context("failed to read Brainpod API response")?;
    if text.trim().is_empty() {
        Ok(Value::Null)
    } else {
        Ok(serde_json::from_str(&text).unwrap_or(Value::String(text)))
    }
}

fn parse_sse_record(
    record: &[u8],
    last_event_id: &mut Option<String>,
) -> Result<Option<EventStreamMessage>> {
    let record = std::str::from_utf8(record).context("Brainpod event stream is not UTF-8")?;
    let normalized = record.replace("\r\n", "\n").replace('\r', "\n");
    let mut event = "message";
    let mut data = Vec::new();
    let mut saw_data = false;

    for line in normalized.lines() {
        if line.starts_with(':') {
            continue;
        }
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "event" => event = value,
            "data" => {
                data.push(value);
                saw_data = true;
            }
            "id" if !value.contains('\0') => *last_event_id = Some(value.to_owned()),
            _ => {}
        }
    }

    if !saw_data {
        return Ok(None);
    }

    Ok(Some(EventStreamMessage {
        event: if event.is_empty() { "message" } else { event }.to_owned(),
        id: last_event_id.clone(),
        data: data.join("\n"),
    }))
}

fn take_sse_record(buffer: &mut Vec<u8>) -> Option<Vec<u8>> {
    let mut line_start = 0;
    let mut index = 0;

    while index < buffer.len() {
        let ending_length = match buffer[index] {
            b'\n' => 1,
            b'\r' if buffer.get(index + 1) == Some(&b'\n') => 2,
            b'\r' => 1,
            _ => {
                index += 1;
                continue;
            }
        };

        if index == line_start {
            let record = buffer[..index].to_vec();
            buffer.drain(..index + ending_length);
            return Some(record);
        }

        index += ending_length;
        line_start = index;
    }

    None
}

#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub body: Value,
}

impl ApiError {
    pub fn is_account_limit_error(&self) -> bool {
        self.body.pointer("/error/code").and_then(Value::as_str) == Some("VALIDATION_ERROR")
            && self
                .body
                .pointer("/error/details")
                .and_then(Value::as_array)
                .is_some_and(|details| {
                    details.iter().any(|detail| {
                        detail
                            .get("path")
                            .and_then(Value::as_str)
                            .is_some_and(|path| path.starts_with("limits."))
                    })
                })
    }
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

#[cfg(test)]
mod tests {
    use super::{parse_sse_record, take_sse_record};

    #[test]
    fn extracts_complete_records_across_chunks() {
        let mut buffer = b"event: event\r\ndata: {\"id\":\"one\"}".to_vec();
        assert!(take_sse_record(&mut buffer).is_none());

        buffer.extend_from_slice(b"\r\n\r\nevent: end\ndata: {}\n\n");
        assert_eq!(
            take_sse_record(&mut buffer).unwrap(),
            b"event: event\r\ndata: {\"id\":\"one\"}\r\n"
        );
        assert_eq!(
            take_sse_record(&mut buffer).unwrap(),
            b"event: end\ndata: {}\n"
        );
        assert!(buffer.is_empty());
    }

    #[test]
    fn parses_multiline_data_and_tracks_event_id() {
        let mut last_event_id = None;
        let message = parse_sse_record(
            b"id: event-1\nevent: event\ndata: first\ndata: second\n",
            &mut last_event_id,
        )
        .unwrap()
        .unwrap();

        assert_eq!(message.event, "event");
        assert_eq!(message.id.as_deref(), Some("event-1"));
        assert_eq!(message.data, "first\nsecond");
        assert_eq!(last_event_id.as_deref(), Some("event-1"));
    }

    #[test]
    fn ignores_heartbeat_records() {
        let mut last_event_id = Some("event-1".to_owned());
        let message = parse_sse_record(b": heartbeat\n", &mut last_event_id).unwrap();

        assert!(message.is_none());
        assert_eq!(last_event_id.as_deref(), Some("event-1"));
    }
}
