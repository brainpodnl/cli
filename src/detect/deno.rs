use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use serde_json::Value;

use super::parsers::{Catalogs, Documents};
use super::{Dependency, Diagnostic, Node, Stats, Workspace, display_path, relative_file};

type Bindings = BTreeMap<String, Option<(String, Dependency)>>;

struct Context {
    source: String,
    workspaces: Vec<Workspace>,
    bindings: Bindings,
}

pub(super) fn resolve(
    root: &Path,
    nodes: &mut [Node],
    documents: &Documents,
    catalogs: &mut HashMap<PathBuf, Option<Catalogs>>,
    stats: &mut Stats,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let mut contexts = HashMap::new();
    for node in nodes.iter() {
        let Some(source) = node.packages.get("deno") else {
            continue;
        };
        let Some(file) = Path::new(source).file_name().and_then(|file| file.to_str()) else {
            continue;
        };
        let Some(document) = documents.get(&node.path).and_then(|docs| docs.get(file)) else {
            continue;
        };
        let mut bindings = Bindings::new();
        if let Some(reference) = document.get("importMap").and_then(Value::as_str) {
            if document.get("imports").is_some() || document.get("scopes").is_some() {
                diagnostics.push(Diagnostic {
                    path: source.clone(),
                    message: "importMap is exclusive with imports/scopes".to_owned(),
                });
            } else if let Some((source, map)) =
                import_map(root, &node.path, reference, stats, diagnostics)
            {
                imports(&map, &source, &mut bindings, diagnostics);
            }
        } else {
            imports(document, source, &mut bindings, diagnostics);
        }
        let workspaces = node
            .workspaces
            .iter()
            .filter(|workspace| {
                workspace.ecosystem == "deno"
                    || (workspace.ecosystem == "node"
                        && workspace.source == relative_file(&node.path, "package.json"))
            })
            .cloned()
            .collect::<Vec<_>>();
        let package = documents
            .get(&node.path)
            .and_then(|docs| docs.get("package.json"));
        let definitions = package
            .filter(|doc| doc.get("catalog").is_some() || doc.get("catalogs").is_some())
            .unwrap_or(document);
        if definitions.get("catalog").is_some() || definitions.get("catalogs").is_some() {
            let source = if std::ptr::eq(definitions, document) {
                source.clone()
            } else {
                relative_file(&node.path, "package.json")
            };
            catalogs.entry(node.path.clone()).or_insert_with(|| {
                Some(Catalogs {
                    workspaces: workspaces.clone(),
                    source,
                    catalog: definitions
                        .get("catalog")
                        .and_then(|v| serde_json::from_value(v.clone()).ok())
                        .unwrap_or_default(),
                    catalogs: definitions
                        .get("catalogs")
                        .and_then(|v| serde_json::from_value(v.clone()).ok())
                        .unwrap_or_default(),
                })
            });
        }
        contexts.insert(
            node.path.clone(),
            Context {
                source: source.clone(),
                workspaces,
                bindings,
            },
        );
    }
    if contexts.is_empty() {
        return;
    }
    for node in nodes {
        let local = contexts.get(&node.path);
        let inherited = node.path.ancestors().skip(1).find_map(|path| {
            contexts.get(path).filter(|context| {
                node.files.iter().any(|file| {
                    matches!(file.as_str(), "deno.json" | "deno.jsonc" | "package.json")
                }) && node.path.strip_prefix(path).ok().is_some_and(|relative| {
                    context
                        .workspaces
                        .iter()
                        .any(|workspace| workspace.contains(&display_path(relative)))
                })
            })
        });
        if let Some(context) = local.or(inherited) {
            node.runtimes
                .entry("deno".to_owned())
                .or_insert_with(|| context.source.clone());
        }
        let mut bindings = inherited.map_or_else(Bindings::new, |context| context.bindings.clone());
        if let Some(local) = local {
            bindings.extend(local.bindings.clone());
        }
        for (key, dependency) in bindings.into_values().flatten() {
            node.merge_dependency(key, dependency);
        }
    }
}

fn imports(
    document: &Value,
    source: &str,
    bindings: &mut Bindings,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if let Some(entries) = document.get("imports").and_then(Value::as_object) {
        for (alias, value) in entries {
            bindings.insert(
                alias.clone(),
                value
                    .as_str()
                    .and_then(|value| {
                        specifier(value).or_else(|| {
                            value.starts_with("catalog:").then_some((
                                "npm",
                                alias.as_str(),
                                Some(value),
                            ))
                        })
                    })
                    .map(|(ecosystem, name, version)| {
                        (
                            format!("{ecosystem}:{name}"),
                            Dependency {
                                version: version.map(str::to_owned),
                                source: source.to_owned(),
                                resolved: None,
                                declarations: Vec::new(),
                            },
                        )
                    }),
            );
        }
    }
    if document
        .get("scopes")
        .is_some_and(|scopes| scopes.as_object().is_none_or(|scopes| !scopes.is_empty()))
    {
        diagnostics.push(Diagnostic {
            path: source.to_owned(),
            message: "scoped import-map overrides are not evaluated".to_owned(),
        });
    }
}

