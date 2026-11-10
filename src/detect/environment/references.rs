//! Literal search for catalog-declared environment names in source files.
//!
//! This is not a read-site scanner: only names that a platform injects or a
//! framework exposes by documented convention are recognised, by exact name or
//! by `PREFIX[A-Z0-9_]+` on identifier boundaries. Bytes are inspected for
//! ASCII identifiers only; no source text, value or context is retained.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Result, bail};
use serde::Deserialize;

use super::super::rules::FilePattern;
use super::{CodeReference, Declaration, Format, Occurrence, Origin, Scope, Semantics};

/// Occurrences listed per `(project, name)` record; `occurrenceCount` keeps the total.
pub const MAX_OCCURRENCES: usize = 20;

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(in super::super) struct Spec {
    extensions: Vec<String>,
    exclude: Vec<String>,
    patterns: Vec<PatternSpec>,
}

impl Spec {
    pub fn extend(&mut self, extra: Self) {
        self.extensions.extend(extra.extensions);
        self.exclude.extend(extra.exclude);
        self.patterns.extend(extra.patterns);
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PatternSpec {
    origin: Origin,
    provider: String,
    #[serde(default)]
    prefixes: Vec<String>,
    #[serde(default)]
    names: Vec<String>,
    #[serde(default)]
    scope: Option<Scope>,
    /// Basenames; when set, matches count only if one of them exists in the scan.
    #[serde(default)]
    signals: Vec<String>,
    /// Detection ids; when set, the owning project must have one of them.
    #[serde(default)]
    requires: Vec<String>,
    /// Primary documentation for the names/prefixes; validated, not emitted.
    docs: Vec<String>,
}

pub(in super::super) struct Catalog {
    patterns: Vec<PatternSpec>,
    extensions: BTreeSet<String>,
    exclude: Vec<FilePattern>,
    /// Exact names and prefixes map to candidate patterns in catalog order.
    names: BTreeMap<String, Vec<usize>>,
    /// Longest prefix first, so `NUXT_PUBLIC_` wins over `NUXT_`.
    prefixes: Vec<(String, Vec<usize>)>,
    signals: BTreeSet<String>,
}

/// One literal match in a scanned file.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(in super::super) struct Hit {
    pub source: String,
    pub line: usize,
    pub name: String,
}

fn upper(value: &str) -> bool {
    value.as_bytes().first().is_some_and(u8::is_ascii_uppercase)
        && value
            .bytes()
            .all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit() || ch == b'_')
}

