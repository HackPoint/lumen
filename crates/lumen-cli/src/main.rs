use std::{
    io,
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

use clap::Parser;
use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
    tty::IsTty,
};
use ratatui::{Terminal, backend::CrosstermBackend};

mod data;
mod ui;

#[derive(Parser)]
#[command(
    name = "lumen",
    version,
    about = "Live Claude Code context + cost monitor"
)]
struct Cli {
    /// Disable pulse-ring animation
    #[arg(long)]
    no_anim: bool,
}

fn main() {
    let cli = Cli::parse();

    // Piped output → one-shot plain text + exit 0
    if !io::stdout().is_tty() {
        oneshot_print();
        return;
    }

    setup_panic_hook();

    let state: Arc<Mutex<data::AppState>> = Arc::default();
    let state_bg = Arc::clone(&state);
    thread::spawn(move || data::run(state_bg));

    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, cursor::Hide).unwrap();
    terminal::enable_raw_mode().unwrap();

    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend).unwrap();

    let result = run_loop(&mut terminal, &state, cli.no_anim);

    let _ = terminal::disable_raw_mode();
    let _ = execute!(io::stdout(), LeaveAlternateScreen, cursor::Show);

    if let Err(e) = result {
        eprintln!("lumen: {e}");
        std::process::exit(1);
    }
}

fn run_loop<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    state: &Arc<Mutex<data::AppState>>,
    no_anim: bool,
) -> io::Result<()> {
    loop {
        {
            let mut s = state.lock().unwrap();
            s.tick = s.tick.wrapping_add(1);
        }

        terminal.draw(|f| ui::render(f, &state.lock().unwrap(), no_anim))?;

        if event::poll(Duration::from_millis(400))?
            && let Event::Key(key) = event::read()?
        {
            let quit = matches!(key.code, KeyCode::Char('q') | KeyCode::Esc)
                || (key.code == KeyCode::Char('c')
                    && key.modifiers.contains(KeyModifiers::CONTROL));
            if quit {
                return Ok(());
            }
        }
    }
}

fn oneshot_print() {
    match data::read_db_oneshot() {
        Some(d) => {
            let pct = (d.fill * 100).checked_div(d.window).unwrap_or(0);
            println!("fill={}% ({}/{}) model={}", pct, d.fill, d.window, d.model);
        }
        None => println!("no data"),
    }
}

fn setup_panic_hook() {
    let original = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = terminal::disable_raw_mode();
        let _ = execute!(io::stderr(), LeaveAlternateScreen, cursor::Show);
        original(info);
    }));
}
