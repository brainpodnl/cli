use std::collections::HashSet;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, Subcommand};
use reqwest::{Method, StatusCode, Url};
use serde_json::{Value, json};

use crate::output::{CommandOutput, View};

const MAX_OBJECT: usize = 25 * 1024 * 1024;
const MAX_XML: usize = 4 * 1024 * 1024;

#[derive(Debug, Args)]
pub struct S3Args {
    #[command(subcommand)]
    command: S3Command,
}

#[derive(Debug, Subcommand)]
enum S3Command {
    /// List buckets, or one level beneath s3://bucket/prefix (all pages)
    Ls { path: Option<String> },
    /// Copy one object (at most 25 MiB); remote paths require s3://bucket/key
    Cp { source: String, destination: String },
    /// Copy one object, then delete its source only after success
    Mv { source: String, destination: String },
    /// Upload one local file, or multipart stdin (-) with an explicit producer commit file
    Upload {
        source: String,
        destination: String,
        /// Required for stdin: private per-run file containing commit followed by a newline
        #[arg(long)]
        require_success_file: Option<PathBuf>,
        /// Object metadata; repeat KEY=VALUE
        #[arg(long = "metadata", value_name = "KEY=VALUE")]
        metadata: Vec<String>,
    },
    /// Stream a remote object to stdout (-), without the cp size limit
    Download { source: String, destination: String },
    /// Delete exactly one key, never its prefix contents
    Rm { path: String },
}

#[derive(Debug, PartialEq, Eq)]
struct Remote {
    bucket: String,
    key: String,
}

impl Remote {
    fn parse(value: &str) -> Result<Option<Self>> {
        let Some(path) = value.strip_prefix("s3://") else {
            return Ok(None);
        };
        let (bucket, key) = path.split_once('/').unwrap_or((path, ""));
        if bucket.is_empty()
            || !bucket
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.')
            || matches!(bucket, "." | "..")
        {
            bail!("invalid logical bucket in S3 path");
        }
        Ok(Some(Self {
            bucket: bucket.to_owned(),
            key: key.to_owned(),
        }))
    }

    fn object(&self) -> Result<()> {
        if self.key.is_empty() || self.key.len() > 1024 {
            bail!("object paths require an explicit key of 1–1024 bytes");
        }
        if self.key.split('/').any(|s| matches!(s, "." | "..")) {
            bail!(
                "dot-segment object keys are unsupported because URL handling would normalize them"
            );
        }
        Ok(())
    }
}

struct Gateway<'a> {
    http: reqwest::Client,
    sdk: aws_sdk_s3::Client,
    origin: Url,
    token: &'a str,
}

