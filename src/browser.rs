//! Starting Microsoft Edge (or Chrome) for sign-in and connecting to its
//! DevTools endpoint.
//!
//! The browser runs with its own user-data folder under the application's
//! data directory, so the user's own profile, extensions and signed-in
//! sessions are untouched. `--remote-debugging-port=0` lets the browser pick
//! a free port and write it, with the endpoint path, into
//! `DevToolsActivePort` in that folder.

use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::ws::WebSocket;

/// The visible sign-in window is about the size of a phone screen.
pub const WINDOW_SIZE: (u32, u32) = (420, 760);

/// How long to wait for the browser to publish its debugging port.
const START_LIMIT: Duration = Duration::from_secs(30);

/// The browsers sign-in can drive: every one is built on Chromium, so all
/// of them speak the DevTools protocol sign-in uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    Edge,
    /// Chrome, or Chromium where only that is installed.
    Chrome,
    Brave,
    OperaGx,
    Opera,
    Vivaldi,
    Arc,
}

/// The order browsers are tried in after the one asked for.
const ALL: [Channel; 7] = [
    Channel::Edge,
    Channel::Chrome,
    Channel::Brave,
    Channel::OperaGx,
    Channel::Opera,
    Channel::Vivaldi,
    Channel::Arc,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// No window: the terminal drives the page.
    Background,
    /// A standalone window, roughly phone-sized, that the user drives.
    Window,
}

/// Command-line arguments for one sign-in run.
pub fn arguments(mode: Mode, profile: &Path, url: &str) -> Vec<String> {
    let (width, height) = WINDOW_SIZE;
    let mut args = vec![
        format!("--user-data-dir={}", profile.display()),
        "--remote-debugging-port=0".to_string(),
        "--no-first-run".to_string(),
        "--no-default-browser-check".to_string(),
        "--disable-sync".to_string(),
        "--no-service-autorun".to_string(),
        format!("--window-size={width},{height}"),
    ];
    match mode {
        Mode::Background => {
            args.push("--headless=new".to_string());
            args.push(url.to_string());
        }
        // A separate window with no tabs or toolbar, rather than a tab in a
        // browser the user already has open.
        Mode::Window => args.push(format!("--app={url}")),
    }
    args
}

/// Where a browser is installed on this machine: the one asked for first,
/// then every other supported one, since all speak the same protocol.
/// `CANTER_BROWSER` overrides the search with one exact path.
pub fn executable(channel: Channel) -> Option<PathBuf> {
    if let Ok(path) = std::env::var("CANTER_BROWSER") {
        let path = PathBuf::from(path);
        return path.is_file().then_some(path);
    }
    std::iter::once(channel)
        .chain(ALL.into_iter().filter(|c| *c != channel))
        .flat_map(candidates)
        .find(|p| p.is_file())
}

#[cfg(windows)]
fn candidates(channel: Channel) -> Vec<PathBuf> {
    let env = |name: &str| std::env::var(name).ok().map(PathBuf::from);
    let program_files = || {
        ["ProgramFiles(x86)", "ProgramFiles", "LOCALAPPDATA"]
            .iter()
            .filter_map(|name| env(name))
            .collect::<Vec<_>>()
    };
    let under = |bases: Vec<PathBuf>, parts: &[&str]| -> Vec<PathBuf> {
        bases
            .into_iter()
            .map(|base| parts.iter().fold(base, |p, part| p.join(part)))
            .collect()
    };
    match channel {
        Channel::Edge => under(
            program_files(),
            &["Microsoft", "Edge", "Application", "msedge.exe"],
        ),
        Channel::Chrome => under(
            program_files(),
            &["Google", "Chrome", "Application", "chrome.exe"],
        ),
        Channel::Brave => under(
            program_files(),
            &["BraveSoftware", "Brave-Browser", "Application", "brave.exe"],
        ),
        Channel::Vivaldi => under(program_files(), &["Vivaldi", "Application", "vivaldi.exe"]),
        Channel::Opera | Channel::OperaGx => {
            let folder = if channel == Channel::Opera {
                "Opera"
            } else {
                "Opera GX"
            };
            let roots = env("LOCALAPPDATA")
                .map(|base| base.join("Programs").join(folder))
                .into_iter()
                .chain(env("ProgramFiles").map(|base| base.join(folder)));
            roots.filter_map(|root| opera_binary(&root)).collect()
        }
        // Arc for Windows installs only as a Store app, which cannot be
        // started from its path; `CANTER_BROWSER` is the way to use it.
        Channel::Arc => Vec::new(),
    }
}

