//! Rendering for `hold ui`. Pure functions from [`App`] state to widgets;
//! visual language follows `assets/brand/cli.md` (tinctures, ledger tables,
//! the battlement rule).

use std::fmt;

use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{Block, Clear, List, ListState, Paragraph, Row as TableRow, Table, TableState, Wrap},
};

use crate::{
    store::MemoryStore,
    types::{AuditEntry, Memory},
    ui::app::{App, Detail, Focus, Mode, Status},
};

/// One battlement unit: eight merlons, four gaps.
const BATTLEMENT_UNIT: &str = "\u{2580}\u{2580}\u{2580}\u{2580}\u{2580}\u{2580}\u{2580}\u{2580}    ";

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
        draw_detail(frame, app, detail, main);
    }
}

fn draw_header<S>(frame: &mut Frame<'_>, app: &App<S>, area: Rect)
where
    S: MemoryStore + Clone + fmt::Debug + 'static,
{
    let mut spans = vec![Span::raw(" local"), Span::styled("hold", app.theme.accent()), Span::styled("  \u{bb} ", app.theme.label())];
    if app.query.is_empty() && app.mode != Mode::Search {
        spans.push(Span::styled("press / to search the hold", app.theme.label()));
    } else {
        spans.push(Span::raw(app.query.clone()));
    }
    if app.mode == Mode::Search {
        spans.push(Span::styled("\u{2588}", app.theme.accent()));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);

    let mode = app.requested_mode.map_or_else(|| "auto".to_owned(), |mode| mode.to_string());
    let right = Line::from(vec![Span::styled("mode ", app.theme.label()), Span::styled(mode, app.theme.ident()), Span::raw(" ")]);
    frame.render_widget(Paragraph::new(right).alignment(Alignment::Right), area);
}

fn pane_block(title: &str, focused: bool, app_ident: Style, label: Style) -> Block<'_> {
    let border = if focused { app_ident } else { label };
    Block::bordered().border_style(border).title(Span::styled(title, label))
}

fn draw_scopes<S>(frame: &mut Frame<'_>, app: &App<S>, area: Rect)
where
    S: MemoryStore + Clone + fmt::Debug + 'static,
{
    let mut items = vec![Line::from("(all scopes)")];
    items.extend(app.scopes.iter().map(|scope| Line::from(scope.display_name.clone())));
    let list = List::new(items)
        .block(pane_block(" SCOPES ", app.focus == Focus::Scopes, app.theme.ident(), app.theme.label()))
        .highlight_style(app.theme.accent().bold())
        .highlight_symbol("\u{258c}");
    let mut state = ListState::default().with_selected(Some(app.scope_selected));
    frame.render_stateful_widget(list, area, &mut state);
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
    let table = Table::new(rows, widths)
        .header(header)
        .block(pane_block(" MEMORIES ", app.focus == Focus::Memories, app.theme.ident(), app.theme.label()))
        .row_highlight_style(app.theme.accent().bold())
        .highlight_symbol("\u{258c}");
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

fn content_line<'a, S>(app: &App<S>, memory: &'a Memory) -> Line<'a>
where
    S: MemoryStore + Clone + fmt::Debug + 'static,
{
    let preview = memory.content.lines().next().unwrap_or_default();
    let mut spans = vec![Span::raw(preview)];
    for tag in &memory.tags {
        spans.push(Span::styled(format!(" #{tag}"), app.theme.ident()));
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
        Status::Held(text) => Line::from(vec![Span::styled(" \u{2713} held  ", app.theme.held()), Span::raw(text.clone())]),
        Status::NotHeld(text) => Line::from(vec![Span::styled(" \u{2717} not held  ", app.theme.not_held()), Span::raw(text.clone())]),
        Status::Note(text) => Line::from(vec![Span::styled(" \u{b7} ", app.theme.label()), Span::styled(text.clone(), app.theme.label())]),
    };
    let line = if app.loading {
        Line::from(vec![Span::styled(" \u{2026} recalling", app.theme.label())])
    } else {
        verb
    };
    frame.render_widget(Paragraph::new(line), area);
    let hints = Line::from(Span::styled("/ search  m mode  tab pane  enter open  q quit ", app.theme.label()));
    frame.render_widget(Paragraph::new(hints).alignment(Alignment::Right), area);
}

fn draw_detail<S>(frame: &mut Frame<'_>, app: &App<S>, detail: &Detail, main: Rect)
where
    S: MemoryStore + Clone + fmt::Debug + 'static,
{
    let [_, mid, _] = Layout::horizontal([Constraint::Percentage(8), Constraint::Percentage(84), Constraint::Percentage(8)]).areas(main);
    let [_, popup, _] = Layout::vertical([Constraint::Percentage(6), Constraint::Percentage(88), Constraint::Percentage(6)]).areas(mid);
    frame.render_widget(Clear, popup);
    let block = Block::bordered().border_style(app.theme.accent()).title(Span::styled(" MEMORY ", app.theme.label()));
    let mut lines = meta_lines(app, &detail.memory);
    lines.push(Line::default());
    lines.extend(detail.memory.content.lines().map(|line| Line::from(line.to_owned())));
    lines.push(Line::default());
    lines.push(Line::from(Span::styled("AUDIT", app.theme.label())));
    lines.extend(audit_lines(app, &detail.audit));
    let paragraph = Paragraph::new(lines).block(block).wrap(Wrap { trim: false }).scroll((detail.scroll, 0_u16));
    frame.render_widget(paragraph, popup);
}

fn meta_lines<'a, S>(app: &App<S>, memory: &'a Memory) -> Vec<Line<'a>>
where
    S: MemoryStore + Clone + fmt::Debug + 'static,
{
    let mut tags = Line::default();
    for tag in &memory.tags {
        tags.push_span(Span::styled(format!("#{tag} "), app.theme.ident()));
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
            Span::raw(memory.provenance.source_agent.clone().unwrap_or_else(|| "\u{2014}".to_owned())),
            Span::styled("   scope ", app.theme.label()),
            Span::styled(memory.provenance.source_conversation.clone().unwrap_or_else(|| "\u{2014}".to_owned()), app.theme.ident()),
        ]),
        Line::from(vec![
            Span::styled("updated    ", app.theme.label()),
            Span::raw(memory.updated_at.format("%Y-%m-%d %H:%M").to_string()),
            Span::styled(format!("   embedded {}", if memory.has_embedding { "yes" } else { "no" }), app.theme.label()),
        ]),
        tags,
    ]
}

fn audit_lines<'a, S>(app: &App<S>, audit: &'a [AuditEntry]) -> Vec<Line<'a>>
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
                Span::styled(format!("  {}", entry.caller_agent.clone().unwrap_or_else(|| "\u{2014}".to_owned())), app.theme.ident()),
            ])
        })
        .collect()
}
