use std::sync::Arc;

use anyhow::Result;

use brainpod_core::pod::PodMeta;
use brainpod_core::resource::{Resource, ResourceKind};

pub mod auth;

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    endpoint: Arc<str>,
}

impl Client {
    pub fn try_new(endpoint: &str, api_key: &auth::ApiKey) -> Result<Self> {
        let endpoint = Arc::from(endpoint);
        let headers = auth::default_headers(api_key)?;
        let http = reqwest::ClientBuilder::new()
            .default_headers(headers)
            .build()?;

        Ok(Self { http, endpoint })
    }

    pub fn pods(&self) -> PodsClient<'_> {
        PodsClient { client: self }
    }

    pub async fn list_resources(&self, kind: &ResourceKind) -> Result<Vec<Resource>> {
        let endpoint = format!(
            "{}/v1/resources/{}",
            self.endpoint,
            kind.to_string().to_lowercase()
        );
        Ok(self
            .http
            .get(endpoint)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }
}

pub struct PodsClient<'c> {
    client: &'c Client,
}

impl PodsClient<'_> {
    pub async fn list(&self) -> Result<Vec<PodMeta>> {
        Ok(self
            .client
            .http
            .get(format!("{}/v1/pods", self.client.endpoint))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }

    pub fn by_name<'p>(&self, name: &'p str) -> PodClient<'p> {
        PodClient {
            http: self.client.http.clone(),
            base_url: Arc::from(format!("{}/v1/pods/{name}", self.client.endpoint)),
            pod_name: name,
        }
    }
}

pub struct PodClient<'p> {
    http: reqwest::Client,
    base_url: Arc<str>,
    pod_name: &'p str,
}

impl PodClient<'_> {
    pub async fn describe(&self) -> Result<PodMeta> {
        Ok(self
            .http
            .get(format!("{}/describe", self.base_url))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }
}

pub struct ResourcesClient<'p> {
    pod_client: PodClient<'p>,
}

impl ResourcesClient<'_> {
    pub async fn list(&self) -> Result<Vec<serde_json::Value>> {
        Ok(self
            .pod_client
            .http
            .get(format!("{}/resources", self.pod_client.base_url))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }
}
