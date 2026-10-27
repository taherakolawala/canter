//! What the terminal shows during sign-in, and how a page is read as one of
//! the known sign-in screens.
//!
//! The screens are UF's GatorLink form, UF's own hand-off pages, and Duo's
//! Universal Prompt. Known screens get a short line on the status display and
//! continue on their own; anything else keeps its own instructions and
//! numbered controls, because a new challenge must not be given a meaning it
//! does not have or be dismissed silently.

use std::io::Write;

use serde::Deserialize;

pub const UF_HOST: &str = "login.ufl.edu";

/// One visible, enabled control on the page.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct Control {
    #[serde(default)]
    pub tag: String,
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub href: Option<String>,
    #[serde(default)]
    pub options: Vec<Choice>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct Choice {
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub value: String,
    #[serde(default)]
    pub disabled: bool,
}

/// A page as read in one pass: its visible text and its controls.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct Snapshot {
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub controls: Vec<Control>,
}

/// Remove escape sequences and control characters so page text can never
/// retarget the terminal.
pub fn clean(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '\u{1b}' {
            match chars.get(i + 1) {
                // An operating-system command runs until BEL or ST.
                Some(']') => {
                    i += 2;
                    while i < chars.len() {
                        if chars[i] == '\u{7}' {
                            i += 1;
                            break;
                        }
                        if chars[i] == '\u{1b}' && chars.get(i + 1) == Some(&'\\') {
                            i += 2;
                            break;
                        }
                        i += 1;
                    }
                    continue;
                }
                // A control sequence runs until its final byte.
                Some('[') => {
                    i += 2;
                    while i < chars.len() && !matches!(chars[i], '@'..='~') {
                        i += 1;
                    }
                    i += 1;
                    continue;
                }
                _ => {}
            }
        }
        let c = chars[i];
        let keep = c == '\n' || c == '\t' || (c as u32 >= 32 && !(127..=159).contains(&(c as u32)));
        if keep {
            out.push(c);
        }
        i += 1;
    }
    out
}

/// A control's label with its whitespace collapsed.
pub fn label(control: &Control) -> String {
    clean(&control.label)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// One screen as the terminal should present it. `automatic` means the screen
/// continues on its own and only shows progress.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoginView {
    pub kind: &'static str,
    pub title: String,
    pub lines: Vec<String>,
    /// (key to press, index of the control it uses, visible label)
    pub actions: Vec<(char, usize, String)>,
    pub automatic: bool,
}

impl LoginView {
    fn progress(kind: &'static str, title: &str) -> Self {
        LoginView {
            kind,
            title: title.to_string(),
            lines: vec![],
            actions: vec![],
            automatic: true,
        }
    }

    fn with_actions(mut self, actions: Vec<(char, usize, String)>) -> Self {
        self.actions = actions;
        self
    }

    /// The keys this screen offers, for the status line.
    pub fn hint(&self) -> String {
        let mut parts: Vec<String> = self
            .actions
            .iter()
            .map(|(key, _, name)| format!("{key} {name}"))
            .collect();
        parts.push("q cancel".to_string());
        parts.join(" · ")
    }
}

fn visible_lines(text: &str) -> Vec<String> {
    clean(text)
        .lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|line| !line.is_empty())
        .collect()
}

fn index_of(controls: &[Control], name: &str) -> Option<usize> {
    controls
        .iter()
        .position(|c| label(c).eq_ignore_ascii_case(name))
}

