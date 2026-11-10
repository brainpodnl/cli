use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

use super::catalog::{Catalog, Normalize, Reader, Resolver, workspace_patterns};
use super::{Diagnostic, Node, Workspace, relative_file};

pub(super) type Documents = HashMap<PathBuf, BTreeMap<String, Value>>;

pub fn parse(nodes: &mut Vec<Node>, catalog: &Catalog, diagnostics: &mut Vec<Diagnostic>) {
    let mut documents = Documents::new();
    let mut pnpm = HashMap::<PathBuf, Option<Pnpm>>::new();
    for node in nodes.iter() {
        if node
            .files
            .iter()
            .any(|file| catalog.reader(file, Reader::Pnpm))
        {
            pnpm.insert(node.path.clone(), None);
        }
        for (file, content) in &node.contents {
            let manifests = catalog.matching(file).collect::<Vec<_>>();
            let Some(manifest) = manifests.first() else {
                continue;
            };
            if manifest.reader == Reader::Pnpm {
                match serde_saphyr::from_str::<PnpmDocument>(content) {
                    Ok(manifest) => {
                        let (members, exclude) = workspace_patterns(
                            manifest.packages.unwrap_or_else(|| vec!["**".to_owned()]),
                        );
                        pnpm.insert(
                            node.path.clone(),
                            Some(Pnpm {
                                workspace: Workspace {
                                    ecosystem: "node".to_owned(),
                                    source: relative_file(&node.path, file),
                                    members,
                                    exclude,
                                },
                                catalog: manifest.catalog.unwrap_or_default(),
                                catalogs: manifest.catalogs.unwrap_or_default(),
                            }),
                        );
                    }
                    Err(error) => diagnostics.push(Diagnostic {
                        path: relative_file(&node.path, file),
                        message: error.to_string(),
                    }),
                }
                continue;
            }
            let parsed = match manifest.reader {
                Reader::Json => {
                    Some(serde_json::from_str::<Value>(content).map_err(|e| e.to_string()))
                }
                Reader::Toml => Some(toml::from_str::<Value>(content).map_err(|e| e.to_string())),
                _ => None,
            };
            if let Some(parsed) = parsed {
                match parsed {
                    Ok(doc) if doc.is_object() => {
                        documents
                            .entry(node.path.clone())
                            .or_default()
                            .insert(file.clone(), doc);
                    }
                    Ok(_) => diagnostics.push(Diagnostic {
                        path: relative_file(&node.path, file),
                        message: "manifest must be an object".to_owned(),
                    }),
                    Err(message) => diagnostics.push(Diagnostic {
                        path: relative_file(&node.path, file),
                        message,
                    }),
                }
            }
        }
    }
    for node in nodes.iter_mut() {
        if let Some(Some(manifest)) = pnpm.get(&node.path) {
            node.workspaces.push(manifest.workspace.clone());
        }
        for (file, doc) in documents.get(&node.path).into_iter().flatten() {
            for manifest in catalog.matching(file) {
                manifest.apply(node, file, doc);
            }
        }
    }
    super::dotnet::parse(nodes, catalog, &documents, diagnostics);
    super::java::parse(nodes, catalog, diagnostics);
    for node in nodes {
        for (file, doc) in documents.get(&node.path).into_iter().flatten() {
            if catalog
                .matching(file)
                .any(|manifest| manifest.resolver == Some(Resolver::Cargo))
            {
                cargo(node, doc, &documents);
            }
        }
        let contents = std::mem::take(&mut node.contents);
        for (file, text) in &contents {
            for manifest in catalog.matching(file) {
                match manifest.reader {
                    Reader::Go => go(node, file, text, manifest.workspace.as_deref()),
                    Reader::Gemfile => ruby(node, file, text, diagnostics),
                    Reader::Gemspec => gemspec(node, file, text),
                    Reader::Pin | Reader::Properties | Reader::Tools => {
                        manifest.text_versions(node, file, text)
                    }
                    Reader::Requirements => {
                        for line in logical_lines(text) {
                            let line = line.trim();
                            if line.starts_with('-') {
                                if !line.starts_with("--hash=") {
                                    diagnostics.push(Diagnostic {
                                        path: relative_file(&node.path, file),
                                        message: format!(
                                            "requirements option not evaluated: {line}"
                                        ),
                                    });
                                }
                            } else {
                                requirement(node, line, file);
                            }
                        }
                    }
                    _ => (),
                }
            }
        }
        resolve_catalogs(node, &pnpm, diagnostics);
        for (file, doc) in documents.get(&node.path).into_iter().flatten() {
            for manifest in catalog.matching(file) {
                manifest.versions(node, file, Some(doc), None, true);
            }
        }
    }
}

