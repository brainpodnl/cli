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
