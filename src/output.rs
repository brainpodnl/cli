use std::io::{self, IsTerminal, Write};

use anyhow::{Context, Result, anyhow};
use serde_json::{Value, json};

use crate::client::{EventStreamMessage, EventWatch};

pub enum CommandOutput {
    Buffered { value: Value, view: View },
    EventWatch(EventWatch),
}

impl CommandOutput {
    pub const fn new(value: Value, view: View) -> Self {
        Self::Buffered { value, view }
    }

    pub const fn event_watch(watch: EventWatch) -> Self {
        Self::EventWatch(watch)
    }
}

#[derive(Clone, Copy)]
pub enum View {
    Describe,
    ConfigShow,
    ConfigPath,
    ConfigChange,
    Whoami,
    PodList,
    PodCreated,
    PodGet,
    BlueprintList,
    BlueprintGet,
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

pub async fn write(output: CommandOutput, json: bool) -> Result<()> {
    match output {
        CommandOutput::Buffered { value, view } => write_buffered(&value, view, json),
        CommandOutput::EventWatch(watch) => write_event_watch(watch, json).await,
    }
}

fn write_buffered(value: &Value, view: View, json: bool) -> Result<()> {
    let stdout = io::stdout();
    let color = !json && stdout.is_terminal();
    let mut stdout = stdout.lock();

    if json {
        serde_json::to_writer_pretty(&mut stdout, value)?;
        writeln!(stdout)?;
        return Ok(());
    }

    for line in render(value, view, color) {
        writeln!(stdout, "{line}")?;
    }
    Ok(())
}

async fn write_event_watch(mut watch: EventWatch, json: bool) -> Result<()> {
    let stdout = io::stdout();
    let color = !json && stdout.is_terminal();
    let mut stdout = stdout.lock();

    while let Some(message) = watch.next().await? {
        match message.event.as_str() {
            "event" | "message" => {
                if json {
                    write_stream_json(&mut stdout, &message)?;
                } else {
                    let value: Value = serde_json::from_str(&message.data)
                        .context("Brainpod event stream returned invalid event JSON")?;
                    writeln!(stdout, "{}", render_event(&value, color))?;
                }
                stdout.flush()?;
            }
            "error" => return Err(stream_error(&message.data)),
            event => {
                return Err(anyhow!(
                    "Brainpod event stream returned unknown event {event}"
                ));
            }
        }
    }

    Err(anyhow!("Brainpod event stream ended without an end event"))
}

fn write_stream_json(writer: &mut impl Write, message: &EventStreamMessage) -> Result<()> {
    let data = serde_json::from_str(&message.data).unwrap_or_else(|_| json!(message.data));
    serde_json::to_writer(
        &mut *writer,
        &json!({
            "event": &message.event,
            "id": &message.id,
            "data": data,
        }),
    )?;
    writeln!(writer)?;
    Ok(())
}

fn stream_error(data: &str) -> anyhow::Error {
    let message = serde_json::from_str::<Value>(data)
        .ok()
        .and_then(|value| {
            value
                .pointer("/error/message")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| data.to_owned());
    anyhow!("Brainpod event stream returned an error: {message}")
}

fn render(value: &Value, view: View, color: bool) -> Vec<String> {
    match view {
        View::Describe => render_describe(value),
        View::ConfigShow => render_config_show(value),
        View::ConfigPath => vec![format!("Config: {}", field(value, "path"))],
        View::ConfigChange => render_config_change(value),
        View::Whoami => vec![format!("Email: {}", field(value, "email"))],
        View::PodList => render_pod_list(value),
        View::PodCreated => {
            let mut lines = vec!["Pod created".to_owned(), String::new()];
            lines.extend(render_pod(value));
            lines
        }
        View::PodGet => render_pod(value),
        View::BlueprintList => render_blueprint_list(value),
        View::BlueprintGet => render_blueprint(value),
        View::RevisionList => render_revision_list(value),
        View::RevisionGet => render_revision(value),
        View::RevisionDiff => render_revision_diff(value),
        View::ResourceList => render_resource_list(value),
        View::ResourceGet => render_resource(value),
        View::ResourceMutation => render_resource_mutation(value),
        View::ResourceValidation => vec![format!("Valid: {}", yes_no(value_at(value, "valid")))],
        View::Deploy => render_deployment(value, "Deployment accepted"),
        View::Redeploy => render_deployment(value, "Redeployment accepted"),
        View::Events => render_events(value, color),
    }
}

fn render_describe(value: &Value) -> Vec<String> {
    let Some(command) = value.get("command") else {
        return vec!["No command description returned.".to_owned()];
    };

    let mut lines = vec![
        field(command, "invocation"),
        field(command, "summary"),
        String::new(),
        format!("Usage: {}", field(command, "usage")),
        format!(
            "API key: {}",
            requirement(command.pointer("/requirements/apiKey"))
        ),
        format!("Pod: {}", requirement(command.pointer("/requirements/pod"))),
        format!("Effect: {}", field(command, "effectDescription")),
    ];

    let arguments = command
        .get("arguments")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    if !arguments.is_empty() {
        lines.push(String::new());
        lines.push("Arguments".to_owned());
        lines.extend(arguments.iter().map(render_describe_argument));
    }

    let global_arguments = value
        .get("globalArguments")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    if !global_arguments.is_empty() {
        lines.push(String::new());
        lines.push("Global options".to_owned());
        lines.extend(global_arguments.iter().map(render_describe_argument));
    }

    let subcommands = command
        .get("subcommands")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    if !subcommands.is_empty() {
        lines.push(String::new());
        lines.push("Subcommands".to_owned());
        lines.extend(table(
            &["COMMAND", "DESCRIPTION", "EFFECT"],
            subcommands
                .iter()
                .map(|subcommand| {
                    vec![
                        field(subcommand, "invocation"),
                        field(subcommand, "summary"),
                        field(subcommand, "effect"),
                    ]
                })
                .collect(),
        ));
    }

    let examples = command
        .get("examples")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    if !examples.is_empty() {
        lines.push(String::new());
        lines.push("Examples".to_owned());
        lines.extend(examples.iter().map(|example| format!("  {}", scalar(example))));
    }

    let next_steps = command
        .get("nextSteps")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    if !next_steps.is_empty() {
        lines.push(String::new());
        lines.push("Next steps".to_owned());
        lines.extend(
            next_steps
                .iter()
                .map(|item| format!("  - {}", scalar(item))),
        );
    }

    let guidance = value
        .get("guidance")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    if !guidance.is_empty() {
        lines.push(String::new());
        lines.push("Guidance".to_owned());
        lines.extend(
            guidance
                .iter()
                .map(|item| format!("  - {}", scalar(item))),
        );
    }

    lines
}

fn render_describe_argument(argument: &Value) -> String {
    let required = if value_at(argument, "required").and_then(Value::as_bool) == Some(true) {
        " (required)"
    } else {
        ""
    };
    format!(
        "  {}{required}: {}",
        field(argument, "syntax"),
        field(argument, "help")
    )
}

fn requirement(value: Option<&Value>) -> &'static str {
    match value.and_then(Value::as_bool) {
        Some(true) => "required",
        Some(false) => "not required",
        None => "depends on subcommand",
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

fn render_blueprint_list(value: &Value) -> Vec<String> {
    let Some(blueprints) = value.as_array() else {
        return vec!["No blueprints returned.".to_owned()];
    };
    if blueprints.is_empty() {
        return vec!["No blueprints.".to_owned()];
    }

    let rows = blueprints
        .iter()
        .map(|blueprint| {
            vec![
                field(blueprint, "id"),
                field(blueprint, "name"),
                field(blueprint, "category"),
                field(blueprint, "version"),
                string_list(blueprint.get("tags")),
                field(blueprint, "tagline"),
            ]
        })
        .collect();
    table(
        &["ID", "NAME", "CATEGORY", "VERSION", "TAGS", "TAGLINE"],
        rows,
    )
}

fn render_blueprint(value: &Value) -> Vec<String> {
    let mut lines = vec![
        format!("Blueprint: {}", field(value, "name")),
        format!("ID: {}", field(value, "id")),
        format!("Category: {}", field(value, "category")),
        format!("Version: {}", field(value, "version")),
        format!("Tags: {}", string_list(value.get("tags"))),
        format!("Tagline: {}", field(value, "tagline")),
        format!("Description: {}", field(value, "description")),
        String::new(),
        "Documentation".to_owned(),
    ];
    let body = value.get("body").and_then(Value::as_str).unwrap_or_default();
    if body.is_empty() {
        lines.push("  None".to_owned());
    } else {
        lines.extend(body.lines().map(|line| format!("  {line}")));
    }

    lines.push(String::new());
    lines.push("Defaults".to_owned());
    render_node(value.get("defaults").unwrap_or(&Value::Null), 2, &mut lines);
    lines.push(String::new());
    lines.push("Input schema".to_owned());
    render_node(
        value.get("inputSchema").unwrap_or(&Value::Null),
        2,
        &mut lines,
    );
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

fn render_events(value: &Value, color: bool) -> Vec<String> {
    let events = value
        .get("items")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    if events.is_empty() {
        return vec!["No events.".to_owned()];
    }

    let mut lines = events
        .iter()
        .map(|event| render_event(event, color))
        .collect::<Vec<_>>();
    append_next(value, &mut lines);
    lines
}

fn render_event(event: &Value, color: bool) -> String {
    let timestamp = style(&field(event, "timestamp"), "2", color);
    let kind = field(event, "kind");
    let body = field(event, "body").replace('\n', "\\n");
    match kind.as_str() {
        "app" => {
            let level = field(event, "level").to_uppercase();
            let level = format!("{level:<5}");
            let code = match level.trim() {
                "TRACE" => "2",
                "DEBUG" => "36",
                "INFO" => "32",
                "WARN" => "33",
                "ERROR" => "1;31",
                _ => "",
            };
            format!("{timestamp} {} {body}", style(&level, code, color))
        }
        "platform" => format!(
            "{timestamp} {} {}: {body}",
            style("PLATFORM", "35", color),
            field(event, "reason")
        ),
        "httpAccess" => {
            let status = field(event, "status");
            let code = match status.chars().next() {
                Some('2') => "32",
                Some('3') => "36",
                Some('4') => "33",
                Some('5') => "1;31",
                _ => "",
            };
            format!(
                "{timestamp} {} {} {} {}{} {}ms",
                style("HTTP ", "36", color),
                style(&status, code, color),
                field(event, "method"),
                field(event, "host"),
                field(event, "path"),
                field(event, "durationMs")
            )
        }
        _ => format!("{timestamp} {} {body}", style(&kind, "36", color)),
    }
}

fn style(value: &str, code: &str, enabled: bool) -> String {
    if enabled && !code.is_empty() {
        format!("\u{1b}[{code}m{value}\u{1b}[0m")
    } else {
        value.to_owned()
    }
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

fn string_list(value: Option<&Value>) -> String {
    value
        .and_then(Value::as_array)
        .map(|values| values.iter().map(scalar).collect::<Vec<_>>().join(", "))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "-".to_owned())
}

fn yes_no(value: Option<&Value>) -> String {
    match value.and_then(Value::as_bool) {
        Some(true) => "yes".to_owned(),
        Some(false) => "no".to_owned(),
        None => "-".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::client::EventStreamMessage;

    use super::{render_event, stream_error, write_stream_json};

    #[test]
    fn renders_platform_event() {
        let event = json!({
            "timestamp": "2026-08-03T12:00:00Z",
            "kind": "platform",
            "reason": "Started",
            "body": "Container started",
        });

        assert_eq!(
            render_event(&event, false),
            "2026-08-03T12:00:00Z PLATFORM Started: Container started"
        );
    }

    #[test]
    fn colors_event_level_when_enabled() {
        let event = json!({
            "timestamp": "2026-08-03T12:00:00Z",
            "kind": "app",
            "level": "info",
            "body": "Ready",
        });

        assert_eq!(
            render_event(&event, true),
            "\u{1b}[2m2026-08-03T12:00:00Z\u{1b}[0m \u{1b}[32mINFO \u{1b}[0m Ready"
        );
    }

    #[test]
    fn writes_stream_message_as_ndjson() {
        let message = EventStreamMessage {
            event: "event".to_owned(),
            id: Some("event-1".to_owned()),
            data: r#"{"kind":"app"}"#.to_owned(),
        };
        let mut output = Vec::new();

        write_stream_json(&mut output, &message).unwrap();

        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&output).unwrap(),
            json!({
                "event": "event",
                "id": "event-1",
                "data": {"kind": "app"},
            })
        );
    }

    #[test]
    fn extracts_stream_error_message() {
        let error = stream_error(r#"{"error":{"message":"stream failed"}}"#);

        assert_eq!(
            error.to_string(),
            "Brainpod event stream returned an error: stream failed"
        );
    }
}
