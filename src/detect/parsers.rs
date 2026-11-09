use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

use super::{Diagnostic, Node, Workspace, relative_file};

pub fn readable(name: &str) -> bool {
    matches!(
        name,
        "composer.json"
            | "package.json"
            | "pnpm-workspace.yaml"
            | "Cargo.toml"
            | "go.mod"
            | "go.work"
            | "Gemfile"
            | "pyproject.toml"
            | "Pipfile"
            | ".go-version"
            | ".php-version"
            | ".ruby-version"
            | ".python-version"
            | ".node-version"
            | ".nvmrc"
            | ".tool-versions"
            | "rust-toolchain"
            | "rust-toolchain.toml"
            | "global.json"
            | "pom.xml"
            | "gradle.properties"
            | "gradle-daemon-jvm.properties"
            | ".java-version"
            | ".sdkmanrc"
    ) || name.ends_with(".csproj")
        || name.ends_with(".sln")
        || name.ends_with(".slnx")
        || name.ends_with(".props")
        || name.ends_with(".targets")
        || name.ends_with(".gradle")
        || name.ends_with(".gradle.kts")
        || name.ends_with(".versions.toml")
        || name.ends_with(".gemspec")
        || (name.starts_with("requirements") && name.ends_with(".txt"))
}

pub fn parse(nodes: &mut Vec<Node>, diagnostics: &mut Vec<Diagnostic>) {
    let mut documents = Documents::new();
    let mut pnpm = HashMap::<PathBuf, Option<Pnpm>>::new();
    for node in nodes.iter() {
        if node.files.contains("pnpm-workspace.yaml") {
            pnpm.insert(node.path.clone(), None);
        }
        for (file, content) in &node.contents {
            if file == "pnpm-workspace.yaml" {
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
            let parsed = match file.as_str() {
                "composer.json" | "package.json" => {
                    Some(serde_json::from_str::<Value>(content).map_err(|e| e.to_string()))
                }
                "Cargo.toml" | "pyproject.toml" | "Pipfile" | "rust-toolchain.toml" => {
                    Some(toml::from_str::<Value>(content).map_err(|e| e.to_string()))
                }
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
    super::dotnet::parse(nodes, diagnostics);
    super::java::parse(nodes, diagnostics);
    for node in nodes {
        if let Some(Some(manifest)) = pnpm.get(&node.path) {
            node.workspaces.push(manifest.workspace.clone());
        }
        for (file, doc) in documents.get(&node.path).into_iter().flatten() {
            if let Some(name) = doc
                .get("name")
                .or_else(|| doc.pointer("/package/name"))
                .or_else(|| doc.pointer("/project/name"))
                .and_then(Value::as_str)
            {
                node.names.insert(name.to_owned());
            }
            match file.as_str() {
                "composer.json" => composer(node, doc),
                "package.json" => javascript(node, doc),
                "Cargo.toml" => cargo(node, doc, &documents),
                "pyproject.toml" => python(node, doc),
                "Pipfile" => {
                    dependencies(node, doc.get("packages"), "pypi", file);
                    if let Some(v) = doc
                        .pointer("/requires/python_full_version")
                        .or_else(|| doc.pointer("/requires/python_version"))
                        .and_then(Value::as_str)
                    {
                        node.version("python", v, file, "requirement");
                    }
                }
                _ => (),
            }
        }
        let contents = std::mem::take(&mut node.contents);
        for (file, text) in &contents {
            match file.as_str() {
                "go.mod" | "go.work" => go(node, file, text),
                "Gemfile" => ruby(node, text, diagnostics),
                f if f.ends_with(".gemspec") => gemspec(node, file, text),
                ".java-version" => pin(node, "java", file, text),
                ".sdkmanrc" => {
                    if let Some(value) =
                        super::values::properties(text, &relative_file(&node.path, file))
                            .get("java")
                    {
                        node.version("java", &value.value, file, "pin");
                    }
                }
                ".go-version" => pin(node, "go", file, text),
                ".php-version" => pin(node, "php", file, text),
                ".ruby-version" => pin(node, "ruby", file, text),
                ".python-version" => pin(node, "python", file, text),
                ".node-version" | ".nvmrc" => pin(node, "javascript", file, text),
                "rust-toolchain" => pin(node, "rust", file, text),
                "rust-toolchain.toml" => {
                    if let Some(v) = documents
                        .get(&node.path)
                        .and_then(|docs| docs.get(file))
                        .and_then(|d| d.pointer("/toolchain/channel"))
                        .and_then(Value::as_str)
                    {
                        node.version("rust", v, file, "toolchain");
                    }
                }
                ".tool-versions" => {
                    for line in text.lines() {
                        let mut words = line.split('#').next().unwrap_or("").split_whitespace();
                        let language = match words.next() {
                            Some("nodejs") => "javascript",
                            Some("golang") => "go",
                            Some("java") => "java",
                            Some("python") => "python",
                            Some("ruby") => "ruby",
                            Some("rust") => "rust",
                            Some("php") => "php",
                            _ => continue,
                        };
                        let value = words.collect::<Vec<_>>().join(" ");
                        node.version(language, &value, file, "pin");
                    }
                }
                f if f.starts_with("requirements") && f.ends_with(".txt") => {
                    for line in logical_lines(text) {
                        let line = line.trim();
                        if line.starts_with('-') {
                            if !line.starts_with("--hash=") {
                                diagnostics.push(Diagnostic {
                                    path: relative_file(&node.path, file),
                                    message: format!("requirements option not evaluated: {line}"),
                                });
                            }
                            continue;
                        }
                        requirement(node, line, file);
                    }
                }
                _ => (),
            }
        }
        resolve_catalogs(node, &pnpm, diagnostics);
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

fn workspace_patterns(patterns: Vec<String>) -> (Vec<String>, Vec<String>) {
    let (mut members, mut exclude) = (Vec::new(), Vec::new());
    for pattern in patterns {
        if let Some(pattern) = pattern.strip_prefix('!') {
            exclude.push(pattern.to_owned());
        } else {
            members.push(pattern);
        }
    }
    (members, exclude)
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
            if package == "typescript" {
                node.versions
                    .entry("typescript".to_owned())
                    .or_default()
                    .push(resolved.clone());
            }
            dependency.resolved = Some(resolved);
        } else {
            diagnostics.push(Diagnostic {
                path: dependency.source.clone(),
                message: format!("could not resolve catalog:{reference} for {package}"),
            });
        }
    }
}

fn dependencies(node: &mut Node, object: Option<&Value>, ecosystem: &str, file: &str) {
    if let Some(deps) = object.and_then(Value::as_object) {
        for (name, value) in deps {
            let name = if ecosystem == "pypi" {
                normalize_python(name)
            } else {
                name.clone()
            };
            let version = value
                .as_str()
                .or_else(|| value.get("version").and_then(Value::as_str));
            node.dependency(ecosystem, &name, version, file);
        }
    }
}

fn composer(node: &mut Node, doc: &Value) {
    if let Some(name) = doc.get("name").and_then(Value::as_str) {
        node.identity(
            "composer",
            name,
            doc.get("version").and_then(Value::as_str),
            "composer.json",
        );
    }
    dependencies(node, doc.get("require"), "composer", "composer.json");
    if let Some(v) = doc.pointer("/require/php").and_then(Value::as_str) {
        node.version("php", v, "composer.json", "requirement");
    }
    if let Some(v) = doc.pointer("/config/platform/php").and_then(Value::as_str) {
        node.version("php", v, "composer.json", "platform");
    }
}

fn javascript(node: &mut Node, doc: &Value) {
    if let Some(name) = doc.get("name").and_then(Value::as_str) {
        node.identity(
            "npm",
            name,
            doc.get("version").and_then(Value::as_str),
            "package.json",
        );
    }
    dependencies(node, doc.get("devDependencies"), "npm", "package.json");
    dependencies(node, doc.get("dependencies"), "npm", "package.json");
    if let Some(v) = doc.pointer("/engines/node").and_then(Value::as_str) {
        node.version("javascript", v, "package.json", "requirement");
    }
    if let Some(v) = doc.pointer("/volta/node").and_then(Value::as_str) {
        node.version("javascript", v, "package.json", "pin");
    }
    if let Some(v) = doc
        .pointer("/dependencies/typescript")
        .or_else(|| doc.pointer("/devDependencies/typescript"))
        .and_then(Value::as_str)
    {
        node.version(
            "typescript",
            v,
            "package.json",
            if v.starts_with("catalog:") {
                "reference"
            } else {
                "requirement"
            },
        );
    }
    if let Some(workspaces) = doc.get("workspaces") {
        let (members, exclude) =
            workspace_patterns(strings(workspaces.get("packages").unwrap_or(workspaces)));
        node.workspaces.push(Workspace {
            ecosystem: "node".to_owned(),
            source: relative_file(&node.path, "package.json"),
            members,
            exclude,
        });
    }
}

type Documents = HashMap<PathBuf, BTreeMap<String, Value>>;

fn cargo(node: &mut Node, doc: &Value, documents: &Documents) {
    let file = "Cargo.toml";
    if doc.get("package").is_some_and(Value::is_object) {
        node.packages
            .insert("cargo".to_owned(), relative_file(&node.path, file));
    }
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
    if let Some(name) = doc.pointer("/package/name").and_then(Value::as_str) {
        node.identity(
            "cargo",
            name,
            doc.pointer("/package/version").and_then(Value::as_str),
            file,
        );
        if doc
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
    }
    if let Some(version) = doc.pointer("/package/rust-version") {
        if let Some(v) = version.as_str() {
            node.version("rust", v, file, "requirement");
        } else if version.get("workspace").and_then(Value::as_bool) == Some(true)
            && let Some((path, ws)) = &workspace
            && let Some(v) = ws
                .pointer("/workspace/package/rust-version")
                .and_then(Value::as_str)
        {
            node.versions
                .entry("rust".to_owned())
                .or_default()
                .push(super::Version {
                    value: v.to_owned(),
                    source: relative_file(path, file),
                    kind: "requirement".to_owned(),
                });
        }
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
    if let Some(ws) = doc.get("workspace") {
        node.workspaces.push(Workspace {
            ecosystem: "cargo".to_owned(),
            source: relative_file(&node.path, file),
            members: strings(&ws["members"]),
            exclude: strings(&ws["exclude"]),
        });
    }
}

fn python(node: &mut Node, doc: &Value) {
    let file = "pyproject.toml";
    if let Some(name) = doc
        .pointer("/project/name")
        .or_else(|| doc.pointer("/tool/poetry/name"))
        .and_then(Value::as_str)
    {
        let version = doc
            .pointer("/project/version")
            .or_else(|| doc.pointer("/tool/poetry/version"))
            .and_then(Value::as_str);
        node.identity("pypi", &normalize_python(name), version, file);
        node.names.insert(name.to_owned());
    }
    if let Some(v) = doc
        .pointer("/project/requires-python")
        .and_then(Value::as_str)
    {
        node.version("python", v, file, "requirement");
    }
    for dep in doc
        .pointer("/project/dependencies")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        requirement(node, dep, file);
    }
    if let Some(deps) = doc
        .pointer("/tool/poetry/dependencies")
        .and_then(Value::as_object)
    {
        for (name, v) in deps {
            let version = v
                .as_str()
                .or_else(|| v.get("version").and_then(Value::as_str));
            if name == "python" {
                if let Some(v) = version {
                    node.version("python", v, file, "requirement");
                }
            } else {
                node.dependency("pypi", &normalize_python(name), version, file);
            }
        }
    }
    if let Some(ws) = doc.pointer("/tool/uv/workspace") {
        node.workspaces.push(Workspace {
            ecosystem: "python".to_owned(),
            source: relative_file(&node.path, file),
            members: strings(&ws["members"]),
            exclude: strings(&ws["exclude"]),
        });
    }
}

fn normalize_python(name: &str) -> String {
    let mut normalized = String::new();
    for ch in name.chars() {
        if matches!(ch, '-' | '_' | '.') {
            if !normalized.ends_with('-') {
                normalized.push('-');
            }
        } else {
            normalized.push(ch.to_ascii_lowercase());
        }
    }
    normalized
}

fn requirement(node: &mut Node, line: &str, file: &str) {
    let line = line.split('#').next().unwrap_or("").trim();
    if line.is_empty() {
        return;
    }
    let end = line
        .find(|c: char| !c.is_ascii_alphanumeric() && !matches!(c, '-' | '_' | '.'))
        .unwrap_or(line.len());
    if end == 0 {
        return;
    }
    let name = normalize_python(&line[..end]);
    let mut rest = line[end..].trim();
    if rest.starts_with('[') {
        let Some(end) = rest.find(']') else {
            return;
        };
        rest = rest[end + 1..].trim();
    }
    let constraint = rest.split(';').next().unwrap_or("").trim();
    let constraint = constraint.split(" --hash=").next().unwrap_or("").trim();
    let version = if constraint.is_empty() || constraint.starts_with('@') {
        None
    } else {
        Some(constraint)
    };
    node.dependency("pypi", &name, version, file);
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

fn go(node: &mut Node, file: &str, text: &str) {
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
    if file == "go.work" {
        node.workspaces.push(Workspace {
            ecosystem: "go".to_owned(),
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

fn ruby(node: &mut Node, text: &str, diagnostics: &mut Vec<Diagnostic>) {
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
                path: relative_file(&node.path, "Gemfile"),
                message: format!("dynamic {directive} declaration not evaluated"),
            });
            continue;
        };
        if directive == "ruby" {
            node.version("ruby", name, "Gemfile", "requirement");
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
                "Gemfile",
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

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}

fn pin(node: &mut Node, language: &str, file: &str, text: &str) {
    let value = text
        .lines()
        .filter(|line| !line.trim().starts_with('#'))
        .collect::<Vec<_>>()
        .join(" ");
    node.version(language, &value, file, "pin");
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
