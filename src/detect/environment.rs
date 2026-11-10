use std::path::Path;

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::rules::{FilePattern, path_glob};
use super::{Diagnostic, relative_file};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
pub enum Format {
    Dotenv,
    Compose,
    Dockerfile,
    Kubernetes,
    GithubActions,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Spec {
    format: Format,
    files: Vec<String>,
    #[serde(default)]
    paths: Vec<String>,
    #[serde(default)]
    exclude: Vec<String>,
}

struct Entry {
    format: Format,
    files: Vec<FilePattern>,
    paths: Vec<String>,
    exclude: Vec<FilePattern>,
}

pub(super) struct Catalog(Vec<Entry>);

impl Catalog {
    pub fn new(specs: &[Spec]) -> Result<Self> {
        let mut entries = Vec::new();
        for spec in specs {
            if spec.files.is_empty()
                || spec
                    .files
                    .iter()
                    .chain(&spec.exclude)
                    .any(|file| file.is_empty() || file.contains(['/', '\\']))
                || spec.paths.iter().any(|path| {
                    path.is_empty() || path.starts_with('/') || path.split('/').any(|p| p == "..")
                })
            {
                bail!("invalid environment catalog file/path pattern");
            }
            entries.push(Entry {
                format: spec.format,
                files: spec.files.iter().map(|p| FilePattern::new(p)).collect(),
                paths: spec.paths.clone(),
                exclude: spec.exclude.iter().map(|p| FilePattern::new(p)).collect(),
            });
        }
        Ok(Self(entries))
    }

    pub fn matching(&self, directory: &Path, file: &str) -> Option<Format> {
        self.0
            .iter()
            .find(|entry| {
                entry.files.iter().any(|pattern| pattern.matches(file))
                    && !entry.exclude.iter().any(|pattern| pattern.matches(file))
                    && (entry.paths.is_empty()
                        || entry
                            .paths
                            .iter()
                            .any(|pattern| path_glob(pattern, &relative_file(directory, file))))
            })
            .map(|entry| entry.format)
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct Declaration {
    pub source: String,
    pub format: Format,
    pub location: String,
    pub project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reference: Option<Reference>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct Reference {
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
}

struct Parser<'a> {
    source: &'a str,
    format: Format,
    declarations: Vec<Declaration>,
    diagnostics: &'a mut Vec<Diagnostic>,
}

impl Parser<'_> {
    fn emit(
        &mut self,
        location: &str,
        name: Option<&str>,
        state: &str,
        reference: Option<Reference>,
    ) {
        if name.is_some_and(|name| !identifier(name)) {
            self.warn("unsupported or dynamic environment variable name");
            return;
        }
        self.declarations.push(Declaration {
            source: self.source.to_owned(),
            format: self.format,
            location: location.to_owned(),
            project: None,
            name: name.map(str::to_owned),
            state: state.to_owned(),
            reference,
        });
    }

    fn warn(&mut self, message: &str) {
        if !self
            .diagnostics
            .iter()
            .any(|d| d.path == self.source && d.message == message)
        {
            self.diagnostics.push(Diagnostic {
                path: self.source.to_owned(),
                message: message.to_owned(),
            });
        }
    }

    fn env_map(&mut self, value: &Value, location: &str, compose: bool) {
        match value {
            Value::Object(map) => {
                for (name, value) in map {
                    if !(value.is_null()
                        || value.is_string()
                        || value.is_number()
                        || value.is_boolean())
                    {
                        self.warn("environment value must be a scalar; declaration skipped");
                        continue;
                    }
                    self.emit(
                        &format!("{location}/{}", pointer(name)),
                        Some(name),
                        if compose && value.is_null() {
                            "external"
                        } else {
                            "declared"
                        },
                        None,
                    );
                }
            }
            Value::Array(list) if compose => {
                for (index, value) in list.iter().enumerate() {
                    if let Some(value) = value.as_str() {
                        let (name, state) = value
                            .split_once('=')
                            .map_or((value, "external"), |(name, _)| (name, "declared"));
                        self.emit(&format!("{location}/{index}"), Some(name), state, None);
                    } else {
                        self.warn("environment list entry must be a string");
                    }
                }
            }
            Value::String(_) => self.emit(
                location,
                None,
                "unresolved",
                Some(reference("dynamic-env", None)),
            ),
            _ => self.warn("environment declaration must be a mapping or supported list"),
        }
    }
}

pub(super) fn parse(
    format: Format,
    source: &str,
    text: &str,
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<Declaration> {
    let mut parser = Parser {
        source,
        format,
        declarations: Vec::new(),
        diagnostics,
    };
    match format {
        Format::Dotenv => dotenv(&mut parser, text),
        Format::Dockerfile => dockerfile(&mut parser, text),
        _ => {
            let options = serde_saphyr::options! {
                budget: serde_saphyr::budget! {
                    max_depth: 48,
                    max_documents: 256,
                    max_events: 100_000,
                    max_nodes: 50_000,
                    max_aliases: 128,
                    max_anchors: 128,
                    max_recorded_anchor_events: 100_000,
                    max_total_scalar_bytes: 2 * 1024 * 1024,
                    max_inclusion_depth: 0,
                },
                strict_booleans: true,
                reject_unsupported_tags: true,
            };
            match serde_saphyr::from_multiple_with_options::<Value>(text, options) {
                Ok(documents) => {
                    for (index, doc) in documents.iter().enumerate() {
                        let location = format!("document:{index}");
                        match format {
                            Format::Compose => compose(&mut parser, doc, &location),
                            Format::Kubernetes => kubernetes(&mut parser, doc, &location),
                            Format::GithubActions => actions(&mut parser, doc, &location),
                            _ => (),
                        }
                    }
                }
                Err(_) => parser.warn("environment YAML is invalid or exceeds parser limits"),
            }
        }
    }
    parser.declarations
}

fn identifier(name: &str) -> bool {
    let mut chars = name.bytes();
    chars
        .next()
        .is_some_and(|ch| ch.is_ascii_alphabetic() || ch == b'_')
        && chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, b'_' | b'.' | b'-'))
}

fn metadata_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, b'_' | b'.' | b'-'))
}

