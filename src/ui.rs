use std::collections::HashSet;
use std::io::{self, stdout};
use std::path::PathBuf;

use anyhow::{Context, Result};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Row, Table, TableState};

use crate::profile::{Library, Profile};
use crate::tree::{CallTree, NodePath, format_frame, pct};

pub struct App {
    pub profile_path: PathBuf,
    pub thread_name: String,
    pub thread_index: usize,
    pub tree: CallTree,
    pub libs: Vec<Library>,
    pub expanded: HashSet<NodePath>,
    pub cursor: usize,
    pub filter: String,
    pub filtering: bool,
    pub status: String,
    /// Horizontal scroll offset (in chars) for the Symbol column.
    pub symbol_scroll: usize,
}

impl App {
    pub fn new(
        profile: &Profile,
        profile_path: PathBuf,
        thread_index: usize,
        thread_name: String,
        tree: CallTree,
    ) -> Self {
        let mut expanded = HashSet::new();
        for (i, root) in tree.roots.iter().enumerate() {
            if pct(root.total, tree.total_samples) >= 5.0 {
                expanded.insert(vec![i]);
            }
        }
        Self {
            profile_path,
            thread_name,
            thread_index,
            tree,
            libs: profile.libs.clone(),
            expanded,
            cursor: 0,
            filter: String::new(),
            filtering: false,
            status: String::new(),
            symbol_scroll: 0,
        }
    }

    fn visible_rows(&self) -> Vec<crate::tree::VisibleRow> {
        if self.filter.is_empty() {
            return self.tree.flatten(&self.expanded);
        }
        let filter = self.filter.to_lowercase();
        let libs = &self.libs;
        self.tree.flatten_filtered(&self.expanded, |frame| {
            format_frame(frame, libs).to_lowercase().contains(&filter)
        })
    }

    fn expand_node(&mut self) {
        let rows = self.visible_rows();
        let Some(row) = rows.get(self.cursor) else {
            return;
        };
        if !row.has_children {
            return;
        }
        // One level only: show immediate children, all collapsed.
        self.clear_descendants(&row.path);
        self.expanded.insert(row.path.clone());
    }

    fn collapse_node(&mut self) {
        let rows = self.visible_rows();
        let Some(row) = rows.get(self.cursor) else {
            return;
        };
        if row.expanded {
            // Fold children away; stay on this node.
            self.collapse_path(&row.path);
            return;
        }
        // Already collapsed: collapse the parent and move cursor up to it.
        // In filter mode, visual roots (depth 0) have no visible parent.
        if row.depth == 0 {
            return;
        }
        if row.path.len() > 1 {
            let parent: NodePath = row.path[..row.path.len() - 1].to_vec();
            self.collapse_path(&parent);
            let rows = self.visible_rows();
            if let Some(idx) = rows.iter().position(|r| r.path == parent) {
                self.cursor = idx;
            }
        }
    }

    /// Remove `path` and every expanded descendant of it.
    fn collapse_path(&mut self, path: &NodePath) {
        self.expanded.remove(path);
        self.clear_descendants(path);
    }

    fn clear_descendants(&mut self, path: &NodePath) {
        self.expanded
            .retain(|p| !(p.starts_with(path) && p.len() > path.len()));
    }
}

