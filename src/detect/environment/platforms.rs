//! Environment declarations in deployment manifests of other hosting platforms.
//!
//! Each adapter reads only the documented, statically declared variable fields.
//! Literal values are inspected solely to classify them (scalar, secret/binding
//! reference) and are never copied into declarations or diagnostics.

use serde_json::{Map, Value};

use super::{
    Format, Parser, Reference, Scope, Semantics, metadata_name, pointer, reference,
    safe_reference_path,
};

pub(super) fn parse(parser: &mut Parser<'_>, doc: &Value, location: &str) {
    match parser.format {
        Format::AppJson => app_json(parser, doc, location),
        Format::HerokuYml => heroku_yml(parser, doc, location),
        Format::Render => render(parser, doc, location),
        Format::Fly => literal_map(
            parser,
            doc.get("env"),
            &format!("{location}/env"),
            scoped(Scope::Runtime),
        ),
        Format::Digitalocean => digitalocean(parser, doc, location),
        Format::Vercel => vercel(parser, doc, location),
        Format::Netlify => netlify(parser, doc, location),
        Format::AppEngine => app_engine(parser, doc, location),
        Format::Copilot => copilot(parser, doc, location),
        _ => (),
    }
}

fn scalar(value: &Value) -> bool {
    value.is_string() || value.is_number() || value.is_boolean()
}

fn child(location: &str, key: &str) -> String {
    format!("{location}/{}", pointer(key))
}

fn scoped(scope: Scope) -> Semantics {
    Semantics {
        scope: Some(scope),
        ..Semantics::default()
    }
}

fn string<'a>(value: &'a Value, field: &str) -> Option<&'a str> {
    value.get(field).and_then(Value::as_str)
}

fn list<'a>(value: &'a Value, field: &str) -> impl Iterator<Item = (usize, &'a Value)> {
    value
        .get(field)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
}

fn object<'a>(
    parser: &mut Parser<'_>,
    value: &'a Value,
    message: &str,
) -> Option<&'a Map<String, Value>> {
    let map = value.as_object();
    if map.is_none() {
        parser.warn(message);
    }
    map
}

/// Named sub-tables (contexts, environments, sidecars) whose keys become location segments.
fn named<'a>(parser: &mut Parser<'_>, value: Option<&'a Value>) -> Vec<(&'a String, &'a Value)> {
    let mut entries = value
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    if entries.iter().any(|(name, _)| !metadata_name(name)) {
        parser.warn("unsupported platform environment or context name");
        entries.retain(|(name, _)| metadata_name(name));
    }
    entries
}

/// `NAME = literal` maps: Fly `[env]`, Netlify/App Engine/heroku.yml/Vercel maps.
fn literal_map(parser: &mut Parser<'_>, value: Option<&Value>, location: &str, base: Semantics) {
    let Some(value) = value else {
        return;
    };
    let Some(map) = object(parser, value, "environment declaration must be a mapping") else {
        return;
    };
    for (name, value) in map {
        if !(value.is_null() || scalar(value)) {
            parser.warn("environment value must be a scalar; declaration skipped");
            continue;
        }
        parser.declare(
            &child(location, name),
            Some(name),
            "declared",
            None,
            Semantics {
                has_default: Some(!value.is_null()),
                ..base
            },
        );
    }
}

/// Heroku `app.json` (also read by Dokku) and Scalingo `scalingo.json`.
fn app_json(parser: &mut Parser<'_>, doc: &Value, location: &str) {
    app_json_env(parser, doc.get("env"), &format!("{location}/env"));
    for (name, environment) in named(parser, doc.get("environments")) {
        app_json_env(
            parser,
            environment.get("env"),
            &format!("{location}/environments/{}/env", pointer(name)),
        );
    }
}

fn app_json_env(parser: &mut Parser<'_>, value: Option<&Value>, location: &str) {
    let Some(value) = value else {
        return;
    };
    let Some(map) = object(parser, value, "app.json env must be an object") else {
        return;
    };
    for (name, entry) in map {
        let semantics = if scalar(entry) {
            Semantics {
                has_default: Some(true),
                ..Semantics::default()
            }
        } else if entry.is_object() {
            let generator = string(entry, "generator");
            Semantics {
                required: entry.get("required").and_then(Value::as_bool),
                secret: (generator == Some("secret")).then_some(true),
                generated: generator.map(|_| true),
                has_default: generator
                    .is_none()
                    .then(|| entry.get("value").is_some_and(scalar)),
                scope: None,
            }
        } else {
            parser.warn("app.json env entry must be a string or object; declaration skipped");
            continue;
        };
        parser.declare(
            &child(location, name),
            Some(name),
            "declared",
            None,
            semantics,
        );
    }
}

