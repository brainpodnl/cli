use std::path::Path;

use anyhow::{Context, Result, anyhow};
use reqwest::header::{CONTENT_LENGTH, CONTENT_TYPE, LOCATION};
use reqwest::{Client, Response, StatusCode, Url};
use tokio_util::io::ReaderStream;

use super::{Descriptor, ImageLayout, NETWORK_TIMEOUT, blob_path};

pub struct Registry<'a> {
    api_token: &'a str,
    authority: String,
    base: Url,
    http: Client,
}

impl<'a> Registry<'a> {
    pub fn new(endpoint: &str, api_token: &'a str) -> Result<Self> {
        let mut base = Url::parse(endpoint).context("invalid Brainpod registry endpoint")?;
        if !matches!(base.scheme(), "http" | "https") {
            return Err(anyhow!("Brainpod registry endpoint must use http or https"));
        }
        if !base.username().is_empty()
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
            || base.path() != "/"
        {
            return Err(anyhow!(
                "Brainpod registry endpoint must not contain credentials, a path, query, or fragment"
            ));
        }
        base.set_path("/");
        let host = base
            .host_str()
            .ok_or_else(|| anyhow!("Brainpod registry endpoint has no host"))?;
        let authority = match base.port() {
            Some(port) => format!("{host}:{port}"),
            None => host.to_owned(),
        };

        Ok(Self {
            api_token,
            authority,
            base,
            http: Client::builder()
                .timeout(NETWORK_TIMEOUT)
                .build()
                .context("failed to create registry client")?,
        })
    }

    pub fn authority(&self) -> &str {
        &self.authority
    }

    pub async fn authenticate(&self) -> Result<()> {
        let response = self
            .http
            .get(self.route("v2/")?)
            .basic_auth("api", Some(self.api_token))
            .send()
            .await
            .context("failed to connect to Brainpod registry")?;
        expect_status(response, &[StatusCode::OK])
            .await
            .context("Brainpod registry authentication failed")?;
        Ok(())
    }

    /// Pushes the image, naming each blob as it starts.
    ///
    /// The naming is not decoration. Uploading a large image is the longest
    /// stretch of a deploy in which nothing else is written anywhere, and a
    /// console with nothing arriving is one the user reads as hung.
    pub async fn push(&self, repository: &str, tag: &str, image: &ImageLayout) -> Result<()> {
        let mut progress = crate::agent::sink("push");
        let total = image.manifest.layers.len() + 1;
        let mut announce = |position: usize, what: &str| {
            if let Some(progress) = progress.as_mut() {
                progress.write(&format!("{position}/{total} {what}"));
            }
        };

        for (index, layer) in image.manifest.layers.iter().enumerate() {
            announce(index + 1, &format!("layer {}", short(&layer.digest)));
            self.push_blob(repository, &image.root, layer).await?;
        }
        announce(total, "image configuration");
        self.push_blob(repository, &image.root, &image.manifest.config)
            .await?;
        announce(total, "manifest");

        let url = self.route(&format!("v2/{repository}/manifests/{tag}"))?;
        let response = self
            .http
            .put(url)
            .basic_auth("api", Some(self.api_token))
            .header(CONTENT_TYPE, &image.descriptor.media_type)
            .body(image.manifest_bytes.clone())
            .send()
            .await
            .context("failed to push image manifest")?;
        let response = expect_status(response, &[StatusCode::CREATED, StatusCode::ACCEPTED])
            .await
            .context("registry rejected image manifest")?;
        if let Some(digest) = response.headers().get("docker-content-digest") {
            let digest = digest
                .to_str()
                .context("registry returned an invalid image digest")?;
            if digest != image.descriptor.digest.as_str() {
                return Err(anyhow!(
                    "registry stored image digest {digest}, expected {}",
                    image.descriptor.digest
                ));
            }
        }
        Ok(())
    }