impl<'a> Gateway<'a> {
    fn new(endpoint: &str, pod: &str, token: &'a str) -> Result<Self> {
        let mut origin = Url::parse(endpoint).context("invalid S3 endpoint")?;
        if !matches!(origin.scheme(), "http" | "https")
            || !origin.username().is_empty()
            || origin.password().is_some()
            || origin.path() != "/"
            || origin.query().is_some()
            || origin.fragment().is_some()
        {
            bail!(
                "S3 endpoint must be an HTTP(S) origin without credentials, path, query, or fragment"
            );
        }
        if pod.is_empty() || !pod.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
            bail!("S3 requires a Pod hostname name, not an ID or URL");
        }
        let host = origin
            .host_str()
            .ok_or_else(|| anyhow!("S3 endpoint has no hostname"))?;
        origin.set_host(Some(&format!("{pod}.{host}")))?;
        let http = crate::http_client_builder()?
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(30))
            .read_timeout(Duration::from_secs(60))
            .build()?;
        let sdk = sdk::client(http.clone(), &origin, token)?;
        Ok(Self {
            http,
            sdk,
            origin,
            token,
        })
    }

    fn url(&self, remote: Option<&Remote>) -> Result<Url> {
        let mut url = self.origin.clone();
        if let Some(remote) = remote {
            let mut path = format!("/{}", remote.bucket);
            if !remote.key.is_empty() {
                remote.object()?;
                path.push('/');
                path.push_str(&aws_smithy_http::label::fmt_string(
                    &remote.key,
                    aws_smithy_http::label::EncodingStrategy::Greedy,
                ));
            }
            url.set_path(&path);
        }
        Ok(url)
    }

    async fn delete(&self, remote: &Remote) -> Result<()> {
        remote.object()?;
        self.sdk
            .delete_object()
            .bucket(&remote.bucket)
            .key(sdk::key(&remote.key))
            .customize()
            .interceptor(sdk::LiteralKey::new(&remote.key))
            .send()
            .await
            .map_err(sdk::error)?;
        Ok(())
    }

    async fn download(&self, remote: &Remote) -> Result<Vec<u8>> {
        bounded_body(self.provider_response(remote, false).await?, MAX_OBJECT).await
    }

    async fn provider_response(
        &self,
        remote: &Remote,
        streaming: bool,
    ) -> Result<reqwest::Response> {
        remote.object()?;
        let response = self
            .http
            .get(self.url(Some(remote))?)
            .bearer_auth(self.token)
            .timeout(Duration::from_secs(120))
            .send()
            .await
            .map_err(|_| anyhow!("S3 gateway request failed"))?;
        let response = expect(response, StatusCode::TEMPORARY_REDIRECT).await?;
        let location = response
            .headers()
            .get(reqwest::header::LOCATION)
            .ok_or_else(|| anyhow!("gateway redirect has no Location"))?
            .to_str()?;
        let url = provider_url(location)?;
        let mut request = self.http.get(url);
        if !streaming {
            request = request.timeout(Duration::from_secs(120));
        }
        let response = request
            .send()
            .await
            .map_err(|_| anyhow!("provider download request failed"))?;
        expect(response, StatusCode::OK).await
    }

    async fn copy(
        &self,
        source: &str,
        destination: &str,
        moving: bool,
        upload: bool,
    ) -> Result<Value> {
        let src = Remote::parse(source)?;
        let dst = Remote::parse(destination)?;
        if src.is_none() && dst.is_none() {
            bail!("at least one path must use s3://bucket/key");
        }
        if upload && (src.is_some() || dst.is_none()) {
            bail!("upload requires a local source and s3://bucket/key destination");
        }
        if src.is_some() && src == dst {
            bail!("source and destination must differ");
        }
        if let Some(dst) = &dst {
            dst.object()?;
        }
        if dst.is_none() && Path::new(destination).exists() {
            bail!("local destination already exists; choose a new file path");
        }
        let bytes = match &src {
            Some(src) => self.download(src).await?,
            None => {
                let file = std::fs::File::open(source).context("failed to open local source")?;
                if !file.metadata()?.is_file() {
                    bail!("source must be a regular file; recursive transfers are unsupported");
                }
                let mut bytes = Vec::new();
                file.take((MAX_OBJECT + 1) as u64).read_to_end(&mut bytes)?;
                check_size(bytes.len(), MAX_OBJECT)?;
                bytes
            }
        };
        let size = bytes.len();
        match dst {
            Some(dst) => {
                self.sdk
                    .put_object()
                    .bucket(&dst.bucket)
                    .key(sdk::key(&dst.key))
                    .content_type("application/octet-stream")
                    .body(aws_sdk_s3::primitives::ByteStream::from(bytes))
                    .customize()
                    .interceptor(sdk::LiteralKey::new(&dst.key))
                    .send()
                    .await
                    .map_err(sdk::error)?;
            }
            None => {
                let path = Path::new(destination);
                let parent = path
                    .parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .unwrap_or(Path::new("."));
                let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
                temporary.write_all(&bytes)?;
                temporary.as_file().sync_all()?;
                temporary
                    .persist_noclobber(path)
                    .context("failed to save local destination")?;
            }
        }
        if moving {
            let deleted = match src {
                Some(src) => self.delete(&src).await,
                None => std::fs::remove_file(source).context("failed to delete local source"),
            };
            deleted
                .context("copy succeeded, but source deletion failed; both copies may remain")?;
        }
        Ok(
            json!({"operation": if moving { "mv" } else { "cp" }, "source": source, "destination": destination, "size": size}),
        )
    }

    async fn list(&self, path: Option<&str>) -> Result<Value> {
        let remote = path
            .map(|p| {
                Remote::parse(p)?.ok_or_else(|| anyhow!("listing path must use s3://bucket/prefix"))
            })
            .transpose()?;
        let mut items = Vec::new();
        let mut token: Option<String> = None;
        let mut seen = HashSet::new();
        loop {
            token = if let Some(remote) = &remote {
                let page = self
                    .sdk
                    .list_objects_v2()
                    .bucket(&remote.bucket)
                    .prefix(&remote.key)
                    .delimiter("/")
                    .encoding_type(aws_sdk_s3::types::EncodingType::Url)
                    .max_keys(1000)
                    .set_continuation_token(token)
                    .send()
                    .await
                    .map_err(sdk::error)?;
                let encoded = match page.encoding_type().map(|encoding| encoding.as_str()) {
                    Some("url") => true,
                    None => false,
                    _ => bail!("unsupported gateway listing encoding"),
                };
                for object in page.contents() {
                    let key = object
                        .key()
                        .ok_or_else(|| anyhow!("listing object has no key"))?;
                    let size = object
                        .size()
                        .filter(|size| *size >= 0)
                        .ok_or_else(|| anyhow!("listing object has no valid size"))?;
                    let modified = object
                        .last_modified()
                        .map(|date| date.fmt(aws_smithy_types::date_time::Format::DateTime))
                        .transpose()?;
                    items.push(json!({"kind": "object", "key": decode_field(key, encoded)?, "size": size, "lastModified": modified}));
                }
                for prefix in page.common_prefixes() {
                    let prefix = prefix
                        .prefix()
                        .ok_or_else(|| anyhow!("listing prefix has no key"))?;
                    items.push(json!({"kind": "prefix", "key": decode_field(prefix, encoded)?}));
                }
                let next = if page.is_truncated() == Some(true) {
                    page.next_continuation_token()
                        .filter(|token| !token.is_empty())
                        .map(str::to_owned)
                } else {
                    None
                };
                if page.is_truncated() == Some(true) && next.is_none() {
                    bail!("truncated gateway listing has no continuation token");
                }
                next
            } else {
                let page = self
                    .sdk
                    .list_buckets()
                    .set_continuation_token(token)
                    .send()
                    .await
                    .map_err(sdk::error)?;
                for bucket in page.buckets() {
                    let name = bucket
                        .name()
                        .ok_or_else(|| anyhow!("listing bucket has no name"))?;
                    items.push(json!({"kind": "bucket", "name": name}));
                }
                page.continuation_token()
                    .filter(|token| !token.is_empty())
                    .map(str::to_owned)
            };
            let Some(next) = &token else {
                break;
            };
            if !seen.insert(next.clone()) {
                bail!("gateway repeated a pagination token");
            }
        }
        Ok(json!({"items": items}))
    }
}