fn pointer(value: &str) -> String {
    value.replace('~', "~0").replace('/', "~1")
}

fn reference(kind: &str, target: Option<&str>) -> Reference {
    Reference {
        kind: kind.to_owned(),
        target: target
            .filter(|value| metadata_name(value))
            .map(str::to_owned),
        key: None,
        prefix: None,
    }
}

fn dotenv(parser: &mut Parser<'_>, text: &str) {
    let mut lines = text.trim_start_matches('\u{feff}').lines().enumerate();
    while let Some((index, line)) = lines.next() {
        let mut line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line
            .strip_prefix("export")
            .filter(|rest| rest.starts_with(char::is_whitespace))
        {
            line = rest.trim_start();
        }
        let Some((name, value)) = line.split_once('=') else {
            parser.warn("unsupported dotenv declaration; expected NAME=value");
            continue;
        };
        let name = name.trim();
        let value = value.trim_start();
        let mut valid = true;
        if let Some(quote) = value
            .chars()
            .next()
            .filter(|ch| matches!(ch, '\'' | '"' | '`'))
        {
            let mut tail = closing_quote(&value[quote.len_utf8()..], quote);
            while tail.is_none() {
                let Some((_, next)) = lines.next() else {
                    break;
                };
                tail = closing_quote(next, quote);
            }
            valid = tail
                .is_some_and(|tail| tail.trim().is_empty() || tail.trim_start().starts_with('#'));
        }
        if valid {
            parser.emit(&format!("line:{}", index + 1), Some(name), "declared", None);
        } else {
            parser.warn("unterminated or malformed quoted dotenv declaration");
        }
    }
}

