//! Working / finished marks for LLM agents running inside panes.
//!
//! Grok and Claude Code publish their state as the pane's window title
//! (OSC 0 / OSC 2). libghostty already parses that into `Terminal::title`;
//! this module turns the string into a stable mark so a spinner frame
//! does not repaint the tab bar on every tick.
//!
//! Grammars, from the programs themselves:
//!
//! * Grok joins title items with ` - ` and always includes a `grok` item
//!   (`title.items` defaults to action-required, spinner, activity,
//!   session-name, grok). A turn puts a braille spinner in that string
//!   (`⠋ - reading src/main.rs - grok`); the idle title has no spinner
//!   (`Fix the auth bug - grok`, or just `grok`).
//! * Claude Code prefixes the title with `◐` / `◑` while a turn is
//!   running and `✳` while it is sitting at the prompt (`◐ Claude Code`).
//!   Older builds used a `. ` working prefix and a `* ` idle prefix.

/// What an agent inside a pane is doing, as far as its title says.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AgentActivity {
    /// A turn is in progress.
    Working,
    /// The agent is at its prompt.
    Idle,
}

/// One-column mark for a list or tab label. Empty when there is no agent.
pub fn mark(activity: Option<AgentActivity>) -> &'static str {
    match activity {
        Some(AgentActivity::Working) => "▶",
        Some(AgentActivity::Idle) => "✓",
        None => "",
    }
}

/// Columns a mark occupies in a label, including the separating space.
pub fn mark_columns(activity: Option<AgentActivity>) -> usize {
    match activity {
        Some(_) => 2,
        None => 0,
    }
}

/// `▶ name` when `activity` is set, otherwise `name` unchanged.
pub fn prefix_name(name: &str, activity: Option<AgentActivity>) -> String {
    match activity {
        Some(state) => format!("{} {name}", mark(Some(state))),
        None => name.to_string(),
    }
}

/// Classify one window title. `None` is a shell, an editor, or a title
/// these two programs do not claim.
pub fn classify_title(title: &str) -> Option<AgentActivity> {
    let title = title.trim();
    if title.is_empty() {
        return None;
    }
    classify_claude(title).or_else(|| classify_grok(title))
}

/// Any working pane wins; otherwise an idle agent pane; otherwise nothing.
pub fn rollup(states: impl IntoIterator<Item = Option<AgentActivity>>) -> Option<AgentActivity> {
    let mut idle = false;
    for state in states {
        match state {
            Some(AgentActivity::Working) => return Some(AgentActivity::Working),
            Some(AgentActivity::Idle) => idle = true,
            None => {}
        }
    }
    idle.then_some(AgentActivity::Idle)
}

fn classify_claude(title: &str) -> Option<AgentActivity> {
    match title.chars().next() {
        // Current Claude Code: half-circle spinner while the turn runs,
        // eight-spoked asterisk at the prompt.
        Some('◐' | '◑') => return Some(AgentActivity::Working),
        Some('✳') => return Some(AgentActivity::Idle),
        _ => {}
    }
    // Older builds. The dot and star prefixes are too common to trust
    // on their own, so the rest of the title has to name Claude.
    if let Some(rest) = title.strip_prefix(". ")
        && has_word(rest, "claude")
    {
        return Some(AgentActivity::Working);
    }
    if let Some(rest) = title.strip_prefix("* ")
        && has_word(rest, "claude")
    {
        return Some(AgentActivity::Idle);
    }
    if has_braille(title) && has_word(title, "claude") {
        return Some(AgentActivity::Working);
    }
    None
}

fn classify_grok(title: &str) -> Option<AgentActivity> {
    // A `grok` item (`name - grok`, or the bare label) is the identity.
    // A braille spinner anywhere in that title is a turn in progress;
    // the same title without one is the prompt. A spinner plus the word
    // "grok" buried in task text (`⠋ wire up grok`) is not this program.
    let named = title.eq_ignore_ascii_case("grok")
        || title
            .split(" - ")
            .any(|part| part.trim().eq_ignore_ascii_case("grok"));
    let collapsed = is_collapsed_grok_spinner(title);
    if !named && !collapsed {
        return None;
    }
    if has_braille(title) {
        Some(AgentActivity::Working)
    } else {
        Some(AgentActivity::Idle)
    }
}

/// `⠋ grok` — the stable label a spinner frame collapses to.
fn is_collapsed_grok_spinner(title: &str) -> bool {
    let mut parts = title.split_whitespace();
    let Some(spinner) = parts.next() else {
        return false;
    };
    let Some(name) = parts.next() else {
        return false;
    };
    parts.next().is_none()
        && name.eq_ignore_ascii_case("grok")
        && spinner
            .chars()
            .all(|c| ('\u{2800}'..='\u{28FF}').contains(&c))
        && !spinner.is_empty()
}

fn has_braille(title: &str) -> bool {
    title
        .chars()
        .any(|c| ('\u{2800}'..='\u{28FF}').contains(&c))
}

fn has_word(title: &str, word: &str) -> bool {
    title
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|part| part.eq_ignore_ascii_case(word))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grok_working_frames_are_one_state() {
        for title in [
            "⠋ - reading src/main.rs - grok",
            "⠙ - reading src/main.rs - grok",
            "⠦ - thinking - grok",
            "⠋ grok",
        ] {
            assert_eq!(
                classify_title(title),
                Some(AgentActivity::Working),
                "{title}"
            );
        }
    }

    #[test]
    fn grok_idle_titles() {
        assert_eq!(
            classify_title("Fix the auth bug - grok"),
            Some(AgentActivity::Idle)
        );
        assert_eq!(classify_title("grok"), Some(AgentActivity::Idle));
        assert_eq!(classify_title("  Grok  "), Some(AgentActivity::Idle));
    }

    #[test]
    fn shell_titles_are_not_agents() {
        for title in [
            "",
            "~/code/grok",
            "sveinn@host: ~/code/tools/xmux",
            "vim src/main.rs",
            "⠋ loading",
            "⠋ wire up grok",
            "project-grok",
        ] {
            assert_eq!(classify_title(title), None, "{title}");
        }
    }

    #[test]
    fn claude_spinner_and_prompt() {
        assert_eq!(
            classify_title("◐ Claude Code"),
            Some(AgentActivity::Working)
        );
        assert_eq!(
            classify_title("◑ rename the parser"),
            Some(AgentActivity::Working)
        );
        assert_eq!(classify_title("✳ Claude Code"), Some(AgentActivity::Idle));
        assert_eq!(classify_title("✳ fix the tests"), Some(AgentActivity::Idle));
        assert_eq!(
            classify_title(". Claude Code"),
            Some(AgentActivity::Working)
        );
        assert_eq!(classify_title("* Claude Code"), Some(AgentActivity::Idle));
    }

    #[test]
    fn legacy_dot_prefix_needs_the_name() {
        assert_eq!(classify_title(". cargo test"), None);
        assert_eq!(classify_title("* just a note"), None);
    }

    #[test]
    fn working_pane_wins_the_rollup() {
        use AgentActivity::*;
        assert_eq!(rollup([None, Some(Idle), Some(Working)]), Some(Working));
        assert_eq!(rollup([None, Some(Idle)]), Some(Idle));
        assert_eq!(rollup([None, None]), None);
    }
}
