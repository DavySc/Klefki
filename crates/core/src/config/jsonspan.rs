//! Finds where members and values sit in JSON text, so a change can replace a few bytes instead of rewriting a
//! large file. Callers check that the whole text is valid JSON first; the scanner still returns errors rather than
//! panicking on anything else. It recurses only into the objects `descend` accepts, one level each, and scans every
//! other value without recursion, so deep nesting cannot overflow the stack.
use std::ops::Range;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Member {
    /// The decoded key.
    pub key: String,
    /// Where the key's opening quote is.
    pub key_start: usize,
    pub value: Range<usize>,
    /// The value's own members, when the caller asked to look inside it (see [`object_with`]).
    pub object: Option<Object>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Object {
    /// The `{`.
    pub open: usize,
    /// The `}`.
    pub close: usize,
    pub members: Vec<Member>,
}

pub(crate) fn skip_ws(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && matches!(b[i], b' ' | b'\t' | b'\n' | b'\r') {
        i += 1;
    }
    i
}

/// The members of the object whose `{` is at `open`, in file order (duplicates included).
#[cfg(test)]
pub(crate) fn object_at(text: &str, open: usize) -> Result<Object, String> {
    object_with(text, open, &mut |_, _| false)
}

/// Like `object_at` (for tests), and also reads the members of nested objects for which `descend(level, key)` says so
/// (`level` 0 for this object's members). Each byte is scanned once, however deep the caller looks: a nested object
/// is read in place of being skipped.
pub(crate) fn object_with(
    text: &str,
    open: usize,
    descend: &mut dyn FnMut(usize, &str) -> bool,
) -> Result<Object, String> {
    object_level(text, open, 0, descend)
}

fn object_level(
    text: &str,
    open: usize,
    level: usize,
    descend: &mut dyn FnMut(usize, &str) -> bool,
) -> Result<Object, String> {
    let b = text.as_bytes();
    if b.get(open) != Some(&b'{') {
        return Err("expected an object".to_owned());
    }
    let mut members = Vec::new();
    let mut i = skip_ws(b, open + 1);
    if b.get(i) == Some(&b'}') {
        return Ok(Object {
            open,
            close: i,
            members,
        });
    }
    loop {
        if b.get(i) != Some(&b'"') {
            return Err("expected a key".to_owned());
        }
        let key_end = string_end(b, i)?;
        let key: String =
            serde_json::from_str(&text[i..key_end]).map_err(|_| "a key is not valid".to_owned())?;
        let colon = skip_ws(b, key_end);
        if b.get(colon) != Some(&b':') {
            return Err("expected ':'".to_owned());
        }
        let start = skip_ws(b, colon + 1);
        let (end, object) = if b.get(start) == Some(&b'{') && descend(level, &key) {
            let inner = object_level(text, start, level + 1, descend)?;
            (inner.close + 1, Some(inner))
        } else {
            (value_end(b, start)?, None)
        };
        members.push(Member {
            key,
            key_start: i,
            value: start..end,
            object,
        });
        i = skip_ws(b, end);
        match b.get(i) {
            Some(b',') => i = skip_ws(b, i + 1),
            Some(b'}') => {
                return Ok(Object {
                    open,
                    close: i,
                    members,
                });
            }
            _ => return Err("expected ',' or '}'".to_owned()),
        }
    }
}

/// Where a member's key ends: the index just past its closing quote. `member.key_start` is its opening quote.
pub(crate) fn key_end(text: &str, member: &Member) -> Result<usize, String> {
    string_end(text.as_bytes(), member.key_start)
}

/// `i` is at an opening quote; returns the index just past the closing one. Strings hold most of the bytes of a
/// large configuration (chat history), so this jumps between quotes and backslashes with `memchr`, which uses the
/// CPU's vector instructions where it can.
fn string_end(b: &[u8], i: usize) -> Result<usize, String> {
    let mut j = i + 1;
    while let Some(k) = memchr::memchr2(b'"', b'\\', &b[j.min(b.len())..]) {
        let at = j + k;
        if b[at] == b'"' {
            return Ok(at + 1);
        }
        j = at + 2;
    }
    Err("unterminated string".to_owned())
}

/// The index just past the value that starts at `i`.
fn value_end(b: &[u8], i: usize) -> Result<usize, String> {
    match b.get(i) {
        None => Err("expected a value".to_owned()),
        Some(b'"') => string_end(b, i),
        Some(b'{' | b'[') => {
            let mut depth = 0usize;
            let mut j = i;
            while j < b.len() {
                match b[j] {
                    b'"' => {
                        j = string_end(b, j)?;
                        continue;
                    }
                    b'{' | b'[' => depth += 1,
                    b'}' | b']' => {
                        depth -= 1;
                        if depth == 0 {
                            return Ok(j + 1);
                        }
                    }
                    _ => {}
                }
                j += 1;
            }
            Err("unterminated object or list".to_owned())
        }
        Some(_) => {
            let mut j = i;
            while j < b.len() && !matches!(b[j], b',' | b'}' | b']' | b' ' | b'\t' | b'\n' | b'\r')
            {
                j += 1;
            }
            if j == i {
                Err("expected a value".to_owned())
            } else {
                Ok(j)
            }
        }
    }
}

/// The whitespace before `pos` when `pos` starts a line (after indentation only), else `None`: the text there is
/// written on one line.
pub(crate) fn line_indent(text: &str, pos: usize) -> Option<&str> {
    let b = text.as_bytes();
    let mut k = pos;
    while k > 0 && matches!(b[k - 1], b' ' | b'\t') {
        k -= 1;
    }
    (k > 0 && b[k - 1] == b'\n').then(|| &text[k..pos])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn members_are_found_with_their_value_spans() {
        let text = r#" { "a" : 1 , "b\"q": {"x": [1, "]}"]}, "c": "s\\", "a": null } "#;
        let o = object_at(text, 1).unwrap();
        let got: Vec<(&str, &str)> = o
            .members
            .iter()
            .map(|m| (m.key.as_str(), &text[m.value.clone()]))
            .collect();
        assert_eq!(
            got,
            [
                ("a", "1"),
                ("b\"q", r#"{"x": [1, "]}"]}"#),
                ("c", r#""s\\""#),
                ("a", "null")
            ]
        );
        assert_eq!(&text[o.close..=o.close], "}");
    }

    #[test]
    fn empty_objects_and_escaped_keys() {
        assert!(object_at("{}", 0).unwrap().members.is_empty());
        assert!(object_at("{ \n }", 0).unwrap().members.is_empty());
        let o = object_at(r#"{"café":true}"#, 0).unwrap();
        assert_eq!(o.members[0].key, "café");
    }

    #[test]
    fn broken_text_is_an_error_not_a_panic() {
        for text in [
            "",
            "{",
            "{\"a\"",
            "{\"a\":",
            "{\"a\":1",
            "{\"a\" 1}",
            "{1:2}",
            "{\"a\":[1,2}",
            "{\"a\":\"x",
            "{\"a\":1,}",
            "{\"a\":\"\\",
            "[]",
        ] {
            assert!(object_at(text, 0).is_err(), "{text:?}");
        }
    }

    #[test]
    fn nested_objects_are_read_only_where_asked() {
        let text = r#"{"skip": {"a": {"b": 1}}, "into": {"x": {"y": [1]}, "z": 2}}"#;
        let mut asked = Vec::new();
        let o = object_with(text, 0, &mut |level, key| {
            asked.push((level, key.to_owned()));
            key == "into" || key == "x"
        })
        .unwrap();
        assert_eq!(o.members[0].object, None);
        let into = o.members[1].object.as_ref().unwrap();
        assert_eq!(into.members[0].key, "x");
        let x = into.members[0].object.as_ref().unwrap();
        assert_eq!(&text[x.members[0].value.clone()], "[1]");
        assert_eq!(
            &text[o.members[1].value.clone()],
            r#"{"x": {"y": [1]}, "z": 2}"#
        );
        // Only object values are offered, each once, with their depth.
        assert_eq!(
            asked,
            [(0, "skip".into()), (0, "into".into()), (1, "x".into())]
        );
    }

    #[test]
    fn deep_nesting_does_not_recurse() {
        let deep = format!("{{\"a\":{}{}}}", "[".repeat(200_000), "]".repeat(200_000));
        let o = object_at(&deep, 0).unwrap();
        assert_eq!(o.members[0].value.len(), 400_000);
    }

    #[test]
    fn indentation_is_read_only_at_the_start_of_a_line() {
        let text = "{\n    \"a\": 1, \"b\": 2\n}";
        assert_eq!(line_indent(text, text.find("\"a\"").unwrap()), Some("    "));
        assert_eq!(line_indent(text, text.find("\"b\"").unwrap()), None);
        assert_eq!(line_indent("\"x\"", 0), None);
    }
}
