use std::collections::BTreeSet;
use std::io;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, TableState, Widget};
use ratatui::{DefaultTerminal, Frame};
use sysinfo::Pid;

use crate::error::AppResult;
use crate::process::{ProcessCatalog, ProcessRecord};

pub fn pick_interactive(catalog: &ProcessCatalog, verbose: bool) -> AppResult<BTreeSet<Pid>> {
    let records = catalog.process_records();
    if records.is_empty() {
        return Ok(BTreeSet::new());
    }

    let mut terminal =
        init_terminal().map_err(|error| format!("interactive mode failed: {error}"))?;
    let result = run_app(&mut terminal, records, verbose);
    restore_terminal(terminal).map_err(|error| format!("failed to restore terminal: {error}"))?;

    result.map(|state| state.selected_pids().into_iter().collect::<BTreeSet<_>>())
}

fn run_app(
    terminal: &mut DefaultTerminal,
    records: Vec<ProcessRecord>,
    verbose: bool,
) -> AppResult<AppState> {
    let mut state = AppState::new(records, verbose);
    drain_pending_events()?;

    loop {
        terminal
            .draw(|frame| draw(frame, &mut state))
            .map_err(|error| format!("failed to draw interactive UI: {error}"))?;

        if event::poll(Duration::from_millis(200))
            .map_err(|error| format!("failed to read terminal events: {error}"))?
            && let Event::Key(key) =
                event::read().map_err(|error| format!("failed to read key event: {error}"))?
            && handle_key_event(&mut state, key)
        {
            return Ok(state);
        }
    }
}

fn init_terminal() -> io::Result<DefaultTerminal> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    ratatui::Terminal::new(ratatui::backend::CrosstermBackend::new(stdout))
}

fn restore_terminal(mut terminal: DefaultTerminal) -> io::Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

fn drain_pending_events() -> AppResult<()> {
    while event::poll(Duration::from_millis(0))
        .map_err(|error| format!("failed to drain terminal events: {error}"))?
    {
        let _ = event::read().map_err(|error| format!("failed to read terminal event: {error}"))?;
    }
    Ok(())
}

fn handle_key_event(state: &mut AppState, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Esc => {
            state.cancelled = true;
            true
        }
        KeyCode::Enter => !state.selected.is_empty(),
        KeyCode::Up => {
            state.move_up();
            false
        }
        KeyCode::Down => {
            state.move_down();
            false
        }
        KeyCode::PageUp => {
            state.page_up();
            false
        }
        KeyCode::PageDown => {
            state.page_down();
            false
        }
        KeyCode::Home => {
            state.jump_to_start();
            false
        }
        KeyCode::End => {
            state.jump_to_end();
            false
        }
        KeyCode::Char(' ') => {
            state.toggle_current();
            false
        }
        KeyCode::Backspace => {
            state.pop_query();
            false
        }
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            state.cancelled = true;
            true
        }
        KeyCode::Char(ch)
            if !key.modifiers.contains(KeyModifiers::CONTROL)
                && !key.modifiers.contains(KeyModifiers::ALT) =>
        {
            state.push_query(ch);
            false
        }
        _ => false,
    }
}

fn draw(frame: &mut Frame<'_>, state: &mut AppState) {
    let vertical = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(8),
        Constraint::Length(2),
    ]);
    let [search_area, table_area, help_area] = vertical.areas(frame.area());

    render_search(frame, search_area, state);
    render_table(frame, table_area, state);
    render_help(frame, help_area, state);
}

fn render_search(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    let title = format!(
        " Search {} ",
        if state.query.is_empty() {
            "(type to filter)"
        } else {
            ""
        }
    );
    let input = Paragraph::new(state.query.clone()).block(
        Block::bordered()
            .title(title)
            .border_style(Style::default().fg(Color::Cyan)),
    );
    frame.render_widget(input, area);
}

