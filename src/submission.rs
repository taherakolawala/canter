//! Deliberate, one-shot assignment file submissions.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::Value;

use crate::canvas::CanvasClient;
use crate::model::{AssignmentDetail, valid_id};

#[derive(Clone, Debug, PartialEq)]
pub struct FileChoice {
    pub path: PathBuf,
    pub name: String,
    pub size: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct Attempt {
    #[serde(default)]
    pub attempt: Option<i64>,
    #[serde(default)]
    pub submitted_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub submission_type: Option<String>,
    #[serde(default)]
    pub workflow_state: Option<String>,
    #[serde(default)]
    pub late: Option<bool>,
    #[serde(default)]
    pub grade: Option<String>,
    #[serde(default)]
    pub score: Option<f64>,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default, deserialize_with = "null_or_default")]
    pub attachments: Vec<SubmittedFile>,
}

fn null_or_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct SubmittedFile {
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub filename: Option<String>,
    #[serde(default)]
    pub size: Option<u64>,
    /// Set when Canvas serves the file through its document viewer, which is
    /// also where it draws annotations made on it.
    #[serde(default)]
    pub canvadoc_document_id: Option<Value>,
    #[serde(default)]
    pub preview_url: Option<String>,
}

impl SubmittedFile {
    pub fn name(&self) -> &str {
        self.display_name
            .as_deref()
            .or(self.filename.as_deref())
            .unwrap_or("file")
    }

    /// Whether Canvas shows this file in its document viewer, so the
    /// instructor may have drawn feedback on it that the API has no text for.
    pub fn in_document_viewer(&self) -> bool {
        self.canvadoc_document_id
            .as_ref()
            .is_some_and(|v| !v.is_null())
            || self
                .preview_url
                .as_deref()
                .is_some_and(|u| u.contains("canvadoc"))
    }
}

/// One instructor comment on a submission. Canvas sends the comments of every
/// attempt together, each tagged with the attempt it belongs to.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct Comment {
    #[serde(default, deserialize_with = "crate::model::de_opt_id")]
    pub id: Option<String>,
    #[serde(default)]
    pub author_name: Option<String>,
    #[serde(default)]
    pub created_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub comment: Option<String>,
    /// The attempt the comment was left on; absent on comments Canvas does not
    /// tie to one.
    #[serde(default)]
    pub attempt: Option<i64>,
    #[serde(default, deserialize_with = "null_or_default")]
    pub attachments: Vec<crate::model::CanvasFile>,
    /// Audio or video left in Canvas; it has no text to show here.
    #[serde(default)]
    pub media_comment: Option<Value>,
}

/// Why a comment's content cannot be shown as text in the terminal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrowserOnly {
    /// An audio or video comment recorded in Canvas.
    Media,
    /// Canvas draws this one on the submitted document instead of writing it
    /// out, so the API carries no text for it.
    Annotation,
}

impl BrowserOnly {
    pub fn describe(self) -> &'static str {
        match self {
            BrowserOnly::Media => {
                "Audio or video comment; Canvas plays it only in the browser. Press o to open it."
            }
            BrowserOnly::Annotation => {
                "Drawn on the submitted file in Canvas's document viewer, so it has no text here. Press o to open it."
            }
        }
    }
}

impl Comment {
    pub fn author(&self) -> &str {
        self.author_name
            .as_deref()
            .filter(|a| !a.trim().is_empty())
            .unwrap_or("Instructor")
    }

    pub fn text(&self) -> &str {
        self.comment.as_deref().map(str::trim).unwrap_or("")
    }

    /// `None` when the comment's own words are in the response.
    pub fn browser_only(&self) -> Option<BrowserOnly> {
        if !self.text().is_empty() {
            return None;
        }
        Some(if self.media_comment.is_some() {
            BrowserOnly::Media
        } else {
            BrowserOnly::Annotation
        })
    }

    /// Comment attachments whose ids are safe to put in a request path.
    pub fn files(&self) -> impl Iterator<Item = &crate::model::CanvasFile> {
        self.attachments.iter().filter(|f| valid_id(&f.id))
    }
}

/// What the instructor scored one rubric criterion, keyed in the response by
/// the criterion's id.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct CriterionScore {
    #[serde(default)]
    pub points: Option<f64>,
    #[serde(default)]
    pub comments: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct History {
    pub attempts: Vec<Attempt>,
    /// Every instructor comment on this submission, newest last.
    pub comments: Vec<Comment>,
    /// The instructor's rubric scores, by criterion id. Canvas keeps one per
    /// submission rather than one per attempt.
    pub rubric: std::collections::HashMap<String, CriterionScore>,
}

