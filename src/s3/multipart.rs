use std::future::Future;

use aws_sdk_s3::types::{CompletedMultipartUpload, CompletedPart};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};

use super::*;

const PART_SIZE: usize = 8 * 1024 * 1024;
const MAX_PARTS: usize = 10_000;
pub(super) const MAX_COMPLETION: usize = 2 * 1024 * 1024;
pub(super) fn cancellation() -> Result<impl Future<Output = ()>> {
    #[cfg(unix)]
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    Ok(async move {
        #[cfg(unix)]
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = terminate.recv() => {},
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
        }
    })
}

fn metadata_headers(metadata: &[String]) -> Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    let mut total = 0;
    for entry in metadata {
        let (key, value) = entry
            .split_once('=')
            .ok_or_else(|| anyhow!("metadata must use KEY=VALUE"))?;
        if key.is_empty() {
            bail!("metadata keys must not be empty");
        }
        if value.is_empty() || !value.bytes().all(|b| (0x20..=0x7e).contains(&b)) {
            bail!("metadata values must be nonempty printable ASCII");
        }
        total += key.len() + value.len();
        if total > 2048 {
            bail!("metadata names and values exceed 2048 bytes");
        }
        let name = HeaderName::from_bytes(format!("x-amz-meta-{key}").as_bytes())
            .context("invalid metadata key")?;
        if headers
            .insert(name, HeaderValue::from_str(value)?)
            .is_some()
        {
            bail!("duplicate metadata key: {key}");
        }
    }
    Ok(headers)
}

fn validate_etag(etag: &str) -> Result<()> {
    if !(3..=256).contains(&etag.len())
        || !etag.starts_with('"')
        || !etag.ends_with('"')
        || !etag[1..etag.len() - 1]
            .bytes()
            .all(|b| (0x21..=0x7e).contains(&b) && b != b'"')
    {
        bail!("multipart response requires a quoted ETag of at most 256 bytes");
    }
    Ok(())
}