fn provider_url(location: &str) -> Result<Url> {
    let url = Url::parse(location).context("invalid provider redirect URL")?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        bail!("provider redirect must be an absolute HTTPS URL without credentials or fragment");
    }
    Ok(url)
}

fn check_size(size: usize, max: usize) -> Result<()> {
    if size > max {
        bail!(
            "response or file exceeds the {} byte limit; object transfers support at most 25 MiB, without multipart",
            max
        );
    }
    Ok(())
}

async fn bounded_body(mut response: reqwest::Response, max: usize) -> Result<Vec<u8>> {
    if response.content_length().is_some_and(|n| n > max as u64) {
        check_size(max + 1, max)?;
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| anyhow!("failed to read transfer body"))?
    {
        check_size(bytes.len().saturating_add(chunk.len()), max)?;
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

async fn expect(response: reqwest::Response, status: StatusCode) -> Result<reqwest::Response> {
    if response.status() == status {
        return Ok(response);
    }
    let actual = response.status();
    bail!("S3 request returned HTTP {}", actual.as_u16())
}

fn decode_field(value: &str, encoded: bool) -> Result<String> {
    if !encoded {
        return Ok(value.to_owned());
    }
    let bytes = value.as_bytes();
    let mut decoded = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => decoded.push(b' '),
            b'%' => {
                let pair = bytes
                    .get(index + 1..index + 3)
                    .ok_or_else(|| anyhow!("invalid listing percent escape"))?;
                let hex = std::str::from_utf8(pair)?;
                decoded
                    .push(u8::from_str_radix(hex, 16).context("invalid listing percent escape")?);
                index += 2;
            }
            byte => decoded.push(byte),
        }
        index += 1;
    }
    String::from_utf8(decoded).context("listing field is not UTF-8")
}