#[derive(Deserialize)]
struct HistoryResponse {
    #[serde(flatten)]
    current: Attempt,
    submission_history: Vec<Attempt>,
    #[serde(default, deserialize_with = "null_or_default")]
    submission_comments: Vec<Comment>,
    #[serde(default, deserialize_with = "null_or_default")]
    rubric_assessment: std::collections::HashMap<String, CriterionScore>,
}

impl History {
    /// Comments shown with one attempt: those Canvas tagged with its number,
    /// plus, on the latest attempt, the ones it tagged with no attempt.
    pub fn comments_for(&self, attempt: Option<i64>, latest: bool) -> Vec<&Comment> {
        self.comments
            .iter()
            .filter(|c| match c.attempt {
                Some(n) => Some(n) == attempt,
                None => latest,
            })
            .collect()
    }

    pub fn parse(value: Value) -> Result<Self, serde_json::Error> {
        let HistoryResponse {
            current,
            submission_history: mut attempts,
            mut submission_comments,
            rubric_assessment,
        } = serde_json::from_value(value)?;
        if current.submitted_at.is_some()
            && !attempts
                .iter()
                .any(|a| a.attempt == current.attempt && a.submitted_at == current.submitted_at)
        {
            attempts.push(current);
        }
        attempts.retain(|a| a.submitted_at.is_some());
        attempts.sort_by(|a, b| {
            b.attempt
                .cmp(&a.attempt)
                .then_with(|| b.submitted_at.cmp(&a.submitted_at))
        });
        submission_comments.sort_by_key(|c| c.created_at);
        Ok(Self {
            attempts,
            comments: submission_comments,
            rubric: rubric_assessment,
        })
    }
}

/// Plain text (an instructor comment, a rubric note) wrapped to `width`
/// columns at word boundaries, keeping the blank lines the author left.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.clamp(10, 400);
    let mut out = Vec::new();
    // Split the lines first: sanitising drops the line breaks with the other
    // control characters.
    for paragraph in text.lines().map(crate::html::sanitize) {
        let mut line = String::new();
        for word in paragraph.split_whitespace() {
            if line.is_empty() {
                line.push_str(word);
            } else if line.chars().count() + 1 + word.chars().count() <= width {
                line.push(' ');
                line.push_str(word);
            } else {
                out.push(std::mem::take(&mut line));
                line.push_str(word);
            }
            // A single word longer than the line gets its own, overlong line.
        }
        out.push(line);
    }
    while out.last().is_some_and(String::is_empty) {
        out.pop();
    }
    while out.first().is_some_and(String::is_empty) {
        out.remove(0);
    }
    out
}

/// Local path matches are cached for the current directory, so typing each
/// character filters memory instead of repeatedly walking the filesystem.
#[derive(Clone, Debug, Default)]
pub struct PathSuggestions {
    directory: Option<PathBuf>,
    entries: Vec<(String, bool)>,
    pub matches: Vec<PathSuggestion>,
    pub selected: usize,
}

#[derive(Clone, Debug)]
pub struct PathSuggestion {
    pub path: String,
    pub name: String,
    pub directory: bool,
}

pub fn starting_directory() -> String {
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .filter(|path| path.is_dir())
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    let mut path = home.to_string_lossy().into_owned();
    if !path.ends_with(['\\', '/']) {
        path.push(std::path::MAIN_SEPARATOR);
    }
    path
}

impl PathSuggestions {
    pub fn update(&mut self, input: &str) {
        self.matches.clear();
        self.selected = 0;
        let input = input.trim_matches('"');
        if input.is_empty() {
            return;
        }
        let separator = std::path::MAIN_SEPARATOR;
        let (typed_dir, fragment) = match input.rfind(['\\', '/']) {
            Some(index) => (&input[..=index], &input[index + 1..]),
            None => ("", input),
        };
        let expanded_dir = if typed_dir == "~\\" || typed_dir == "~/" {
            std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."))
        } else if typed_dir.is_empty() {
            PathBuf::from(".")
        } else {
            PathBuf::from(typed_dir)
        };
        if self.directory.as_ref() != Some(&expanded_dir) {
            self.directory = Some(expanded_dir.clone());
            self.entries = std::fs::read_dir(&expanded_dir)
                .ok()
                .into_iter()
                .flatten()
                .filter_map(|entry| {
                    let entry = entry.ok()?;
                    let name = entry.file_name().into_string().ok()?;
                    let directory = entry.file_type().ok()?.is_dir();
                    if directory && name.starts_with('.') {
                        return None;
                    }
                    Some((name, directory))
                })
                .collect();
            self.entries.sort_by(|a, b| {
                b.1.cmp(&a.1)
                    .then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase()))
            });
        }
        let prefix = fragment.to_lowercase();
        let output_dir = if typed_dir == "~\\" || typed_dir == "~/" {
            format!("{}{}", expanded_dir.display(), separator)
        } else {
            typed_dir.to_string()
        };
        self.matches = self
            .entries
            .iter()
            .filter(|(name, _)| name.to_lowercase().starts_with(&prefix))
            .take(8)
            .map(|(name, directory)| PathSuggestion {
                path: format!(
                    "{output_dir}{name}{}",
                    if *directory {
                        separator.to_string()
                    } else {
                        String::new()
                    }
                ),
                name: name.clone(),
                directory: *directory,
            })
            .collect();
    }

    pub fn move_selection(&mut self, delta: isize) {
        if !self.matches.is_empty() {
            self.selected =
                (self.selected as isize + delta).rem_euclid(self.matches.len() as isize) as usize;
        }
    }

    pub fn complete(&self) -> Option<String> {
        self.matches
            .get(self.selected)
            .map(|entry| entry.path.clone())
    }
}