/// Heroku `heroku.yml`: provisioning config vars and build-time variables.
fn heroku_yml(parser: &mut Parser<'_>, doc: &Value, location: &str) {
    literal_map(
        parser,
        doc.pointer("/setup/config"),
        &format!("{location}/setup/config"),
        scoped(Scope::Runtime),
    );
    literal_map(
        parser,
        doc.pointer("/build/config"),
        &format!("{location}/build/config"),
        scoped(Scope::Build),
    );
}

/// Render Blueprint `render.yaml`.
fn render(parser: &mut Parser<'_>, doc: &Value, location: &str) {
    render_resources(parser, doc, location);
    if let Some(ungrouped) = doc.get("ungrouped") {
        render_resources(parser, ungrouped, &format!("{location}/ungrouped"));
    }
    for (project_index, project) in list(doc, "projects") {
        for (index, environment) in list(project, "environments") {
            render_resources(
                parser,
                environment,
                &format!("{location}/projects/{project_index}/environments/{index}"),
            );
        }
    }
}

fn render_resources(parser: &mut Parser<'_>, value: &Value, location: &str) {
    for (field, group) in [("services", false), ("envVarGroups", true)] {
        for (index, resource) in list(value, field) {
            if let Some(vars) = resource.get("envVars") {
                render_env_vars(
                    parser,
                    vars,
                    &format!("{location}/{field}/{index}/envVars"),
                    group,
                );
            }
        }
    }
}

fn render_env_vars(parser: &mut Parser<'_>, vars: &Value, location: &str, group: bool) {
    let Some(vars) = vars.as_array() else {
        parser.warn("Render envVars must be a list");
        return;
    };
    for (index, var) in vars.iter().enumerate() {
        let location = format!("{location}/{index}");
        if let Some(name) = var.get("fromGroup") {
            parser.emit(
                &location,
                None,
                "unresolved",
                Some(reference("env-group", name.as_str())),
            );
            continue;
        }
        let Some(name) = string(var, "key") else {
            parser.warn("Render envVars entry requires a key");
            continue;
        };
        if let Some(database) = var.get("fromDatabase") {
            let reference = keyed(
                "database",
                string(database, "name"),
                string(database, "property"),
            );
            parser.emit(&location, Some(name), "unresolved", Some(reference));
        } else if let Some(service) = var.get("fromService") {
            let target = string(service, "name");
            let reference = match string(service, "envVarKey") {
                Some(key) => keyed("service-env", target, Some(key)),
                None => keyed(
                    if matches!(string(service, "type"), Some("keyvalue" | "redis")) {
                        "key-value"
                    } else {
                        "service"
                    },
                    target,
                    string(service, "property"),
                ),
            };
            parser.emit(&location, Some(name), "unresolved", Some(reference));
        } else if var.get("generateValue").and_then(Value::as_bool) == Some(true) {
            let semantics = Semantics {
                secret: Some(true),
                generated: Some(true),
                ..Semantics::default()
            };
            parser.declare(&location, Some(name), "declared", None, semantics);
        } else if var.get("sync").and_then(Value::as_bool) == Some(false) {
            if group {
                parser
                    .warn("Render ignores sync: false in environment groups; declaration skipped");
                continue;
            }
            let semantics = Semantics {
                secret: Some(true),
                has_default: Some(false),
                ..Semantics::default()
            };
            parser.declare(&location, Some(name), "declared", None, semantics);
        } else if var.get("value").is_some_and(scalar) {
            let semantics = Semantics {
                has_default: Some(true),
                ..Semantics::default()
            };
            parser.declare(&location, Some(name), "declared", None, semantics);
        } else {
            parser.warn("unsupported Render envVars entry; declaration skipped");
        }
    }
}

fn keyed(kind: &str, target: Option<&str>, key: Option<&str>) -> Reference {
    let mut result = reference(kind, target);
    result.key = key.filter(|key| metadata_name(key)).map(str::to_owned);
    result
}

