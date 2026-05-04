use std::path::Path;
use std::path::PathBuf;

use ratatui::style::Stylize;
use ratatui::text::Line;

use crate::history_cell::CompositeHistoryCell;
use crate::history_cell::HistoryCell;
use crate::history_cell::PlainHistoryCell;

const CODEX_GUIDE: &str = include_str!("../codex_guide.md");

#[derive(Debug)]
struct CodexGuideCell {
    cwd: PathBuf,
}

impl CodexGuideCell {
    fn new(cwd: &Path) -> Self {
        Self {
            cwd: cwd.to_path_buf(),
        }
    }
}

impl HistoryCell for CodexGuideCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let Some(wrap_width) =
            crate::width::usable_content_width_u16(width, /*reserved_cols*/ 0)
        else {
            return Vec::new();
        };

        let mut lines = Vec::new();
        crate::markdown::append_markdown(
            CODEX_GUIDE,
            Some(wrap_width),
            Some(self.cwd.as_path()),
            &mut lines,
        );
        lines
    }
}

pub(crate) fn new_codex_guide_output(cwd: &Path) -> CompositeHistoryCell {
    let command = PlainHistoryCell::new(vec!["/codex".magenta().into()]);
    CompositeHistoryCell::new(vec![Box::new(command), Box::new(CodexGuideCell::new(cwd))])
}