impl Gateway<'_> {
    pub(super) async fn upload_stream<R: AsyncRead + Unpin, C: Future<Output = ()>>(
        &self,
        mut input: R,
        remote: &Remote,
        success_file: &Path,
        metadata: &[String],
        cancelled: C,
    ) -> Result<Value> {
        remote.object()?;
        let metadata = metadata_headers(metadata)?;
        let mut upload_id = None;
        let mut completing = false;
        let result = tokio::select! {
            biased;
            _ = cancelled => Err(anyhow!("multipart upload cancelled")),
            result = self.multipart_transfer(&mut input, remote, success_file, metadata, &mut upload_id, &mut completing) => result,
        };
        match result {
            Ok(value) => Ok(value),
            Err(error) => {
                if let Some(upload_id) = upload_id {
                    let abort = async {
                        self.sdk
                            .abort_multipart_upload()
                            .bucket(&remote.bucket)
                            .key(sdk::key(&remote.key))
                            .upload_id(&upload_id)
                            .customize()
                            .interceptor(sdk::LiteralKey::new(&remote.key))
                            .send()
                            .await
                            .map_err(sdk::error)?;
                        Ok::<_, anyhow::Error>(())
                    };
                    if !matches!(
                        tokio::time::timeout(Duration::from_secs(30), abort).await,
                        Ok(Ok(()))
                    ) {
                        eprintln!("warning: multipart abort failed; incomplete upload may remain");
                    }
                } else {
                    eprintln!(
                        "warning: initiation did not yield an upload ID; any allocated multipart upload cannot be aborted"
                    );
                }
                if completing {
                    Err(error.context("completion outcome may be uncertain; inspect the exact key before retrying"))
                } else {
                    Err(error)
                }
            }
        }
    }

    async fn multipart_transfer<R: AsyncRead + Unpin>(
        &self,
        input: &mut R,
        remote: &Remote,
        success_file: &Path,
        metadata: HeaderMap,
        upload_id: &mut Option<String>,
        completing: &mut bool,
    ) -> Result<Value> {
        let metadata = metadata
            .iter()
            .map(|(name, value)| {
                Ok((
                    name.as_str()["x-amz-meta-".len()..].to_owned(),
                    value.to_str()?.to_owned(),
                ))
            })
            .collect::<Result<std::collections::HashMap<_, _>>>()?;
        let initiated = self
            .sdk
            .create_multipart_upload()
            .bucket(&remote.bucket)
            .key(sdk::key(&remote.key))
            .set_metadata(Some(metadata))
            .content_type("application/octet-stream")
            .customize()
            .interceptor(sdk::LiteralKey::new(&remote.key))
            .send()
            .await
            .map_err(sdk::error)?;
        let id = initiated
            .upload_id()
            .filter(|id| !id.is_empty() && id.len() <= 8192)
            .ok_or_else(|| anyhow!("gateway returned an invalid multipart upload ID"))?;
        *upload_id = Some(id.to_owned());
        if initiated.bucket() != Some(remote.bucket.as_str())
            || initiated.key() != Some(remote.key.as_str())
        {
            bail!("gateway initiated a different object");
        }
        let mut parts = Vec::new();
        let mut size = 0u64;
        loop {
            let mut bytes = vec![0; PART_SIZE];
            let mut filled = 0;
            while filled < PART_SIZE {
                let count = input
                    .read(&mut bytes[filled..])
                    .await
                    .context("failed to read upload input; not completing")?;
                if count == 0 {
                    break;
                }
                filled += count;
            }
            if filled == 0 && !parts.is_empty() {
                break;
            }
            if parts.len() == MAX_PARTS {
                bail!("stream exceeds the 10000-part limit (8 MiB per part)");
            }
            bytes.truncate(filled);
            let number = parts.len() + 1;
            let response = self
                .sdk
                .upload_part()
                .bucket(&remote.bucket)
                .key(sdk::key(&remote.key))
                .upload_id(id)
                .part_number(number as i32)
                .body(aws_sdk_s3::primitives::ByteStream::from(bytes))
                .customize()
                .interceptor(sdk::LiteralKey::new(&remote.key))
                .send()
                .await
                .map_err(sdk::error)?;
            let etag = response
                .e_tag()
                .ok_or_else(|| anyhow!("uploaded part response has no ETag"))?;
            validate_etag(etag)?;
            parts.push(
                CompletedPart::builder()
                    .part_number(number as i32)
                    .e_tag(etag)
                    .build(),
            );
            size += filled as u64;
            if filled < PART_SIZE {
                break;
            }
        }
        let marker_metadata = tokio::fs::metadata(success_file)
            .await
            .context("producer success file missing; aborting without completion")?;
        if !marker_metadata.is_file() {
            bail!("producer success path is not a regular file");
        }
        let mut marker = Vec::new();
        tokio::fs::File::open(success_file)
            .await
            .context("producer success file missing; aborting without completion")?
            .take(8)
            .read_to_end(&mut marker)
            .await
            .context("cannot read producer success file")?;
        if marker != b"commit\n" {
            bail!("invalid producer success file; aborting without completion");
        }
        *completing = true;
        let completed = self
            .sdk
            .complete_multipart_upload()
            .bucket(&remote.bucket)
            .key(sdk::key(&remote.key))
            .upload_id(id)
            .multipart_upload(
                CompletedMultipartUpload::builder()
                    .set_parts(Some(parts))
                    .build(),
            )
            .customize()
            .interceptor(sdk::LiteralKey::new(&remote.key))
            .send()
            .await
            .map_err(sdk::error)?;
        if completed.bucket() != Some(remote.bucket.as_str())
            || completed.key() != Some(remote.key.as_str())
        {
            bail!("gateway completed a different object");
        }
        validate_etag(
            completed
                .e_tag()
                .ok_or_else(|| anyhow!("completion response has no ETag"))?,
        )?;
        Ok(
            json!({"operation": "upload", "destination": format!("s3://{}/{}", remote.bucket, remote.key), "size": size}),
        )
    }

    pub(super) async fn download_stream<W: AsyncWrite + Unpin>(
        &self,
        remote: &Remote,
        output: &mut W,
    ) -> Result<u64> {
        let mut response = self.provider_response(remote, true).await?;
        let mut size = 0u64;
        while let Some(chunk) = response.chunk().await.map_err(|error| {
            #[cfg(test)]
            eprintln!(
                "provider body failed after {size} bytes: timeout={}, body={}, error={:?}",
                error.is_timeout(),
                error.is_body(),
                error.without_url()
            );
            #[cfg(not(test))]
            let _ = error;
            anyhow!("provider download interrupted; output may be partial")
        })? {
            output
                .write_all(&chunk)
                .await
                .context("failed to write download output; output may be partial")?;
            size = size
                .checked_add(chunk.len() as u64)
                .ok_or_else(|| anyhow!("download size overflow"))?;
        }
        output
            .flush()
            .await
            .context("failed to flush download output")?;
        Ok(size)
    }
}

