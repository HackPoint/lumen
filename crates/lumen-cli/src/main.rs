use std::{
    io,
    path::PathBuf,
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

use clap::{Args, Parser, Subcommand};
use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
    tty::IsTty,
};
use lumen_core::report;
use ratatui::{Terminal, backend::CrosstermBackend};

mod data;
mod doctor;
mod import_ledger;
mod savings;
mod ui;

#[derive(Parser)]
#[command(
    name = "lumen",
    version,
    about = "What Lumen saved you, and Claude Code's live context and cost"
)]
struct Cli {
    /// Before 1.9.0 bare `lumen` was the dashboard and this flag went with it. Kept, hidden, so
    /// an old `lumen --no-anim` still opens the dashboard; the spelling now is
    /// `lumen dash --no-anim`.
    #[arg(long, hide = true)]
    no_anim: bool,

    /// Bare `lumen`'s line as it stood at this time (unix milliseconds or ISO-8601), as
    /// `lumen report --as-of` gives the detail. For tests and for reproducing a figure.
    #[arg(long, hide = true, value_name = "TIME")]
    as_of: Option<String>,

    /// Bare `lumen` prints the headline; each subcommand is one of the views behind it.
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Live terminal dashboard: context fill, model and cost. Piped, it prints one line.
    Dash {
        /// Disable pulse-ring animation
        #[arg(long)]
        no_anim: bool,
    },
    /// What Lumen saved, in detail: each period, how it is measured, the best and worst calls.
    Report {
        /// Print the whole summary, every call line included, as the app receives it.
        #[arg(long)]
        json: bool,
        /// The summary as it stood at this time (unix milliseconds or ISO-8601). For tests and
        /// for reproducing a figure; the default is now.
        #[arg(long, hide = true, value_name = "TIME")]
        as_of: Option<String>,
        #[command(flatten)]
        legacy: LegacyFaultFlags,
    },
    /// Render a fault report, and optionally file it as a GitHub issue.
    Faults(FaultArgs),
    /// Ask a running Lumen to show its window — a way in when the menu-bar icon is missing.
    Show,
    /// Print everything a bug report needs: status-item preferences, processes, paths, logs.
    Doctor,
    /// Bring in a Lumen ledger an older install left behind: a folder an old app re-created, a
    /// restored copy of the previous folder, or a ledger Lumen set aside. Rows already in this
    /// ledger are not added twice; the folder moves into `legacy-imports` beside it.
    ImportLedger {
        /// The folder, its `lumen.db`, or a set-aside ledger such as `lumen.db.pre-legacy`.
        path: PathBuf,
        /// Say what would be added, and change nothing.
        #[arg(long)]
        dry_run: bool,
    },
}

/// The fault report's flags.
#[derive(Args, Debug, Clone, PartialEq, Eq)]
struct FaultArgs {
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
}

/// The fault report's flags as `lumen report` took them until 1.9.0, when the fault report
/// moved to `lumen faults`.
///
/// Accepted and hidden rather than refused: they are in shell history, in docs written before
/// 1.9.0 and in an older plugin's `/lumen-report` command, and a fault report that stops
/// working is the one a user will not find out about until they need it. `repo` has no default
/// here, so a given `--repo` can be told apart from none.
#[derive(Args, Debug, Clone, PartialEq, Eq)]
struct LegacyFaultFlags {
    #[arg(long, hide = true)]
    dry_run: bool,
    #[arg(long, hide = true, value_name = "FILE")]
    faults: Option<PathBuf>,
    #[arg(long, hide = true)]
    include_source: bool,
    #[arg(long, hide = true)]
    repo: Option<String>,
    #[arg(long, hide = true)]
    yes: bool,
}

