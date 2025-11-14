use std::io::{self, Stdout};
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use super::app::App;

/// Spin up the terminal backend, enter the draw loop, and keep processing input
/// until the user quits.
pub fn run_app(app: &mut App) -> Result<()> {
    let mut stdout = io::stdout();
    enable_raw_mode().context("failed to enable raw mode")?;
    execute!(stdout, EnterAlternateScreen).context("failed to enter alternate screen")?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context("failed to create terminal backend")?;

    let result = loop {
        terminal
            .draw(|frame| app.draw(frame))
            .context("failed to draw frame")?;

        if event::poll(Duration::from_millis(250)).context("event polling failed")? {
            if let Event::Key(key_event) = event::read().context("failed to read event")? {
                if key_event.kind == KeyEventKind::Press {
                    if key_event.modifiers.contains(KeyModifiers::CONTROL) {
                        match key_event.code {
                            KeyCode::Char('e') => {
                                app.handle_ctrl_e()?;
                                continue;
                            }
                            KeyCode::Char('l') => {
                                app.handle_ctrl_l()?;
                                continue;
                            }
                            _ => {}
                        }
                    }

                    if app.handle_key(key_event.code)? {
                        break Ok(());
                    }
                }
            }
        }
    };

    cleanup_terminal(&mut terminal)?;
    result
}

fn cleanup_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
    disable_raw_mode().context("failed to disable raw mode")?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)
        .context("failed to leave alternate screen")?;
    terminal
        .show_cursor()
        .context("failed to restore cursor visibility")
}
