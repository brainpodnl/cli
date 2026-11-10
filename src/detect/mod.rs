use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

use anyhow::{Context, Result, bail};
use clap::Args;
use serde::Serialize;

mod catalog;
mod deno;
#[cfg(test)]
mod deno_tests;
mod dotnet;
mod environment;
#[cfg(test)]
mod environment_platform_tests;
#[cfg(test)]
mod environment_reference_tests;
#[cfg(test)]
mod environment_tests;
mod gradle;
mod java;
mod jsonc;
#[cfg(test)]
mod jvm_dotnet_tests;
mod parsers;
mod rules;
#[cfg(test)]
mod tests;
mod values;
mod xml;

use rules::{Kind, Rules};

const MAX_FILE_BYTES: u64 = 1024 * 1024;
const MAX_DEPTH: usize = 128;
#[derive(Debug, Args)]
pub struct DetectArgs {
    /// Repository or project directory to inspect without executing project code
    #[arg(default_value = ".")]
    pub path: PathBuf,
    /// Additional TOML detection rules; IDs must be unique
    #[arg(long, value_name = "PATH")]
    pub rules: Option<PathBuf>,
    /// Discover environment declarations and references without exposing values
    #[arg(long)]
    pub with_env: bool,
    /// Also search application source files for cataloged platform/framework variable names
    #[arg(long, requires = "with_env")]
    pub with_source: bool,
}

/// How much environment metadata a scan collects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EnvScan {
    /// No environment adapters and no additional reads.
    Off,
    /// Manifest, config and deployment files only.
    Declarations,
    /// Declarations plus the `catalog.code_references` source search.
    Source,
}

impl EnvScan {
    fn declarations(self) -> bool {
        self != Self::Off
    }

