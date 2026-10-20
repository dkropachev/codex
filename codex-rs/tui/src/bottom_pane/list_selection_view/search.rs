//! Search activation, query restoration, and editable search-field rendering for list pickers.
//! A nonempty global query switches the item source; the parent retains row mapping and event order.

use super::ListSelectionView;
use crate::key_hint::KeyBinding;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Widget;
use unicode_width::UnicodeWidthStr;

/// Controls whether a searchable list starts in search mode or waits for a shortcut.
#[derive(Clone, Copy, Default)]
pub(crate) enum SearchMode {
    #[default]
    Immediate,
    OnDemand(KeyBinding),
}

impl ListSelectionView {
    pub(super) fn search_items_active(&self) -> bool {
        !self.search_query.is_empty() && !self.search_items.is_empty()
    }

    pub(super) fn edit_search_query(&mut self, edit: impl FnOnce(&mut String)) {
        let search_items_were_active = self.search_items_active();
        edit(&mut self.search_query);
        if search_items_were_active != self.search_items_active() {
            self.state.reset();
        }
        self.apply_filter();
    }

    /// Preserve a searchable picker's filter when its parent rebuilds the view.
    pub(crate) fn search_query(&self) -> Option<&str> {
        self.search_field_visible()
            .then_some(self.search_query.as_str())
    }

    pub(super) fn search_field_visible(&self) -> bool {
        self.is_searchable && self.search_active
    }

    pub(super) fn on_demand_search_active(&self) -> bool {
        self.search_active && matches!(self.search_mode, SearchMode::OnDemand(_))
    }

    pub(super) fn stop_on_demand_search(&mut self) {
        self.search_active = false;
        self.edit_search_query(String::clear);
    }

    pub(crate) fn set_search_query(&mut self, query: String) {
        let selected_actual_idx = self.selected_actual_idx();
        let changes_item_source =
            self.search_items_active() != (!query.is_empty() && !self.search_items.is_empty());
        self.search_active = true;
        self.search_query = query;
        if changes_item_source {
            self.state.reset();
            self.initial_selected_idx = None;
        } else {
            self.state.selected_idx = None;
            self.initial_selected_idx = selected_actual_idx;
        }
        self.apply_filter();
    }

    pub(super) fn render_search_field(&self, search_area: Rect, buf: &mut Buffer) {
        if self.search_field_visible() {
            let query_span: Span<'static> = if self.search_query.is_empty() {
                self.search_placeholder
                    .as_ref()
                    .map(|placeholder| placeholder.clone().dim())
                    .unwrap_or_else(|| "".into())
            } else {
                self.search_query.clone().into()
            };
            if self.on_demand_search_active() {
                let label = "Search › ";
                let active_style = super::super::picker_style::active_tab_style();
                buf.set_style(search_area, active_style);
                Line::from(vec![
                    label
                        .fg(crate::style::accent_color_on(active_style.bg))
                        .bold(),
                    query_span,
                ])
                .render(search_area, buf);
                let cursor_offset = (label.width() + self.search_query.width())
                    .min(usize::from(search_area.width.saturating_sub(/*rhs*/ 1)));
                self.search_cursor.set(Some((
                    search_area.x.saturating_add(cursor_offset as u16),
                    search_area.y,
                )));
            } else {
                Line::from(query_span).render(search_area, buf);
            }
        }
    }
}