/// Read a Duo screen. Mirrors the screens observed live on 2026-10-20.
pub fn duo_view(text: &str, controls: &[Control]) -> LoginView {
    let lines = visible_lines(text);
    let lower = lines.join("\n").to_ascii_lowercase();
    let labels: Vec<String> = controls.iter().map(|c| label(c).to_lowercase()).collect();
    let other = index_of(controls, "other options")
        .map(|i| vec![('o', i, "other options".to_string())])
        .unwrap_or_default();

    // Never accept a remembered-device or other consent choice on the
    // user's behalf.
    let yes = index_of(controls, "yes, this is my device");
    let no = index_of(controls, "no, other people use this device");
    if lower.contains("is this your device?")
        && let (Some(yes), Some(no)) = (yes, no)
    {
        return LoginView::progress(
            "remember",
            "Remember this device? Say yes only if nobody else uses it",
        )
        .with_actions(vec![
            ('y', yes, "yes".to_string()),
            ('n', no, "no".to_string()),
        ]);
    }
    // Duo says plainly when a request was refused or ran out of time; neither
    // is retried on its own.
    if lower.contains("request denied") || lower.contains("login denied") {
        return LoginView::progress("denied", "Duo refused the sign-in");
    }
    if lower.contains("request timed out") || lower.contains("request has expired") {
        return LoginView::progress("expired", "The Duo request ran out of time");
    }
    if lower.contains("success!") && lower.contains("logging you in") {
        return LoginView::progress("connecting", "Finishing sign-in");
    }
    if lower.contains("enter code in duo mobile") {
        let codes: Vec<&String> = lines
            .iter()
            .filter(|l| l.len() == 3 && l.chars().all(|c| c.is_ascii_digit()))
            .collect();
        let first = codes.first().copied();
        if let Some(code) = first
            && codes.iter().all(|c| *c == code)
        {
            return LoginView::progress(
                "verified_push",
                &format!("Enter {code} in Duo Mobile to approve"),
            )
            .with_actions(other);
        }
        // A heading can arrive before its code; keep polling rather than
        // asking the user to refresh. A code is never guessed from a phone
        // number's last digits.
        if codes.is_empty() {
            return LoginView::progress("code_loading", "Waiting for your Duo code")
                .with_actions(other);
        }
    }
    if lower.contains("check for a duo push") || lower.contains("approving the notification") {
        let device = device_name(&lines);
        let where_to = device.map(|d| format!(" ({d})")).unwrap_or_default();
        return LoginView::progress(
            "push",
            &format!("Approve the Duo Push on your phone{where_to}"),
        )
        .with_actions(other);
    }
    // Leave out screen-reader announcements, branding and help links.
    let mut body: Vec<String> = vec![];
    for line in &lines {
        let value = line.to_lowercase();
        let noise = value.starts_with("now on page called,")
            || labels.contains(&value)
            || matches!(
                value.as_str(),
                "secured by duo" | "need help?" | "need help?secured by duo"
            );
        if !noise && !body.contains(line) {
            body.push(line.clone());
        }
    }
    let usable: Vec<(usize, String)> = controls
        .iter()
        .enumerate()
        .map(|(i, c)| (i, label(c)))
        .filter(|(_, name)| !matches!(name.to_lowercase().as_str(), "need help?" | "help"))
        .collect();
    let loading = body.is_empty()
        || body.iter().all(|l| {
            matches!(
                l.to_lowercase().as_str(),
                "loading" | "loading..." | "loading…"
            )
        });
    if usable.is_empty() {
        if loading {
            return LoginView::progress("loading", "Waiting for Duo");
        }
        let mut view = LoginView::progress("waiting", "Waiting for sign-in");
        view.lines = body;
        return view;
    }
    // An unknown prompt keeps its instructions and its real controls.
    LoginView {
        kind: "other",
        title: "Sign-in needs your input".into(),
        lines: body,
        actions: usable
            .into_iter()
            .enumerate()
            .filter(|(n, _)| *n < 9)
            .map(|(n, (index, name))| (char::from(b'1' + n as u8), index, name))
            .collect(),
        automatic: false,
    }
}

/// The device a push went to, from Duo's `Sent to "..."` line.
fn device_name(lines: &[String]) -> Option<String> {
    for line in lines {
        let Some(at) = find_ignoring_case(line, "sent to \"") else {
            continue;
        };
        let rest = &line[at + "sent to \"".len()..];
        if let Some(end) = rest.find('"') {
            let name = &rest[..end];
            if !name.is_empty() && name.chars().count() <= 40 {
                return Some(name.to_string());
            }
        }
    }
    None
}

/// Find an ASCII pattern regardless of case. The pattern has no non-ASCII
/// bytes, so the returned offset is always a character boundary.
fn find_ignoring_case(haystack: &str, needle: &str) -> Option<usize> {
    let (h, n) = (haystack.as_bytes(), needle.as_bytes());
    (0..=h.len().checked_sub(n.len())?).find(|&i| h[i..i + n.len()].eq_ignore_ascii_case(n))
}

