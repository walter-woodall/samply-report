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
        }
    }

    fn visible_rows(&self) -> Vec<crate::tree::VisibleRow> {
        let rows = self.tree.flatten(&self.expanded);
        if self.filter.is_empty() {
            return rows;
        }
        let filter = self.filter.to_lowercase();
        rows.into_iter()
            .filter(|row| {
                format_frame(&row.frame, &self.libs)
                    .to_lowercase()
                    .contains(&filter)
            })
            .collect()
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

            let table_rows = rows.iter().map(|row| {
                let marker = if !row.has_children {
                    " "
                } else if row.expanded {
                    "▼"
                } else {
                    "▶"
                };
                let indent = "  ".repeat(row.depth);
                let mut name = format_frame(&row.frame, &app.libs);
                if name.len() > 120 {
                    name.truncate(117);
                    name.push('…');
                }
                let symbol = format!("{indent}{marker} {name}");
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
                Constraint::Min(20),
            ];
            let table = Table::new(table_rows, widths)
                .header(header)
                .block(Block::default().borders(Borders::ALL).title("Call tree"))
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
                    "{} | filter=\"{}\" | e expand  c collapse  / filter  q quit",
                    app.status, app.filter
                )
            } else {
                format!(
                    "{} | j/k move  e expand  c collapse  / filter  q quit",
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
                }
                KeyCode::Enter => {
                    app.filtering = false;
                    app.cursor = 0;
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
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                app.cursor = app.cursor.saturating_sub(1);
            }
            KeyCode::PageDown => {
                let len = app.visible_rows().len();
                if len > 0 {
                    app.cursor = (app.cursor + 20).min(len - 1);
                }
            }
            KeyCode::PageUp => {
                app.cursor = app.cursor.saturating_sub(20);
            }
            KeyCode::Home => app.cursor = 0,
            KeyCode::End => {
                let len = app.visible_rows().len();
                if len > 0 {
                    app.cursor = len - 1;
                }
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
