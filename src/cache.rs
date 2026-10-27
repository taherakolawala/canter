//! Account-scoped snapshots and local UI preferences. Snapshots are displayed
//! as saved copies while fresh requests run; preferences are kept separately.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ResourceKey {
    Courses,
    /// One month of the home-screen calendar (`2026-10`).
    Planner(String),
    /// The student's course colours.
    Colors,
    Assignments(String),
    Detail(String, String),
    /// Loaded on demand; kept only in memory because attempts may contain work.
    History(String, String),
    Announcements(String),
    Announcement(String, String),
    /// A course's Files area (folder tree and file list).
    Files(String),
    /// One Canvas file's metadata.
    File(String),
    /// The course tabs Canvas shows the student (read once per run).
    Tabs(String),
    /// A course's modules with their items.
    Modules(String),
    /// A course wiki page, by its URL name.
    WikiPage(String, String),
    /// A course's grades: its groups, scores and instructor comments.
    Grades(String),
    /// A course's page list, without the pages' text.
    Pages(String),
    /// One folder of the Inbox, by its name (`inbox`, `unread`, ...).
    Conversations(String),
    /// One Inbox conversation with its messages.
    Conversation(String),
}

impl ResourceKey {
    fn file_name(&self) -> Option<String> {
        let ok = |id: &str| crate::model::valid_id(id);
        let name = match self {
            ResourceKey::Courses => "courses".to_string(),
            ResourceKey::Planner(m) if crate::calendar::month_of(m).is_some() && m.len() == 7 => {
                format!("planner-{m}")
            }
            ResourceKey::Colors => "colors".to_string(),
            ResourceKey::Announcements(c) if ok(c) => {
                format!("announcements-{}", c.replace('~', "_"))
            }
            ResourceKey::Announcement(c, a) if ok(c) && ok(a) => format!(
                "announcement-{}-{}",
                c.replace('~', "_"),
                a.replace('~', "_")
            ),
            ResourceKey::Assignments(c) if ok(c) => format!("assignments-{}", c.replace('~', "_")),
            ResourceKey::Detail(c, a) if ok(c) && ok(a) => {
                format!("assignment-{}-{}", c.replace('~', "_"), a.replace('~', "_"))
            }
            ResourceKey::History(..) => return None,
            ResourceKey::Files(c) if ok(c) => format!("files-{}", c.replace('~', "_")),
            ResourceKey::File(f) if ok(f) => format!("file-{}", f.replace('~', "_")),
            ResourceKey::Tabs(c) if ok(c) => format!("tabs-{}", c.replace('~', "_")),
            ResourceKey::Modules(c) if ok(c) => format!("modules-{}", c.replace('~', "_")),
            ResourceKey::WikiPage(c, u) if ok(c) && u == crate::modules::FRONT_PAGE => {
                format!("home-{}", c.replace('~', "_"))
            }
            ResourceKey::WikiPage(c, u) if ok(c) && crate::modules::valid_slug(u) => {
                format!("page-{}-{u}", c.replace('~', "_"))
            }
            ResourceKey::Grades(c) if ok(c) => format!("grades-{}", c.replace('~', "_")),
            ResourceKey::Pages(c) if ok(c) => format!("pages-{}", c.replace('~', "_")),
            ResourceKey::Conversations(f) if crate::inbox::Folder::of_id(f).is_some() => {
                format!("conversations-{f}")
            }
            ResourceKey::Conversation(c) if ok(c) => {
                format!("conversation-{}", c.replace('~', "_"))
            }
            _ => return None,
        };
        Some(name + ".json")
    }