pub fn choose_file(
    input: &str,
    assignment: &AssignmentDetail,
    existing: &[FileChoice],
) -> Result<FileChoice, String> {
    let raw = input.trim().trim_matches('"');
    if raw.is_empty() {
        return Err("Enter a file path.".into());
    }
    let path = std::fs::canonicalize(raw).map_err(|_| "File not found.".to_string())?;
    let meta = std::fs::metadata(&path).map_err(|_| "Could not read file metadata.".to_string())?;
    if !meta.is_file() {
        return Err("Select a file, not a folder.".into());
    }
    if meta.len() == 0 {
        return Err("The file is empty.".into());
    }
    if existing.iter().any(|f| f.path == path) {
        return Err("That file is already selected.".into());
    }
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| "File name is not valid UTF-8.".to_string())?
        .to_string();
    if !assignment.allowed_extensions.is_empty() {
        let ext = Path::new(&name)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        if !assignment
            .allowed_extensions
            .iter()
            .any(|allowed| allowed.trim_start_matches('.').eq_ignore_ascii_case(ext))
        {
            return Err(format!(
                "Canvas allows only: {}",
                assignment.allowed_extensions.join(", ")
            ));
        }
    }
    Ok(FileChoice {
        path,
        name,
        size: meta.len(),
    })
}

/// What a draft hands in. Every kind goes through the same review screen and
/// the same explicit confirmation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DraftKind {
    #[default]
    Files,
    Text,
    Url,
}

impl DraftKind {
    /// The Canvas submission type this kind sends.
    pub fn canvas_type(self) -> &'static str {
        match self {
            DraftKind::Files => "online_upload",
            DraftKind::Text => "online_text_entry",
            DraftKind::Url => "online_url",
        }
    }

    pub fn noun(self) -> &'static str {
        match self {
            DraftKind::Files => "file submission",
            DraftKind::Text => "text submission",
            DraftKind::Url => "website URL submission",
        }
    }
}

/// Longest text entry the reader will send. Canvas accepts more; this keeps a
/// mistyped path or a pasted binary from becoming a submission.
pub const MAX_TEXT: usize = 100_000;
/// Longest website address the reader will send.
pub const MAX_URL: usize = 2_000;

/// The text as it will be sent: the user's words, with nothing added and only
/// trailing blank space removed.
pub fn choose_text(input: &str) -> Result<String, String> {
    let text = input.trim_end();
    if text.trim().is_empty() {
        return Err("Type the text to submit first.".into());
    }
    if text.chars().count() > MAX_TEXT {
        return Err(format!("The text is longer than {MAX_TEXT} characters."));
    }
    Ok(text.to_string())
}

/// Read the text of a submission from a file the user named.
pub fn read_text_file(input: &str) -> Result<(String, PathBuf), String> {
    let raw = input.trim().trim_matches('"');
    if raw.is_empty() {
        return Err("Enter a file path.".into());
    }
    let path = std::fs::canonicalize(raw).map_err(|_| "File not found.".to_string())?;
    let meta = std::fs::metadata(&path).map_err(|_| "Could not read file metadata.".to_string())?;
    if !meta.is_file() {
        return Err("Select a file, not a folder.".into());
    }
    if meta.len() == 0 {
        return Err("The file is empty.".into());
    }
    if meta.len() as usize > MAX_TEXT {
        return Err(format!("The file is larger than {MAX_TEXT} bytes."));
    }
    let bytes = std::fs::read(&path).map_err(|_| "Could not read the file.".to_string())?;
    let text = String::from_utf8(bytes).map_err(|_| {
        "The file is not UTF-8 text. Submit it as a file upload instead.".to_string()
    })?;
    let text = choose_text(&text)?;
    Ok((text, path))
}

