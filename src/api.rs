use std::sync::Arc;

use anyhow::{Result, anyhow};
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue, InvalidHeaderValue};

use brainpod_core::pod::PodMeta;
use brainpod_core::resource::{Resource, ResourceKind};

#[derive(Debug, Clone)]
pub struct ApiKey(Box<str>);

impl std::str::FromStr for ApiKey {
    type Err = anyhow::Error;

    fn from_str(key: &str) -> Result<Self, Self::Err> {
        if let Some(key_id) = key.strip_prefix("brain_")
            && key_id.len() == 32
        {
            Ok(Self(Box::from(key)))
        } else {
            Err(anyhow!("malformed api key"))
        }
    }
}

impl ApiKey {
    const fn as_str(&self) -> &str {
        &self.0
    }
}

fn bearer(api_key: &ApiKey) -> Result<HeaderValue, InvalidHeaderValue> {
    HeaderValue::from_str(&format!("Bearer {}", api_key.as_str()))
}

fn default_headers(api_key: &ApiKey) -> Result<HeaderMap> {
    let mut map = HeaderMap::new();
    map.insert(AUTHORIZATION, bearer(api_key)?);
    Ok(map)
}

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    endpoint: Arc<str>,
}

impl Client {
    pub fn try_new(endpoint: &str, api_key: &ApiKey) -> Result<Self> {
        let endpoint = Arc::from(endpoint);
        let headers = default_headers(api_key)?;
        let http = reqwest::ClientBuilder::new()
            .default_headers(headers)
            .build()?;

        Ok(Self { http, endpoint })
    }

    pub async fn list_pods(&self) -> Result<Vec<PodMeta>> {
        let endpoint = format!("{}/v1/pods", self.endpoint);
        Ok(self
            .http
            .get(endpoint)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
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
