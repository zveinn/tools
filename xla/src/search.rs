//! Matching the typed query against the application list.
//!
//! A launcher is judged almost entirely on whether three keystrokes put the
//! right thing on the first row, so this is a subsequence match with bonuses
//! rather than a plain `contains`: typing "gimp" should find GIMP, "calc" the
//! calculator whose command is `gnome-calculator`, and "fi de" Firefox
//! Developer Edition.
//!
//! Every candidate string is lowercased once when the application list is
//! built ([`crate::apps::Field`]), so matching itself allocates nothing.

use crate::apps::App;
use crate::history::History;

/// A character immediately after the previous match. Contiguous runs are what
/// separates "gimp" matching GIMP from it matching "Graphics Image
/// Manipulation Program".
const CONTIGUOUS: i32 = 12;
/// A character at the start of a word, which is what an acronym-style query
/// ("vsc" for "Visual Studio Code") is made of.
const BOUNDARY: i32 = 10;
/// A character matched in the middle of a word.
const SCATTERED: i32 = 2;
/// The query is a prefix of the candidate: what the user is typing is very
/// probably the start of the name they have in mind.
const PREFIX: i32 = 30;
/// The query is the whole candidate.
const EXACT: i32 = 60;
/// How much a late first match can cost. Capped so that a match near the end
/// of a long description is not scored below one that fails to match at all.
const LATE_START_CAP: usize = 10;

/// How well `needle` matches `haystack`, or `None` if it does not.
///
/// Both must already be lowercase; the caller does that once rather than per
/// comparison.
pub fn score(haystack: &str, needle: &str) -> Option<i32> {
    if needle.is_empty() {
        return Some(0);
    }

    let mut total = 0;
    // Indices are in characters rather than bytes, so "contiguous" stays
    // correct for multi-byte text without having to allocate a char vector.
    let mut chars = haystack.chars().enumerate();
    // The character just before the one being considered, for the
    // word-boundary test.
    let mut before: Option<char> = None;
    let mut previous: Option<usize> = None;
    let mut first: Option<usize> = None;

    for wanted in needle.chars() {
        let mut hit = None;
        for (index, c) in chars.by_ref() {
            if c == wanted {
                hit = Some(index);
                break;
            }
            before = Some(c);
        }
        // The needle ran past the end of the haystack: not a match at all.
        let index = hit?;

        total += if previous == Some(index.wrapping_sub(1)) {
            CONTIGUOUS
        } else if before.is_none_or(|c| !c.is_alphanumeric()) {
            BOUNDARY
        } else {
            SCATTERED
        };

        first.get_or_insert(index);
        previous = Some(index);
        before = Some(wanted);
    }

    total -= first.unwrap_or(0).min(LATE_START_CAP) as i32;
    if haystack.starts_with(needle) {
        total += PREFIX;
    }
    if haystack == needle {
        total += EXACT;
    }
    // Any match beats no match, however poor it is.
    Some(total.max(1))
}

/// The applications matching `query`, best first, as indices into `apps`.
///
/// An empty query keeps everything, which is what makes the launcher usable as
/// a plain list of what is installed. `history` is `None` when recency
/// ordering is turned off.
pub fn filter(apps: &[App], query: &str, history: Option<&History>) -> Vec<usize> {
    let needle = query.trim().to_lowercase();
    let terms: Vec<&str> = needle.split_whitespace().collect();

    let mut scored: Vec<(i32, usize)> = apps
        .iter()
        .enumerate()
        .filter_map(|(index, app)| {
            let matched = if terms.is_empty() { 0 } else { app_score(app, &terms)? };
            let bonus = history.map_or(0, |history| history.bonus(&app.id));
            Some((matched + bonus, index))
        })
        .collect();

    // A stable sort over a list that is already in name order, so applications
    // that score the same stay alphabetical instead of shuffling between
    // keystrokes.
    scored.sort_by_key(|&(score, _)| std::cmp::Reverse(score));
    scored.into_iter().map(|(_, index)| index).collect()
}