pub(super) fn cancellable_stdin() -> impl AsyncRead + Unpin {
    let (sender, receiver) = tokio::sync::mpsc::channel(1);
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        let mut stdin = stdin.lock();
        loop {
            let mut bytes = vec![0; 64 * 1024];
            let result = match std::io::Read::read(&mut stdin, &mut bytes) {
                Ok(0) => break,
                Ok(count) => {
                    bytes.truncate(count);
                    Ok(tokio_util::bytes::Bytes::from(bytes))
                }
                Err(error) => Err(error),
            };
            let failed = result.is_err();
            if sender.blocking_send(result).is_err() || failed {
                break;
            }
        }
    });
    tokio_util::io::StreamReader::new(tokio_stream::wrappers::ReceiverStream::new(receiver))
}

pub(super) struct CancellableOutput {
    sender: std::sync::mpsc::Sender<(Vec<u8>, tokio::sync::oneshot::Sender<std::io::Result<()>>)>,
    pending: Option<(usize, tokio::sync::oneshot::Receiver<std::io::Result<()>>)>,
}

impl tokio::io::AsyncWrite for CancellableOutput {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
        bytes: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        if self.pending.is_none() {
            let size = bytes.len().min(64 * 1024);
            let (sender, receiver) = tokio::sync::oneshot::channel();
            if self.sender.send((bytes[..size].to_vec(), sender)).is_err() {
                return std::task::Poll::Ready(Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "stdout worker stopped",
                )));
            }
            self.pending = Some((size, receiver));
        }
        let (size, receiver) = self
            .pending
            .as_mut()
            .ok_or_else(|| std::io::Error::other("no pending stdout write"))?;
        let size = *size;
        match std::pin::Pin::new(receiver).poll(context) {
            std::task::Poll::Pending => std::task::Poll::Pending,
            std::task::Poll::Ready(result) => {
                self.pending = None;
                std::task::Poll::Ready(
                    result
                        .unwrap_or_else(|_| Err(std::io::Error::other("stdout worker stopped")))
                        .map(|()| size),
                )
            }
        }
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        self.poll_flush(context)
    }
}

pub(super) fn cancellable_output<W: std::io::Write + Send + 'static>(
    mut output: W,
) -> CancellableOutput {
    let (sender, receiver) =
        std::sync::mpsc::channel::<(Vec<u8>, tokio::sync::oneshot::Sender<std::io::Result<()>>)>();
    std::thread::spawn(move || {
        while let Ok((bytes, acknowledge)) = receiver.recv() {
            let result = output.write_all(&bytes).and_then(|()| output.flush());
            let failed = result.is_err();
            if acknowledge.send(result).is_err() || failed {
                break;
            }
        }
    });
    CancellableOutput {
        sender,
        pending: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_generic_metadata_names_values_and_bounds() {
        let headers = metadata_headers(&["project=example".into(), "Source=cli".into()]).unwrap();
        assert_eq!(headers.len(), 2);
        assert_eq!(headers["x-amz-meta-project"], "example");
        assert_eq!(headers["x-amz-meta-source"], "cli");
        assert!(metadata_headers(&[format!("description={}", "x".repeat(1024))]).is_ok());
        for metadata in [
            vec!["missing-equals".into()],
            vec!["=value".into()],
            vec!["bad key=value".into()],
            vec!["bad:key=value".into()],
            vec!["name=".into()],
            vec!["name=a\r\nbad".into()],
            vec!["name=雪".into()],
            vec![format!("description={}", "x".repeat(2048))],
            vec!["name=a".into(), "NAME=b".into()],
            (0..5)
                .map(|n| format!("field{n}={}", "x".repeat(512)))
                .collect(),
        ] {
            assert!(metadata_headers(&metadata).is_err());
        }
    }

    #[test]
    fn completion_manifest_supports_10000_ordered_parts() {
        let completion = CompletedMultipartUpload::builder()
            .set_parts(Some(
                (1..=MAX_PARTS)
                    .map(|number| {
                        CompletedPart::builder()
                            .part_number(number as i32)
                            .e_tag("\"opaque&etag\"")
                            .build()
                    })
                    .collect(),
            ))
            .build();
        assert_eq!(completion.parts().len(), MAX_PARTS);
        assert_eq!(completion.parts()[MAX_PARTS - 1].part_number(), Some(10000));
    }

    #[tokio::test]
    async fn output_worker_preserves_raw_bytes_and_reports_write_errors() {
        struct Writer(Arc<std::sync::Mutex<Vec<u8>>>);
        use std::sync::Arc;
        impl std::io::Write for Writer {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let bytes = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut output = cancellable_output(Writer(bytes.clone()));
        output.write_all(&vec![0xff; 128 * 1024]).await.unwrap();
        output.flush().await.unwrap();
        assert_eq!(*bytes.lock().unwrap(), vec![0xff; 128 * 1024]);
        struct Broken;
        impl std::io::Write for Broken {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let error = cancellable_output(Broken)
            .write_all(b"raw bytes")
            .await
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::BrokenPipe);
    }
}
