use anyhow::Result;
use crossterm::{
    ExecutableCommand,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::prelude::{CrosstermBackend, Terminal};
use std::io::{Stdout, stdout};

pub struct Tui {
    pub terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl Tui {
    pub fn new() -> Result<Self> {
        let terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
        Ok(Self { terminal })
    }

    pub fn enter(&mut self) -> Result<()> {
        stdout().execute(EnterAlternateScreen)?;
        enable_raw_mode()?;
        self.terminal.clear()?;
        Ok(())
    }

    pub fn exit(&mut self) -> Result<()> {
        stdout().execute(LeaveAlternateScreen)?;
        disable_raw_mode()?;
        self.terminal.show_cursor()?;
        Ok(())
    }
}
