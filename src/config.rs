//! Locations and the configured Canvas origin.

use std::path::{Path, PathBuf};

pub const DEFAULT_ORIGIN: &str = "https://ufl.instructure.com";
pub const DEFAULT_HOST: &str = "ufl.instructure.com";

/// How to sign in: Rust driving a browser with no window, Rust opening a
/// separate sign-in window the user drives, or the Python helper.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignIn {
    Background,
    Window,
    Python,
}

#[derive(Clone, Debug)]
pub struct Config {
    /// `https://host` with no trailing slash.
    pub origin: String,
    pub host: String,
    /// Per-user application data: protected session and content snapshots.
    pub data_dir: PathBuf,
    /// Project checkout containing the login helper and private benchmark folder.
    pub project_dir: PathBuf,
}

impl Config {
    pub fn load() -> Result<Self, String> {
        let origin =
            std::env::var("CANTER_ORIGIN").unwrap_or_else(|_| DEFAULT_ORIGIN.to_string());
        let host = parse_origin(&origin)?;
        Ok(Config {
            origin: format!("https://{host}"),
            host,
            data_dir: data_dir()?,
            project_dir: project_dir(),
        })
    }

    /// Protected session file, scoped to the configured Canvas host.
    pub fn session_path(&self) -> PathBuf {
        let name = if self.host == "ufl.instructure.com" {
            "ufl.session.dpapi".to_string()
        } else {
            format!("{}.session.dpapi", self.host)
        };
        self.data_dir.join(name)
    }

    pub fn cache_root(&self) -> PathBuf {
        self.data_dir.join("cache").join("v1").join(&self.host)
    }