pub async fn handle(args: S3Args, endpoint: &str, pod: &str, token: &str) -> Result<CommandOutput> {
    let gateway = Gateway::new(endpoint, pod, token)?;
    let value = match args.command {
        S3Command::Ls { path } => gateway.list(path.as_deref()).await?,
        S3Command::Cp {
            source,
            destination,
        } => gateway.copy(&source, &destination, false, false).await?,
        S3Command::Mv {
            source,
            destination,
        } => gateway.copy(&source, &destination, true, false).await?,
        S3Command::Upload {
            source,
            destination,
            require_success_file,
            metadata,
        } => {
            if source == "-" {
                let remote = Remote::parse(&destination)?
                    .ok_or_else(|| anyhow!("upload requires s3://bucket/key"))?;
                let require_success_file = require_success_file.ok_or_else(|| anyhow!("stdin uploads require --require-success-file; pipeline EOF alone cannot prove producer success"))?;
                gateway
                    .upload_stream(
                        multipart::cancellable_stdin(),
                        &remote,
                        &require_success_file,
                        &metadata,
                        multipart::cancellation()?,
                    )
                    .await?
            } else {
                if require_success_file.is_some() || !metadata.is_empty() {
                    bail!("--require-success-file and --metadata require streaming stdin (-)");
                }
                gateway.copy(&source, &destination, false, true).await?
            }
        }
        S3Command::Download {
            source,
            destination,
        } => {
            if destination != "-" {
                bail!(
                    "streaming download destination must be -; use cp for a bounded file download"
                );
            }
            let remote = Remote::parse(&source)?
                .ok_or_else(|| anyhow!("download requires s3://bucket/key"))?;
            let mut stdout = multipart::cancellable_output(std::io::stdout());
            tokio::select! {
                result = gateway.download_stream(&remote, &mut stdout) => { result?; }
                _ = multipart::cancellation()? => bail!("download cancelled; stdout may contain a partial object"),
            }
            return Ok(CommandOutput::Silent);
        }
        S3Command::Rm { path } => {
            let remote =
                Remote::parse(&path)?.ok_or_else(|| anyhow!("rm requires s3://bucket/key"))?;
            gateway.delete(&remote).await?;
            json!({"operation": "rm", "path": path})
        }
    };
    Ok(CommandOutput::new(value, View::S3))
}

pub fn render(value: &Value) -> Vec<String> {
    if let Some(items) = value.get("items").and_then(Value::as_array) {
        return items
            .iter()
            .map(|item| match item["kind"].as_str() {
                Some("bucket") => format!("Bucket: {}", item["name"].as_str().unwrap_or_default()),
                Some("prefix") => format!("Prefix: {}", item["key"].as_str().unwrap_or_default()),
                _ => format!(
                    "{}\t{}",
                    item["size"],
                    item["key"].as_str().unwrap_or_default()
                ),
            })
            .collect();
    }
    vec![format!(
        "{}: {}",
        value["operation"].as_str().unwrap_or_default(),
        value
            .get("destination")
            .or_else(|| value.get("path"))
            .and_then(Value::as_str)
            .unwrap_or_default()
    )]
}

mod multipart;
mod sdk;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod sdk_tests;
