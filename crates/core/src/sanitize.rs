//! Strips terminal escapes, control characters and bidi overrides from untrusted text.

pub(crate) fn sanitize(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            skip_escape(&mut chars);
        } else if !is_bad(c) {
            out.push(c);
        }
    }
    out
}

/// Cuts `s` to at most `max` characters, marking the cut with an ellipsis.
pub(crate) fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

fn skip_escape(chars: &mut std::str::Chars<'_>) {
    match chars.next() {
        // CSI: parameters/intermediates, then a final byte 0x40..=0x7E.
        Some('[') => {
            for c in chars.by_ref() {
                if ('\u{40}'..='\u{7e}').contains(&c) {
                    break;
                }
            }
        }
        // OSC: terminated by BEL or ST (ESC \).
        Some(']') => {
            while let Some(c) = chars.next() {
                if c == '\u{7}' {
                    break;
                }
                if c == '\u{1b}' {
                    chars.next();
                    break;
                }
            }
        }
        _ => {}
    }
}

fn is_bad(c: char) -> bool {
    let control = c.is_control() && c != '\n' && c != '\t';
    let bidi = matches!(
        c,
        '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{200e}' | '\u{200f}' | '\u{061c}'
    );
    control || bidi
}

#[cfg(test)]
mod tests {
    use super::sanitize;

    #[test]
    fn strips_ansi_csi_and_osc() {
        assert_eq!(sanitize("\u{1b}[31mred\u{1b}[0m"), "red");
        assert_eq!(sanitize("a\u{1b}]0;title\u{7}b"), "ab");
        assert_eq!(sanitize("a\u{1b}]8;;http://x\u{1b}\\b"), "ab");
    }

    #[test]
    fn strips_c0_c1_and_bidi() {
        assert_eq!(sanitize("a\u{0}b\u{7}c\u{85}d\u{9b}e"), "abcde");
        assert_eq!(sanitize("evil\u{202e}gnp.exe"), "evilgnp.exe");
        assert_eq!(sanitize("x\u{2066}y\u{2069}"), "xy");
    }

    #[test]
    fn keeps_newline_tab_and_unicode() {
        assert_eq!(sanitize("a\tb\nc é 日本"), "a\tb\nc é 日本");
    }

    #[test]
    fn clip_cuts_on_characters_not_bytes() {
        assert_eq!(super::clip("abc", 3), "abc");
        assert_eq!(super::clip("abcd", 3), "abc…");
        assert_eq!(super::clip("日本語です", 2), "日本…");
        assert_eq!(super::clip("", 0), "");
    }

    #[test]
    fn truncated_escape_does_not_panic() {
        assert_eq!(sanitize("a\u{1b}"), "a");
        assert_eq!(sanitize("a\u{1b}[31"), "a");
        assert_eq!(sanitize("a\u{1b}]0;never ends"), "a");
    }
}
