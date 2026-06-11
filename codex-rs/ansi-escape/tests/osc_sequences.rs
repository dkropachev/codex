use codex_ansi_escape::ansi_escape;
use codex_ansi_escape::ansi_escape_line;
use pretty_assertions::assert_eq;
use ratatui::text::Line;

fn line_text(line: Line<'static>) -> String {
    line.spans
        .into_iter()
        .map(|span| span.content.into_owned())
        .collect()
}

#[test]
fn ansi_escape_line_strips_bel_terminated_osc() {
    let input = "\x1b]8;;https://github.com\x07github.com\x1b]8;;\x07";

    assert_eq!(line_text(ansi_escape_line(input)), "github.com");
}

#[test]
fn ansi_escape_line_strips_st_terminated_osc() {
    let input = "\x1b]11;rgb:0000/0000/0000\x1b\\visible";

    assert_eq!(line_text(ansi_escape_line(input)), "visible");
}

#[test]
fn ansi_escape_line_strips_unterminated_osc() {
    let input = "visible\x1b]10;?";

    assert_eq!(line_text(ansi_escape_line(input)), "visible");
}

#[test]
fn ansi_escape_line_strips_dangling_escape() {
    assert_eq!(line_text(ansi_escape_line("visible\x1b")), "visible");
}

#[test]
fn ansi_escape_strips_osc_sequences_from_multiline_text() {
    let input = "one\x1b]8;;https://github.com\x07two\x1b]8;;\x07\nthree";
    let text = ansi_escape(input);
    let lines: Vec<String> = text.lines.into_iter().map(line_text).collect();

    assert_eq!(lines, vec!["onetwo".to_string(), "three".to_string()]);
}