/// DigitalOcean App Platform app spec (`.do/app.yaml`) and Deploy button
/// template (`.do/deploy.template.yaml`, spec nested under `spec`).
fn digitalocean(parser: &mut Parser<'_>, doc: &Value, location: &str) {
    let (spec, location) = match doc.get("spec") {
        Some(spec) if spec.is_object() => (spec, format!("{location}/spec")),
        _ => (doc, location.to_owned()),
    };
    let databases = list(spec, "databases")
        .filter_map(|(_, database)| string(database, "name"))
        .collect::<Vec<_>>();
    if let Some(envs) = spec.get("envs") {
        digitalocean_envs(parser, envs, &format!("{location}/envs"), &databases);
    }
    for kind in ["services", "static_sites", "workers", "jobs", "functions"] {
        for (index, component) in list(spec, kind) {
            if let Some(envs) = component.get("envs") {
                digitalocean_envs(
                    parser,
                    envs,
                    &format!("{location}/{kind}/{index}/envs"),
                    &databases,
                );
            }
        }
    }
}

fn digitalocean_envs(parser: &mut Parser<'_>, envs: &Value, location: &str, databases: &[&str]) {
    let Some(envs) = envs.as_array() else {
        parser.warn("DigitalOcean envs must be a list");
        return;
    };
    for (index, env) in envs.iter().enumerate() {
        let location = format!("{location}/{index}");
        let Some(name) = string(env, "key") else {
            parser.warn("DigitalOcean env entry requires a key");
            continue;
        };
        let secret = match string(env, "type") {
            Some("SECRET") => Some(true),
            Some("GENERAL") => Some(false),
            _ => None,
        };
        let mut semantics = Semantics {
            secret,
            scope: match string(env, "scope") {
                Some("RUN_TIME") => Some(Scope::Runtime),
                Some("BUILD_TIME") => Some(Scope::Build),
                Some("RUN_AND_BUILD_TIME") => Some(Scope::BuildAndRuntime),
                _ => None,
            },
            ..Semantics::default()
        };
        // Encrypted SECRET values are opaque; only non-secret values may be bindings.
        let binding =
            string(env, "value").filter(|value| secret != Some(true) && value.contains("${"));
        if let Some(binding) = binding {
            let reference = bindable(binding, databases);
            parser.declare(
                &location,
                Some(name),
                "unresolved",
                Some(reference),
                semantics,
            );
        } else {
            semantics.has_default = Some(env.get("value").is_some_and(scalar));
            parser.declare(&location, Some(name), "declared", None, semantics);
        }
    }
}

/// Whole-value `${APP_URL}`, `${component.VAR}` or `${database.pool.VAR}` bindings.
/// Partial interpolation is reported without echoing any part of the value.
fn bindable(value: &str, databases: &[&str]) -> Reference {
    let inner = value
        .strip_prefix("${")
        .and_then(|value| value.strip_suffix('}'))
        .filter(|inner| !inner.contains(['$', '{', '}']));
    let parts = inner.map(|inner| inner.split('.').collect::<Vec<_>>());
    match parts.as_deref() {
        Some([key]) => keyed("app", None, Some(key)),
        Some([target, key] | [target, _, key]) => {
            let database = databases
                .iter()
                .any(|name| name == target || (target.len() == 32 && name.starts_with(target)));
            keyed(
                if database { "database" } else { "service" },
                Some(target),
                Some(key),
            )
        }
        _ => reference("interpolation", None),
    }
}

/// Legacy `vercel.json`/`vercel.toml` `env` (functions) and `build.env` (builds).
fn vercel(parser: &mut Parser<'_>, doc: &Value, location: &str) {
    for (path, scope) in [("/env", Scope::Runtime), ("/build/env", Scope::Build)] {
        let Some(value) = doc.pointer(path) else {
            continue;
        };
        let Some(map) = object(parser, value, "Vercel env must be an object") else {
            continue;
        };
        for (name, value) in map {
            if !scalar(value) {
                parser.warn("environment value must be a scalar; declaration skipped");
                continue;
            }
            let location = child(&format!("{location}{path}"), name);
            match value.as_str().and_then(|value| value.strip_prefix('@')) {
                Some(secret) => parser.declare(
                    &location,
                    Some(name),
                    "unresolved",
                    Some(reference("platform-secret", Some(secret))),
                    Semantics {
                        secret: Some(true),
                        scope: Some(scope),
                        ..Semantics::default()
                    },
                ),
                None => parser.declare(
                    &location,
                    Some(name),
                    "declared",
                    None,
                    Semantics {
                        has_default: Some(true),
                        scope: Some(scope),
                        ..Semantics::default()
                    },
                ),
            }
        }
    }
}