#[derive(Deserialize)]
struct PnpmDocument {
    packages: Option<Vec<String>>,
    catalog: Option<BTreeMap<String, String>>,
    catalogs: Option<BTreeMap<String, BTreeMap<String, String>>>,
}

struct Pnpm {
    workspace: Workspace,
    catalog: BTreeMap<String, String>,
    catalogs: BTreeMap<String, BTreeMap<String, String>>,
}

fn resolve_catalogs(
    node: &mut Node,
    manifests: &HashMap<PathBuf, Option<Pnpm>>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let workspace = node
        .path
        .ancestors()
        .find_map(|path| manifests.get(path).map(|manifest| (path, manifest)))
        .and_then(|(path, manifest)| manifest.as_ref().map(|manifest| (path, manifest)))
        .filter(|(path, manifest)| {
            *path == node.path
                || node.path.strip_prefix(path).ok().is_some_and(|relative| {
                    manifest.workspace.contains(&super::display_path(relative))
                })
        })
        .map(|(_, manifest)| manifest);
    for (key, dependency) in &mut node.dependencies {
        let Some(package) = key.strip_prefix("npm:") else {
            continue;
        };
        let Some(reference) = dependency
            .version
            .as_deref()
            .and_then(|v| v.strip_prefix("catalog:"))
        else {
            continue;
        };
        let value = workspace.and_then(|manifest| {
            let catalog = if reference.is_empty() || reference == "default" {
                Some(&manifest.catalog)
            } else {
                manifest.catalogs.get(reference)
            };
            catalog
                .and_then(|catalog| catalog.get(package))
                .filter(|v| !v.starts_with("catalog:"))
                .map(|value| super::Version {
                    value: value.clone(),
                    source: manifest.workspace.source.clone(),
                    kind: "requirement".to_owned(),
                })
        });
        if let Some(resolved) = value {
            dependency.resolved = Some(resolved);
        } else {
            diagnostics.push(Diagnostic {
                path: dependency.source.clone(),
                message: format!("could not resolve catalog:{reference} for {package}"),
            });
        }
    }
}