/// The browser itself inside an Opera install. The `opera.exe` at the top
/// is a launcher that starts the newest version and exits, which would
/// look to sign-in like the browser closing; the real one sits in a folder
/// named for its version.
#[cfg(windows)]
fn opera_binary(root: &Path) -> Option<PathBuf> {
    let version = |name: &str| -> Option<Vec<u32>> {
        name.split('.').map(|part| part.parse().ok()).collect()
    };
    std::fs::read_dir(root)
        .ok()?
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let parsed = version(&entry.file_name().to_string_lossy())?;
            let binary = entry.path().join("opera.exe");
            binary.is_file().then_some((parsed, binary))
        })
        .max_by(|a, b| a.0.cmp(&b.0))
        .map(|(_, binary)| binary)
}

/// The system-wide app bundle, then one installed for this user only.
#[cfg(target_os = "macos")]
fn candidates(channel: Channel) -> Vec<PathBuf> {
    // (app bundle, program inside it)
    let apps: &[(&str, &str)] = match channel {
        Channel::Edge => &[("Microsoft Edge", "Microsoft Edge")],
        Channel::Chrome => &[("Google Chrome", "Google Chrome"), ("Chromium", "Chromium")],
        Channel::Brave => &[("Brave Browser", "Brave Browser")],
        Channel::OperaGx => &[("Opera GX", "Opera GX"), ("Opera GX", "Opera")],
        Channel::Opera => &[("Opera", "Opera")],
        Channel::Vivaldi => &[("Vivaldi", "Vivaldi")],
        Channel::Arc => &[("Arc", "Arc")],
    };
    let mut roots = vec![PathBuf::from("/Applications")];
    if let Some(home) = std::env::var_os("HOME") {
        roots.push(PathBuf::from(home).join("Applications"));
    }
    apps.iter()
        .flat_map(|(app, program)| {
            roots.iter().map(move |root| {
                root.join(format!("{app}.app"))
                    .join("Contents")
                    .join("MacOS")
                    .join(program)
            })
        })
        .collect()
}

/// Linux and the BSDs name the browser on `PATH`; Chromium stands in for
/// Chrome, since many distributions package only that. Opera GX and Arc
/// have no Linux release.
#[cfg(all(unix, not(target_os = "macos")))]
fn candidates(channel: Channel) -> Vec<PathBuf> {
    let names: &[&str] = match channel {
        Channel::Edge => &["microsoft-edge", "microsoft-edge-stable"],
        Channel::Chrome => &[
            "google-chrome",
            "google-chrome-stable",
            "chromium",
            "chromium-browser",
        ],
        Channel::Brave => &["brave-browser", "brave-browser-stable", "brave"],
        Channel::Opera => &["opera"],
        Channel::Vivaldi => &["vivaldi", "vivaldi-stable"],
        Channel::OperaGx | Channel::Arc => &[],
    };
    let dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default();
    names
        .iter()
        .flat_map(|name| dirs.iter().map(move |dir| dir.join(name)))
        .collect()
}

pub fn channel_name(channel: Channel) -> &'static str {
    match channel {
        Channel::Edge => "Microsoft Edge",
        Channel::Chrome => "Google Chrome",
        Channel::Brave => "Brave",
        Channel::OperaGx => "Opera GX",
        Channel::Opera => "Opera",
        Channel::Vivaldi => "Vivaldi",
        Channel::Arc => "Arc",
    }
}

/// The port and endpoint path a browser wrote to `DevToolsActivePort`.
pub fn parse_active_port(contents: &str) -> Option<(u16, String)> {
    let mut lines = contents.lines();
    let port = lines.next()?.trim().parse().ok()?;
    let path = lines.next().unwrap_or("").trim();
    let path = if path.starts_with('/') {
        path.to_string()
    } else {
        return None;
    };
    Some((port, path))
}

/// A running browser owned by this process.
pub struct Browser {
    child: Child,
    port: u16,
    endpoint: String,
}

