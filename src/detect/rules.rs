use std::collections::{BTreeSet, HashMap};
use std::sync::OnceLock;

use anyhow::{Result, bail};
use serde::Deserialize;

use super::{Detection, Evidence, FrameworkRole, Node};

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub id: String,
    pub language: String,
    pub kind: Kind,
    pub priority: i32,
    #[serde(default)]
    pub any: Vec<Pattern>,
    #[serde(default)]
    pub all: Vec<Pattern>,
    #[serde(default)]
    pub none: Vec<Pattern>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub enum Pattern {
    Atom(String),
    All(AllPattern),
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AllPattern {
    all: Vec<String>,
}

impl Pattern {
    fn atoms(&self) -> &[String] {
        match self {
            Self::Atom(atom) => std::slice::from_ref(atom),
            Self::All(group) => &group.all,
        }
    }

    fn matches(&self, node: &Node) -> bool {
        self.atoms()
            .iter()
            .all(|atom| matched(node, atom).is_some())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    Framework,
    Project,
    Workspace,
    Source,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    rules: Vec<Rule>,
}

pub struct Rules {
    rules: Vec<Rule>,
    files: HashMap<String, Vec<usize>>,
    sources: HashMap<String, Vec<usize>>,
    dependencies: HashMap<String, Vec<usize>>,
    identities: HashMap<String, Vec<usize>>,
    workspaces: HashMap<String, Vec<usize>>,
    packages: HashMap<String, Vec<usize>>,
    wildcard_files: Vec<(String, usize)>,
    file_patterns: BTreeSet<String>,
    wildcard_interest: BTreeSet<String>,
    source_interest: BTreeSet<String>,
}

impl Rules {
    pub fn builtin() -> Result<&'static Self> {
        static RULES: OnceLock<Result<Rules, String>> = OnceLock::new();
        RULES
            .get_or_init(|| Self::new(None).map_err(|e| e.to_string()))
            .as_ref()
            .map_err(|e| anyhow::anyhow!("{e}"))
    }

    pub fn new(extra: Option<&str>) -> Result<Self> {
        let mut document: Document = toml::from_str(include_str!("rules.toml"))?;
        if let Some(extra) = extra {
            document
                .rules
                .extend(toml::from_str::<Document>(extra)?.rules);
        }
        let mut ids = BTreeSet::new();
        for rule in &document.rules {
            if rule.id.is_empty() || rule.language.is_empty() || !ids.insert(&rule.id) {
                bail!("empty or duplicate detection rule: {}", rule.id);
            }
            if rule.any.is_empty() && rule.all.is_empty() {
                bail!("rule {} needs an any or all pattern", rule.id);
            }
            for pattern in rule.any.iter().chain(&rule.all).chain(&rule.none) {
                if pattern.atoms().is_empty() {
                    bail!("empty pattern group in rule {}", rule.id);
                }
                for pattern in pattern.atoms() {
                    let valid = match pattern.split_once(':') {
                        Some(("file" | "source", value)) => {
                            !value.is_empty() && !value.contains('/')
                        }
                        Some(("dep" | "identity", value)) => {
                            value.split_once(':').is_some_and(|(ecosystem, name)| {
                                !ecosystem.is_empty() && !name.is_empty()
                            })
                        }
                        Some(("package", "cargo")) => true,
                        Some(("workspace", value)) => {
                            matches!(value, "cargo" | "node" | "go" | "python")
                        }
                        _ => false,
                    };
                    if !valid {
                        bail!("invalid pattern {pattern:?} in rule {}", rule.id);
                    }
                }
            }
        }
        document
            .rules
            .sort_by(|a, b| b.priority.cmp(&a.priority).then(a.id.cmp(&b.id)));
        let mut rules = Self {
            rules: document.rules,
            files: HashMap::new(),
            sources: HashMap::new(),
            dependencies: HashMap::new(),
            identities: HashMap::new(),
            workspaces: HashMap::new(),
            packages: HashMap::new(),
            wildcard_files: Vec::new(),
            file_patterns: BTreeSet::new(),
            wildcard_interest: BTreeSet::new(),
            source_interest: BTreeSet::new(),
        };
        for (i, rule) in rules.rules.iter().enumerate() {
            for pattern in rule
                .all
                .iter()
                .chain(&rule.any)
                .chain(&rule.none)
                .flat_map(Pattern::atoms)
            {
                if let Some(value) = pattern.strip_prefix("source:") {
                    rules.source_interest.insert(value.to_owned());
                }
                if let Some(value) = pattern.strip_prefix("file:") {
                    if value.contains(['*', '?']) {
                        rules.wildcard_interest.insert(value.to_owned());
                    } else {
                        rules.file_patterns.insert(value.to_owned());
                    }
                }
            }
            for pattern in rule.all.iter().chain(&rule.any).flat_map(Pattern::atoms) {
                let Some((kind, value)) = pattern.split_once(':') else {
                    continue;
                };
                let index = match kind {
                    "file" if value.contains(['*', '?']) => {
                        rules.wildcard_files.push((value.to_owned(), i));
                        continue;
                    }
                    "file" => &mut rules.files,
                    "source" => &mut rules.sources,
                    "dep" => &mut rules.dependencies,
                    "identity" => &mut rules.identities,
                    "workspace" => &mut rules.workspaces,
                    "package" => &mut rules.packages,
                    _ => continue,
                };
                index.entry(value.to_owned()).or_default().push(i);
            }
        }
        Ok(rules)
    }
    pub fn needs_source(&self, extension: &str) -> bool {
        self.source_interest.contains(extension)
    }

    pub fn needs_filename(&self, name: &str) -> bool {
        self.file_patterns.contains(name) || self.wildcard_interest.iter().any(|p| glob(p, name))
    }

    pub fn evaluate(&self, node: &Node) -> Vec<Detection> {
        let mut candidates = Vec::new();
        for file in &node.files {
            if let Some(ids) = self.files.get(file) {
                candidates.extend(ids);
            }
        }
        for source in node.sources.keys() {
            if let Some(ids) = self.sources.get(source) {
                candidates.extend(ids);
            }
        }
        for dependency in node.dependencies.keys() {
            if let Some(ids) = self.dependencies.get(dependency) {
                candidates.extend(ids);
            }
        }
        for identity in node.identities.keys() {
            if let Some(ids) = self.identities.get(identity) {
                candidates.extend(ids);
            }
        }
        for package in &node.packages {
            if let Some(ids) = self.packages.get(package) {
                candidates.extend(ids);
            }
        }
        for workspace in &node.workspaces {
            if let Some(ids) = self.workspaces.get(&workspace.ecosystem) {
                candidates.extend(ids);
            }
        }
        for (pattern, id) in &self.wildcard_files {
            if node.files.iter().any(|file| glob(pattern, file)) {
                candidates.push(id);
            }
        }
        candidates.sort_unstable();
        candidates.dedup();
        candidates
            .into_iter()
            .filter_map(|&i| {
                let rule = &self.rules[i];
                rule.evaluate(node).map(|evidence| Detection {
                    id: rule.id.clone(),
                    language: rule.language.clone(),
                    kind: rule.kind,
                    priority: rule.priority,
                    roles: if rule.kind == Kind::Framework {
                        let mut roles = evidence
                            .iter()
                            .filter_map(|e| {
                                if e.pattern.starts_with("identity:") {
                                    Some(FrameworkRole::Provides)
                                } else if e.pattern.starts_with("dep:") {
                                    Some(FrameworkRole::Uses)
                                } else {
                                    None
                                }
                            })
                            .collect::<Vec<_>>();
                        if roles.is_empty() {
                            roles.push(FrameworkRole::Uses);
                        }
                        roles.sort();
                        roles.dedup();
                        roles
                    } else {
                        Vec::new()
                    },
                    evidence,
                })
            })
            .collect()
    }
}

impl Rule {
    pub fn evaluate(&self, node: &Node) -> Option<Vec<Evidence>> {
        if !self.all.iter().all(|p| p.matches(node))
            || (!self.any.is_empty() && !self.any.iter().any(|p| p.matches(node)))
            || self.none.iter().any(|p| p.matches(node))
        {
            return None;
        }
        let mut evidence = Vec::new();
        for pattern in self.all.iter().chain(&self.any).filter(|p| p.matches(node)) {
            for atom in pattern.atoms() {
                if let Some(found) = matched(node, atom) {
                    found.evidence(node, atom, &mut evidence);
                }
            }
        }
        evidence.sort();
        evidence.dedup();
        Some(evidence)
    }
}

enum Matched<'a> {
    File(&'a str),
    Dependency(&'a super::Dependency),
    Workspace(&'a super::Workspace),
}

impl Matched<'_> {
    fn evidence(self, node: &Node, pattern: &str, evidence: &mut Vec<Evidence>) {
        let (source, value) = match self {
            Self::File(name) => (super::relative_file(&node.path, name), None),
            Self::Dependency(dep) => {
                if let Some(resolved) = &dep.resolved {
                    evidence.push(Evidence {
                        source: resolved.source.clone(),
                        pattern: pattern.to_owned(),
                        value: Some(resolved.value.clone()),
                    });
                }
                (dep.source.clone(), dep.version.clone())
            }
            Self::Workspace(ws) => (ws.source.clone(), None),
        };
        evidence.push(Evidence {
            source,
            pattern: pattern.to_owned(),
            value,
        });
    }
}

fn matched<'a>(node: &'a Node, pattern: &str) -> Option<Matched<'a>> {
    let (kind, value) = pattern.split_once(':')?;
    match kind {
        "file" => {
            let name = if value.contains(['*', '?']) {
                node.files.iter().find(|name| glob(value, name))
            } else {
                node.files.get(value)
            };
            name.map(|name| Matched::File(name))
        }
        "source" => node.sources.get(value).map(|name| Matched::File(name)),
        "dep" => node.dependencies.get(value).map(Matched::Dependency),
        "identity" => node.identities.get(value).map(Matched::Dependency),
        "package" => node
            .packages
            .get(value)
            .map(|_| Matched::File("Cargo.toml")),
        "workspace" => node
            .workspaces
            .iter()
            .find(|w| w.ecosystem == value)
            .map(Matched::Workspace),
        _ => None,
    }
}

