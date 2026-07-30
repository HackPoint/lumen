use std::{
    io,
    path::PathBuf,
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

use clap::{Parser, Subcommand};
use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
    tty::IsTty,
};
use ratatui::{Terminal, backend::CrosstermBackend};

mod data;
mod report;
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

    /// Bare `lumen` stays the TUI; a subcommand opts out of it.
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Render a fault report, and optionally file it as a GitHub issue.
    Report {
        /// Print the issue body and exit without filing it.
        #[arg(long)]
        dry_run: bool,
        /// Render this fault fixture instead of the database. Kept permanently: it is
        /// how the renderer is snapshot-tested without standing up a database.
        #[arg(long, value_name = "FILE")]
        faults: Option<PathBuf>,
        /// Embed the contents of affected in-workspace files. Prints a manifest of
        /// exactly what will be embedded first.
        #[arg(long)]
        include_source: bool,
        /// Issue tracker to file against.
        #[arg(long, default_value = report::DEFAULT_REPO)]
        repo: String,
        /// Required to actually file. Without it, filing stops after the preview.
        #[arg(long)]
        yes: bool,
    },
}

fn main() {
    let cli = Cli::parse();

    // Before the TTY branch below, not after: `lumen report > issue.md` has a non-TTY
    // stdout, and would otherwise print the one-shot gauge line instead of the report.
    if let Some(cmd) = cli.cmd {
        std::process::exit(run_subcommand(cmd));
    }

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

// ratatui 0.30 gave `Backend` an associated `Error` type, so the backend's error
// has to be convertible into the `io::Error` this returns (it is for Crossterm).
fn run_loop<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    state: &Arc<Mutex<data::AppState>>,
    no_anim: bool,
) -> io::Result<()>
where
    io::Error: From<B::Error>,
{
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

/// Returns the process exit code.
fn run_subcommand(cmd: Cmd) -> i32 {
    match cmd {
        Cmd::Report {
            dry_run,
            faults,
            include_source,
            repo,
            yes,
        } => run_report(dry_run, faults, include_source, &repo, yes),
    }
}

fn run_report(
    dry_run: bool,
    fixture: Option<PathBuf>,
    include_source: bool,
    repo: &str,
    yes: bool,
) -> i32 {
    let loaded = match &fixture {
        Some(p) => report::load_faults(p),
        None => open_db().and_then(|c| report::load_faults_from_db(&c)),
    };
    let faults = match loaded {
        Ok(f) => f,
        Err(e) => {
            eprintln!("lumen report: {e}");
            return 1;
        }
    };

    let env = report::Environment::collect();
    let opts = report::RenderOpts { include_source };

    // The manifest goes to stderr before the body goes anywhere, so `--include-source`
    // can never upload a file the operator has not been shown.
    if include_source {
        let manifest = report::source_manifest(&faults, &env);
        if manifest.is_empty() {
            eprintln!("lumen report: --include-source matched no in-workspace file");
        } else {
            eprintln!("lumen report: --include-source will embed these files verbatim:");
            for (label, bytes) in manifest {
                eprintln!("  {label} ({bytes} bytes)");
            }
        }
    }

    let Some(body) = report::render(&faults, &env, &opts) else {
        eprintln!("lumen report: no faults recorded — nothing to report");
        return 0;
    };

    if dry_run {
        print!("{body}");
        return 0;
    }

    // Filing is outward-facing and hard to undo, so it needs an explicit --yes even
    // once the body is known good.
    if !yes {
        print!("{body}");
        eprintln!(
            "\nlumen report: not filed. Re-run with --yes to file against {repo}, \
             or --dry-run to suppress this notice."
        );
        return 2;
    }

    let fp = report::fingerprint(&faults, &env);
    match report::file_issue(repo, &report::title_from(&body), &body, &fp) {
        Ok(report::Filed::Created(url)) => {
            println!("opened {url}");
            0
        }
        Ok(report::Filed::Commented(url)) => {
            println!("commented on existing issue for this fingerprint: {url}");
            0
        }
        Err(e) => {
            eprintln!("lumen report: {e}");
            1
        }
    }
}

/// Open the metering database read-write: `report` drains the fault spool into it.
fn open_db() -> Result<rusqlite::Connection, String> {
    let path = lumen_core::meter::db_path()
        .ok_or_else(|| "cannot resolve a database path; set LUMEN_DB".to_string())?;
    // connect_db applies DDL + MIGRATIONS, so the faults table exists before the drain.
    lumen_core::meter::connect_db(&path).map_err(|e| format!("cannot open {}: {e}", path.display()))
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
