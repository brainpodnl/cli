use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use reqwest::Url;
use serde_json::{Value, json};

const PRODUCTION_OPENAPI_URL: &str = "https://api.prod.brainpod.io/v1/openapi.json";
const EMBEDDED_OPENAPI: &str = include_str!("openapi.json");

pub fn is_resource_path(path: &[String]) -> bool {
    match path {
        [resource] => resource == "resource",
        [resource, kind] => {
            resource == "resource"
                && !matches!(kind.as_str(), "list" | "get" | "create" | "replace" | "delete")
        }
        _ => false,
    }
}

pub async fn describe(path: &[String], endpoint: Option<&str>) -> Result<Value> {
    let requested_kind = path.get(1).map(String::as_str);
    let url = openapi_url(endpoint)?;
    let (spec, source) = load_spec(&url).await?;
    let resources = resource_schemas(&spec)?;

    if let Some(requested_kind) = requested_kind {
        let requested_kind = requested_kind.to_ascii_lowercase();
        let Some((kind, schema)) = resources
            .iter()
            .find(|(kind, _)| kind.to_ascii_lowercase() == requested_kind)
        else {
            let available = resources
                .iter()
                .map(|(kind, _)| kind.to_ascii_lowercase())
                .collect::<Vec<_>>();
            return Err(anyhow!(
                "unknown resource kind `{requested_kind}`; available resource kinds: {}",
                available.join(", ")
            ));
        };

        return Ok(json!({
            "schemaVersion": 1,
            "resource": kind,
            "source": source,
            "sourceUrl": url,
            "schema": schema,
        }));
    }

    Ok(json!({
        "schemaVersion": 1,
        "source": source,
        "sourceUrl": url,
        "resources": resources.into_iter().map(|(kind, schema)| json!({
            "kind": kind,
            "schema": schema,
        })).collect::<Vec<_>>(),
    }))
}

async fn load_spec(url: &str) -> Result<(Value, &'static str)> {
    if let Ok(spec) = fetch_spec(url).await
        && resource_schemas(&spec).is_ok() {
            return Ok((spec, "remote"));
        }

    let embedded = serde_json::from_str(EMBEDDED_OPENAPI)
        .context("embedded Brainpod OpenAPI specification is invalid JSON")?;
    Ok((embedded, "embedded"))
}

async fn fetch_spec(url: &str) -> Result<Value> {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .context("failed to create OpenAPI client")?;
    let response = http
        .get(url)
        .send()
        .await
        .context("failed to fetch Brainpod OpenAPI specification")?
        .error_for_status()
        .context("Brainpod OpenAPI specification returned an error")?;
    response
        .json()
        .await
        .context("Brainpod OpenAPI specification is not valid JSON")
}

fn openapi_url(endpoint: Option<&str>) -> Result<String> {
    if let Some(url) = std::env::var_os("BRAINPOD_OPENAPI_URL") {
        let url = url
            .into_string()
            .map_err(|_| anyhow!("BRAINPOD_OPENAPI_URL is not valid UTF-8"))?;
        if url.trim().is_empty() {
            return Err(anyhow!("BRAINPOD_OPENAPI_URL cannot be empty"));
        }
        return Ok(url);
    }

    let Some(endpoint) = endpoint else {
        return Ok(PRODUCTION_OPENAPI_URL.to_owned());
    };

    let mut url = Url::parse(endpoint).context("invalid Brainpod API endpoint")?;
    {
        let mut segments = url
            .path_segments_mut()
            .map_err(|_| anyhow!("Brainpod API endpoint cannot be a base URL"))?;
        segments.pop_if_empty();
        segments.extend(["v1", "openapi.json"]);
    }
    Ok(url.to_string())
}

fn resource_schemas(spec: &Value) -> Result<Vec<(String, Value)>> {
    let branches = spec
        .pointer("/components/schemas/ResourceInput/oneOf")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("OpenAPI specification has no ResourceInput schemas"))?;

    let resources = branches
        .iter()
        .filter_map(|schema| {
            let kind = schema
                .pointer("/properties/kind/const")
                .and_then(Value::as_str)?;
            Some((kind.to_owned(), schema.clone()))
        })
        .collect::<Vec<_>>();

    if resources.is_empty() {
        Err(anyhow!(
            "OpenAPI specification has no discoverable resource schemas"
        ))
    } else {
        Ok(resources)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{is_resource_path, resource_schemas};

    #[test]
    fn recognizes_resource_schema_paths_without_shadowing_commands() {
        assert!(is_resource_path(&["resource".to_owned()]));
        assert!(is_resource_path(&["resource".to_owned(), "app".to_owned()]));
        assert!(!is_resource_path(&[
            "resource".to_owned(),
            "create".to_owned()
        ]));
    }

    #[test]
    fn extracts_resource_schemas_from_openapi() {
        let spec = json!({
            "components": {
                "schemas": {
                    "ResourceInput": {
                        "oneOf": [
                            {"properties": {"kind": {"const": "App"}}},
                            {"properties": {"kind": {"const": "Disk"}}}
                        ]
                    }
                }
            }
        });

        let resources = resource_schemas(&spec).unwrap();

        assert_eq!(
            resources.iter().map(|(kind, _)| kind.as_str()).collect::<Vec<_>>(),
            vec!["App", "Disk"]
        );
    }
}