fn closing_quote(value: &str, quote: char) -> Option<&str> {
    let mut escaped = false;
    for (index, ch) in value.char_indices() {
        if escaped {
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == quote {
            return Some(&value[index + ch.len_utf8()..]);
        }
    }
    None
}

fn words(text: &str, escape: char) -> Option<Vec<String>> {
    let mut result = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    let mut escaped = false;
    for ch in text.chars() {
        if escaped {
            word.push(ch);
            escaped = false;
        } else if ch == escape {
            escaped = true;
        } else if quote == Some(ch) {
            quote = None;
        } else if quote.is_none() && matches!(ch, '\'' | '"') {
            quote = Some(ch);
        } else if quote.is_none() && ch.is_whitespace() {
            if !word.is_empty() {
                result.push(std::mem::take(&mut word));
            }
        } else {
            word.push(ch);
        }
    }
    if quote.is_some() || escaped {
        return None;
    }
    if !word.is_empty() {
        result.push(word);
    }
    Some(result)
}

fn dockerfile(parser: &mut Parser<'_>, text: &str) {
    let mut escape = '\\';
    let mut directives = true;
    let mut stage = 0;
    let mut lines = text.lines().enumerate();
    while let Some((index, raw)) = lines.next() {
        let line = raw.trim();
        if directives {
            if let Some((key, value)) = line.strip_prefix('#').and_then(|s| s.split_once('=')) {
                if key.trim().eq_ignore_ascii_case("escape") {
                    match value.trim() {
                        "`" => escape = '`',
                        "\\" => escape = '\\',
                        _ => {
                            parser.warn("unsupported Dockerfile escape directive");
                            return;
                        }
                    }
                    continue;
                }
                if key.trim().eq_ignore_ascii_case("syntax")
                    || key.trim().eq_ignore_ascii_case("check")
                {
                    continue;
                }
            }
            directives = false;
        }
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut instruction = line.to_owned();
        while instruction.trim_end().ends_with(escape) {
            instruction.truncate(instruction.trim_end().len() - escape.len_utf8());
            let Some((_, next)) = lines
                .by_ref()
                .find(|(_, line)| !line.trim().is_empty() && !line.trim_start().starts_with('#'))
            else {
                parser.warn("incomplete Dockerfile continuation");
                return;
            };
            instruction.push_str(next.trim());
        }
        let (command, body) = instruction
            .split_once(char::is_whitespace)
            .unwrap_or((&instruction, ""));
        if command.eq_ignore_ascii_case("FROM") {
            stage += 1;
        }
        if (command.eq_ignore_ascii_case("RUN")
            || command.eq_ignore_ascii_case("COPY")
            || command.eq_ignore_ascii_case("ADD"))
            && body.contains("<<")
        {
            parser.warn("Dockerfile heredoc and subsequent instructions not inspected");
            return;
        }
        if !command.eq_ignore_ascii_case("ENV") {
            continue;
        }
        let Some(words) = words(body, escape) else {
            parser.warn("malformed Dockerfile ENV instruction");
            continue;
        };
        let Some(first) = words.first() else {
            parser.warn("empty Dockerfile ENV instruction");
            continue;
        };
        let location = format!("stage:{stage}/line:{}", index + 1);
        if first.starts_with("--") {
            parser.warn("Dockerfile ENV flags are not supported");
        } else if first.contains('=') {
            if words.iter().any(|word| !word.contains('=')) {
                parser.warn("malformed Dockerfile ENV assignment list");
                continue;
            }
            for (index, word) in words.iter().enumerate() {
                if let Some((name, _)) = word.split_once('=') {
                    parser.emit(&format!("{location}/{index}"), Some(name), "declared", None);
                }
            }
        } else if words.len() >= 2 {
            parser.emit(&location, Some(first), "declared", None);
        } else {
            parser.warn("Dockerfile ENV requires a value");
        }
    }
}

fn compose(parser: &mut Parser<'_>, doc: &Value, location: &str) {
    let Some(services) = doc.get("services").and_then(Value::as_object) else {
        return;
    };
    for (service, config) in services {
        if !identifier(service) {
            parser.warn("unsupported Compose service name");
            continue;
        }
        let location = format!("{location}/services/{}", pointer(service));
        if let Some(env) = config.get("environment") {
            parser.env_map(env, &format!("{location}/environment"), true);
        }
        if let Some(files) = config.get("env_file") {
            let list = files
                .as_array()
                .map_or_else(|| vec![files], |files| files.iter().collect());
            for (index, file) in list.iter().enumerate() {
                let path = file
                    .as_str()
                    .or_else(|| file.get("path").and_then(Value::as_str));
                let mut reference = reference("env-file", None);
                reference.target = path
                    .filter(|path| safe_reference_path(path))
                    .map(str::to_owned);
                parser.emit(
                    &format!("{location}/env_file/{index}"),
                    None,
                    "unresolved",
                    Some(reference),
                );
            }
        }
    }
}

fn safe_reference_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains(['$', '{', '}', '\\', ':'])
        && !path.chars().any(char::is_control)
}

fn actions(parser: &mut Parser<'_>, doc: &Value, location: &str) {
    if let Some(env) = doc.get("env") {
        parser.env_map(env, &format!("{location}/env"), false);
    }
    if let Some(jobs) = doc.get("jobs").and_then(Value::as_object) {
        for (job, config) in jobs {
            if !identifier(job) {
                parser.warn("unsupported Actions job name");
                continue;
            }
            let location = format!("{location}/jobs/{}", pointer(job));
            if let Some(env) = config.get("env") {
                parser.env_map(env, &format!("{location}/env"), false);
            }
            if let Some(steps) = config.get("steps").and_then(Value::as_array) {
                for (index, step) in steps.iter().enumerate() {
                    if let Some(env) = step.get("env") {
                        parser.env_map(env, &format!("{location}/steps/{index}/env"), false);
                    }
                }
            }
        }
    }
}

