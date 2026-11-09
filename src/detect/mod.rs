use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Args;
use serde::Serialize;

mod parsers;
mod rules;
#[cfg(test)]
mod tests;

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
    pub evidence: Vec<Evidence>,
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
    files: BTreeSet<String>,
    contents: BTreeMap<String, String>,
    sources: BTreeMap<String, String>,
    dependencies: BTreeMap<String, Dependency>,
    packages: BTreeSet<String>,
    versions: BTreeMap<String, Vec<Version>>,
    names: BTreeSet<String>,
    workspaces: Vec<Workspace>,
}

struct Dependency {
    version: Option<String>,
    source: String,
}

struct Workspace {
    ecosystem: String,
    source: String,
    members: Vec<String>,
    exclude: Vec<String>,
}

impl Node {
    fn dependency(&mut self, ecosystem: &str, name: &str, version: Option<&str>, file: &str) {
        self.dependencies.insert(
            format!("{ecosystem}:{name}"),
            Dependency {
                version: version.map(str::to_owned),
                source: relative_file(&self.path, file),
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
    let mut nodes = Vec::new();
    let mut pending = vec![(PathBuf::new(), 0)];
    while let Some((path, depth)) = pending.pop() {
        let mut node = Node {
            path: path.clone(),
            ..Node::default()
        };
        report.stats.directories += 1;
        let entries = fs::read_dir(root.join(&path))
            .with_context(|| format!("could not scan {}", root.join(&path).display()))?;
        let mut children = Vec::new();
        for entry in entries {
            let entry = entry?;
            let ty = entry.file_type()?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if ty.is_dir() {
                if IGNORED.contains(&name.as_str()) {
                    report.stats.skipped_directories += 1;
                } else if depth == MAX_DEPTH {
                    report.diagnostics.push(Diagnostic {
                        path: relative_file(&path, &name),
                        message: "directory depth limit reached".to_owned(),
                    });
                } else {
                    children.push((path.join(&name), depth + 1));
                }
            } else if ty.is_file() {
                report.stats.files += 1;
                if let Some(ext) = Path::new(&name).extension().and_then(|e| e.to_str()) {
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
                    report.stats.files_read += 1;
                    report.stats.bytes_read += bytes.len();
                    let content = if bytes.len() > MAX_FILE_BYTES as usize {
                        Err("manifest exceeds 1 MiB read limit".to_owned())
                    } else {
                        String::from_utf8(bytes).map_err(|_| "manifest is not UTF-8".to_owned())
                    };
                    match content {
                        Ok(content) => {
                            node.contents.insert(name.clone(), content);
                        }
                        Err(message) => report.diagnostics.push(Diagnostic {
                            path: relative_file(&path, &name),
                            message,
                        }),
                    }
                }
                if parsers::readable(&name) || rules.needs_filename(&name) {
                    node.files.insert(name);
                }
            }
        }
        children.sort();
        pending.extend(children.into_iter().rev());
        nodes.push(node);
    }
    parsers::parse(&mut nodes, &mut report.diagnostics);

    let mut projects = BTreeMap::<PathBuf, Project>::new();
    let mut source_detections = Vec::new();
    for node in &nodes {
        let mut detections = Vec::new();
        for detection in rules.evaluate(node) {
            if detection.kind == Kind::Source {
                source_detections.push((&node.path, detection));
            } else {
                detections.push(detection);
            }
        }
        if !detections.is_empty() {
            projects.insert(
                node.path.clone(),
                Project {
                    path: display_path(&node.path),
                    names: node.names.iter().cloned().collect(),
                    languages: Vec::new(),
                    detections,
                    workspace_members: Vec::new(),
                },
            );
        }
    }
    for (path, detection) in source_detections {
        let owner = path
            .ancestors()
            .find(|p| {
                projects.get(*p).is_some_and(|project| {
                    project.detections.iter().any(|d| {
                        d.kind != Kind::Workspace
                            && family(&d.language) == family(&detection.language)
                    })
                })
            })
            .map(Path::to_path_buf)
            .unwrap_or_else(|| path.clone());
        let project = projects.entry(owner.clone()).or_insert_with(|| Project {
            path: display_path(&owner),
            names: Vec::new(),
            languages: Vec::new(),
            detections: Vec::new(),
            workspace_members: Vec::new(),
        });
        if !project
            .detections
            .iter()
            .any(|d| d.language == detection.language)
        {
            project.detections.push(detection);
        }
    }
    for (path, project) in &mut projects {
        let languages = project
            .detections
            .iter()
            .map(|d| d.language.clone())
            .collect::<BTreeSet<_>>();
        for language in languages {
            let mut versions = Vec::new();
            for ancestor in path.ancestors() {
                if let Ok(i) = nodes.binary_search_by(|n| n.path.as_path().cmp(ancestor)) {
                    let node = &nodes[i];
                    if let Some(found) = node.versions.get(&language) {
                        versions.extend(
                            found
                                .iter()
                                .filter(|v| {
                                    ancestor == path || v.kind == "pin" || v.kind == "toolchain"
                                })
                                .cloned(),
                        );
                    }
                }
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
    let paths = projects.keys().cloned().collect::<Vec<_>>();
    for node in &nodes {
        let Some(project) = projects.get_mut(&node.path) else {
            continue;
        };
        for workspace in &node.workspaces {
            for member in &paths {
                if member == &node.path {
                    continue;
                }
                let Ok(relative) = member.strip_prefix(&node.path) else {
                    continue;
                };
                let relative = display_path(relative);
                let is_member = workspace
                    .members
                    .iter()
                    .any(|p| rules::path_glob(p, &relative));
                let excluded = workspace
                    .exclude
                    .iter()
                    .any(|p| rules::path_glob(p, &relative));
                let same_language = projects_language(&nodes, member, &workspace.ecosystem);
                if is_member && !excluded && same_language {
                    project.workspace_members.push(display_path(member));
                }
            }
        }
        project.workspace_members.sort();
        project.workspace_members.dedup();
    }
    report.projects = projects.into_values().collect();
    report
        .diagnostics
        .sort_by(|a, b| a.path.cmp(&b.path).then(a.message.cmp(&b.message)));
    Ok(report)
}

fn projects_language(nodes: &[Node], path: &Path, ecosystem: &str) -> bool {
    let file = match ecosystem {
        "cargo" => "Cargo.toml",
        "go" => "go.mod",
        "node" => "package.json",
        "python" => "pyproject.toml",
        _ => return false,
    };
    nodes
        .binary_search_by(|n| n.path.as_path().cmp(path))
        .ok()
        .is_some_and(|i| nodes[i].files.contains(file))
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
