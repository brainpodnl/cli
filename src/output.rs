use std::io::{self, Write};

use anyhow::Result;
use serde_json::Value;

pub struct CommandOutput {
    pub value: Value,
    pub view: View,
}

impl CommandOutput {
    pub const fn new(value: Value, view: View) -> Self {
        Self { value, view }
    }
}

#[derive(Clone, Copy)]
pub enum View {
    ConfigShow,
    ConfigPath,
    ConfigChange,
    Whoami,
    PodList,
    PodCreated,
    PodGet,
    RevisionList,
    RevisionGet,
    RevisionDiff,
    ResourceList,
    ResourceGet,
    ResourceMutation,
    ResourceValidation,
    Deploy,
    Redeploy,
    Events,
}

pub fn write(output: &CommandOutput, json: bool) -> Result<()> {
    let stdout = io::stdout();
    let mut stdout = stdout.lock();

    if json {
        serde_json::to_writer_pretty(&mut stdout, &output.value)?;
        writeln!(stdout)?;
        return Ok(());
    }

    for line in render(output) {
        writeln!(stdout, "{line}")?;
    }
    Ok(())
}

fn render(output: &CommandOutput) -> Vec<String> {
    match output.view {
        View::ConfigShow => render_config_show(&output.value),
        View::ConfigPath => vec![format!("Config: {}", field(&output.value, "path"))],
        View::ConfigChange => render_config_change(&output.value),
        View::Whoami => vec![format!("Email: {}", field(&output.value, "email"))],
        View::PodList => render_pod_list(&output.value),
        View::PodCreated => {
            let mut lines = vec!["Pod created".to_owned(), String::new()];
            lines.extend(render_pod(&output.value));
            lines
        }
        View::PodGet => render_pod(&output.value),
        View::RevisionList => render_revision_list(&output.value),
        View::RevisionGet => render_revision(&output.value),
        View::RevisionDiff => render_revision_diff(&output.value),
        View::ResourceList => render_resource_list(&output.value),
        View::ResourceGet => render_resource(&output.value),
        View::ResourceMutation => render_resource_mutation(&output.value),
        View::ResourceValidation => vec![format!(
            "Valid: {}",
            yes_no(value_at(&output.value, "valid"))
        )],
        View::Deploy => render_deployment(&output.value, "Deployment accepted"),
        View::Redeploy => render_deployment(&output.value, "Redeployment accepted"),
        View::Events => render_events(&output.value),
    }
}

fn render_config_show(value: &Value) -> Vec<String> {
    vec![
        format!("Config: {}", field(value, "path")),
        format!("Endpoint: {}", field(value, "endpoint")),
        format!(
            "API key configured: {}",
            yes_no(value_at(value, "apiKeyConfigured"))
        ),
        format!("Default pod: {}", field(value, "pod")),
    ]
}

fn render_config_change(value: &Value) -> Vec<String> {
    if let Some(updated) = value.get("updated") {
        vec![
            format!("Updated: {}", scalar(updated)),
            format!("Config: {}", field(value, "path")),
        ]
    } else {
        vec![
            format!("Removed: {}", field(value, "removed")),
            format!("Config: {}", field(value, "path")),
        ]
    }
}

fn render_pod_list(value: &Value) -> Vec<String> {
    let Some(pods) = value.as_array() else {
        return vec!["No pods returned.".to_owned()];
    };
    if pods.is_empty() {
        return vec!["No pods.".to_owned()];
    }

    let rows = pods
        .iter()
        .map(|pod| {
            vec![
                field(pod, "name"),
                field(pod, "displayName"),
                field(pod, "head.status"),
                revision_reference(pod, "head"),
                revision_reference(pod, "deployed"),
                field(pod, "createdAt"),
            ]
        })
        .collect();
    table(
        &[
            "NAME",
            "DISPLAY NAME",
            "HEAD STATUS",
            "HEAD",
            "DEPLOYED",
            "CREATED",
        ],
        rows,
    )
}