fn cargo(node: &mut Node, doc: &Value, documents: &Documents) {
    let file = "Cargo.toml";
    let workspace =
        if let Some(explicit) = doc.pointer("/package/workspace").and_then(Value::as_str) {
            normalize_path(&node.path.join(explicit)).and_then(|path| {
                documents
                    .get(&path)
                    .and_then(|docs| docs.get(file))
                    .filter(|d| d.get("workspace").is_some())
                    .map(|d| (path, d))
            })
        } else {
            node.path.ancestors().find_map(|path| {
                documents
                    .get(path)
                    .and_then(|docs| docs.get(file))
                    .filter(|d| d.get("workspace").is_some())
                    .map(|d| (path.to_path_buf(), d))
            })
        };
    if let Some(name) = doc.pointer("/package/name").and_then(Value::as_str)
        && doc
            .pointer("/package/version/workspace")
            .and_then(Value::as_bool)
            == Some(true)
        && let Some((path, workspace)) = &workspace
        && let Some(version) = workspace
            .pointer("/workspace/package/version")
            .and_then(Value::as_str)
        && let Some(identity) = node.identities.get_mut(&format!("cargo:{name}"))
    {
        identity.resolved = Some(super::Version {
            value: version.to_owned(),
            source: relative_file(path, file),
            kind: "package".to_owned(),
        });
    }
    if doc
        .pointer("/package/rust-version/workspace")
        .and_then(Value::as_bool)
        == Some(true)
        && let Some((path, ws)) = &workspace
        && let Some(version) = ws
            .pointer("/workspace/package/rust-version")
            .and_then(Value::as_str)
    {
        node.versions
            .entry("rust".to_owned())
            .or_default()
            .push(super::Version {
                value: version.to_owned(),
                source: relative_file(path, file),
                kind: "requirement".to_owned(),
            });
    }
    let mut dep_tables = Vec::new();
    if let Some(d) = doc.get("dependencies") {
        dep_tables.push(d);
    }
    if let Some(targets) = doc.get("target").and_then(Value::as_object) {
        dep_tables.extend(targets.values().filter_map(|v| v.get("dependencies")));
    }
    for table in dep_tables {
        if let Some(deps) = table.as_object() {
            for (alias, declaration) in deps {
                let inherited = declaration.get("workspace").and_then(Value::as_bool) == Some(true);
                let (value, source) = if inherited {
                    let Some((path, ws)) = &workspace else {
                        continue;
                    };
                    let Some(value) = ws
                        .pointer("/workspace/dependencies")
                        .and_then(|d| d.get(alias))
                    else {
                        continue;
                    };
                    (value, relative_file(path, file))
                } else {
                    (declaration, relative_file(&node.path, file))
                };
                let name = value
                    .get("package")
                    .and_then(Value::as_str)
                    .unwrap_or(alias);
                let version = value
                    .as_str()
                    .or_else(|| value.get("version").and_then(Value::as_str))
                    .map(str::to_owned);
                node.dependencies.insert(
                    format!("cargo:{name}"),
                    super::Dependency {
                        version,
                        source,
                        resolved: None,
                    },
                );
            }
        }
    }
}

pub(super) fn requirement_parts(line: &str) -> Option<(&str, Option<&str>)> {
    let line = line.split('#').next().unwrap_or("").trim();
    if line.is_empty() {
        return None;
    }
    let end = line
        .find(|c: char| !c.is_ascii_alphanumeric() && !matches!(c, '-' | '_' | '.'))
        .unwrap_or(line.len());
    if end == 0 {
        return None;
    }
    let name = &line[..end];
    let mut rest = line[end..].trim();
    if rest.starts_with('[') {
        let end = rest.find(']')?;
        rest = rest[end + 1..].trim();
    }
    let constraint = rest.split(';').next().unwrap_or("").trim();
    let constraint = constraint.split(" --hash=").next().unwrap_or("").trim();
    let version = if constraint.is_empty() || constraint.starts_with('@') {
        None
    } else {
        Some(constraint)
    };
    Some((name, version))
}

fn requirement(node: &mut Node, line: &str, file: &str) {
    if let Some((name, version)) = requirement_parts(line) {
        node.dependency("pypi", &Normalize::Python.name(name), version, file);
    }
}

