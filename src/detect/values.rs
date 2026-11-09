use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use super::{Dependency, Node, Version};

#[derive(Clone, Debug)]
pub(super) struct Located {
    pub value: String,
    pub source: String,
    pub conditional: bool,
}

pub(super) type Properties = BTreeMap<String, Located>;

#[derive(Clone, Copy)]
pub(super) enum Syntax {
    Maven,
    MsBuild,
}

pub(super) fn resolve(raw: &Located, properties: &Properties, syntax: Syntax) -> Option<Located> {
    let (prefix, suffix) = match syntax {
        Syntax::Maven => ("${", '}'),
        Syntax::MsBuild => ("$(", ')'),
    };
    let mut result = raw.clone();
    let mut seen = BTreeSet::new();
    for _ in 0..64 {
        let Some(start) = result.value.find(prefix) else {
            return Some(result);
        };
        if !seen.insert(result.value.clone()) {
            return None;
        }
        let end = result.value[start + 2..].find(suffix)? + start + 2;
        let name = &result.value[start + 2..end];
        let name = match syntax {
            Syntax::Maven => name.to_owned(),
            Syntax::MsBuild => name.to_ascii_lowercase(),
        };
        let replacement = properties.get(&name)?;
        if replacement.conditional {
            return None;
        }
        let entire = start == 0 && end + 1 == result.value.len();
        let length = result
            .value
            .len()
            .checked_sub(end + 1 - start)?
            .checked_add(replacement.value.len())?;
        if length > super::MAX_FILE_BYTES as usize {
            return None;
        }
        result
            .value
            .replace_range(start..end + 1, &replacement.value);
        if entire {
            result.source = replacement.source.clone();
        }
    }
    None
}

pub(super) fn version(
    node: &mut Node,
    language: &str,
    raw: &Located,
    kind: &str,
    properties: &Properties,
    syntax: Syntax,
) {
    let resolved = resolve(raw, properties, syntax);
    let reference = match syntax {
        Syntax::Maven => raw.value.contains("${"),
        Syntax::MsBuild => raw.value.contains("$("),
    };
    let kind = if raw.conditional {
        format!("conditional-{kind}")
    } else {
        kind.to_owned()
    };
    if reference {
        node.versions
            .entry(language.to_owned())
            .or_default()
            .push(Version {
                value: raw.value.clone(),
                source: raw.source.clone(),
                kind: "reference".to_owned(),
            });
    }
    if let Some(value) = resolved.filter(|v| !v.value.trim().is_empty()) {
        node.versions
            .entry(language.to_owned())
            .or_default()
            .push(Version {
                value: value.value,
                source: value.source,
                kind,
            });
    }
}

fn declaration(
    source: &str,
    raw: Option<&Located>,
    properties: &Properties,
    syntax: Syntax,
) -> Dependency {
    let mut dependency = Dependency {
        version: raw.filter(|v| v.source == source).map(|v| v.value.clone()),
        source: source.to_owned(),
        resolved: None,
    };
    if let Some(raw) = raw {
        let resolved = resolve(raw, properties, syntax).unwrap_or_else(|| raw.clone());
        if raw.source != source || raw.value != resolved.value {
            dependency.resolved = Some(Version {
                value: resolved.value,
                source: resolved.source,
                kind: "declaration".to_owned(),
            });
        }
    }
    dependency
}

pub(super) fn dependency(
    node: &mut Node,
    ecosystem: &str,
    name: &str,
    source: &str,
    raw: Option<&Located>,
    properties: &Properties,
    syntax: Syntax,
) {
    node.dependencies.insert(
        format!("{ecosystem}:{name}"),
        declaration(source, raw, properties, syntax),
    );
}

pub(super) fn identity(
    node: &mut Node,
    ecosystem: &str,
    name: &str,
    source: &str,
    raw: Option<&Located>,
    properties: &Properties,
    syntax: Syntax,
) {
    node.identities.insert(
        format!("{ecosystem}:{name}"),
        declaration(source, raw, properties, syntax),
    );
}

pub(super) fn relative(path: &Path) -> Option<PathBuf> {
    if path.is_absolute() {
        return None;
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                if !normalized.pop() {
                    return None;
                }
            }
            Component::CurDir => (),
            other => normalized.push(other),
        }
    }
    Some(normalized)
}

pub(super) fn child_file(base: &Path, value: &str) -> Option<PathBuf> {
    relative(&base.join(value.replace('\\', "/")))
}

pub(super) fn properties(text: &str, source: &str) -> Properties {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.starts_with(['#', '!']) {
                return None;
            }
            let (name, value) = line.split_once('=')?;
            Some((
                name.trim().to_owned(),
                Located {
                    value: value.trim().to_owned(),
                    source: source.to_owned(),
                    conditional: false,
                },
            ))
        })
        .collect()
}