fn render_table(frame: &mut Frame<'_>, area: Rect, state: &mut AppState) {
    state.sync_table_state();

    let header = Row::new([
        Cell::from("Sel"),
        Cell::from("PID"),
        Cell::from("App"),
        Cell::from("Process"),
        Cell::from("CPU"),
        Cell::from("Memory"),
        Cell::from("Ports"),
    ])
    .style(
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )
    .height(1);

    let filtered_records = state.filtered_records();
    let rows = filtered_records.into_iter().map(|record| {
        let selected = if state.is_selected(record.pid) {
            "[x]"
        } else {
            "[ ]"
        };

        let process_name = if state.verbose && !record.cmd.is_empty() {
            format!("{} | {}", record.name, truncate(&record.cmd, 48))
        } else {
            record.name.clone()
        };

        let cpu_style = cpu_style(record.cpu_usage);
        let mem_style = memory_style(record.memory_bytes);

        Row::new([
            Cell::from(selected),
            Cell::from(record.pid.as_u32().to_string()),
            Cell::from(truncate(&record.app_name, 22)),
            Cell::from(truncate(&process_name, 42)),
            Cell::from(format!("{:>5.1}%", record.cpu_usage)).style(cpu_style),
            Cell::from(format_memory(record.memory_bytes)).style(mem_style),
            Cell::from(format_ports(&record.ports)),
        ])
    });

    let table = Table::new(
        rows,
        [
            Constraint::Length(5),
            Constraint::Length(7),
            Constraint::Length(24),
            Constraint::Percentage(45),
            Constraint::Length(8),
            Constraint::Length(10),
            Constraint::Min(8),
        ],
    )
    .header(header)
    .row_highlight_style(
        Style::default()
            .bg(Color::Rgb(24, 34, 54))
            .add_modifier(Modifier::BOLD),
    )
    .block(
        Block::default()
            .title(" Processes ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Blue)),
    );

    frame.render_stateful_widget(table, area, &mut state.table_state);

    if state.filtered_indexes.is_empty() {
        frame.render_widget(Clear, centered_rect(50, 10, area));
        frame.render_widget(EmptyState, centered_rect(50, 10, area));
    }
}

fn render_help(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    let total = state.filtered_records().len();
    let selected = state.selected.len();
    let line = Line::from(vec![
        "Type to filter  ".dark_gray(),
        "Space".cyan().bold(),
        " toggle  ".dark_gray(),
        "Enter".green().bold(),
        " confirm  ".dark_gray(),
        "Esc".yellow().bold(),
        " cancel  ".dark_gray(),
        format!("Showing {total}  Selected {selected}").white(),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

fn centered_rect(horizontal: u16, vertical: u16, area: Rect) -> Rect {
    let vertical_layout = Layout::vertical([
        Constraint::Percentage((100 - vertical) / 2),
        Constraint::Percentage(vertical),
        Constraint::Percentage((100 - vertical) / 2),
    ]);
    let [_, middle, _] = vertical_layout.areas(area);
    let horizontal_layout = Layout::horizontal([
        Constraint::Percentage((100 - horizontal) / 2),
        Constraint::Percentage(horizontal),
        Constraint::Percentage((100 - horizontal) / 2),
    ]);
    let [_, center, _] = horizontal_layout.areas(middle);
    center
}

fn cpu_style(cpu: f32) -> Style {
    if cpu >= 60.0 {
        Style::default().fg(Color::Red)
    } else if cpu >= 25.0 {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::Green)
    }
}

fn memory_style(memory: u64) -> Style {
    if memory >= 1_500_000_000 {
        Style::default().fg(Color::Red)
    } else if memory >= 512_000_000 {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::Green)
    }
}

fn format_ports(ports: &BTreeSet<u16>) -> String {
    if ports.is_empty() {
        return "-".to_string();
    }

    ports
        .iter()
        .take(4)
        .map(|port| format!(":{port}"))
        .collect::<Vec<_>>()
        .join(",")
}

fn format_memory(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;

    let bytes = bytes as f64;
    if bytes >= GB {
        format!("{:.1}G", bytes / GB)
    } else if bytes >= MB {
        format!("{:.1}M", bytes / MB)
    } else if bytes >= KB {
        format!("{:.1}K", bytes / KB)
    } else {
        format!("{:.0}B", bytes)
    }
}

fn truncate(value: &str, width: usize) -> String {
    let mut result = value.chars().take(width).collect::<String>();
    if value.chars().count() > width && width > 1 {
        result.pop();
        result.push('~');
    }
    result
}

#[derive(Default)]
struct EmptyState;

impl Widget for EmptyState {
    fn render(self, area: Rect, buf: &mut Buffer) {
        Paragraph::new("No matching processes")
            .block(Block::bordered().title(" Filter "))
            .render(area, buf);
    }
}

struct AppState {
    records: Vec<ProcessRecord>,
    filtered_indexes: Vec<usize>,
    selected: BTreeSet<Pid>,
    cursor: usize,
    query: String,
    verbose: bool,
    cancelled: bool,
    table_state: TableState,
}

impl AppState {
    fn new(records: Vec<ProcessRecord>, verbose: bool) -> Self {
        let filtered_indexes = (0..records.len()).collect::<Vec<_>>();
        let mut table_state = TableState::default();
        table_state.select(Some(0));

        Self {
            records,
            filtered_indexes,
            selected: BTreeSet::new(),
            cursor: 0,
            query: String::new(),
            verbose,
            cancelled: false,
            table_state,
        }
    }

    fn filtered_records(&self) -> Vec<&ProcessRecord> {
        self.filtered_indexes
            .iter()
            .map(|index| &self.records[*index])
            .collect()
    }

    fn selected_pids(&self) -> Vec<Pid> {
        if self.cancelled {
            Vec::new()
        } else {
            self.selected.iter().copied().collect()
        }
    }

    fn is_selected(&self, pid: Pid) -> bool {
        self.selected.contains(&pid)
    }

    fn sync_table_state(&mut self) {
        if self.filtered_indexes.is_empty() {
            self.cursor = 0;
            self.table_state.select(None);
        } else {
            self.cursor = self.cursor.min(self.filtered_indexes.len() - 1);
            self.table_state.select(Some(self.cursor));
        }
    }

    fn move_up(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
        }
    }

    fn move_down(&mut self) {
        if !self.filtered_indexes.is_empty() && self.cursor + 1 < self.filtered_indexes.len() {
            self.cursor += 1;
        }
    }

    fn page_up(&mut self) {
        self.cursor = self.cursor.saturating_sub(10);
    }

    fn page_down(&mut self) {
        if !self.filtered_indexes.is_empty() {
            self.cursor = (self.cursor + 10).min(self.filtered_indexes.len() - 1);
        }
    }

    fn jump_to_start(&mut self) {
        self.cursor = 0;
    }

    fn jump_to_end(&mut self) {
        if !self.filtered_indexes.is_empty() {
            self.cursor = self.filtered_indexes.len() - 1;
        }
    }

    fn toggle_current(&mut self) {
        if let Some(pid) = self.current_pid() {
            if !self.selected.insert(pid) {
                self.selected.remove(&pid);
            }
        }
    }

    fn current_pid(&self) -> Option<Pid> {
        self.filtered_indexes
            .get(self.cursor)
            .map(|index| self.records[*index].pid)
    }

    fn push_query(&mut self, ch: char) {
        self.query.push(ch);
        self.refresh_filter();
    }

    fn pop_query(&mut self) {
        self.query.pop();
        self.refresh_filter();
    }

    fn refresh_filter(&mut self) {
        self.filtered_indexes = self
            .records
            .iter()
            .enumerate()
            .filter_map(|(index, record)| record_matches(record, &self.query).then_some(index))
            .collect();
        self.cursor = 0;
        self.sync_table_state();
    }
}

fn record_matches(record: &ProcessRecord, query: &str) -> bool {
    if query.trim().is_empty() {
        return true;
    }

    let query = query.to_lowercase();
    let ports = format_ports(&record.ports);
    record.app_name.to_lowercase().contains(&query)
        || record.name.to_lowercase().contains(&query)
        || record.cmd.to_lowercase().contains(&query)
        || record.pid.as_u32().to_string().contains(&query)
        || ports.to_lowercase().contains(&query)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use sysinfo::Pid;

    use super::{AppState, format_memory, record_matches};
    use crate::process::ProcessRecord;

    fn sample_record() -> ProcessRecord {
        ProcessRecord {
            pid: Pid::from_u32(42),
            app_name: "chrome".to_string(),
            name: "chrome.exe".to_string(),
            cmd: "chrome.exe --profile".to_string(),
            cpu_usage: 12.5,
            memory_bytes: 512 * 1024 * 1024,
            ports: BTreeSet::from([9222]),
        }
    }

    #[test]
    fn filter_matches_app_name_and_ports() {
        let record = sample_record();
        assert!(record_matches(&record, "chrome"));
        assert!(record_matches(&record, "9222"));
        assert!(!record_matches(&record, "firefox"));
    }

    #[test]
    fn state_resets_cursor_on_filter_change() {
        let records = vec![sample_record()];
        let mut state = AppState::new(records, false);
        state.push_query('c');
        assert_eq!(state.cursor, 0);
    }

    #[test]
    fn memory_format_uses_human_units() {
        assert_eq!(format_memory(1024), "1.0K");
        assert_eq!(format_memory(1024 * 1024), "1.0M");
    }
}
