use ansi_to_tui::Error;
use ansi_to_tui::IntoText;
use ratatui::text::Line;
use ratatui::text::Text;

// Expand tabs in a best-effort way for transcript rendering.
// Tabs can interact poorly with left-gutter prefixes in our TUI and CLI
// transcript views (e.g., `nl` separates line numbers from content with a tab).
// Replacing tabs with spaces avoids odd visual artifacts without changing
// semantics for our use cases.
fn expand_tabs(s: &str) -> std::borrow::Cow<'_, str> {
    if s.contains('\t') {
        // Keep it simple: replace each tab with 4 spaces.
        // We do not try to align to tab stops since most usages (like `nl`)
        // look acceptable with a fixed substitution and this avoids stateful math
        // across spans.
        std::borrow::Cow::Owned(s.replace('\t', "    "))
    } else {
        std::borrow::Cow::Borrowed(s)
    }
}

fn strip_osc_sequences(s: &str) -> std::borrow::Cow<'_, str> {
    if !s.contains('\x1B') && !s.contains('\x07') {
        return std::borrow::Cow::Borrowed(s);
    }

    let bytes = s.as_bytes();
    let mut index = 0;
    let mut output = String::with_capacity(s.len());
    let mut modified = false;

    while index < bytes.len() {
        if bytes[index] == 0x1B && bytes.get(index + 1) == Some(&b']') {
            modified = true;
            index += 2;
            while index < bytes.len() {
                if bytes[index] == 0x07 {
                    index += 1;
                    break;
                }
                if bytes[index] == 0x1B && bytes.get(index + 1) == Some(&b'\\') {
                    index += 2;
                    break;
                }
                index += 1;
            }
            continue;
        }

        if bytes[index] == 0x07 {
            modified = true;
            index += 1;
            continue;
        }

        output.push(bytes[index] as char);
        index += 1;
    }

    if modified {
        std::borrow::Cow::Owned(output)
    } else {
        std::borrow::Cow::Borrowed(s)
    }
}

/// This function should be used when the contents of `s` are expected to match
/// a single line. If multiple lines are found, a warning is logged and only the
/// first line is returned.
pub fn ansi_escape_line(s: &str) -> Line<'static> {
    // Normalize tabs to spaces to avoid odd gutter collisions in transcript mode.
    let s = expand_tabs(s);
    let s = strip_osc_sequences(&s);
    let text = ansi_escape(&s);
    match text.lines.as_slice() {
        [] => "".into(),
        [only] => only.clone(),
        [first, rest @ ..] => {
            tracing::warn!("ansi_escape_line: expected a single line, got {first:?} and {rest:?}");
            first.clone()
        }
    }
}

pub fn ansi_escape(s: &str) -> Text<'static> {
    let s = strip_osc_sequences(s);
    // to_text() claims to be faster, but introduces complex lifetime issues
    // such that it's not worth it.
    match s.as_ref().into_text() {
        Ok(text) => text,
        Err(err) => match err {
            Error::NomError(message) => {
                tracing::error!(
                    "ansi_to_tui NomError docs claim should never happen when parsing `{}`: {message}",
                    s.as_ref()
                );
                panic!();
            }
            Error::Utf8Error(utf8error) => {
                tracing::error!("Utf8Error: {utf8error}");
                panic!();
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::ansi_escape_line;
    use super::strip_osc_sequences;
    use ratatui::text::Line;

    fn line_text(line: Line<'static>) -> String {
        line.spans
            .into_iter()
            .map(|span| span.content.into_owned())
            .collect()
    }

    #[test]
    fn strip_osc_sequences_removes_bel_terminated_osc() {
        let input = "\x1b]8;;https://github.com\x07github.com\x1b]8;;\x07";
        assert_eq!(strip_osc_sequences(input), "github.com");
    }

    #[test]
    fn strip_osc_sequences_removes_st_terminated_osc() {
        let input = "\x1b]11;rgb:0000/0000/0000\x1b\\visible";
        assert_eq!(strip_osc_sequences(input), "visible");
    }

    #[test]
    fn ansi_escape_line_strips_osc_payloads() {
        let input = "\x1b]8;;https://github.com\x07github.com\x1b]8;;\x07";
        assert_eq!(line_text(ansi_escape_line(input)), "github.com");
    }
}