/// A short line for UF pages that normally pass by on their own. `None` means
/// show the page in full: it has real controls, or it has stayed long enough
/// that its text may explain a problem.
pub fn transition_view(
    host: &str,
    text: &str,
    controls: &[Control],
    seconds_on_page: f64,
) -> Option<LoginView> {
    if host != UF_HOST {
        return None;
    }
    let lower = clean(text).to_lowercase();
    if lower.contains("saving session information") || lower.contains("saving login session") {
        // UF's own page says to ask for help after a minute; show it in full then.
        return (seconds_on_page < 60.0)
            .then(|| LoginView::progress("uf_saving", "Finishing sign-in"));
    }
    let usable = controls
        .iter()
        .filter(|c| !matches!(label(c).to_lowercase().as_str(), "need help?" | "help"))
        .count();
    if usable == 0 && seconds_on_page < 6.0 {
        return Some(LoginView::progress("uf_waiting", "Finishing sign-in"));
    }
    None
}

/// One status line, rewritten in place. When output is not a terminal, each
/// new status is printed once instead.
pub struct Status {
    sink: Box<dyn Write + Send>,
    tty: bool,
    fancy: bool,
    columns: usize,
    text: Option<String>,
    hint: String,
    width: usize,
    frame: usize,
}

const FANCY_FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const PLAIN_FRAMES: [&str; 4] = ["|", "/", "-", "\\"];

impl Status {
    pub fn new() -> Self {
        let tty = std::io::IsTerminal::is_terminal(&std::io::stdout());
        // Windows consoles that canter supports draw the braille spinner;
        // anything redirected gets plain text.
        Status::with_sink(Box::new(std::io::stdout()), tty, tty, 80)
    }

    pub fn with_sink(sink: Box<dyn Write + Send>, tty: bool, fancy: bool, columns: usize) -> Self {
        Status {
            sink,
            tty,
            fancy,
            columns,
            text: None,
            hint: String::new(),
            width: 0,
            frame: 0,
        }
    }

    fn marks(&self) -> (&'static str, &'static str, &'static str, &'static str) {
        if self.fancy {
            ("✓", "•", " · ", "…")
        } else {
            ("OK", "-", " - ", "...")
        }
    }

    pub fn show(&mut self, text: &str, hint: &str) {
        if self.text.as_deref() == Some(text) && self.hint == hint {
            return;
        }
        self.text = Some(text.to_string());
        self.hint = hint.to_string();
        if !self.tty {
            let (_, _, _, ellipsis) = self.marks();
            let suffix = if hint.is_empty() {
                String::new()
            } else {
                format!("  ({hint})")
            };
            let _ = writeln!(self.sink, "{text}{ellipsis}{suffix}");
            let _ = self.sink.flush();
            return;
        }
        self.draw();
    }

    /// Advance the spinner. The caller does this while it waits.
    pub fn tick(&mut self) {
        if self.tty && self.text.is_some() {
            self.frame += 1;
            self.draw();
        }
    }

    fn draw(&mut self) {
        let (_, _, _, ellipsis) = self.marks();
        let frames: &[&str] = if self.fancy {
            &FANCY_FRAMES
        } else {
            &PLAIN_FRAMES
        };
        let spinner = frames[self.frame % frames.len()];
        let text = self.text.clone().unwrap_or_default();
        let mut line = format!("{spinner} {text}{ellipsis}");
        if !self.hint.is_empty() {
            line.push_str(&format!("   ({})", self.hint));
        }
        let line: String = line.chars().take(self.columns.saturating_sub(1)).collect();
        let pad = self.width.saturating_sub(line.chars().count());
        let _ = write!(self.sink, "\r{line}{}", " ".repeat(pad));
        let _ = self.sink.flush();
        self.width = line.chars().count();
    }

    /// Remove the status line before printing anything else or prompting.
    pub fn clear(&mut self) {
        if self.tty && self.width > 0 {
            let _ = write!(self.sink, "\r{}\r", " ".repeat(self.width));
            let _ = self.sink.flush();
        }
        self.width = 0;
        self.text = None;
        self.hint = String::new();
    }

    /// Finish with one line that stays on screen.
    pub fn done(&mut self, text: &str, ok: bool) {
        self.clear();
        let (ok_mark, info_mark, _, _) = self.marks();
        let mark = if ok { ok_mark } else { info_mark };
        let _ = writeln!(self.sink, "{mark} {text}");
        let _ = self.sink.flush();
    }

