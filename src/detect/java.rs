use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use super::values::{self, Located, Properties, Syntax};
use super::{Diagnostic, Node, Workspace, display_path, relative_file, xml};

#[derive(Default)]
struct Artifact {
    group: String,
    name: String,
    version: Option<Located>,
    scope: String,
    kind: String,
    compiler: Vec<(String, Located)>,
}

#[derive(Default)]
struct Pom {
    group: String,
    name: String,
    version: String,
    parent: Option<Artifact>,
    parent_path: Option<String>,
    properties: Properties,
    dependencies: Vec<Artifact>,
    managed: Vec<Artifact>,
    plugins: Vec<Artifact>,
    modules: Vec<String>,
}

fn pom(text: &str, source: &str) -> Result<Pom> {
    let mut doc = Pom::default();
    let mut artifacts = BTreeMap::<usize, Artifact>::new();
    xml::read(text, |stack| {
        if stack.first().is_none_or(|e| e.name != "project") {
            bail!("expected a Maven project root");
        }
        let Some(element) = stack.last() else {
            return Ok(());
        };
        let is = |path: &[&str]| stack.iter().map(|e| e.name).eq(path.iter().copied());
        let located = || Located {
            value: element.text.trim().to_owned(),
            source: source.to_owned(),
            conditional: false,
        };
        if stack.len() == 2 {
            match element.name {
                "groupId" => doc.group = element.text.trim().to_owned(),
                "artifactId" => doc.name = element.text.trim().to_owned(),
                "version" => doc.version = element.text.trim().to_owned(),
                _ => (),
            }
        }
        if stack.len() == 3 && stack[1].name == "parent" {
            let parent = doc.parent.get_or_insert_with(Artifact::default);
            match element.name {
                "groupId" => parent.group = element.text.trim().to_owned(),
                "artifactId" => parent.name = element.text.trim().to_owned(),
                "version" => parent.version = Some(located()),
                "relativePath" => doc.parent_path = Some(element.text.trim().to_owned()),
                _ => (),
            }
        }
        if stack.len() == 3 && stack[1].name == "properties" {
            doc.properties.insert(element.name.to_owned(), located());
        }
        if is(&["project", "modules", "module"]) {
            doc.modules.push(element.text.trim().to_owned());
        }
        if let Some(parent) = stack.get(stack.len().saturating_sub(2))
            && matches!(parent.name, "dependency" | "plugin")
        {
            let artifact = artifacts.entry(stack.len() - 1).or_default();
            match element.name {
                "groupId" => artifact.group = element.text.trim().to_owned(),
                "artifactId" => artifact.name = element.text.trim().to_owned(),
                "version" => artifact.version = Some(located()),
                "scope" => artifact.scope = element.text.trim().to_owned(),
                "type" => artifact.kind = element.text.trim().to_owned(),
                _ => (),
            }
        }
        if let Some(plugin) = stack.iter().position(|e| e.name == "plugin")
            && matches!(element.name, "release" | "source" | "target")
            && stack
                .get(stack.len().saturating_sub(2))
                .is_some_and(|e| e.name == "configuration")
        {
            artifacts
                .entry(plugin + 1)
                .or_default()
                .compiler
                .push((element.name.to_owned(), located()));
        }
        if element.name == "dependency" {
            let artifact = artifacts.remove(&stack.len()).unwrap_or_default();
            if is(&["project", "dependencies", "dependency"]) && artifact.scope != "test" {
                doc.dependencies.push(artifact);
            } else if is(&[
                "project",
                "dependencyManagement",
                "dependencies",
                "dependency",
            ]) {
                doc.managed.push(artifact);
            }
        }
        if element.name == "plugin" {
            let mut artifact = artifacts.remove(&stack.len()).unwrap_or_default();
            if artifact.group.is_empty() {
                artifact.group = "org.apache.maven.plugins".to_owned();
            }
            if is(&["project", "build", "plugins", "plugin"]) {
                doc.plugins.push(artifact);
            }
        }
        Ok(())
    })?;
    if doc.name.is_empty() {
        bail!("Maven project artifactId is missing");
    }
    Ok(doc)
}

fn chain<'a>(
    path: &Path,
    documents: &'a BTreeMap<PathBuf, Option<Pom>>,
    seen: &mut BTreeSet<PathBuf>,
    output: &mut Vec<(PathBuf, &'a Pom)>,
) {
    let Some(Some(doc)) = documents.get(path) else {
        return;
    };
    if seen.len() == 64 || !seen.insert(path.to_path_buf()) {
        return;
    }
    if let Some(parent) = &doc.parent {
        let relative = doc.parent_path.as_deref().unwrap_or("../pom.xml");
        if !relative.is_empty()
            && let Some(target) =
                values::child_file(path.parent().unwrap_or(Path::new("")), relative)
            && let Some(Some(local)) = documents.get(&target)
            && local.name == parent.name
            && (if local.group.is_empty() {
                local.parent.as_ref().map(|p| p.group.as_str())
            } else {
                Some(local.group.as_str())
            }) == Some(parent.group.as_str())
            && parent.version.as_ref().is_none_or(|requested| {
                let actual = if local.version.is_empty() {
                    local
                        .parent
                        .as_ref()
                        .and_then(|p| p.version.as_ref())
                        .map(|v| v.value.as_str())
                } else {
                    Some(local.version.as_str())
                };
                actual == Some(requested.value.as_str())
            })
        {
            chain(&target, documents, seen, output);
        }
    }
    output.push((path.to_path_buf(), doc));
}