    async fn push_blob(
        &self,
        repository: &str,
        root: &Path,
        descriptor: &Descriptor,
    ) -> Result<()> {
        let mut start_url = self.route(&format!("v2/{repository}/blobs/uploads/"))?;
        start_url
            .query_pairs_mut()
            .append_pair("mount", &descriptor.digest);
        let response = self
            .http
            .post(start_url)
            .basic_auth("api", Some(self.api_token))
            .send()
            .await
            .with_context(|| format!("failed to start upload for blob {}", descriptor.digest))?;
        if response.status() == StatusCode::CREATED {
            return Ok(());
        }
        eprintln!("Pushing blob {}", descriptor.digest);
        let response = expect_status(response, &[StatusCode::ACCEPTED])
            .await
            .with_context(|| format!("registry rejected blob upload {}", descriptor.digest))?;
        let location = response
            .headers()
            .get(LOCATION)
            .ok_or_else(|| anyhow!("registry blob upload response has no location"))?
            .to_str()
            .context("registry returned an invalid blob upload location")?;
        let mut upload_url = self
            .base
            .join(location)
            .context("registry returned an invalid blob upload location")?;
        self.validate_upload_url(&upload_url)?;
        upload_url
            .query_pairs_mut()
            .append_pair("digest", &descriptor.digest);

        let path = blob_path(root, &descriptor.digest)?;
        let file = tokio::fs::File::open(&path)
            .await
            .with_context(|| format!("failed to open OCI blob {}", path.display()))?;
        let size = file
            .metadata()
            .await
            .with_context(|| format!("failed to inspect OCI blob {}", path.display()))?
            .len();
        if size != descriptor.size {
            return Err(anyhow!(
                "OCI blob {} has size {size}, expected {}",
                descriptor.digest,
                descriptor.size
            ));
        }

        let response = self
            .http
            .put(upload_url)
            .basic_auth("api", Some(self.api_token))
            .header(CONTENT_TYPE, "application/octet-stream")
            .header(CONTENT_LENGTH, size)
            .body(reqwest::Body::wrap_stream(ReaderStream::new(file)))
            .send()
            .await
            .with_context(|| format!("failed to upload blob {}", descriptor.digest))?;
        expect_status(response, &[StatusCode::CREATED])
            .await
            .with_context(|| format!("registry rejected blob {}", descriptor.digest))?;
        Ok(())
    }

    fn route(&self, path: &str) -> Result<Url> {
        self.base
            .join(path)
            .with_context(|| format!("failed to construct registry URL for {path}"))
    }

    fn validate_upload_url(&self, url: &Url) -> Result<()> {
        let same_origin = url.scheme() == self.base.scheme()
            && url.host_str() == self.base.host_str()
            && url.port_or_known_default() == self.base.port_or_known_default();
        if same_origin {
            Ok(())
        } else {
            Err(anyhow!(
                "registry returned a blob upload location on another origin"
            ))
        }
    }
}

async fn expect_status(response: Response, expected: &[StatusCode]) -> Result<Response> {
    if expected.contains(&response.status()) {
        return Ok(response);
    }

    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if body.trim().is_empty() {
        Err(anyhow!("registry returned {status}"))
    } else {
        Err(anyhow!("registry returned {status}: {}", body.trim()))
    }
}

#[cfg(test)]
mod tests {
    use reqwest::Url;

    use super::Registry;

    #[test]
    fn validates_registry_endpoints() {
        assert!(Registry::new("https://registry.brainpod.io", "key").is_ok());
        assert!(Registry::new("http://localhost:5000", "key").is_ok());
        assert!(Registry::new("https://example.com/registry", "key").is_err());
        assert!(Registry::new("https://user:pass@example.com", "key").is_err());
        assert!(Registry::new("file:///tmp/registry", "key").is_err());
    }

    #[test]
    fn rejects_cross_origin_upload_locations() {
        let registry = Registry::new("https://registry.brainpod.io", "key").unwrap();
        let other = Url::parse("https://uploads.example.com/v2/blob").unwrap();

        assert!(registry.validate_upload_url(&other).is_err());
    }
}

/// A digest short enough to read, long enough to tell two layers apart.
fn short(digest: &str) -> &str {
    let digest = digest.strip_prefix("sha256:").unwrap_or(digest);
    &digest[..digest.len().min(12)]
}
