//! Rendering for `hold ui`. Pure functions from [`App`] state to widgets;
//! visual language follows `assets/brand/cli.md` (tinctures, ledger tables,
//! the battlement rule).

use std::fmt;

use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{Block, Clear, HighlightSpacing, List, ListState, Paragraph, Row as TableRow, Table, TableState, Wrap},
};

use crate::{
    store::MemoryStore,
    types::{AuditEntry, Memory},
    ui::{
        app::{App, Detail, Focus, Mode, Status},
        editor::{EditDraft, EditField, TextInput},
    },
};

/// One battlement unit: eight merlons, four gaps.
const BATTLEMENT_UNIT: &str = "\u{2580}\u{2580}\u{2580}\u{2580}\u{2580}\u{2580}\u{2580}\u{2580}    ";

/// Scope rows reserve two border cells and one selection-marker cell.
const SCOPE_LIST_CHROME_WIDTH: u16 = 3_u16;
const SCOPE_HIGHLIGHT_SYMBOL: &str = "\u{258c}";

/// Render one frame.
pub(crate) fn draw<S>(frame: &mut Frame<'_>, app: &App<S>)
where
    S: MemoryStore + Clone + fmt::Debug + 'static,
{
    let [header, main, rule, status] = Layout::vertical([Constraint::Length(1), Constraint::Min(3), Constraint::Length(1), Constraint::Length(1)]).areas(frame.area());
    draw_header(frame, app, header);
    let [scopes, memories] = Layout::horizontal([Constraint::Length(26), Constraint::Min(30)]).areas(main);
    draw_scopes(frame, app, scopes);
    draw_memories(frame, app, memories);
    draw_rule(frame, app, rule);
    draw_status(frame, app, status);
    if let Some(detail) = &app.detail {
        if matches!(app.mode, Mode::Edit | Mode::ConfirmDiscard) {
            if let Some(edit) = &app.edit {
                draw_edit(frame, app, detail, edit, main);
            }
        } else {
            draw_detail(frame, app, detail, main);
        }
    }
    if app.mode == Mode::ConfirmDelete {
        draw_confirmation(frame, app, main, "Forget this memory permanently?  y yes  n no");
    } else if app.mode == Mode::ConfirmDiscard {
        draw_confirmation(frame, app, main, "Discard unsaved changes?  y yes  n no");
    }
}

fn draw_header<S>(frame: &mut Frame<'_>, app: &App<S>, area: Rect)
where
    S: MemoryStore + Clone + fmt::Debug + 'static,
{
    let emphasis = Style::default().bold();
    let mut spans = vec![Span::raw(" local"), Span::styled("hold", emphasis), Span::styled("  \u{bb} ", app.theme.label())];
    if app.query.is_empty() && app.mode != Mode::Search {
        spans.push(Span::styled("press / to search the hold", app.theme.label()));
    } else {
        spans.push(Span::raw(escape_terminal_text(&app.query)));
    }
    if app.mode == Mode::Search {
        spans.push(Span::styled("\u{2588}", emphasis));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);

    let mode = active_search_mode(app).to_string();
    let right = Line::from(vec![Span::styled("mode ", app.theme.label()), Span::styled(mode, app.theme.ident()), Span::raw(" ")]);
    frame.render_widget(Paragraph::new(right).alignment(Alignment::Right), area);
}

fn active_search_mode<S>(app: &App<S>) -> crate::types::SearchMode
where
    S: MemoryStore + Clone + fmt::Debug + 'static,
{
    let configured = app.engine.search_config().default_mode;
    if app.loading {
        return app.requested_mode.unwrap_or(configured);
    }
    app.executed_mode.or(app.requested_mode).unwrap_or(configured)
}

fn pane_block(title: &str, focused: bool, app_ident: Style, label: Style) -> Block<'_> {
    let border = if focused { app_ident } else { label };
    Block::bordered().border_style(border).title(Span::styled(title, label))
}

fn draw_scopes<S>(frame: &mut Frame<'_>, app: &App<S>, area: Rect)
where
    S: MemoryStore + Clone + fmt::Debug + 'static,
{
    let selected_style = Style::default().bold();
    let marker_style = if app.focus == Focus::Scopes { selected_style } else { app.theme.label() };
    let width = usize::from(area.width.saturating_sub(SCOPE_LIST_CHROME_WIDTH));
    let mut items = vec![scope_list_line("All memories", app.scope_total, width, Style::default())];
    if app.scopes.is_empty() {
        items.push(Line::from(Span::styled("  no scoped memories", app.theme.label())));
    } else {
        items.extend(app.scopes.iter().map(|scope| scope_list_line(&scope.label, Some(scope.count), width, app.theme.ident())));
    }
    let list = List::new(items)
        .block(pane_block(" SCOPES ", app.focus == Focus::Scopes, app.theme.ident(), app.theme.label()))
        .highlight_style(selected_style)
        .highlight_symbol(Line::styled(SCOPE_HIGHLIGHT_SYMBOL, marker_style))
        .highlight_spacing(HighlightSpacing::Always);
    let mut state = ListState::default().with_selected(Some(app.scope_selected));
    frame.render_stateful_widget(list, area, &mut state);
}

fn scope_list_line(label: &str, count: Option<u64>, width: usize, style: Style) -> Line<'static> {
    let label = escape_terminal_text(label);
    let Some(count) = count else {
        return Line::styled(label, style);
    };
    let count = count.to_string();
    let count_width = count.chars().count();
    if count_width > width {
        let mut overflow = " ".repeat(width.saturating_sub(1_usize));
        if width > 0_usize {
            overflow.push('\u{2026}');
        }
        return Line::styled(overflow, style);
    }
    let reserved = count_width.saturating_add(1_usize);
    let label_width = width.saturating_sub(reserved);
    let mut fitted = label.chars().take(label_width).collect::<String>();
    let padding = width.saturating_sub(fitted.chars().count()).saturating_sub(count_width);
    fitted.push_str(&" ".repeat(padding));
    fitted.push_str(&count);
    Line::styled(fitted, style)
}

fn draw_memories<S>(frame: &mut Frame<'_>, app: &App<S>, area: Rect)
where
    S: MemoryStore + Clone + fmt::Debug + 'static,
{
    let header = TableRow::new(vec!["AGE", "TYPE", "SCORE", "CONTENT"]).style(app.theme.label());
    let rows = app.rows.iter().map(|row| {
        let score = row.score.map_or_else(|| "\u{2014}".to_owned(), |score| format!("{score:.0}"));
        TableRow::new(vec![
            Line::from(age_label(app, &row.memory)),
            Line::from(row.memory.memory_type.to_string()),
            Line::from(score),
            content_line(app, &row.memory),
        ])
    });
    let widths = [Constraint::Length(5), Constraint::Length(10), Constraint::Length(5), Constraint::Min(20)];
    let selected_style = Style::default().bold();
    let marker_style = if app.focus == Focus::Memories { selected_style } else { app.theme.label() };
    let table = Table::new(rows, widths)
        .header(header)
        .block(pane_block(" MEMORIES ", app.focus == Focus::Memories, app.theme.ident(), app.theme.label()))
        .row_highlight_style(selected_style)
        .highlight_symbol(Line::styled("\u{258c}", marker_style));
    let mut state = TableState::default().with_selected(Some(app.row_selected));
    frame.render_stateful_widget(table, area, &mut state);
}

fn age_label<S>(app: &App<S>, memory: &Memory) -> String
where
    S: MemoryStore + Clone + fmt::Debug + 'static,
{
    let elapsed = app.now.signed_duration_since(memory.updated_at);
    let days = elapsed.num_days();
    if days > 0_i64 {
        return format!("{days}d");
    }
    let hours = elapsed.num_hours();
    if hours > 0_i64 {
        return format!("{hours}h");
    }
    let minutes = elapsed.num_minutes();
    if minutes > 0_i64 {
        return format!("{minutes}m");
    }
    "now".to_owned()
}

fn content_line<S>(app: &App<S>, memory: &Memory) -> Line<'static>
where
    S: MemoryStore + Clone + fmt::Debug + 'static,
{
    let preview = memory.content.lines().next().unwrap_or_default();
    let mut spans = vec![Span::raw(escape_terminal_text(preview))];
    for tag in &memory.tags {
        spans.push(Span::styled(format!(" #{}", escape_terminal_text(tag)), app.theme.ident()));
    }
    Line::from(spans)
}

fn draw_rule<S>(frame: &mut Frame<'_>, app: &App<S>, area: Rect)
where
    S: MemoryStore + Clone + fmt::Debug + 'static,
{
    let width = usize::from(area.width);
    let unit_width = BATTLEMENT_UNIT.chars().count();
    let repetitions = width.div_ceil(unit_width);
    let pattern: String = BATTLEMENT_UNIT.repeat(repetitions).chars().take(width).collect();
    frame.render_widget(Paragraph::new(Span::styled(pattern, app.theme.accent().dim())), area);
}

fn draw_status<S>(frame: &mut Frame<'_>, app: &App<S>, area: Rect)
where
    S: MemoryStore + Clone + fmt::Debug + 'static,
{
    let verb = match &app.status {
        Status::Held(text) => Line::from(vec![Span::styled(" \u{2713} held  ", app.theme.held()), Span::raw(escape_terminal_text(text))]),
        Status::NotHeld(text) => Line::from(vec![Span::styled(" \u{2717} not held  ", app.theme.not_held()), Span::raw(escape_terminal_text(text))]),
        Status::Note(text) => Line::from(vec![
            Span::styled(" \u{b7} ", app.theme.label()),
            Span::styled(escape_terminal_text(text), app.theme.label()),
        ]),
    };
    let mut line = if app.loading {
        Line::from(vec![Span::styled(" \u{2026} recalling", app.theme.label())])
    } else {
        verb
    };
    if let Some(notice) = &app.notice {
        line.push_span(Span::styled(format!("  ! {}", escape_terminal_text(notice)), app.theme.not_held()));
    }
    if let Some(notice) = &app.scope_notice {
        line.push_span(Span::styled(format!("  ! {}", escape_terminal_text(notice)), app.theme.not_held()));
    }
    frame.render_widget(Paragraph::new(line), area);
    let hint = match (app.mode, app.focus) {
        (Mode::Browse, Focus::Scopes) => "j/k filter  enter results  tab/\u{2192} memories  / search  q quit ",
        (Mode::Browse, Focus::Memories) => "j/k move  enter open  tab/\u{2190} scopes  / search  q quit ",
        (Mode::Search, _) => "type query  enter apply  esc browse ",
        (Mode::Detail, _) => "e edit  d delete  j/k scroll  esc close ",
        (Mode::Edit, _) => "tab field  arrows edit  ctrl+s save  esc cancel ",
        (Mode::ConfirmDelete | Mode::ConfirmDiscard, _) => "y confirm  n cancel ",
    };
    let hints = Line::from(Span::styled(hint, app.theme.label()));
    frame.render_widget(Paragraph::new(hints).alignment(Alignment::Right), area);
}

fn draw_detail<S>(frame: &mut Frame<'_>, app: &App<S>, detail: &Detail, main: Rect)
where
    S: MemoryStore + Clone + fmt::Debug + 'static,
{
    let popup = main;
    frame.render_widget(Clear, popup);
    let block = Block::bordered().border_style(app.theme.label()).title(Span::styled(" MEMORY ", app.theme.label()));
    let mut lines = meta_lines(app, &detail.memory);
    if let Some(metadata) = &detail.metadata {
        lines.push(Line::from(vec![
            Span::styled("summary    ", app.theme.label()),
            Span::raw(metadata.summary.as_deref().map_or_else(|| "\u{2014}".to_owned(), escape_terminal_text)),
        ]));
        lines.push(Line::from(vec![
            Span::styled("label      ", app.theme.label()),
            Span::raw(metadata.agent_label.as_deref().map_or_else(|| "\u{2014}".to_owned(), escape_terminal_text)),
        ]));
    }
    lines.push(Line::default());
    lines.extend(detail.memory.content.lines().map(|line| Line::from(escape_terminal_text(line))));
    lines.push(Line::default());
    lines.push(Line::from(Span::styled("AUDIT", app.theme.label())));
    lines.extend(audit_lines(app, &detail.audit));
    let paragraph = Paragraph::new(lines).block(block).wrap(Wrap { trim: false }).scroll((detail.scroll, 0_u16));
    frame.render_widget(paragraph, popup);
}

fn meta_lines<S>(app: &App<S>, memory: &Memory) -> Vec<Line<'static>>
where
    S: MemoryStore + Clone + fmt::Debug + 'static,
{
    let mut tags = Line::default();
    for tag in &memory.tags {
        tags.push_span(Span::styled(format!("#{} ", escape_terminal_text(tag)), app.theme.ident()));
    }
    let importance = memory.importance.value();
    let confidence = memory.confidence.value();
    vec![
        Line::from(vec![Span::styled("id         ", app.theme.label()), Span::styled(memory.id.to_string(), app.theme.ident())]),
        Line::from(vec![
            Span::styled("kind       ", app.theme.label()),
            Span::raw(memory.memory_type.to_string()),
            Span::styled(format!("   importance {importance:.2}   confidence {confidence:.2}"), app.theme.label()),
        ]),
        Line::from(vec![
            Span::styled("agent      ", app.theme.label()),
            Span::raw(memory.provenance.source_agent.as_deref().map_or_else(|| "\u{2014}".to_owned(), escape_terminal_text)),
            Span::styled("   scope ", app.theme.label()),
            Span::styled(
                memory.provenance.source_conversation.as_deref().map_or_else(|| "\u{2014}".to_owned(), escape_terminal_text),
                app.theme.ident(),
            ),
        ]),
        Line::from(vec![
            Span::styled("updated    ", app.theme.label()),
            Span::raw(memory.updated_at.format("%Y-%m-%d %H:%M").to_string()),
            Span::styled(format!("   embedded {}", if memory.has_embedding { "yes" } else { "no" }), app.theme.label()),
        ]),
        Line::from(vec![
            Span::styled("expires    ", app.theme.label()),
            Span::raw(memory.expires_at.map_or_else(|| "\u{2014}".to_owned(), |value| value.to_rfc3339())),
        ]),
        tags,
    ]
}

fn draw_edit<S>(frame: &mut Frame<'_>, app: &App<S>, detail: &Detail, edit: &EditDraft, area: Rect)
where
    S: MemoryStore + Clone + fmt::Debug + 'static,
{
    frame.render_widget(Clear, area);
    let title = if app.pending { " EDIT MEMORY \u{b7} SAVING " } else { " EDIT MEMORY " };
    let block = Block::bordered().border_style(app.theme.label()).title(Span::styled(title, app.theme.label()));
    let mut lines = vec![
        Line::from(vec![
            Span::styled("id         ", app.theme.label()),
            Span::styled(detail.memory.id.to_string(), app.theme.ident()),
        ]),
        Line::from(vec![
            Span::styled("scope      ", app.theme.label()),
            Span::raw(
                detail
                    .memory
                    .provenance
                    .source_conversation
                    .as_deref()
                    .map_or_else(|| "\u{2014}".to_owned(), escape_terminal_text),
            ),
            Span::styled("  (read-only)", app.theme.label()),
        ]),
        Line::default(),
    ];
    append_edit_field(&mut lines, app, edit, EditField::Content, &edit.content);
    append_edit_field(&mut lines, app, edit, EditField::Tags, &edit.tags);
    append_edit_field(&mut lines, app, edit, EditField::Importance, &edit.importance);
    append_edit_field(&mut lines, app, edit, EditField::Expiry, &edit.expiry);
    append_edit_field(&mut lines, app, edit, EditField::Metadata, &edit.metadata);
    let scroll = edit_render_scroll(&lines, edit, area);
    let paragraph = Paragraph::new(lines).block(block).wrap(Wrap { trim: false }).scroll((scroll, 0_u16));
    frame.render_widget(paragraph, area);
}

fn append_edit_field<S>(lines: &mut Vec<Line<'static>>, app: &App<S>, edit: &EditDraft, field: EditField, input: &TextInput)
where
    S: MemoryStore + Clone + fmt::Debug + 'static,
{
    let active = edit.field == field;
    let marker = if active { "\u{258c} " } else { "  " };
    let label_style = if active { Style::default().bold() } else { app.theme.label() };
    lines.push(Line::from(vec![Span::styled(marker, label_style), Span::styled(field.label(), label_style)]));
    let mut value = input.value.clone();
    if active && !app.pending {
        value.insert(input.cursor, '\u{2588}');
    }
    if value.is_empty() {
        lines.push(Line::from(Span::styled("  \u{2014}", app.theme.label())));
    } else {
        lines.extend(value.lines().map(|line| Line::from(format!("  {}", escape_terminal_text(line)))));
    }
    lines.push(Line::default());
}

#[expect(
    clippy::string_slice,
    clippy::arithmetic_side_effects,
    clippy::integer_division,
    clippy::integer_division_remainder_used,
    reason = "cursor offsets are UTF-8 boundaries and terminal row calculation intentionally uses integer cell widths"
)]
fn edit_render_scroll(lines: &[Line<'_>], edit: &EditDraft, area: Rect) -> u16 {
    let width = usize::from(area.width.saturating_sub(2_u16)).max(1_usize);
    let height = usize::from(area.height.saturating_sub(2_u16)).max(1_usize);
    let cursor_line = edit.cursor_document_line().min(lines.len().saturating_sub(1_usize));
    let rows_before_cursor = lines[..cursor_line].iter().map(|line| line.width().max(1_usize).div_ceil(width)).sum::<usize>();
    let input = edit.active();
    let prefix = input.value[..input.cursor].rsplit_once('\n').map_or(&input.value[..input.cursor], |(_, suffix)| suffix);
    let cursor_column = Line::from(format!("  {}", escape_terminal_text(prefix))).width();
    let cursor_row = rows_before_cursor.saturating_add(cursor_column / width);
    let current = usize::from(edit.scroll);
    let target = if cursor_row < current {
        cursor_row
    } else if cursor_row >= current.saturating_add(height) {
        cursor_row.saturating_sub(height.saturating_sub(1_usize))
    } else {
        current
    };
    u16::try_from(target).unwrap_or(u16::MAX)
}

fn draw_confirmation<S>(frame: &mut Frame<'_>, app: &App<S>, area: Rect, message: &str)
where
    S: MemoryStore + Clone + fmt::Debug + 'static,
{
    let [_, middle, _] = Layout::vertical([Constraint::Percentage(40), Constraint::Length(5), Constraint::Percentage(40)]).areas(area);
    let [_, popup, _] = Layout::horizontal([Constraint::Percentage(20), Constraint::Percentage(60), Constraint::Percentage(20)]).areas(middle);
    frame.render_widget(Clear, popup);
    let block = Block::bordered().border_style(app.theme.not_held()).title(Span::styled(" CONFIRM ", app.theme.label()));
    frame.render_widget(Paragraph::new(escape_terminal_text(message)).block(block).alignment(Alignment::Center), popup);
}

fn audit_lines<S>(app: &App<S>, audit: &[AuditEntry]) -> Vec<Line<'static>>
where
    S: MemoryStore + Clone + fmt::Debug + 'static,
{
    if audit.is_empty() {
        return vec![Line::from(Span::styled("no recorded activity", app.theme.label()))];
    }
    audit
        .iter()
        .map(|entry| {
            Line::from(vec![
                Span::styled(entry.timestamp.format("%Y-%m-%d %H:%M  ").to_string(), app.theme.label()),
                Span::raw(entry.action.to_string()),
                Span::styled(
                    format!("  {}", entry.caller_agent.as_deref().map_or_else(|| "\u{2014}".to_owned(), escape_terminal_text)),
                    app.theme.ident(),
                ),
            ])
        })
        .collect()
}

fn escape_terminal_text(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        if character.is_control() {
            escaped.extend(character.escape_default());
        } else {
            escaped.push(character);
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use ratatui::{
        buffer::Buffer,
        layout::Rect,
        style::{Color, Style},
        widgets::{HighlightSpacing, List, ListState, StatefulWidget as _},
    };

    use super::{SCOPE_HIGHLIGHT_SYMBOL, escape_terminal_text, scope_list_line};

    fn line_text(line: &ratatui::text::Line<'_>) -> String {
        line.spans.iter().map(|span| span.content.as_ref()).collect()
    }

    #[test]
    fn scope_count_line_stays_within_available_width() {
        let style = Style::default();
        assert_eq!(line_text(&scope_list_line("scope", Some(u64::MAX), 0_usize, style)), "");
        assert_eq!(line_text(&scope_list_line("scope", Some(u64::MAX), 3_usize, style)), "  \u{2026}");
        assert_eq!(line_text(&scope_list_line("scope", Some(u64::MAX), 20_usize, style)), u64::MAX.to_string());
        assert_eq!(line_text(&scope_list_line("alpha", Some(42_u64), 8_usize, style)), "alpha 42");
    }

    #[test]
    fn scope_count_line_preserves_identifier_style() {
        let line = scope_list_line("project:localhold", Some(7_u64), 24_usize, Style::default().fg(Color::Blue));
        assert_eq!(line.style.fg, Some(Color::Blue));
    }

    #[test]
    fn selected_scope_keeps_overflow_ellipsis_visible() {
        let list = List::new([scope_list_line("scope", Some(u64::MAX), 3_usize, Style::default())])
            .highlight_symbol(SCOPE_HIGHLIGHT_SYMBOL)
            .highlight_spacing(HighlightSpacing::Always);
        let mut state = ListState::default().with_selected(Some(0_usize));
        let mut buffer = Buffer::empty(Rect::new(0_u16, 0_u16, 4_u16, 1_u16));

        list.render(buffer.area, &mut buffer, &mut state);

        assert_eq!(buffer, Buffer::with_lines(["\u{258c}  \u{2026}"]));
    }

    #[test]
    fn terminal_control_characters_are_visibly_escaped() {
        let escaped = escape_terminal_text("safe\u{1b}]52;c;payload\u{7}\tend");
        assert!(!escaped.chars().any(char::is_control));
        assert!(escaped.starts_with("safe\\"));
        assert!(escaped.contains("]52;c;payload"));
        assert!(escaped.ends_with("\\tend"));
    }
}
