//! Sign-in without Python: TUICanvas drives a browser over the DevTools
//! protocol and saves the Canvas session itself.
//!
//! Two modes share this code. In the background mode (the default for UF)
//! there is no window: the terminal asks for the GatorLink password, fills
//! UF's form, and shows each Duo screen on one status line. In the window
//! mode a separate, phone-sized window opens and the user drives it, which is
//! what a password manager, a passkey, a security key or a Duo phone call
//! needs; TUICanvas only watches for the Canvas session and then closes the
//! window.
//!
//! Either way the finish is the same as the Python helper's: the browser's
//! cookies go through one Canvas API read, and the session is written to the
//! same DPAPI-protected file (`session.rs`), so nothing downstream changes.

use std::io::{BufRead, Write};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::api;
use crate::browser::{self, Browser, Channel, Mode};
use crate::canvas::{CanvasClient, CanvasError};
use crate::cdp::{Cdp, CdpError, Page, Wire};
use crate::config::Config;
use crate::diag::Diagnostics;
use crate::session::{CookieJar, SessionFile, StoredCookie};
use crate::signin_view::{
    Control, LoginView, Snapshot, Status, clean, duo_view, label, transition_view,
};
use crate::transport::ReqwestTransport;

/// The whole sign-in, including the user's own typing and phone approval.
const OVERALL_LIMIT: Duration = Duration::from_secs(600);
/// How often the page is read.
const POLL: Duration = Duration::from_millis(250);
/// Once the browser is back on Canvas, confirm sign-in with a native API read
/// at most this often, and give up on the native read after this long.
const NATIVE_CHECK_INTERVAL: Duration = Duration::from_millis(1500);
const CANVAS_SETTLE_LIMIT: Duration = Duration::from_secs(20);
/// Duo's own origin suffix; UF hands the second factor to it.
const DUO_SUFFIX: &str = ".duosecurity.com";
pub const UF_CANVAS_HOST: &str = "ufl.instructure.com";
pub const UF_IDP_HOST: &str = "login.ufl.edu";

#[derive(Clone, Debug)]
pub struct Options {
    pub mode: Mode,
    pub channel: Channel,
    /// Show the password characters while typing, as the helper's
    /// `--show-password` does.
    pub show_password: bool,
    /// Ignore the saved session's browser state for this sign-in.
    pub fresh: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            mode: Mode::Background,
            channel: Channel::Edge,
            show_password: false,
            fresh: false,
        }
    }
}

/// Why sign-in stopped. Each case is reported differently, because what the
/// user should do next differs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SignInError {
    /// No Edge or Chrome to drive.
    NoBrowser(String),
    /// UF returned its own form after the password was sent.
    WrongPassword,
    DuoDenied,
    DuoTimedOut,
    /// The window was closed, or the browser exited.
    WindowClosed,
    /// Canvas or UF could not be reached.
    Network,
    /// The user pressed `q`, or answered a prompt with nothing.
    Cancelled,
    /// Nothing finished within the overall limit.
    TimedOut,
    /// Sign-in left the UF, Duo and Canvas origins.
    UnexpectedOrigin,
    /// Canvas accepted the browser but not a direct API read.
    NativeReadFailed,
    Failed(String),
}

impl SignInError {
    pub fn message(&self) -> String {
        match self {
            SignInError::NoBrowser(what) => format!(
                "{what} was not found. Install it, set TUICANVAS_BROWSER to its path, \
                 or sign in with the Python helper (tuicanvas --login --python)."
            ),
            SignInError::WrongPassword => {
                "UF did not accept that sign-in. Nothing was retried.".into()
            }
            SignInError::DuoDenied => {
                "Duo refused the sign-in. Start again and approve the request.".into()
            }
            SignInError::DuoTimedOut => {
                "The Duo request ran out of time. Start again to send another.".into()
            }
            SignInError::WindowClosed => "The sign-in window was closed.".into(),
            SignInError::Network => "Could not reach Canvas. Try again when you are online.".into(),
            SignInError::Cancelled => "Sign-in cancelled.".into(),
            SignInError::TimedOut => {
                "Sign-in timed out. Run tuicanvas --login --window to sign in in a window.".into()
            }
            SignInError::UnexpectedOrigin => "Sign-in reached an unexpected site and was stopped. \
                 Use tuicanvas --login --window to sign in in a window."
                .into(),
            SignInError::NativeReadFailed => {
                "Canvas opened, but reading it directly failed. Try again, or use \
                 tuicanvas --login --python."
                    .into()
            }
            SignInError::Failed(why) => why.clone(),
        }
    }
}

fn from_cdp(e: CdpError) -> SignInError {
    match e {
        CdpError::Closed => SignInError::WindowClosed,
        other => SignInError::Failed(other.message()),
    }
}

/// Where sign-in starts. UF's Canvas hands straight to GatorLink; any other
/// Canvas site starts at its own login page.
pub fn start_url(host: &str) -> String {
    if host == UF_CANVAS_HOST {
        format!("https://{host}/login/saml/355")
    } else {
        format!("https://{host}/login")
    }
}

