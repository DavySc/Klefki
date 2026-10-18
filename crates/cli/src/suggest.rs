//! "Did you mean …?" for a mistyped command or server name.

/// The candidate closest to `typed`, if it is close enough to be what was meant: at most two edits, and at most a
/// third of the longer word, so `gama` finds `gamma` but `x` does not find `y`. Case does not count as an edit.
pub fn closest<'a>(typed: &str, candidates: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    let typed_lower = typed.to_lowercase();
    candidates
        .into_iter()
        .filter(|c| *c != typed)
        .map(|c| (distance(&typed_lower, &c.to_lowercase()), c))
        .filter(|&(d, c)| {
            let longer = typed.chars().count().max(c.chars().count());
            d <= 2 && d * 3 <= longer
        })
        // On a tie, the one whose case is closest to what was typed.
        .min_by_key(|&(d, c)| (d, distance(typed, c), c))
        .map(|(_, c)| c)
}

/// Edit distance over characters, where swapping two neighbours counts as one edit (`btea` → `beta`).
fn distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut d = vec![vec![0; b.len() + 1]; a.len() + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in d[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            d[i][j] = (d[i - 1][j] + 1)
                .min(d[i][j - 1] + 1)
                .min(d[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                d[i][j] = d[i][j].min(d[i - 2][j - 2] + 1);
            }
        }
    }
    d[a.len()][b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn close_typos_are_found_and_far_ones_are_not() {
        let names = ["gamma", "alpha", "beta", "docs"];
        assert_eq!(closest("gama", names), Some("gamma"));
        assert_eq!(closest("Gamma", names), Some("gamma"));
        assert_eq!(closest("btea", names), Some("beta"));
        assert_eq!(closest("dcos", names), Some("docs"));
        assert_eq!(closest("zeta", names), Some("beta"));
        assert_eq!(closest("network", names), None);
        assert_eq!(closest("x", ["y"]), None);
        assert_eq!(closest("lst", ["list", "status"]), Some("list"));
        assert_eq!(closest("stauts", ["list", "status"]), Some("status"));
        assert_eq!(
            closest("gamma", names),
            None,
            "an exact name needs no suggestion"
        );
    }

    #[test]
    fn distance_counts_edits() {
        assert_eq!(distance("", "abc"), 3);
        assert_eq!(distance("kitten", "sitting"), 3);
        assert_eq!(distance("ab", "ab"), 0);
        assert_eq!(distance("btea", "beta"), 1);
    }
}
