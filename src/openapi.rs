use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use reqwest::Url;
use serde_json::{Value, json};

const PRODUCTION_OPENAPI_URL: &str = "https://api.brainpod.io/v1/openapi.json";
const EMBEDDED_OPENAPI: &str = include_str!("openapi.json");

const RESOURCE_SUBCOMMANDS: &[&str] = &["list", "get", "create", "replace", "delete", "variables"];

pub fn is_resource_path(path: &[String]) -> bool {
    match path {
        [resource] => resource == "resource",
        [resource, kind] => {
            resource == "resource" && !RESOURCE_SUBCOMMANDS.contains(&kind.as_str())
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
        let Some(resource) = resources
            .iter()
            .find(|resource| resource.kind.to_ascii_lowercase() == requested_kind)
        else {
            let available = resources
                .iter()
                .map(|resource| resource.kind.to_ascii_lowercase())
                .collect::<Vec<_>>();
            return Err(anyhow!(
                "unknown resource kind `{requested_kind}`; available resource kinds: {}",
                available.join(", ")
            ));
        };

        return Ok(json!({
            "schemaVersion": 1,
            "resource": resource.kind,
            "source": source,
            "sourceUrl": url,
            "schema": resource.schema,
            "variables": resource.variables,
        }));
    }

    Ok(json!({
        "schemaVersion": 1,
        "source": source,
        "sourceUrl": url,
        "resources": resources.into_iter().map(|resource| json!({
            "kind": resource.kind,
            "schema": resource.schema,
            "variables": resource.variables,
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

const VARIABLES_EXTENSION: &str = "x-brainpod-variables";

struct ResourceCatalog {
    kind: String,
    schema: Value,
    variables: Value,
}

fn resource_schemas(spec: &Value) -> Result<Vec<ResourceCatalog>> {
    let branches = spec
        .pointer("/components/schemas/ResourceInput/oneOf")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("OpenAPI specification has no ResourceInput schemas"))?;

    let resources = branches
        .iter()
        .filter_map(|branch| {
            let kind = branch
                .pointer("/properties/kind/const")
                .and_then(Value::as_str)?
                .to_owned();
            let mut schema = branch.clone();
            let variables = schema
                .as_object_mut()
                .and_then(|schema| schema.remove(VARIABLES_EXTENSION))
                .filter(Value::is_array)
                .unwrap_or_else(|| json!([]));
            Some(ResourceCatalog {
                kind,
                schema,
                variables,
            })
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
    use serde_json::{Value, json};

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
    fn treats_variables_as_a_subcommand_not_a_resource_kind() {
        assert!(!is_resource_path(&[
            "resource".to_owned(),
            "variables".to_owned()
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
            resources
                .iter()
                .map(|resource| resource.kind.as_str())
                .collect::<Vec<_>>(),
            vec!["App", "Disk"]
        );
    }

    #[test]
    fn extracts_the_variable_catalog_out_of_the_schema() {
        let spec = json!({
            "components": {
                "schemas": {
                    "ResourceInput": {
                        "oneOf": [{
                            "properties": {"kind": {"const": "Postgres"}},
                            "x-brainpod-variables": [{
                                "name": "uri",
                                "ref": "${<name>.uri}",
                                "secret": true,
                                "template": "postgres://${<name>.user}@<name>:5432/brainpod",
                                "description": "Full connection string."
                            }]
                        }]
                    }
                }
            }
        });

        let resources = resource_schemas(&spec).unwrap();
        let [postgres] = resources.as_slice() else {
            panic!("expected one resource");
        };

        assert_eq!(
            postgres.variables.pointer("/0/ref").and_then(Value::as_str),
            Some("${<name>.uri}")
        );
        assert_eq!(
            postgres
                .variables
                .pointer("/0/secret")
                .and_then(Value::as_bool),
            Some(true)
        );
        assert!(postgres.schema.get("x-brainpod-variables").is_none());
    }

    #[test]
    fn reports_an_empty_catalog_for_kinds_without_variables() {
        let spec = json!({
            "components": {
                "schemas": {
                    "ResourceInput": {
                        "oneOf": [{"properties": {"kind": {"const": "Disk"}}}]
                    }
                }
            }
        });

        let resources = resource_schemas(&spec).unwrap();

        assert_eq!(resources[0].variables, json!([]));
    }
}
