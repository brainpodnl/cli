use std::collections::{BTreeMap, BTreeSet, HashMap};

use anyhow::{Result, bail};
use serde::Deserialize;
use serde_json::Value;

use super::rules::{FilePattern, glob};
use super::values::{self, Located, Properties, Syntax};
use super::{Node, Version, Workspace, relative_file};

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(super) enum Reader {
    Json,
    Jsonc,
    Toml,
    Pnpm,
    Go,
    Gemfile,
    Gemspec,
    Requirements,
    Pin,
    Tools,
    Properties,
    Msbuild,
    Solution,
    Maven,
    Gradle,
    GradleProperties,
    GradleJvm,
    VersionCatalog,
}

#[derive(Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(super) enum Resolver {
    Cargo,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct CatalogSpec {
    manifests: Vec<Manifest>,
    environment: Vec<super::environment::Spec>,
    code_references: super::environment::references::Spec,
    scan: Scan,
    families: BTreeMap<String, String>,
    inheritance: Vec<Inheritance>,
    workspaces: Vec<WorkspaceMembers>,
    property_versions: Vec<PropertyVersion>,
    reference_kinds: BTreeMap<String, ReferenceKind>,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Scan {
    ignore: Vec<String>,
    prune: Vec<Prune>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Prune {
    when_files: Vec<String>,
    directories: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Inheritance {
    kind: String,
    #[serde(default)]
    boundary_files: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReferenceKind {
    pub ecosystem: String,
    #[serde(default)]
    pub catalog: bool,
    #[serde(default)]
    normalize: Normalize,
    separator: Option<char>,
}

impl ReferenceKind {
    pub fn name(&self, value: &str) -> String {
        self.normalize.name(
            self.separator
                .and_then(|separator| value.split(separator).next())
                .unwrap_or(value)
                .trim(),
        )
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkspaceMembers {
    ecosystem: String,
    #[serde(default)]
    files: Vec<String>,
    package: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub(super) enum PropertyReader {
    Msbuild,
    Maven,
    MavenCompiler,
    Gradle,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PropertyVersion {
    reader: PropertyReader,
    fields: Vec<String>,
    pub language: String,
    pub kind: String,
    split: Option<char>,
}

impl PropertyVersion {
    pub fn emit(&self, node: &mut Node, raw: &Located, properties: &Properties, syntax: Syntax) {
        for value in self.split.filter(|_| !raw.conditional).map_or_else(
            || vec![raw.value.as_str()],
            |split| raw.value.split(split).collect(),
        ) {
            if !value.trim().is_empty() {
                values::version(
                    node,
                    &self.language,
                    &Located {
                        value: value.trim().to_owned(),
                        ..raw.clone()
                    },
                    &self.kind,
                    properties,
                    syntax,
                );
            }
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Manifest {
    id: String,
    files: Vec<String>,
    #[serde(default)]
    prefer_first: bool,
    pub reader: Reader,
    pub resolver: Option<Resolver>,
    pub package: Option<String>,
    package_path: Option<String>,
    #[serde(default)]
    names: Vec<String>,
    identity: Option<Identity>,
    #[serde(default)]
    dependencies: Vec<Dependencies>,
    #[serde(default)]
    versions: Vec<VersionField>,
    #[serde(default)]
    workspaces: Vec<WorkspaceField>,
    pub workspace: Option<String>,
    pub runtime: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Identity {
    ecosystem: String,
    paths: Vec<String>,
    #[serde(default)]
    versions: Vec<String>,
    #[serde(default)]
    normalize: Normalize,
}

#[derive(Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum Normalize {
    #[default]
    None,
    Python,
    Lowercase,
}

impl Normalize {
    pub fn name(self, name: &str) -> String {
        match self {
            Self::None => name.to_owned(),
            Self::Lowercase => name.to_ascii_lowercase(),
            Self::Python => {
                let mut result = String::new();
                for ch in name.chars() {
                    if matches!(ch, '-' | '_' | '.') {
                        if !result.ends_with('-') {
                            result.push('-');
                        }
                    } else {
                        result.push(ch.to_ascii_lowercase());
                    }
                }
                result
            }
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Dependencies {
    ecosystem: String,
    paths: Vec<String>,
    #[serde(default)]
    exclude: Vec<String>,
    #[serde(default)]
    normalize: Normalize,
    #[serde(default)]
    requirements: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VersionField {
    #[serde(default)]
    language: String,
    runtime: Option<String>,
    kind: String,
    #[serde(default)]
    paths: Vec<String>,
    dependency: Option<String>,
    key: Option<String>,
    reference_prefix: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkspaceField {
    ecosystem: String,
    present: String,
    members: Vec<String>,
    #[serde(default)]
    exclude: Vec<String>,
}

pub(super) struct Catalog {
    spec: CatalogSpec,
    pub environment: super::environment::Catalog,
    pub references: super::environment::references::Catalog,
    exact: HashMap<String, Vec<usize>>,
    wildcard: Vec<(FilePattern, usize)>,
    extensions: HashMap<String, Vec<(FilePattern, usize)>>,
    fields: HashMap<PropertyReader, HashMap<String, usize>>,
    ignored: BTreeSet<String>,
    readable_extensions: BTreeSet<String>,
}

impl CatalogSpec {
    pub fn extend(&mut self, extra: Self) {
        self.manifests.extend(extra.manifests);
        self.environment.extend(extra.environment);
        self.code_references.extend(extra.code_references);
        self.scan.ignore.extend(extra.scan.ignore);
        self.scan.prune.extend(extra.scan.prune);
        self.families.extend(extra.families);
        self.inheritance.extend(extra.inheritance);
        self.workspaces.extend(extra.workspaces);
        self.property_versions.extend(extra.property_versions);
        self.reference_kinds.extend(extra.reference_kinds);
    }
}

impl Catalog {
    pub fn new(mut spec: CatalogSpec) -> Result<Self> {
        let environment = super::environment::Catalog::new(&spec.environment)?;
        let references = super::environment::references::Catalog::new(std::mem::take(
            &mut spec.code_references,
        ))?;
        let mut catalog = Self {
            environment,
            references,
            spec,
            exact: HashMap::new(),
            wildcard: Vec::new(),
            extensions: HashMap::new(),
            fields: HashMap::new(),
            ignored: BTreeSet::new(),
            readable_extensions: BTreeSet::new(),
        };
        let mut ids = BTreeSet::new();
        for (index, manifest) in catalog.spec.manifests.iter().enumerate() {
            if manifest.id.is_empty() || !ids.insert(&manifest.id) || manifest.files.is_empty() {
                bail!("empty or duplicate manifest definition: {}", manifest.id);
            }
            for pattern in &manifest.files {
                if pattern.is_empty() || pattern.contains(['/', '\\']) {
                    bail!("manifest patterns must be basenames: {pattern}");
                }
                if pattern.contains(['*', '?']) {
                    let compiled = (FilePattern::new(pattern), index);
                    if let Some((_, extension)) = pattern.rsplit_once('.')
                        && !extension.is_empty()
                        && !extension.contains(['*', '?'])
                    {
                        catalog
                            .extensions
                            .entry(extension.to_owned())
                            .or_default()
                            .push(compiled);
                    } else {
                        catalog.wildcard.push(compiled);
                    }
                } else {
                    catalog
                        .exact
                        .entry(pattern.clone())
                        .or_default()
                        .push(index);
                }
            }
            let pointers = manifest
                .names
                .iter()
                .chain(
                    manifest
                        .identity
                        .iter()
                        .flat_map(|v| v.paths.iter().chain(&v.versions)),
                )
                .chain(manifest.dependencies.iter().flat_map(|v| &v.paths))
                .chain(manifest.versions.iter().flat_map(|v| &v.paths))
                .chain(manifest.workspaces.iter().flat_map(|v| {
                    std::iter::once(&v.present)
                        .chain(&v.members)
                        .chain(&v.exclude)
                }))
                .chain(manifest.package_path.iter());
            for path in pointers {
                if !path.is_empty() && !path.starts_with('/') {
                    bail!("manifest selectors must be JSON pointers: {path}");
                }
            }
            for version in &manifest.versions {
                if version.language.is_empty()
                    == version
                        .runtime
                        .as_ref()
                        .is_none_or(|runtime| runtime.is_empty())
                    || version.kind.is_empty()
                {
                    bail!(
                        "manifest version fields need exactly one language/runtime target and a kind"
                    );
                }
                let valid_source = match manifest.reader {
                    Reader::Json | Reader::Jsonc | Reader::Toml => {
                        !version.paths.is_empty() || version.dependency.is_some()
                    }
                    Reader::Pin => {
                        version.key.is_none()
                            && version.dependency.is_none()
                            && version.paths.is_empty()
                    }
                    Reader::Tools | Reader::Properties => {
                        version.key.as_ref().is_some_and(|key| !key.is_empty())
                    }
                    _ => false,
                };
                if !valid_source {
                    bail!(
                        "version field has no applicable value source in {}",
                        manifest.id
                    );
                }
            }
        }
        for (file, indexes) in &catalog.exact {
            let first = catalog.spec.manifests[indexes[0]].reader;
            if indexes
                .iter()
                .any(|index| catalog.spec.manifests[*index].reader != first)
                || catalog
                    .matching(file)
                    .any(|manifest| manifest.reader != first)
            {
                bail!("conflicting manifest readers for {file}");
            }
        }
        for (index, field) in catalog.spec.property_versions.iter().enumerate() {
            if field.fields.is_empty() || field.language.is_empty() || field.kind.is_empty() {
                bail!("incomplete property version definition");
            }
            for name in &field.fields {
                if catalog
                    .fields
                    .entry(field.reader)
                    .or_default()
                    .insert(name.clone(), index)
                    .is_some()
                {
                    bail!(
                        "duplicate property version field: {:?}:{name}",
                        field.reader
                    );
                }
            }
        }
        catalog
            .readable_extensions
            .extend(catalog.exact.keys().map(|file| {
                file.rsplit_once('.')
                    .map_or("", |(_, extension)| extension)
                    .to_owned()
            }));
        catalog
            .readable_extensions
            .extend(catalog.extensions.keys().cloned());
        catalog
            .ignored
            .extend(catalog.spec.scan.ignore.iter().cloned());
        Ok(catalog)
    }

    pub fn matching<'a>(&'a self, file: &'a str) -> impl Iterator<Item = &'a Manifest> + 'a {
        let extension = file
            .rsplit_once('.')
            .map(|(_, extension)| extension)
            .unwrap_or("");
        self.exact
            .get(file)
            .into_iter()
            .flatten()
            .copied()
            .chain(
                self.extensions
                    .get(extension)
                    .into_iter()
                    .flatten()
                    .chain(&self.wildcard)
                    .filter(move |(pattern, _)| pattern.matches(file))
                    .map(|(_, index)| *index),
            )
            .map(|index| &self.spec.manifests[index])
    }

    pub fn readable(&self, file: &str) -> bool {
        let extension = file.rsplit_once('.').map_or("", |(_, extension)| extension);
        (!self.wildcard.is_empty() || self.readable_extensions.contains(extension))
            && self.matching(file).next().is_some()
    }
    pub fn reader(&self, file: &str, reader: Reader) -> bool {
        self.matching(file)
            .any(|manifest| manifest.reader == reader)
    }
    pub fn ignored(&self, directory: &str) -> bool {
        self.ignored.contains(directory)
    }
    pub fn pruned(&self, node: &Node, directory: &str) -> bool {
        self.spec.scan.prune.iter().any(|rule| {
            rule.directories
                .iter()
                .any(|name| name.eq_ignore_ascii_case(directory))
                && node
                    .files
                    .iter()
                    .any(|file| rule.when_files.iter().any(|pattern| glob(pattern, file)))
        })
    }
    pub fn family<'a>(&'a self, language: &'a str) -> &'a str {
        self.spec
            .families
            .get(language)
            .map_or(language, String::as_str)
    }
    pub fn inherited(&self, kind: &str) -> bool {
        self.spec.inheritance.iter().any(|rule| rule.kind == kind)
    }
    pub fn boundaries<'a>(&'a self, node: &'a Node) -> impl Iterator<Item = &'a str> {
        self.spec
            .inheritance
            .iter()
            .filter(|rule| {
                node.files.iter().any(|file| {
                    rule.boundary_files
                        .iter()
                        .any(|pattern| glob(pattern, file))
                })
            })
            .map(|rule| rule.kind.as_str())
    }
    pub fn workspace_candidates<'a>(&'a self, node: &'a Node) -> impl Iterator<Item = &'a str> {
        self.spec
            .workspaces
            .iter()
            .filter(|rule| {
                rule.package
                    .as_ref()
                    .is_some_and(|package| node.packages.contains_key(package))
                    || node
                        .files
                        .iter()
                        .any(|file| rule.files.iter().any(|pattern| glob(pattern, file)))
            })
            .map(|rule| rule.ecosystem.as_str())
    }
    pub fn property(&self, reader: PropertyReader, name: &str) -> Option<&PropertyVersion> {
        self.fields
            .get(&reader)
            .and_then(|fields| fields.get(name))
            .map(|index| &self.spec.property_versions[*index])
    }
    pub fn properties<'a>(
        &'a self,
        reader: PropertyReader,
    ) -> impl Iterator<Item = &'a PropertyVersion> + 'a {
        self.spec
            .property_versions
            .iter()
            .filter(move |field| field.reader == reader)
    }
    pub fn property_values<'a>(
        &'a self,
        reader: PropertyReader,
        values: &'a Properties,
    ) -> impl Iterator<Item = (&'a PropertyVersion, &'a Located)> {
        self.properties(reader).flat_map(move |field| {
            field
                .fields
                .iter()
                .filter_map(move |key| values.get(key).map(|value| (field, value)))
        })
    }
    pub fn reference(&self, kind: &str) -> Option<&ReferenceKind> {
        self.spec.reference_kinds.get(kind)
    }
}

fn select<'a>(document: &'a Value, paths: &[String]) -> Option<&'a Value> {
    paths.iter().find_map(|path| document.pointer(path))
}
fn scalar(value: &Value) -> Option<&str> {
    value
        .as_str()
        .or_else(|| value.get("version").and_then(Value::as_str))
}
fn selected_scalar<'a>(document: &'a Value, paths: &[String]) -> Option<&'a str> {
    select(document, paths).and_then(scalar)
}
fn strings(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}

pub(super) fn workspace_patterns(patterns: Vec<String>) -> (Vec<String>, Vec<String>) {
    let mut members = Vec::new();
    let mut exclude = Vec::new();
    for pattern in patterns {
        if let Some(pattern) = pattern.strip_prefix('!') {
            exclude.push(pattern.to_owned());
        } else {
            members.push(pattern);
        }
    }
    (members, exclude)
}

impl Manifest {
    pub fn preferred(&self, file: &str, files: &BTreeSet<String>) -> bool {
        if self.prefer_first {
            self.files
                .iter()
                .find(|name| files.contains(*name))
                .is_none_or(|name| name == file)
        } else {
            true
        }
    }
    pub fn apply(&self, node: &mut Node, file: &str, document: &Value) {
        if let Some(runtime) = &self.runtime {
            node.runtimes
                .insert(runtime.clone(), relative_file(&node.path, file));
        }
        if let Some(name) = selected_scalar(document, &self.names) {
            node.names.insert(name.to_owned());
        }
        if let Some(identity) = &self.identity
            && let Some(name) = selected_scalar(document, &identity.paths)
        {
            node.names.insert(name.to_owned());
            node.identity(
                &identity.ecosystem,
                &identity.normalize.name(name),
                selected_scalar(document, &identity.versions),
                file,
            );
        }
        if let Some(package) = &self.package
            && self
                .package_path
                .as_ref()
                .is_none_or(|path| document.pointer(path).is_some_and(Value::is_object))
        {
            node.packages
                .insert(package.clone(), relative_file(&node.path, file));
        }
        for dependencies in &self.dependencies {
            for path in &dependencies.paths {
                if dependencies.requirements {
                    for requirement in document
                        .pointer(path)
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                    {
                        if let Some((name, version)) =
                            super::parsers::requirement_parts(requirement)
                        {
                            node.dependency(
                                &dependencies.ecosystem,
                                &dependencies.normalize.name(name),
                                version,
                                file,
                            );
                        }
                    }
                } else if let Some(entries) = document.pointer(path).and_then(Value::as_object) {
                    for (name, value) in entries {
                        if !dependencies.exclude.contains(name) {
                            node.dependency(
                                &dependencies.ecosystem,
                                &dependencies.normalize.name(name),
                                scalar(value),
                                file,
                            );
                        }
                    }
                }
            }
        }
        for workspace in &self.workspaces {
            if document.pointer(&workspace.present).is_none() {
                continue;
            }
            let (members, mut exclude) =
                workspace_patterns(strings(select(document, &workspace.members)));
            exclude.extend(strings(select(document, &workspace.exclude)));
            node.workspaces.push(Workspace {
                ecosystem: workspace.ecosystem.clone(),
                source: relative_file(&node.path, file),
                members,
                exclude,
            });
        }
        self.versions(node, file, Some(document), None, false);
    }

    pub fn versions(
        &self,
        node: &mut Node,
        file: &str,
        document: Option<&Value>,
        text: Option<&str>,
        dependencies: bool,
    ) {
        for field in &self.versions {
            if dependencies != field.dependency.is_some() {
                continue;
            }
            if let Some(key) = &field.dependency {
                if let Some(dependency) = node.dependencies.get(key) {
                    let mut versions = dependency
                        .declarations
                        .iter()
                        .map(|version| Version {
                            kind: if field
                                .reference_prefix
                                .as_ref()
                                .is_some_and(|prefix| version.value.starts_with(prefix))
                            {
                                "reference".to_owned()
                            } else {
                                field.kind.clone()
                            },
                            ..version.clone()
                        })
                        .collect::<Vec<_>>();
                    if let Some(raw) = &dependency.version {
                        let kind = if field
                            .reference_prefix
                            .as_ref()
                            .is_some_and(|prefix| raw.starts_with(prefix))
                        {
                            "reference"
                        } else {
                            &field.kind
                        };
                        versions.push(Version {
                            value: raw.clone(),
                            source: dependency.source.clone(),
                            kind: kind.to_owned(),
                        });
                    }
                    if let Some(resolved) = &dependency.resolved {
                        versions.push(Version {
                            kind: field.kind.clone(),
                            ..resolved.clone()
                        });
                    }
                    field
                        .target(node)
                        .entry(field.name().to_owned())
                        .or_default()
                        .extend(versions);
                }
            } else if let Some(value) = document
                .and_then(|doc| selected_scalar(doc, &field.paths))
                .or(text)
            {
                field.emit(node, value, file);
            }
        }
    }

    pub fn text_versions(&self, node: &mut Node, file: &str, text: &str) {
        match self.reader {
            Reader::Pin => {
                let value = text
                    .lines()
                    .filter(|line| !line.trim().starts_with('#'))
                    .collect::<Vec<_>>()
                    .join(" ");
                self.versions(node, file, None, Some(&value), false);
            }
            Reader::Tools => {
                for line in text.lines() {
                    let mut words = line.split('#').next().unwrap_or("").split_whitespace();
                    let Some(key) = words.next() else {
                        continue;
                    };
                    let value = words.collect::<Vec<_>>().join(" ");
                    for field in self
                        .versions
                        .iter()
                        .filter(|field| field.key.as_deref() == Some(key))
                    {
                        field.emit(node, &value, file);
                    }
                }
            }
            Reader::Properties => {
                let properties = values::properties(text, &relative_file(&node.path, file));
                for field in &self.versions {
                    if let Some(value) = field.key.as_ref().and_then(|key| properties.get(key)) {
                        field.emit(node, &value.value, file);
                    }
                }
            }
            _ => (),
        }
    }
}

impl VersionField {
    fn name(&self) -> &str {
        self.runtime.as_deref().unwrap_or(&self.language)
    }
    fn target<'a>(&self, node: &'a mut Node) -> &'a mut BTreeMap<String, Vec<Version>> {
        if self.runtime.is_some() {
            &mut node.runtime_versions
        } else {
            &mut node.versions
        }
    }
    fn emit(&self, node: &mut Node, value: &str, file: &str) {
        if !value.trim().is_empty() {
            let version = Version {
                value: value.trim().to_owned(),
                source: relative_file(&node.path, file),
                kind: self.kind.clone(),
            };
            self.target(node)
                .entry(self.name().to_owned())
                .or_default()
                .push(version);
        }
    }
}