/// Split an https URL into its host and path. Anything else is not a page
/// sign-in may visit.
pub fn split_url(url: &str) -> Option<(String, String)> {
    let rest = url.strip_prefix("https://")?;
    let (authority, path) = match rest.find(['/', '?', '#']) {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    if authority.is_empty() || authority.contains('@') || authority.contains(':') {
        return None;
    }
    let path = path.split(['?', '#']).next().unwrap_or("/");
    let path = if path.starts_with('/') { path } else { "/" };
    Some((authority.to_ascii_lowercase(), path.to_string()))
}

/// The origins the background mode will drive: Canvas, its identity provider
/// and Duo. The window mode does not read or type into pages, so it does not
/// restrict them.
pub fn trusted(url: &str, canvas_host: &str) -> bool {
    let Some((host, _)) = split_url(url) else {
        return false;
    };
    host == canvas_host
        || host.ends_with(DUO_SUFFIX)
        || (canvas_host == UF_CANVAS_HOST && host == UF_IDP_HOST)
}

/// What the driver should do with the page it just read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// Back on Canvas: confirm with a native API read.
    Canvas,
    /// A Canvas `/login/*` page: part of the hand-off, not a prompt.
    Handoff,
    /// UF's GatorLink form is showing and nothing has been sent yet.
    Credentials,
    /// UF's form is showing again after a password was sent.
    Rejected,
    /// The page is outside the origins sign-in may drive.
    Untrusted,
    Screen(LoginView),
}

/// Decide what one page read means. `form_visible` is whether UF's
/// username and password fields are both on screen.
pub fn next_step(
    canvas_host: &str,
    url: &str,
    form_visible: bool,
    submitted: bool,
    snapshot: &Snapshot,
    seconds_on_page: f64,
) -> Step {
    if !trusted(url, canvas_host) {
        return Step::Untrusted;
    }
    let (host, path) = match split_url(url) {
        Some(parts) => parts,
        None => return Step::Untrusted,
    };
    if host == canvas_host {
        // Canvas's own pages are never presented as a sign-in prompt.
        return if path.starts_with("/login") {
            Step::Handoff
        } else {
            Step::Canvas
        };
    }
    if host == UF_IDP_HOST && form_visible {
        return if submitted {
            Step::Rejected
        } else {
            Step::Credentials
        };
    }
    let view = transition_view(&host, &snapshot.text, &snapshot.controls, seconds_on_page)
        .unwrap_or_else(|| duo_view(&snapshot.text, &snapshot.controls));
    // Pages without controls normally pass by on their own; their text is
    // shown only if one lingers.
    let view = if view.kind == "waiting" && seconds_on_page < 8.0 {
        duo_view("Loading", &[])
    } else {
        view
    };
    Step::Screen(view)
}

/// The page script that reads one screen: its visible text and its visible,
/// enabled controls. Each control is marked so a later step can act on the
/// same element.
const SNAPSHOT_SCRIPT: &str = r#"(() => {
  const out = {text: '', controls: []};
  const visit = (doc) => {
    try { out.text += ((doc.body && doc.body.innerText) || '').slice(0, 12000) + '\n'; }
    catch (e) { return; }
    for (const e of doc.querySelectorAll('button,input:not([type=hidden]),select,a[href],[role=button]')) {
      let box;
      try { box = e.getBoundingClientRect(); } catch (_) { continue; }
      if (!box.width || !box.height || e.disabled) continue;
      const labels = e.labels ? Array.from(e.labels).map(l => l.innerText).join(' ') : '';
      e.setAttribute('data-tuicanvas', String(out.controls.length));
      out.controls.push({
        tag: e.tagName.toLowerCase(), type: e.type || '',
        label: e.getAttribute('aria-label') || labels || e.innerText ||
               e.getAttribute('placeholder') || e.name || e.id || 'control',
        href: e.tagName === 'A' ? e.href : null,
        options: e.tagName === 'SELECT'
          ? Array.from(e.options).map(o => ({label: o.text, value: o.value, disabled: o.disabled}))
          : []
      });
    }
    for (const frame of doc.querySelectorAll('iframe')) {
      try { if (frame.contentDocument) visit(frame.contentDocument); } catch (_) {}
    }
  };
  visit(document);
  return JSON.stringify(out);
})()"#;

/// Find a marked control in the page or any same-origin frame of it.
const FIND_FUNCTION: &str = r#"const find = (doc, mark) => {
    const hit = doc.querySelector('[data-tuicanvas="' + mark + '"]');
    if (hit) return hit;
    for (const frame of doc.querySelectorAll('iframe')) {
      try { if (frame.contentDocument) { const r = find(frame.contentDocument, mark); if (r) return r; } }
      catch (_) {}
    }
    return null;
  };"#;

fn click_script(mark: usize) -> String {
    format!(
        "(() => {{ {FIND_FUNCTION} const e = find(document, {mark});\
         if (!e) return 'gone'; e.click(); return 'ok'; }})()"
    )
}

fn set_checked_script(mark: usize, checked: bool) -> String {
    format!(
        "(() => {{ {FIND_FUNCTION} const e = find(document, {mark});\
         if (!e) return 'gone'; e.checked = {checked};\
         e.dispatchEvent(new Event('change', {{bubbles: true}})); return 'ok'; }})()"
    )
}

fn select_script(mark: usize, value: &str) -> String {
    let value = json!(value);
    format!(
        "(() => {{ {FIND_FUNCTION} const e = find(document, {mark});\
         if (!e) return 'gone'; e.value = {value};\
         e.dispatchEvent(new Event('change', {{bubbles: true}})); return 'ok'; }})()"
    )
}

