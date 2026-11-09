use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use serde_json::Value;

use super::values::{self, Located, Properties, Syntax};
use super::{Diagnostic, Node, Version, Workspace, display_path, relative_file, xml};

#[derive(Default)]
struct Document {
    properties: Vec<(String, Located, Vec<String>)>,
    steps: Vec<Step>,
    references: Vec<Reference>,
    imports: Vec<(String, Vec<String>)>,
    projects: Vec<String>,
    sdk: Vec<(String, Option<String>)>,
    solution: bool,
}

enum Step {
    Property(usize),
    Import(usize),
}

struct Reference {
    kind: String,
    name: String,
    version: Option<Located>,
}

fn document(text: &str, source: &str) -> Result<Document> {
    let mut doc = Document::default();
    let mut nested_versions = BTreeMap::new();
    xml::read(text, |stack| {
        let Some(element) = stack.last() else {
            return Ok(());
        };
        let root = stack.first().map(|e| e.name);
        if root == Some("Solution") {
            doc.solution = true;
            if element.name == "Project"
                && let Some(path) = element.attributes.get("Path")
            {
                doc.projects.push(path.clone());
            }
            return Ok(());
        }
        if root != Some("Project") {
            bail!("expected an MSBuild Project or Solution root");
        }
        let conditions = stack
            .iter()
            .filter_map(|e| {
                e.attributes
                    .get("Condition")
                    .filter(|v| !v.trim().is_empty())
                    .cloned()
            })
            .collect::<Vec<_>>();
        let conditional = !conditions.is_empty();
        let located = |value: &str| Located {
            value: value.trim().to_owned(),
            source: source.to_owned(),
            conditional,
        };
        let parent = stack.get(stack.len().saturating_sub(2));
        if parent.is_some_and(|p| p.name == "PropertyGroup") && !element.text.trim().is_empty() {
            doc.steps.push(Step::Property(doc.properties.len()));
            doc.properties.push((
                element.name.to_ascii_lowercase(),
                located(&element.text),
                conditions.clone(),
            ));
        }
        if element.name == "Version"
            && let Some(parent) = parent
        {
            nested_versions.insert(
                (stack.len() - 1, parent.name.to_owned()),
                located(&element.text),
            );
        }
        if matches!(
            element.name,
            "PackageReference" | "PackageVersion" | "FrameworkReference" | "Reference"
        ) && let Some(name) = element
            .attributes
            .get("Include")
            .or_else(|| element.attributes.get("Update"))
        {
            let version = element
                .attributes
                .get("VersionOverride")
                .or_else(|| element.attributes.get("Version"))
                .map(|v| located(v))
                .or_else(|| nested_versions.remove(&(stack.len(), element.name.to_owned())));
            doc.references.push(Reference {
                kind: element.name.to_owned(),
                name: name.clone(),
                version,
            });
        }
        if element.name == "ProjectReference"
            && let Some(path) = element.attributes.get("Include")
        {
            doc.projects.push(path.clone());
        }
        if element.name == "Import" {
            if let Some(sdk) = element.attributes.get("Sdk") {
                doc.sdk
                    .push((sdk.clone(), element.attributes.get("Version").cloned()));
            } else if let Some(path) = element.attributes.get("Project") {
                doc.steps.push(Step::Import(doc.imports.len()));
                doc.imports.push((path.clone(), conditions.clone()));
            }
        }
        if element.name == "Sdk"
            && let Some(name) = element.attributes.get("Name")
        {
            doc.sdk
                .push((name.clone(), element.attributes.get("Version").cloned()));
        }
        if stack.len() == 1
            && let Some(sdk) = element.attributes.get("Sdk")
        {
            for sdk in sdk.split(';') {
                let (name, version) = sdk
                    .trim()
                    .split_once('/')
                    .map_or((sdk.trim(), None), |(name, version)| {
                        (name, Some(version.to_owned()))
                    });
                if !name.is_empty() {
                    doc.sdk.push((name.to_owned(), version));
                }
            }
        }
        Ok(())
    })?;
    Ok(doc)
}