    /// Print a line without disturbing later progress.
    pub fn line(&mut self, text: &str) {
        self.clear();
        let _ = writeln!(self.sink, "{text}");
        let _ = self.sink.flush();
    }
}

impl Default for Status {
    fn default() -> Self {
        Status::new()
    }
}

/// A sink tests can read back.
#[cfg(test)]
#[derive(Clone, Default)]
pub struct SharedSink(pub std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

#[cfg(test)]
impl SharedSink {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap_or_else(|e| e.into_inner())).into_owned()
    }
}

#[cfg(test)]
impl Write for SharedSink {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn controls(labels: &[&str]) -> Vec<Control> {
        labels
            .iter()
            .map(|text| Control {
                tag: "button".into(),
                kind: "button".into(),
                label: (*text).into(),
                href: None,
                options: vec![],
            })
            .collect()
    }

    const VERIFIED: &str = "Enter code in Duo Mobile\n\
        Verify it's you by entering this verification code in the Duo Mobile app…\n\
        123\n\
        Sent to \"Test phone\" (•••-•••-0123)\n\
        Sent to phone number ending in 0123\n\
        Other options\n\
        Need help?Secured by Duo\n\
        Now on page called, Loading";

    #[test]
    fn verified_push_shows_the_code_and_nothing_else() {
        let view = duo_view(VERIFIED, &controls(&["Other options", "Need help?"]));
        assert_eq!(view.kind, "verified_push");
        assert!(view.automatic);
        assert_eq!(view.title, "Enter 123 in Duo Mobile to approve");
        assert!(view.lines.is_empty());
        assert_eq!(view.actions, vec![('o', 0, "other options".to_string())]);
        assert_eq!(view.hint(), "o other options · q cancel");
    }

    #[test]
    fn remember_device_needs_an_explicit_answer() {
        let view = duo_view(
            "Is this your device?",
            &controls(&["No, other people use this device", "Yes, this is my device"]),
        );
        assert_eq!(view.kind, "remember");
        assert_eq!(view.actions[0].0, 'y');
        assert_eq!(view.actions[0].1, 1);
        assert_eq!(view.actions[1].0, 'n');
        assert_eq!(view.actions[1].1, 0);
    }

    #[test]
    fn loading_and_success_continue_on_their_own() {
        for text in [
            "Secured by Duo\nNow on page called, Loading",
            "Success!\nLogging you in…\nSecured by Duo",
        ] {
            assert!(duo_view(text, &[]).automatic);
        }
        assert_eq!(duo_view("Loading", &[]).kind, "loading");
    }

    #[test]
    fn denied_and_expired_requests_are_told_apart() {
        let denied = duo_view(
            "Login request denied\nFor security, this request was denied.",
            &controls(&["Other options"]),
        );
        assert_eq!(denied.kind, "denied");
        let expired = duo_view(
            "Duo Push\nThe request timed out\nTry again",
            &controls(&["Try again"]),
        );
        assert_eq!(expired.kind, "expired");
    }

    #[test]
    fn unfamiliar_challenge_keeps_its_instructions_and_real_options() {
        let view = duo_view(
            "Enter a passcode from your security key",
            &controls(&["Passcode", "Verify", "Need help?"]),
        );
        assert!(!view.automatic);
        assert!(
            view.lines
                .contains(&"Enter a passcode from your security key".to_string())
        );
        let names: Vec<&str> = view.actions.iter().map(|(_, _, n)| n.as_str()).collect();
        assert_eq!(names, vec!["Passcode", "Verify"]);
        assert_eq!(view.actions[0].0, '1');
    }

    #[test]
    fn a_missing_code_is_not_invented_from_a_phone_number() {
        let view = duo_view(
            "Enter code in Duo Mobile\nSent to phone number ending in 0123",
            &controls(&["Other options"]),
        );
        assert_eq!(view.kind, "code_loading");
        assert!(!view.title.contains("0123"));
        assert!(view.lines.is_empty());
    }