fn specifier(value: &str) -> Option<(&str, &str, Option<&str>)> {
    let (ecosystem, package) = if let Some(package) = value.strip_prefix("npm:") {
        ("npm", package)
    } else if let Some(package) = value
        .strip_prefix("jsr:")
        .or_else(|| value.strip_prefix("https://jsr.io/"))
    {
        ("jsr", package)
    } else if let Some(package) = value.strip_prefix("https://deno.land/x/") {
        ("deno", package)
    } else if let Some(package) = value.strip_prefix("https://esm.sh/") {
        ("npm", package)
    } else {
        return None;
    };
    let start = if package.starts_with('@') {
        package.find('/')? + 1
    } else {
        0
    };
    let tail = &package[start..];
    let end = tail
        .find(['@', '/', '?', '#'])
        .map_or(package.len(), |index| start + index);
    let name = &package[..end];
    if name.is_empty() || name.ends_with('/') {
        return None;
    }
    let version = package[end..]
        .strip_prefix('@')
        .map(|value| value.split(['/', '?', '#']).next().unwrap_or(""));
    if version == Some("") {
        return None;
    }
    Some((ecosystem, name, version))
}

fn import_map(
    root: &Path,
    base: &Path,
    reference: &str,
    stats: &mut Stats,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<(String, Value)> {
    let loaded = (|| -> anyhow::Result<(String, Value)> {
        if reference.contains(':') || reference.contains('\\') || Path::new(reference).is_absolute()
        {
            anyhow::bail!("only local relative import maps are inspected");
        }
        let mut relative = PathBuf::new();
        for part in base.join(reference).components() {
            match part {
                Component::Normal(value) => relative.push(value),
                Component::CurDir => (),
                Component::ParentDir if relative.pop() => (),
                _ => anyhow::bail!("import map escapes detection root"),
            }
        }
        let mut path = root.to_path_buf();
        for part in &relative {
            path.push(part);
            if fs::symlink_metadata(&path)?.file_type().is_symlink() {
                anyhow::bail!("import-map symlinks are not followed");
            }
        }
        if !fs::metadata(&path)?.is_file() {
            anyhow::bail!("import map must be a regular file");
        }
        let file = File::open(&path)?;
        let mut bytes = Vec::new();
        file.take(super::MAX_FILE_BYTES + 1)
            .read_to_end(&mut bytes)?;
        stats.files_read += 1;
        stats.bytes_read += bytes.len();
        if bytes.len() > super::MAX_FILE_BYTES as usize {
            anyhow::bail!("import map exceeds 1 MiB read limit");
        }
        let text = String::from_utf8(bytes)?;
        let document = super::jsonc::parse(&text).map_err(anyhow::Error::msg)?;
        if !document.is_object() {
            anyhow::bail!("import map must be an object");
        }
        Ok((display_path(&relative), document))
    })();
    match loaded {
        Ok(result) => Some(result),
        Err(error) => {
            diagnostics.push(Diagnostic {
                path: relative_file(base, reference),
                message: format!("could not inspect import map: {error}"),
            });
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::specifier;
    #[test]
    fn registry_and_url_specifiers_use_targets_not_alias_names() {
        for (input, expected) in [
            ("npm:hono@^4/subpath", Some(("npm", "hono", Some("^4")))),
            (
                "npm:@scope/pkg@~2.1/sub",
                Some(("npm", "@scope/pkg", Some("~2.1"))),
            ),
            (
                "jsr:@fresh/core@^2.0.0",
                Some(("jsr", "@fresh/core", Some("^2.0.0"))),
            ),
            (
                "https://deno.land/x/oak@v17.0.0/mod.ts",
                Some(("deno", "oak", Some("v17.0.0"))),
            ),
            (
                "https://esm.sh/preact@10.2.0?dev",
                Some(("npm", "preact", Some("10.2.0"))),
            ),
            ("jsr:@oak/oak", Some(("jsr", "@oak/oak", None))),
            ("https://deno.land.evil/x/fresh@1.0", None),
            ("./npm:hono@4.ts", None),
            ("npm:", None),
            ("npm:@scope", None),
            ("npm:hono@", None),
        ] {
            assert_eq!(specifier(input), expected, "{input}");
        }
    }
}