fn kubernetes(parser: &mut Parser<'_>, doc: &Value, location: &str) {
    let Some(api) = doc.get("apiVersion").and_then(Value::as_str) else {
        return;
    };
    let Some(kind) = doc.get("kind").and_then(Value::as_str) else {
        return;
    };
    let supported_api = match kind {
        "Pod" | "List" | "ReplicationController" => api == "v1",
        "Deployment" | "StatefulSet" | "DaemonSet" | "ReplicaSet" => api == "apps/v1",
        "Job" | "CronJob" => api == "batch/v1",
        _ => false,
    };
    if !supported_api {
        return;
    }
    if kind == "List" {
        if let Some(items) = doc.get("items").and_then(Value::as_array) {
            for (index, item) in items.iter().enumerate() {
                kubernetes(parser, item, &format!("{location}/items/{index}"));
            }
        }
        return;
    }
    let path = match kind {
        "Pod" => "/spec",
        "Deployment"
        | "StatefulSet"
        | "DaemonSet"
        | "ReplicaSet"
        | "ReplicationController"
        | "Job" => "/spec/template/spec",
        "CronJob" => "/spec/jobTemplate/spec/template/spec",
        _ => return,
    };
    let Some(spec) = doc.pointer(path) else {
        return;
    };
    for containers in ["containers", "initContainers", "ephemeralContainers"] {
        if let Some(list) = spec.get(containers).and_then(Value::as_array) {
            for (index, container) in list.iter().enumerate() {
                let location = format!("{location}{path}/{containers}/{index}");
                if let Some(env) = container.get("env") {
                    if let Some(env) = env.as_array() {
                        for (index, env) in env.iter().enumerate() {
                            let Some(name) = env.get("name").and_then(Value::as_str) else {
                                parser.warn("Kubernetes env entry requires a name");
                                continue;
                            };
                            let reference = env.get("valueFrom").map(kube_reference);
                            parser.emit(
                                &format!("{location}/env/{index}"),
                                Some(name),
                                if reference.is_some() {
                                    "unresolved"
                                } else {
                                    "declared"
                                },
                                reference,
                            );
                        }
                    } else {
                        parser.warn("Kubernetes env must be a list");
                    }
                }
                if let Some(env) = container.get("envFrom") {
                    if let Some(env) = env.as_array() {
                        for (index, env) in env.iter().enumerate() {
                            let mut reference = kube_reference(env);
                            reference.prefix = env
                                .get("prefix")
                                .and_then(Value::as_str)
                                .filter(|p| identifier(p))
                                .map(str::to_owned);
                            parser.emit(
                                &format!("{location}/envFrom/{index}"),
                                None,
                                "unresolved",
                                Some(reference),
                            );
                        }
                    } else {
                        parser.warn("Kubernetes envFrom must be a list");
                    }
                }
            }
        }
    }
}

fn kube_reference(value: &Value) -> Reference {
    for (field, kind) in [
        ("secretKeyRef", "secret-key"),
        ("configMapKeyRef", "config-map-key"),
        ("secretRef", "secret"),
        ("configMapRef", "config-map"),
    ] {
        if let Some(value) = value.get(field) {
            let mut result = reference(kind, value.get("name").and_then(Value::as_str));
            result.key = value
                .get("key")
                .and_then(Value::as_str)
                .filter(|key| metadata_name(key))
                .map(str::to_owned);
            return result;
        }
    }
    if let Some(field) = value.get("fieldRef") {
        let mut result = reference("field", None);
        result.key = field
            .get("fieldPath")
            .and_then(Value::as_str)
            .filter(|key| metadata_name(key))
            .map(str::to_owned);
        return result;
    }
    if let Some(field) = value.get("resourceFieldRef") {
        let mut result = reference(
            "resource-field",
            field.get("containerName").and_then(Value::as_str),
        );
        result.key = field
            .get("resource")
            .and_then(Value::as_str)
            .filter(|key| metadata_name(key))
            .map(str::to_owned);
        return result;
    }
    reference("unknown", None)
}
