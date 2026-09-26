//! A small deterministic fuzzy matcher for the command palette (30) and the
//! script menu filter.
//!
//! The query is split into words; each word must match the candidate as a
//! case-insensitive subsequence, the words in order. Scoring per word:
//!
//! * `+1` per matched character;
//! * `+5` when a character directly follows the previous match
//!   (contiguous runs: `rest` in `restart` beats `r-e-s-e-t`);
//! * `+3` when a character starts a word of the candidate (after the
//!   start, a space, `-`, `_`, `.`, `/`, `(` or `:`);
//! * `-1` per skipped candidate character between matches, and before a
//!   word's first match (at most 5 per gap), so tight, early matches win.
//!
//! Every start position of a word's first character is tried and the best
//! word score kept. [`rank`] sorts by score (highest first), then shorter
//! candidates, then the original order: stable and deterministic.

/// The score of `candidate` for `query`; `None` when it does not match.
/// An empty query matches everything with score 0.
pub fn score(query: &str, candidate: &str) -> Option<i64> {
    let cand: Vec<char> = candidate.to_lowercase().chars().collect();
    let mut total = 0i64;
    let mut from = 0usize;
    for word in query.split_whitespace() {
        let w: Vec<char> = word.to_lowercase().chars().collect();
        let (s, end) = best_word(&w, &cand, from)?;
        total += s;
        from = end;
    }
    Some(total)
}

fn is_boundary(cand: &[char], i: usize) -> bool {
    i == 0 || matches!(cand[i - 1], ' ' | '-' | '_' | '.' | '/' | '(' | ':')
}

/// Best score of `word` in `cand[from..]` and the index after its last match.
fn best_word(word: &[char], cand: &[char], from: usize) -> Option<(i64, usize)> {
    let first = *word.first()?;
    let mut best: Option<(i64, usize)> = None;
    for start in from..cand.len() {
        if cand[start] != first {
            continue;
        }
        let Some((s, end)) = greedy(word, cand, start) else {
            break; // a later start cannot match either
        };
        let s = s - ((start - from) as i64).min(5);
        if best.is_none_or(|(b, _)| s > b) {
            best = Some((s, end));
        }
    }
    best
}

/// Greedy match of `word` starting with its first char at `start`.
fn greedy(word: &[char], cand: &[char], start: usize) -> Option<(i64, usize)> {
    let mut score = 0i64;
    let mut prev: Option<usize> = None;
    let mut i = start;
    for &c in word {
        let pos = (i..cand.len()).find(|&p| cand[p] == c)?;
        score += 1;
        if is_boundary(cand, pos) {
            score += 3;
        }
        match prev {
            Some(p) if pos == p + 1 => score += 5,
            Some(p) => score -= ((pos - p - 1) as i64).min(5),
            None => {}
        }
        prev = Some(pos);
        i = pos + 1;
    }
    Some((score, i))
}

/// Indices of the `candidates` matching `query`, best first (score, then
/// shorter, then original order).
pub fn rank<S: AsRef<str>>(query: &str, candidates: &[S]) -> Vec<usize> {
    let mut hits: Vec<(i64, usize, usize)> = candidates
        .iter()
        .enumerate()
        .filter_map(|(i, c)| score(query, c.as_ref()).map(|s| (s, c.as_ref().chars().count(), i)))
        .collect();
    hits.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    hits.into_iter().map(|(_, _, i)| i).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn top<'a>(q: &str, c: &[&'a str]) -> Vec<&'a str> {
        rank(q, c).into_iter().map(|i| c[i]).collect()
    }

    #[test]
    fn subsequence_in_order_case_insensitive() {
        assert!(score("rst", "restart").is_some());
        assert!(score("RST", "restart").is_some());
        assert!(score("tsr", "restart").is_none());
        assert!(score("", "anything") == Some(0));
        assert!(score("api rest", "restart api").is_none(), "words in order");
        assert!(score("rest api", "restart shop-api").is_some());
    }

    #[test]
    fn contiguous_runs_beat_scattered_matches() {
        let c = [
            "reset shop-api",
            "restart shop-api",
            "run create-test-user (shop-api)",
        ];
        assert_eq!(top("rest api", &c)[0], "restart shop-api");
        assert!(score("rest", "restart").unwrap() > score("rest", "reset").unwrap());
    }

    #[test]
    fn word_starts_and_tightness() {
        let c = ["stop shop-web", "start shop-web", "stats"];
        assert_eq!(top("sta", &c), ["stats", "start shop-web"]);
        // `pw` hits two word starts in "pause watch api".
        let c = ["open api in $EDITOR", "pause watch api"];
        assert_eq!(top("pw", &c)[0], "pause watch api");
        let c = ["view logs a", "view table"];
        assert_eq!(top("view logs", &c), ["view logs a"]);
    }

    #[test]
    fn earlier_matches_win() {
        let c = ["stop a (cascade)", "stop c (cascade)", "stop c"];
        assert_eq!(
            top("stop c", &c),
            ["stop c", "stop c (cascade)", "stop a (cascade)"]
        );
    }

    #[test]
    fn ties_keep_shorter_then_original_order() {
        let c = ["restart b", "restart a", "restart ab"];
        assert_eq!(top("restart", &c), ["restart b", "restart a", "restart ab"]);
        assert_eq!(top("", &c), ["restart b", "restart a", "restart ab"]);
    }

    #[test]
    fn best_start_position_is_used() {
        // A greedy leftmost match would take the `s` of "sort"; the best
        // start is the contiguous "seed" later on.
        assert!(score("seed", "sort by seed").unwrap() >= 4 + 15 + 3 - 5);
        let c = ["run seed-large (api)", "restart seed"];
        assert_eq!(top("run seed", &c)[0], "run seed-large (api)");
    }
}
