use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use super::catalog::{Catalog, PropertyReader, Reader};
use super::values::{self, Located, Properties, Syntax};
use super::{Diagnostic, Node, Workspace, display_path, relative_file};

#[derive(Debug)]
enum Token<'a> {
    Word(&'a str),
    Text(String),
    Symbol(char),
    Break,
}

fn tokens(text: &str) -> Result<Vec<Token<'_>>, String> {
    let bytes = text.as_bytes();
    let mut result = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\n' {
            result.push(Token::Break);
            i += 1;
        } else if bytes[i].is_ascii_whitespace() {
            i += 1;
        } else if bytes[i..].starts_with(b"//") {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
        } else if bytes[i..].starts_with(b"/*") {
            i += 2;
            let mut depth = 1;
            while i < bytes.len() && depth > 0 {
                if bytes[i..].starts_with(b"/*") {
                    depth += 1;
                    i += 2;
                } else if bytes[i..].starts_with(b"*/") {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            if depth != 0 {
                return Err("unterminated Gradle comment".to_owned());
            }
        } else if bytes[i..].starts_with(b"$/") {
            let start = i + 2;
            let Some(end) = text[start..].find("/$") else {
                return Err("unterminated Gradle dollar-slashy string".to_owned());
            };
            result.push(Token::Text(text[start..start + end].to_owned()));
            i = start + end + 2;
        } else if bytes[i] == b'/'
            && result.last().is_some_and(|t| {
                matches!(t, Token::Symbol('~' | '=' | '(' | '[' | ',' | ':' | '!'))
                    || word(t) == Some("return")
            })
        {
            i += 1;
            let start = i;
            while i < bytes.len() && bytes[i] != b'/' {
                if bytes[i] == b'\\' {
                    i += 1;
                }
                i += 1;
            }
            if i >= bytes.len() {
                return Err("unterminated Gradle slashy string".to_owned());
            }
            result.push(Token::Text(text[start..i].to_owned()));
            i += 1;
        } else if bytes[i] == b'\'' || bytes[i] == b'"' {
            let quote = bytes[i];
            let triple = bytes[i..].starts_with(&[quote, quote, quote]);
            i += if triple { 3 } else { 1 };
            let start = i;
            while i < bytes.len() {
                if triple && bytes[i..].starts_with(&[quote, quote, quote]) {
                    break;
                }
                if !triple && bytes[i] == quote {
                    break;
                }
                if !triple && bytes[i] == b'\\' {
                    i += 1;
                }
                i += 1;
            }
            if i >= bytes.len() {
                return Err("unterminated Gradle string".to_owned());
            }
            let value = &text[start..i];
            result.push(Token::Text(
                value
                    .replace("\\'", "'")
                    .replace("\\\"", "\"")
                    .replace("\\\\", "\\"),
            ));
            i += if triple { 3 } else { 1 };
        } else if bytes[i].is_ascii_alphanumeric() || matches!(bytes[i], b'_' | b'$') {
            let start = i;
            while i < bytes.len()
                && (bytes[i].is_ascii_alphanumeric() || matches!(bytes[i], b'_' | b'.' | b'$'))
            {
                i += 1;
            }
            result.push(Token::Word(&text[start..i]));
        } else {
            let Some(ch) = text[i..].chars().next() else {
                break;
            };
            result.push(Token::Symbol(ch));
            i += ch.len_utf8();
        }
    }
    Ok(result)
}

fn word<'a>(token: &'a Token<'_>) -> Option<&'a str> {
    match token {
        Token::Word(word) => Some(word),
        _ => None,
    }
}

