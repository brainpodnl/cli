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
    ResourceSchema,
    Login,
    ConfigShow,
    ConfigPath,
    ConfigChange,
    Whoami,
    ClusterList,
    PodList,
    PodCreated,
    PodGet,
    BlueprintList,
    BlueprintGet,
    ImageBuild,
    ImageList,
    ImageInspect,
    RevisionList,
    RevisionGet,
    RevisionDiff,
    RevisionWait,
    ResourceList,
    ResourceGet,
    ResourceMutation,
    ResourceValidation,
    Deploy,
    DeployWait,
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
        if matches!(view, View::Login) {
            write_login_json(&mut stdout, value)?;
        } else {
            serde_json::to_writer_pretty(&mut stdout, value)?;
            writeln!(stdout)?;
        }
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

fn write_login_json(writer: &mut impl Write, user: &Value) -> Result<()> {
    serde_json::to_writer(
        &mut *writer,
        &json!({
            "event": "authenticated",
            "user": user,
        }),
    )?;
    writeln!(writer)?;
    Ok(())
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
        View::ResourceSchema => render_resource_schema(value),
        View::Login => vec![format!("Authenticated as: {}", field(value, "email"))],
        View::ConfigShow => render_config_show(value),
        View::ConfigPath => vec![format!("Config: {}", field(value, "path"))],
        View::ConfigChange => render_config_change(value),
        View::Whoami => render_whoami(value),
        View::ClusterList => render_cluster_list(value),
        View::PodList => render_pod_list(value),
        View::PodCreated => {
            let mut lines = vec!["Pod created".to_owned(), String::new()];
            lines.extend(render_pod(value));
            lines
        }
        View::PodGet => render_pod(value),
        View::BlueprintList => render_blueprint_list(value),
        View::BlueprintGet => render_blueprint(value),
        View::ImageBuild => render_image_build(value),
        View::ImageList => render_image_list(value),
        View::ImageInspect => render_image_inspect(value),
        View::RevisionList => render_revision_list(value),
        View::RevisionGet => render_revision(value),
        View::RevisionDiff => render_revision_diff(value),
        View::RevisionWait => render_healthy_revision(value, "Revision is healthy"),
        View::ResourceList => render_resource_list(value),
        View::ResourceGet => render_resource(value),
        View::ResourceMutation => render_resource_mutation(value),
        View::ResourceValidation => vec![format!("Valid: {}", yes_no(value_at(value, "valid")))],
        View::Deploy => render_deployment(value, "Deployment accepted"),
        View::DeployWait => render_healthy_revision(value, "Deployment is healthy"),
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
            "API token: {}",
            requirement(command.pointer("/requirements/apiToken"))
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

    if let Some(resource_schemas) = value.get("resourceSchemas") {
        lines.push(String::new());
        lines.extend(render_resource_schema_list(resource_schemas, "Resource kinds"));
    }

    lines
}

fn render_resource_schema(value: &Value) -> Vec<String> {
    if value.get("resource").and_then(Value::as_str).is_none() {
        return render_resource_schema_list(value, "Resource schemas");
    }

    let mut lines = vec![
        format!("Source: {}", field(value, "source")),
        format!("OpenAPI: {}", field(value, "sourceUrl")),
        format!("Resource: {}", field(value, "resource")),
        String::new(),
        "Required fields".to_owned(),
        format!("  {}", schema_required(value.get("schema"))),
        String::new(),
        "Properties".to_owned(),
    ];
    render_schema_properties(value.get("schema"), 2, 0, &mut lines);
    lines
}

fn render_resource_schema_list(value: &Value, heading: &str) -> Vec<String> {
    let mut lines = vec![
        format!("Source: {}", field(value, "source")),
        format!("OpenAPI: {}", field(value, "sourceUrl")),
        String::new(),
        heading.to_owned(),
    ];
    let rows = value
        .get("resources")
        .and_then(Value::as_array)
        .map(|resources| {
            resources
                .iter()
                .map(|resource| {
                    vec![
                        field(resource, "kind"),
                        schema_required(resource.get("schema")),
                    ]
                })
                .collect()
        })
        .unwrap_or_default();
    lines.extend(table(&["KIND", "REQUIRED FIELDS"], rows));
    lines
}

fn schema_required(schema: Option<&Value>) -> String {
    schema
        .and_then(|schema| schema.get("required"))
        .and_then(Value::as_array)
        .map(|required| required.iter().map(scalar).collect::<Vec<_>>().join(", "))
        .filter(|required| !required.is_empty())
        .unwrap_or_else(|| "none".to_owned())
}