    /// The user's file library: `CANTER_LIBRARY`, else `<home>\canter`.
    pub fn library_root(&self) -> PathBuf {
        if let Ok(dir) = std::env::var("CANTER_LIBRARY") {
            return PathBuf::from(dir);
        }
        let home = PathBuf::from(
            std::env::var(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
                .unwrap_or_else(|_| ".".into()),
        );
        carried_over(home.join("TUICanvas"), home.join("canter"))
    }

    pub fn private_reports(&self) -> PathBuf {
        self.project_dir.join("benchmarks").join("private")
    }

    /// The browser profile sign-in uses, kept away from the user's own
    /// browser profile so their sessions and extensions are untouched.
    pub fn signin_profile_dir(&self) -> PathBuf {
        self.data_dir.join("signin").join(&self.host)
    }

    /// Which sign-in path runs when nothing is asked for on the command line.
    /// `CANTER_SIGNIN` selects one; other Canvas sites get the window,
    /// because their identity provider is unknown here.
    pub fn signin_choice(&self) -> SignIn {
        signin_choice(
            &std::env::var("CANTER_SIGNIN").unwrap_or_default(),
            &self.host,
        )
    }

    pub fn login_helper(&self) -> Option<(PathBuf, PathBuf)> {
        let script = self.project_dir.join("tools").join("terminal_login.py");
        let python = if cfg!(windows) {
            self.project_dir
                .join(".venv")
                .join("Scripts")
                .join("python.exe")
        } else {
            self.project_dir.join(".venv").join("bin").join("python")
        };
        (script.is_file() && python.is_file()).then_some((python, self.project_dir.clone()))
    }
}

fn signin_choice(setting: &str, host: &str) -> SignIn {
    match setting.to_ascii_lowercase().as_str() {
        "background" | "headless" => SignIn::Background,
        "window" | "browser" => SignIn::Window,
        "python" | "helper" => SignIn::Python,
        // Only UF's GatorLink and Duo pages are known here; another site's
        // identity provider has to be driven by its own user.
        _ if host == DEFAULT_HOST => SignIn::Background,
        _ => SignIn::Window,
    }
}

/// Accept only an https origin with a plain host name.
pub fn parse_origin(origin: &str) -> Result<String, String> {
    let rest = origin
        .strip_prefix("https://")
        .ok_or_else(|| "Canvas origin must start with https://".to_string())?;
    let host = rest.trim_end_matches('/');
    let valid = !host.is_empty()
        && host.contains('.')
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.');
    if valid {
        Ok(host.to_ascii_lowercase())
    } else {
        Err("Canvas origin must be https://<host> with no path or port".to_string())
    }
}

/// Carry a folder over from the name this reader had before it was called
/// canter.
///
/// The saved sign-in, the saved copies and every downloaded file live under a
/// folder named after the program, so renaming the program would otherwise
/// orphan them: the reader would ask for a password again and re-download
/// everything, with the old folder left behind for the user to find. Moving
/// it is one rename and happens only when there is nothing at the new name to
/// overwrite. A rename that fails (the old folder is open elsewhere, or the
/// two are on different drives) leaves the new name to be created empty,
/// which is the behaviour of a fresh install rather than an error.
pub fn carried_over(old: PathBuf, new: PathBuf) -> PathBuf {
    if !new.exists() && old.is_dir() {
        let _ = std::fs::rename(&old, &new);
    }
    new
}

fn data_dir() -> Result<PathBuf, String> {
    if let Ok(dir) = std::env::var("CANTER_DATA_DIR") {
        return Ok(PathBuf::from(dir));
    }
    if cfg!(windows) {
        let base = PathBuf::from(
            std::env::var("LOCALAPPDATA").map_err(|_| "LOCALAPPDATA is unavailable".to_string())?,
        );
        return Ok(carried_over(base.join("TUICanvas"), base.join("canter")));
    }
    if let Ok(base) = std::env::var("XDG_DATA_HOME") {
        let base = PathBuf::from(base);
        return Ok(carried_over(base.join("tuicanvas"), base.join("canter")));
    }
    let share = PathBuf::from(std::env::var("HOME").map_err(|_| "HOME is unavailable".to_string())?)
        .join(".local")
        .join("share");
    Ok(carried_over(share.join("tuicanvas"), share.join("canter")))
}

fn project_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("CANTER_HOME") {
        return PathBuf::from(dir);
    }
    let marker = |dir: &Path| dir.join("tools").join("terminal_login.py").is_file();
    if let Ok(exe) = std::env::current_exe() {
        for dir in exe.ancestors().skip(1) {
            if marker(dir) {
                return dir.to_path_buf();
            }
        }
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    for dir in cwd.ancestors() {
        if marker(dir) {
            return dir.to_path_buf();
        }
    }
    cwd
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_validation() {
        assert_eq!(
            parse_origin("https://ufl.instructure.com/").unwrap(),
            "ufl.instructure.com"
        );
        assert!(parse_origin("http://ufl.instructure.com").is_err());
        assert!(parse_origin("https://ufl.instructure.com/api").is_err());
        assert!(parse_origin("https://ufl.instructure.com:8443").is_err());
        assert!(parse_origin("https://user@host.com").is_err());
    }

    #[test]
    fn sign_in_choice_follows_the_setting_then_the_site() {
        assert_eq!(signin_choice("", DEFAULT_HOST), SignIn::Background);
        assert_eq!(signin_choice("", "canvas.example.edu"), SignIn::Window);
        assert_eq!(signin_choice("Window", DEFAULT_HOST), SignIn::Window);
        assert_eq!(
            signin_choice("python", "canvas.example.edu"),
            SignIn::Python
        );
        assert_eq!(
            signin_choice("headless", "canvas.example.edu"),
            SignIn::Background
        );
    }

    #[test]
    fn the_old_folder_is_carried_over_only_when_the_new_one_is_absent() {
        let root = std::env::temp_dir().join(format!("canter-carry-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (old, new) = (root.join("TUICanvas"), root.join("canter"));
        // Nothing to carry over: the new name is simply the answer.
        assert_eq!(carried_over(old.clone(), new.clone()), new);
        assert!(!new.exists(), "nothing is created before it is needed");
        // A saved sign-in under the old name moves across, file and all.
        std::fs::create_dir_all(old.join("cache")).expect("the old folder");
        std::fs::write(old.join("ufl.session.dpapi"), b"session").expect("a session");
        assert_eq!(carried_over(old.clone(), new.clone()), new);
        assert!(!old.exists(), "the old folder is not left behind");
        assert_eq!(
            std::fs::read(new.join("ufl.session.dpapi")).expect("the session"),
            b"session"
        );
        assert!(new.join("cache").is_dir());
        // With both names in place the new one is left alone: a folder of
        // saved copies is never overwritten by an older one.
        std::fs::create_dir_all(&old).expect("the old folder again");
        std::fs::write(old.join("ufl.session.dpapi"), b"older").expect("an older session");
        assert_eq!(carried_over(old.clone(), new.clone()), new);
        assert_eq!(
            std::fs::read(new.join("ufl.session.dpapi")).expect("the session"),
            b"session"
        );
        assert!(old.is_dir(), "and the old one is left for the user to see");
        let _ = std::fs::remove_dir_all(&root);
    }
}
