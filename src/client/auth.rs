use anyhow::{Result, anyhow};
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue, InvalidHeaderValue};

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
    pub const fn as_str(&self) -> &str {
        &self.0
    }
}

fn bearer(api_key: &ApiKey) -> Result<HeaderValue, InvalidHeaderValue> {
    HeaderValue::from_str(&format!("Bearer {}", api_key.as_str()))
}

pub fn default_headers(api_key: &ApiKey) -> Result<HeaderMap> {
    let mut map = HeaderMap::new();
    map.insert(AUTHORIZATION, bearer(api_key)?);
    Ok(map)
}