fn render_schema_properties(
    schema: Option<&Value>,
    indent: usize,
    depth: usize,
    lines: &mut Vec<String>,
) {
    let Some(properties) = schema
        .and_then(|schema| schema.get("properties"))
        .and_then(Value::as_object)
    else {
        lines.push(format!("{}none", " ".repeat(indent)));
        return;
    };

    let required = schema
        .and_then(|schema| schema.get("required"))
        .and_then(Value::as_array);
    for (name, property) in properties {
        let marker = if required.is_some_and(|required| {
            required.iter().any(|value| value.as_str() == Some(name))
        }) {
            " (required)"
        } else {
            ""
        };
        lines.push(format!(
            "{}{name}{marker}: {}",
            " ".repeat(indent),
            schema_summary(property)
        ));
        if depth < 2 && property.get("properties").is_some() {
            render_schema_properties(Some(property), indent + 2, depth + 1, lines);
        }
    }
}

fn schema_summary(schema: &Value) -> String {
    let mut summary = schema
        .get("const")
        .map(scalar)
        .map(|value| format!("const {value}"))
        .or_else(|| {
            schema.get("type").and_then(Value::as_str).map(str::to_owned)
        })
        .unwrap_or_else(|| {
            schema
                .get("oneOf")
                .and_then(Value::as_array)
                .map(|variants| format!("one of {} variants", variants.len()))
                .unwrap_or_else(|| "schema".to_owned())
        });

    if let Some(values) = schema.get("enum").and_then(Value::as_array) {
        summary.push_str(&format!(
            " [{}]",
            values.iter().map(scalar).collect::<Vec<_>>().join(", ")
        ));
    }
    for key in ["minLength", "maxLength", "minimum", "maximum", "default"] {
        if let Some(value) = schema.get(key) {
            summary.push_str(&format!(" {key}={}", scalar(value)));
        }
    }
    summary
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
        format!("Registry endpoint: {}", field(value, "registryEndpoint")),
        format!(
            "API token configured: {}",
            yes_no(value_at(value, "apiTokenConfigured"))
        ),
        format!("Default pod: {}", field(value, "pod")),
        format!("Default architecture: {}", field(value, "architecture")),
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

fn render_whoami(value: &Value) -> Vec<String> {
    let mut lines = vec![format!("Email: {}", field(value, "email"))];

    lines.push(String::new());
    render_whoami_policy(value.get("policy"), &mut lines);

    lines.push(String::new());
    lines.push("Permissions".to_owned());
    match value.get("permissions").and_then(Value::as_array) {
        Some(permissions) if !permissions.is_empty() => {
            for permission in permissions {
                lines.push(format!("  {}", field(permission, "action")));
                lines.push(format!(
                    "    Resources: {}",
                    whoami_list(permission.get("resources"))
                ));
                lines.push(format!(
                    "    Excluded resources: {}",
                    whoami_list(permission.get("excludedResources"))
                ));
            }
        }
        _ => lines.push("  none".to_owned()),
    }

    lines.push(String::new());
    lines.push("Links".to_owned());
    let links = value.get("_links").and_then(Value::as_object);
    let link_labels = [
        ("self", "Self"),
        ("pods", "Pods"),
        ("blueprints", "Blueprints"),
        ("clusters", "Clusters"),
    ];
    if links.is_none_or(|links| links.is_empty()) {
        lines.push("  none".to_owned());
    } else {
        for (key, label) in link_labels {
            if let Some(href) = links
                .and_then(|links| links.get(key))
                .and_then(|link| link.get("href"))
            {
                lines.push(format!("  {label}: {}", scalar(href)));
            }
        }
    }

    lines
}

fn render_whoami_policy(policy: Option<&Value>, lines: &mut Vec<String>) {
    let Some(policy) = policy.filter(|policy| !policy.is_null()) else {
        lines.push("Policy: none".to_owned());
        return;
    };

    lines.push(format!("Policy (version {}):", field(policy, "version")));
    let Some(statements) = policy.get("statements").and_then(Value::as_array) else {
        lines.push("  none".to_owned());
        return;
    };
    if statements.is_empty() {
        lines.push("  none".to_owned());
        return;
    }

    for statement in statements {
        lines.push(format!(
            "  {} ({})",
            field(statement, "sid"),
            field(statement, "effect")
        ));
        lines.push(format!(
            "    Actions: {}",
            whoami_list(statement.get("actions"))
        ));
        lines.push(format!(
            "    Resources: {}",
            whoami_list(statement.get("resources"))
        ));

        if let Some(conditions) = statement
            .get("conditions")
            .and_then(Value::as_object)
            .filter(|conditions| !conditions.is_empty())
        {
            lines.push("    Conditions:".to_owned());
            for (key, condition) in conditions {
                lines.push(format!("      {key}: {}", whoami_value(condition)));
            }
        }
    }
}

fn whoami_list(value: Option<&Value>) -> String {
    let Some(values) = value.and_then(Value::as_array) else {
        return "none".to_owned();
    };
    if values.is_empty() {
        "none".to_owned()
    } else {
        values.iter().map(scalar).collect::<Vec<_>>().join(", ")
    }
}

fn whoami_value(value: &Value) -> String {
    match value {
        Value::Array(_) => whoami_list(Some(value)),
        _ => scalar(value),
    }
}

fn render_cluster_list(value: &Value) -> Vec<String> {
    let Some(clusters) = value.as_array() else {
        return vec!["No clusters returned.".to_owned()];
    };
    if clusters.is_empty() {
        return vec!["No clusters.".to_owned()];
    }

    let rows = clusters
        .iter()
        .map(|cluster| {
            vec![
                field(cluster, "id"),
                field(cluster, "provider"),
                field(cluster, "region"),
                string_list(cluster.get("architectures")),
            ]
        })
        .collect();
    table(
        &["ID", "PROVIDER", "REGION", "ARCHITECTURES"],
        rows,
    )
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

fn render_image_build(value: &Value) -> Vec<String> {
    let mut lines = vec![
        "Image built and pushed".to_owned(),
        String::new(),
        format!("Image: {}", field(value, "image")),
        format!("Digest: {}", field(value, "digest")),
        format!("Reference: {}", field(value, "reference")),
        format!("Platform: {}", field(value, "platform")),
        format!("Builder: {}", field(value, "builder")),
        format!(
            "User: {}",
            value
                .get("user")
                .filter(|user| !user.is_null())
                .map(scalar)
                .unwrap_or_else(|| "root (default)".to_owned())
        ),
    ];
    if value
        .get("railpackVersion")
        .is_some_and(|version| !version.is_null())
    {
        lines.push(format!("Railpack: {}", field(value, "railpackVersion")));
    }
    if value.get("output").is_some_and(|output| !output.is_null()) {
        lines.push(format!("OCI layout: {}", field(value, "output")));
    }
    lines
}

fn render_image_list(value: &Value) -> Vec<String> {
    let images = value
        .get("items")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let mut lines = vec![format!("Total: {}", field(value, "total"))];
    if images.is_empty() {
        lines.push("No images.".to_owned());
        return lines;
    }

    lines.push(String::new());
    lines.extend(table(
        &[
            "REPOSITORY",
            "TAG",
            "NAMESPACE",
            "ARCHITECTURES",
            "DIGEST",
            "CREATED",
        ],
        images
            .iter()
            .map(|image| {
                vec![
                    field(image, "repository"),
                    field(image, "tag"),
                    field(image, "namespace"),
                    truncated_string_list(image.get("architectures"), 2),
                    field(image, "digest"),
                    field(image, "createdAt"),
                ]
            })
            .collect(),
    ));
    append_next(value, &mut lines);
    lines
}

fn render_image_inspect(value: &Value) -> Vec<String> {
    let mut lines = vec![
        format!("Repository: {}", field(value, "repository")),
        format!("Tag: {}", field(value, "tag")),
        format!("Namespace: {}", field(value, "namespace")),
        format!("Visibility: {}", field(value, "visibility")),
        format!("Reference: {}", field(value, "reference")),
        String::new(),
        "Variants".to_owned(),
    ];
    let variants = value
        .get("variants")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    if variants.is_empty() {
        lines.push("  None".to_owned());
        return lines;
    }

    lines.extend(table(
        &[
            "ARCHITECTURE",
            "DIGEST",
            "REFERENCE",
            "UID",
            "GID",
            "EXPOSED PORTS",
            "CREATED",
            "UPDATED",
        ],
        variants
            .iter()
            .map(|variant| {
                vec![
                    field(variant, "architecture"),
                    field(variant, "digest"),
                    field(variant, "reference"),
                    field(variant, "uid"),
                    field(variant, "gid"),
                    string_list(variant.get("exposedPorts")),
                    field(variant, "createdAt"),
                    field(variant, "updatedAt"),
                ]
            })
            .collect(),
    ));
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
            &["KIND", "NAME", "HEALTHY", "STATUS", "DETAILS"],
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
        &["KIND", "NAME", "HEALTHY", "STATUS", "DETAILS"],
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
        format!("Healthy: {}", yes_no(value.get("healthy"))),
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
            &["KIND", "NAME", "HEALTHY", "STATUS", "DETAILS"],
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

fn render_healthy_revision(value: &Value, heading: &str) -> Vec<String> {
    let mut lines = vec![heading.to_owned(), String::new()];
    lines.extend(render_revision(value));
    lines
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
                yes_no(resource.get("healthy")),
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

fn truncated_string_list(value: Option<&Value>, limit: usize) -> String {
    let Some(values) = value.and_then(Value::as_array) else {
        return "-".to_owned();
    };
    if values.is_empty() {
        return "-".to_owned();
    }

    let mut items = values
        .iter()
        .take(limit)
        .map(scalar)
        .collect::<Vec<_>>();
    if values.len() > limit {
        items.push("...".to_owned());
    }
    items.join(", ")
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

    use super::{
        render_cluster_list, render_event, render_image_inspect, render_image_list,
        render_whoami, stream_error, write_login_json, write_stream_json,
    };

    #[test]
    fn renders_extended_current_user() {
        let lines = render_whoami(&json!({
            "email": "user@example.com",
            "policy": {
                "version": "1",
                "statements": [{
                    "sid": "pods-read",
                    "effect": "allow",
                    "actions": ["pods:read"],
                    "resources": ["urn:brain:pod:*"]
                }]
            },
            "permissions": [{
                "action": "pods:read",
                "resources": ["urn:brain:pod:default:demo"],
                "excludedResources": []
            }],
            "_links": {
                "self": {"href": "/v1/me"},
                "pods": {"href": "/v1/pods"},
                "blueprints": {"href": "/v1/blueprints"},
                "clusters": {"href": "/v1/clusters"}
            }
        }));

        assert_eq!(lines[0], "Email: user@example.com");
        assert!(lines.iter().any(|line| line == "Policy (version 1):"));
        assert!(lines.iter().any(|line| line == "  pods-read (allow)"));
        assert!(lines.iter().any(|line| line == "    Actions: pods:read"));
        assert!(lines
            .iter()
            .any(|line| line == "    Resources: urn:brain:pod:*"));
        assert!(lines.iter().any(|line| line == "Permissions"));
        assert!(lines.iter().any(|line| line == "  pods:read"));
        assert!(lines.iter().any(|line| line == "    Excluded resources: none"));
        assert!(lines.iter().any(|line| line == "Links"));
        assert!(lines.iter().any(|line| line == "  Pods: /v1/pods"));
    }

    #[test]
    fn renders_cluster_list() {
        let lines = render_cluster_list(&json!([
            {
                "id": "cluster-1",
                "provider": "hetzner",
                "region": "fsn1",
                "architectures": ["amd64", "arm64"]
            }
        ]));

        assert!(lines.iter().any(|line| line.contains("ID")));
        assert!(lines.iter().any(|line| line.contains("cluster-1")));
        assert!(lines.iter().any(|line| line.contains("hetzner")));
        assert!(lines.iter().any(|line| line.contains("amd64, arm64")));
    }

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
    fn writes_login_result_as_ndjson() {
        let mut output = Vec::new();

        write_login_json(&mut output, &json!({"email": "user@example.com"})).unwrap();

        assert_eq!(
            String::from_utf8(output).unwrap(),
            "{\"event\":\"authenticated\",\"user\":{\"email\":\"user@example.com\"}}\n"
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

    #[test]
    fn renders_image_list_with_pagination() {
        let lines = render_image_list(&json!({
            "items": [{
                "repository": "api",
                "tag": "latest",
                "namespace": "my-pod",
                "visibility": "pod",
                "architectures": ["amd64", "arm64", "ppc64le"],
                "digest": "sha256:abc",
                "reference": "registry.example/my-pod/api@sha256:abc",
                "createdAt": "2026-08-04T12:00:00Z"
            }],
            "total": 1,
            "_links": {"next": {"href": "/v1/pods/my-pod/images?offset=1"}}
        }));

        assert!(lines.iter().any(|line| line == "Total: 1"));
        assert!(lines.iter().any(|line| line.contains("api")));
        assert!(lines.iter().any(|line| line.contains("amd64, arm64, ...")));
        assert!(!lines.iter().any(|line| line.contains("VISIBILITY")));
        assert!(!lines.iter().any(|line| line.contains("REFERENCE")));
        assert!(!lines
            .iter()
            .any(|line| line.contains("registry.example/my-pod/api@sha256:abc")));
        assert!(lines.iter().any(|line| line.starts_with("Next: ")));
    }

    #[test]
    fn renders_image_inspect_variants() {
        let lines = render_image_inspect(&json!({
            "repository": "ubuntu",
            "tag": "latest",
            "namespace": "public",
            "visibility": "public",
            "reference": "registry.example/ubuntu:latest",
            "variants": [{
                "architecture": "amd64",
                "digest": "sha256:abc",
                "reference": "registry.example/ubuntu@sha256:abc",
                "uid": null,
                "gid": null,
                "exposedPorts": ["80/tcp"],
                "createdAt": "2026-08-04T12:00:00Z",
                "updatedAt": null
            }]
        }));

        assert!(lines.iter().any(|line| line == "Variants"));
        assert!(lines.iter().any(|line| line.contains("amd64")));
        assert!(lines.iter().any(|line| line.contains("80/tcp")));
    }
}
