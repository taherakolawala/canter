//! TUICanvas: a keyboard-driven terminal interface for Canvas.

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
mod html;
mod images;
mod keys;
mod library;
mod model;
mod modules;
mod procstats;
mod protect;
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

const HELP: &str = "TUICanvas — terminal Canvas interface

Usage:
  tuicanvas                 Open the reader (signs in first if needed)
  tuicanvas --diag          Also log request timings to the data folder
  tuicanvas --login [--window | --python] [--show-password] [--fresh] [--chrome]
                            Sign in again. By default TUICanvas drives Edge in
                            the background; --window opens a small sign-in
                            window you drive; --python runs the old helper
  tuicanvas --logout        Delete the saved session and this account's saved copies
  tuicanvas --smoke [--course NAME] [--assignment NAME]
                            Read-only live check; writes benchmarks/private/smoke.json
  tuicanvas --dump DIR [--course NAME] [--assignment NAME]
                            Headless walk-through; writes rendered screens to DIR
  tuicanvas --bench [N] [--cold | --warm] [--terminal] [--course NAME] [--assignment NAME]
                            Time the workflow N times (default 10) for each condition;
                            writes benchmarks/private/tui-bench*.json

Keys: arrows/jk move, Enter open, Esc back, / filter, r refresh,
      s submit files from an assignment, o open in browser, L sign in, q quit
Env:  TUICANVAS_ORIGIN (default https://ufl.instructure.com), TUICANVAS_HOME,
      TUICANVAS_SIGNIN (background | window | python), TUICANVAS_BROWSER";

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
    let config = match config::Config::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
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
    std::process::exit(code);
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