/// The address as it will be sent. A bare address gets `https://`, and the
/// review screen shows the result, so what is confirmed is what is sent.
pub fn choose_url(input: &str) -> Result<String, String> {
    let raw = input.trim().trim_matches('"');
    if raw.is_empty() {
        return Err("Enter a website address.".into());
    }
    if raw.chars().any(char::is_whitespace) {
        return Err("A web address cannot contain spaces.".into());
    }
    if raw.chars().any(|c| c.is_control()) {
        return Err("That address contains characters Canvas cannot accept.".into());
    }
    let url = match raw.split_once("://") {
        Some(("http" | "https", _)) => raw.to_string(),
        Some((scheme, _)) => {
            return Err(format!(
                "Only http and https addresses can be sent, not {scheme}."
            ));
        }
        None if raw.contains(':') => {
            return Err("Start the address with https://".into());
        }
        None => format!("https://{raw}"),
    };
    let host = url
        .split_once("://")
        .map(|(_, rest)| rest.split(['/', '?', '#']).next().unwrap_or(""))
        .unwrap_or("");
    if host.is_empty() || !host.contains('.') || host.starts_with('.') || host.ends_with('.') {
        return Err("That does not look like a web address.".into());
    }
    if url.chars().count() > MAX_URL {
        return Err(format!("The address is longer than {MAX_URL} characters."));
    }
    Ok(url)
}

/// Whether this assignment will take a submission of this kind now.
pub fn eligible_for(
    assignment: &AssignmentDetail,
    now: DateTime<Utc>,
    kind: DraftKind,
) -> Result<(), String> {
    if !assignment
        .base
        .submission_types
        .iter()
        .any(|t| t == kind.canvas_type())
    {
        return Err(match kind {
            DraftKind::Files => "This assignment does not accept file uploads.".into(),
            DraftKind::Text => "This assignment does not accept typed text.".into(),
            DraftKind::Url => "This assignment does not accept a website URL.".into(),
        });
    }
    if assignment.base.locked_for_user == Some(true)
        || assignment.base.unlock_at.is_some_and(|t| t > now)
        || assignment.base.lock_at.is_some_and(|t| t <= now)
    {
        return Err("This assignment is locked for submissions.".into());
    }
    if let Some(max) = assignment.allowed_attempts
        && max >= 0
        && prior_attempts(assignment) >= max
    {
        return Err("No submission attempts remain.".into());
    }
    Ok(())
}

pub fn prior_attempts(assignment: &AssignmentDetail) -> i64 {
    assignment
        .base
        .submission
        .as_ref()
        .map(|s| {
            s.attempt
                .unwrap_or(0)
                .max(if s.submitted_at.is_some() { 1 } else { 0 })
        })
        .unwrap_or(0)
}

pub async fn submit(
    client: &CanvasClient,
    course: &str,
    assignment: &str,
    files: &[FileChoice],
) -> Result<String, String> {
    if !valid_id(course) || !valid_id(assignment) || files.is_empty() {
        return Err("Invalid submission request.".into());
    }
    let mut ids = Vec::new();
    for file in files {
        let meta = std::fs::metadata(&file.path)
            .map_err(|_| format!("{} is no longer readable.", file.name))?;
        if !meta.is_file() || meta.len() != file.size {
            return Err(format!("{} changed since review. Start again.", file.name));
        }
        let size = file.size.to_string();
        let init: Value = client
            .post_form(
                &format!(
                    "/api/v1/courses/{course}/assignments/{assignment}/submissions/self/files"
                ),
                &[("name", &file.name), ("size", &size)],
            )
            .await
            .map_err(|e| format!("Could not start upload for {}: {}", file.name, e.describe()))?;
        let url = init
            .get("upload_url")
            .and_then(Value::as_str)
            .ok_or_else(|| "Canvas did not provide an upload URL.".to_string())?;
        let params = init
            .get("upload_params")
            .and_then(Value::as_object)
            .ok_or_else(|| "Canvas did not provide upload parameters.".to_string())?;
        let uploaded = client
            .upload_signed(url, params, &file.path, file.size)
            .await
            .map_err(|e| format!("Could not upload {}: {}", file.name, e.describe()))?;
        let id = uploaded
            .get("id")
            .and_then(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .or_else(|| v.as_u64().map(|n| n.to_string()))
            })
            .filter(|id| valid_id(id))
            .ok_or_else(|| format!("Canvas did not confirm the upload of {}.", file.name))?;
        ids.push(id);
    }
    let endpoint = format!("/api/v1/courses/{course}/assignments/{assignment}/submissions");
    let mut fields = vec![("submission[submission_type]", "online_upload")];
    fields.extend(ids.iter().map(|id| ("submission[file_ids][]", id.as_str())));
    let response: Value = client.post_form(&endpoint, &fields).await.map_err(|e| {
        format!(
            "Submission may not have completed: {}. Refresh before trying again.",
            e.describe()
        )
    })?;
    if response
        .get("submitted_at")
        .and_then(Value::as_str)
        .is_none()
    {
        return Err(
            "Canvas did not confirm a submission time. Refresh before trying again.".into(),
        );
    }
    let check: Value = client
        .get_json(&format!("{endpoint}/self?include[]=submission_history"))
        .await
        .map(|(v, _)| v)
        .map_err(|e| {
            format!(
                "Canvas accepted the submission, but verification failed: {}. Refresh to check it.",
                e.describe()
            )
        })?;
    let recorded: HashSet<String> = check
        .get("attachments")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|v| v.get("id"))
        .filter_map(|v| {
            v.as_str()
                .map(str::to_owned)
                .or_else(|| v.as_u64().map(|n| n.to_string()))
        })
        .collect();
    if !ids.iter().all(|id| recorded.contains(id)) {
        return Err("Canvas accepted the submission, but the file list could not be verified. Refresh to check it.".into());
    }
    Ok(format!(
        "Submitted and verified {} file{} on Canvas.",
        files.len(),
        if files.len() == 1 { "" } else { "s" }
    ))
}