impl Catalog {
    pub fn new(spec: Spec) -> Result<Self> {
        let mut names = BTreeMap::<String, Vec<usize>>::new();
        let mut prefixes = Vec::<(String, Vec<usize>)>::new();
        let mut signals = BTreeSet::new();
        for (index, pattern) in spec.patterns.iter().enumerate() {
            if pattern.provider.is_empty()
                || pattern.docs.iter().all(|url| !url.starts_with("https://"))
                || (pattern.names.is_empty() && pattern.prefixes.is_empty())
                || !pattern
                    .names
                    .iter()
                    .chain(&pattern.prefixes)
                    .all(|n| upper(n))
                || pattern
                    .signals
                    .iter()
                    .any(|s| s.is_empty() || s.contains(['/', '\\', '*', '?']))
            {
                bail!(
                    "invalid environment code-reference pattern: {}",
                    pattern.provider
                );
            }
            for name in &pattern.names {
                names.entry(name.clone()).or_default().push(index);
            }
            for prefix in &pattern.prefixes {
                match prefixes.iter_mut().find(|(existing, _)| existing == prefix) {
                    Some((_, candidates)) => candidates.push(index),
                    None => prefixes.push((prefix.clone(), vec![index])),
                }
            }
            signals.extend(pattern.signals.iter().cloned());
        }
        prefixes.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then(a.0.cmp(&b.0)));
        if spec
            .extensions
            .iter()
            .any(|e| e.is_empty() || e.contains(['.', '/', '\\', '*']))
            || spec
                .exclude
                .iter()
                .any(|e| e.is_empty() || e.contains(['/', '\\']))
        {
            bail!("invalid environment code-reference file pattern");
        }
        Ok(Self {
            extensions: spec.extensions.into_iter().collect(),
            exclude: spec.exclude.iter().map(|p| FilePattern::new(p)).collect(),
            patterns: spec.patterns,
            names,
            prefixes,
            signals,
        })
    }

    /// Whether a file of this basename is searched for references.
    pub fn scans(&self, file: &str) -> bool {
        !self.patterns.is_empty()
            && file.rsplit_once('.').is_some_and(|(stem, extension)| {
                !stem.is_empty() && self.extensions.contains(extension)
            })
            && !self.exclude.iter().any(|pattern| pattern.matches(file))
    }

    pub fn is_signal(&self, file: &str) -> bool {
        self.signals.contains(file)
    }

    /// Candidate patterns: exact names first, then the longest matching prefix.
    fn candidates(&self, token: &str) -> &[usize] {
        if let Some(candidates) = self.names.get(token) {
            return candidates;
        }
        self.prefixes
            .iter()
            .find(|(prefix, _)| {
                token.len() > prefix.len() && token.starts_with(prefix.as_str()) && upper(token)
            })
            .map_or(&[], |(_, candidates)| candidates)
    }

    /// Whole ASCII identifier tokens (`[A-Za-z0-9_]+`) that match a pattern.
    pub fn find(&self, source: &str, bytes: &[u8], hits: &mut Vec<Hit>) {
        let word = |ch: u8| ch.is_ascii_alphanumeric() || ch == b'_';
        let mut line = 1;
        let mut index = 0;
        while index < bytes.len() {
            let ch = bytes[index];
            if ch == b'\n' {
                line += 1;
                index += 1;
            } else if word(ch) {
                let start = index;
                while index < bytes.len() && word(bytes[index]) {
                    index += 1;
                }
                if ch.is_ascii_uppercase()
                    && let Ok(token) = std::str::from_utf8(&bytes[start..index])
                    && !self.candidates(token).is_empty()
                {
                    hits.push(Hit {
                        source: source.to_owned(),
                        line,
                        name: token.to_owned(),
                    });
                }
            } else {
                index += 1;
            }
        }
    }

    /// Builds one record per `(project, name)`, choosing the first candidate
    /// pattern whose signal and framework gates pass.
    pub fn records(
        &self,
        hits: Vec<(Option<String>, BTreeSet<String>, Hit)>,
        signals: &BTreeSet<String>,
    ) -> Vec<Declaration> {
        let mut grouped = BTreeMap::<(Option<String>, String), (usize, Vec<Occurrence>)>::new();
        for (project, detections, hit) in hits {
            let Some(&index) = self.candidates(&hit.name).iter().find(|&&index| {
                let pattern = &self.patterns[index];
                (pattern.signals.is_empty() || pattern.signals.iter().any(|s| signals.contains(s)))
                    && (pattern.requires.is_empty()
                        || pattern.requires.iter().any(|id| detections.contains(id)))
            }) else {
                continue;
            };
            let (pattern, occurrences) = grouped
                .entry((project, hit.name))
                .or_insert_with(|| (index, Vec::new()));
            *pattern = (*pattern).min(index);
            occurrences.push(Occurrence {
                source: hit.source,
                line: hit.line,
            });
        }
        grouped
            .into_iter()
            .map(|((project, name), (index, mut occurrences))| {
                occurrences.sort();
                occurrences.dedup();
                let pattern = &self.patterns[index];
                let first = occurrences[0].clone();
                let occurrence_count = occurrences.len();
                occurrences.truncate(MAX_OCCURRENCES);
                Declaration {
                    source: first.source,
                    format: Format::CodeReference,
                    location: format!("line:{}", first.line),
                    project,
                    name: Some(name),
                    state: "referenced".to_owned(),
                    reference: None,
                    semantics: Semantics {
                        scope: pattern.scope,
                        ..Semantics::default()
                    },
                    code: Some(CodeReference {
                        origin: pattern.origin,
                        provider: pattern.provider.clone(),
                        occurrences,
                        occurrence_count,
                    }),
                }
            })
            .collect()
    }
}
