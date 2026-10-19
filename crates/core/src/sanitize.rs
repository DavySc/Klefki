//! Strips terminal escapes, control characters and bidi overrides from untrusted text.

pub(crate) fn sanitize(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    while i < bytes.len() {
        // Most text is plain (printable ASCII, tab, newline) and is copied a run at a time; only the rest is looked
        // at character by character. Plain bytes are ASCII, so `i` always lands on a character boundary.
        let run = plain_run(&bytes[i..]);
        out.push_str(&input[i..i + run]);
        i += run;
        let Some(c) = input[i..].chars().next() else {
            break;
        };
        if c == '\u{1b}' {
            let mut rest = input[i + 1..].chars();
            skip_escape(&mut rest);
            i = input.len() - rest.as_str().len();
        } else {
            if !is_bad(c) {
                out.push(c);
            }
            i += c.len_utf8();
        }
    }
    out
}

/// Like [`sanitize`], for text shown on one line, such as a name or a command read from a file: line breaks, tabs
/// and the Unicode line and paragraph separators become spaces, so the text cannot draw a line of its own.
pub(crate) fn sanitize_line(input: &str) -> String {
    let mut out = sanitize(input);
    if out.contains(['\n', '\t', '\u{2028}', '\u{2029}']) {
        out = out.replace(['\n', '\t', '\u{2028}', '\u{2029}'], " ");
    }
    out
}

fn is_plain(b: u8) -> bool {
    (b' '..=b'~').contains(&b) || b == b'\n' || b == b'\t'
}

/// How many bytes at the start of `bytes` are plain. Whole 16-byte blocks are tested without an early exit inside
/// them, which the compiler turns into vector instructions.
fn plain_run(bytes: &[u8]) -> usize {
    let mut n = 0;
    for block in bytes.as_chunks::<16>().0 {
        if !block.iter().fold(true, |all, &b| all & is_plain(b)) {
            break;
        }
        n += 16;
    }
    n + bytes[n..].iter().take_while(|&&b| is_plain(b)).count()
}

/// Cuts `s` to at most `max` characters, marking the cut with an ellipsis.
/// How a name or path read from a file is shown: cleaned, and cut at 512 characters.
pub(crate) const MAX_SHOWN_CHARS: usize = 512;

pub(crate) fn shown(s: &str) -> String {
    clip(&sanitize(s), MAX_SHOWN_CHARS)
}

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
        // CSI: parameters and intermediates (0x20..=0x3F), then a final byte (0x40..=0x7E). Anything else ends it
        // there and is text again, so a sequence that never was one cannot hide what follows.
        Some('[') => loop {
            let mut ahead = chars.clone();
            match ahead.next() {
                Some('\u{20}'..='\u{3f}') => *chars = ahead,
                Some('\u{40}'..='\u{7e}') => {
                    *chars = ahead;
                    break;
                }
                _ => break,
            }
        },
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

/// A character [`sanitize`] removes (besides escape sequences): control characters other than line breaks and tabs, and
/// bidi controls.
pub(crate) fn is_bad(c: char) -> bool {
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
    fn a_csi_ends_where_its_text_starts() {
        assert_eq!(sanitize("a\u{1b}[31;1mb"), "ab");
        assert_eq!(sanitize("a\u{1b}[日本hello"), "a日本hello");
        assert_eq!(sanitize("a\u{1b}[\nb"), "a\nb");
    }

    #[test]
    fn one_line_text_keeps_no_line_breaks() {
        assert_eq!(
            super::sanitize_line("a\nb\tc\u{2028}d\u{2029}e\r\n"),
            "a b c d e "
        );
        assert_eq!(super::sanitize_line("plain"), "plain");
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

    /// The character-by-character version this module had before it copied plain runs whole; kept as an oracle.
    fn reference(input: &str) -> String {
        let mut out = String::new();
        let mut chars = input.chars();
        while let Some(c) = chars.next() {
            if c == '\u{1b}' {
                super::skip_escape(&mut chars);
            } else if !super::is_bad(c) {
                out.push(c);
            }
        }
        out
    }

    #[test]
    fn copying_plain_runs_gives_the_same_text_as_going_character_by_character() {
        let pieces = [
            "a",
            "plain text ",
            "\t",
            "\n",
            "\r",
            "\u{1b}[31m",
            "\u{1b}]0;t\u{7}",
            "\u{1b}",
            "\u{1b}[",
            "\u{1b}]x",
            "\u{1b}\\",
            "\u{7}",
            "\u{7f}",
            "\u{85}",
            "\u{9b}",
            "\u{202e}",
            "\u{2066}",
            "é",
            "日本",
            "😀",
            "~",
            &"x".repeat(15),
            &"y".repeat(16),
            &"z".repeat(33),
        ];
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        for _ in 0..20_000 {
            let mut s = String::new();
            for _ in 0..(seed % 9) {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                s.push_str(pieces[(seed % pieces.len() as u64) as usize]);
            }
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            assert_eq!(sanitize(&s), reference(&s), "{s:?}");
        }
    }
}
