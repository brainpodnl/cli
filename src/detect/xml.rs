use std::collections::BTreeMap;

use anyhow::{Result, bail};
use xmlparser::{ElementEnd, Token, Tokenizer};

pub(super) struct Element<'a> {
    pub name: &'a str,
    pub attributes: BTreeMap<String, String>,
    pub text: String,
    prefix: &'a str,
}

pub(super) fn read(input: &str, mut visit: impl FnMut(&[Element<'_>]) -> Result<()>) -> Result<()> {
    let mut stack = Vec::<Element<'_>>::new();
    let mut roots = 0;
    for token in Tokenizer::from(input) {
        match token? {
            Token::ElementStart { prefix, local, .. } => {
                if stack.is_empty() {
                    roots += 1;
                }
                if stack.len() == 128 || roots > 1 {
                    bail!("invalid XML root or nesting exceeds 128 elements");
                }
                stack.push(Element {
                    name: local.as_str(),
                    prefix: prefix.as_str(),
                    attributes: BTreeMap::new(),
                    text: String::new(),
                });
            }
            Token::Attribute {
                prefix,
                local,
                value,
                ..
            } => {
                let Some(element) = stack.last_mut() else {
                    bail!("XML attribute outside an element");
                };
                let name = if prefix.is_empty() {
                    local.as_str().to_owned()
                } else {
                    format!("{}:{}", prefix.as_str(), local.as_str())
                };
                if element
                    .attributes
                    .insert(name, unescape(value.as_str())?)
                    .is_some()
                {
                    bail!("duplicate XML attribute");
                }
            }
            Token::ElementEnd { end, .. } => match end {
                ElementEnd::Open => validate_namespaces(&stack)?,
                ElementEnd::Empty => {
                    validate_namespaces(&stack)?;
                    visit(&stack)?;
                    stack.pop();
                }
                ElementEnd::Close(prefix, name) => {
                    if !stack
                        .last()
                        .is_some_and(|e| e.name == name.as_str() && e.prefix == prefix.as_str())
                    {
                        bail!("mismatched XML closing element");
                    }
                    visit(&stack)?;
                    stack.pop();
                }
            },
            Token::Text { text } => {
                if let Some(element) = stack.last_mut() {
                    if !text.as_str().trim().is_empty() || !element.text.is_empty() {
                        element.text.push_str(&unescape(text.as_str())?);
                    }
                } else if !text.as_str().trim().is_empty() {
                    bail!("XML text outside the root element");
                }
            }
            Token::Cdata { text, .. } => {
                let Some(element) = stack.last_mut() else {
                    bail!("CDATA outside an XML element");
                };
                element.text.push_str(text.as_str());
            }
            Token::DtdStart { .. } | Token::EmptyDtd { .. } | Token::EntityDeclaration { .. } => {
                bail!("XML DTDs and custom entities are not evaluated")
            }
            _ => (),
        }
    }
    if roots != 1 || !stack.is_empty() {
        bail!("XML document has a missing root or unclosed element");
    }
    Ok(())
}

fn validate_namespaces(stack: &[Element<'_>]) -> Result<()> {
    let Some(element) = stack.last() else {
        bail!("XML element missing");
    };
    let namespace = |prefix: &str| -> Option<&str> {
        if prefix == "xml" {
            return Some("http://www.w3.org/XML/1998/namespace");
        }
        let name = format!("xmlns:{prefix}");
        stack
            .iter()
            .rev()
            .find_map(|e| e.attributes.get(&name).map(String::as_str))
            .filter(|value| !value.is_empty())
    };
    if !element.prefix.is_empty() && namespace(element.prefix).is_none() {
        bail!("undeclared XML namespace prefix");
    }
    let mut expanded = Vec::new();
    for name in element.attributes.keys() {
        if name == "xmlns" || name.starts_with("xmlns:") {
            continue;
        }
        let (uri, local) = if let Some((prefix, local)) = name.split_once(':') {
            let Some(uri) = namespace(prefix) else {
                bail!("undeclared XML attribute namespace");
            };
            (uri, local)
        } else {
            ("", name.as_str())
        };
        if expanded.contains(&(uri, local)) {
            bail!("duplicate expanded XML attribute");
        }
        expanded.push((uri, local));
    }
    Ok(())
}

fn unescape(input: &str) -> Result<String> {
    let mut rest = input;
    let mut output = String::with_capacity(input.len());
    while let Some(index) = rest.find('&') {
        output.push_str(&rest[..index]);
        rest = &rest[index + 1..];
        let Some(end) = rest.find(';') else {
            bail!("unterminated XML entity reference");
        };
        let entity = &rest[..end];
        let ch = match entity {
            "amp" => '&',
            "lt" => '<',
            "gt" => '>',
            "quot" => '"',
            "apos" => '\'',
            _ => {
                let code = if let Some(hex) = entity.strip_prefix("#x") {
                    u32::from_str_radix(hex, 16).ok()
                } else {
                    entity
                        .strip_prefix('#')
                        .and_then(|digits| digits.parse::<u32>().ok())
                };
                let Some(ch) = code.and_then(char::from_u32).filter(|ch| matches!(u32::from(*ch), 9 | 10 | 13 | 0x20..=0xD7FF | 0xE000..=0xFFFD | 0x10000..=0x10FFFF)) else { bail!("unsupported XML entity reference: {entity}"); };
                ch
            }
        };
        output.push(ch);
        rest = &rest[end + 1..];
    }
    output.push_str(rest);
    Ok(output)
}

#[cfg(test)]
mod tests {
    use std::fs::{self, File};
    use std::hint::black_box;
    use std::io::Read;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    use anyhow::{Result, ensure};

    use super::read;
    use crate::detect::{MAX_FILE_BYTES, inventory, rules::Rules};

    fn kind(name: &str) -> Option<&'static str> {
        if name == "pom.xml" {
            Some("pom")
        } else {
            ["csproj", "props", "targets", "slnx"]
                .into_iter()
                .find(|ext| name.ends_with(&format!(".{ext}")))
        }
    }

    fn files(root: &Path) -> Result<Vec<(PathBuf, String, &'static str)>> {
        let inventory = inventory(root, Rules::builtin()?, 1)?;
        for diagnostic in &inventory.diagnostics {
            eprintln!("inventory: {}: {}", diagnostic.path, diagnostic.message);
        }
        Ok(inventory
            .nodes
            .into_iter()
            .flat_map(|node| {
                node.contents.into_iter().filter_map(move |(name, text)| {
                    kind(&name).map(|kind| (root.join(&node.path).join(name), text, kind))
                })
            })
            .collect())
    }

    fn bounded_read(path: &Path) -> Result<String> {
        let mut bytes = Vec::new();
        File::open(path)?
            .take(MAX_FILE_BYTES + 1)
            .read_to_end(&mut bytes)?;
        ensure!(bytes.len() <= MAX_FILE_BYTES as usize, "manifest too large");
        Ok(String::from_utf8(bytes)?)
    }

    fn timed(mut work: impl FnMut() -> Result<()>) -> Result<(usize, Duration)> {
        let start = Instant::now();
        let mut iterations = 0;
        loop {
            work()?;
            iterations += 1;
            if start.elapsed() >= Duration::from_millis(200) {
                return Ok((iterations, start.elapsed()));
            }
        }
    }

    fn benchmark(label: &str, files: &[(PathBuf, String, &'static str)]) -> Result<()> {
        for kind in ["csproj", "props", "targets", "slnx", "pom", "all"] {
            let selected: Vec<_> = files
                .iter()
                .filter(|(_, _, ext)| kind == "all" || *ext == kind)
                .collect();
            if selected.is_empty() {
                continue;
            }
            let bytes: usize = selected.iter().map(|(_, text, _)| text.len()).sum();
            let mut errors = 0;
            let mut elements = 0;
            for (path, text, _) in &selected {
                if let Err(error) = read(text, |stack| {
                    elements += 1;
                    black_box(stack);
                    Ok(())
                }) {
                    errors += 1;
                    if kind != "all" {
                        eprintln!("parse: {}: {error}", path.display());
                    }
                }
            }
            let (reads, read_elapsed) = timed(|| {
                for (path, _, _) in &selected {
                    black_box(bounded_read(path)?);
                }
                Ok(())
            })?;
            let (parses, parse_elapsed) = timed(|| {
                for (_, text, _) in &selected {
                    let result = read(black_box(text), |stack| {
                        black_box(stack);
                        Ok(())
                    });
                    black_box(result.is_ok());
                }
                Ok(())
            })?;
            eprintln!(
                "{label} {kind}: files={} bytes={bytes} elements={elements} errors={errors} read_us={:.2} parse_us={:.2} read_iters={reads} parse_iters={parses}",
                selected.len(),
                read_elapsed.as_secs_f64() * 1e6 / reads as f64,
                parse_elapsed.as_secs_f64() * 1e6 / parses as f64,
            );
        }
        Ok(())
    }

    #[test]
    #[ignore = "local XML I/O and parser microbenchmark; run release with --ignored --nocapture"]
    fn benchmark_xml_local() -> Result<()> {
        let root = tempfile::tempdir()?;
        let item = "<Item Include=\"src/**/*.cs\"><Value>a &amp; b</Value></Item>";
        let project = format!(
            "<Project Sdk=\"Microsoft.NET.Sdk\">{}</Project>",
            item.repeat(128)
        );
        for name in [
            "app.csproj",
            "Directory.Build.props",
            "Directory.Build.targets",
        ] {
            fs::write(root.path().join(name), &project)?;
        }
        fs::write(
            root.path().join("app.slnx"),
            "<Solution><Project Path=\"app.csproj\" /></Solution>",
        )?;
        fs::write(
            root.path().join("pom.xml"),
            format!(
                "<project xmlns=\"http://maven.apache.org/POM/4.0.0\"><modelVersion>4.0.0</modelVersion><properties>{}</properties></project>",
                "<java.version>21</java.version>".repeat(128)
            ),
        )?;
        for dir in ["target", "bin", "obj"] {
            fs::create_dir(root.path().join(dir))?;
            fs::write(root.path().join(dir).join("ignored.csproj"), &project)?;
        }
        fs::write(
            root.path().join("oversized.props"),
            vec![b' '; MAX_FILE_BYTES as usize + 1],
        )?;
        let files = files(root.path())?;
        assert_eq!(files.len(), 5);
        benchmark("local-synthetic", &files)
    }

    #[test]
    #[ignore = "set XML_BENCH_CACHE to pinned detect-corpus cache; run release with --ignored --nocapture"]
    fn benchmark_xml_corpus() -> Result<()> {
        let cache = PathBuf::from(std::env::var("XML_BENCH_CACHE")?);
        let manifest: serde_json::Value =
            serde_json::from_str(include_str!("../../tests/detect-corpus.json"))?;
        let mut all = Vec::new();
        let entries = manifest["repositories"].as_array().unwrap();
        for entry in entries {
            let repository = entry["repository"].as_str().unwrap();
            let name = repository.replace('/', "--");
            let metadata: serde_json::Value = serde_json::from_str(&fs::read_to_string(
                cache.join("metadata").join(format!("{name}.json")),
            )?)?;
            let revision = metadata.get("revision").or_else(|| metadata.get("sha"));
            ensure!(
                revision == Some(&entry["revision"]),
                "unpinned {repository}"
            );
            let selected = files(&cache.join("repos").join(name))?;
            benchmark(repository, &selected)?;
            all.extend(selected);
        }
        eprintln!("corpus repositories={}", entries.len());
        benchmark("corpus-total", &all)
    }
}
