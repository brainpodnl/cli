use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

use anyhow::{Context, Result, bail};
use clap::Args;
use serde::Serialize;

mod dotnet;
mod gradle;
mod java;
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
const IGNORED: &[&str] = &[
    ".git",
    ".jj",
    ".svn",
    "node_modules",
    "vendor",
    "target",
    ".venv",
    ".gradle",
    ".vs",
    "venv",
    "__pycache__",
    ".next",
    ".nuxt",
    ".output",
    "dist",
    "build",
    "coverage",
    ".bundle",
    ".brainpod",
    ".cache",
    "site-packages",
];

#[derive(Debug, Args)]
pub struct DetectArgs {
    /// Repository or project directory to inspect without executing project code
    #[arg(default_value = ".")]
    pub path: PathBuf,
    /// Additional TOML detection rules; IDs must be unique
    #[arg(long, value_name = "PATH")]
    pub rules: Option<PathBuf>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub schema_version: u32,
    pub projects: Vec<Project>,
    pub diagnostics: Vec<Diagnostic>,
    pub stats: Stats,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub path: String,
    pub names: Vec<String>,
    pub languages: Vec<Language>,
    pub detections: Vec<Detection>,
    pub workspace_members: Vec<String>,
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
    parent: Option<usize>,
    files: BTreeSet<String>,
    contents: BTreeMap<String, String>,
    sources: BTreeMap<String, String>,
    dependencies: BTreeMap<String, Dependency>,
    identities: BTreeMap<String, Dependency>,
    packages: BTreeMap<String, String>,
    versions: BTreeMap<String, Vec<Version>>,
    names: BTreeSet<String>,
    workspaces: Vec<Workspace>,
}

struct Dependency {
    version: Option<String>,
    source: String,
    resolved: Option<Version>,
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
        self.dependencies.insert(
            format!("{ecosystem}:{name}"),
            Dependency {
                version: version.map(str::to_owned),
                source: relative_file(&self.path, file),
                resolved: None,
            },
        );
    }

    fn identity(&mut self, ecosystem: &str, name: &str, version: Option<&str>, file: &str) {
        self.identities.insert(
            format!("{ecosystem}:{name}"),
            Dependency {
                version: version.map(str::to_owned),
                source: relative_file(&self.path, file),
                resolved: None,
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
    let report = scan(&args.path, extra.as_deref())?;
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
}

impl Inventory {
    fn read_directory(
        &mut self,
        root: &Path,
        path: &Path,
        depth: usize,
        rules: &Rules,
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
                if IGNORED.contains(&name.as_str()) {
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
                if parsers::readable(&name) {
                    let file = File::open(entry.path())?;
                    let mut bytes = Vec::new();
                    file.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes)?;
                    self.stats.files_read += 1;
                    self.stats.bytes_read += bytes.len();
                    let content = if bytes.len() > MAX_FILE_BYTES as usize {
                        Err("manifest exceeds 1 MiB read limit".to_owned())
                    } else {
                        String::from_utf8(bytes).map_err(|_| "manifest is not UTF-8".to_owned())
                    };
                    match content {
                        Ok(content) => {
                            node.contents.insert(name.clone(), content);
                        }
                        Err(message) => self.diagnostics.push(Diagnostic {
                            path: relative_file(path, &name),
                            message,
                        }),
                    }
                }
                if parsers::readable(&name) || rules.needs_filename(&name) {
                    node.files.insert(name);
                }
            }
        }
        if node.files.iter().any(|file| file.ends_with(".csproj")) {
            children.retain(|(path, _)| {
                let output = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| {
                        name.eq_ignore_ascii_case("bin") || name.eq_ignore_ascii_case("obj")
                    });
                if output {
                    self.stats.skipped_directories += 1;
                }
                !output
            });
        }
        if !node.files.is_empty() || !node.sources.is_empty() {
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
    ) -> Result<()> {
        while let Some((path, depth)) = pending.pop() {
            pending.extend(self.read_directory(root, &path, depth, rules)?);
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
    }
}

fn inventory(root: &Path, rules: &Rules, workers: usize) -> Result<Inventory> {
    let mut inventory = Inventory::default();
    let mut pending = vec![(PathBuf::new(), 0)];
    for _ in 0..3 {
        let mut next = Vec::new();
        for (path, depth) in pending {
            next.extend(inventory.read_directory(root, &path, depth, rules)?);
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
                            inventory.walk(root, rules, vec![task.clone()])?;
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
        inventory.walk(root, rules, pending)?;
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

pub fn scan(root: &Path, extra_rules: Option<&str>) -> Result<Report> {
    let custom = extra_rules
        .map(|extra| Rules::new(Some(extra)))
        .transpose()?;
    let rules = match custom.as_ref() {
        Some(rules) => rules,
        None => Rules::builtin()?,
    };
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
    };
    let workers = thread::available_parallelism().map_or(1, |n| n.get().min(4));
    let Inventory {
        mut nodes,
        diagnostics,
        stats,
    } = inventory(&root, rules, workers)?;
    report.diagnostics = diagnostics;
    report.stats = stats;
    let original_nodes = nodes.len();
    parsers::parse(&mut nodes, &mut report.diagnostics);
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
                    detections,
                    workspace_members: Vec::new(),
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
                    d.kind != Kind::Workspace && family(&d.language) == family(&detection.language)
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
                    detections: Vec::new(),
                    workspace_members: Vec::new(),
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
            let mut sdk_scope_seen = false;
            while let Some(i) = ancestor {
                if let Some(found) = nodes[i].versions.get(&language) {
                    versions.extend(
                        found
                            .iter()
                            .filter(|v| {
                                if v.kind == "sdk" {
                                    return !sdk_scope_seen;
                                }
                                i == *index || v.kind == "pin" || v.kind == "toolchain"
                            })
                            .cloned(),
                    );
                }
                if nodes[i].files.contains("global.json") {
                    sdk_scope_seen = true;
                }
                ancestor = nodes[i].parent;
            }
            let mut seen = BTreeSet::new();
            versions.retain(|v| seen.insert(v.clone()));
            project.languages.push(Language {
                name: language,
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
        for ecosystem in ["dotnet", "gradle"] {
            if node.packages.contains_key(ecosystem) {
                candidates.entry(ecosystem).or_default().push(index);
            }
        }
        for (ecosystem, file) in [
            ("cargo", "Cargo.toml"),
            ("go", "go.mod"),
            ("node", "package.json"),
            ("python", "pyproject.toml"),
            ("maven", "pom.xml"),
        ] {
            if node.files.contains(file) {
                candidates.entry(ecosystem).or_default().push(index);
            }
        }
    }
    for (index, project) in &mut projects {
        let node = &nodes[*index];
        for workspace in &node.workspaces {
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
                    project
                        .workspace_members
                        .push(display_path(&nodes[member].path));
                }
            }
        }
        project.workspace_members.sort();
        project.workspace_members.dedup();
    }
    projects.sort_by(|a, b| nodes[a.0].path.cmp(&nodes[b.0].path));
    report.projects = projects.into_iter().map(|(_, project)| project).collect();
    report
        .diagnostics
        .sort_by(|a, b| a.path.cmp(&b.path).then(a.message.cmp(&b.message)));
    Ok(report)
}

fn family(language: &str) -> &str {
    match language {
        "typescript" | "javascript" => "node",
        other => other,
    }
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
            for language in project["languages"].as_array().into_iter().flatten() {
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