impl LegacyFaultFlags {
    /// The fault report these flags ask for, or `None` when no fault flag was given, which is
    /// the new `lumen report`.
    fn into_fault_args(self) -> Option<FaultArgs> {
        let LegacyFaultFlags {
            dry_run,
            faults,
            include_source,
            repo,
            yes,
        } = self;
        if !(dry_run || faults.is_some() || include_source || repo.is_some() || yes) {
            return None;
        }
        Some(FaultArgs {
            dry_run,
            faults,
            include_source,
            repo: match repo {
                Some(r) => r,
                None => report::DEFAULT_REPO.to_string(),
            },
            yes,
        })
    }
}

/// What a command line asks for, decided before anything runs so the routing is testable
/// without a terminal, a ledger or a network.
#[derive(Debug, PartialEq, Eq)]
enum Route {
    /// Bare `lumen`: the one headline line, as of now or as of `as_of`.
    Headline {
        as_of: Option<String>,
    },
    /// The dashboard. `moved` is set for the pre-1.9.0 spelling, bare `lumen --no-anim`.
    Dash {
        no_anim: bool,
        moved: bool,
    },
    /// `lumen report` with no fault flag: the savings detail.
    SavingsDetail {
        json: bool,
        as_of: Option<String>,
    },
    /// The fault report. `moved` is set for the pre-1.9.0 spelling, `lumen report <flags>`.
    Faults {
        args: FaultArgs,
        moved: bool,
    },
    Show,
    Doctor,
    ImportLedger {
        path: PathBuf,
        dry_run: bool,
    },
}

fn route(cli: Cli) -> Route {
    match cli.cmd {
        None if cli.no_anim => Route::Dash {
            no_anim: true,
            moved: true,
        },
        None => Route::Headline { as_of: cli.as_of },
        Some(Cmd::Dash { no_anim }) => Route::Dash {
            no_anim: no_anim || cli.no_anim,
            moved: false,
        },
        Some(Cmd::Report {
            json,
            as_of,
            legacy,
        }) => match legacy.into_fault_args() {
            Some(args) => Route::Faults { args, moved: true },
            None => Route::SavingsDetail { json, as_of },
        },
        Some(Cmd::Faults(args)) => Route::Faults { args, moved: false },
        Some(Cmd::Show) => Route::Show,
        Some(Cmd::Doctor) => Route::Doctor,
        Some(Cmd::ImportLedger { path, dry_run }) => Route::ImportLedger { path, dry_run },
    }
}

/// The one line bare `lumen` prints, from the savings summary: the tray menu's line is made
/// from the same summary by the same module, so the two cannot disagree. It names `lumen dash`
/// because until 1.9.0 bare `lumen` was the dashboard.
fn headline_line(as_of: Option<&str>) -> (String, i32) {
    let at = match as_of.map(savings::parse_as_of) {
        Some(Ok(t)) => t,
        Some(Err(e)) => return (format!("lumen: --as-of: {e}"), 2),
        None => lumen_core::repeat::now_ms(),
    };
    match savings::summary(at) {
        Ok(s) => (lumen_core::savings::text::headline_line(&s), 0),
        Err(e) => (savings::headline_error(&e), 1),
    }
}

/// `lumen report`: the savings detail, or with `--json` the summary itself.
fn savings_detail(json: bool, as_of: Option<&str>) -> i32 {
    let at = match as_of.map(savings::parse_as_of) {
        Some(Ok(t)) => t,
        Some(Err(e)) => {
            eprintln!("lumen report: --as-of: {e}");
            return 2;
        }
        None => lumen_core::repeat::now_ms(),
    };
    let s = match savings::summary(at) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("lumen report: {e}");
            return 1;
        }
    };
    if json {
        match serde_json::to_string_pretty(&s) {
            Ok(text) => println!("{text}"),
            Err(e) => {
                eprintln!("lumen report: cannot serialize the summary: {e}");
                return 1;
            }
        }
    } else {
        print!("{}", savings::render_detail(&s));
    }
    0
}