fn literal(value: &str, source: &str) -> Located {
    Located {
        value: value.to_owned(),
        source: source.to_owned(),
        conditional: false,
    }
}

fn resolved(value: &str, source: &str, properties: &Properties) -> Option<String> {
    values::resolve(&literal(value, source), properties, Syntax::Maven).map(|v| v.value)
}

pub(super) fn parse(nodes: &mut Vec<Node>, diagnostics: &mut Vec<Diagnostic>) {
    let mut documents = BTreeMap::<PathBuf, Option<Pom>>::new();
    for node in nodes.iter() {
        if node.files.contains("pom.xml") {
            documents.insert(node.path.join("pom.xml"), None);
        }
        if let Some(text) = node.contents.get("pom.xml") {
            let source = relative_file(&node.path, "pom.xml");
            match pom(text, &source) {
                Ok(doc) => {
                    documents.insert(node.path.join("pom.xml"), Some(doc));
                }
                Err(error) => diagnostics.push(Diagnostic {
                    path: source,
                    message: error.to_string(),
                }),
            }
        }
    }
    if documents.is_empty() {
        super::gradle::parse(nodes, diagnostics);
        return;
    }
    for node in nodes.iter_mut() {
        let path = node.path.join("pom.xml");
        let Some(Some(doc)) = documents.get(&path) else {
            continue;
        };
        node.packages
            .insert("maven".to_owned(), display_path(&path));
        let mut inherited = Vec::new();
        chain(&path, &documents, &mut BTreeSet::new(), &mut inherited);
        let mut properties = Properties::new();
        let mut managed = BTreeMap::new();
        for (path, parent) in &inherited {
            let source = display_path(path);
            properties.extend(
                parent
                    .properties
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone())),
            );
            let group = if parent.group.is_empty() {
                parent.parent.as_ref().map_or("", |p| p.group.as_str())
            } else {
                &parent.group
            };
            let version = if parent.version.is_empty() {
                parent
                    .parent
                    .as_ref()
                    .and_then(|p| p.version.as_ref())
                    .map_or("", |v| v.value.as_str())
            } else {
                &parent.version
            };
            for (key, value) in [
                ("project.groupId", group),
                ("pom.groupId", group),
                ("project.artifactId", parent.name.as_str()),
                ("pom.artifactId", parent.name.as_str()),
                ("project.version", version),
                ("pom.version", version),
            ] {
                properties.insert(key.to_owned(), literal(value, &source));
            }
            for dependency in &parent.managed {
                if let (Some(group), Some(name)) = (
                    resolved(&dependency.group, &source, &properties),
                    resolved(&dependency.name, &source, &properties),
                ) {
                    managed.insert(format!("{group}:{name}"), dependency.version.as_ref());
                }
            }
        }
        let source = display_path(&path);
        let group = if doc.group.is_empty() {
            doc.parent.as_ref().map_or("", |p| p.group.as_str())
        } else {
            &doc.group
        };
        let group = resolved(group, &source, &properties).unwrap_or_else(|| group.to_owned());
        node.names.insert(doc.name.clone());
        if !group.is_empty() {
            node.names.insert(format!("{group}:{}", doc.name));
            let version = if doc.version.is_empty() {
                doc.parent
                    .as_ref()
                    .and_then(|p| p.version.as_ref())
                    .cloned()
            } else {
                Some(literal(&doc.version, &source))
            };
            values::identity(
                node,
                "maven",
                &format!("{group}:{}", doc.name),
                &source,
                version.as_ref(),
                &properties,
                Syntax::Maven,
            );
        }
        for (key, kind) in [
            ("java.version", "requirement"),
            ("maven.compiler.release", "release"),
            ("maven.compiler.source", "source"),
            ("maven.compiler.target", "target"),
        ] {
            if let Some(raw) = properties.get(key) {
                values::version(node, "java", raw, kind, &properties, Syntax::Maven);
            }
        }
        for (path, parent) in &inherited {
            let source = display_path(path);
            if let Some(parent) = &parent.parent {
                values::dependency(
                    node,
                    "maven-parent",
                    &format!("{}:{}", parent.group, parent.name),
                    &source,
                    parent.version.as_ref(),
                    &properties,
                    Syntax::Maven,
                );
            }
            for (ecosystem, dependencies) in [
                ("maven", &parent.dependencies),
                ("maven-plugin", &parent.plugins),
            ] {
                for artifact in dependencies {
                    let (Some(group), Some(name)) = (
                        resolved(&artifact.group, &source, &properties),
                        resolved(&artifact.name, &source, &properties),
                    ) else {
                        continue;
                    };
                    let key = format!("{group}:{name}");
                    let version = artifact
                        .version
                        .as_ref()
                        .or_else(|| managed.get(&key).copied().flatten());
                    values::dependency(
                        node,
                        ecosystem,
                        &key,
                        &source,
                        version,
                        &properties,
                        Syntax::Maven,
                    );
                    if artifact.name == "maven-compiler-plugin" {
                        for (kind, raw) in &artifact.compiler {
                            values::version(node, "java", raw, kind, &properties, Syntax::Maven);
                        }
                    }
                }
            }
        }
        if !doc.modules.is_empty() {
            let members = doc
                .modules
                .iter()
                .filter_map(|module| resolved(module, &source, &properties))
                .filter_map(|module| values::child_file(Path::new(""), &module))
                .map(|path| display_path(&path))
                .collect();
            node.workspaces.push(Workspace {
                ecosystem: "maven".to_owned(),
                source,
                members,
                exclude: Vec::new(),
            });
        }
    }
    super::gradle::parse(nodes, diagnostics);
}