fn nearest(
    path: &Path,
    file: &str,
    documents: &BTreeMap<PathBuf, Option<Document>>,
) -> Option<PathBuf> {
    path.ancestors()
        .map(|p| p.join(file))
        .find(|p| documents.contains_key(p))
}

fn split_condition<'a>(text: &'a str, operator: &str) -> Option<(&'a str, &'a str)> {
    let upper = text.to_ascii_uppercase();
    let mut depth = 0;
    let mut quote = None;
    for (index, ch) in text.char_indices() {
        if quote == Some(ch) {
            quote = None;
            continue;
        }
        if quote.is_some() {
            continue;
        }
        if ch == '\'' || ch == '"' {
            quote = Some(ch);
            continue;
        }
        if ch == '(' {
            depth += 1;
        } else if ch == ')' {
            depth -= 1;
        }
        if depth == 0 && upper[index..].starts_with(operator) {
            return Some((&text[..index], &text[index + operator.len()..]));
        }
    }
    None
}

fn condition(text: &str, properties: &Properties, depth: usize) -> Option<bool> {
    if depth == 64 {
        return None;
    }
    let text = text.trim();
    if text.is_empty() {
        return Some(true);
    }
    for (operator, or) in [(" OR ", true), (" AND ", false)] {
        if let Some((left, right)) = split_condition(text, operator) {
            let a = condition(left, properties, depth + 1);
            let b = condition(right, properties, depth + 1);
            return if or {
                if a == Some(true) || b == Some(true) {
                    Some(true)
                } else if a == Some(false) && b == Some(false) {
                    Some(false)
                } else {
                    None
                }
            } else if a == Some(false) || b == Some(false) {
                Some(false)
            } else if a == Some(true) && b == Some(true) {
                Some(true)
            } else {
                None
            };
        }
    }
    if text.starts_with('(') && text.ends_with(')') {
        return condition(&text[1..text.len() - 1], properties, depth + 1);
    }
    let mut expanded = text.to_owned();
    for _ in 0..64 {
        let Some(start) = expanded.find("$(") else {
            break;
        };
        let end = expanded[start + 2..].find(')')? + start + 2;
        let name = &expanded[start + 2..end];
        if !name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.'))
        {
            return None;
        }
        let key = name.to_ascii_lowercase();
        let value = if let Some(value) = properties.get(&key) {
            value.value.as_str()
        } else if key.starts_with("msbuild")
            || matches!(
                key.as_str(),
                "configuration"
                    | "platform"
                    | "platformtarget"
                    | "runtimeidentifier"
                    | "os"
                    | "designtimebuild"
                    | "buildinginsidevisualstudio"
            )
        {
            return None;
        } else {
            ""
        };
        if value.contains("$(") {
            return None;
        }
        expanded.replace_range(start..end + 1, value);
    }
    for operator in ["==", "!="] {
        if let Some((left, right)) = split_condition(&expanded, operator) {
            let literal = |value: &str| -> Option<String> {
                let value = value.trim();
                if value.len() >= 2
                    && ((value.starts_with('\'') && value.ends_with('\''))
                        || (value.starts_with('"') && value.ends_with('"')))
                {
                    return Some(value[1..value.len() - 1].to_owned());
                }
                None
            };
            let equal = literal(left)?.eq_ignore_ascii_case(&literal(right)?);
            return Some(if operator == "==" { equal } else { !equal });
        }
    }
    match expanded.trim().to_ascii_lowercase().as_str() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

fn conditions(values: &[String], properties: &Properties) -> Option<bool> {
    let mut unknown = false;
    for value in values {
        match condition(value, properties, 0) {
            Some(false) => return Some(false),
            Some(true) => (),
            None => unknown = true,
        }
    }
    if unknown { None } else { Some(true) }
}

fn apply(
    path: &Path,
    documents: &BTreeMap<PathBuf, Option<Document>>,
    properties: &mut Properties,
    active: &mut BTreeSet<PathBuf>,
    sources: &mut Vec<PathBuf>,
    conditional: &mut Vec<(String, Located)>,
) {
    let Some(Some(doc)) = documents.get(path) else {
        return;
    };
    if active.len() == 64 || !active.insert(path.to_path_buf()) {
        return;
    }
    if !sources.iter().any(|p| p == path) {
        sources.push(path.to_path_buf());
    }
    let directory = path.parent().unwrap_or(Path::new(""));
    let this_directory = Located {
        value: format!("{}/", display_path(directory)),
        source: display_path(path),
        conditional: false,
    };
    let previous = properties.insert("msbuildthisfiledirectory".to_owned(), this_directory);
    for step in &doc.steps {
        match step {
            Step::Property(index) => {
                let (name, value, tests) = &doc.properties[*index];
                match conditions(tests, properties) {
                    Some(true) => {
                        let mut value = value.clone();
                        value.conditional = false;
                        let value =
                            values::resolve(&value, properties, Syntax::MsBuild).unwrap_or(value);
                        properties.insert(name.clone(), value);
                    }
                    None => conditional.push((name.clone(), value.clone())),
                    Some(false) => (),
                }
            }
            Step::Import(index) => {
                let (import, tests) = &doc.imports[*index];
                if conditions(tests, properties) != Some(true) {
                    continue;
                }
                let raw = Located {
                    value: import.clone(),
                    source: display_path(path),
                    conditional: false,
                };
                if let Some(resolved) = values::resolve(&raw, properties, Syntax::MsBuild)
                    && let Some(target) = values::child_file(directory, &resolved.value)
                {
                    apply(&target, documents, properties, active, sources, conditional);
                }
            }
        }
    }
    if let Some(previous) = previous {
        properties.insert("msbuildthisfiledirectory".to_owned(), previous);
    } else {
        properties.remove("msbuildthisfiledirectory");
    }
    active.remove(path);
}

pub(super) fn parse(nodes: &mut [Node], diagnostics: &mut Vec<Diagnostic>) {
    if !nodes.iter().any(|node| {
        node.files.iter().any(|file| {
            file == "global.json"
                || file.ends_with(".csproj")
                || file.ends_with(".sln")
                || file.ends_with(".slnx")
        })
    }) {
        return;
    }
    let mut documents = BTreeMap::<PathBuf, Option<Document>>::new();
    let mut sdk_versions = BTreeMap::<PathBuf, BTreeMap<String, Located>>::new();
    for node in nodes.iter_mut() {
        for file in &node.files {
            if file == "global.json" {
                sdk_versions.insert(node.path.clone(), BTreeMap::new());
            }
            if file.ends_with(".csproj")
                || file.ends_with(".slnx")
                || file.ends_with(".props")
                || file.ends_with(".targets")
            {
                documents.insert(node.path.join(file), None);
            }
        }
        for (file, text) in &node.contents {
            let source = relative_file(&node.path, file);
            if file == "global.json" {
                sdk_versions.insert(node.path.clone(), BTreeMap::new());
                match serde_json::from_str::<Value>(text) {
                    Ok(doc) => {
                        if let Some(version) = doc.pointer("/sdk/version").and_then(Value::as_str) {
                            node.versions
                                .entry("csharp".to_owned())
                                .or_default()
                                .push(Version {
                                    value: version.to_owned(),
                                    source: source.clone(),
                                    kind: "sdk".to_owned(),
                                });
                        }
                        if let Some(versions) = doc.get("msbuild-sdks").and_then(Value::as_object) {
                            sdk_versions.insert(
                                node.path.clone(),
                                versions
                                    .iter()
                                    .filter_map(|(name, version)| {
                                        version.as_str().map(|value| {
                                            (
                                                name.to_ascii_lowercase(),
                                                Located {
                                                    value: value.to_owned(),
                                                    source: source.clone(),
                                                    conditional: false,
                                                },
                                            )
                                        })
                                    })
                                    .collect(),
                            );
                        }
                    }
                    Err(error) => diagnostics.push(Diagnostic {
                        path: source,
                        message: error.to_string(),
                    }),
                }
            } else if let std::collections::btree_map::Entry::Occupied(mut entry) =
                documents.entry(node.path.join(file))
            {
                match document(text, &source) {
                    Ok(doc) if file.ends_with(".slnx") == doc.solution => {
                        entry.insert(Some(doc));
                    }
                    Ok(_) => diagnostics.push(Diagnostic {
                        path: source,
                        message: "XML root does not match the MSBuild manifest type".to_owned(),
                    }),
                    Err(error) => diagnostics.push(Diagnostic {
                        path: source,
                        message: error.to_string(),
                    }),
                }
            }
        }
    }
    for node in nodes {
        for file in node
            .files
            .iter()
            .filter(|file| file.ends_with(".sln") || file.ends_with(".slnx"))
            .cloned()
            .collect::<Vec<_>>()
        {
            let projects = if file.ends_with(".slnx") {
                documents
                    .get(&node.path.join(&file))
                    .and_then(Option::as_ref)
                    .map(|d| d.projects.clone())
                    .unwrap_or_default()
            } else {
                node.contents
                    .get(&file)
                    .into_iter()
                    .flat_map(|text| text.lines())
                    .filter(|line| line.trim_start().starts_with("Project("))
                    .filter_map(|line| line.split_once('=')?.1.split('"').nth(3))
                    .map(str::to_owned)
                    .collect()
            };
            let members = projects
                .iter()
                .filter(|p| p.to_ascii_lowercase().ends_with(".csproj"))
                .filter_map(|p| values::child_file(&node.path, p))
                .filter_map(|p| p.parent().map(Path::to_path_buf))
                .filter_map(|p| p.strip_prefix(&node.path).ok().map(display_path))
                .collect::<Vec<_>>();
            if !members.is_empty() {
                node.names.insert(
                    Path::new(&file)
                        .file_stem()
                        .map_or_else(String::new, |s| s.to_string_lossy().into_owned()),
                );
                node.workspaces.push(Workspace {
                    ecosystem: "dotnet".to_owned(),
                    source: relative_file(&node.path, &file),
                    members,
                    exclude: Vec::new(),
                });
            }
        }
        let projects = node
            .files
            .iter()
            .filter(|file| file.ends_with(".csproj"))
            .cloned()
            .collect::<Vec<_>>();
        for file in projects {
            let path = node.path.join(&file);
            let source = display_path(&path);
            let Some(Some(_)) = documents.get(&path) else {
                continue;
            };
            let name = Path::new(&file)
                .file_stem()
                .map_or_else(String::new, |s| s.to_string_lossy().into_owned());
            node.packages.insert("dotnet".to_owned(), source.clone());
            node.names.insert(name.clone());
            let mut properties = Properties::new();
            for (key, value) in [
                ("msbuildprojectname", name.as_str()),
                ("assemblyname", name.as_str()),
            ] {
                properties.insert(
                    key.to_owned(),
                    Located {
                        value: value.to_owned(),
                        source: source.clone(),
                        conditional: false,
                    },
                );
            }
            let mut sources = Vec::new();
            let mut conditional = Vec::new();
            properties.insert(
                "msbuildprojectextension".to_owned(),
                Located {
                    value: ".csproj".to_owned(),
                    source: source.clone(),
                    conditional: false,
                },
            );
            if let Some(path) = nearest(&node.path, "Directory.Build.props", &documents) {
                apply(
                    &path,
                    &documents,
                    &mut properties,
                    &mut BTreeSet::new(),
                    &mut sources,
                    &mut conditional,
                );
            }
            apply(
                &path,
                &documents,
                &mut properties,
                &mut BTreeSet::new(),
                &mut sources,
                &mut conditional,
            );
            if let Some(path) = nearest(&node.path, "Directory.Build.targets", &documents) {
                apply(
                    &path,
                    &documents,
                    &mut properties,
                    &mut BTreeSet::new(),
                    &mut sources,
                    &mut conditional,
                );
            }
            let mut central = BTreeMap::new();
            if let Some(path) = nearest(&node.path, "Directory.Packages.props", &documents)
                && !properties
                    .get("managepackageversionscentrally")
                    .is_some_and(|value| value.value.eq_ignore_ascii_case("false"))
            {
                let mut central_sources = Vec::new();
                apply(
                    &path,
                    &documents,
                    &mut properties,
                    &mut BTreeSet::new(),
                    &mut central_sources,
                    &mut conditional,
                );
                if properties
                    .get("managepackageversionscentrally")
                    .is_some_and(|value| value.value.eq_ignore_ascii_case("false"))
                {
                    central_sources.clear();
                }
                for path in central_sources {
                    if let Some(Some(doc)) = documents.get(&path) {
                        for reference in &doc.references {
                            if reference.kind == "PackageVersion" {
                                central.insert(
                                    reference.name.to_ascii_lowercase(),
                                    reference.version.as_ref(),
                                );
                            }
                        }
                    }
                }
            }
            for (key, kind) in [
                ("targetframework", "target"),
                ("targetframeworks", "target"),
                ("targetframeworkversion", "target"),
                ("langversion", "language"),
                ("runtimeframeworkversion", "runtime"),
            ] {
                if let Some(raw) = properties.get(key) {
                    for value in raw.value.split(';').filter(|v| !v.trim().is_empty()) {
                        values::version(
                            node,
                            "csharp",
                            &Located {
                                value: value.trim().to_owned(),
                                ..raw.clone()
                            },
                            kind,
                            &properties,
                            Syntax::MsBuild,
                        );
                    }
                }
            }
            for (key, raw) in &conditional {
                let kind = match key.as_str() {
                    "targetframework" | "targetframeworks" | "targetframeworkversion" => "target",
                    "langversion" => "language",
                    "runtimeframeworkversion" => "runtime",
                    _ => continue,
                };
                values::version(node, "csharp", raw, kind, &properties, Syntax::MsBuild);
            }
            for location in &sources {
                let Some(Some(document)) = documents.get(location) else {
                    continue;
                };
                for (sdk, version) in &document.sdk {
                    let raw = Located {
                        value: sdk.clone(),
                        source: display_path(location),
                        conditional: false,
                    };
                    let Some(sdk) = values::resolve(&raw, &properties, Syntax::MsBuild) else {
                        continue;
                    };
                    let sdk = sdk.value.to_ascii_lowercase();
                    let version = version.as_ref().map(|v| Located {
                        value: v.clone(),
                        source: raw.source.clone(),
                        conditional: false,
                    });
                    let inherited = node
                        .path
                        .ancestors()
                        .find_map(|p| sdk_versions.get(p))
                        .and_then(|v| v.get(&sdk));
                    values::dependency(
                        node,
                        "dotnet-sdk",
                        &sdk,
                        &raw.source,
                        version.as_ref().or(inherited),
                        &properties,
                        Syntax::MsBuild,
                    );
                }
                for reference in &document.references {
                    let ecosystem = match reference.kind.as_str() {
                        "PackageReference" => "nuget",
                        "FrameworkReference" => "dotnet-framework",
                        "Reference" => "dotnet-reference",
                        _ => continue,
                    };
                    let name = reference
                        .name
                        .split(',')
                        .next()
                        .unwrap_or("")
                        .trim()
                        .to_ascii_lowercase();
                    let version = reference
                        .version
                        .as_ref()
                        .or_else(|| central.get(&name).copied().flatten());
                    values::dependency(
                        node,
                        ecosystem,
                        &name,
                        &display_path(location),
                        version,
                        &properties,
                        Syntax::MsBuild,
                    );
                }
            }
            if let Some(raw) = properties
                .get("packageid")
                .or_else(|| properties.get("assemblyname"))
                && let Some(package) = values::resolve(raw, &properties, Syntax::MsBuild)
            {
                node.names.insert(package.value.clone());
                values::identity(
                    node,
                    "nuget",
                    &package.value.to_ascii_lowercase(),
                    &source,
                    properties.get("version"),
                    &properties,
                    Syntax::MsBuild,
                );
            }
        }
    }
}
