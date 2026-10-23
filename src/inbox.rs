//! The Canvas Inbox: conversation folders, lists and messages.
//!
//! Reading a conversation must not change its state on Canvas, so the detail
//! read always sends `auto_mark_as_read=false`. Canvas marks a conversation
//! read as a side effect of that endpoint otherwise, and of nothing else the
//! reader asks for. "Read" is kept locally instead (see `conversation_reads`
//! in `src/cache.rs`).

use chrono::{DateTime, Local, Utc};
use serde::{Deserialize, Serialize};

use crate::canvas::{CanvasClient, CanvasError, FetchStats};
use crate::model::{CanvasFile, de_id, de_opt_id, valid_id};

/// Saved copies keep at most this many conversations in a folder.
pub const MAX_CONVERSATIONS: usize = 100;
/// A saved conversation keeps at most this many messages.
pub const MAX_MESSAGES: usize = 100;

/// One of the folders Canvas's Inbox offers the student.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Folder {
    #[default]
    Inbox,
    Unread,
    Starred,
    Sent,
    Archived,
}

impl Folder {
    /// In the order the Inbox shows them.
    pub const ALL: [Folder; 5] = [
        Folder::Inbox,
        Folder::Unread,
        Folder::Starred,
        Folder::Sent,
        Folder::Archived,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Folder::Inbox => "Inbox",
            Folder::Unread => "Unread",
            Folder::Starred => "Starred",
            Folder::Sent => "Sent",
            Folder::Archived => "Archived",
        }
    }

    /// Stable name for saved copies and for `scope=` (the Inbox has no scope).
    pub fn id(self) -> &'static str {
        match self {
            Folder::Inbox => "inbox",
            Folder::Unread => "unread",
            Folder::Starred => "starred",
            Folder::Sent => "sent",
            Folder::Archived => "archived",
        }
    }

    pub fn of_id(id: &str) -> Option<Folder> {
        Folder::ALL.into_iter().find(|f| f.id() == id)
    }

    /// The folder after this one, or before it.
    pub fn step(self, forward: bool) -> Folder {
        let n = Folder::ALL.len();
        let at = Folder::ALL
            .iter()
            .position(|f| *f == self)
            .unwrap_or_default();
        Folder::ALL[(at + if forward { 1 } else { n - 1 }) % n]
    }

    fn query(self) -> &'static str {
        match self {
            Folder::Inbox => "",
            other => other.id(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Participant {
    #[serde(deserialize_with = "de_id")]
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
}

impl Participant {
    pub fn name(&self) -> &str {
        self.name.as_deref().unwrap_or("Unknown person")
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Summary {
    #[serde(deserialize_with = "de_id")]
    pub id: String,
    #[serde(default)]
    pub subject: Option<String>,
    /// `read`, `unread` or `archived`.
    #[serde(default)]
    pub workflow_state: Option<String>,
    /// The first line or so of the latest message, as Canvas abbreviates it.
    #[serde(default)]
    pub last_message: Option<String>,
    #[serde(default)]
    pub last_message_at: Option<DateTime<Utc>>,
    /// Set instead of `last_message_at` for conversations in Sent.
    #[serde(default)]
    pub last_authored_message_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub message_count: Option<u64>,
    #[serde(default)]
    pub starred: Option<bool>,
    /// The course or group the conversation belongs to, when it has one.
    #[serde(default)]
    pub context_name: Option<String>,
    #[serde(default)]
    pub participants: Vec<Participant>,
}

impl Summary {
    pub fn subject(&self) -> &str {
        self.subject
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("(no subject)")
    }

    /// Everyone in the conversation, as one line.
    pub fn who(&self) -> String {
        if self.participants.is_empty() {
            return "Unknown people".into();
        }
        let names: Vec<&str> = self
            .participants
            .iter()
            .take(3)
            .map(Participant::name)
            .collect();
        let rest = self.participants.len() - names.len();
        if rest > 0 {
            format!("{} +{rest}", names.join(", "))
        } else {
            names.join(", ")
        }
    }

    pub fn date(&self) -> Option<DateTime<Utc>> {
        self.last_message_at.or(self.last_authored_message_at)
    }

    /// Canvas's own state for the conversation. The reader shows its local
    /// reading history instead wherever it knows better.
    pub fn status(&self) -> &'static str {
        match self.workflow_state.as_deref() {
            Some("read") => "Read",
            Some("unread") => "Unread",
            Some("archived") => "Archived",
            _ => "Unknown",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Message {
    #[serde(deserialize_with = "de_id")]
    pub id: String,
    #[serde(default)]
    pub created_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default, deserialize_with = "de_opt_id")]
    pub author_id: Option<String>,
    /// True for the notices Canvas writes itself ("x was added to the
    /// conversation"), which have no author to show.
    #[serde(default)]
    pub generated: Option<bool>,
    #[serde(default)]
    pub attachments: Vec<CanvasFile>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Detail {
    #[serde(flatten)]
    pub base: Summary,
    #[serde(default)]
    pub messages: Vec<Message>,
}

impl Detail {
    /// Who wrote a message, from the conversation's participant list.
    pub fn author_of(&self, message: &Message) -> &str {
        if message.generated == Some(true) {
            return "Canvas";
        }
        message
            .author_id
            .as_deref()
            .and_then(|id| self.base.participants.iter().find(|p| p.id == id))
            .map(Participant::name)
            .unwrap_or("Unknown sender")
    }

    /// Every attachment of every message, in message order.
    pub fn attachments(&self) -> Vec<&CanvasFile> {
        self.messages.iter().flat_map(|m| &m.attachments).collect()
    }

    /// The whole conversation as one HTML document, newest message first,
    /// each under a heading with its author and date. The reader's text and
    /// its files & links list are both built from this document, so the `[n]`
    /// marks in the text and the numbers in the list agree.
    pub fn document(&self) -> String {
        let mut out = String::new();
        for message in &self.messages {
            let when = message
                .created_at
                .map(|at| {
                    at.with_timezone(&Local)
                        .format("%A, %B %-d, %Y at %-I:%M %p")
                        .to_string()
                })
                .unwrap_or_else(|| "Date unavailable".into());
            out.push_str(&format!(
                "<h2>{} &middot; {}</h2>",
                escape(self.author_of(message)),
                escape(&when)
            ));
            out.push_str(&body_html(message.body.as_deref().unwrap_or("")));
        }
        out
    }
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Canvas keeps a message's body as the sender gave it: the Inbox composer
/// sends plain text, while messages Canvas generates from notifications carry
/// HTML. Plain text is escaped, its line breaks become `<br>` and its bare
/// addresses become links, so the HTML renderer and the link list see one
/// kind of document either way.
fn body_html(body: &str) -> String {
    if looks_like_html(body) {
        return body.to_string();
    }
    let mut out = String::from("<p>");
    for (i, line) in body.lines().enumerate() {
        if i > 0 {
            out.push_str("<br>");
        }
        out.push_str(&linkify(line));
    }
    out.push_str("</p>");
    out
}

/// Whether the body carries real markup. A named tag that the renderer knows
/// decides it, so text a person typed (`a < b`, `<3`) is still treated as
/// text and shown as they wrote it.
fn looks_like_html(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    let mut at = 0;
    while let Some(pos) = lower[at..].find('<') {
        at += pos + 1;
        let rest = lower[at..].strip_prefix('/').unwrap_or(&lower[at..]);
        let name: String = rest
            .chars()
            .take_while(char::is_ascii_alphanumeric)
            .collect();
        let known = matches!(
            name.as_str(),
            "a" | "b"
                | "blockquote"
                | "br"
                | "div"
                | "em"
                | "h1"
                | "h2"
                | "h3"
                | "h4"
                | "hr"
                | "i"
                | "img"
                | "li"
                | "ol"
                | "p"
                | "pre"
                | "span"
                | "strong"
                | "table"
                | "td"
                | "tr"
                | "u"
                | "ul"
        );
        if known && rest[name.len()..].starts_with([' ', '>', '/', '\n', '\r', '\t']) {
            return true;
        }
    }
    false
}

/// Wrap bare web addresses in a plain-text line as links, escaping the rest.
fn linkify(line: &str) -> String {
    let mut out = String::new();
    let mut rest = line;
    while let Some(at) = rest.find("http") {
        let tail = &rest[at..];
        if !(tail.starts_with("http://") || tail.starts_with("https://")) {
            let (before, after) = rest.split_at(at + 4);
            out.push_str(&escape(before));
            rest = after;
            continue;
        }
        out.push_str(&escape(&rest[..at]));
        let end = tail
            .find(|c: char| c.is_whitespace() || c == '<' || c == '"')
            .unwrap_or(tail.len());
        // A sentence's full stop or a closing bracket is punctuation, not address.
        let url = tail[..end].trim_end_matches(['.', ',', ';', ':', '!', '?', ')', ']']);
        if url.len() <= "https://".len() {
            out.push_str(&escape(&tail[..end]));
        } else {
            let safe = escape(url);
            out.push_str(&format!("<a href=\"{safe}\">{safe}</a>"));
            out.push_str(&escape(&tail[url.len()..end]));
        }
        rest = &tail[end..];
    }
    out.push_str(&escape(rest));
    out
}

fn check(id: &str) -> Result<(), CanvasError> {
    if valid_id(id) {
        Ok(())
    } else {
        Err(CanvasError::Invalid("invalid conversation ID"))
    }
}

/// Newest activity first; conversations without a date come last.
fn sort(list: &mut [Summary]) {
    list.sort_by(|a, b| {
        b.date()
            .cmp(&a.date())
            .then_with(|| b.id.len().cmp(&a.id.len()).then_with(|| b.id.cmp(&a.id)))
    });
}

/// One folder of the Inbox. This read never changes a conversation's state.
pub async fn list(
    client: &CanvasClient,
    folder: Folder,
) -> Result<(Vec<Summary>, FetchStats), CanvasError> {
    let scope = folder.query();
    let path = if scope.is_empty() {
        "/api/v1/conversations?per_page=100".to_string()
    } else {
        format!("/api/v1/conversations?scope={scope}&per_page=100")
    };
    let (mut items, stats) = client.get_all_pages::<Summary>(&path, 10).await?;
    if items.iter().any(|c| !valid_id(&c.id)) {
        return Err(CanvasError::Invalid("invalid conversation ID"));
    }
    sort(&mut items);
    let mut seen = std::collections::HashSet::new();
    items.retain(|c| seen.insert(c.id.clone()));
    items.truncate(MAX_CONVERSATIONS);
    Ok((items, stats))
}

/// One conversation with its messages. `auto_mark_as_read=false` keeps Canvas
/// from marking it read; without it this endpoint would do so.
pub async fn detail(client: &CanvasClient, id: &str) -> Result<(Detail, FetchStats), CanvasError> {
    check(id)?;
    let (mut detail, stats) = client
        .get_json::<Detail>(&format!(
            "/api/v1/conversations/{id}?auto_mark_as_read=false"
        ))
        .await?;
    if detail.base.id != id {
        return Err(CanvasError::Invalid("unexpected conversation"));
    }
    detail
        .messages
        .sort_by_key(|m| std::cmp::Reverse(m.created_at));
    detail.messages.truncate(MAX_MESSAGES);
    for message in &mut detail.messages {
        message.attachments.retain(|f| valid_id(&f.id));
    }
    Ok((detail, stats))
}

/// Exactly what a confirmed message will send. Built on the review screen,
/// so what the user read there is what goes to Canvas.
#[derive(Clone, Debug, PartialEq)]
pub enum Send {
    /// One more message in an existing conversation, to everyone in it.
    Reply {
        conversation_id: String,
        body: String,
    },
    /// A new conversation with a course's teachers.
    ToTeachers {
        course_id: String,
        subject: String,
        body: String,
    },
}

/// The longest message Canvas is asked to take. Well under its own limit;
/// enough for anything typed into a terminal box.
pub const MAX_BODY: usize = 8_000;
pub const MAX_SUBJECT: usize = 255;

impl Send {
    /// Why this cannot be sent, if it cannot. Checked again here so no path
    /// to the request can skip it.
    pub fn problem(&self) -> Option<&'static str> {
        let (body, subject) = match self {
            Send::Reply { body, .. } => (body, None),
            Send::ToTeachers { subject, body, .. } => (body, Some(subject)),
        };
        if body.trim().is_empty() {
            return Some("The message is empty.");
        }
        if body.chars().count() > MAX_BODY {
            return Some("The message is too long.");
        }
        if subject.is_some_and(|s| s.trim().is_empty()) {
            return Some("The subject is empty.");
        }
        if subject.is_some_and(|s| s.chars().count() > MAX_SUBJECT) {
            return Some("The subject is too long.");
        }
        match self {
            Send::Reply {
                conversation_id: id,
                ..
            } if !valid_id(id) => Some("Invalid conversation."),
            Send::ToTeachers { course_id, .. } if !valid_id(course_id) => Some("Invalid course."),
            _ => None,
        }
    }
}

/// Send a message. Called only after the user confirmed the review screen.
/// Sending a reply does let Canvas mark that conversation read, which is
/// what sending means; nothing else here changes read state.
pub async fn send(client: &CanvasClient, message: &Send) -> Result<String, String> {
    if let Some(problem) = message.problem() {
        return Err(problem.to_string());
    }
    match message {
        Send::Reply {
            conversation_id,
            body,
        } => {
            let sent: Detail = client
                .post_form(
                    &format!("/api/v1/conversations/{conversation_id}/add_message"),
                    &[("body", body.as_str())],
                )
                .await
                .map_err(|e| {
                    format!(
                        "Reply may not have been sent: {}. Refresh before trying again.",
                        e.describe()
                    )
                })?;
            if sent.base.id != *conversation_id || sent.messages.is_empty() {
                return Err(
                    "Canvas did not confirm the reply. Refresh to check whether it was sent."
                        .into(),
                );
            }
            Ok("Reply sent.".into())
        }
        Send::ToTeachers {
            course_id,
            subject,
            body,
        } => {
            // Canvas addresses a course's teachers by context code. Without
            // group_conversation it gives each teacher a private copy.
            let recipient = format!("course_{course_id}_teachers");
            let created: Vec<Summary> = client
                .post_form(
                    "/api/v1/conversations",
                    &[
                        ("recipients[]", recipient.as_str()),
                        ("subject", subject.as_str()),
                        ("body", body.as_str()),
                        ("context_code", &format!("course_{course_id}")),
                        ("mode", "sync"),
                    ],
                )
                .await
                .map_err(|e| {
                    format!(
                        "Message may not have been sent: {}. Check your Sent folder.",
                        e.describe()
                    )
                })?;
            if created.is_empty() || created.iter().any(|c| !valid_id(&c.id)) {
                return Err("Canvas did not confirm the message. Check your Sent folder.".into());
            }
            Ok(format!(
                "Message sent to {} teacher{}.",
                created.len(),
                if created.len() == 1 { "" } else { "s" }
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::tests::{FakeTransport, client, json};

    #[test]
    fn folders_cycle_both_ways_and_keep_stable_names() {
        assert_eq!(Folder::Inbox.step(true), Folder::Unread);
        assert_eq!(Folder::Inbox.step(false), Folder::Archived);
        assert_eq!(Folder::Archived.step(true), Folder::Inbox);
        for folder in Folder::ALL {
            assert_eq!(Folder::of_id(folder.id()), Some(folder));
        }
        assert_eq!(Folder::of_id("drafts"), None);
        assert_eq!(Folder::Inbox.query(), "");
        assert_eq!(Folder::Archived.query(), "archived");
    }

    #[tokio::test]
    async fn every_folder_reads_its_own_scope_and_the_inbox_sends_none() {
        for folder in Folder::ALL {
            let t = FakeTransport::new(vec![json(200, "[]")]);
            let (items, _) = list(&client(t.clone()), folder).await.unwrap();
            assert!(items.is_empty());
            let url = t.requests.lock().unwrap()[0].url.clone();
            if folder == Folder::Inbox {
                assert!(!url.contains("scope="), "{url}");
            } else {
                assert!(url.contains(&format!("scope={}", folder.id())), "{url}");
            }
        }
    }

    #[tokio::test]
    async fn list_sorts_by_latest_activity_and_caps_saved_conversations() {
        let t = FakeTransport::new(vec![json(
            200,
            r#"[{"id":1,"subject":"Older","last_message_at":"2026-10-01T00:00:00Z","workflow_state":"read"},
                {"id":2,"subject":"Newest","last_message_at":"2026-10-21T00:00:00Z","workflow_state":"unread",
                 "participants":[{"id":7,"name":"Dr Ada"},{"id":8,"name":"You"}]},
                {"id":3},
                {"id":4,"subject":"Sent","last_authored_message_at":"2026-10-10T00:00:00Z"}]"#,
        )]);
        let (items, stats) = list(&client(t), Folder::Inbox).await.unwrap();
        assert_eq!(
            items.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
            ["2", "4", "1", "3"]
        );
        assert_eq!(items[0].status(), "Unread");
        assert_eq!(items[0].who(), "Dr Ada, You");
        assert_eq!(items[2].status(), "Read");
        assert_eq!(items[3].subject(), "(no subject)");
        assert_eq!(items[3].status(), "Unknown");
        assert_eq!(stats.requests, 1);

        // More than the cap: only the newest are kept for the saved copy.
        let many: Vec<String> = (0..MAX_CONVERSATIONS + 30)
            .map(|n| {
                format!(
                    r#"{{"id":{},"last_message_at":"2026-01-01T00:00:{:02}Z"}}"#,
                    n + 1,
                    n % 60
                )
            })
            .collect();
        let t = FakeTransport::new(vec![json(200, &format!("[{}]", many.join(",")))]);
        let (items, _) = list(&client(t), Folder::Inbox).await.unwrap();
        assert_eq!(items.len(), MAX_CONVERSATIONS);
    }

    #[tokio::test]
    async fn list_follows_pages_and_refuses_unsafe_ids() {
        let mut first = json(200, r#"[{"id":"1"}]"#).unwrap();
        first.headers.push((
            "link".into(),
            "<https://ufl.instructure.com/api/v1/conversations?page=2>; rel=\"next\"".into(),
        ));
        let t = FakeTransport::new(vec![Ok(first), json(200, r#"[{"id":"2"}]"#)]);
        let (items, stats) = list(&client(t), Folder::Inbox).await.unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(stats.requests, 2);
        let t = FakeTransport::new(vec![json(200, r#"[{"id":"../secrets"}]"#)]);
        assert!(list(&client(t), Folder::Inbox).await.is_err());
    }

    #[tokio::test]
    async fn detail_never_lets_canvas_mark_the_conversation_read() {
        let t = FakeTransport::new(vec![json(
            200,
            r#"{"id":5,"subject":"Lab 3","participants":[{"id":7,"name":"Dr Ada"}],
                "messages":[{"id":9,"created_at":"2026-10-20T00:00:00Z","body":"First","author_id":7}]}"#,
        )]);
        let (d, _) = detail(&client(t.clone()), "5").await.unwrap();
        assert_eq!(d.base.subject(), "Lab 3");
        assert_eq!(d.author_of(&d.messages[0]), "Dr Ada");
        let url = t.requests.lock().unwrap()[0].url.clone();
        assert!(url.contains("auto_mark_as_read=false"), "{url}");
        assert!(!url.contains("workflow_state"), "{url}");
        assert_eq!(
            t.requests.lock().unwrap()[0].method,
            crate::transport::Method::Get
        );
    }

    #[tokio::test]
    async fn detail_validates_identity_and_caps_messages() {
        let t = FakeTransport::new(vec![json(200, r#"{"id":6,"messages":[]}"#)]);
        assert!(detail(&client(t), "5").await.is_err());
        let t = FakeTransport::new(vec![]);
        assert!(detail(&client(t.clone()), "../x").await.is_err());
        assert!(t.requests.lock().unwrap().is_empty());

        let many: Vec<String> = (0..MAX_MESSAGES + 10)
            .map(|n| {
                format!(
                    r#"{{"id":{},"created_at":"2026-01-01T00:00:{:02}Z","body":"m"}}"#,
                    n + 1,
                    n % 60
                )
            })
            .collect();
        let t = FakeTransport::new(vec![json(
            200,
            &format!(r#"{{"id":5,"messages":[{}]}}"#, many.join(",")),
        )]);
        let (d, _) = detail(&client(t), "5").await.unwrap();
        assert_eq!(d.messages.len(), MAX_MESSAGES);
    }

    #[tokio::test]
    async fn messages_read_newest_first_with_only_safe_attachments() {
        let t = FakeTransport::new(vec![json(
            200,
            r#"{"id":5,"participants":[{"id":7,"name":"Dr Ada"}],"messages":[
                {"id":1,"created_at":"2026-10-01T00:00:00Z","body":"Older","author_id":7},
                {"id":2,"created_at":"2026-10-20T00:00:00Z","body":"Newer","author_id":7,
                 "attachments":[{"id":9,"display_name":"rubric.pdf"},{"id":"../x","display_name":"bad"}]},
                {"id":3,"created_at":"2026-10-10T00:00:00Z","generated":true,"body":"Ada was added"}]}"#,
        )]);
        let (d, _) = detail(&client(t), "5").await.unwrap();
        assert_eq!(
            d.messages.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            ["2", "3", "1"]
        );
        assert_eq!(d.author_of(&d.messages[1]), "Canvas");
        let files: Vec<&str> = d.attachments().iter().map(|f| f.id.as_str()).collect();
        assert_eq!(files, ["9"]);
    }

    #[test]
    fn plain_bodies_keep_their_line_breaks_and_their_addresses() {
        let message = Message {
            body: Some("See\nhttps://ufl.instructure.com/courses/10. Thanks <3".into()),
            ..Default::default()
        };
        let detail = Detail {
            messages: vec![message],
            ..Default::default()
        };
        let document = detail.document();
        assert!(document.contains("<br>"), "{document}");
        assert!(
            document.contains("<a href=\"https://ufl.instructure.com/courses/10\">"),
            "{document}"
        );
        // The full stop after the address is text, and `<3` is not a tag.
        assert!(document.contains("courses/10</a>."), "{document}");
        assert!(document.contains("&lt;3"), "{document}");
        let links = crate::html::links(&document, "https://ufl.instructure.com");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].url, "https://ufl.instructure.com/courses/10");
    }

    #[test]
    fn bodies_canvas_wrote_as_html_are_left_alone() {
        let detail = Detail {
            messages: vec![Message {
                body: Some("<p>Graded: <a href=\"/courses/10/grades\">grades</a></p>".into()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let document = detail.document();
        assert!(
            document.contains("<a href=\"/courses/10/grades\">"),
            "{document}"
        );
        assert_eq!(
            crate::html::links(&document, "https://ufl.instructure.com").len(),
            1
        );
    }

    #[test]
    fn markup_is_told_from_typed_text_by_the_tags_it_uses() {
        // A lone self-closing tag is still markup: its image must be linkable.
        let image = Detail {
            messages: vec![Message {
                body: Some("<img src='/courses/10/files/12/preview'>".into()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let links = crate::html::links(&image.document(), "https://ufl.instructure.com");
        assert_eq!(links.len(), 1, "{}", image.document());
        assert!(links[0].image);
        // Text a person typed is shown as typed, not swallowed as tags.
        for typed in ["a < b and b > c", "I give it <3", "use x<y"] {
            let detail = Detail {
                messages: vec![Message {
                    body: Some(typed.into()),
                    ..Default::default()
                }],
                ..Default::default()
            };
            let document = detail.document();
            assert!(document.contains("&lt;"), "{typed}: {document}");
            let shown = crate::html::render(&document, 60, "https://ufl.instructure.com").join(" ");
            assert!(shown.contains('<'), "{typed}: {shown}");
        }
    }

    #[test]
    fn the_document_heads_each_message_with_its_author_and_date() {
        let detail = Detail {
            base: Summary {
                participants: vec![Participant {
                    id: "7".into(),
                    name: Some("Dr Ada".into()),
                }],
                ..Default::default()
            },
            messages: vec![Message {
                author_id: Some("7".into()),
                created_at: Some(
                    DateTime::parse_from_rfc3339("2026-10-20T15:04:00Z")
                        .unwrap()
                        .with_timezone(&Utc),
                ),
                body: Some("Hello".into()),
                ..Default::default()
            }],
        };
        let document = detail.document();
        assert!(document.contains("<h2>Dr Ada &middot; "), "{document}");
        let lines = crate::html::render(&document, 60, "https://ufl.instructure.com");
        assert!(lines.iter().any(|l| l.contains("Dr Ada")), "{lines:?}");
        assert!(lines.iter().any(|l| l.contains("Hello")), "{lines:?}");
    }

    #[tokio::test]
    async fn nothing_is_sent_until_the_message_passes_its_own_checks() {
        let empty = Send::Reply {
            conversation_id: "5".into(),
            body: "   \n ".into(),
        };
        let cases = [
            empty.clone(),
            Send::Reply {
                conversation_id: "../x".into(),
                body: "Hello".into(),
            },
            Send::Reply {
                conversation_id: "5".into(),
                body: "x".repeat(MAX_BODY + 1),
            },
            Send::ToTeachers {
                course_id: "10".into(),
                subject: " ".into(),
                body: "Hello".into(),
            },
            Send::ToTeachers {
                course_id: "10".into(),
                subject: "x".repeat(MAX_SUBJECT + 1),
                body: "Hello".into(),
            },
            Send::ToTeachers {
                course_id: "10~".into(),
                subject: "Q".into(),
                body: "Hello".into(),
            },
        ];
        for case in &cases[..cases.len() - 1] {
            assert!(case.problem().is_some(), "{case:?}");
            let t = FakeTransport::new(vec![]);
            assert!(send(&client(t.clone()), case).await.is_err());
            assert!(t.requests.lock().unwrap().is_empty(), "{case:?}");
        }
        // A shard id is a valid Canvas id, so that one is allowed through.
        assert!(cases[cases.len() - 1].problem().is_none());
    }

    #[tokio::test]
    async fn a_reply_adds_one_message_and_is_confirmed_by_canvas() {
        let t = FakeTransport::new(vec![json(
            200,
            r#"{"id":5,"messages":[{"id":9,"body":"Thanks"}]}"#,
        )]);
        let message = Send::Reply {
            conversation_id: "5".into(),
            body: "Thanks".into(),
        };
        assert_eq!(
            send(&client(t.clone()), &message).await.unwrap(),
            "Reply sent."
        );
        {
            let requests = t.requests.lock().unwrap();
            assert_eq!(requests.len(), 1);
            assert_eq!(requests[0].method, crate::transport::Method::Post);
            assert!(requests[0].url.ends_with("/conversations/5/add_message"));
            let body = String::from_utf8_lossy(requests[0].body.as_ref().unwrap());
            assert_eq!(body, "body=Thanks");
            // No workflow_state, and nothing that would mark anything read.
            assert!(!body.contains("workflow_state"), "{body}");
        }

        // A conversation Canvas does not confirm is reported, not assumed.
        for reply in [
            r#"{"id":6,"messages":[{"id":9}]}"#,
            r#"{"id":5,"messages":[]}"#,
        ] {
            let t = FakeTransport::new(vec![json(200, reply)]);
            assert!(send(&client(t), &message).await.is_err(), "{reply}");
        }
    }

    #[tokio::test]
    async fn a_new_message_goes_to_one_course_s_teachers() {
        let t = FakeTransport::new(vec![json(
            200,
            r#"[{"id":11,"subject":"Lab 3"},{"id":12,"subject":"Lab 3"}]"#,
        )]);
        let message = Send::ToTeachers {
            course_id: "10".into(),
            subject: "Lab 3".into(),
            body: "A question about part b.".into(),
        };
        assert_eq!(
            send(&client(t.clone()), &message).await.unwrap(),
            "Message sent to 2 teachers."
        );
        {
            let requests = t.requests.lock().unwrap();
            assert!(requests[0].url.ends_with("/api/v1/conversations"));
            let body = String::from_utf8_lossy(requests[0].body.as_ref().unwrap());
            assert!(
                body.contains("recipients%5B%5D=course_10_teachers"),
                "{body}"
            );
            assert!(body.contains("context_code=course_10"), "{body}");
            assert!(body.contains("subject=Lab+3"), "{body}");
            assert!(body.contains("mode=sync"), "{body}");
        }
        // Canvas answering with nothing created is a failure, not a success.
        let t = FakeTransport::new(vec![json(200, "[]")]);
        assert!(send(&client(t), &message).await.is_err());
    }

    /// Content-free live validation of the exact Rust reader, using the saved
    /// session: counts and timings only, and a check that Canvas's read state
    /// is the same before and after the reader opens a conversation.
    #[tokio::test]
    #[ignore = "requires a valid saved Canvas session and network access"]
    async fn live_inbox() {
        use std::sync::Arc;
        let config = crate::config::Config::load().unwrap();
        let session = crate::session::SessionFile::load(&config.session_path(), &config.origin)
            .unwrap()
            .expect("sign in first");
        let client = CanvasClient::new(
            &config.origin,
            &config.host,
            Arc::new(crate::transport::ReqwestTransport::new().unwrap()),
            session.jar,
            Arc::new(crate::diag::Diagnostics::new(None)),
        );
        let mut report = Vec::new();
        for folder in Folder::ALL {
            let (items, stats) = list(&client, folder).await.unwrap();
            assert!(items.windows(2).all(|w| w[0].date() >= w[1].date()));
            report.push(serde_json::json!({
                "folder": folder.id(), "count": items.len(), "list": stats,
            }));
        }
        // Opening an unread conversation must leave Canvas's state untouched.
        let (unread, _) = list(&client, Folder::Unread).await.unwrap();
        let checked = match unread.first() {
            Some(c) => {
                let (d, stats) = detail(&client, &c.id).await.unwrap();
                assert_eq!(d.base.id, c.id);
                let (again, _) = list(&client, Folder::Unread).await.unwrap();
                let still_unread = again.iter().any(|x| x.id == c.id);
                assert!(still_unread, "Canvas marked a conversation read");
                serde_json::json!({
                    "messages": d.messages.len(),
                    "attachments": d.attachments().len(),
                    "still_unread": still_unread, "detail": stats,
                })
            }
            None => serde_json::json!({"messages": 0, "note": "nothing unread"}),
        };
        report.push(serde_json::json!({"read_state_check": checked}));
        let path = config.private_reports().join("inbox-live.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
        println!(
            "Validated {} Inbox folders and one conversation; saved counts/timings only.",
            Folder::ALL.len()
        );
    }
}
