use std::collections::{BTreeSet, HashMap};
use std::sync::OnceLock;

use anyhow::{Result, bail};
use serde::Deserialize;

use super::catalog::{Catalog, CatalogSpec};
use super::{Detection, Evidence, FrameworkRole, Node};

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
struct RuleSpec {
    id: String,
    language: String,
    kind: Kind,
    priority: i32,
    #[serde(default)]
    any: Vec<PatternSpec>,
    #[serde(default)]
    all: Vec<PatternSpec>,
    #[serde(default)]
    none: Vec<PatternSpec>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum PatternSpec {
    Atom(String),
    All(AllPattern),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AllPattern {
    all: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    #[serde(default)]
    rules: Vec<RuleSpec>,
    #[serde(default)]
    catalog: CatalogSpec,
}

#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
enum FactKind {
    File,
    Source,
    Dependency,
    Identity,
    Package,
    Workspace,
}

impl FactKind {
    fn parse(name: &str) -> Result<Self> {
        match name {
            "file" => Ok(Self::File),
            "source" => Ok(Self::Source),
            "dep" => Ok(Self::Dependency),
            "identity" => Ok(Self::Identity),
            "package" => Ok(Self::Package),
            "workspace" => Ok(Self::Workspace),
            _ => bail!("unknown detection fact: {name}"),
        }
    }
}

pub(super) enum FilePattern {
    Exact(String),
    Star { prefix: String, suffix: String },
    Glob(String),
}

impl FilePattern {
    pub fn new(value: &str) -> Self {
        if !value.contains(['*', '?']) {
            Self::Exact(value.to_owned())
        } else if !value.contains('?') && value.bytes().filter(|b| *b == b'*').count() == 1 {
            let (prefix, suffix) = value.split_once('*').unwrap_or((value, ""));
            Self::Star {
                prefix: prefix.to_owned(),
                suffix: suffix.to_owned(),
            }
        } else {
            Self::Glob(value.to_owned())
        }
    }

    pub fn matches(&self, value: &str) -> bool {
        match self {
            Self::Exact(exact) => exact == value,
            Self::Star { prefix, suffix } => {
                value.len() >= prefix.len() + suffix.len()
                    && value.starts_with(prefix)
                    && value.ends_with(suffix)
            }
            Self::Glob(pattern) => glob(pattern, value),
        }
    }
}

struct Atom {
    kind: FactKind,
    value: String,
    text: String,
    file: FilePattern,
}
struct Pattern {
    atoms: Vec<Atom>,
}
struct Rule {
    id: String,
    language: String,
    kind: Kind,
    priority: i32,
    any: Vec<Pattern>,
    all: Vec<Pattern>,
    none: Vec<Pattern>,
}

impl PatternSpec {
    fn compile(self) -> Result<Pattern> {
        let atoms = match self {
            Self::Atom(atom) => vec![atom],
            Self::All(group) => group.all,
        };
        if atoms.is_empty() {
            bail!("empty detection pattern group");
        }
        let atoms = atoms
            .into_iter()
            .map(|text| {
                let Some((kind, value)) = text.split_once(':') else {
                    bail!("invalid detection pattern: {text}");
                };
                let kind = FactKind::parse(kind)?;
                let valid = match kind {
                    FactKind::File | FactKind::Source => !value.is_empty() && !value.contains('/'),
                    FactKind::Dependency | FactKind::Identity => value
                        .split_once(':')
                        .is_some_and(|(ecosystem, name)| !ecosystem.is_empty() && !name.is_empty()),
                    FactKind::Package | FactKind::Workspace => !value.is_empty(),
                };
                if !valid {
                    bail!("invalid detection pattern: {text}");
                }
                Ok(Atom {
                    kind,
                    value: value.to_owned(),
                    file: FilePattern::new(value),
                    text,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Pattern { atoms })
    }
}

impl Pattern {
    fn matches(&self, node: &Node) -> bool {
        self.atoms.iter().all(|atom| atom.matched(node).is_some())
    }
}

pub struct Rules {
    pub catalog: Catalog,
    rules: Vec<Rule>,
    indexes: [HashMap<String, Vec<usize>>; 6],
    wildcard_files: Vec<(FilePattern, usize)>,
    file_interest: BTreeSet<String>,
    wildcard_interest: Vec<FilePattern>,
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
            let extra: Document = toml::from_str(extra)?;
            document.rules.extend(extra.rules);
            document.catalog.extend(extra.catalog);
        }
        let mut ids = BTreeSet::new();
        let mut rules = Self {
            catalog: Catalog::new(document.catalog)?,
            rules: Vec::new(),
            indexes: std::array::from_fn(|_| HashMap::new()),
            wildcard_files: Vec::new(),
            file_interest: BTreeSet::new(),
            wildcard_interest: Vec::new(),
            source_interest: BTreeSet::new(),
        };
        for spec in document.rules {
            if spec.id.is_empty() || spec.language.is_empty() || !ids.insert(spec.id.clone()) {
                bail!("empty or duplicate detection rule: {}", spec.id);
            }
            if spec.any.is_empty() && spec.all.is_empty() {
                bail!("rule {} needs an any or all pattern", spec.id);
            }
            let compile = |patterns: Vec<PatternSpec>| {
                patterns
                    .into_iter()
                    .map(PatternSpec::compile)
                    .collect::<Result<Vec<_>>>()
            };
            rules.rules.push(Rule {
                id: spec.id,
                language: spec.language,
                kind: spec.kind,
                priority: spec.priority,
                any: compile(spec.any)?,
                all: compile(spec.all)?,
                none: compile(spec.none)?,
            });
        }
        rules
            .rules
            .sort_by(|a, b| b.priority.cmp(&a.priority).then(a.id.cmp(&b.id)));
        let mut wildcards = BTreeSet::new();
        for (index, rule) in rules.rules.iter().enumerate() {
            for atom in rule
                .all
                .iter()
                .chain(&rule.any)
                .chain(&rule.none)
                .flat_map(|p| &p.atoms)
            {
                match atom.kind {
                    FactKind::Source => {
                        rules.source_interest.insert(atom.value.clone());
                    }
                    FactKind::File if matches!(atom.file, FilePattern::Exact(_)) => {
                        rules.file_interest.insert(atom.value.clone());
                    }
                    FactKind::File => {
                        wildcards.insert(atom.value.clone());
                    }
                    _ => (),
                }
            }
            for atom in rule.all.iter().chain(&rule.any).flat_map(|p| &p.atoms) {
                if atom.kind == FactKind::File && !matches!(atom.file, FilePattern::Exact(_)) {
                    rules
                        .wildcard_files
                        .push((FilePattern::new(&atom.value), index));
                } else {
                    rules.indexes[atom.kind as usize]
                        .entry(atom.value.clone())
                        .or_default()
                        .push(index);
                }
            }
        }
        rules.wildcard_interest = wildcards
            .iter()
            .map(|pattern| FilePattern::new(pattern))
            .collect();
        Ok(rules)
    }

    pub fn needs_source(&self, extension: &str) -> bool {
        self.source_interest.contains(extension)
    }
    pub fn needs_filename(&self, name: &str) -> bool {
        self.file_interest.contains(name)
            || self
                .wildcard_interest
                .iter()
                .any(|pattern| pattern.matches(name))
    }

    pub fn evaluate(&self, node: &Node) -> Vec<Detection> {
        let mut candidates = Vec::new();
        let facts = node
            .files
            .iter()
            .map(|s| (FactKind::File, s.as_str()))
            .chain(node.sources.keys().map(|s| (FactKind::Source, s.as_str())))
            .chain(
                node.dependencies
                    .keys()
                    .map(|s| (FactKind::Dependency, s.as_str())),
            )
            .chain(
                node.identities
                    .keys()
                    .map(|s| (FactKind::Identity, s.as_str())),
            )
            .chain(
                node.packages
                    .keys()
                    .map(|s| (FactKind::Package, s.as_str())),
            )
            .chain(
                node.workspaces
                    .iter()
                    .map(|w| (FactKind::Workspace, w.ecosystem.as_str())),
            );
        for (kind, value) in facts {
            if let Some(ids) = self.indexes[kind as usize].get(value) {
                candidates.extend(ids);
            }
        }
        for (pattern, index) in &self.wildcard_files {
            if node.files.iter().any(|file| pattern.matches(file)) {
                candidates.push(index);
            }
        }
        candidates.sort_unstable();
        candidates.dedup();
        candidates
            .into_iter()
            .filter_map(|&index| self.rules[index].evaluate(node))
            .collect()
    }
}

impl Rule {
    fn evaluate(&self, node: &Node) -> Option<Detection> {
        if !self.all.iter().all(|p| p.matches(node))
            || (!self.any.is_empty() && !self.any.iter().any(|p| p.matches(node)))
            || self.none.iter().any(|p| p.matches(node))
        {
            return None;
        }
        let mut evidence = Vec::new();
        let mut roles = Vec::new();
        for atom in self
            .all
            .iter()
            .chain(&self.any)
            .filter(|p| p.matches(node))
            .flat_map(|p| &p.atoms)
        {
            if let Some(found) = atom.matched(node) {
                found.evidence(node, &atom.text, &mut evidence);
                match atom.kind {
                    FactKind::Identity => roles.push(FrameworkRole::Provides),
                    FactKind::Dependency => roles.push(FrameworkRole::Uses),
                    _ => (),
                }
            }
        }
        evidence.sort();
        evidence.dedup();
        roles.sort();
        roles.dedup();
        if self.kind != Kind::Framework {
            roles.clear();
        } else if roles.is_empty() {
            roles.push(FrameworkRole::Uses);
        }
        Some(Detection {
            id: self.id.clone(),
            language: self.language.clone(),
            kind: self.kind,
            priority: self.priority,
            roles,
            evidence,
        })
    }
}

enum Matched<'a> {
    File(&'a str),
    Dependency(&'a super::Dependency),
    Workspace(&'a super::Workspace),
    Package(&'a str),
}

impl Matched<'_> {
    fn evidence(self, node: &Node, pattern: &str, evidence: &mut Vec<Evidence>) {
        let (source, value) = match self {
            Self::Package(source) => (source.to_owned(), None),
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

impl Atom {
    fn matched<'a>(&self, node: &'a Node) -> Option<Matched<'a>> {
        match self.kind {
            FactKind::File => {
                let file = if matches!(self.file, FilePattern::Exact(_)) {
                    node.files.get(&self.value)
                } else {
                    node.files.iter().find(|name| self.file.matches(name))
                };
                file.map(|name| Matched::File(name))
            }
            FactKind::Source => node
                .sources
                .get(&self.value)
                .map(|name| Matched::File(name)),
            FactKind::Dependency => node.dependencies.get(&self.value).map(Matched::Dependency),
            FactKind::Identity => node.identities.get(&self.value).map(Matched::Dependency),
            FactKind::Package => node
                .packages
                .get(&self.value)
                .map(|source| Matched::Package(source)),
            FactKind::Workspace => node
                .workspaces
                .iter()
                .find(|w| w.ecosystem == self.value)
                .map(Matched::Workspace),
        }
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