    #[test]
    fn plain_duo_push_names_the_device_without_its_number() {
        // Observed 2026-10-20: a push without a code first, then Verified Push.
        let text = "Check for a Duo Push\n\
            Verify it's you by approving the notification…\n\
            Sent to \"iOS\" (•••-•••-0581)\n\
            Sent to phone number ending in 0581\n\
            Remember me";
        let view = duo_view(
            text,
            &controls(&["Other options", "Remember me", "Need help?"]),
        );
        assert_eq!(view.kind, "push");
        assert!(view.automatic);
        assert_eq!(view.title, "Approve the Duo Push on your phone (iOS)");
        assert!(!view.title.contains("0581"));
    }

    #[test]
    fn uf_hand_off_pages_show_progress_until_they_linger() {
        let saving = transition_view(UF_HOST, "Saving session information", &[], 1.0).unwrap();
        assert_eq!(saving.kind, "uf_saving");
        assert!(transition_view(UF_HOST, "Saving session information", &[], 61.0).is_none());
        assert!(transition_view(UF_HOST, "Please wait", &[], 1.0).is_some());
        assert!(transition_view(UF_HOST, "Please wait", &[], 9.0).is_none());
        assert!(transition_view(UF_HOST, "x", &controls(&["Continue"]), 1.0).is_none());
        assert!(
            transition_view("duo.example.com", "Saving session information", &[], 1.0).is_none()
        );
    }

    #[test]
    fn page_text_cannot_drive_the_terminal() {
        let hostile = "\u{1b}]0;title\u{7}line\u{1b}[31mred\u{1b}[0m\u{7}\n\u{9b}bad";
        assert_eq!(clean(hostile), "linered\nbad");
        let view = duo_view("\u{1b}[2JEnter code in Duo Mobile\n123", &controls(&[]));
        assert_eq!(view.title, "Enter 123 in Duo Mobile to approve");
    }

    #[test]
    fn status_line_updates_in_place_on_a_terminal() {
        let sink = SharedSink::default();
        let mut status = Status::with_sink(Box::new(sink.clone()), true, true, 80);
        status.show("Waiting for Duo", "q cancel");
        status.show("Waiting for Duo", "q cancel");
        status.show("Enter 123 in Duo Mobile to approve", "");
        status.done("Signed in.", true);
        let text = sink.text();
        assert!(text.contains("\r⠋ Waiting for Duo…   (q cancel)"));
        assert!(text.contains("\r⠋ Enter 123 in Duo Mobile to approve…"));
        assert!(text.ends_with("✓ Signed in.\n"));
        // Progress never scrolls the terminal.
        assert_eq!(text.matches('\n').count(), 1);
    }

    #[test]
    fn status_prints_each_state_once_when_not_a_terminal() {
        let sink = SharedSink::default();
        let mut status = Status::with_sink(Box::new(sink.clone()), false, false, 80);
        for _ in 0..3 {
            status.show("Waiting for Duo", "");
        }
        status.done("Signed in.", true);
        assert_eq!(sink.text(), "Waiting for Duo...\nOK Signed in.\n");
    }

    #[test]
    fn tick_advances_the_spinner_and_clear_erases_the_line() {
        let sink = SharedSink::default();
        let mut status = Status::with_sink(Box::new(sink.clone()), true, true, 80);
        status.show("Waiting for Duo", "");
        status.tick();
        status.clear();
        let text = sink.text();
        assert!(text.contains("⠙ Waiting for Duo…"));
        assert!(text.ends_with("\r"));
        assert!(!text.contains('\n'));
    }

    #[test]
    fn snapshot_parses_the_page_script_output() {
        let raw = r#"{"text":"Hello","controls":[
            {"tag":"input","type":"password","label":"Password"},
            {"tag":"select","type":"","label":"Device","options":[{"label":"Phone","value":"1","disabled":false}]},
            {"tag":"a","type":"","label":"Help","href":"https://login.ufl.edu/help"}]}"#;
        let snapshot: Snapshot = serde_json::from_str(raw).unwrap();
        assert_eq!(snapshot.text, "Hello");
        assert_eq!(snapshot.controls.len(), 3);
        assert_eq!(snapshot.controls[0].kind, "password");
        assert_eq!(snapshot.controls[1].options[0].value, "1");
        assert_eq!(
            snapshot.controls[2].href.as_deref(),
            Some("https://login.ufl.edu/help")
        );
        assert_eq!(label(&snapshot.controls[2]), "Help");
    }
}
