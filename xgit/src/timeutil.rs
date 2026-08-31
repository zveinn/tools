use chrono::{DateTime, Datelike, Utc};

pub fn parse_rfc3339(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

pub fn now_rfc3339() -> String {
    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Compact relative time for list rows (`now`, `12m`, `5h`, `3d`, else `YYYY-MM-DD`).
pub fn relative(stamp: &str) -> String {
    let Some(t) = parse_rfc3339(stamp) else {
        return stamp.chars().take(10).collect();
    };
    let secs = (Utc::now() - t).num_seconds();
    if secs < 0 {
        return "now".into();
    }
    if secs < 60 {
        return "now".into();
    }
    let mins = secs / 60;
    if mins < 60 {
        return format!("{mins}m");
    }
    let hours = mins / 60;
    if hours < 48 {
        return format!("{hours}h");
    }
    let days = hours / 24;
    if days < 21 {
        return format!("{days}d");
    }
    t.format("%Y-%m-%d").to_string()
}

/// Fits a list-column clock: `now`, `12m`, `5h`, `3d`, or `8/13`.
pub fn relative_short(stamp: &str) -> String {
    let Some(t) = parse_rfc3339(stamp) else {
        return stamp.chars().take(5).collect();
    };
    let secs = (Utc::now() - t).num_seconds();
    if secs < 60 {
        return "now".into();
    }
    let mins = secs / 60;
    if mins < 60 {
        return format!("{mins}m");
    }
    let hours = mins / 60;
    if hours < 48 {
        return format!("{hours}h");
    }
    let days = hours / 24;
    if days < 21 {
        return format!("{days}d");
    }
    format!("{}/{}", t.month(), t.day())
}

pub fn truncate_width(s: &str, max: usize) -> String {
    use unicode_width::UnicodeWidthStr;
    if max == 0 {
        return String::new();
    }
    if s.width() <= max {
        return s.to_string();
    }
    if max == 1 {
        return "…".into();
    }
    let mut out = String::new();
    let mut w = 0;
    for ch in s.chars() {
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if w + cw >= max {
            break;
        }
        out.push(ch);
        w += cw;
    }
    out.push('…');
    out
}

pub fn wrap_text(s: &str, width: usize) -> Vec<String> {
    use unicode_width::UnicodeWidthStr;
    if width == 0 {
        return vec![];
    }
    let mut lines = Vec::new();
    for raw in s.lines() {
        if raw.is_empty() {
            lines.push(String::new());
            continue;
        }
        let mut current = String::new();
        for word in raw.split(' ') {
            if word.width() > width {
                if !current.is_empty() {
                    lines.push(std::mem::take(&mut current));
                }
                let mut chunk = String::new();
                for ch in word.chars() {
                    let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
                    if chunk.width() + cw > width {
                        lines.push(std::mem::take(&mut chunk));
                    }
                    chunk.push(ch);
                }
                current = chunk;
                continue;
            }
            if current.is_empty() {
                current.push_str(word);
            } else if current.width() + 1 + word.width() <= width {
                current.push(' ');
                current.push_str(word);
            } else {
                lines.push(std::mem::take(&mut current));
                current.push_str(word);
            }
        }
        lines.push(current);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_ascii() {
        assert_eq!(truncate_width("hello world", 8), "hello w…");
        assert_eq!(truncate_width("hi", 8), "hi");
    }

    #[test]
    fn wrap_simple() {
        let lines = wrap_text("one two three four", 10);
        assert!(lines.iter().all(|l| l.len() <= 10));
        assert!(lines.len() >= 2);
    }

    #[test]
    fn short_minutes() {
        let t = (Utc::now() - chrono::Duration::minutes(12)).to_rfc3339();
        assert_eq!(relative_short(&t), "12m");
    }
}