fn render_pod(value: &Value) -> Vec<String> {
    let mut lines = vec![
        format!("Pod: {}", field(value, "name")),
        format!("Display name: {}", field(value, "displayName")),
        format!("Owner: {}", yes_no(value_at(value, "isOwner"))),
        format!("Created: {}", field(value, "createdAt")),
        String::new(),
        "Head revision".to_owned(),
        format!("  ID: {}", field(value, "head.id")),
        format!("  Version: {}", field(value, "head.version")),
        format!("  Status: {}", field(value, "head.status")),
        format!("  Summary: {}", field(value, "head.summary")),
        format!("  Error: {}", field(value, "head.error")),
        format!("  Created: {}", field(value, "head.createdAt")),
    ];

    lines.push(String::new());
    lines.push("Deployed revision".to_owned());
    if value_at(value, "deployed").is_some_and(|value| !value.is_null()) {
        lines.extend([
            format!("  ID: {}", field(value, "deployed.id")),
            format!("  Version: {}", field(value, "deployed.version")),
            format!("  Status: {}", field(value, "deployed.status")),
            format!("  Summary: {}", field(value, "deployed.summary")),
            format!("  Created: {}", field(value, "deployed.createdAt")),
        ]);
    } else {
        lines.push("  None".to_owned());
    }
    lines
}

fn render_revision_list(value: &Value) -> Vec<String> {
    let revisions = value
        .get("items")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    if revisions.is_empty() {
        return vec!["No revisions.".to_owned()];
    }

    let rows = revisions
        .iter()
        .map(|revision| {
            vec![
                field(revision, "version"),
                field(revision, "state"),
                field(revision, "id"),
                yes_no(value_at(revision, "isLatest")),
                yes_no(value_at(revision, "isDeployed")),
                field(revision, "createdAt"),
                field(revision, "summary"),
            ]
        })
        .collect();
    let mut lines = table(
        &[
            "VERSION", "STATE", "ID", "LATEST", "DEPLOYED", "CREATED", "SUMMARY",
        ],
        rows,
    );
    append_next(value, &mut lines);
    lines
}

fn render_revision(value: &Value) -> Vec<String> {
    let mut lines = vec![
        format!("Revision: {}", field(value, "id")),
        format!("Version: {}", field(value, "version")),
        format!("State: {}", field(value, "state")),
        format!("Parent: {}", field(value, "parent")),
        format!("Latest: {}", yes_no(value_at(value, "isLatest"))),
        format!("Deployed: {}", yes_no(value_at(value, "isDeployed"))),
        format!("Summary: {}", field(value, "summary")),
        format!("Error: {}", field(value, "error")),
        format!("Checksum: {}", field(value, "checksum")),
        format!("Created: {}", field(value, "createdAt")),
    ];

    lines.push(String::new());
    lines.push("Resources".to_owned());
    let resources = value
        .get("resources")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    if resources.is_empty() {
        lines.push("  None".to_owned());
    } else {
        lines.extend(table(
            &["KIND", "NAME", "STATUS", "DETAILS"],
            resource_rows(resources),
        ));
    }
    lines
}

fn render_revision_diff(value: &Value) -> Vec<String> {
    let entries = value
        .get("entries")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let mut lines = vec![
        format!("Revision: {}", field(value, "revision")),
        format!("Base: {}", field(value, "base")),
        format!("Changes: {}", entries.len()),
    ];

    for entry in entries {
        lines.push(String::new());
        lines.push(format!(
            "{} {}/{}",
            field(entry, "changeType").to_uppercase(),
            field(entry, "kind"),
            field(entry, "name")
        ));
        if let Some(patch) = entry.get("patch").and_then(Value::as_str) {
            lines.extend(patch.lines().map(|line| format!("  {line}")));
        }
    }
    lines
}

fn render_resource_list(value: &Value) -> Vec<String> {
    let Some(resources) = value.as_array() else {
        return vec!["No resources returned.".to_owned()];
    };
    if resources.is_empty() {
        return vec!["No resources.".to_owned()];
    }
    table(
        &["KIND", "NAME", "STATUS", "DETAILS"],
        resource_rows(resources),
    )
}

