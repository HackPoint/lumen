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
    /// What Lumen saved, in detail. A placeholder until the savings engine lands.
    Report {
        #[command(flatten)]
        legacy: LegacyFaultFlags,
    },
    /// Render a fault report, and optionally file it as a GitHub issue.
    Faults(FaultArgs),
    /// Ask a running Lumen to show its window — a way in when the menu-bar icon is missing.
    Show,
    /// Print everything a bug report needs: status-item preferences, processes, paths, logs.
    Doctor,
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
    /// Bare `lumen`: the one headline line.
    Headline,
    /// The dashboard. `moved` is set for the pre-1.9.0 spelling, bare `lumen --no-anim`.
    Dash {
        no_anim: bool,
        moved: bool,
    },
    /// `lumen report` with no fault flag: the savings detail.
    SavingsDetail,
    /// The fault report. `moved` is set for the pre-1.9.0 spelling, `lumen report <flags>`.
    Faults {
        args: FaultArgs,
        moved: bool,
    },
    Show,
    Doctor,
}

fn route(cli: Cli) -> Route {
    match cli.cmd {
        None if cli.no_anim => Route::Dash {
            no_anim: true,
            moved: true,
        },
        None => Route::Headline,
        Some(Cmd::Dash { no_anim }) => Route::Dash {
            no_anim: no_anim || cli.no_anim,
            moved: false,
        },
        Some(Cmd::Report { legacy }) => match legacy.into_fault_args() {
            Some(args) => Route::Faults { args, moved: true },
            None => Route::SavingsDetail,
        },
        Some(Cmd::Faults(args)) => Route::Faults { args, moved: false },
        Some(Cmd::Show) => Route::Show,
        Some(Cmd::Doctor) => Route::Doctor,
    }
}

/// The one line bare `lumen` prints.
///
/// A placeholder until the savings engine lands. The engine replaces this function's body and
/// nothing else prints a headline, so the line cannot say one thing here and another elsewhere.
/// It claims no figure, because nothing has computed one yet. It names `lumen dash` because
/// until 1.9.0 bare `lumen` was the dashboard, and someone who typed it for that has to be told
/// where it went.
fn headline_line() -> String {
    "Lumen: no savings computed yet (lumen report for detail, lumen dash for the live dashboard)"
        .to_string()
}

/// What `lumen report` prints until the savings engine lands: a stub, and it says so, with
/// where the two things it and bare `lumen` used to show have gone.
fn savings_detail() -> String {
    "lumen report: the savings detail is not computed yet. This command is a placeholder \
     until Lumen's savings engine lands.\n\
     The fault report moved to `lumen faults` (see `lumen faults --help`), and the live \
     dashboard to `lumen dash`.\n"
        .to_string()
}

/// True when `--no-anim` was given to a command it does nothing for. Said rather than ignored,
/// as the moved spellings are, so a flag that has stopped mattering does not look like it works.
fn stray_no_anim(cli: &Cli) -> bool {
    cli.no_anim && !matches!(cli.cmd, None | Some(Cmd::Dash { .. }))
}

fn main() {
    let cli = Cli::parse();
    if stray_no_anim(&cli) {
        eprintln!("lumen: --no-anim only changes `lumen dash`, so it does nothing here");
    }
    let code = match route(cli) {
        Route::Headline => {
            println!("{}", headline_line());
            0
        }
        Route::SavingsDetail => {
            print!("{}", savings_detail());
            0
        }
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
    fn bare_lumen_is_the_headline() {
        assert_eq!(routed(&[]), Route::Headline);
    }

    #[test]
    fn the_headline_is_one_line_and_claims_no_figure() {
        let line = headline_line();
        assert!(!line.contains('\n'), "{line:?}");
        assert!(!line.chars().any(|c| c.is_ascii_digit()), "{line:?}");
        assert!(line.contains("lumen report"), "{line:?}");
        // Bare `lumen` was the dashboard until 1.9.0: the line says where it went.
        assert!(line.contains("lumen dash"), "{line:?}");
    }

    #[test]
    fn the_savings_detail_says_it_is_a_stub_and_where_the_fault_report_went() {
        let said = savings_detail();
        assert!(said.contains("placeholder"), "{said}");
        assert!(said.contains("not computed yet"), "{said}");
        assert!(said.contains("`lumen faults`"), "{said}");
        assert!(said.contains("`lumen dash`"), "{said}");
        assert!(!said.chars().any(|c| c.is_ascii_digit()), "{said}");
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
        assert_eq!(routed(&["report"]), Route::SavingsDetail);
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
