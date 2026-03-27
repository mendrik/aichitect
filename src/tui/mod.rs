pub mod app;
pub mod events;
pub mod input;
pub mod ui;

use anyhow::Result;
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture,
        EnableBracketedPaste, DisableBracketedPaste,
        Event,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::io;
use std::time::Duration;
use tokio::sync::mpsc;

use crate::config::Config;
use crate::document::Document;
use app::{App, AppEvent};

pub async fn run(config: Config, doc: Document) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture, EnableBracketedPaste)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let (event_tx, mut event_rx) = mpsc::channel::<AppEvent>(64);
    let mut app = App::new(config, doc, event_tx.clone());

    loop {
        terminal.draw(|f| ui::draw(f, &mut app))?;
        app.spinner_tick = app.spinner_tick.wrapping_add(1);

        if event::poll(Duration::from_millis(50))? {
            match event::read()? {
                Event::Key(key) => events::handle_key(&mut app, key).await,
                Event::Mouse(mouse) => events::handle_mouse(&mut app, mouse),
                Event::Paste(s) => events::handle_paste(&mut app, s),
                Event::Resize(w, h) => {
                    app.terminal_width = w;
                    app.terminal_height = h;
                }
                _ => {}
            }
        }

        while let Ok(ev) = event_rx.try_recv() {
            app.handle_event(ev).await;
        }

        if app.should_quit {
            break;
        }
    }

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture,
        DisableBracketedPaste,
    )?;
    terminal.show_cursor()?;

    Ok(())
}
