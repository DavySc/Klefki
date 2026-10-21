//! How lists, counts, folders and sentences are put in words, one copy for the engine, the app and the CLI. Each
//! place keeps its own cut-off for a long list: the callers pass how many names they show.

/// `a`, `a and b`, `a, b and c`.
pub fn and_list<S: AsRef<str>>(items: &[S]) -> String {
    match items {
        [] => String::new(),
        [one] => one.as_ref().to_owned(),
        [rest @ .., last] => format!("{} and {}", joined(rest), last.as_ref()),
    }
}

/// Like [`and_list`] while there are at most `max` items; past that the first `named` and how many more:
/// `a, b, c and 4 more`.
pub fn and_list_upto<S: AsRef<str>>(items: &[S], max: usize, named: usize) -> String {
    if items.len() <= max {
        and_list(items)
    } else {
        and_more(&items[..named.min(items.len())], items.len())
    }
}

/// The items joined with commas while there are at most `max`; past that the first `named` and how many more:
/// `a, b, c and 4 more`.
pub fn comma_list_upto<S: AsRef<str>>(items: &[S], max: usize, named: usize) -> String {
    if items.len() <= max {
        joined(items)
    } else {
        and_more(&items[..named.min(items.len())], items.len())
    }
}

/// `shown` joined with commas, then how many of `total` it leaves out: `a, b and 3 more`; just `a, b` when it leaves
/// out none.
pub fn and_more<S: AsRef<str>>(shown: &[S], total: usize) -> String {
    let mut text = joined(shown);
    if total > shown.len() {
        text.push_str(&format!(" and {} more", total - shown.len()));
    }
    text
}

/// `1 project`, `3 projects`.
pub fn count(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

/// The last part of a project path: `C:/work/app` -> `app`. A root is named as it is written: `/`.
pub fn folder_name(path: &str) -> &str {
    let trimmed = path.trim_end_matches(['/', '\\']);
    match trimmed.rfind(['/', '\\']) {
        Some(cut) => &trimmed[cut + 1..],
        None if trimmed.is_empty() => path,
        None => trimmed,
    }
}

/// The folder a project's folder is in: `C:/work/app` -> `C:/work`. Empty for a project at the top.
pub fn parent_folder(path: &str) -> &str {
    let trimmed = path.trim_end_matches(['/', '\\']);
    match trimmed.rfind(['/', '\\']) {
        Some(cut) => &trimmed[..cut],
        None => "",
    }
}

/// `text` with its first letter made upper case, as a sentence starts.
pub fn capitalized(text: &str) -> String {
    let mut c = text.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}

/// `text` as a sentence: its first letter upper case, and a full stop at the end.
pub fn sentence(text: &str) -> String {
    let mut out = capitalized(text);
    if !out.ends_with('.') {
        out.push('.');
    }
    out
}

fn joined<S: AsRef<str>>(items: &[S]) -> String {
    items
        .iter()
        .map(AsRef::as_ref)
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("s{i}")).collect()
    }

    #[test]
    fn and_list_joins_the_last_with_and() {
        for (n, want) in [
            (0, ""),
            (1, "s0"),
            (2, "s0 and s1"),
            (3, "s0, s1 and s2"),
            (4, "s0, s1, s2 and s3"),
        ] {
            assert_eq!(and_list(&names(n)), want, "{n} names");
        }
    }

    #[test]
    fn and_list_upto_counts_past_its_maximum() {
        // (count, max, named, words): the cut-offs the callers use, each at and just past its edge.
        for (n, max, named, want) in [
            (0, 3, 3, ""),
            (3, 3, 3, "s0, s1 and s2"),
            (4, 3, 3, "s0, s1, s2 and 1 more"),
            (6, 6, 5, "s0, s1, s2, s3, s4 and s5"),
            (7, 6, 5, "s0, s1, s2, s3, s4 and 2 more"),
            (9, 6, 5, "s0, s1, s2, s3, s4 and 4 more"),
            (8, 8, 8, "s0, s1, s2, s3, s4, s5, s6 and s7"),
            (9, 8, 8, "s0, s1, s2, s3, s4, s5, s6, s7 and 1 more"),
        ] {
            assert_eq!(
                and_list_upto(&names(n), max, named),
                want,
                "{n} of {max}/{named}"
            );
        }
    }

    #[test]
    fn comma_list_upto_has_no_and_until_it_counts() {
        for (n, max, named, want) in [
            (0, 3, 3, ""),
            (1, 3, 3, "s0"),
            (3, 3, 3, "s0, s1, s2"),
            (4, 3, 3, "s0, s1, s2 and 1 more"),
            (6, 6, 5, "s0, s1, s2, s3, s4, s5"),
            (7, 6, 5, "s0, s1, s2, s3, s4 and 2 more"),
            (9, 9, 8, "s0, s1, s2, s3, s4, s5, s6, s7, s8"),
            (10, 9, 8, "s0, s1, s2, s3, s4, s5, s6, s7 and 2 more"),
        ] {
            assert_eq!(
                comma_list_upto(&names(n), max, named),
                want,
                "{n} of {max}/{named}"
            );
        }
    }

    #[test]
    fn and_more_counts_what_it_leaves_out() {
        assert_eq!(and_more(&names(2), 2), "s0, s1");
        assert_eq!(and_more(&names(2), 5), "s0, s1 and 3 more");
        assert_eq!(and_more(&names(0), 0), "");
        assert_eq!(and_more(&["a"], 2), "a and 1 more");
    }

    #[test]
    fn count_says_one_without_s() {
        assert_eq!(count(0, "project"), "0 projects");
        assert_eq!(count(1, "project"), "1 project");
        assert_eq!(count(2, "server"), "2 servers");
    }

    #[test]
    fn folder_names_come_from_any_spelling() {
        for (path, folder, parent) in [
            ("/home/me/app", "app", "/home/me"),
            ("/home/me/app/", "app", "/home/me"),
            ("C:\\work\\app", "app", "C:\\work"),
            ("C:/work/app\\", "app", "C:/work"),
            ("c:\\work\\app\\", "app", "c:\\work"),
            ("C:/", "C:", ""),
            ("\\\\nas\\share\\app", "app", "\\\\nas\\share"),
            ("/app", "app", ""),
            ("app", "app", ""),
            ("C:\\", "C:", ""),
            ("/", "/", ""),
            ("\\", "\\", ""),
            ("", "", ""),
        ] {
            assert_eq!(folder_name(path), folder, "folder of {path:?}");
            assert_eq!(parent_folder(path), parent, "parent of {path:?}");
        }
    }

    #[test]
    fn sentences_start_upper_case_and_end_with_a_full_stop() {
        assert_eq!(capitalized("the server is gone"), "The server is gone");
        assert_eq!(capitalized("über"), "Über");
        assert_eq!(capitalized(""), "");
        assert_eq!(sentence("the server is gone"), "The server is gone.");
        assert_eq!(sentence("Done."), "Done.");
        assert_eq!(sentence(""), ".");
    }
}