/// True when `--no-anim` was given to a command it does nothing for. Said rather than ignored,
/// as the moved spellings are, so a flag that has stopped mattering does not look like it works.
fn stray_no_anim(cli: &Cli) -> bool {
    cli.no_anim && !matches!(cli.cmd, None | Some(Cmd::Dash { .. }))
}

/// True when the hidden `--as-of` was given before a command, where only bare `lumen` reads it.
fn stray_as_of(cli: &Cli) -> bool {
    cli.as_of.is_some() && cli.cmd.is_some()
}

fn main() {
    let cli = Cli::parse();
    if stray_no_anim(&cli) {
        eprintln!("lumen: --no-anim only changes `lumen dash`, so it does nothing here");
    }
    if stray_as_of(&cli) {
        eprintln!("lumen: --as-of before a command does nothing; `lumen report --as-of` takes it");
    }
    let code = match route(cli) {
        Route::Headline { as_of } => {
            let (line, code) = headline_line(as_of.as_deref());
            if code == 2 {
                eprintln!("{line}");
            } else {
                println!("{line}");
            }
            code
        }
        Route::SavingsDetail { json, as_of } => savings_detail(json, as_of.as_deref()),
        // Decided by the command, never by the terminal: `lumen faults > issue.md` has a
        // non-TTY stdout, and only the dashboard trades itself for a one-shot line then.
        Route::Faults { args, moved } => {
            if moved {
                eprintln!(
                    "lumen: the fault report is now `lumen faults`; `lumen report` with its \
                     flags still runs it"
                );
            }
            run_faults(args)
        }
        Route::Show => doctor::show(),
        Route::Doctor => {
            print!("{}", doctor::render(&doctor::collect()));
            0
        }
        Route::ImportLedger { path, dry_run } => import_ledger::run(&path, dry_run),
        Route::Dash { no_anim, moved } => {
            if moved {
                eprintln!(
                    "lumen: the dashboard is now `lumen dash`; running `lumen dash --no-anim`"
                );
            }
            dash(no_anim)
        }
    };
    std::process::exit(code);
}