impl Browser {
    /// Start the browser and wait until it publishes its debugging port.
    pub fn start(
        executable: &Path,
        mode: Mode,
        profile: &Path,
        url: &str,
    ) -> Result<Browser, String> {
        std::fs::create_dir_all(profile)
            .map_err(|_| "Could not create the sign-in browser folder.".to_string())?;
        let active = profile.join("DevToolsActivePort");
        // A file left by an earlier run would otherwise look like this one's.
        let _ = std::fs::remove_file(&active);
        let child = Command::new(executable)
            .args(arguments(mode, profile, url))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| "The sign-in browser could not be started.".to_string())?;
        let mut browser = Browser {
            child,
            port: 0,
            endpoint: String::new(),
        };
        let deadline = Instant::now() + START_LIMIT;
        loop {
            if let Some((port, endpoint)) = std::fs::read_to_string(&active)
                .ok()
                .as_deref()
                .and_then(parse_active_port)
            {
                browser.port = port;
                browser.endpoint = endpoint;
                return Ok(browser);
            }
            if !browser.running() {
                // Edge exits at once when another browser already owns this
                // folder; that other window answers on its own port.
                return Err("The sign-in browser closed before it was ready.".to_string());
            }
            if Instant::now() >= deadline {
                return Err("The sign-in browser did not start in time.".to_string());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    pub fn running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Connect to the browser-wide DevTools endpoint. Reads time out so the
    /// caller keeps control of its own deadlines.
    pub fn connect(&self, read_timeout: Duration) -> Result<WebSocket<TcpStream>, String> {
        let address = format!("127.0.0.1:{}", self.port);
        let stream = TcpStream::connect(&address)
            .map_err(|_| "Could not reach the sign-in browser.".to_string())?;
        stream
            .set_read_timeout(Some(read_timeout))
            .map_err(|_| "Could not reach the sign-in browser.".to_string())?;
        let _ = stream.set_nodelay(true);
        WebSocket::start(stream, &address, &self.endpoint).map_err(|e| e.message())
    }

    /// Stop the browser. Called on both the success and the failure path.
    pub fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn background_arguments_pass_the_url_and_keep_the_profile_separate() {
        let args = arguments(
            Mode::Background,
            Path::new("C:\\data\\signin"),
            "https://ufl.instructure.com/login/saml/355",
        );
        assert!(args.contains(&"--headless=new".to_string()));
        assert!(args.contains(&"--user-data-dir=C:\\data\\signin".to_string()));
        assert!(args.contains(&"--remote-debugging-port=0".to_string()));
        assert_eq!(
            args.last().unwrap(),
            "https://ufl.instructure.com/login/saml/355"
        );
        assert!(!args.iter().any(|a| a.starts_with("--app=")));
    }

    #[test]
    fn window_arguments_open_a_standalone_phone_sized_window() {
        let args = arguments(
            Mode::Window,
            Path::new("C:\\data\\signin"),
            "https://x/login",
        );
        assert!(args.contains(&"--app=https://x/login".to_string()));
        assert!(args.contains(&"--window-size=420,760".to_string()));
        assert!(!args.iter().any(|a| a.starts_with("--headless")));
    }

    #[test]
    fn active_port_file_needs_both_lines() {
        assert_eq!(
            parse_active_port("51234\n/devtools/browser/8f-1\n").unwrap(),
            (51234, "/devtools/browser/8f-1".to_string())
        );
        assert!(parse_active_port("51234\n").is_none());
        assert!(parse_active_port("").is_none());
        assert!(parse_active_port("not-a-port\n/devtools/browser/x").is_none());
        assert!(parse_active_port("51234\ndevtools/browser/x").is_none());
    }

    #[test]
    fn candidate_paths_end_in_the_expected_program() {
        for channel in ALL {
            let expected: &[&str] = if cfg!(windows) {
                match channel {
                    Channel::Edge => &["msedge.exe"],
                    Channel::Chrome => &["chrome.exe"],
                    Channel::Brave => &["brave.exe"],
                    Channel::Opera | Channel::OperaGx => &["opera.exe"],
                    Channel::Vivaldi => &["vivaldi.exe"],
                    Channel::Arc => &[],
                }
            } else if cfg!(target_os = "macos") {
                match channel {
                    Channel::Edge => &["Microsoft Edge"],
                    Channel::Chrome => &["Google Chrome", "Chromium"],
                    Channel::Brave => &["Brave Browser"],
                    Channel::OperaGx => &["Opera GX", "Opera"],
                    Channel::Opera => &["Opera"],
                    Channel::Vivaldi => &["Vivaldi"],
                    Channel::Arc => &["Arc"],
                }
            } else {
                match channel {
                    Channel::Edge => &["microsoft-edge", "microsoft-edge-stable"],
                    Channel::Chrome => &[
                        "google-chrome",
                        "google-chrome-stable",
                        "chromium",
                        "chromium-browser",
                    ],
                    Channel::Brave => &["brave-browser", "brave-browser-stable", "brave"],
                    Channel::Opera => &["opera"],
                    Channel::Vivaldi => &["vivaldi", "vivaldi-stable"],
                    Channel::OperaGx | Channel::Arc => &[],
                }
            };
            for path in candidates(channel) {
                let name = path.file_name().unwrap().to_string_lossy().into_owned();
                assert!(expected.contains(&name.as_str()), "{channel:?}: {name}");
            }
        }
        assert_eq!(channel_name(Channel::Edge), "Microsoft Edge");
    }

    #[cfg(windows)]
    #[test]
    fn opera_starts_the_newest_version_not_the_launcher() {
        let root = std::env::temp_dir().join(format!("canter-opera-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for dir in ["99.0.4788.9", "112.0.5197.53", "assistant", "localization"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
            std::fs::write(root.join(dir).join("opera.exe"), b"").unwrap();
        }
        std::fs::write(root.join("opera.exe"), b"").unwrap();
        assert_eq!(
            opera_binary(&root).unwrap(),
            root.join("112.0.5197.53").join("opera.exe")
        );
        std::fs::remove_dir_all(&root).unwrap();
        assert!(opera_binary(&root).is_none());
    }
}