fn argument(tokens: &[Token<'_>], mut index: usize) -> Option<(String, usize)> {
    while matches!(tokens.get(index), Some(Token::Symbol('(' | '=' | ':'))) {
        index += 1;
    }
    match tokens.get(index)? {
        Token::Text(text) => Some((text.clone(), index + 1)),
        Token::Word(word) if word.chars().all(|c| c.is_ascii_digit() || c == '.') => {
            Some(((*word).to_owned(), index + 1))
        }
        Token::Word(word) if word.starts_with("JavaVersion.VERSION_") => Some((
            word.trim_start_matches("JavaVersion.VERSION_")
                .replace('_', "."),
            index + 1,
        )),
        Token::Word(word)
            if word.ends_with("JavaLanguageVersion.of") || *word == "JavaVersion.toVersion" =>
        {
            argument(tokens, index + 1)
        }
        Token::Word(word) => Some((format!("${{{word}}}"), index + 1)),
        _ => None,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Scope {
    Other,
    All,
    Subprojects,
    Plugins,
    Dependencies,
    Management,
    Buildscript,
    Java,
    Extra,
}

struct Declaration {
    value: Located,
    shared: bool,
    root: bool,
}
struct Plugin {
    name: String,
    version: Option<Located>,
    active: bool,
    shared: bool,
    root: bool,
}
struct Dependency {
    value: Located,
    shared: bool,
    root: bool,
}

#[derive(Default)]
struct Script {
    properties: BTreeMap<String, Declaration>,
    plugins: Vec<Plugin>,
    dependencies: Vec<Dependency>,
    java: Vec<(String, Declaration)>,
    modules: Vec<String>,
    directories: BTreeMap<String, String>,
    project_names: BTreeMap<String, String>,
    name: Option<String>,
    dynamic_includes: bool,
}

fn script(text: &str, source: &str, catalog: &Catalog) -> Result<Script, String> {
    let tokens = tokens(text)?;
    let mut script = Script::default();
    let mut scopes = Vec::new();
    let mut lists = BTreeMap::<String, Vec<String>>::new();
    for (index, token) in tokens.iter().enumerate() {
        if matches!(token, Token::Symbol('{')) {
            let previous = tokens[..index]
                .iter()
                .rev()
                .take(32)
                .take_while(|t| !matches!(t, Token::Break | Token::Symbol('{' | '}' | ';')))
                .filter_map(word)
                .collect::<Vec<_>>();
            let scope = if previous.contains(&"allprojects") {
                Scope::All
            } else if previous.contains(&"subprojects") {
                Scope::Subprojects
            } else if previous.contains(&"pluginManagement")
                || previous.contains(&"dependencyManagement")
            {
                Scope::Management
            } else if previous.contains(&"buildscript") {
                Scope::Buildscript
            } else if previous.contains(&"plugins") {
                Scope::Plugins
            } else if previous.contains(&"dependencies") {
                Scope::Dependencies
            } else if previous
                .iter()
                .any(|w| matches!(*w, "java" | "toolchain" | "JavaCompile"))
            {
                Scope::Java
            } else if previous.contains(&"ext") {
                Scope::Extra
            } else {
                Scope::Other
            };
            scopes.push(scope);
            continue;
        }
        if matches!(token, Token::Symbol('}')) {
            scopes.pop();
            continue;
        }
        let Some(key) = word(token) else {
            continue;
        };
        let shared = scopes
            .iter()
            .any(|s| matches!(s, Scope::All | Scope::Subprojects));
        let root = !scopes.contains(&Scope::Subprojects);
        let located = |value| Located {
            value,
            source: source.to_owned(),
            conditional: false,
        };
        let scalar_scope = scopes.is_empty()
            || scopes.last().is_some_and(|s| {
                matches!(
                    s,
                    Scope::All | Scope::Subprojects | Scope::Extra | Scope::Java
                )
            });
        if matches!(tokens.get(index + 1), Some(Token::Symbol('='))) && scalar_scope {
            if matches!(tokens.get(index + 2), Some(Token::Symbol('['))) {
                let list = tokens[index + 3..]
                    .iter()
                    .take_while(|t| !matches!(t, Token::Symbol(']')))
                    .filter_map(|t| {
                        if let Token::Text(v) = t {
                            Some(v.clone())
                        } else {
                            None
                        }
                    })
                    .collect();
                lists.insert(key.to_owned(), list);
            } else if let Some((value, _)) = argument(&tokens, index + 1) {
                if key == "rootProject.name" {
                    script.name = Some(value);
                } else {
                    let declaration = Declaration {
                        value: located(value),
                        shared,
                        root,
                    };
                    if catalog.property(PropertyReader::Gradle, key).is_some() {
                        script.java.push((key.to_owned(), declaration));
                    } else {
                        script.properties.insert(key.to_owned(), declaration);
                    }
                }
            }
        }
        if catalog.property(PropertyReader::Gradle, key).is_some()
            && matches!(tokens.get(index + 1), Some(Token::Symbol('(')))
            && let Some((value, _)) = argument(&tokens, index + 1)
        {
            script.java.push((
                key.to_owned(),
                Declaration {
                    value: located(value),
                    shared,
                    root,
                },
            ));
        }
        if key == "include" || key == "includeBuild" {
            let first = if matches!(tokens.get(index + 1), Some(Token::Symbol('('))) {
                index + 2
            } else {
                index + 1
            };
            if let Some(name) = tokens
                .get(first)
                .and_then(word)
                .map(|name| name.strip_suffix(".toArray").unwrap_or(name))
                && let Some(list) = lists.get(name)
            {
                script.modules.extend(
                    list.iter()
                        .map(|v| v.trim_start_matches(':').replace(':', "/")),
                );
                continue;
            }
            let mut cursor = index + 1;
            while let Some((value, next)) = argument(&tokens, cursor) {
                if value.contains('$') {
                    script.dynamic_includes = true;
                    break;
                }
                script
                    .modules
                    .push(value.trim_start_matches(':').replace(':', "/"));
                cursor = next;
                if matches!(tokens.get(cursor), Some(Token::Symbol(','))) {
                    cursor += 1;
                } else {
                    break;
                }
            }
        }
        if (key.ends_with(".each") || key.ends_with(".forEach"))
            && let Some(list) =
                lists.get(key.trim_end_matches(".each").trim_end_matches(".forEach"))
            && tokens[index + 1..]
                .iter()
                .take(40)
                .filter_map(word)
                .any(|w| w == "include")
        {
            script.modules.extend(
                list.iter()
                    .map(|v| v.trim_start_matches(':').replace(':', "/")),
            );
        }
        if key == "project"
            && let Some((project, end)) = argument(&tokens, index + 1)
        {
            let tail = &tokens[end..tokens.len().min(end + 12)];
            if let Some(name) = tail.iter().position(|t| word(t) == Some("name"))
                && let Some((value, _)) = argument(tail, name + 1)
                && !value.contains('$')
            {
                script
                    .project_names
                    .insert(project.trim_start_matches(':').replace(':', "/"), value);
            }
            if tail
                .iter()
                .filter_map(word)
                .any(|w| w == ".projectDir" || w == "projectDir")
                && let Some(file) = tail.iter().position(|t| word(t) == Some("file"))
                && let Some((path, _)) = argument(tail, file + 1)
            {
                script
                    .directories
                    .insert(project.trim_start_matches(':').replace(':', "/"), path);
            }
        }
        let managed = scopes.contains(&Scope::Management) || scopes.contains(&Scope::Buildscript);
        if key == "id"
            && scopes.last() == Some(&Scope::Plugins)
            && let Some((name, end)) = argument(&tokens, index + 1)
        {
            let tail = &tokens[end..tokens.len().min(end + 8)];
            let version = tail
                .iter()
                .position(|t| word(t) == Some("version"))
                .and_then(|i| argument(tail, i + 1))
                .map(|(value, _)| located(value));
            let inactive = tail
                .windows(2)
                .any(|w| word(&w[0]) == Some("apply") && word(&w[1]) == Some("false"));
            script.plugins.push(Plugin {
                name,
                version,
                active: !managed && !inactive,
                shared,
                root,
            });
        }
        if key == "apply" && !managed {
            let mut cursor = index + 1;
            if matches!(tokens.get(cursor), Some(Token::Symbol('('))) {
                cursor += 1;
            }
            if word(tokens.get(cursor).unwrap_or(token)) == Some("plugin")
                && let Some((name, _)) = argument(&tokens, cursor + 1)
            {
                script.plugins.push(Plugin {
                    name,
                    version: None,
                    active: true,
                    shared,
                    root,
                });
            }
        }
        if scopes.contains(&Scope::Dependencies)
            && !managed
            && matches!(
                key,
                "implementation"
                    | "api"
                    | "compile"
                    | "compileOnly"
                    | "runtimeOnly"
                    | "runtime"
                    | "annotationProcessor"
            )
            && let Some((value, _)) = argument(&tokens, index + 1)
        {
            script.dependencies.push(Dependency {
                value: located(value),
                shared,
                root,
            });
        }
    }
    Ok(script)
}

fn build_file(path: &Path) -> bool {
    let Some(file) = path.file_name().and_then(|f| f.to_str()) else {
        return false;
    };
    let directory = path
        .parent()
        .and_then(Path::file_name)
        .and_then(|f| f.to_str())
        .unwrap_or("");
    matches!(file, "build.gradle" | "build.gradle.kts")
        || file == format!("{directory}.gradle")
        || file == format!("{directory}.gradle.kts")
}

pub(super) fn parse(nodes: &mut Vec<Node>, catalog: &Catalog, diagnostics: &mut Vec<Diagnostic>) {
    let mut scripts = BTreeMap::<PathBuf, Script>::new();
    let mut property_files = BTreeMap::<PathBuf, Properties>::new();
    let mut daemon_versions = BTreeMap::<PathBuf, Located>::new();
    for node in nodes.iter() {
        for (file, text) in &node.contents {
            let source = relative_file(&node.path, file);
            if catalog.reader(file, Reader::Gradle) {
                match script(text, &source, catalog) {
                    Ok(script) => {
                        if matches!(file.as_str(), "settings.gradle" | "settings.gradle.kts")
                            && script.dynamic_includes
                        {
                            diagnostics.push(Diagnostic { path: source.clone(), message: "dynamic Gradle include declarations are not evaluated; projects with manifests are still discovered".to_owned() });
                        }
                        scripts.insert(node.path.join(file), script);
                    }
                    Err(message) => diagnostics.push(Diagnostic {
                        path: source,
                        message,
                    }),
                }
            } else if catalog.reader(file, Reader::GradleJvm) {
                if let Some(version) = values::properties(text, &source).get("toolchainVersion") {
                    daemon_versions.insert(
                        node.path.parent().unwrap_or(Path::new("")).to_path_buf(),
                        version.clone(),
                    );
                }
            } else if catalog.reader(file, Reader::GradleProperties) {
                property_files.insert(node.path.clone(), values::properties(text, &source));
            }
        }
    }
    if scripts.is_empty() {
        return;
    }
    let settings = scripts
        .iter()
        .filter(|(path, _)| {
            path.file_name()
                .is_some_and(|f| f == "settings.gradle" || f == "settings.gradle.kts")
        })
        .map(|(path, script)| {
            (
                path.parent().unwrap_or(Path::new("")).to_path_buf(),
                (path, script),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut builds = BTreeMap::<PathBuf, Vec<(&PathBuf, &Script)>>::new();
    for (path, script) in &scripts {
        if build_file(path) {
            builds
                .entry(path.parent().unwrap_or(Path::new("")).to_path_buf())
                .or_default()
                .push((path, script));
        }
    }
    let mut declared = BTreeMap::<PathBuf, String>::new();
    let mut declared_names = BTreeMap::<PathBuf, String>::new();
    for (path, script) in settings.values() {
        let base = path.parent().unwrap_or(Path::new(""));
        for module in script.modules.iter().chain(script.project_names.keys()) {
            let directory = script.directories.get(module).unwrap_or(module);
            if let Some(directory) = values::child_file(base, directory) {
                let name = script
                    .project_names
                    .get(module)
                    .cloned()
                    .unwrap_or_else(|| module.rsplit('/').next().unwrap_or(module).to_owned());
                declared_names.insert(directory.clone(), name);
                if script.modules.contains(module) {
                    declared.insert(directory, display_path(path));
                }
            }
        }
    }
    let existing = nodes
        .iter()
        .map(|n| n.path.clone())
        .collect::<BTreeSet<_>>();
    let synthetic = declared
        .keys()
        .filter(|path| {
            !existing.contains(*path)
                && existing
                    .range((*path).clone()..)
                    .next()
                    .is_some_and(|candidate| candidate.starts_with(path))
        })
        .cloned()
        .collect::<Vec<_>>();
    for path in synthetic {
        nodes.push(Node {
            path,
            ..Node::default()
        });
    }
    for node in nodes.iter_mut() {
        let build = builds.get(&node.path).map(Vec::as_slice).unwrap_or(&[]);
        let settings_here = settings.get(&node.path);
        let module_source = declared.get(&node.path);
        if build.is_empty() && settings_here.is_none() && module_source.is_none() {
            continue;
        }
        let source = build
            .first()
            .map(|(path, _)| display_path(path))
            .or_else(|| module_source.cloned())
            .or_else(|| settings_here.map(|(path, _)| display_path(path)))
            .unwrap_or_default();
        node.packages.insert("gradle".to_owned(), source.clone());
        let root = node
            .path
            .ancestors()
            .find_map(|parent| settings.get(parent));
        let base = root
            .map(|(path, _)| path.parent().unwrap_or(Path::new("")))
            .map(Path::to_path_buf)
            .unwrap_or_else(|| node.path.clone());
        if let Some(raw) = daemon_versions.get(&base) {
            values::version(
                node,
                "java",
                raw,
                "build-toolchain",
                &Properties::new(),
                Syntax::Maven,
            );
        }
        let mut properties = Properties::new();
        for parent in node.path.ancestors().collect::<Vec<_>>().into_iter().rev() {
            if let Some(found) = property_files.get(parent) {
                properties.extend(found.iter().map(|(k, v)| (k.clone(), v.clone())));
            }
        }
        let root_build = builds.get(&base).map(Vec::as_slice).unwrap_or(&[]);
        let mut declarations = root_build
            .iter()
            .map(|(path, script)| (*path, *script, base != node.path))
            .collect::<Vec<_>>();
        if base != node.path {
            declarations.extend(build.iter().map(|(path, script)| (*path, *script, false)));
        }
        for (_, script, inherited) in &declarations {
            for (name, declaration) in &script.properties {
                if (!*inherited && declaration.root) || (*inherited && declaration.shared) {
                    properties.insert(name.clone(), declaration.value.clone());
                }
            }
        }
        let name = settings_here
            .and_then(|(_, script)| script.name.clone())
            .or_else(|| declared_names.get(&node.path).cloned())
            .or_else(|| {
                node.path
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
            });
        if let Some(name) = &name {
            node.names.insert(name.clone());
        }
        for (path, script, inherited) in declarations {
            for (kind, declaration) in &script.java {
                if ((!inherited && declaration.root) || (inherited && declaration.shared))
                    && let Some(field) = catalog.property(PropertyReader::Gradle, kind)
                {
                    field.emit(node, &declaration.value, &properties, Syntax::Maven);
                }
            }
            for plugin in &script.plugins {
                if plugin.active && ((!inherited && plugin.root) || (inherited && plugin.shared)) {
                    values::dependency(
                        node,
                        "gradle-plugin",
                        &plugin.name,
                        &display_path(path),
                        plugin.version.as_ref(),
                        &properties,
                        Syntax::Maven,
                    );
                }
            }
            for dependency in &script.dependencies {
                if !((!inherited && dependency.root) || (inherited && dependency.shared)) {
                    continue;
                }
                let Some(coordinate) =
                    values::resolve(&dependency.value, &properties, Syntax::Maven)
                else {
                    continue;
                };
                let parts = coordinate.value.split(':').collect::<Vec<_>>();
                if parts.len() < 2 || parts[0].is_empty() || parts[1].is_empty() {
                    continue;
                }
                let version = parts.get(2).map(|version| Located {
                    value: (*version).to_owned(),
                    source: coordinate.source.clone(),
                    conditional: false,
                });
                values::dependency(
                    node,
                    "maven",
                    &format!("{}:{}", parts[0], parts[1]),
                    &display_path(path),
                    version.as_ref(),
                    &properties,
                    Syntax::Maven,
                );
            }
        }
        if let Some(name) = name
            && let Some(group) = properties
                .get("group")
                .and_then(|v| values::resolve(v, &properties, Syntax::Maven))
        {
            values::identity(
                node,
                "maven",
                &format!("{}:{name}", group.value),
                &source,
                properties.get("version"),
                &properties,
                Syntax::Maven,
            );
        }
        if let Some((path, settings)) = settings_here {
            let members = settings
                .modules
                .iter()
                .map(|module| settings.directories.get(module).unwrap_or(module).clone())
                .collect();
            node.workspaces.push(Workspace {
                ecosystem: "gradle".to_owned(),
                source: display_path(path),
                members,
                exclude: Vec::new(),
            });
        }
    }
}