/// Empty a field and put the caret in it, so the text that follows is typed
/// through `Input.insertText` and never appears in a script.
fn focus_field_script(selector: &str) -> String {
    let selector = json!(selector);
    format!(
        "(() => {{ const e = document.querySelector({selector});\
         if (!e) return 'gone'; e.value = ''; e.focus(); return 'ok'; }})()"
    )
}

const FORM_VISIBLE_SCRIPT: &str = "(() => {
  const seen = (name) => {
    const e = document.querySelector('input[name=\"' + name + '\"]');
    if (!e) return false;
    const box = e.getBoundingClientRect();
    return box.width > 0 && box.height > 0;
  };
  return seen('j_username') && seen('j_password');
})()";

const SUBMIT_SELECTOR: &str = "[name=\"_eventId_proceed\"]";

/// Cookies as the DevTools protocol reports them, in the same shape the
/// session file already stores (the Python helper's Playwright state uses the
/// same field names and the same leading-dot domain convention).
pub fn cookies_from_cdp(result: &Value) -> Vec<StoredCookie> {
    result
        .get("cookies")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|c| {
                    let name = c.get("name")?.as_str()?.to_string();
                    let domain = c.get("domain")?.as_str()?.to_string();
                    if name.is_empty() || domain.is_empty() {
                        return None;
                    }
                    let same_site = match c.get("sameSite").and_then(Value::as_str) {
                        Some("Strict") => "Strict",
                        Some("None") => "None",
                        _ => "Lax",
                    };
                    Some(StoredCookie {
                        name,
                        value: c.get("value")?.as_str()?.to_string(),
                        domain,
                        path: c
                            .get("path")
                            .and_then(Value::as_str)
                            .unwrap_or("/")
                            .to_string(),
                        expires: c.get("expires").and_then(Value::as_f64).unwrap_or(-1.0),
                        http_only: c.get("httpOnly").and_then(Value::as_bool).unwrap_or(false),
                        secure: c.get("secure").and_then(Value::as_bool).unwrap_or(true),
                        same_site: same_site.to_string(),
                        extra: Default::default(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The cookies of a previous session, in the form `Storage.setCookies` takes,
/// so UF's and Duo's remembered-device cookies are available again.
pub fn cookies_to_cdp(cookies: &[StoredCookie]) -> Vec<Value> {
    cookies
        .iter()
        .filter(|c| !c.name.is_empty() && c.domain.trim_start_matches('.').contains('.'))
        .map(|c| {
            let mut value = json!({
                "name": c.name,
                "value": c.value,
                "domain": c.domain,
                "path": c.path,
                "httpOnly": c.http_only,
                "secure": c.secure,
                "sameSite": c.same_site,
            });
            if c.expires >= 0.0 {
                value["expires"] = json!(c.expires);
            }
            value
        })
        .collect()
}

/// A Canvas API read with the browser's cookies, which is what decides that
/// sign-in finished. Canvas changes its pages; this does not depend on them.
struct Native {
    runtime: tokio::runtime::Runtime,
}

enum Verified {
    /// The account id, and the jar including any rotated cookie.
    Signed(u64, CookieJar),
    NotSignedIn,
    Offline,
}

impl Native {
    fn new() -> Result<Self, SignInError> {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map(|runtime| Native { runtime })
            .map_err(|_| SignInError::Failed("Could not start the async runtime.".into()))
    }

    fn check(&self, config: &Config, jar: CookieJar) -> Verified {
        let Ok(transport) = ReqwestTransport::new() else {
            return Verified::Offline;
        };
        let client = CanvasClient::new(
            &config.origin,
            &config.host,
            std::sync::Arc::new(transport),
            jar.clone(),
            std::sync::Arc::new(Diagnostics::new(None)),
        );
        match self.runtime.block_on(api::profile(&client)) {
            Ok((profile, _)) => {
                let jar = client.take_changed_jar().unwrap_or(jar);
                Verified::Signed(profile.id, jar)
            }
            Err(CanvasError::Network(_)) => Verified::Offline,
            Err(_) => Verified::NotSignedIn,
        }
    }
}

/// Run sign-in and return the saved session.
pub fn sign_in(config: &Config, options: Options) -> Result<SessionFile, SignInError> {
    let mut status = Status::new();
    let result = run(config, &options, &mut status);
    match &result {
        Ok(_) => status.done("Signed in. Your session is saved for next time.", true),
        Err(error) => status.done(&error.message(), false),
    }
    result
}

fn run(
    config: &Config,
    options: &Options,
    status: &mut Status,
) -> Result<SessionFile, SignInError> {
    let executable = browser::executable(options.channel).ok_or_else(|| {
        SignInError::NoBrowser(browser::channel_name(options.channel).to_string())
    })?;
    let previous = if options.fresh {
        None
    } else {
        SessionFile::load(&config.session_path(), &config.origin)
            .ok()
            .flatten()
    };
    let native = Native::new()?;
    let url = start_url(&config.host);
    status.show("Connecting to Canvas", "");
    let mut browser = Browser::start(
        &executable,
        options.mode,
        &config.signin_profile_dir(),
        &url,
    )
    .map_err(SignInError::Failed)?;
    // Reads return after this long with nothing, which keeps the status line
    // moving and the overall deadline honest.
    let socket = browser
        .connect(Duration::from_millis(100))
        .map_err(SignInError::Failed)?;
    let mut cdp = Cdp::new(socket);
    let session_id = attach_to_page(&mut cdp)?;

    if let Some(previous) = &previous {
        let cookies = cookies_to_cdp(&previous.jar.cookies);
        if !cookies.is_empty() {
            // A failure here only means the next sign-in asks for Duo again.
            let _ = cdp.call(
                None,
                "Storage.setCookies",
                json!({"cookies": cookies}),
                crate::cdp::COMMAND_TIMEOUT,
            );
        }
    }
    {
        let mut page = Page {
            cdp: &mut cdp,
            session: session_id.clone(),
        };
        if options.mode == Mode::Background {
            page.navigate(&url).map_err(|e| match e {
                CdpError::Protocol(_) => SignInError::Network,
                other => from_cdp(other),
            })?;
        }
    }

    let mut flow = Flow {
        config,
        options,
        status,
        native: &native,
        previous,
        submitted: false,
        manual_prompt: false,
        last_view: None,
        url_since: (String::new(), Instant::now()),
        canvas_since: None,
        last_native: None,
        deadline: Instant::now() + OVERALL_LIMIT,
    };
    let outcome = flow.drive(&mut cdp, &session_id, &mut browser);
    // Close the window as soon as sign-in finishes, either way.
    let _ = cdp.call(None, "Browser.close", json!({}), Duration::from_secs(2));
    browser.stop();
    outcome
}

/// Attach to the page the browser opened, and keep its session id.
fn attach_to_page<W: Wire>(cdp: &mut Cdp<W>) -> Result<String, SignInError> {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let targets = cdp
            .call(
                None,
                "Target.getTargets",
                json!({}),
                crate::cdp::COMMAND_TIMEOUT,
            )
            .map_err(from_cdp)?;
        if let Some(target) = crate::cdp::first_page_target(&targets) {
            let attached = cdp
                .call(
                    None,
                    "Target.attachToTarget",
                    json!({"targetId": target, "flatten": true}),
                    crate::cdp::COMMAND_TIMEOUT,
                )
                .map_err(from_cdp)?;
            return attached
                .get("sessionId")
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| {
                    SignInError::Failed("The sign-in page could not be attached to.".into())
                });
        }
        if Instant::now() >= deadline {
            return Err(SignInError::WindowClosed);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The state that lives across one sign-in's page reads.
struct Flow<'a> {
    config: &'a Config,
    options: &'a Options,
    status: &'a mut Status,
    native: &'a Native,
    previous: Option<SessionFile>,
    submitted: bool,
    manual_prompt: bool,
    last_view: Option<LoginView>,
    url_since: (String, Instant),
    canvas_since: Option<Instant>,
    last_native: Option<Instant>,
    deadline: Instant,
}

impl Flow<'_> {
    fn drive<W: Wire>(
        &mut self,
        cdp: &mut Cdp<W>,
        session: &str,
        browser: &mut Browser,
    ) -> Result<SessionFile, SignInError> {
        while Instant::now() < self.deadline {
            self.wait();
            if !browser.running() {
                return Err(SignInError::WindowClosed);
            }
            let url = {
                let mut page = Page {
                    cdp,
                    session: session.to_string(),
                };
                page.evaluate("location.href")
                    .map_err(from_cdp)?
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            };
            if url != self.url_since.0 {
                self.url_since = (url.clone(), Instant::now());
            }
            let seconds = self.url_since.1.elapsed().as_secs_f64();

            // The window mode never reads or types into the page. It watches
            // for the Canvas session and closes the window.
            if self.options.mode == Mode::Window {
                if let Some(session_file) = self.try_canvas(cdp, &url)? {
                    return Ok(session_file);
                }
                self.status
                    .show("Finish signing in in the sign-in window", "q cancel");
                if poll_key() == Some('q') {
                    return Err(SignInError::Cancelled);
                }
                continue;
            }
            if let Some(session_file) = self.try_canvas(cdp, &url)? {
                return Ok(session_file);
            }
            let (form_visible, snapshot) = {
                let mut page = Page {
                    cdp,
                    session: session.to_string(),
                };
                let visible = page
                    .evaluate(FORM_VISIBLE_SCRIPT)
                    .map_err(from_cdp)?
                    .as_bool()
                    .unwrap_or(false);
                let raw = page.evaluate(SNAPSHOT_SCRIPT).map_err(from_cdp)?;
                let snapshot: Snapshot = raw
                    .as_str()
                    .and_then(|text| serde_json::from_str(text).ok())
                    .unwrap_or_default();
                (visible, snapshot)
            };
            // The page may have moved on while it was being read; never
            // describe one page's content as another's.
            let moved = {
                let mut page = Page {
                    cdp,
                    session: session.to_string(),
                };
                page.evaluate("location.href")
                    .map_err(from_cdp)?
                    .as_str()
                    .unwrap_or_default()
                    != url
            };
            if moved {
                continue;
            }
            let snapshot = self.only_trusted_links(snapshot);
            let step = next_step(
                &self.config.host,
                &url,
                form_visible,
                self.submitted,
                &snapshot,
                seconds,
            );
            match step {
                Step::Canvas | Step::Handoff => continue,
                Step::Untrusted => return Err(SignInError::UnexpectedOrigin),
                Step::Credentials => self.sign_in_form(cdp, session, &url)?,
                Step::Rejected => {
                    self.status.clear();
                    self.status
                        .line("UF did not accept that sign-in. Nothing is retried automatically.");
                    if !self.ask_retry()? {
                        return Err(SignInError::WrongPassword);
                    }
                    self.submitted = false;
                }
                Step::Screen(view) => {
                    match view.kind {
                        "denied" => return Err(SignInError::DuoDenied),
                        "expired" => return Err(SignInError::DuoTimedOut),
                        _ => {}
                    }
                    self.show_screen(cdp, session, view, &snapshot, &url)?;
                }
            }
        }
        Err(SignInError::TimedOut)
    }

    /// Advance the status line while the browser works.
    fn wait(&mut self) {
        let until = Instant::now() + POLL;
        while Instant::now() < until {
            std::thread::sleep(Duration::from_millis(100));
            self.status.tick();
        }
    }

    /// A link whose target leaves the sign-in origins is not offered.
    fn only_trusted_links(&self, mut snapshot: Snapshot) -> Snapshot {
        let host = self.config.host.clone();
        snapshot.controls.retain(|c| match &c.href {
            Some(href) => trusted(href, &host),
            None => true,
        });
        snapshot
    }

    /// When the browser is back on Canvas, confirm with a native read and
    /// save the session.
    fn try_canvas<W: Wire>(
        &mut self,
        cdp: &mut Cdp<W>,
        url: &str,
    ) -> Result<Option<SessionFile>, SignInError> {
        let Some((host, path)) = split_url(url) else {
            return Ok(None);
        };
        if host != self.config.host || path.starts_with("/login") {
            self.canvas_since = None;
            return Ok(None);
        }
        self.status.show("Opening Canvas", "");
        let now = Instant::now();
        let since = *self.canvas_since.get_or_insert(now);
        let due = self
            .last_native
            .is_none_or(|last| now.duration_since(last) >= NATIVE_CHECK_INTERVAL);
        if due {
            self.last_native = Some(now);
            let jar = CookieJar {
                cookies: cookies_from_cdp(
                    &cdp.call(
                        None,
                        "Storage.getCookies",
                        json!({}),
                        crate::cdp::COMMAND_TIMEOUT,
                    )
                    .map_err(from_cdp)?,
                ),
            };
            match self.native.check(self.config, jar) {
                Verified::Signed(user_id, jar) => {
                    let mut session = SessionFile::new(
                        &self.config.origin,
                        Some(user_id),
                        jar,
                        self.previous.as_ref(),
                    );
                    session
                        .save(&self.config.session_path())
                        .map_err(|e| SignInError::Failed(e.0))?;
                    return Ok(Some(session));
                }
                Verified::Offline => return Err(SignInError::Network),
                Verified::NotSignedIn => {}
            }
        }
        if now.duration_since(since) >= CANVAS_SETTLE_LIMIT {
            return Err(SignInError::NativeReadFailed);
        }
        Ok(None)
    }

    /// UF's GatorLink form: ask for the username and password, fill them, and
    /// submit once.
    fn sign_in_form<W: Wire>(
        &mut self,
        cdp: &mut Cdp<W>,
        session: &str,
        url: &str,
    ) -> Result<(), SignInError> {
        self.status.clear();
        self.status.line("Sign in with your GatorLink account");
        let username = read_line("  Username: ")?;
        let password = if self.options.show_password {
            read_line("  Password (visible): ")?
        } else {
            read_secret("  Password: ")?
        };
        if username.is_empty() || password.is_empty() {
            self.status
                .line("Username and password are both required; nothing was sent.");
            return Ok(());
        }
        let mut page = Page {
            cdp,
            session: session.to_string(),
        };
        // Nothing is typed unless the page is still the one that was read.
        if page.evaluate("location.href").map_err(from_cdp)?.as_str() != Some(url) {
            return Ok(());
        }
        for (selector, text) in [
            ("input[name=\"j_username\"]", &username),
            ("input[name=\"j_password\"]", &password),
        ] {
            if page
                .evaluate(&focus_field_script(selector))
                .map_err(from_cdp)?
                != "ok"
            {
                return Ok(());
            }
            page.insert_text(text).map_err(from_cdp)?;
        }
        drop(password);
        if page.evaluate("location.href").map_err(from_cdp)?.as_str() != Some(url) {
            return Ok(());
        }
        // The submit control is a button, so the form's action field is only
        // sent when it is clicked.
        let click = format!(
            "(() => {{ const e = document.querySelector({});\
             if (!e) return 'gone'; e.click(); return 'ok'; }})()",
            json!(SUBMIT_SELECTOR)
        );
        if page.evaluate(&click).map_err(from_cdp)? != "ok" {
            return Err(SignInError::Failed(
                "UF's sign-in button was missing, so nothing was sent.".into(),
            ));
        }
        self.submitted = true;
        self.status.show("Checking your password", "");
        Ok(())
    }

    /// Show one Duo or hand-off screen and take the key the user pressed.
    fn show_screen<W: Wire>(
        &mut self,
        cdp: &mut Cdp<W>,
        session: &str,
        view: LoginView,
        snapshot: &Snapshot,
        url: &str,
    ) -> Result<(), SignInError> {
        let view = if self.manual_prompt {
            self.manual_prompt = false;
            LoginView {
                kind: "manual",
                title: "Duo prompt".into(),
                lines: clean(&snapshot.text).lines().map(str::to_string).collect(),
                actions: snapshot
                    .controls
                    .iter()
                    .enumerate()
                    .take(9)
                    .map(|(n, c)| (char::from(b'1' + n as u8), n, label(c)))
                    .collect(),
                automatic: false,
            }
        } else {
            view
        };
        let chosen = if view.automatic {
            self.status.show(&view.title, &view.hint());
            self.last_view = Some(view.clone());
            match poll_key() {
                Some('q') => return Err(SignInError::Cancelled),
                Some('m') if matches!(view.kind, "verified_push" | "code_loading" | "push") => {
                    self.manual_prompt = true;
                    return Ok(());
                }
                key => key.and_then(|key| action_for(&view, key)),
            }
        } else {
            self.status.clear();
            if self.last_view.as_ref() != Some(&view) {
                render(self.status, &view);
                self.last_view = Some(view.clone());
            }
            let key = read_line("Choose an option, press Enter to check again, or q to cancel: ")?
                .to_lowercase();
            if key == "q" {
                return Err(SignInError::Cancelled);
            }
            key.chars().next().and_then(|key| action_for(&view, key))
        };
        if let Some(mark) = chosen {
            self.use_control(cdp, session, snapshot, mark, url)?;
        }
        Ok(())
    }

    /// Use one control of the screen that was just read.
    fn use_control<W: Wire>(
        &mut self,
        cdp: &mut Cdp<W>,
        session: &str,
        snapshot: &Snapshot,
        mark: usize,
        url: &str,
    ) -> Result<(), SignInError> {
        let Some(control) = snapshot.controls.get(mark) else {
            return Ok(());
        };
        let control = control.clone();
        let mut page = Page {
            cdp,
            session: session.to_string(),
        };
        if page.evaluate("location.href").map_err(from_cdp)?.as_str() != Some(url) {
            // The screen changed since it was read; the caller re-reads it.
            return Ok(());
        }
        let result = if control.tag == "select" {
            let choices: Vec<_> = control.options.iter().filter(|o| !o.disabled).collect();
            for (n, option) in choices.iter().enumerate() {
                self.status
                    .line(&format!("  {}. {}", n + 1, clean(&option.label)));
            }
            let answer = read_line("Selection: ")?;
            match answer.parse::<usize>().ok().filter(|n| *n >= 1) {
                Some(n) if n <= choices.len() => {
                    let script = select_script(mark, &choices[n - 1].value);
                    page.evaluate(&script).map_err(from_cdp)?
                }
                _ => {
                    self.status.line("Invalid selection.");
                    return Ok(());
                }
            }
        } else if typed_field(&control) {
            let prompt: String = clean(&control.label).chars().take(160).collect();
            let value = if control.kind == "password" && !self.options.show_password {
                read_secret(&format!("{prompt}: "))?
            } else {
                read_line(&format!("{prompt}: "))?
            };
            let selector = format!("[data-tuicanvas=\"{mark}\"]");
            if page
                .evaluate(&focus_field_script(&selector))
                .map_err(from_cdp)?
                == "ok"
            {
                page.insert_text(&value).map_err(from_cdp)?;
            }
            drop(value);
            Value::from("ok")
        } else if matches!(control.kind.as_str(), "checkbox" | "radio") {
            let script = set_checked_script(mark, true);
            page.evaluate(&script).map_err(from_cdp)?
        } else if control.kind == "file" {
            self.status
                .line("That option needs a file, which sign-in does not provide.");
            return Ok(());
        } else {
            let script = click_script(mark);
            page.evaluate(&script).map_err(from_cdp)?
        };
        if result == "gone" {
            self.status
                .line("That option changed before it could be used; showing the current screen.");
        }
        Ok(())
    }

    /// Offer one more password attempt, as the helper does.
    fn ask_retry(&mut self) -> Result<bool, SignInError> {
        let answer = read_line("Press r to try again, or Enter to stop: ")?;
        Ok(answer.trim().eq_ignore_ascii_case("r"))
    }
}

/// Whether a control takes typed text rather than a click.
fn typed_field(control: &Control) -> bool {
    control.tag == "input"
        && !matches!(
            control.kind.as_str(),
            "button" | "submit" | "reset" | "checkbox" | "radio" | "file" | "image"
        )
}

/// The control a key press selects on a screen.
pub fn action_for(view: &LoginView, key: char) -> Option<usize> {
    view.actions
        .iter()
        .find(|(action, _, _)| *action == key)
        .map(|(_, index, _)| *index)
}

/// A prompt that needs a typed choice: its instructions and numbered options.
fn render(status: &mut Status, view: &LoginView) {
    status.line("");
    status.line(&view.title);
    for line in &view.lines {
        status.line(&format!("  {line}"));
    }
    if !view.actions.is_empty() {
        let keys: Vec<String> = view
            .actions
            .iter()
            .map(|(key, _, name)| format!("[{key}] {name}"))
            .collect();
        status.line(&format!("  {}", keys.join("   ")));
    }
}

/// A key already pressed, without waiting for the browser or the phone.
fn poll_key() -> Option<char> {
    use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
    if !event::poll(Duration::ZERO).unwrap_or(false) {
        return None;
    }
    match event::read() {
        Ok(Event::Key(key)) if key.kind != KeyEventKind::Release => match key.code {
            KeyCode::Char(c) => Some(c.to_ascii_lowercase()),
            _ => None,
        },
        _ => None,
    }
}

fn read_line(prompt: &str) -> Result<String, SignInError> {
    print!("{prompt}");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    match std::io::stdin().lock().read_line(&mut line) {
        Ok(0) => Err(SignInError::Cancelled),
        Ok(_) => Ok(line.trim_end_matches(['\r', '\n']).trim().to_string()),
        Err(_) => Err(SignInError::Cancelled),
    }
}

/// Read a password without echoing it. The characters are never printed,
/// logged or stored; they go to the browser and nowhere else.
fn read_secret(prompt: &str) -> Result<String, SignInError> {
    use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
    use ratatui::crossterm::terminal::{disable_raw_mode, enable_raw_mode};

    print!("{prompt}");
    let _ = std::io::stdout().flush();
    if enable_raw_mode().is_err() {
        println!();
        return Err(SignInError::Failed(
            "This terminal cannot hide a password. Run tuicanvas --login --show-password \
             to type it visibly, or --window to sign in in a window."
                .into(),
        ));
    }
    let mut secret = String::new();
    let outcome = loop {
        match event::read() {
            Ok(Event::Key(key)) if key.kind == KeyEventKind::Release => continue,
            Ok(Event::Key(key)) => match key.code {
                KeyCode::Enter => break Ok(std::mem::take(&mut secret)),
                KeyCode::Backspace => {
                    secret.pop();
                }
                KeyCode::Esc => break Err(SignInError::Cancelled),
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    break Err(SignInError::Cancelled);
                }
                KeyCode::Char(c) => secret.push(c),
                _ => {}
            },
            Ok(_) => {}
            Err(_) => break Err(SignInError::Cancelled),
        }
    };
    let _ = disable_raw_mode();
    println!();
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signin_view::Choice;

    fn snapshot(text: &str, labels: &[&str]) -> Snapshot {
        Snapshot {
            text: text.into(),
            controls: labels
                .iter()
                .map(|l| Control {
                    tag: "button".into(),
                    kind: "button".into(),
                    label: (*l).into(),
                    href: None,
                    options: vec![],
                })
                .collect(),
        }
    }

    #[test]
    fn start_url_follows_the_configured_site() {
        assert_eq!(
            start_url(UF_CANVAS_HOST),
            "https://ufl.instructure.com/login/saml/355"
        );
        assert_eq!(
            start_url("canvas.example.edu"),
            "https://canvas.example.edu/login"
        );
    }

    #[test]
    fn urls_are_split_and_restricted_to_the_sign_in_origins() {
        assert_eq!(
            split_url("https://login.ufl.edu/idp/profile?x=1").unwrap(),
            ("login.ufl.edu".to_string(), "/idp/profile".to_string())
        );
        assert_eq!(
            split_url("https://ufl.instructure.com").unwrap().1,
            "/".to_string()
        );
        assert!(split_url("http://login.ufl.edu/").is_none());
        assert!(split_url("https://user@login.ufl.edu/").is_none());
        assert!(split_url("https://login.ufl.edu:8443/").is_none());
        assert!(split_url("about:blank").is_none());

        assert!(trusted("https://ufl.instructure.com/", UF_CANVAS_HOST));
        assert!(trusted("https://login.ufl.edu/idp", UF_CANVAS_HOST));
        assert!(trusted(
            "https://api-123.duosecurity.com/frame",
            UF_CANVAS_HOST
        ));
        assert!(!trusted("https://evilduosecurity.com/", UF_CANVAS_HOST));
        assert!(!trusted("https://login.ufl.edu.evil.com/", UF_CANVAS_HOST));
        assert!(!trusted("https://example.com/", UF_CANVAS_HOST));
        // Another Canvas site does not share UF's identity provider.
        assert!(!trusted("https://login.ufl.edu/", "canvas.example.edu"));
    }

    #[test]
    fn canvas_pages_are_told_apart_from_the_hand_off() {
        let empty = Snapshot::default();
        assert_eq!(
            next_step(
                UF_CANVAS_HOST,
                "https://ufl.instructure.com/?login_success=1",
                false,
                false,
                &empty,
                1.0
            ),
            Step::Canvas
        );
        assert_eq!(
            next_step(
                UF_CANVAS_HOST,
                "https://ufl.instructure.com/login/saml/355",
                false,
                false,
                &empty,
                1.0
            ),
            Step::Handoff
        );
        assert_eq!(
            next_step(
                UF_CANVAS_HOST,
                "https://example.com/",
                false,
                false,
                &empty,
                1.0
            ),
            Step::Untrusted
        );
    }

    #[test]
    fn the_uf_form_asks_once_and_reports_a_rejection() {
        let form = snapshot("Sign in with your GatorLink", &["Log In"]);
        assert_eq!(
            next_step(
                UF_CANVAS_HOST,
                "https://login.ufl.edu/idp/x",
                true,
                false,
                &form,
                1.0
            ),
            Step::Credentials
        );
        assert_eq!(
            next_step(
                UF_CANVAS_HOST,
                "https://login.ufl.edu/idp/x",
                true,
                true,
                &form,
                1.0
            ),
            Step::Rejected
        );
    }

    #[test]
    fn duo_screens_become_views_and_lingering_pages_show_their_text() {
        let push = snapshot(
            "Check for a Duo Push\nSent to \"iOS\" (•••-•••-0581)",
            &["Other options"],
        );
        match next_step(
            UF_CANVAS_HOST,
            "https://api-1.duosecurity.com/frame",
            false,
            true,
            &push,
            2.0,
        ) {
            Step::Screen(view) => assert_eq!(view.kind, "push"),
            other => panic!("{other:?}"),
        }
        let odd = snapshot("Something unexpected happened", &[]);
        match next_step(
            UF_CANVAS_HOST,
            "https://api-1.duosecurity.com/frame",
            false,
            true,
            &odd,
            1.0,
        ) {
            Step::Screen(view) => assert_eq!(view.kind, "loading"),
            other => panic!("{other:?}"),
        }
        match next_step(
            UF_CANVAS_HOST,
            "https://api-1.duosecurity.com/frame",
            false,
            true,
            &odd,
            9.0,
        ) {
            Step::Screen(view) => {
                assert_eq!(view.kind, "waiting");
                assert_eq!(
                    view.lines,
                    vec!["Something unexpected happened".to_string()]
                );
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn cookies_survive_the_round_trip_through_the_protocol() {
        let result = json!({"cookies": [
            {"name": "canvas_session", "value": "v1", "domain": "ufl.instructure.com",
             "path": "/", "expires": -1.0, "httpOnly": true, "secure": true, "sameSite": "Lax"},
            {"name": "remember", "value": "v2", "domain": ".duosecurity.com", "path": "/",
             "expires": 1800000000.0, "httpOnly": false, "secure": true, "sameSite": "None"},
            {"name": "", "value": "x", "domain": "ufl.instructure.com"},
            {"name": "nodomain", "value": "x", "domain": ""}
        ]});
        let cookies = cookies_from_cdp(&result);
        assert_eq!(cookies.len(), 2);
        assert_eq!(cookies[0].name, "canvas_session");
        assert_eq!(cookies[0].expires, -1.0);
        assert_eq!(cookies[1].domain, ".duosecurity.com");
        assert_eq!(cookies[1].same_site, "None");

        let back = cookies_to_cdp(&cookies);
        assert_eq!(back.len(), 2);
        assert_eq!(back[0]["name"], json!("canvas_session"));
        // A session cookie has no expiry to restore.
        assert!(back[0].get("expires").is_none());
        assert_eq!(back[1]["expires"], json!(1800000000.0));
        assert_eq!(back[1]["sameSite"], json!("None"));
    }

    #[test]
    fn single_label_cookie_domains_are_not_restored() {
        let cookies = cookies_from_cdp(&json!({"cookies": [
            {"name": "a", "value": "1", "domain": "localhost", "path": "/"}
        ]}));
        assert_eq!(cookies.len(), 1);
        assert!(cookies_to_cdp(&cookies).is_empty());
    }

    #[test]
    fn scripts_carry_values_as_json_and_never_a_password() {
        let script = select_script(2, "phone-\"1\"");
        assert!(script.contains("data-tuicanvas=\"' + mark + '\""));
        assert!(script.contains(r#"e.value = "phone-\"1\"""#));
        assert!(click_script(3).contains("find(document, 3)"));
        assert!(set_checked_script(1, true).contains("e.checked = true"));
        let focus = focus_field_script("input[name=\"j_password\"]");
        assert!(focus.contains("e.value = ''"));
        assert!(focus.contains("e.focus()"));
        // The snapshot script reads text and controls, and marks them.
        assert!(SNAPSHOT_SCRIPT.contains("data-tuicanvas"));
        assert!(FORM_VISIBLE_SCRIPT.contains("j_username"));
    }

    #[test]
    fn typed_fields_are_told_apart_from_buttons() {
        let field = |tag: &str, kind: &str| Control {
            tag: tag.into(),
            kind: kind.into(),
            label: "x".into(),
            href: None,
            options: vec![],
        };
        assert!(typed_field(&field("input", "password")));
        assert!(typed_field(&field("input", "text")));
        assert!(!typed_field(&field("input", "submit")));
        assert!(!typed_field(&field("input", "checkbox")));
        assert!(!typed_field(&field("button", "")));
    }

    #[test]
    fn a_key_selects_the_control_its_view_offered() {
        let view = LoginView {
            kind: "remember",
            title: "t".into(),
            lines: vec![],
            actions: vec![('y', 4, "yes".into()), ('n', 2, "no".into())],
            automatic: true,
        };
        assert_eq!(action_for(&view, 'y'), Some(4));
        assert_eq!(action_for(&view, 'n'), Some(2));
        assert_eq!(action_for(&view, 'x'), None);
    }

    #[test]
    fn select_options_keep_their_values() {
        let choice = Choice {
            label: "Phone 1".into(),
            value: "p1".into(),
            disabled: false,
        };
        assert_eq!(choice.value, "p1");
        assert!(select_script(0, &choice.value).contains("\"p1\""));
    }

    #[test]
    fn errors_say_what_to_do_next() {
        for error in [
            SignInError::NoBrowser("Microsoft Edge".into()),
            SignInError::WrongPassword,
            SignInError::DuoDenied,
            SignInError::DuoTimedOut,
            SignInError::WindowClosed,
            SignInError::Network,
            SignInError::Cancelled,
            SignInError::TimedOut,
            SignInError::UnexpectedOrigin,
            SignInError::NativeReadFailed,
        ] {
            let message = error.message();
            assert!(!message.is_empty());
            assert!(message.chars().next().unwrap().is_uppercase(), "{message}");
            assert!(!message.contains('!'), "{message}");
        }
        assert!(
            SignInError::NoBrowser("Microsoft Edge".into())
                .message()
                .contains("Microsoft Edge")
        );
        assert_eq!(from_cdp(CdpError::Closed), SignInError::WindowClosed);
    }
}