pub fn run_tui(mut app: App) -> Result<()> {
    enable_raw_mode().context("enable raw mode")?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    execute!(terminal.backend_mut(), EnterAlternateScreen)?;
    terminal.clear()?;

    let result = run_loop(&mut terminal, &mut app);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

fn run_loop(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, app: &mut App) -> Result<()> {
    let mut table_state = TableState::default();
    loop {
        let rows = app.visible_rows();
        if app.cursor >= rows.len() && !rows.is_empty() {
            app.cursor = rows.len() - 1;
        }
        table_state.select(Some(app.cursor));

        terminal.draw(|frame| {
            let area = frame.area();
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(1),
                    Constraint::Min(3),
                    Constraint::Length(1),
                    Constraint::Length(1),
                ])
                .split(area);

            let title = format!(
                " samply-report  {}  thread[{}]: {}  samples: {} ",
                app.profile_path.display(),
                app.thread_index,
                app.thread_name,
                app.tree.total_samples
            );
            frame.render_widget(
                Paragraph::new(title).style(Style::default().add_modifier(Modifier::BOLD)),
                chunks[0],
            );

            let header = Row::new(vec!["Children", "Self", "Samples", "Symbol"])
                .style(Style::default().add_modifier(Modifier::BOLD));

            // Fixed columns + borders/highlight leave the rest for Symbol.
            let symbol_width = chunks[1]
                .width
                .saturating_sub(10 + 10 + 10 + 3 + 2 + 4) as usize;

            let scroll = app.symbol_scroll;
            let table_rows = rows.iter().map(|row| {
                let marker = if !row.has_children {
                    ' '
                } else if row.expanded {
                    '▼'
                } else {
                    '▶'
                };
                let name = format_frame(&row.frame, &app.libs);
                let symbol = fit_symbol(row.depth, marker, &name, scroll, symbol_width.max(8));
                Row::new(vec![
                    format!("{:>7.2}%", pct(row.total, app.tree.total_samples)),
                    format!("{:>7.2}%", pct(row.self_count, app.tree.total_samples)),
                    format!("{:>8}", row.total),
                    symbol,
                ])
            });

            let widths = [
                Constraint::Length(10),
                Constraint::Length(10),
                Constraint::Length(10),
                Constraint::Min(8),
            ];
            let table = Table::new(table_rows, widths)
                .header(header)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(if app.filter.is_empty() {
                            "Call tree".to_string()
                        } else {
                            format!("Call tree (filter: {})", app.filter)
                        }),
                )
                .row_highlight_style(
                    Style::default()
                        .add_modifier(Modifier::REVERSED)
                        .add_modifier(Modifier::BOLD),
                )
                .highlight_symbol(">> ");
            frame.render_stateful_widget(table, chunks[1], &mut table_state);

            let status = if app.filtering {
                format!("Filter: {}_  (Enter apply, Esc cancel)", app.filter)
            } else if !app.filter.is_empty() {
                format!(
                    "{} | filter=\"{}\" (matches re-rooted at depth 0) | e/c  h/l scroll  /  q",
                    app.status, app.filter
                )
            } else {
                format!(
                    "{} | j/k move  e expand  c collapse  h/l scroll  / filter  q quit",
                    app.status
                )
            };
            frame.render_widget(Paragraph::new(status), chunks[2]);
            frame.render_widget(Clear, chunks[3]);
        })?;

        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }

        if app.filtering {
            match key.code {
                KeyCode::Esc => {
                    app.filtering = false;
                    app.filter.clear();
                    app.cursor = 0;
                    app.symbol_scroll = 0;
                }
                KeyCode::Enter => {
                    app.filtering = false;
                    app.cursor = 0;
                    app.symbol_scroll = 0;
                }
                KeyCode::Backspace => {
                    app.filter.pop();
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    app.filter.push(c);
                }
                _ => {}
            }
            continue;
        }

        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => break,
            KeyCode::Char('j') | KeyCode::Down => {
                let len = app.visible_rows().len();
                if len > 0 {
                    app.cursor = (app.cursor + 1).min(len - 1);
                    app.symbol_scroll = 0;
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                app.cursor = app.cursor.saturating_sub(1);
                app.symbol_scroll = 0;
            }
            KeyCode::PageDown => {
                let len = app.visible_rows().len();
                if len > 0 {
                    app.cursor = (app.cursor + 20).min(len - 1);
                    app.symbol_scroll = 0;
                }
            }
            KeyCode::PageUp => {
                app.cursor = app.cursor.saturating_sub(20);
                app.symbol_scroll = 0;
            }
            KeyCode::Home => {
                app.cursor = 0;
                app.symbol_scroll = 0;
            }
            KeyCode::End => {
                let len = app.visible_rows().len();
                if len > 0 {
                    app.cursor = len - 1;
                    app.symbol_scroll = 0;
                }
            }
            KeyCode::Char('h') | KeyCode::Left => {
                app.symbol_scroll = app.symbol_scroll.saturating_sub(8);
            }
            KeyCode::Char('l') | KeyCode::Right => {
                app.symbol_scroll = app.symbol_scroll.saturating_add(8);
            }
            KeyCode::Char('e') => app.expand_node(),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => break,
            KeyCode::Char('c') => app.collapse_node(),
            KeyCode::Char('/') => {
                app.filtering = true;
                app.filter.clear();
            }
            _ => {}
        }
    }
    Ok(())
}

/// Render indented `marker + name`, honoring horizontal scroll and fitting to `width`.
fn fit_symbol(depth: usize, marker: char, name: &str, scroll: usize, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let indent = "  ".repeat(depth);
    let full = format!("{indent}{marker} {name}");
    let chars: Vec<char> = full.chars().collect();
    if chars.is_empty() {
        return String::new();
    }
    let max_scroll = chars.len().saturating_sub(1);
    let start = scroll.min(max_scroll);
    let visible = &chars[start..];
    if visible.len() <= width {
        return visible.iter().collect();
    }
    if width == 1 {
        return "…".to_string();
    }
    let mut out: String = visible[..width - 1].iter().collect();
    out.push('…');
    out
}