pub fn glob(pattern: &str, value: &str) -> bool {
    let (p, v) = (pattern.as_bytes(), value.as_bytes());
    let (mut i, mut j, mut star, mut retry) = (0, 0, None, 0);
    while j < v.len() {
        if i < p.len() && (p[i] == b'?' || p[i] == v[j]) {
            i += 1;
            j += 1;
        } else if i < p.len() && p[i] == b'*' {
            star = Some(i);
            i += 1;
            retry = j;
        } else if let Some(s) = star {
            retry += 1;
            j = retry;
            i = s + 1;
        } else {
            return false;
        }
    }
    while i < p.len() && p[i] == b'*' {
        i += 1;
    }
    i == p.len()
}

pub fn path_glob(pattern: &str, value: &str) -> bool {
    let pattern = pattern.strip_prefix("./").unwrap_or(pattern);
    let (head, tail) = pattern.split_once('/').unwrap_or((pattern, ""));
    if head == "**" {
        if tail.is_empty() {
            return true;
        }
        if path_glob(tail, value) {
            return true;
        }
        return value
            .split_once('/')
            .is_some_and(|(_, rest)| path_glob(pattern, rest));
    }
    let (part, rest) = value.split_once('/').unwrap_or((value, ""));
    glob(head, part)
        && if tail.is_empty() {
            rest.is_empty()
        } else if rest.is_empty() {
            tail.split('/').all(|part| part == "**")
        } else {
            path_glob(tail, rest)
        }
}