fn render_resource(value: &Value) -> Vec<String> {
    let content = value.get("content").unwrap_or(&Value::Null);
    let mut lines = vec![
        format!(
            "Resource: {}/{}/{}",
            field(content, "kind"),
            field(content, "metadata.namespace"),
            field(content, "metadata.name")
        ),
        format!("URN: {}", field(value, "urn")),
        format!("API version: {}", field(content, "apiVersion")),
        format!("Status: {}", resource_status(value)),
        String::new(),
        "Spec".to_owned(),
    ];
    render_node(content.get("spec").unwrap_or(&Value::Null), 2, &mut lines);
    lines
}

fn render_resource_mutation(value: &Value) -> Vec<String> {
    let resources = value
        .get("resources")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let mut lines = vec![
        format!("Revision: {}", field(value, "revisionId")),
        format!("Resources changed: {}", resources.len()),
    ];
    if !resources.is_empty() {
        lines.push(String::new());
        lines.extend(table(
            &["KIND", "NAME", "STATUS", "DETAILS"],
            resource_rows(resources),
        ));
    }
    lines
}

fn render_deployment(value: &Value, heading: &str) -> Vec<String> {
    vec![
        heading.to_owned(),
        format!("Revision: {}", field(value, "revisionId")),
    ]
}

fn render_events(value: &Value) -> Vec<String> {
    let events = value
        .get("items")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    if events.is_empty() {
        return vec!["No events.".to_owned()];
    }

    let mut lines = Vec::with_capacity(events.len() + 2);
    for event in events {
        let timestamp = field(event, "timestamp");
        let kind = field(event, "kind");
        let body = field(event, "body").replace('\n', "\\n");
        let line = match kind.as_str() {
            "app" => format!(
                "{timestamp} {:<5} {body}",
                field(event, "level").to_uppercase()
            ),
            "k8s" => format!("{timestamp} K8S   {}: {body}", field(event, "reason")),
            "httpAccess" => format!(
                "{timestamp} HTTP  {} {} {}{} {}ms",
                field(event, "status"),
                field(event, "method"),
                field(event, "host"),
                field(event, "path"),
                field(event, "durationMs")
            ),
            _ => format!("{timestamp} {kind} {body}"),
        };
        lines.push(line);
    }
    append_next(value, &mut lines);
    lines
}

fn revision_reference(value: &Value, path: &str) -> String {
    let Some(revision) = value_at(value, path).filter(|revision| !revision.is_null()) else {
        return "-".to_owned();
    };
    let version = revision.get("version").map(scalar);
    let id = revision.get("id").map(scalar);
    match (version, id) {
        (Some(version), Some(id)) => format!("v{version} ({id})"),
        (Some(version), None) => format!("v{version}"),
        (None, Some(id)) => id,
        (None, None) => "-".to_owned(),
    }
}

fn resource_rows(resources: &[Value]) -> Vec<Vec<String>> {
    resources
        .iter()
        .map(|resource| {
            vec![
                field(resource, "content.kind"),
                field(resource, "content.metadata.name"),
                resource_status(resource),
                resource_details(resource),
            ]
        })
        .collect()
}

fn resource_status(resource: &Value) -> String {
    let Some(status) = resource.get("status").filter(|status| !status.is_null()) else {
        return "-".to_owned();
    };
    if let Some(phase) = status.get("phase") {
        let mut result = scalar(phase);
        if let Some(ready) = status.get("readyReplicas") {
            let replicas = value_at(resource, "content.spec.replicas")
                .map(scalar)
                .unwrap_or_else(|| "?".to_owned());
            result.push_str(&format!(" ({}/{replicas} ready)", scalar(ready)));
        } else if let Some(ready) = status.get("ready") {
            result.push_str(&format!(" (ready: {})", yes_no(Some(ready))));
        }
        return result;
    }
    if let Some(ready) = status.get("ready") {
        return if ready.as_bool() == Some(true) {
            "Ready".to_owned()
        } else {
            "Not ready".to_owned()
        };
    }
    scalar(status)
}