/// An application's score, or `None` unless *every* term matches something.
///
/// Requiring all of them is what makes a second word narrow the list rather
/// than widen it, which is how anyone who has typed "fire dev" expects it to
/// behave.
fn app_score(app: &App, terms: &[&str]) -> Option<i32> {
    let mut total = 0;
    for term in terms {
        // The best field wins rather than the sum of them, so an application
        // that happens to repeat a word everywhere does not float to the top.
        let best = app
            .fields
            .iter()
            .filter_map(|field| score(&field.text, term).map(|s| s * field.weight / 100))
            .max()?;
        total += best;
    }
    Some(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apps::Field;

    fn app(id: &str, name: &str, description: &str) -> App {
        App {
            id: id.to_string(),
            name: name.to_string(),
            description: description.to_string(),
            icon: None,
            exec: id.to_string(),
            terminal: false,
            path: None,
            fields: [
                Field::new(name, 100),
                Field::new(id, 60),
                Field::new(description, 30),
            ]
            .into_iter()
            .flatten()
            .collect(),
        }
    }

    /// The names of the matches, in the order they would be shown.
    fn matches(apps: &[App], query: &str) -> Vec<String> {
        filter(apps, query, None).into_iter().map(|i| apps[i].name.clone()).collect()
    }

    #[test]
    fn an_empty_needle_matches_anything() {
        assert_eq!(score("firefox", ""), Some(0));
    }

    #[test]
    fn characters_not_present_do_not_match() {
        assert_eq!(score("firefox", "z"), None);
        // Order matters: it is a subsequence match, not a bag of letters.
        assert_eq!(score("firefox", "xf"), None);
    }

    #[test]
    fn a_prefix_outscores_a_scattered_match() {
        let prefix = score("firefox", "fir").unwrap();
        let scattered = score("file roller finder", "fir").unwrap();
        assert!(prefix > scattered, "{prefix} should beat {scattered}");
    }

    #[test]
    fn an_exact_match_outscores_a_longer_prefix_match() {
        assert!(score("gimp", "gimp").unwrap() > score("gimp shortcuts", "gimp").unwrap());
    }

    #[test]
    fn word_starts_outscore_middles() {
        // "vsc" as an acronym of three words beats the same letters landing
        // inside one.
        let acronym = score("visual studio code", "vsc").unwrap();
        let inside = score("avoiding subsequence collisions", "vsc").unwrap();
        assert!(acronym > inside, "{acronym} should beat {inside}");
    }

    #[test]
    fn a_late_first_match_is_penalised_but_still_matches() {
        let early = score("terminal", "term").unwrap();
        let late = score("cosmic terminal", "term").unwrap();
        assert!(early > late);
        assert!(late > 0, "a late match is still a match");
    }

    #[test]
    fn name_matches_beat_description_matches() {
        let apps = [
            app("text-editor", "Text Editor", "Edit files"),
            app("archiver", "Archive Manager", "A text mode editor for archives"),
        ];
        assert_eq!(matches(&apps, "text")[0], "Text Editor");
    }

    #[test]
    fn every_term_has_to_match_something() {
        let apps = [
            app("firefox-dev", "Firefox Developer Edition", "Web browser"),
            app("firefox", "Firefox", "Web browser"),
        ];
        assert_eq!(matches(&apps, "fire dev"), ["Firefox Developer Edition"]);
        assert_eq!(matches(&apps, "fire").len(), 2);
        assert!(matches(&apps, "fire zzz").is_empty());
    }

    #[test]
    fn an_empty_query_keeps_everything_in_the_given_order() {
        let apps = [app("a", "Alpha", ""), app("b", "Beta", ""), app("c", "Gamma", "")];
        assert_eq!(matches(&apps, ""), ["Alpha", "Beta", "Gamma"]);
        assert_eq!(matches(&apps, "   "), ["Alpha", "Beta", "Gamma"], "whitespace only");
    }

    #[test]
    fn history_breaks_ties_without_overriding_a_better_match() {
        let apps = [app("alpha", "Alpha", ""), app("beta", "Beta", "")];
        let mut history = History::default();
        history.promote("beta");

        // With nothing typed, the most recently launched comes first.
        let ordered: Vec<_> = filter(&apps, "", Some(&history))
            .into_iter()
            .map(|i| apps[i].name.clone())
            .collect();
        assert_eq!(ordered, ["Beta", "Alpha"]);

        // But a query that clearly means the other one still wins.
        let ordered: Vec<_> = filter(&apps, "alpha", Some(&history))
            .into_iter()
            .map(|i| apps[i].name.clone())
            .collect();
        assert_eq!(ordered, ["Alpha"]);
    }

    #[test]
    fn matching_is_case_insensitive_through_the_lowercased_fields() {
        let apps = [app("gimp", "GNU Image Manipulation Program", "")];
        assert_eq!(matches(&apps, "GNU").len(), 1);
        assert_eq!(matches(&apps, "image").len(), 1);
    }
}
