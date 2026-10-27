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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    Edge,
    Chrome,
}

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

/// Where Edge and Chrome are installed on this machine, most likely first.
/// `CANTER_BROWSER` overrides the search with one exact path.
pub fn executable(channel: Channel) -> Option<PathBuf> {
    if let Ok(path) = std::env::var("CANTER_BROWSER") {
        let path = PathBuf::from(path);
        return path.is_file().then_some(path);
    }
    candidates(channel).into_iter().find(|p| p.is_file())
}

fn candidates(channel: Channel) -> Vec<PathBuf> {
    let suffix: &[&str] = match channel {
        Channel::Edge => &["Microsoft", "Edge", "Application", "msedge.exe"],
        Channel::Chrome => &["Google", "Chrome", "Application", "chrome.exe"],
    };
    ["ProgramFiles(x86)", "ProgramFiles", "LOCALAPPDATA"]
        .iter()
        .filter_map(|base| std::env::var(base).ok())
        .map(|base| {
            suffix
                .iter()
                .fold(PathBuf::from(base), |p, part| p.join(part))
        })
        .collect()
}

pub fn channel_name(channel: Channel) -> &'static str {
    match channel {
        Channel::Edge => "Microsoft Edge",
        Channel::Chrome => "Google Chrome",
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
        for channel in [Channel::Edge, Channel::Chrome] {
            for path in candidates(channel) {
                let name = path.file_name().unwrap().to_string_lossy().into_owned();
                assert!(name == "msedge.exe" || name == "chrome.exe", "{name}");
            }
        }
        assert_eq!(channel_name(Channel::Edge), "Microsoft Edge");
    }
}