    pub fn label(&self) -> &'static str {
        match self {
            ResourceKey::Courses => "courses",
            ResourceKey::Planner(_) => "calendar",
            ResourceKey::Colors => "colors",
            ResourceKey::Announcements(_) => "announcements",
            ResourceKey::Announcement(..) => "announcement",
            ResourceKey::Assignments(_) => "assignments",
            ResourceKey::Detail(..) => "assignment",
            ResourceKey::History(..) => "submission history",
            ResourceKey::Files(_) => "files",
            ResourceKey::File(_) => "file",
            ResourceKey::Tabs(_) => "course tabs",
            ResourceKey::Modules(_) => "modules",
            ResourceKey::WikiPage(..) => "page",
            ResourceKey::Grades(_) => "grades",
            ResourceKey::Pages(_) => "pages",
            ResourceKey::Conversations(_) => "inbox",
            ResourceKey::Conversation(_) => "conversation",
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Envelope<T> {
    version: u32,
    account: String,
    fetched_at: DateTime<Utc>,
    data: T,
}

const VERSION: u32 = 1;

pub struct SnapshotStore {
    dir: PathBuf,
    account: String,
}

impl SnapshotStore {
    /// `root` is origin-scoped; each account gets its own directory.
    pub fn new(root: &Path, account: &str) -> Self {
        let safe: String = account
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '~')
            .collect();
        SnapshotStore {
            dir: root.join(format!("user-{safe}")),
            account: account.to_string(),
        }
    }

    pub fn load<T: DeserializeOwned>(&self, key: &ResourceKey) -> Option<(T, DateTime<Utc>)> {
        self.load_named(&key.file_name()?)
    }

    fn load_named<T: DeserializeOwned>(&self, name: &str) -> Option<(T, DateTime<Utc>)> {
        let bytes = std::fs::read(self.dir.join(name)).ok()?;
        let env: Envelope<T> = serde_json::from_slice(&bytes).ok()?;
        (env.version == VERSION && env.account == self.account)
            .then_some((env.data, env.fetched_at))
    }

    pub fn save<T: Serialize>(&self, key: &ResourceKey, data: &T, fetched_at: DateTime<Utc>) {
        let Some(name) = key.file_name() else { return };
        self.save_named(&name, data, fetched_at);
    }

    fn save_named<T: Serialize>(&self, name: &str, data: &T, fetched_at: DateTime<Utc>) {
        let env = Envelope {
            version: VERSION,
            account: self.account.clone(),
            fetched_at,
            data,
        };
        let Ok(bytes) = serde_json::to_vec(&env) else {
            return;
        };
        if std::fs::create_dir_all(&self.dir).is_err() {
            return;
        }
        let path = self.dir.join(name);
        let tmp = self.dir.join(format!("{name}.{}.tmp", std::process::id()));
        if std::fs::write(&tmp, bytes).is_ok() && std::fs::rename(&tmp, &path).is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
    }

    pub fn clear(&self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }

    /// Local reading history is separate from refreshed Canvas snapshots.
    pub fn announcement_reads(&self) -> std::collections::BTreeSet<(String, String)> {
        self.load_named("local-announcement-reads.json")
            .map(|(reads, _)| reads)
            .unwrap_or_default()
    }

    pub fn save_announcement_reads(&self, reads: &std::collections::BTreeSet<(String, String)>) {
        self.save_named("local-announcement-reads.json", reads, Utc::now());
    }

    /// Instructor comments already seen in canter, by course and
    /// assignment. Canvas is never told what has been read.
    pub fn seen_feedback(&self) -> std::collections::BTreeSet<(String, String)> {
        self.load_named("local-seen-feedback.json")
            .map(|(seen, _)| seen)
            .unwrap_or_default()
    }

    pub fn save_seen_feedback(&self, seen: &std::collections::BTreeSet<(String, String)>) {
        self.save_named("local-seen-feedback.json", seen, Utc::now());
    }

    /// One switch for every course: whether the course list shows grades.
    pub fn show_course_grades(&self) -> bool {
        self.load_named("local-show-course-grades.json")
            .map(|(on, _)| on)
            .unwrap_or(false)
    }

    pub fn save_show_course_grades(&self, on: bool) {
        self.save_named("local-show-course-grades.json", &on, Utc::now());
    }

    /// Whether the search box includes things in hidden courses. Hiding a
    /// course tidies the course list; whether it also leaves the search is a
    /// separate choice, so it is stored separately.
    pub fn search_hidden(&self) -> bool {
        self.load_named("local-search-hidden.json")
            .map(|(on, _)| on)
            .unwrap_or(false)
    }

    pub fn save_search_hidden(&self, on: bool) {
        self.save_named("local-search-hidden.json", &on, Utc::now());
    }

    /// Conversations read in canter. Canvas is never told, so this is the
    /// only record of it and it is kept apart from refreshed snapshots.
    pub fn conversation_reads(&self) -> std::collections::BTreeSet<String> {
        self.load_named("local-conversation-reads.json")
            .map(|(reads, _)| reads)
            .unwrap_or_default()
    }

    pub fn save_conversation_reads(&self, reads: &std::collections::BTreeSet<String>) {
        self.save_named("local-conversation-reads.json", reads, Utc::now());
    }

    /// Keep the saved conversation bodies within `limit` messages in total,
    /// removing the least recently read ones first. Conversation lists are
    /// capped where they are read (`crate::inbox::MAX_CONVERSATIONS`).
    pub fn prune_conversations(&self, limit: usize) {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return;
        };
        let mut saved: Vec<(DateTime<Utc>, usize, PathBuf)> = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !(name.starts_with("conversation-") && name.ends_with(".json")) {
                continue;
            }
            let path = entry.path();
            // Counted from the stored JSON so this stays independent of the type.
            let Some((data, at)) = self.load_named::<serde_json::Value>(&name) else {
                let _ = std::fs::remove_file(&path);
                continue;
            };
            let count = data
                .get("messages")
                .and_then(|m| m.as_array())
                .map_or(0, Vec::len);
            saved.push((at, count, path));
        }
        saved.sort_by_key(|s| std::cmp::Reverse(s.0));
        let mut kept = 0;
        for (_, count, path) in saved {
            if kept + count <= limit {
                kept += count;
            } else {
                let _ = std::fs::remove_file(&path);
            }
        }
    }

    /// Local course visibility does not change Canvas enrollment or its course snapshot.
    pub fn hidden_courses(&self) -> std::collections::BTreeSet<String> {
        self.load_named("local-hidden-courses.json")
            .map(|(ids, _)| ids)
            .unwrap_or_default()
    }

    pub fn save_hidden_courses(&self, ids: &std::collections::BTreeSet<String>) {
        self.save_named("local-hidden-courses.json", ids, Utc::now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("canter-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn snapshots_are_account_scoped() {
        let root = temp_root("scope");
        let a = SnapshotStore::new(&root, "100");
        let b = SnapshotStore::new(&root, "200");
        let when = Utc::now();
        a.save(&ResourceKey::Courses, &vec!["x".to_string()], when);
        let (data, at): (Vec<String>, _) = a.load(&ResourceKey::Courses).unwrap();
        assert_eq!(data, vec!["x"]);
        assert_eq!(at, when);
        assert!(b.load::<Vec<String>>(&ResourceKey::Courses).is_none());
        a.clear();
        assert!(a.load::<Vec<String>>(&ResourceKey::Courses).is_none());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_unsafe_keys() {
        assert!(
            ResourceKey::Assignments("../x".into())
                .file_name()
                .is_none()
        );
        assert_eq!(
            ResourceKey::Detail("1~2".into(), "3".into())
                .file_name()
                .unwrap(),
            "assignment-1_2-3.json"
        );
        assert!(
            ResourceKey::Conversations("../drafts".into())
                .file_name()
                .is_none()
        );
        assert_eq!(
            ResourceKey::Conversations("unread".into())
                .file_name()
                .unwrap(),
            "conversations-unread.json"
        );
        assert!(
            ResourceKey::Conversation("a/b".into())
                .file_name()
                .is_none()
        );
        assert_eq!(
            ResourceKey::Conversation("5".into()).file_name().unwrap(),
            "conversation-5.json"
        );
    }

    #[test]
    fn local_conversation_reads_are_account_scoped_and_outlive_snapshots() {
        let root = temp_root("inbox-reads");
        let store = SnapshotStore::new(&root, "100");
        assert!(store.conversation_reads().is_empty());
        let reads: std::collections::BTreeSet<String> = ["5".to_string(), "9".to_string()].into();
        store.save_conversation_reads(&reads);
        assert_eq!(store.conversation_reads(), reads);
        assert!(
            SnapshotStore::new(&root, "200")
                .conversation_reads()
                .is_empty()
        );
        store.clear();
        assert!(store.conversation_reads().is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn saved_conversations_stay_within_the_message_cap() {
        let root = temp_root("inbox-prune");
        let store = SnapshotStore::new(&root, "100");
        let body = |n: usize| serde_json::json!({"messages": vec![0; n]});
        let now = Utc::now();
        // Newest first: 5 + 4 fit in 10, the oldest does not.
        store.save(&ResourceKey::Conversation("1".into()), &body(5), now);
        store.save(
            &ResourceKey::Conversation("2".into()),
            &body(4),
            now - chrono::Duration::minutes(1),
        );
        store.save(
            &ResourceKey::Conversation("3".into()),
            &body(6),
            now - chrono::Duration::minutes(2),
        );
        store.prune_conversations(10);
        let kept = |id: &str| {
            store
                .load::<serde_json::Value>(&ResourceKey::Conversation(id.into()))
                .is_some()
        };
        assert!(kept("1") && kept("2"));
        assert!(!kept("3"));
        // A folder list is not a conversation body and is left alone.
        store.save(
            &ResourceKey::Conversations("inbox".into()),
            &vec!["x".to_string()],
            now,
        );
        store.prune_conversations(0);
        assert!(!kept("1"));
        assert!(
            store
                .load::<Vec<String>>(&ResourceKey::Conversations("inbox".into()))
                .is_some()
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
