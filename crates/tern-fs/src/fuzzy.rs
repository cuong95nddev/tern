//! Fuzzy matching of a query against file paths, for Go to File: the
//! query's characters in order, not necessarily together, case ignored.
//! A match in the file's name beats one spread over its folders; matches at
//! word starts and runs of matched characters score more.

/// Each matched character.
const CHAR: i32 = 16;
/// A matched character starting a word (after `/`, `_`, `-`, `.`, a space,
/// or a lowercase letter before an uppercase one).
const BOUNDARY: i32 = 8;
/// A matched character right after the previous one.
const RUN: i32 = 8;
/// Every match within the file's name.
const IN_NAME: i32 = 64;
/// The query matched from the name's first character.
const NAME_START: i32 = 16;
/// Each character skipped inside the match: a run beats scattered letters.
const GAP: i32 = 3;

/// How well `query` matches `path` (higher is better) and the byte offsets
/// of `path` it matched; `None` when it doesn't. Whitespace in the query is
/// ignored; an empty query matches everything with no offsets.
pub fn score(query: &str, path: &str) -> Option<(i32, Vec<usize>)> {
    let q: Vec<char> = query.chars().filter(|c| !c.is_whitespace()).flat_map(char::to_lowercase).collect();
    if q.is_empty() {
        return Some((0, vec![]));
    }
    let name_at = path.rfind('/').map_or(0, |i| i + 1);
    if let Some((s, at)) = match_in(&q, path, name_at) {
        let start = if at[0] == name_at { NAME_START } else { 0 };
        return Some((s + IN_NAME + start, at));
    }
    match_in(&q, path, 0)
}

/// Match `q` in `path` from byte `from`: the tightest window the query fits
/// in (found forward, then shrunk backward, as fzf does), then its score.
fn match_in(q: &[char], path: &str, from: usize) -> Option<(i32, Vec<usize>)> {
    let chars: Vec<(usize, char)> = path[from..].char_indices().map(|(i, c)| (i + from, c)).collect();
    let eq = |c: char, want: char| c.to_lowercase().eq(std::iter::once(want));
    // Forward: where the last query character is first matched.
    let mut qi = 0;
    let mut end = None;
    for (i, &(_, c)) in chars.iter().enumerate() {
        if eq(c, q[qi]) {
            qi += 1;
            if qi == q.len() {
                end = Some(i);
                break;
            }
        }
    }
    let end = end?;
    // Backward from there: the latest start that still fits the query.
    let mut qi = q.len();
    let mut start = end;
    for i in (0..=end).rev() {
        if eq(chars[i].1, q[qi - 1]) {
            qi -= 1;
            if qi == 0 {
                start = i;
                break;
            }
        }
    }
    // Forward again within the window, for the offsets and the score.
    let mut at = Vec::with_capacity(q.len());
    let mut score = 0;
    let mut last: Option<usize> = None;
    let mut qi = 0;
    for i in start..=end {
        if qi < q.len() && eq(chars[i].1, q[qi]) {
            score += CHAR;
            if is_boundary(&chars, i) {
                score += BOUNDARY;
            }
            if last == Some(i.wrapping_sub(1)) {
                score += RUN;
            }
            last = Some(i);
            at.push(chars[i].0);
            qi += 1;
        }
    }
    // Characters skipped inside the window.
    score -= GAP * (end - start + 1 - q.len()) as i32;
    Some((score, at))
}

fn is_boundary(chars: &[(usize, char)], i: usize) -> bool {
    let Some(&(_, before)) = i.checked_sub(1).and_then(|p| chars.get(p)) else { return true };
    let c = chars[i].1;
    matches!(before, '/' | '\\' | '_' | '-' | '.' | ' ') || (before.is_lowercase() && c.is_uppercase())
}

#[cfg(test)]
mod tests {
    use super::score;

    fn best<'a>(query: &str, paths: &[&'a str]) -> Vec<&'a str> {
        let mut found: Vec<(i32, &str)> = paths.iter().filter_map(|p| score(query, p).map(|(s, _)| (s, *p))).collect();
        found.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.len().cmp(&b.1.len())));
        found.into_iter().map(|(_, p)| p).collect()
    }

    #[test]
    fn matches_in_order_ignoring_case_and_spaces() {
        assert_eq!(score("wsp", "src/WorkSpace.rs").unwrap().1, [4, 8, 9]);
        assert!(score("xyz", "src/workspace.rs").is_none());
        assert!(score("sw", "ws").is_none());
        assert_eq!(score("", "a").unwrap(), (0, vec![]));
        assert!(score("src ws", "src/workspace.rs").is_some());
    }

    #[test]
    fn name_matches_come_first() {
        let paths = ["crates/tern-app/src/palette.rs", "crates/tern-app/src/panes.rs", "docs/pal/ette.md", "src/plate.rs"];
        assert_eq!(best("pal", &paths)[0], "crates/tern-app/src/palette.rs");
        // Spread over folders and name, it still matches, after the name ones.
        assert_eq!(best("tapal", &paths), ["crates/tern-app/src/palette.rs"]);
        assert_eq!(best("ete", &paths)[0], "docs/pal/ette.md");
        // Together in a name beats spread over a shorter one.
        assert_eq!(best("pal", &["pages.yml", "docs/search-palette.md"]), ["docs/search-palette.md", "pages.yml"]);
    }

    #[test]
    fn tight_and_word_start_matches_score_more() {
        let (tight, _) = score("view", "src/viewer.rs").unwrap();
        let (loose, _) = score("view", "src/vision_overview.rs").unwrap();
        assert!(tight > loose);
        // The window shrinks to the last start that fits: `ab` in `a_xab` is the run at the end.
        assert_eq!(score("ab", "a_xab").unwrap().1, [3, 4]);
        let (word, _) = score("fi", "file_icons.rs").unwrap();
        let (mid, _) = score("fi", "xfxi.rs").unwrap();
        assert!(word > mid);
    }

    #[test]
    fn offsets_are_bytes() {
        assert_eq!(score("é", "a/bé.rs").unwrap().1, [3]);
    }
}
