pub(super) fn parse(text: &str) -> Result<serde_json::Value, String> {
    if let Ok(value) = serde_json::from_str(text) {
        return Ok(value);
    }

    let mut bytes = text.as_bytes().to_vec();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => i = string_end(&bytes, i),
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                while i < bytes.len() && !matches!(bytes[i], b'\r' | b'\n') {
                    bytes[i] = b' ';
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                let start = i;
                i += 2;
                while i + 1 < bytes.len() && &bytes[i..i + 2] != b"*/" {
                    i += 1;
                }
                if i + 1 >= bytes.len() {
                    let line = text.as_bytes()[..start]
                        .iter()
                        .filter(|&&byte| byte == b'\n')
                        .count()
                        + 1;
                    let column = text.as_bytes()[..start]
                        .iter()
                        .rposition(|&byte| byte == b'\n')
                        .map_or(start + 1, |newline| start - newline);
                    return Err(format!(
                        "unterminated block comment at line {line} column {column}"
                    ));
                }
                i += 2;
                for byte in &mut bytes[start..i] {
                    if !matches!(*byte, b'\r' | b'\n') {
                        *byte = b' ';
                    }
                }
            }
            _ => i += 1,
        }
    }

    i = 0;
    let mut previous = None;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                i = string_end(&bytes, i);
                previous = Some(b'"');
                continue;
            }
            b',' => {
                let next = bytes[i + 1..].iter().find(|&&byte| !is_whitespace(byte));
                if matches!(next, Some(b'}' | b']'))
                    && !matches!(previous, None | Some(b'[' | b'{' | b',' | b':'))
                {
                    bytes[i] = b' ';
                }
                previous = Some(b',');
            }
            byte if !is_whitespace(byte) => previous = Some(byte),
            _ => {}
        }
        i += 1;
    }

    serde_json::from_slice(&bytes).map_err(|error| error.to_string())
}

fn string_end(bytes: &[u8], start: usize) -> usize {
    let mut i = start + 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'"' => return i + 1,
            _ => i += 1,
        }
    }
    bytes.len()
}

fn is_whitespace(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\r' | b'\n')
}

#[cfg(test)]
mod tests {
    use super::parse;
    use serde_json::json;

    #[test]
    fn parses_nested_comments_and_trailing_commas() {
        let text = r#"// config
        {
            /* Unicode: 日本語 🦀 */
            "tasks": {"dev": "deno run main.ts", /* task */},
            "values": [1, {"nested": [true, null,],},],
        } // end"#;
        assert_eq!(
            parse(text).unwrap(),
            json!({
                "tasks": {"dev": "deno run main.ts"},
                "values": [1, {"nested": [true, null]}]
            })
        );
    }

    #[test]
    fn preserves_strings_and_escapes() {
        let expected = json!({
            "url": "https://example.com/日本語/*path*/?q=//",
            "markers": "/* not a comment */ // still a string ,] ,}",
            "escaped": "a\"//b\\\"/*c*/",
            "backslash": "ends with \\",
        });
        let plain = serde_json::to_string(&expected).unwrap();
        assert_eq!(parse(&plain).unwrap(), expected);
        assert_eq!(parse(&format!("/* 🦀 */{plain}// end")).unwrap(), expected);
    }

    #[test]
    fn accepts_json_primitives_and_empty_containers() {
        for text in [
            "null",
            "true",
            "false",
            "42",
            "-1.25e2",
            r#""🦀""#,
            "[]",
            "{}",
        ] {
            let expected: serde_json::Value = serde_json::from_str(text).unwrap();
            assert_eq!(parse(text).unwrap(), expected);
            assert_eq!(
                parse(&format!("/* before */{text}// after")).unwrap(),
                expected
            );
        }
        assert_eq!(parse("[1,// comment\r\n]").unwrap(), json!([1]));
    }

    #[test]
    fn rejects_malformed_comments_and_json() {
        for text in [
            "/*",
            "/**",
            "/* no end 🦀",
            "{} /* no end",
            "/* outer /* inner */ */",
            "[,]",
            "{,}",
            "[/* comment */,]",
            "[1,,]",
            "[1,/* c */,]",
            r#"{"a":1,,}"#,
            r#"{"a":,}"#,
            "[1,}",
            "[true false,]",
            "[01,]",
            "[NaN,]",
            "[1\u{a0},]",
            "// only comment",
            "/* only */",
            r#"{"a":"unterminated //}"#,
            r#"["bad\q",]"#,
            r#"["\uD800",]"#,
            "[\"raw\nnewline\",]",
            "[\"\0\",]",
            "{} garbage",
        ] {
            assert!(parse(text).is_err(), "accepted {text:?}");
        }
    }

    #[test]
    fn preserves_diagnostic_byte_columns_and_newlines() {
        let text = "/* 🦀\r\n日本語 */\r\n[1, /*é*/ @,]";
        let normalized = "       \r\n            \r\n[1,        @ ]";
        assert_eq!(text.len(), normalized.len());
        let expected = serde_json::from_str::<serde_json::Value>(normalized)
            .unwrap_err()
            .to_string();
        assert_eq!(parse(text).unwrap_err(), expected);
    }

    #[test]
    fn input_is_utf8_and_comment_masking_keeps_it_valid() {
        assert!(String::from_utf8(vec![0xff]).is_err());
        assert_eq!(
            parse("/* é日本🦀 */[\"é日本🦀\",]").unwrap(),
            json!(["é日本🦀"])
        );
    }
}