/// Hand in typed text or a website address. The content is exactly what the
/// review screen showed; nothing is added here.
pub async fn submit_content(
    client: &CanvasClient,
    course: &str,
    assignment: &str,
    kind: DraftKind,
    content: &str,
) -> Result<String, String> {
    if !valid_id(course) || !valid_id(assignment) || content.is_empty() {
        return Err("Invalid submission request.".into());
    }
    let field = match kind {
        DraftKind::Text => "submission[body]",
        DraftKind::Url => "submission[url]",
        DraftKind::Files => return Err("Files are submitted through the upload flow.".into()),
    };
    let endpoint = format!("/api/v1/courses/{course}/assignments/{assignment}/submissions");
    let response: Value = client
        .post_form(
            &endpoint,
            &[
                ("submission[submission_type]", kind.canvas_type()),
                (field, content),
            ],
        )
        .await
        .map_err(|e| {
            format!(
                "Submission may not have completed: {}. Refresh before trying again.",
                e.describe()
            )
        })?;
    if response
        .get("submitted_at")
        .and_then(Value::as_str)
        .is_none()
    {
        return Err(
            "Canvas did not confirm a submission time. Refresh before trying again.".into(),
        );
    }
    let check: Value = client
        .get_json(&format!("{endpoint}/self?include[]=submission_history"))
        .await
        .map(|(v, _)| v)
        .map_err(|e| {
            format!(
                "Canvas accepted the submission, but verification failed: {}. Refresh to check it.",
                e.describe()
            )
        })?;
    if check.get("submission_type").and_then(Value::as_str) != Some(kind.canvas_type()) {
        return Err(
            "Canvas accepted the submission, but recorded it as another type. Refresh to check it."
                .into(),
        );
    }
    match kind {
        DraftKind::Text => {
            // Canvas sanitises the HTML it stores, so the text is checked for
            // presence rather than for an exact match.
            if check
                .get("body")
                .and_then(Value::as_str)
                .is_none_or(|b| b.trim().is_empty())
            {
                return Err(
                    "Canvas accepted the submission, but recorded no text. Refresh to check it."
                        .into(),
                );
            }
            Ok("Submitted and verified your text on Canvas.".into())
        }
        DraftKind::Url => match check.get("url").and_then(Value::as_str) {
            Some(recorded) if recorded == content => {
                Ok("Submitted and verified the address on Canvas.".into())
            }
            Some(recorded) => Ok(format!(
                "Submitted. Canvas recorded the address as {recorded}."
            )),
            None => Err(
                "Canvas accepted the submission, but recorded no address. Refresh to check it."
                    .into(),
            ),
        },
        DraftKind::Files => unreachable!("rejected above"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::tests::{FakeTransport, client, json};
    use crate::model::{AssignmentSummary, Submission};

    fn assignment() -> AssignmentDetail {
        AssignmentDetail {
            base: AssignmentSummary {
                submission_types: vec!["online_upload".into()],
                ..Default::default()
            },
            allowed_extensions: vec!["pdf".into()],
            allowed_attempts: Some(2),
            ..Default::default()
        }
    }

    #[test]
    fn parses_and_orders_past_attempts_without_duplicating_current() {
        let value = serde_json::json!({
            "attempt": 2,
            "submitted_at": "2026-10-22T12:00:00Z",
            "submission_type": "online_upload",
            "attachments": [{"display_name": "new.pdf", "size": 20}],
            "submission_history": [
                {"attempt": 1, "submitted_at": "2026-10-20T12:00:00Z", "submission_type": "online_text_entry", "body": "<p>First draft</p>"},
                {"attempt": 2, "submitted_at": "2026-10-22T12:00:00Z", "submission_type": "online_upload", "attachments": [{"display_name": "new.pdf", "size": 20}]}
            ]
        });
        let history = History::parse(value).unwrap();
        assert_eq!(history.attempts.len(), 2);
        assert_eq!(history.attempts[0].attempt, Some(2));
        assert_eq!(history.attempts[0].attachments[0].name(), "new.pdf");
        assert_eq!(
            history.attempts[1].body.as_deref(),
            Some("<p>First draft</p>")
        );
        let fallback = History::parse(serde_json::json!({
            "attempt": 1,
            "submitted_at": "2026-10-22T12:00:00Z",
            "attachments": [],
            "submission_history": []
        }))
        .unwrap();
        assert_eq!(fallback.attempts.len(), 1);
        assert!(History::parse(serde_json::json!({"attempt": 2})).is_err());
    }

    #[test]
    fn parses_feedback_comments_and_rubric_scores_from_the_same_read() {
        let value = serde_json::json!({
            "attempt": 2,
            "submitted_at": "2026-10-22T12:00:00Z",
            "submission_type": "online_upload",
            "attachments": [{"display_name": "final.pdf", "size": 20, "canvadoc_document_id": 991}],
            "submission_history": [
                {"attempt": 1, "submitted_at": "2026-10-20T12:00:00Z"},
                {"attempt": 2, "submitted_at": "2026-10-22T12:00:00Z",
                 "attachments": [{"display_name": "final.pdf", "size": 20, "canvadoc_document_id": 991}]}
            ],
            "submission_comments": [
                {"id": 7, "author_name": "Dr Lee", "created_at": "2026-10-23T09:00:00Z",
                 "comment": "Check the second proof.", "attempt": 2,
                 "attachments": [{"id": 55, "display_name": "marked.pdf", "size": 400}]},
                {"id": 6, "author_name": "Dr Lee", "created_at": "2026-10-21T09:00:00Z",
                 "comment": "Rework the first part.", "attempt": 1},
                {"id": 8, "created_at": "2026-10-24T09:00:00Z", "comment": "",
                 "media_comment": {"media_id": "m-1"}},
                {"id": 9, "created_at": "2026-10-24T10:00:00Z", "comment": "   "}
            ],
            "rubric_assessment": {
                "_1": {"points": 4.0, "comments": "Clear."},
                "_2": {"points": 1.5}
            }
        });
        let history = History::parse(value).unwrap();
        // Comments are ordered oldest first and tied to their attempt.
        let latest = history.comments_for(Some(2), true);
        assert_eq!(
            latest
                .iter()
                .filter_map(|c| c.id.clone())
                .collect::<Vec<_>>(),
            ["7", "8", "9"],
            "the latest attempt also shows the comments Canvas tags with no attempt"
        );
        let first = history.comments_for(Some(1), false);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].text(), "Rework the first part.");
        assert_eq!(first[0].author(), "Dr Lee");
        assert!(first[0].browser_only().is_none());
        // Comments with no text cannot be shown here.
        assert_eq!(latest[1].browser_only(), Some(BrowserOnly::Media));
        assert_eq!(latest[2].browser_only(), Some(BrowserOnly::Annotation));
        assert!(
            BrowserOnly::Annotation.describe().contains("Press o"),
            "the row tells the reader how to see it"
        );
        assert_eq!(latest[0].files().count(), 1);
        assert_eq!(latest[0].files().next().unwrap().id, "55");
        assert_eq!(history.rubric["_1"].points, Some(4.0));
        assert_eq!(history.rubric["_1"].comments.as_deref(), Some("Clear."));
        assert_eq!(history.rubric["_2"].comments, None);
        // The submitted file is served through the document viewer, where
        // Canvas also draws annotations.
        assert!(history.attempts[0].attachments[0].in_document_viewer());
        // A response without the includes still parses.
        let plain = History::parse(serde_json::json!({
            "attempt": 1, "submitted_at": "2026-10-20T12:00:00Z",
            "submission_history": [], "submission_comments": null,
            "rubric_assessment": null
        }))
        .unwrap();
        assert!(plain.comments.is_empty() && plain.rubric.is_empty());
    }

    #[test]
    fn comment_attachments_without_usable_ids_are_dropped() {
        let history = History::parse(serde_json::json!({
            "attempt": 1, "submitted_at": "2026-10-20T12:00:00Z", "submission_history": [],
            "submission_comments": [{"id": 1, "comment": "see files", "attempt": 1,
                "attachments": [{"id": "../etc", "display_name": "x"}, {"id": 12, "display_name": "ok.pdf"}]}]
        }))
        .unwrap();
        let files: Vec<&str> = history.comments[0].files().map(|f| f.id.as_str()).collect();
        assert_eq!(files, ["12"]);
    }

    #[test]
    fn plain_text_wraps_at_word_boundaries_and_keeps_paragraphs() {
        let lines = wrap("one two three four five\n\nsecond part", 10);
        assert_eq!(
            lines,
            ["one two", "three four", "five", "", "second", "part"]
        );
        assert_eq!(wrap("   ", 20), Vec::<String>::new());
        // Terminal control sequences never reach the screen.
        assert_eq!(wrap("a\u{1b}[31mb", 20), ["ab"]);
    }

    #[test]
    fn text_and_url_entries_are_checked_before_review() {
        assert_eq!(choose_text("  hello \n").unwrap(), "  hello");
        assert!(choose_text("   ").unwrap_err().contains("Type the text"));
        assert!(
            choose_text(&"x".repeat(MAX_TEXT + 1))
                .unwrap_err()
                .contains("longer than")
        );
        assert_eq!(
            choose_url("example.com/work").unwrap(),
            "https://example.com/work"
        );
        assert_eq!(
            choose_url(" \"http://a.example.com\" ").unwrap(),
            "http://a.example.com"
        );
        assert!(choose_url("ftp://a.com").unwrap_err().contains("Only http"));
        assert!(choose_url("file:/x").unwrap_err().contains("https://"));
        assert!(choose_url("localhost").unwrap_err().contains("web address"));
        assert!(choose_url("a b.com").unwrap_err().contains("spaces"));
        assert!(choose_url("").unwrap_err().contains("Enter a website"));
    }

    #[test]
    fn text_read_from_a_file_must_be_utf8_and_not_empty() {
        let dir = std::env::temp_dir().join(format!("tuicanvas-text-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let good = dir.join("answer.txt");
        let empty = dir.join("empty.txt");
        let binary = dir.join("blob.bin");
        std::fs::write(&good, "My answer.\n").unwrap();
        std::fs::write(&empty, b"").unwrap();
        std::fs::write(&binary, [0xff, 0xfe, 0x00, 0x41]).unwrap();
        let (text, path) = read_text_file(&good.to_string_lossy()).unwrap();
        assert_eq!(text, "My answer.");
        assert!(path.ends_with("answer.txt"));
        assert!(
            read_text_file(&empty.to_string_lossy())
                .unwrap_err()
                .contains("empty")
        );
        assert!(
            read_text_file(&binary.to_string_lossy())
                .unwrap_err()
                .contains("UTF-8")
        );
        assert!(
            read_text_file(&dir.to_string_lossy())
                .unwrap_err()
                .contains("folder")
        );
        assert!(read_text_file("  ").unwrap_err().contains("Enter a file"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn eligibility_is_checked_per_submission_type() {
        let now = Utc::now();
        let mut a = assignment();
        a.base.submission_types = vec!["online_text_entry".into()];
        assert!(eligible_for(&a, now, DraftKind::Text).is_ok());
        assert!(
            eligible_for(&a, now, DraftKind::Url)
                .unwrap_err()
                .contains("website URL")
        );
        assert!(
            eligible_for(&a, now, DraftKind::Files)
                .unwrap_err()
                .contains("file uploads")
        );
        a.base.submission_types = vec!["online_url".into()];
        assert!(eligible_for(&a, now, DraftKind::Url).is_ok());
        // Locks and the attempt limit apply to every kind.
        a.base.locked_for_user = Some(true);
        assert!(
            eligible_for(&a, now, DraftKind::Url)
                .unwrap_err()
                .contains("locked")
        );
    }

    #[tokio::test]
    async fn text_submission_sends_the_reviewed_body_and_verifies_it() {
        let t = FakeTransport::new(vec![
            json(
                200,
                r#"{"id":1,"submitted_at":"2026-10-22T12:00:00Z","submission_type":"online_text_entry"}"#,
            ),
            json(
                200,
                r#"{"submitted_at":"2026-10-22T12:00:00Z","submission_type":"online_text_entry","body":"<p>My answer</p>"}"#,
            ),
        ]);
        let c = client(t.clone());
        let message = submit_content(&c, "10", "20", DraftKind::Text, "My answer")
            .await
            .unwrap();
        assert!(message.contains("verified"), "{message}");
        let requests = t.requests.lock().unwrap();
        assert_eq!(requests.len(), 2, "one write and one verifying read");
        let body = String::from_utf8(requests[0].body.clone().unwrap()).unwrap();
        assert!(
            body.contains("submission%5Bsubmission_type%5D=online_text_entry")
                && body.contains("submission%5Bbody%5D=My+answer"),
            "{body}"
        );
    }

    #[tokio::test]
    async fn url_submission_reports_what_canvas_recorded() {
        let t = FakeTransport::new(vec![
            json(200, r#"{"submitted_at":"2026-10-22T12:00:00Z"}"#),
            json(
                200,
                r#"{"submission_type":"online_url","url":"https://example.com/work/"}"#,
            ),
        ]);
        let c = client(t.clone());
        let message = submit_content(&c, "10", "20", DraftKind::Url, "https://example.com/work")
            .await
            .unwrap();
        assert!(message.contains("https://example.com/work/"), "{message}");
        // A submission Canvas does not confirm is reported as a failure.
        let t = FakeTransport::new(vec![json(200, r#"{"id":1}"#)]);
        assert!(
            submit_content(&client(t), "10", "20", DraftKind::Url, "https://x.example")
                .await
                .unwrap_err()
                .contains("did not confirm")
        );
        // Invalid ids never reach the network.
        let t = FakeTransport::new(vec![]);
        assert!(
            submit_content(&client(t.clone()), "10/x", "20", DraftKind::Text, "hi")
                .await
                .is_err()
        );
        assert!(t.requests.lock().unwrap().is_empty());
    }

    #[test]
    fn validates_paths_extensions_and_duplicates_before_upload() {
        let dir =
            std::env::temp_dir().join(format!("tuicanvas-submit-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let pdf = dir.join("my work.PDF");
        let txt = dir.join("notes.txt");
        std::fs::write(&pdf, b"example").unwrap();
        std::fs::write(&txt, b"example").unwrap();
        let a = assignment();
        let first = choose_file(&pdf.to_string_lossy(), &a, &[]).unwrap();
        assert_eq!(first.size, 7);
        assert!(
            choose_file(&pdf.to_string_lossy(), &a, &[first])
                .unwrap_err()
                .contains("already")
        );
        assert!(
            choose_file(&txt.to_string_lossy(), &a, &[])
                .unwrap_err()
                .contains("only")
        );
        assert!(
            choose_file(&dir.to_string_lossy(), &a, &[])
                .unwrap_err()
                .contains("folder")
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn respects_lock_and_attempt_limit() {
        let now = Utc::now();
        let mut a = assignment();
        assert!(eligible_for(&a, now, DraftKind::Files).is_ok());
        a.base.submission = Some(Submission {
            attempt: Some(2),
            ..Default::default()
        });
        assert!(
            eligible_for(&a, now, DraftKind::Files)
                .unwrap_err()
                .contains("attempt")
        );
        a.base.submission = None;
        a.base.locked_for_user = Some(true);
        assert!(
            eligible_for(&a, now, DraftKind::Files)
                .unwrap_err()
                .contains("locked")
        );
        a.base.locked_for_user = None;
        a.base.submission_types = vec!["online_text_entry".into()];
        assert!(
            eligible_for(&a, now, DraftKind::Files)
                .unwrap_err()
                .contains("does not accept")
        );
    }

    #[test]
    fn submitted_work_counts_as_a_prior_attempt_even_without_attempt_field() {
        let mut a = assignment();
        a.allowed_attempts = Some(1);
        a.base.submission = Some(Submission {
            submitted_at: Some(Utc::now()),
            ..Default::default()
        });
        assert_eq!(prior_attempts(&a), 1);
        assert!(
            eligible_for(&a, Utc::now(), DraftKind::Files)
                .unwrap_err()
                .contains("attempt")
        );
    }

    #[test]
    fn suggests_paths_and_completes_selected_directory_or_file() {
        let root =
            std::env::temp_dir().join(format!("tuicanvas-path-suggestions-{}", std::process::id()));
        std::fs::create_dir_all(root.join("Course Files")).unwrap();
        std::fs::create_dir_all(root.join(".hidden-folder")).unwrap();
        std::fs::write(root.join("coursework.PDF"), b"pdf").unwrap();
        std::fs::write(root.join(".notes"), b"note").unwrap();
        std::fs::write(root.join("other.txt"), b"text").unwrap();
        let mut suggestions = PathSuggestions::default();
        suggestions.update(&format!(
            "{}{}cou",
            root.display(),
            std::path::MAIN_SEPARATOR
        ));
        assert_eq!(suggestions.matches.len(), 2);
        assert!(suggestions.matches[0].directory);
        assert!(
            suggestions
                .complete()
                .unwrap()
                .ends_with(&format!("Course Files{}", std::path::MAIN_SEPARATOR))
        );
        suggestions.move_selection(1);
        assert!(suggestions.complete().unwrap().ends_with("coursework.PDF"));
        suggestions.update(&format!(
            "{}{}COURSEW",
            root.display(),
            std::path::MAIN_SEPARATOR
        ));
        assert_eq!(suggestions.matches.len(), 1);
        assert!(suggestions.complete().unwrap().ends_with("coursework.PDF"));
        suggestions.update(&format!("{}{}.", root.display(), std::path::MAIN_SEPARATOR));
        assert_eq!(suggestions.matches.len(), 1);
        assert_eq!(suggestions.matches[0].name, ".notes");
        std::fs::remove_dir_all(root).unwrap();
    }
}