fn logical_lines(text: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    for line in text.lines() {
        let line = line.trim_end();
        if let Some(part) = line.strip_suffix('\\') {
            current.push_str(part);
            current.push(' ');
        } else {
            current.push_str(line);
            lines.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

fn go(node: &mut Node, file: &str, text: &str, workspace: Option<&str>) {
    let mut block = "";
    let mut members = Vec::new();
    for line in text.lines() {
        if line
            .split_once("//")
            .is_some_and(|(_, comment)| comment.trim() == "indirect")
        {
            continue;
        }
        let line = line.split("//").next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if line == ")" {
            block = "";
            continue;
        }
        let words = line.split_whitespace().collect::<Vec<_>>();
        let (directive, values) = if block.is_empty() {
            (words[0], &words[1..])
        } else {
            (block, words.as_slice())
        };
        if values == ["("] {
            block = directive;
            continue;
        }
        match directive {
            "go" => {
                if let Some(v) = values.first() {
                    node.version("go", v, file, "requirement");
                }
            }
            "toolchain" => {
                if let Some(v) = values.first() {
                    node.version("go", v.trim_start_matches("go"), file, "toolchain");
                }
            }
            "module" => {
                if let Some(name) = values.first() {
                    node.names.insert(name.trim_matches('"').to_owned());
                    node.identity("go", name.trim_matches('"'), None, file);
                }
            }
            "require" => {
                if let Some(name) = values.first() {
                    node.dependency("go", name, values.get(1).copied(), file);
                }
            }
            "use" => {
                if let Some(path) = values.first() {
                    members.push(path.trim_matches('"').trim_start_matches("./").to_owned());
                }
            }
            _ => (),
        }
    }
    if let Some(ecosystem) = workspace {
        node.workspaces.push(Workspace {
            ecosystem: ecosystem.to_owned(),
            source: relative_file(&node.path, file),
            members,
            exclude: Vec::new(),
        });
    }
}

fn quoted(input: &str) -> Option<(&str, &str)> {
    let input = input.trim_start();
    let quote = input.as_bytes().first().copied()?;
    if quote != b'\'' && quote != b'"' {
        return None;
    }
    let end = input[1..].find(char::from(quote))? + 1;
    let value = &input[1..end];
    if value.contains("#{") || value.contains('\\') {
        return None;
    }
    Some((value, &input[end + 1..]))
}

fn ruby(node: &mut Node, file: &str, text: &str, diagnostics: &mut Vec<Diagnostic>) {
    for line in text.lines() {
        let line = line.trim();
        let (directive, rest) = if let Some(rest) = line
            .strip_prefix("gem ")
            .or_else(|| line.strip_prefix("gem("))
            .or_else(|| line.strip_prefix("gem\t"))
        {
            ("gem", rest)
        } else if let Some(rest) = line
            .strip_prefix("ruby ")
            .or_else(|| line.strip_prefix("ruby("))
            .or_else(|| line.strip_prefix("ruby\t"))
        {
            ("ruby", rest)
        } else {
            continue;
        };
        let Some((name, rest)) = quoted(rest) else {
            diagnostics.push(Diagnostic {
                path: relative_file(&node.path, file),
                message: format!("dynamic {directive} declaration not evaluated"),
            });
            continue;
        };
        if directive == "ruby" {
            node.version("ruby", name, file, "requirement");
        } else {
            let mut versions = Vec::new();
            let mut rest = rest.trim_start();
            while let Some(next) = rest.strip_prefix(',') {
                let Some((version, tail)) = quoted(next) else {
                    break;
                };
                versions.push(version);
                rest = tail.trim_start();
            }
            let version = versions.join(", ");
            node.dependency(
                "gem",
                name,
                (!version.is_empty()).then_some(version.as_str()),
                file,
            );
        }
    }
}

fn gemspec(node: &mut Node, file: &str, text: &str) {
    let mut name = None;
    let mut version = None;
    for line in text.lines() {
        let Some((receiver, assignment)) = line.trim().split_once('.') else {
            continue;
        };
        if receiver.is_empty()
            || !receiver
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            continue;
        }
        let Some((field, value)) = assignment.split_once('=') else {
            continue;
        };
        let Some((value, _)) = quoted(value) else {
            continue;
        };
        match field.trim() {
            "name" => name = Some(value),
            "version" => version = Some(value),
            "required_ruby_version" => node.version("ruby", value, file, "requirement"),
            _ => (),
        }
    }
    if let Some(name) = name {
        node.names.insert(name.to_owned());
        node.identity("gem", name, version, file);
    }
}

fn normalize_path(path: &Path) -> Option<PathBuf> {
    if path.is_absolute() {
        return None;
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                if !normalized.pop() {
                    return None;
                }
            }
            std::path::Component::CurDir => (),
            other => normalized.push(other),
        }
    }
    Some(normalized)
}