fn resource_details(resource: &Value) -> String {
    let kind = field(resource, "content.kind");
    match kind.as_str() {
        "App" => format!(
            "image={}, instance={}, replicas={}",
            field(resource, "content.spec.image"),
            field(resource, "content.spec.instance"),
            field(resource, "content.spec.replicas")
        ),
        "Config" => format!(
            "files={}",
            value_at(resource, "content.spec.files")
                .and_then(Value::as_object)
                .map(|files| files.len().to_string())
                .unwrap_or_else(|| "0".to_owned())
        ),
        "Route" => format!(
            "hostname={}, rules={}",
            field(resource, "content.spec.hostname"),
            value_at(resource, "content.spec.rules")
                .and_then(Value::as_array)
                .map(|rules| rules.len().to_string())
                .unwrap_or_else(|| "0".to_owned())
        ),
        "Disk" => format!("size={} GB", field(resource, "content.spec.size")),
        "Postgres" | "MariaDB" | "Valkey" => format!(
            "version={}, instance={}, disk={}",
            field(resource, "content.spec.version"),
            field(resource, "content.spec.instance"),
            field(resource, "content.spec.diskRef")
        ),
        _ => "-".to_owned(),
    }
}

fn render_node(value: &Value, indent: usize, lines: &mut Vec<String>) {
    let padding = " ".repeat(indent);
    match value {
        Value::Object(object) if object.is_empty() => lines.push(format!("{padding}{{}}")),
        Value::Object(object) => {
            for (key, value) in object {
                if value.is_object() || value.is_array() {
                    lines.push(format!("{padding}{key}:"));
                    render_node(value, indent + 2, lines);
                } else {
                    lines.push(format!("{padding}{key}: {}", scalar(value)));
                }
            }
        }
        Value::Array(values) if values.is_empty() => lines.push(format!("{padding}[]")),
        Value::Array(values) => {
            for value in values {
                if value.is_object() || value.is_array() {
                    lines.push(format!("{padding}-"));
                    render_node(value, indent + 2, lines);
                } else {
                    lines.push(format!("{padding}- {}", scalar(value)));
                }
            }
        }
        _ => lines.push(format!("{padding}{}", scalar(value))),
    }
}

fn table(headers: &[&str], rows: Vec<Vec<String>>) -> Vec<String> {
    let mut widths: Vec<usize> = headers
        .iter()
        .map(|header| header.chars().count())
        .collect();
    let rows: Vec<Vec<String>> = rows
        .into_iter()
        .map(|row| row.into_iter().map(|cell| sanitize_cell(&cell)).collect())
        .collect();

    for row in &rows {
        for (index, cell) in row.iter().enumerate() {
            if let Some(width) = widths.get_mut(index) {
                *width = (*width).max(cell.chars().count());
            }
        }
    }

    let mut lines = vec![format_row(
        &headers
            .iter()
            .map(|header| (*header).to_owned())
            .collect::<Vec<_>>(),
        &widths,
    )];
    lines.push(
        widths
            .iter()
            .map(|width| "-".repeat(*width))
            .collect::<Vec<_>>()
            .join("  "),
    );
    lines.extend(rows.iter().map(|row| format_row(row, &widths)));
    lines
}

fn format_row(row: &[String], widths: &[usize]) -> String {
    row.iter()
        .enumerate()
        .map(|(index, cell)| {
            let width = widths.get(index).copied().unwrap_or_default();
            format!("{cell:<width$}")
        })
        .collect::<Vec<_>>()
        .join("  ")
        .trim_end()
        .to_owned()
}

fn sanitize_cell(value: &str) -> String {
    value.replace('\r', "\\r").replace('\n', "\\n")
}

fn append_next(value: &Value, lines: &mut Vec<String>) {
    if let Some(next) = value_at(value, "_links.next.href") {
        lines.push(String::new());
        lines.push(format!("Next: {}", scalar(next)));
    }
}

fn field(value: &Value, path: &str) -> String {
    value_at(value, path)
        .filter(|value| !value.is_null())
        .map(scalar)
        .unwrap_or_else(|| "-".to_owned())
}

fn value_at<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.').try_fold(value, |value, key| value.get(key))
}

fn scalar(value: &Value) -> String {
    match value {
        Value::Null => "-".to_owned(),
        Value::String(value) => value.clone(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        _ => value.to_string(),
    }
}

fn yes_no(value: Option<&Value>) -> String {
    match value.and_then(Value::as_bool) {
        Some(true) => "yes".to_owned(),
        Some(false) => "no".to_owned(),
        None => "-".to_owned(),
    }
}