/// `netlify.toml` build/context environments and Deploy button template variables.
fn netlify(parser: &mut Parser<'_>, doc: &Value, location: &str) {
    literal_map(
        parser,
        doc.pointer("/build/environment"),
        &format!("{location}/build/environment"),
        scoped(Scope::Build),
    );
    for (name, context) in named(parser, doc.get("context")) {
        literal_map(
            parser,
            context.get("environment"),
            &format!("{location}/context/{}/environment", pointer(name)),
            scoped(Scope::Build),
        );
    }
    let Some(template) = doc.pointer("/template/environment") else {
        return;
    };
    let Some(map) = object(
        parser,
        template,
        "Netlify template environment must be a table",
    ) else {
        return;
    };
    let location = format!("{location}/template/environment");
    for name in map.keys() {
        // Template values are UI labels for required user input, not defaults.
        let semantics = Semantics {
            required: Some(true),
            has_default: Some(false),
            ..Semantics::default()
        };
        parser.declare(
            &child(&location, name),
            Some(name),
            "declared",
            None,
            semantics,
        );
    }
}

/// Google App Engine `app.yaml`; requires the mandatory `runtime` element.
fn app_engine(parser: &mut Parser<'_>, doc: &Value, location: &str) {
    if string(doc, "runtime").is_none() {
        return;
    }
    literal_map(
        parser,
        doc.get("env_variables"),
        &format!("{location}/env_variables"),
        scoped(Scope::Runtime),
    );
    literal_map(
        parser,
        doc.get("build_env_variables"),
        &format!("{location}/build_env_variables"),
        scoped(Scope::Build),
    );
}

/// AWS Copilot workload manifests (`copilot/<name>/manifest.yml`).
fn copilot(parser: &mut Parser<'_>, doc: &Value, location: &str) {
    if string(doc, "type").is_none_or(|kind| kind == "Environment") {
        return;
    }
    copilot_workload(parser, doc, location);
    for (name, environment) in named(parser, doc.get("environments")) {
        copilot_workload(
            parser,
            environment,
            &format!("{location}/environments/{}", pointer(name)),
        );
    }
}

/// The main container plus its sidecars; also the shape of an `environments` override.
fn copilot_workload(parser: &mut Parser<'_>, value: &Value, location: &str) {
    copilot_container(parser, value, location);
    for (name, sidecar) in named(parser, value.get("sidecars")) {
        copilot_container(
            parser,
            sidecar,
            &format!("{location}/sidecars/{}", pointer(name)),
        );
    }
}

fn copilot_container(parser: &mut Parser<'_>, value: &Value, location: &str) {
    for (field, secret) in [("variables", false), ("secrets", true)] {
        let Some(map) = value.get(field) else {
            continue;
        };
        let Some(map) = object(parser, map, "Copilot variables and secrets must be maps") else {
            continue;
        };
        for (name, value) in map {
            let location = child(&format!("{location}/{field}"), name);
            let semantics = Semantics {
                secret: Some(secret),
                scope: Some(Scope::Runtime),
                ..Semantics::default()
            };
            let reference = if let Some(export) = value.get("from_cfn") {
                Some(reference("cloudformation-export", export.as_str()))
            } else if let Some(secret) = value.get("secretsmanager") {
                Some(reference("secrets-manager", secret.as_str()))
            } else if secret && value.is_string() {
                Some(reference("ssm-parameter", value.as_str()))
            } else if !secret && (value.is_null() || scalar(value)) {
                None
            } else {
                parser.warn("unsupported Copilot variable or secret entry; declaration skipped");
                continue;
            };
            if reference.is_some() {
                parser.declare(&location, Some(name), "unresolved", reference, semantics);
            } else {
                let semantics = Semantics {
                    has_default: Some(!value.is_null()),
                    ..semantics
                };
                parser.declare(&location, Some(name), "declared", None, semantics);
            }
        }
    }
    if let Some(file) = value.get("env_file") {
        let mut reference = reference("env-file", None);
        reference.target = file
            .as_str()
            .filter(|path| safe_reference_path(path))
            .map(str::to_owned);
        parser.emit(
            &format!("{location}/env_file"),
            None,
            "unresolved",
            Some(reference),
        );
    }
}
