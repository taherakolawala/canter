//! canter: a keyboard-driven terminal interface for Canvas.

mod announcements;
mod api;
mod app;
mod bench;
mod browser;
mod cache;
mod calendar;
mod canvas;
mod cdp;
mod config;
mod diag;
mod download;
mod grades;
mod home;
mod html;
mod images;
mod inbox;
mod keys;
mod library;
mod model;
mod modules;
mod procstats;
mod protect;
mod school;
mod search;
mod session;
mod signin;
mod signin_view;
mod smoke;
mod submission;
mod theme;
mod transport;
mod tui;
mod ui;
mod upload_body;
mod ws;

const HELP: &str = "canter — terminal Canvas interface

Usage:
  canter                 Open the reader (signs in first if needed)
  canter --login [--window] [--show-password] [--fresh] [--chrome]
                         Sign in again. By default canter drives a browser
                         (Edge, Chrome, Brave, ...) in the background;
                         --window opens a small sign-in window you drive
  canter --logout        Delete the saved session and this account's saved copies
  canter --school [LINK] Choose your school: its Canvas address, or a login link
                         (asked the first time canter runs)
  canter --diag          Also log request timings to the data folder
  canter --version       Print the version

Keys: arrows/jk move, Enter open, Esc back, / search, r refresh,
      s/t/u submit files, text or a URL from an assignment,
      o open in browser, L sign in, ? all keys, q quit
Env:  CANTER_ORIGIN (a Canvas address; overrides the saved school),
      CANTER_BROWSER (path to a Chromium-based browser),
      CANTER_SIGNIN (background | window), CANTER_LIBRARY, CANTER_DATA_DIR

Developer options (run from a source checkout; CANTER_HOME names it):
  canter --login --python
                         Sign in with the old Python helper (Windows only)
  canter --smoke [--course NAME] [--assignment NAME]
                         Read-only live check; writes benchmarks/private/smoke.json
  canter --dump DIR [--course NAME] [--assignment NAME]
                         Headless walk-through; writes rendered screens to DIR
  canter --bench [N] [--cold | --warm] [--terminal] [--course NAME] [--assignment NAME]
                         Time the workflow N times (default 10) for each condition;
                         writes benchmarks/private/tui-bench*.json";

fn value_after(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn main() {
    let started = std::time::Instant::now();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{HELP}");
        return;
    }
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("canter {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    let load = || match config::Config::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    let mut config = load();
    if args.iter().any(|a| a == "--school") {
        let given = value_after(&args, "--school").filter(|v| !v.starts_with('-'));
        let code = choose_school(&config, given);
        if code == 0 {
            println!("Run canter to sign in.");
        }
        std::process::exit(code);
    }
    // The first run in a terminal asks which school; scripts and the
    // developer options keep the default.
    let developer = ["--smoke", "--bench", "--dump", "--logout"];
    if !config.school_chosen
        && std::io::IsTerminal::is_terminal(&std::io::stdin())
        && !args.iter().any(|a| developer.contains(&a.as_str()))
    {
        let code = choose_school(&config, None);
        if code != 0 {
            std::process::exit(code);
        }
        config = load();
    }
    let course = value_after(&args, "--course").unwrap_or_else(|| "GPU".into());
    let assignment = value_after(&args, "--assignment").unwrap_or_else(|| "Lab-2".into());
    let code = if args.iter().any(|a| a == "--smoke") {
        match smoke::run(&config, smoke::SmokeOptions { course, assignment }) {
            Ok(code) => code,
            Err(e) => {
                eprintln!("{e}");
                1
            }
        }
    } else if args.iter().any(|a| a == "--bench") {
        let runs = value_after(&args, "--bench")
            .and_then(|n| n.parse().ok())
            .unwrap_or(10);
        let cold_only = args.iter().any(|a| a == "--cold");
        let warm_only = args.iter().any(|a| a == "--warm");
        let options = bench::BenchOptions {
            runs,
            cold: !warm_only || cold_only,
            warm: !cold_only || warm_only,
            terminal: args.iter().any(|a| a == "--terminal"),
            course,
            assignment,
        };
        match bench::run(&config, options) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("{e}");
                1
            }
        }
    } else if let Some(dir) = value_after(&args, "--dump") {
        match tui::dump(&config, std::path::Path::new(&dir), &course, &assignment) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("{e}");
                1
            }
        }
    } else if args.iter().any(|a| a == "--login") {
        match tui::sign_in(&config, login_choice(&args, &config), login_options(&args)) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("{e}");
                1
            }
        }
    } else if args.iter().any(|a| a == "--logout") {
        logout(&config)
    } else if let Some(unknown) = args.iter().find(|a| a.starts_with('-') && *a != "--diag") {
        eprintln!("Unknown option {unknown}\n\n{HELP}");
        2
    } else {
        let diag_file = args
            .iter()
            .any(|a| a == "--diag")
            .then(|| config.data_dir.join("diagnostics.jsonl"));
        match tui::run(&config, tui::Options { diag_file, started }) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("{e}");
                1
            }
        }
    };
    if let Some(file) = protect::key_file_made() {
        eprintln!(
            "No system keychain was available, so the key to your saved session is in {}, \
             readable only by you.",
            file.display()
        );
    }
    std::process::exit(code);
}

/// Ask for the school and save it.
fn choose_school(config: &config::Config, given: Option<String>) -> i32 {
    if std::env::var_os("CANTER_ORIGIN").is_some() {
        eprintln!("Note: CANTER_ORIGIN is set, and it overrides the school chosen here.");
    }
    match school::choose(given) {
        Ok(Some(chosen)) => match school::save(&config.data_dir, &chosen) {
            Ok(()) => {
                println!("canter will open {}.", chosen.origin);
                0
            }
            Err(e) => {
                eprintln!("{e}");
                1
            }
        },
        Ok(None) => {
            eprintln!("No school was chosen.");
            1
        }
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

/// Which sign-in path `--login` runs: the flags first, then the setting.
fn login_choice(args: &[String], config: &config::Config) -> config::SignIn {
    if args.iter().any(|a| a == "--python") {
        config::SignIn::Python
    } else if args.iter().any(|a| a == "--window" || a == "--browser") {
        config::SignIn::Window
    } else if args
        .iter()
        .any(|a| a == "--headless" || a == "--background")
    {
        config::SignIn::Background
    } else {
        config.signin_choice()
    }
}

fn login_options(args: &[String]) -> signin::Options {
    signin::Options {
        channel: if args.iter().any(|a| a == "--chrome") {
            browser::Channel::Chrome
        } else {
            browser::Channel::Edge
        },
        show_password: args.iter().any(|a| a == "--show-password"),
        fresh: args.iter().any(|a| a == "--fresh"),
        ..signin::Options::default()
    }
}

fn logout(config: &config::Config) -> i32 {
    let path = config.session_path();
    let account = session::SessionFile::load(&path, &config.origin)
        .ok()
        .flatten()
        .and_then(|s| s.user_id);
    if let Some(id) = account {
        cache::SnapshotStore::new(&config.cache_root(), &id.to_string()).clear();
    }
    match std::fs::remove_file(&path) {
        Ok(()) => {
            println!("Signed out locally: session and saved copies removed.");
            0
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            println!("No saved session.");
            0
        }
        Err(_) => {
            eprintln!("Could not remove the saved session.");
            1
        }
    }
}