/// The terminal dashboard, or its one-shot line when stdout is not a terminal. Returns the
/// process exit code.
fn dash(no_anim: bool) -> i32 {
    // Piped output → one-shot plain text + exit 0
    if !io::stdout().is_tty() {
        oneshot_print();
        return 0;
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

    let result = run_loop(&mut terminal, &state, no_anim);

    let _ = terminal::disable_raw_mode();
    let _ = execute!(io::stdout(), LeaveAlternateScreen, cursor::Show);

    if let Err(e) = result {
        eprintln!("lumen: {e}");
        return 1;
    }
    0
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

/// `lumen faults`: render the fault report, and file it only with `--yes`. Returns the process
/// exit code.
fn run_faults(args: FaultArgs) -> i32 {
    let FaultArgs {
        dry_run,
        faults: fixture,
        include_source,
        repo,
        yes,
    } = args;
    let repo = repo.as_str();
    let loaded = match &fixture {
        Some(p) => report::load_faults(p),
        None => open_db().and_then(|c| report::load_faults_from_db(&c)),
    };
    let faults = match loaded {
        Ok(f) => f,
        Err(e) => {
            eprintln!("lumen faults: {e}");
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
            eprintln!("lumen faults: --include-source matched no in-workspace file");
        } else {
            eprintln!("lumen faults: --include-source will embed these files verbatim:");
            for (label, bytes) in manifest {
                eprintln!("  {label} ({bytes} bytes)");
            }
        }
    }

    let Some(body) = report::render(&faults, &env, &opts) else {
        eprintln!("lumen faults: no faults recorded — nothing to report");
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
            "\nlumen faults: not filed. Re-run with --yes to file against {repo}, \
             or --dry-run to suppress this notice."
        );
        return 2;
    }

    let fp = report::fingerprint(&faults, &env);
    match report::file_issue(repo, &report::title_from(&body), &body, &fp) {
        Ok(filing) => {
            // Every fallback is reported, not just the winner: a run that silently
            // switched routes hides that the preferred one is broken.
            for step in &filing.fell_back {
                eprintln!("lumen faults: {step}");
            }
            match filing.outcome {
                report::Filed::Created(url) => println!("opened {url} (via {})", filing.route),
                report::Filed::Commented(url) => println!(
                    "commented on the existing issue for this fingerprint: {url} (via {})",
                    filing.route
                ),
                // Not filed. Saying "opened" here would claim something that has not
                // happened yet -- the form is waiting on a human.
                report::Filed::Handoff(url) => println!(
                    "opened a prefilled form in your browser -- submit it there to file: {url}"
                ),
            }
            0
        }
        Err(e) => {
            eprintln!("lumen faults: {e}");
            1
        }
    }
}

/// Open the metering database read-write: `faults` drains the fault spool into it.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn routed(args: &[&str]) -> Route {
        let argv = std::iter::once("lumen").chain(args.iter().copied());
        route(Cli::try_parse_from(argv).unwrap_or_else(|e| panic!("{args:?}: {e}")))
    }

    fn fault_args(over: impl FnOnce(&mut FaultArgs)) -> FaultArgs {
        let mut a = FaultArgs {
            dry_run: false,
            faults: None,
            include_source: false,
            repo: report::DEFAULT_REPO.to_string(),
            yes: false,
        };
        over(&mut a);
        a
    }

    #[test]
    fn import_ledger_takes_a_path_and_a_dry_run_flag() {
        assert_eq!(
            routed(&["import-ledger", "/x/lumen.db"]),
            Route::ImportLedger {
                path: PathBuf::from("/x/lumen.db"),
                dry_run: false
            }
        );
        assert_eq!(
            routed(&["import-ledger", "/x", "--dry-run"]),
            Route::ImportLedger {
                path: PathBuf::from("/x"),
                dry_run: true
            }
        );
    }

    #[test]
    fn bare_lumen_is_the_headline() {
        assert_eq!(routed(&[]), Route::Headline { as_of: None });
        assert_eq!(
            routed(&["--as-of", "1791806400000"]),
            Route::Headline {
                as_of: Some("1791806400000".into())
            }
        );
    }

    #[test]
    fn as_of_before_a_command_is_said_to_do_nothing() {
        let parsed = |args: &[&str]| {
            let argv = std::iter::once("lumen").chain(args.iter().copied());
            Cli::try_parse_from(argv).unwrap()
        };
        assert!(stray_as_of(&parsed(&["--as-of", "1", "dash"])));
        assert!(!stray_as_of(&parsed(&["--as-of", "1"])));
        assert!(!stray_as_of(&parsed(&["report", "--as-of", "1"])));
    }

    #[test]
    fn a_headline_that_cannot_be_made_says_why_in_one_line_and_names_both_commands() {
        let line = savings::headline_error("no ledger at /x");
        assert!(!line.contains('\n'), "{line:?}");
        assert!(!line.contains('$'), "{line:?}");
        assert!(line.contains("lumen report"), "{line:?}");
        // Bare `lumen` was the dashboard until 1.9.0: the line says where it went.
        assert!(line.contains("lumen dash"), "{line:?}");
    }

    #[test]
    fn no_anim_is_said_to_do_nothing_wherever_it_does_nothing() {
        let parsed = |args: &[&str]| {
            let argv = std::iter::once("lumen").chain(args.iter().copied());
            Cli::try_parse_from(argv).unwrap()
        };
        for args in [
            &["--no-anim", "report"][..],
            &["--no-anim", "faults"][..],
            &["--no-anim", "doctor"][..],
        ] {
            assert!(stray_no_anim(&parsed(args)), "{args:?}");
        }
        for args in [
            &["--no-anim"][..],
            &["--no-anim", "dash"][..],
            &["dash", "--no-anim"][..],
            &["report"][..],
        ] {
            assert!(!stray_no_anim(&parsed(args)), "{args:?}");
        }
    }

    #[test]
    fn dash_is_the_dashboard_with_or_without_animation() {
        assert_eq!(
            routed(&["dash"]),
            Route::Dash {
                no_anim: false,
                moved: false
            }
        );
        for args in [&["dash", "--no-anim"][..], &["--no-anim", "dash"][..]] {
            assert_eq!(
                routed(args),
                Route::Dash {
                    no_anim: true,
                    moved: false
                },
                "{args:?}"
            );
        }
    }

    #[test]
    fn an_old_bare_no_anim_still_opens_the_dashboard_and_is_marked_moved() {
        assert_eq!(
            routed(&["--no-anim"]),
            Route::Dash {
                no_anim: true,
                moved: true
            }
        );
    }

    #[test]
    fn report_with_no_flag_is_the_savings_detail() {
        assert_eq!(
            routed(&["report"]),
            Route::SavingsDetail {
                json: false,
                as_of: None
            }
        );
        assert_eq!(
            routed(&["report", "--json", "--as-of", "1791806400000"]),
            Route::SavingsDetail {
                json: true,
                as_of: Some("1791806400000".into())
            }
        );
    }

    #[test]
    fn faults_takes_every_fault_flag() {
        assert_eq!(
            routed(&["faults"]),
            Route::Faults {
                args: fault_args(|_| {}),
                moved: false
            }
        );
        assert_eq!(
            routed(&[
                "faults",
                "--dry-run",
                "--faults",
                "f.json",
                "--include-source",
                "--repo",
                "o/r",
                "--yes"
            ]),
            Route::Faults {
                args: fault_args(|a| {
                    a.dry_run = true;
                    a.faults = Some(PathBuf::from("f.json"));
                    a.include_source = true;
                    a.repo = "o/r".into();
                    a.yes = true;
                }),
                moved: false
            }
        );
    }

    #[test]
    fn each_old_report_flag_routes_to_faults_as_it_did_and_is_marked_moved() {
        let cases: [(&[&str], FaultArgs); 5] = [
            (&["--dry-run"], fault_args(|a| a.dry_run = true)),
            (
                &["--faults", "f.json"],
                fault_args(|a| a.faults = Some(PathBuf::from("f.json"))),
            ),
            (
                &["--include-source"],
                fault_args(|a| a.include_source = true),
            ),
            (&["--repo", "o/r"], fault_args(|a| a.repo = "o/r".into())),
            (&["--yes"], fault_args(|a| a.yes = true)),
        ];
        for (flags, args) in cases {
            let mut argv = vec!["report"];
            argv.extend_from_slice(flags);
            assert_eq!(
                routed(&argv),
                Route::Faults { args, moved: true },
                "{argv:?}"
            );
        }
    }

    #[test]
    fn the_old_report_flags_are_hidden_from_its_help() {
        use clap::CommandFactory;
        let mut cmd = Cli::command();
        let help = cmd
            .find_subcommand_mut("report")
            .expect("a report subcommand")
            .render_help()
            .to_string();
        for flag in [
            "--dry-run",
            "--faults",
            "--include-source",
            "--repo",
            "--yes",
        ] {
            assert!(!help.contains(flag), "{flag} shows in:\n{help}");
        }
        let help = cmd
            .find_subcommand_mut("faults")
            .expect("a faults subcommand")
            .render_help()
            .to_string();
        for flag in [
            "--dry-run",
            "--faults",
            "--include-source",
            "--repo",
            "--yes",
        ] {
            assert!(help.contains(flag), "{flag} missing from:\n{help}");
        }
    }

    #[test]
    fn show_and_doctor_are_unchanged() {
        assert_eq!(routed(&["show"]), Route::Show);
        assert_eq!(routed(&["doctor"]), Route::Doctor);
    }
}