    fn source(self) -> bool {
        self == Self::Source
    }
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub schema_version: u32,
    pub projects: Vec<Project>,
    pub diagnostics: Vec<Diagnostic>,
    pub stats: Stats,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment: Option<Vec<environment::Declaration>>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub path: String,
    pub names: Vec<String>,
    pub languages: Vec<Language>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub runtimes: Vec<Runtime>,
    pub detections: Vec<Detection>,
    pub workspace_members: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub workspaces: Vec<WorkspaceReport>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct WorkspaceReport {
    pub ecosystem: String,
    pub source: String,
    pub members: Vec<String>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct Runtime {
    pub name: String,
    pub source: String,
    pub versions: Vec<Version>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct Language {
    pub name: String,
    pub versions: Vec<Version>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub value: String,
    pub source: String,
    pub kind: String,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct Detection {
    pub id: String,
    pub language: String,
    pub kind: Kind,
    pub priority: i32,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub roles: Vec<FrameworkRole>,
    pub evidence: Vec<Evidence>,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
pub enum FrameworkRole {
    Uses,
    Provides,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct Evidence {
    pub source: String,
    pub pattern: String,
    pub value: Option<String>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct Diagnostic {
    pub path: String,
    pub message: String,
}

#[derive(Debug, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Stats {
    pub directories: usize,
    pub files: usize,
    pub files_read: usize,
    pub bytes_read: usize,
    pub skipped_directories: usize,
}

#[derive(Default)]
struct Node {
    path: PathBuf,
    environment: Vec<environment::Declaration>,
    code_hits: environment::references::Hits,
    parent: Option<usize>,
    files: BTreeSet<String>,
    contents: BTreeMap<String, String>,
    sources: BTreeMap<String, String>,
    dependencies: BTreeMap<String, Dependency>,
    identities: BTreeMap<String, Dependency>,
    packages: BTreeMap<String, String>,
    versions: BTreeMap<String, Vec<Version>>,
    runtimes: BTreeMap<String, String>,
    runtime_versions: BTreeMap<String, Vec<Version>>,
    names: BTreeSet<String>,
    workspaces: Vec<Workspace>,
}

#[derive(Clone)]
struct Dependency {
    version: Option<String>,
    source: String,
    resolved: Option<Version>,
    declarations: Vec<Version>,
}

#[derive(Clone)]
struct Workspace {
    ecosystem: String,
    source: String,
    members: Vec<String>,
    exclude: Vec<String>,
}

impl Workspace {
    fn contains(&self, path: &str) -> bool {
        self.members.iter().any(|p| rules::path_glob(p, path))
            && !self.exclude.iter().any(|p| rules::path_glob(p, path))
    }
}

impl Node {
    fn dependency(&mut self, ecosystem: &str, name: &str, version: Option<&str>, file: &str) {
        self.merge_dependency(
            format!("{ecosystem}:{name}"),
            Dependency {
                version: version.map(str::to_owned),
                source: relative_file(&self.path, file),
                resolved: None,
                declarations: Vec::new(),
            },
        );
    }

    fn merge_dependency(&mut self, key: String, mut dependency: Dependency) {
        if let Some(previous) = self.dependencies.remove(&key) {
            dependency.declarations.extend(previous.declarations);
            if let Some(value) = previous.version {
                dependency.declarations.push(Version {
                    value,
                    source: previous.source,
                    kind: "requirement".to_owned(),
                });
            }
        }
        self.dependencies.insert(key, dependency);
    }

    fn identity(&mut self, ecosystem: &str, name: &str, version: Option<&str>, file: &str) {
        self.identities.insert(
            format!("{ecosystem}:{name}"),
            Dependency {
                version: version.map(str::to_owned),
                source: relative_file(&self.path, file),
                resolved: None,
                declarations: Vec::new(),
            },
        );
    }

    fn version(&mut self, language: &str, value: &str, file: &str, kind: &str) {
        if !value.trim().is_empty() {
            self.versions
                .entry(language.to_owned())
                .or_default()
                .push(Version {
                    value: value.trim().to_owned(),
                    source: relative_file(&self.path, file),
                    kind: kind.to_owned(),
                });
        }
    }
}

pub fn handle(args: DetectArgs) -> Result<crate::output::CommandOutput> {
    let extra = args
        .rules
        .as_deref()
        .map(fs::read_to_string)
        .transpose()
        .context("could not read detection rules")?;
    let env = match (args.with_env, args.with_source) {
        (false, _) => EnvScan::Off,
        (true, false) => EnvScan::Declarations,
        (true, true) => EnvScan::Source,
    };
    let report = scan_options(&args.path, extra.as_deref(), env)?;
    Ok(crate::output::CommandOutput::new(
        serde_json::to_value(report)?,
        crate::output::View::Detect,
    ))
}

#[derive(Default)]
struct Inventory {
    nodes: Vec<Node>,
    diagnostics: Vec<Diagnostic>,
    stats: Stats,
    /// Code-reference signal basenames seen anywhere in the scan (`--with-env` only).
    signals: BTreeSet<String>,
}

impl Inventory {
    fn read_directory(
        &mut self,
        root: &Path,
        path: &Path,
        depth: usize,
        rules: &Rules,
        env: EnvScan,
    ) -> Result<Vec<(PathBuf, usize)>> {
        let mut node = Node::default();
        self.stats.directories += 1;
        let entries = fs::read_dir(root.join(path))
            .with_context(|| format!("could not scan {}", root.join(path).display()))?;
        let mut children = Vec::new();
        for entry in entries {
            let entry = entry?;
            let ty = entry.file_type()?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if ty.is_dir() {
                if rules.catalog.ignored(&name) {
                    self.stats.skipped_directories += 1;
                } else if depth == MAX_DEPTH {
                    self.diagnostics.push(Diagnostic {
                        path: relative_file(path, &name),
                        message: "directory depth limit reached".to_owned(),
                    });
                } else {
                    children.push((path.join(&name), depth + 1));
                }
            } else if ty.is_file() {
                self.stats.files += 1;
                if let Some(ext) = Path::new(&name).extension().and_then(|e| e.to_str())
                    && rules.needs_source(ext)
                {
                    node.sources
                        .entry(ext.to_owned())
                        .and_modify(|existing| {
                            if name < *existing {
                                *existing = name.clone();
                            }
                        })
                        .or_insert_with(|| name.clone());
                }
                let readable = rules.catalog.readable(&name);
                let format = env
                    .declarations()
                    .then(|| rules.catalog.environment.matching(path, &name))
                    .flatten();
                let code = env.source() && rules.catalog.references.scans(&name);
                if env.source() && rules.catalog.references.is_signal(&name) {
                    self.signals.insert(name.clone());
                }
                if readable || format.is_some() || code {
                    let file = File::open(entry.path())?;
                    let mut bytes = Vec::new();
                    file.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes)?;
                    self.stats.files_read += 1;
                    self.stats.bytes_read += bytes.len();
                    let oversized = bytes.len() > MAX_FILE_BYTES as usize;
                    if code && !oversized {
                        rules.catalog.references.find(
                            &relative_file(path, &name),
                            &bytes,
                            &mut node.code_hits,
                        );
                    }
                    let content = if !(readable || format.is_some()) {
                        oversized.then(|| {
                            Err("source exceeds 1 MiB read limit; environment references not scanned"
                                .to_owned())
                        })
                    } else if oversized {
                        Some(Err("manifest exceeds 1 MiB read limit".to_owned()))
                    } else {
                        Some(
                            String::from_utf8(bytes)
                                .map_err(|_| "manifest is not UTF-8".to_owned()),
                        )
                    };
                    match content {
                        Some(Ok(content)) => {
                            if let Some(format) = format {
                                node.environment.extend(environment::parse(
                                    format,
                                    &relative_file(path, &name),
                                    &content,
                                    &mut self.diagnostics,
                                ));
                            }
                            if readable {
                                node.contents.insert(name.clone(), content);
                            }
                        }
                        Some(Err(message)) => self.diagnostics.push(Diagnostic {
                            path: relative_file(path, &name),
                            message,
                        }),
                        None => (),
                    }
                }
                if readable || rules.needs_filename(&name) {
                    node.files.insert(name);
                }
            }
        }
        children.retain(|(path, _)| {
            let prune = path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| rules.catalog.pruned(&node, name));
            if prune {
                self.stats.skipped_directories += 1;
            }
            !prune
        });
        if !node.files.is_empty()
            || !node.sources.is_empty()
            || !node.environment.is_empty()
            || !node.code_hits.is_empty()
        {
            node.path = path.to_path_buf();
            self.nodes.push(node);
        }
        Ok(children)
    }

    fn walk(
        &mut self,
        root: &Path,
        rules: &Rules,
        mut pending: Vec<(PathBuf, usize)>,
        env: EnvScan,
    ) -> Result<()> {
        while let Some((path, depth)) = pending.pop() {
            pending.extend(self.read_directory(root, &path, depth, rules, env)?);
        }
        Ok(())
    }

    fn merge(&mut self, other: Self) {
        self.nodes.extend(other.nodes);
        self.diagnostics.extend(other.diagnostics);
        self.stats.directories += other.stats.directories;
        self.stats.files += other.stats.files;
        self.stats.files_read += other.stats.files_read;
        self.stats.bytes_read += other.stats.bytes_read;
        self.stats.skipped_directories += other.stats.skipped_directories;
        self.signals.extend(other.signals);
    }
}

fn inventory(root: &Path, rules: &Rules, workers: usize, env: EnvScan) -> Result<Inventory> {
    let mut inventory = Inventory::default();
    let mut pending = vec![(PathBuf::new(), 0)];
    for _ in 0..3 {
        let mut next = Vec::new();
        for (path, depth) in pending {
            next.extend(inventory.read_directory(root, &path, depth, rules, env)?);
        }
        pending = next;
        if pending.is_empty() {
            break;
        }
    }
    if workers > 1 && pending.len() >= 64 {
        let next = AtomicUsize::new(0);
        let results = thread::scope(|scope| {
            let handles = (0..workers)
                .map(|_| {
                    scope.spawn(|| -> Result<Inventory> {
                        let mut inventory = Inventory::default();
                        while let Some(task) = pending.get(next.fetch_add(1, Ordering::Relaxed)) {
                            inventory.walk(root, rules, vec![task.clone()], env)?;
                        }
                        Ok(inventory)
                    })
                })
                .collect::<Vec<_>>();
            handles
                .into_iter()
                .map(|handle| {
                    handle
                        .join()
                        .map_err(|_| anyhow::anyhow!("directory scan worker panicked"))?
                })
                .collect::<Result<Vec<_>>>()
        })?;
        for result in results {
            inventory.merge(result);
        }
    } else {
        inventory.walk(root, rules, pending, env)?;
    }
    index_parents(&mut inventory.nodes);
    Ok(inventory)
}

fn index_parents(nodes: &mut [Node]) {
    nodes.sort_by(|a, b| a.path.cmp(&b.path));
    let mut ancestors = Vec::<usize>::new();
    for index in 0..nodes.len() {
        while ancestors
            .last()
            .is_some_and(|&parent| !nodes[index].path.starts_with(&nodes[parent].path))
        {
            ancestors.pop();
        }
        nodes[index].parent = ancestors.last().copied();
        ancestors.push(index);
    }
}

#[cfg(test)]
pub fn scan(root: &Path, extra_rules: Option<&str>) -> Result<Report> {
    scan_options(root, extra_rules, EnvScan::Off)
}

fn scan_options(root: &Path, extra_rules: Option<&str>, env: EnvScan) -> Result<Report> {
    let custom = extra_rules
        .map(|extra| Rules::new(Some(extra)))
        .transpose()?;
    let rules = match custom.as_ref() {
        Some(rules) => rules,
        None => Rules::builtin()?,
    };
    if env.declarations() && fs::symlink_metadata(root)?.file_type().is_symlink() {
        bail!("environment scan root must not be a symlink");
    }
    let root = root
        .canonicalize()
        .with_context(|| format!("could not open {}", root.display()))?;
    if !root.is_dir() {
        bail!("detection root is not a directory: {}", root.display());
    }
    let mut report = Report {
        schema_version: 1,
        projects: Vec::new(),
        diagnostics: Vec::new(),
        stats: Stats::default(),
        environment: env.declarations().then(Vec::new),
    };
    let workers = thread::available_parallelism().map_or(1, |n| n.get().min(4));
    let Inventory {
        mut nodes,
        diagnostics,
        stats,
        signals,
    } = inventory(&root, rules, workers, env)?;
    report.diagnostics = diagnostics;
    report.stats = stats;
    let original_nodes = nodes.len();
    parsers::parse(
        &root,
        &mut nodes,
        &rules.catalog,
        &mut report.stats,
        &mut report.diagnostics,
    );
    if nodes.len() != original_nodes {
        index_parents(&mut nodes);
    }

    let mut projects = Vec::<(usize, Project)>::new();
    let mut project_indices = vec![None; nodes.len()];
    let mut source_detections = Vec::new();
    for (index, node) in nodes.iter().enumerate() {
        let mut detections = Vec::new();
        for detection in rules.evaluate(node) {
            if detection.kind == Kind::Source {
                source_detections.push((index, detection));
            } else {
                detections.push(detection);
            }
        }
        if !detections.is_empty() {
            project_indices[index] = Some(projects.len());
            projects.push((
                index,
                Project {
                    path: display_path(&node.path),
                    names: node.names.iter().cloned().collect(),
                    languages: Vec::new(),
                    runtimes: Vec::new(),
                    detections,
                    workspace_members: Vec::new(),
                    workspaces: Vec::new(),
                },
            ));
        }
    }
    for (index, detection) in source_detections {
        let mut ancestor = Some(index);
        let mut owner = None;
        while let Some(i) = ancestor {
            if let Some(project_index) = project_indices[i]
                && projects[project_index].1.detections.iter().any(|d| {
                    d.kind != Kind::Workspace
                        && rules.catalog.family(&d.language)
                            == rules.catalog.family(&detection.language)
                })
            {
                owner = Some(project_index);
                break;
            }
            ancestor = nodes[i].parent;
        }
        let owner = owner.or(project_indices[index]).unwrap_or_else(|| {
            let project_index = projects.len();
            project_indices[index] = Some(project_index);
            projects.push((
                index,
                Project {
                    path: display_path(&nodes[index].path),
                    names: Vec::new(),
                    languages: Vec::new(),
                    runtimes: Vec::new(),
                    detections: Vec::new(),
                    workspace_members: Vec::new(),
                    workspaces: Vec::new(),
                },
            ));
            project_index
        });
        let project = &mut projects[owner].1;
        if !project
            .detections
            .iter()
            .any(|d| d.language == detection.language)
        {
            project.detections.push(detection);
        }
    }
    for (index, project) in &mut projects {
        let languages = project
            .detections
            .iter()
            .map(|d| d.language.clone())
            .collect::<BTreeSet<_>>();
        for language in languages {
            let mut versions = Vec::new();
            let mut ancestor = Some(*index);
            let mut boundaries = BTreeSet::new();
            while let Some(i) = ancestor {
                if let Some(found) = nodes[i].versions.get(&language) {
                    versions.extend(
                        found
                            .iter()
                            .filter(|v| {
                                (i == *index || rules.catalog.inherited(&v.kind))
                                    && !boundaries.contains(v.kind.as_str())
                            })
                            .cloned(),
                    );
                }
                boundaries.extend(rules.catalog.boundaries(&nodes[i]));
                ancestor = nodes[i].parent;
            }
            let mut seen = BTreeSet::new();
            versions.retain(|v| seen.insert(v.clone()));
            versions.sort();
            project.languages.push(Language {
                name: language,
                versions,
            });
        }
        let mut runtime_sources = nodes[*index].runtimes.clone();
        for (name, versions) in &nodes[*index].runtime_versions {
            if let Some(version) = versions.first() {
                runtime_sources
                    .entry(name.clone())
                    .or_insert_with(|| version.source.clone());
            }
        }
        for (name, source) in runtime_sources {
            let mut versions = Vec::new();
            let mut ancestor = Some(*index);
            while let Some(i) = ancestor {
                if let Some(found) = nodes[i].runtime_versions.get(&name) {
                    versions.extend(
                        found
                            .iter()
                            .filter(|v| i == *index || rules.catalog.inherited(&v.kind))
                            .cloned(),
                    );
                }
                ancestor = nodes[i].parent;
            }
            versions.sort();
            versions.dedup();
            project.runtimes.push(Runtime {
                name,
                source,
                versions,
            });
        }
        project
            .detections
            .sort_by(|a, b| b.priority.cmp(&a.priority).then(a.id.cmp(&b.id)));
    }
    let mut candidates = BTreeMap::<&str, Vec<usize>>::new();
    for (index, node) in nodes.iter().enumerate() {
        if project_indices[index].is_none() {
            continue;
        }
        for ecosystem in rules.catalog.workspace_candidates(node) {
            candidates.entry(ecosystem).or_default().push(index);
        }
    }
    for (index, project) in &mut projects {
        let node = &nodes[*index];
        for workspace in &node.workspaces {
            let mut members = Vec::new();
            for &member in candidates
                .get(workspace.ecosystem.as_str())
                .into_iter()
                .flatten()
            {
                if member == *index {
                    continue;
                }
                let Ok(relative) = nodes[member].path.strip_prefix(&node.path) else {
                    continue;
                };
                if workspace.contains(&display_path(relative)) {
                    members.push(display_path(&nodes[member].path));
                }
            }
            members.sort();
            members.dedup();
            project.workspace_members.extend(members.iter().cloned());
            project.workspaces.push(WorkspaceReport {
                ecosystem: workspace.ecosystem.clone(),
                source: workspace.source.clone(),
                members,
            });
        }
        project
            .workspaces
            .sort_by(|a, b| a.ecosystem.cmp(&b.ecosystem).then(a.source.cmp(&b.source)));
        project.workspace_members.sort();
        project.workspace_members.dedup();
    }
    if let Some(environment) = &mut report.environment {
        let mut code_hits = Vec::new();
        for (index, node) in nodes.iter().enumerate() {
            let mut ancestor = Some(index);
            let mut owner = None;
            while let Some(i) = ancestor {
                if let Some(project) = project_indices[i] {
                    owner = Some(project);
                    break;
                }
                ancestor = nodes[i].parent;
            }
            let path = owner.map(|project| projects[project].1.path.clone());
            environment.extend(node.environment.iter().cloned().map(|mut declaration| {
                declaration.project = path.clone();
                declaration
            }));
            if !node.code_hits.is_empty() {
                let detections = owner
                    .into_iter()
                    .flat_map(|project| &projects[project].1.detections)
                    .map(|detection| detection.id.clone())
                    .collect::<BTreeSet<_>>();
                code_hits.push((path, detections, &node.code_hits));
            }
        }
        environment.extend(
            rules.catalog.references.records(
                code_hits
                    .iter()
                    .map(|(path, detections, hits)| (path.clone(), detections, *hits)),
                &signals,
            ),
        );
        environment.sort_by(|a, b| a.source.cmp(&b.source));
    }
    projects.sort_by(|a, b| nodes[a.0].path.cmp(&nodes[b.0].path));
    report.projects = projects.into_iter().map(|(_, project)| project).collect();
    report
        .diagnostics
        .sort_by(|a, b| a.path.cmp(&b.path).then(a.message.cmp(&b.message)));
    Ok(report)
}

fn display_path(path: &Path) -> String {
    if path.as_os_str().is_empty() {
        ".".to_owned()
    } else {
        path.to_string_lossy().replace('\\', "/")
    }
}

fn relative_file(path: &Path, name: &str) -> String {
    display_path(&path.join(name))
}

pub fn render(value: &serde_json::Value) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(projects) = value["projects"].as_array() {
        for project in projects {
            let languages = project["languages"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|l| l["name"].as_str())
                .collect::<Vec<_>>();
            let detections = project["detections"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|d| d["id"].as_str())
                .collect::<Vec<_>>();
            lines.push(format!(
                "{}\t{}\t{}",
                project["path"].as_str().unwrap_or("."),
                languages.join(", "),
                detections.join(", ")
            ));
            for runtime in project["runtimes"].as_array().into_iter().flatten() {
                lines.push(format!(
                    "  runtime {} ({})",
                    runtime["name"].as_str().unwrap_or(""),
                    runtime["source"].as_str().unwrap_or("")
                ));
            }
            for language in project["languages"]
                .as_array()
                .into_iter()
                .flatten()
                .chain(project["runtimes"].as_array().into_iter().flatten())
            {
                for version in language["versions"].as_array().into_iter().flatten() {
                    lines.push(format!(
                        "  {} {} ({}, {})",
                        language["name"].as_str().unwrap_or(""),
                        version["value"].as_str().unwrap_or(""),
                        version["kind"].as_str().unwrap_or(""),
                        version["source"].as_str().unwrap_or("")
                    ));
                }
            }
        }
    }
    for declaration in value["environment"].as_array().into_iter().flatten() {
        lines.push(format!(
            "env {}\t{} ({}, {}, {}, project: {})",
            declaration["name"]
                .as_str()
                .unwrap_or("<unresolved reference>"),
            declaration["source"].as_str().unwrap_or(""),
            declaration["format"].as_str().unwrap_or(""),
            declaration["location"].as_str().unwrap_or(""),
            declaration["state"].as_str().unwrap_or(""),
            declaration["project"].as_str().unwrap_or("unowned")
        ));
        if let Some(reference) = declaration.get("reference") {
            lines.push(format!(
                "  reference {} (target: {}, key: {}, prefix: {})",
                reference["kind"].as_str().unwrap_or(""),
                reference["target"].as_str().unwrap_or("unresolved"),
                reference["key"].as_str().unwrap_or("-"),
                reference["prefix"].as_str().unwrap_or("-")
            ));
        }
        let facts = [
            "required",
            "secret",
            "generated",
            "hasDefault",
            "scope",
            "origin",
            "provider",
            "occurrenceCount",
        ]
        .into_iter()
        .filter_map(|key| {
            let value = declaration.get(key)?;
            Some(format!(
                "{key}: {}",
                value
                    .as_str()
                    .map_or_else(|| value.to_string(), str::to_owned)
            ))
        })
        .collect::<Vec<_>>();
        if !facts.is_empty() {
            lines.push(format!("  {}", facts.join(", ")));
        }
    }
    for diagnostic in value["diagnostics"].as_array().into_iter().flatten() {
        lines.push(format!(
            "warning: {}: {}",
            diagnostic["path"].as_str().unwrap_or(""),
            diagnostic["message"].as_str().unwrap_or("")
        ));
    }
    if lines.is_empty() {
        lines.push("No projects detected".to_owned());
    }
    lines
}
