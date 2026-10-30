//! Application state and navigation. Pure logic: input and network results
//! come in as messages; requests to fetch, open, or sign in go out as effects.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::cache::{ResourceKey, SnapshotStore};
use crate::canvas::{CanvasError, FetchStats};
use crate::html::{Link, Target};
use crate::keys::{self, Act, Scope};
use crate::library::{self, Library, LocalState};
use crate::model::{AssignmentDetail, AssignmentSummary, CanvasFile, CanvasFolder, Course};
use crate::modules::ItemKind;
use crate::submission::{self, DraftKind, FileChoice};

/// An in-flight read that started at most this long before an access may
/// satisfy that access (approved freshness rule). Older ones may not.
pub const REUSE_WINDOW: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    /// Saved copy from an earlier session, shown while syncing.
    Cache,
    /// Returned by a request made in this session.
    Network,
}

#[derive(Clone, Debug)]
pub enum SyncState {
    Idle,
    Syncing { request: u64, started: Instant },
    Failed(CanvasError),
}

#[derive(Clone, Debug)]
pub struct Resource<T> {
    pub data: Option<T>,
    pub origin: Origin,
    pub fetched_at: Option<DateTime<Utc>>,
    pub sync: SyncState,
    /// Bumped whenever `data` changes, for render caches.
    pub version: u64,
}

impl<T> Default for Resource<T> {
    fn default() -> Self {
        Resource {
            data: None,
            origin: Origin::Cache,
            fetched_at: None,
            sync: SyncState::Idle,
            version: 0,
        }
    }
}

#[derive(Debug)]
pub enum Payload {
    Courses(Vec<Course>),
    Announcements(Vec<crate::announcements::Summary>),
    Announcement(Box<crate::announcements::Detail>),
    Assignments(Vec<AssignmentSummary>),
    Detail(Box<AssignmentDetail>),
    History(crate::submission::History),
    Files(Box<CourseFilesData>),
    File(Box<CanvasFile>),
    Planner(Vec<crate::calendar::Item>),
    Colors(HashMap<String, String>),
    Tabs(Vec<crate::modules::Tab>),
    Modules(Vec<crate::modules::Module>),
    WikiPage(Box<crate::modules::WikiPage>),
    Grades(Box<crate::grades::Grades>),
    Pages(Vec<crate::modules::PageSummary>),
    Conversations(Vec<crate::inbox::Summary>),
    Conversation(Box<crate::inbox::Detail>),
}

/// One row of the Modules screen: a module's heading or one of its items.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ModuleRow<'a> {
    Module(&'a crate::modules::Module),
    Item(&'a crate::modules::Module, &'a crate::modules::Item),
}

impl ModuleRow<'_> {
    /// Headings (modules and text headers) are not selectable.
    pub fn selectable(&self) -> bool {
        matches!(self, ModuleRow::Item(_, i) if i.kind() != crate::modules::ItemKind::Heading)
    }
}

/// One row of the Grades screen: an assignment group's heading or one of its
/// assignments.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum GradeRow<'a> {
    Group(&'a crate::grades::Group),
    Assignment(&'a crate::grades::Group, &'a AssignmentSummary),
}

impl GradeRow<'_> {
    /// Group headings are not selectable, as module headings are not.
    pub fn selectable(&self) -> bool {
        matches!(self, GradeRow::Assignment(..))
    }
}

/// A course section the reader can show, when Canvas shows its tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CourseTab {
    /// The course's Canvas home, when it is a page.
    Home,
    Assignments,
    Announcements,
    Files,
    Modules,
    Grades,
    Pages,
}

impl CourseTab {
    pub fn label(self) -> &'static str {
        match self {
            CourseTab::Home => "Home",
            CourseTab::Assignments => "Assignments",
            CourseTab::Announcements => "Announcements",
            CourseTab::Files => "Files",
            CourseTab::Modules => "Modules",
            CourseTab::Grades => "Grades",
            CourseTab::Pages => "Pages",
        }
    }

    fn of_canvas(id: &str) -> Option<CourseTab> {
        match id {
            "assignments" => Some(CourseTab::Assignments),
            "announcements" => Some(CourseTab::Announcements),
            "files" => Some(CourseTab::Files),
            "modules" => Some(CourseTab::Modules),
            "grades" => Some(CourseTab::Grades),
            "pages" => Some(CourseTab::Pages),
            _ => None,
        }
    }

    pub fn view(self, course_id: String, course_name: String) -> ViewKind {
        match self {
            CourseTab::Home => ViewKind::WikiPage {
                course_id,
                course_name,
                page_url: crate::modules::FRONT_PAGE.to_string(),
                title: "Home".into(),
            },
            CourseTab::Assignments => ViewKind::Assignments {
                course_id,
                course_name,
            },
            CourseTab::Announcements => ViewKind::Announcements {
                course_id,
                course_name,
            },
            CourseTab::Files => ViewKind::Files {
                course_id,
                course_name,
                folder_id: None,
            },
            CourseTab::Modules => ViewKind::Modules {
                course_id,
                course_name,
            },
            CourseTab::Grades => ViewKind::Grades {
                course_id,
                course_name,
            },
            CourseTab::Pages => ViewKind::Pages {
                course_id,
                course_name,
            },
        }
    }
}

/// A module file waiting for its details before it can be placed.
#[derive(Clone, Debug, PartialEq)]
struct ModuleFile {
    course_id: String,
    module: String,
    open: bool,
    quiet: bool,
}

/// A course's Files area as the reader keeps it.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CourseFilesData {
    pub folders: Vec<CanvasFolder>,
    /// `None` when the instructor hides Files from students.
    pub files: Option<Vec<CanvasFile>>,
}

/// One row of the Files screen.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FileRow<'a> {
    Folder(&'a CanvasFolder),
    File(&'a CanvasFile),
}

impl FileRow<'_> {
    pub fn name(&self) -> &str {
        match self {
            FileRow::Folder(f) => f.name.as_deref().unwrap_or("folder"),
            FileRow::File(f) => f.name(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum TransferEvent {
    Progress { received: u64, total: Option<u64> },
    Done(PathBuf),
    Failed { message: String, needs_login: bool },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Transfer {
    /// Waiting for a download slot.
    Queued {
        total: Option<u64>,
    },
    Running {
        received: u64,
        total: Option<u64>,
    },
    Failed(String),
}

/// A file as the reader sees it locally.
#[derive(Clone, Debug, PartialEq)]
pub enum Local {
    Missing,
    Saved,
    /// Canvas has a newer version than the saved copy.
    Changed,
    Queued,
    Running {
        received: u64,
        total: Option<u64>,
    },
    Failed(String),
    Locked,
}

/// A request to put a file in a folder, waiting for the file's details.
#[derive(Clone, Debug, PartialEq)]
struct Intent {
    dir: PathBuf,
    open: bool,
    /// Downloaded without being asked (assignment images): removed after a week unused.
    automatic: bool,
    /// Part of a batch or automatic: no notice when it finishes.
    quiet: bool,
}

/// Where a running download will appear, and what to do when it finishes.
#[derive(Clone, Debug, PartialEq)]
struct Placement {
    target: PathBuf,
    open: bool,
    automatic: bool,
    quiet: bool,
}

#[derive(Debug)]
pub enum Msg {
    Key(KeyEvent),
    Paste(String),
    Fetched {
        key: ResourceKey,
        request: u64,
        result: Result<(Payload, FetchStats), CanvasError>,
        at: DateTime<Utc>,
    },
    Tick,
    Resize(u16, u16),
    LoginFinished {
        ok: bool,
        message: String,
    },
    Transfer {
        file_id: String,
        event: TransferEvent,
    },
    Submitted {
        course_id: String,
        assignment_id: String,
        result: Result<String, String>,
    },
    MessageSent {
        /// The conversation replied to, if it was a reply.
        conversation_id: Option<String>,
        result: Result<String, String>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    Fetch {
        key: ResourceKey,
        request: u64,
    },
    OpenUrl(String),
    Login,
    /// Download a file into the library at `target` (and install it there).
    Download {
        file: Box<CanvasFile>,
        target: PathBuf,
        automatic: bool,
    },
    /// Open a local file in its default app.
    OpenPath(PathBuf),
    /// Show a local folder in the file manager (created if missing).
    Reveal(PathBuf),
    Submit {
        course_id: String,
        assignment_id: String,
        files: Vec<FileChoice>,
    },
    /// Hand in typed text or a website address, after the review screen.
    SubmitContent {
        course_id: String,
        assignment_id: String,
        kind: DraftKind,
        content: String,
    },
    /// Send an Inbox message the user confirmed on the review screen.
    SendMessage(Box<crate::inbox::Send>),
}

#[derive(Clone, Debug)]
pub struct SubmissionDraft {
    pub input: String,
    pub start_directory: String,
    pub suggestions: submission::PathSuggestions,
    pub files: Vec<FileChoice>,
    pub reviewing: bool,
    pub busy: bool,
    /// What this draft hands in. Files, text and a URL share this screen.
    pub kind: DraftKind,
    /// The text or address that will be sent, exactly as reviewed.
    pub content: String,
    /// The path box is open to read the text from a file.
    pub from_file: bool,
    /// Where the text was read from, when it came from a file.
    pub source: Option<PathBuf>,
}

/// Who a message being written will go to.
#[derive(Clone, Debug, PartialEq)]
pub enum Audience {
    /// Everyone already in a conversation.
    Conversation {
        conversation_id: String,
        subject: String,
        who: String,
    },
    /// The teachers of one course, each of whom gets a private copy.
    Teachers {
        course_id: String,
        course_name: String,
    },
}

/// The box of a message being written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageField {
    Subject,
    Body,
}

/// A message being written. Nothing leaves the reader until the review
/// screen is confirmed with the key that says so.
#[derive(Clone, Debug)]
pub struct MessageDraft {
    pub to: Audience,
    pub subject: String,
    pub body: String,
    pub field: MessageField,
    pub reviewing: bool,
    pub busy: bool,
}

impl MessageDraft {
    /// The boxes this message has: a reply keeps the conversation's subject.
    pub fn fields(&self) -> &'static [MessageField] {
        match self.to {
            Audience::Conversation { .. } => &[MessageField::Body],
            Audience::Teachers { .. } => &[MessageField::Subject, MessageField::Body],
        }
    }

    /// Exactly what confirming will send.
    pub fn send(&self) -> crate::inbox::Send {
        match &self.to {
            Audience::Conversation {
                conversation_id, ..
            } => crate::inbox::Send::Reply {
                conversation_id: conversation_id.clone(),
                body: self.body.clone(),
            },
            Audience::Teachers { course_id, .. } => crate::inbox::Send::ToTeachers {
                course_id: course_id.clone(),
                subject: self.subject.clone(),
                body: self.body.clone(),
            },
        }
    }

    fn text_mut(&mut self) -> &mut String {
        match self.field {
            MessageField::Subject => &mut self.subject,
            MessageField::Body => &mut self.body,
        }
    }
}

/// What has focus on an assignment or announcement page.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Section {
    #[default]
    Text,
    Links,
    Submissions,
    /// The instructor's feedback on the selected attempt.
    Feedback,
    /// The month grid on the course list (a day is selected).
    Calendar,
    /// The selected day's entries, listed under the grid.
    Day,
    /// The Inbox, beside the calendar on the home screen.
    Inbox,
}

/// One selectable entry of an assignment's feedback section.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeedbackEntry {
    /// An instructor comment on the selected attempt.
    Comment(usize),
    /// One scored criterion of the assignment's rubric.
    Criterion(usize),
}

/// The most results the palette shows at once. More than a screenful is
/// noise: the ranking is supposed to put the answer near the top.
pub const PALETTE_RESULTS: usize = 40;

/// How many warm-up reads may be in flight. The executor runs four requests
/// at a time, so this leaves half the capacity for the screen in front of the
/// user.
const WARM_AT_ONCE: usize = 2;

/// The search palette: one box that finds anything the reader knows about.
#[derive(Clone, Debug, Default)]
pub struct PaletteState {
    pub query: String,
    pub selected: usize,
}

/// A score being tried on one assignment of the Grades tab.
///
/// The guess lives in `App::whatif` once it is entered; this is only the box
/// while it is being typed.
#[derive(Clone, Debug, Default)]
pub struct WhatIfInput {
    pub course_id: String,
    pub assignment_id: String,
    /// The assignment's name, for the box to say what is being guessed at.
    pub title: String,
    pub points: Option<f64>,
    pub input: String,
}

/// The actions menu (`Space`).
#[derive(Clone, Debug, Default)]
pub struct MenuState {
    pub selected: usize,
    /// The search text, while searching.
    pub search: Option<String>,
}

/// A screen showing one Canvas item with text, files and links.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Page {
    Assignment(String, String),
    Announcement(String, String),
    /// A course wiki page: course id and the page's URL name.
    Wiki(String, String),
    /// An Inbox conversation, by its id.
    Conversation(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum ViewKind {
    Courses,
    Announcements {
        course_id: String,
        course_name: String,
    },
    Announcement {
        course_id: String,
        course_name: String,
        announcement_id: String,
        title: String,
    },
    Assignments {
        course_id: String,
        course_name: String,
    },
    Detail {
        course_id: String,
        course_name: String,
        assignment_id: String,
        title: String,
    },
    /// A folder in a course's Files area (`None` = its root).
    Files {
        course_id: String,
        course_name: String,
        folder_id: Option<String>,
    },
    Modules {
        course_id: String,
        course_name: String,
    },
    Grades {
        course_id: String,
        course_name: String,
    },
    /// Every page of a course, as a list.
    Pages {
        course_id: String,
        course_name: String,
    },
    WikiPage {
        course_id: String,
        course_name: String,
        page_url: String,
        title: String,
    },
    /// A course where Canvas shows none of the sections the reader has.
    CourseHome {
        course_id: String,
        course_name: String,
    },
    /// Every conversation in one folder, opened from the home screen's
    /// widget. The folder lives here because a view has to be able to say
    /// which resource it shows; `App::inbox_folder` remembers which one to
    /// open next time.
    Inbox { folder: crate::inbox::Folder },
    /// One Inbox conversation, opened from the Inbox.
    Conversation {
        conversation_id: String,
        subject: String,
    },
}

impl ViewKind {
    pub fn page(&self) -> Option<Page> {
        match self {
            ViewKind::Detail {
                course_id,
                assignment_id,
                ..
            } => Some(Page::Assignment(course_id.clone(), assignment_id.clone())),
            ViewKind::Announcement {
                course_id,
                announcement_id,
                ..
            } => Some(Page::Announcement(
                course_id.clone(),
                announcement_id.clone(),
            )),
            ViewKind::WikiPage {
                course_id,
                page_url,
                ..
            } => Some(Page::Wiki(course_id.clone(), page_url.clone())),
            ViewKind::Conversation {
                conversation_id, ..
            } => Some(Page::Conversation(conversation_id.clone())),
            _ => None,
        }
    }

    /// The course section this view is, with the course.
    pub fn tab(&self) -> Option<(CourseTab, &str, &str)> {
        match self {
            ViewKind::Assignments {
                course_id,
                course_name,
            } => Some((CourseTab::Assignments, course_id, course_name)),
            ViewKind::Announcements {
                course_id,
                course_name,
            } => Some((CourseTab::Announcements, course_id, course_name)),
            ViewKind::Files {
                course_id,
                course_name,
                ..
            } => Some((CourseTab::Files, course_id, course_name)),
            ViewKind::Modules {
                course_id,
                course_name,
            } => Some((CourseTab::Modules, course_id, course_name)),
            ViewKind::Grades {
                course_id,
                course_name,
            } => Some((CourseTab::Grades, course_id, course_name)),
            ViewKind::Pages {
                course_id,
                course_name,
            } => Some((CourseTab::Pages, course_id, course_name)),
            ViewKind::WikiPage {
                course_id,
                course_name,
                page_url,
                ..
            } if page_url == crate::modules::FRONT_PAGE => {
                Some((CourseTab::Home, course_id, course_name))
            }
            _ => None,
        }
    }

    /// The course of a course section or the empty course screen.
    fn course(&self) -> Option<(&str, &str)> {
        match self {
            ViewKind::CourseHome {
                course_id,
                course_name,
            } => Some((course_id, course_name)),
            _ => self.tab().map(|(_, c, n)| (c, n)),
        }
    }
}

#[derive(Clone, Debug)]
pub struct View {
    pub kind: ViewKind,
    pub selected: usize,
    pub offset: usize,
    pub scroll: usize,
    /// Focused section on a page.
    pub focus: Section,
    /// Selected entry in a page's files & links.
    pub link: usize,
    /// Selected attempt in an assignment's submissions (0 = latest).
    pub attempt: usize,
    /// Selected entry in the feedback for that attempt.
    pub feedback: usize,
    /// Bring the focused section into view on the next draw.
    pub reveal: bool,
}

impl View {
    pub fn new(kind: ViewKind) -> Self {
        View {
            kind,
            selected: 0,
            offset: 0,
            scroll: 0,
            focus: Section::Text,
            link: 0,
            attempt: 0,
            feedback: 0,
            reveal: false,
        }
    }

    pub fn key(&self) -> ResourceKey {
        match &self.kind {
            ViewKind::Courses => ResourceKey::Courses,
            ViewKind::Announcements { course_id, .. } => {
                ResourceKey::Announcements(course_id.clone())
            }
            ViewKind::Announcement {
                course_id,
                announcement_id,
                ..
            } => ResourceKey::Announcement(course_id.clone(), announcement_id.clone()),
            ViewKind::Assignments { course_id, .. } => ResourceKey::Assignments(course_id.clone()),
            ViewKind::Detail {
                course_id,
                assignment_id,
                ..
            } => ResourceKey::Detail(course_id.clone(), assignment_id.clone()),
            ViewKind::Files { course_id, .. } => ResourceKey::Files(course_id.clone()),
            ViewKind::Modules { course_id, .. } => ResourceKey::Modules(course_id.clone()),
            ViewKind::Grades { course_id, .. } => ResourceKey::Grades(course_id.clone()),
            ViewKind::Pages { course_id, .. } => ResourceKey::Pages(course_id.clone()),
            ViewKind::WikiPage {
                course_id,
                page_url,
                ..
            } => ResourceKey::WikiPage(course_id.clone(), page_url.clone()),
            ViewKind::CourseHome { course_id, .. } => ResourceKey::Tabs(course_id.clone()),
            ViewKind::Conversation {
                conversation_id, ..
            } => ResourceKey::Conversation(conversation_id.clone()),
            ViewKind::Inbox { folder } => ResourceKey::Conversations(folder.id().to_string()),
        }
    }
}

/// Wrapped description lines cached per assignment version and width. Inline
/// images are placed in the same pass, so a box's rows are ordinary lines as
/// far as the page's scrolling is concerned.
pub struct DetailRender {
    pub key: (String, String),
    pub version: u64,
    pub width: u16,
    pub body: crate::images::Body,
}

pub struct HistoryRender {
    pub key: (String, String),
    pub version: u64,
    pub selected: usize,
    pub width: u16,
    pub lines: Vec<String>,
}

pub struct App {
    pub origin: String,
    store: Option<Arc<SnapshotStore>>,
    pub courses: Resource<Vec<Course>>,
    /// The home-screen calendar, one read per month shown (`2026-10`).
    pub planner: HashMap<String, Resource<Vec<crate::calendar::Item>>>,
    /// Course colours chosen in Canvas, by course id.
    pub colors: Resource<HashMap<String, String>>,
    /// The day selected on the calendar; its month is the one shown.
    pub calendar_day: chrono::NaiveDate,
    /// The selected entry of that day.
    pub day_item: usize,
    pub announcements: HashMap<String, Resource<Vec<crate::announcements::Summary>>>,
    pub announcement_details: HashMap<(String, String), Resource<crate::announcements::Detail>>,
    pub announcement_render: Option<DetailRender>,
    announcement_reads: std::collections::BTreeSet<(String, String)>,
    pub hidden_courses: std::collections::BTreeSet<String>,
    /// Hidden courses are listed (marked) so they can be restored.
    pub show_hidden: bool,
    pub assignments: HashMap<String, Resource<Vec<AssignmentSummary>>>,
    pub details: HashMap<(String, String), Resource<AssignmentDetail>>,
    pub histories: HashMap<(String, String), Resource<submission::History>>,
    pub history_render: Option<HistoryRender>,
    pub stack: Vec<View>,
    next_request: u64,
    pub size: (u16, u16),
    pub notice: Option<String>,
    pub login_required: bool,
    pub last_stats: Option<(&'static str, FetchStats)>,
    pub quit: bool,
    pub dirty: bool,
    pub spinner: usize,
    pub detail_render: Option<DetailRender>,
    /// Request ids whose responses were applied since the runtime last drained this (for timing).
    pub applied: Vec<u64>,
    /// Start sign-in automatically if the first read at startup finds the session expired.
    auto_login: bool,
    pub course_files: HashMap<String, Resource<CourseFilesData>>,
    /// File metadata, read when an assignment that links to the file is opened.
    pub files: HashMap<String, Resource<CanvasFile>>,
    pub transfers: HashMap<String, Transfer>,
    /// Files waiting for their details before they can be placed.
    waiting: HashMap<String, Vec<Intent>>,
    /// Running downloads; the first placement is where the file is being installed.
    pending: HashMap<String, Vec<Placement>>,
    pub library: Option<Arc<Library>>,
    pub submission: Option<SubmissionDraft>,
    pub menu: Option<MenuState>,
    /// The search palette, while it is open.
    pub palette: Option<PaletteState>,
    /// Scores the student is trying out, by course and then assignment.
    ///
    /// These are the student's own question, not Canvas's answer: nothing is
    /// sent to Canvas and nothing is written to disk, so they last as long as
    /// the program runs and no longer. Canvas's own what-if scores behave the
    /// same way, and a guess that outlived the run would be indistinguishable
    /// from a real grade a week later.
    pub whatif: HashMap<String, crate::grades::WhatIf>,
    /// The score being typed, while the box is open.
    pub whatif_input: Option<WhatIfInput>,
    /// Lists the palette still wants, read a couple at a time in the
    /// background so that searching can find a course never opened.
    pub warm_queue: std::collections::VecDeque<ResourceKey>,
    /// Warm-up reads in flight, and which keys they are.
    warm_running: usize,
    warm_keys: Vec<ResourceKey>,
    /// The warm-up is queued once per run.
    warmed: bool,
    /// Whether things in hidden courses are searchable.
    pub search_hidden: bool,
    /// Whether to warm the palette's index at all.
    ///
    /// Off unless the reader is being driven by a person: it is the one place
    /// requests are made that nobody asked for, so the headless walk-through,
    /// the benchmark and the tests leave it alone and say what they fetch.
    pub warm_up: bool,
    /// The help screen, with its scroll position.
    pub help: Option<usize>,
    /// Course tabs Canvas shows; read once per run (and on `r`).
    pub tabs: HashMap<String, Resource<Vec<crate::modules::Tab>>>,
    tabs_read: std::collections::HashSet<String>,
    pub modules: HashMap<String, Resource<Vec<crate::modules::Module>>>,
    pub grades: HashMap<String, Resource<crate::grades::Grades>>,
    pub page_lists: HashMap<String, Resource<Vec<crate::modules::PageSummary>>>,
    /// Instructor comments already seen here, by course and assignment. Local
    /// only: Canvas is never told what has been read.
    seen_feedback: std::collections::BTreeSet<(String, String)>,
    /// Show each course's current grade on the course list (one switch for all).
    pub show_course_grades: bool,
    pub wiki_pages: HashMap<(String, String), Resource<crate::modules::WikiPage>>,
    pub wiki_render: Option<DetailRender>,
    /// Module files waiting for their details (and their course's folders).
    module_waiting: HashMap<String, ModuleFile>,
    /// Inbox conversation lists, by folder name.
    pub conversations: HashMap<String, Resource<Vec<crate::inbox::Summary>>>,
    pub conversation_details: HashMap<String, Resource<crate::inbox::Detail>>,
    pub conversation_render: Option<DetailRender>,
    /// Conversations read here. Canvas is never told; see `src/inbox.rs`.
    conversation_reads: std::collections::BTreeSet<String>,
    /// The Inbox folder shown on the home screen.
    pub inbox_folder: crate::inbox::Folder,
    /// The selected conversation in that folder.
    pub inbox_item: usize,
    /// A message being written, if any.
    pub message: Option<MessageDraft>,
}

/// `91.5%`, `88%`: a Canvas score, without a trailing `.0`.
pub fn percent(score: f64) -> String {
    if (score - score.round()).abs() < 0.05 {
        format!("{}%", score.round())
    } else {
        format!("{score:.1}%")
    }
}

/// `9 of 10`, `9.5 of 10`: points as Canvas shows them.
pub fn points(score: f64) -> String {
    if (score - score.round()).abs() < 0.005 {
        format!("{}", score.round())
    } else {
        format!("{score:.2}")
    }
}

/// `1.2 MB`, `340 KB`, `12 B` (1 KB = 1024 B).
pub fn human_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    let b = bytes as f64;
    if b >= KB * KB * KB {
        format!("{:.1} GB", b / (KB * KB * KB))
    } else if b >= KB * KB {
        format!("{:.1} MB", b / (KB * KB))
    } else if b >= KB {
        format!("{:.0} KB", b / KB)
    } else {
        format!("{bytes} B")
    }
}

/// Percent-encode one URL path segment.
fn percent_encode(segment: &str) -> String {
    let mut out = String::new();
    for b in segment.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

impl App {
    pub fn new(origin: &str, store: Option<Arc<SnapshotStore>>) -> Self {
        let announcement_reads = store
            .as_ref()
            .map(|s| s.announcement_reads())
            .unwrap_or_default();
        let hidden_courses = store
            .as_ref()
            .map(|s| s.hidden_courses())
            .unwrap_or_default();
        let seen_feedback = store
            .as_ref()
            .map(|s| s.seen_feedback())
            .unwrap_or_default();
        let show_course_grades = store.as_ref().is_some_and(|s| s.show_course_grades());
        let search_hidden = store.as_ref().is_some_and(|s| s.search_hidden());
        let conversation_reads = store
            .as_ref()
            .map(|s| s.conversation_reads())
            .unwrap_or_default();
        App {
            origin: origin.to_string(),
            store,
            courses: Resource::default(),
            planner: HashMap::new(),
            colors: Resource::default(),
            calendar_day: chrono::Local::now().date_naive(),
            day_item: 0,
            announcements: HashMap::new(),
            announcement_details: HashMap::new(),
            announcement_render: None,
            announcement_reads,
            hidden_courses,
            show_hidden: false,
            assignments: HashMap::new(),
            details: HashMap::new(),
            histories: HashMap::new(),
            history_render: None,
            stack: vec![View::new(ViewKind::Courses)],
            next_request: 0,
            size: (80, 24),
            notice: None,
            login_required: false,
            last_stats: None,
            quit: false,
            dirty: true,
            spinner: 0,
            detail_render: None,
            applied: Vec::new(),
            auto_login: false,
            course_files: HashMap::new(),
            files: HashMap::new(),
            transfers: HashMap::new(),
            waiting: HashMap::new(),
            pending: HashMap::new(),
            library: None,
            submission: None,
            menu: None,
            palette: None,
            whatif: HashMap::new(),
            whatif_input: None,
            warm_queue: std::collections::VecDeque::new(),
            warm_running: 0,
            warm_keys: Vec::new(),
            warmed: false,
            search_hidden,
            warm_up: false,
            help: None,
            tabs: HashMap::new(),
            tabs_read: std::collections::HashSet::new(),
            modules: HashMap::new(),
            grades: HashMap::new(),
            page_lists: HashMap::new(),
            seen_feedback,
            show_course_grades,
            wiki_pages: HashMap::new(),
            wiki_render: None,
            module_waiting: HashMap::new(),
            conversations: HashMap::new(),
            conversation_details: HashMap::new(),
            conversation_render: None,
            conversation_reads,
            inbox_folder: crate::inbox::Folder::default(),
            inbox_item: 0,
            message: None,
        }
    }

    /// Enter the initial view.
    pub fn start(&mut self) -> Vec<Effect> {
        self.auto_login = true;
        self.enter_current(false)
    }

    pub fn view(&self) -> &View {
        self.stack.last().expect("view stack is never empty")
    }

    fn view_mut(&mut self) -> &mut View {
        self.stack.last_mut().expect("view stack is never empty")
    }

    fn sync_of(&self, key: &ResourceKey) -> Option<&SyncState> {
        match key {
            ResourceKey::Courses => Some(&self.courses.sync),
            ResourceKey::Planner(m) => self.planner.get(m).map(|r| &r.sync),
            ResourceKey::Colors => Some(&self.colors.sync),
            ResourceKey::Announcements(c) => self.announcements.get(c).map(|r| &r.sync),
            ResourceKey::Assignments(c) => self.assignments.get(c).map(|r| &r.sync),
            ResourceKey::Announcement(c, a) => self
                .announcement_details
                .get(&(c.clone(), a.clone()))
                .map(|r| &r.sync),
            ResourceKey::Detail(c, a) => self.details.get(&(c.clone(), a.clone())).map(|r| &r.sync),
            ResourceKey::History(c, a) => {
                self.histories.get(&(c.clone(), a.clone())).map(|r| &r.sync)
            }
            ResourceKey::Files(c) => self.course_files.get(c).map(|r| &r.sync),
            ResourceKey::File(f) => self.files.get(f).map(|r| &r.sync),
            ResourceKey::Tabs(c) => self.tabs.get(c).map(|r| &r.sync),
            ResourceKey::Modules(c) => self.modules.get(c).map(|r| &r.sync),
            ResourceKey::Grades(c) => self.grades.get(c).map(|r| &r.sync),
            ResourceKey::Pages(c) => self.page_lists.get(c).map(|r| &r.sync),
            ResourceKey::WikiPage(c, u) => self
                .wiki_pages
                .get(&(c.clone(), u.clone()))
                .map(|r| &r.sync),
            ResourceKey::Conversations(f) => self.conversations.get(f).map(|r| &r.sync),
            ResourceKey::Conversation(c) => self.conversation_details.get(c).map(|r| &r.sync),
        }
    }

    fn set_sync(&mut self, key: &ResourceKey, sync: SyncState) {
        match key {
            ResourceKey::Announcements(c) => {
                self.announcements.entry(c.clone()).or_default().sync = sync
            }
            ResourceKey::Announcement(c, a) => {
                self.announcement_details
                    .entry((c.clone(), a.clone()))
                    .or_default()
                    .sync = sync
            }

            ResourceKey::Courses => self.courses.sync = sync,
            ResourceKey::Planner(m) => self.planner.entry(m.clone()).or_default().sync = sync,
            ResourceKey::Colors => self.colors.sync = sync,
            ResourceKey::Assignments(c) => {
                self.assignments.entry(c.clone()).or_default().sync = sync
            }
            ResourceKey::Detail(c, a) => {
                self.details.entry((c.clone(), a.clone())).or_default().sync = sync
            }
            ResourceKey::History(c, a) => {
                self.histories
                    .entry((c.clone(), a.clone()))
                    .or_default()
                    .sync = sync
            }
            ResourceKey::Files(c) => self.course_files.entry(c.clone()).or_default().sync = sync,
            ResourceKey::File(f) => self.files.entry(f.clone()).or_default().sync = sync,
            ResourceKey::Tabs(c) => self.tabs.entry(c.clone()).or_default().sync = sync,
            ResourceKey::Modules(c) => self.modules.entry(c.clone()).or_default().sync = sync,
            ResourceKey::Grades(c) => self.grades.entry(c.clone()).or_default().sync = sync,
            ResourceKey::Pages(c) => self.page_lists.entry(c.clone()).or_default().sync = sync,
            ResourceKey::WikiPage(c, u) => {
                self.wiki_pages
                    .entry((c.clone(), u.clone()))
                    .or_default()
                    .sync = sync
            }
            ResourceKey::Conversations(f) => {
                self.conversations.entry(f.clone()).or_default().sync = sync
            }
            ResourceKey::Conversation(c) => {
                self.conversation_details.entry(c.clone()).or_default().sync = sync
            }
        }
    }

    pub fn current_syncing(&self) -> bool {
        matches!(
            self.sync_of(&self.view().key()),
            Some(SyncState::Syncing { .. })
        )
    }

    /// Show a saved copy (if any and nothing newer is loaded), then request fresh data.
    fn enter_current(&mut self, explicit: bool) -> Vec<Effect> {
        let key = self.view().key();
        self.load_snapshot(&key);
        self.clamp_selection();
        self.mark_current_announcement_read();
        self.mark_current_conversation_read();
        self.dirty = true;
        // Course tabs are read once per run, not on every visit.
        let mut effects: Vec<Effect> = match key {
            ResourceKey::Tabs(_) => vec![],
            key => self.request(key, explicit).into_iter().collect(),
        };
        if let Some((course_id, _)) = self.view().kind.course() {
            let course_id = course_id.to_string();
            effects.extend(self.read_tabs(&course_id, explicit));
        }
        // The calendar and the Inbox are part of the home screen: read them
        // with the course list.
        if self.view().kind == ViewKind::Courses {
            effects.extend(self.read_calendar(explicit));
            // The widget shows a count, so only the Unread folder is needed.
            // The folder lists themselves are read by the Inbox screen.
            effects.extend(self.read_unread());
        }
        // Past submissions are part of the assignment page: read them with it
        // when the copy on hand shows something was submitted.
        if let ViewKind::Detail {
            course_id,
            assignment_id,
            ..
        } = self.view().kind.clone()
            && self.was_submitted(&course_id, &assignment_id)
        {
            effects.extend(self.request(ResourceKey::History(course_id, assignment_id), explicit));
        }
        effects
    }

    /// Whether Canvas shows any submission for this assignment.
    fn was_submitted(&self, course_id: &str, assignment_id: &str) -> bool {
        self.details
            .get(&(course_id.to_string(), assignment_id.to_string()))
            .and_then(|r| r.data.as_ref())
            .and_then(|d| d.base.submission.as_ref())
            .is_some_and(|s| s.submitted_at.is_some() || s.attempt.unwrap_or(0) > 0)
    }

    pub fn attempt_count(&self, course_id: &str, assignment_id: &str) -> usize {
        self.histories
            .get(&(course_id.to_string(), assignment_id.to_string()))
            .and_then(|r| r.data.as_ref())
            .map_or(0, |h| h.attempts.len())
    }

    fn load_snapshot(&mut self, key: &ResourceKey) {
        let Some(store) = self.store.clone() else {
            return;
        };
        match key {
            ResourceKey::Courses if self.courses.data.is_none() => {
                if let Some((data, at)) = store.load(key) {
                    self.courses.data = Some(data);
                    self.courses.fetched_at = Some(at);
                    self.courses.origin = Origin::Cache;
                    self.courses.version += 1;
                }
            }
            ResourceKey::Planner(m) => {
                let res = self.planner.entry(m.clone()).or_default();
                if res.data.is_none()
                    && let Some((data, at)) = store.load(key)
                {
                    res.data = Some(data);
                    res.fetched_at = Some(at);
                    res.origin = Origin::Cache;
                    res.version += 1;
                }
            }
            ResourceKey::Colors if self.colors.data.is_none() => {
                if let Some((data, at)) = store.load(key) {
                    self.colors.data = Some(data);
                    self.colors.fetched_at = Some(at);
                    self.colors.origin = Origin::Cache;
                    self.colors.version += 1;
                }
            }
            ResourceKey::Colors => {}
            ResourceKey::Announcements(c) => {
                let res = self.announcements.entry(c.clone()).or_default();
                if res.data.is_none()
                    && let Some((data, at)) = store.load(key)
                {
                    res.data = Some(data);
                    res.fetched_at = Some(at);
                    res.origin = Origin::Cache;
                    res.version += 1;
                }
            }
            ResourceKey::Assignments(c) => {
                let res = self.assignments.entry(c.clone()).or_default();
                if res.data.is_none()
                    && let Some((data, at)) = store.load(key)
                {
                    res.data = Some(data);
                    res.fetched_at = Some(at);
                    res.origin = Origin::Cache;
                    res.version += 1;
                }
            }
            ResourceKey::Announcement(c, a) => {
                let res = self
                    .announcement_details
                    .entry((c.clone(), a.clone()))
                    .or_default();
                if res.data.is_none()
                    && let Some((data, at)) = store.load(key)
                {
                    res.data = Some(data);
                    res.fetched_at = Some(at);
                    res.origin = Origin::Cache;
                    res.version += 1;
                }
            }
            ResourceKey::Detail(c, a) => {
                let res = self.details.entry((c.clone(), a.clone())).or_default();
                if res.data.is_none()
                    && let Some((data, at)) = store.load(key)
                {
                    res.data = Some(data);
                    res.fetched_at = Some(at);
                    res.origin = Origin::Cache;
                    res.version += 1;
                }
            }
            ResourceKey::Files(c) => {
                let res = self.course_files.entry(c.clone()).or_default();
                if res.data.is_none()
                    && let Some((data, at)) = store.load(key)
                {
                    res.data = Some(data);
                    res.fetched_at = Some(at);
                    res.origin = Origin::Cache;
                    res.version += 1;
                }
            }
            ResourceKey::File(f) => {
                let res = self.files.entry(f.clone()).or_default();
                if res.data.is_none()
                    && let Some((data, at)) = store.load(key)
                {
                    res.data = Some(data);
                    res.fetched_at = Some(at);
                    res.origin = Origin::Cache;
                    res.version += 1;
                }
            }
            ResourceKey::Tabs(c) => {
                let res = self.tabs.entry(c.clone()).or_default();
                if res.data.is_none()
                    && let Some((data, at)) = store.load(key)
                {
                    res.data = Some(data);
                    res.fetched_at = Some(at);
                    res.origin = Origin::Cache;
                    res.version += 1;
                }
            }
            ResourceKey::Modules(c) => {
                let res = self.modules.entry(c.clone()).or_default();
                if res.data.is_none()
                    && let Some((data, at)) = store.load(key)
                {
                    res.data = Some(data);
                    res.fetched_at = Some(at);
                    res.origin = Origin::Cache;
                    res.version += 1;
                }
            }
            ResourceKey::Pages(c) => {
                let res = self.page_lists.entry(c.clone()).or_default();
                if res.data.is_none()
                    && let Some((data, at)) = store.load(key)
                {
                    res.data = Some(data);
                    res.fetched_at = Some(at);
                    res.origin = Origin::Cache;
                    res.version += 1;
                }
            }
            ResourceKey::Grades(c) => {
                let res = self.grades.entry(c.clone()).or_default();
                if res.data.is_none()
                    && let Some((data, at)) = store.load(key)
                {
                    res.data = Some(data);
                    res.fetched_at = Some(at);
                    res.origin = Origin::Cache;
                    res.version += 1;
                }
            }
            ResourceKey::WikiPage(c, u) => {
                let res = self.wiki_pages.entry((c.clone(), u.clone())).or_default();
                if res.data.is_none()
                    && let Some((data, at)) = store.load(key)
                {
                    res.data = Some(data);
                    res.fetched_at = Some(at);
                    res.origin = Origin::Cache;
                    res.version += 1;
                }
            }
            ResourceKey::Conversations(f) => {
                let res = self.conversations.entry(f.clone()).or_default();
                if res.data.is_none()
                    && let Some((data, at)) = store.load(key)
                {
                    res.data = Some(data);
                    res.fetched_at = Some(at);
                    res.origin = Origin::Cache;
                    res.version += 1;
                }
            }
            ResourceKey::Conversation(c) => {
                let res = self.conversation_details.entry(c.clone()).or_default();
                if res.data.is_none()
                    && let Some((data, at)) = store.load(key)
                {
                    res.data = Some(data);
                    res.fetched_at = Some(at);
                    res.origin = Origin::Cache;
                    res.version += 1;
                }
            }
            ResourceKey::History(..) => {}
            ResourceKey::Courses => {}
        }
    }

    /// Every access needs a fresh read. A qualifying in-flight read (started
    /// within the reuse window) satisfies a non-explicit access.
    fn request(&mut self, key: ResourceKey, explicit: bool) -> Option<Effect> {
        if !explicit
            && let Some(SyncState::Syncing { started, .. }) = self.sync_of(&key)
            && started.elapsed() <= REUSE_WINDOW
        {
            return None;
        }
        self.next_request += 1;
        let request = self.next_request;
        self.set_sync(
            &key,
            SyncState::Syncing {
                request,
                started: Instant::now(),
            },
        );
        Some(Effect::Fetch { key, request })
    }

    pub fn update(&mut self, msg: Msg) -> Vec<Effect> {
        match msg {
            Msg::Key(key) => {
                self.dirty = true;
                self.notice = None;
                self.on_key(key)
            }
            Msg::Paste(value) => {
                if let Some(draft) = &mut self.submission
                    && !draft.reviewing
                    && !draft.busy
                {
                    let value = value.trim_end_matches(['\r', '\n']);
                    if value.contains(['\r', '\n']) {
                        self.notice = Some("Paste one file path at a time.".into());
                    } else {
                        let replace = draft.input == draft.start_directory
                            && Path::new(value.trim_matches('"')).is_absolute();
                        let size = value.chars().count()
                            + if replace {
                                0
                            } else {
                                draft.input.chars().count()
                            };
                        if size > 1024 {
                            return vec![];
                        }
                        if replace {
                            draft.input = value.to_string();
                        } else {
                            draft.input.push_str(value);
                        }
                        draft.suggestions.update(&draft.input);
                        self.dirty = true;
                    }
                }
                vec![]
            }
            Msg::Fetched {
                key,
                request,
                result,
                at,
            } => self.on_fetched(key, request, result, at),
            Msg::Tick => {
                if self.current_syncing() {
                    self.spinner = self.spinner.wrapping_add(1);
                    self.dirty = true;
                }
                vec![]
            }
            Msg::Resize(w, h) => {
                self.size = (w, h);
                self.dirty = true;
                vec![]
            }
            Msg::LoginFinished { ok, message } => {
                self.dirty = true;
                self.notice = Some(message);
                if ok {
                    self.login_required = false;
                    self.enter_current(true)
                } else {
                    vec![]
                }
            }
            Msg::Transfer { file_id, event } => {
                self.dirty = true;
                self.on_transfer(file_id, event)
            }
            Msg::Submitted {
                course_id,
                assignment_id,
                result,
            } => {
                self.submission = None;
                self.notice = Some(result.unwrap_or_else(|e| e));
                self.dirty = true;
                let mut effects = Vec::new();
                if let Some(e) =
                    self.request(ResourceKey::Detail(course_id.clone(), assignment_id), true)
                {
                    effects.push(e);
                }
                if let Some(e) = self.request(ResourceKey::Assignments(course_id), true) {
                    effects.push(e);
                }
                effects
            }
            Msg::MessageSent {
                conversation_id,
                result,
            } => {
                self.message = None;
                self.notice = Some(result.clone().unwrap_or_else(|e| e));
                self.dirty = true;
                if result.is_err() {
                    return vec![];
                }
                // Show what Canvas now has: the thread, and the folder lists.
                let mut effects = Vec::new();
                if let Some(id) = conversation_id {
                    effects.extend(self.request(ResourceKey::Conversation(id), true));
                }
                effects.extend(self.read_inbox(true));
                effects
            }
        }
    }

    fn on_fetched(
        &mut self,
        key: ResourceKey,
        request: u64,
        result: Result<(Payload, FetchStats), CanvasError>,
        at: DateTime<Utc>,
    ) -> Vec<Effect> {
        // Only the most recent request for a resource may update it.
        let current = matches!(self.sync_of(&key), Some(SyncState::Syncing { request: r, .. }) if *r == request);
        if !current {
            return vec![];
        }
        self.dirty = true;
        let first_result = std::mem::take(&mut self.auto_login);
        match result {
            Ok((payload, stats)) => {
                self.login_required = false;
                self.applied.push(request);
                if key == self.view().key() {
                    self.last_stats = Some((key.label(), stats));
                }
                let mut effects = Vec::new();
                match (payload, &key) {
                    (Payload::Planner(items), ResourceKey::Planner(m)) => {
                        let r = self.planner.entry(m.clone()).or_default();
                        r.data = Some(items);
                        r.origin = Origin::Network;
                        r.fetched_at = Some(at);
                        r.sync = SyncState::Idle;
                        r.version += 1;
                        let n = self.day_items(self.calendar_day).len();
                        self.day_item = self.day_item.min(n.saturating_sub(1));
                    }
                    (Payload::Colors(colors), ResourceKey::Colors) => {
                        let r = &mut self.colors;
                        r.data = Some(colors);
                        r.origin = Origin::Network;
                        r.fetched_at = Some(at);
                        r.sync = SyncState::Idle;
                        r.version += 1;
                    }
                    (Payload::Courses(list), ResourceKey::Courses) => {
                        let r = &mut self.courses;
                        r.data = Some(list);
                        r.origin = Origin::Network;
                        r.fetched_at = Some(at);
                        r.sync = SyncState::Idle;
                        r.version += 1;
                        self.clamp_selection();
                        effects.extend(self.read_course_grades());
                        effects.extend(self.start_warm_up());
                    }
                    (Payload::Announcements(list), ResourceKey::Announcements(c)) => {
                        let r = self.announcements.entry(c.clone()).or_default();
                        r.data = Some(list);
                        r.origin = Origin::Network;
                        r.fetched_at = Some(at);
                        r.sync = SyncState::Idle;
                        r.version += 1;
                    }
                    (Payload::Assignments(list), ResourceKey::Assignments(c)) => {
                        let r = self.assignments.entry(c.clone()).or_default();
                        r.data = Some(list);
                        r.origin = Origin::Network;
                        r.fetched_at = Some(at);
                        r.sync = SyncState::Idle;
                        r.version += 1;
                    }
                    (Payload::Announcement(detail), ResourceKey::Announcement(c, a)) => {
                        let r = self
                            .announcement_details
                            .entry((c.clone(), a.clone()))
                            .or_default();
                        r.data = Some(*detail);
                        r.origin = Origin::Network;
                        r.fetched_at = Some(at);
                        r.sync = SyncState::Idle;
                        r.version += 1;
                    }
                    (Payload::Detail(detail), ResourceKey::Detail(c, a)) => {
                        let r = self.details.entry((c.clone(), a.clone())).or_default();
                        r.data = Some(*detail);
                        r.origin = Origin::Network;
                        r.fetched_at = Some(at);
                        r.sync = SyncState::Idle;
                        r.version += 1;
                        effects.extend(self.after_detail(c, a));
                        if self.was_submitted(c, a) {
                            effects.extend(
                                self.request(ResourceKey::History(c.clone(), a.clone()), false),
                            );
                        }
                    }
                    (Payload::History(history), ResourceKey::History(c, a)) => {
                        let r = self.histories.entry((c.clone(), a.clone())).or_default();
                        r.data = Some(history);
                        r.origin = Origin::Network;
                        r.fetched_at = Some(at);
                        r.sync = SyncState::Idle;
                        r.version += 1;
                        let count = r.data.as_ref().map_or(0, |h| h.attempts.len());
                        for view in &mut self.stack {
                            if view.kind.page() == Some(Page::Assignment(c.clone(), a.clone())) {
                                view.attempt = view.attempt.min(count.saturating_sub(1));
                            }
                        }
                    }
                    (Payload::Files(data), ResourceKey::Files(c)) => {
                        let r = self.course_files.entry(c.clone()).or_default();
                        r.data = Some(*data);
                        r.origin = Origin::Network;
                        r.fetched_at = Some(at);
                        r.sync = SyncState::Idle;
                        r.version += 1;
                        effects.extend(self.place_module_files());
                    }
                    (Payload::File(file), ResourceKey::File(f)) => {
                        let r = self.files.entry(f.clone()).or_default();
                        r.data = Some(*file);
                        r.origin = Origin::Network;
                        r.fetched_at = Some(at);
                        r.sync = SyncState::Idle;
                        r.version += 1;
                        for intent in self.waiting.remove(f).unwrap_or_default() {
                            effects.extend(self.place_file(f, intent));
                        }
                        effects.extend(self.place_module_files());
                    }
                    (Payload::Tabs(list), ResourceKey::Tabs(c)) => {
                        let r = self.tabs.entry(c.clone()).or_default();
                        r.data = Some(list);
                        r.origin = Origin::Network;
                        r.fetched_at = Some(at);
                        r.sync = SyncState::Idle;
                        r.version += 1;
                        effects.extend(self.follow_tabs());
                        self.warm_tabs(c);
                    }
                    (Payload::Pages(list), ResourceKey::Pages(c)) => {
                        let r = self.page_lists.entry(c.clone()).or_default();
                        r.data = Some(list);
                        r.origin = Origin::Network;
                        r.fetched_at = Some(at);
                        r.sync = SyncState::Idle;
                        r.version += 1;
                    }
                    (Payload::Grades(grades), ResourceKey::Grades(c)) => {
                        let r = self.grades.entry(c.clone()).or_default();
                        r.data = Some(*grades);
                        r.origin = Origin::Network;
                        r.fetched_at = Some(at);
                        r.sync = SyncState::Idle;
                        r.version += 1;
                    }
                    (Payload::Modules(list), ResourceKey::Modules(c)) => {
                        let r = self.modules.entry(c.clone()).or_default();
                        r.data = Some(list);
                        r.origin = Origin::Network;
                        r.fetched_at = Some(at);
                        r.sync = SyncState::Idle;
                        r.version += 1;
                    }
                    (Payload::Conversations(list), ResourceKey::Conversations(f)) => {
                        let r = self.conversations.entry(f.clone()).or_default();
                        r.data = Some(list);
                        r.origin = Origin::Network;
                        r.fetched_at = Some(at);
                        r.sync = SyncState::Idle;
                        r.version += 1;
                        let n = self.inbox_rows().len();
                        self.inbox_item = self.inbox_item.min(n.saturating_sub(1));
                    }
                    (Payload::Conversation(detail), ResourceKey::Conversation(c)) => {
                        let r = self.conversation_details.entry(c.clone()).or_default();
                        r.data = Some(*detail);
                        r.origin = Origin::Network;
                        r.fetched_at = Some(at);
                        r.sync = SyncState::Idle;
                        r.version += 1;
                    }
                    (Payload::WikiPage(page), ResourceKey::WikiPage(c, u)) => {
                        let r = self.wiki_pages.entry((c.clone(), u.clone())).or_default();
                        r.data = Some(*page);
                        r.origin = Origin::Network;
                        r.fetched_at = Some(at);
                        r.sync = SyncState::Idle;
                        r.version += 1;
                    }
                    _ => self.set_sync(
                        &key,
                        SyncState::Failed(CanvasError::Invalid("mismatched response")),
                    ),
                }
                self.clamp_selection();
                self.mark_current_announcement_read();
                self.mark_current_conversation_read();
                effects.extend(self.warm_done(&key));
                effects
            }
            Err(error) => {
                // Keep whatever was displayed; mark it outdated instead.
                let needs_login = error.needs_login();
                if let ResourceKey::File(f) = &key
                    && let Some(intents) = self.waiting.remove(f)
                    && intents.iter().any(|i| !i.automatic)
                {
                    self.notice = Some(format!(
                        "Could not read that file from Canvas ({}). Press o to open it in the browser.",
                        error.describe()
                    ));
                }
                if let ResourceKey::File(f) = &key
                    && let Some(m) = self.module_waiting.remove(f)
                    && !m.quiet
                {
                    self.notice = Some(format!(
                        "Could not read that file from Canvas ({}). Press o to open it in the browser.",
                        error.describe()
                    ));
                }
                self.set_sync(&key, SyncState::Failed(error));
                // Without the course's folders a module file goes to Module Files.
                let mut effects = if matches!(key, ResourceKey::Files(_)) {
                    self.place_module_files()
                } else {
                    vec![]
                };
                if needs_login {
                    self.login_required = true;
                    // A session found dead at startup goes straight to sign-in;
                    // later expiry waits for the user to press L.
                    if first_result {
                        effects.push(Effect::Login);
                    }
                }
                // A list the warm-up could not read is simply missing from
                // the search; the queue carries on without it.
                effects.extend(self.warm_done(&key));
                effects
            }
        }
    }

    /// Indices of the current list view's rows after filtering.
    pub fn visible_rows(&self) -> Vec<usize> {
        let view = self.view();
        match &view.kind {
            // Hidden courses are the only rows a list leaves out now; the
            // search palette replaced narrowing a list in place.
            ViewKind::Courses => self
                .courses
                .data
                .as_deref()
                .unwrap_or(&[])
                .iter()
                .enumerate()
                .filter(|(_, c)| self.show_hidden || !self.hidden_courses.contains(&c.id))
                .map(|(i, _)| i)
                .collect(),
            ViewKind::Announcements { course_id, .. } => (0..self
                .announcements
                .get(course_id)
                .and_then(|r| r.data.as_deref())
                .map_or(0, |list| list.len()))
                .collect(),
            ViewKind::Assignments { course_id, .. } => (0..self
                .assignments
                .get(course_id)
                .and_then(|r| r.data.as_deref())
                .map_or(0, |list| list.len()))
                .collect(),
            ViewKind::Detail { .. }
            | ViewKind::Announcement { .. }
            | ViewKind::WikiPage { .. }
            | ViewKind::CourseHome { .. }
            | ViewKind::Conversation { .. }
            | ViewKind::Inbox { .. } => vec![],
            ViewKind::Files { .. } => (0..self.file_rows().len()).collect(),
            ViewKind::Modules { course_id, .. } => self.visible_modules(course_id),
            ViewKind::Grades { course_id, .. } => self.visible_grades(course_id),
            ViewKind::Pages { course_id, .. } => (0..self
                .page_lists
                .get(course_id)
                .and_then(|r| r.data.as_deref())
                .map_or(0, |list| list.len()))
                .collect(),
        }
    }

    /// Whether each visible row can be selected (module headings cannot).
    fn row_selectable(&self) -> Vec<bool> {
        match &self.view().kind {
            ViewKind::Modules { course_id, .. } => {
                let rows = self.module_rows(course_id);
                self.visible_rows()
                    .iter()
                    .map(|&i| rows.get(i).is_some_and(|r| r.selectable()))
                    .collect()
            }
            ViewKind::Grades { course_id, .. } => {
                let rows = self.grade_rows(course_id);
                self.visible_rows()
                    .iter()
                    .map(|&i| rows.get(i).is_some_and(|r| r.selectable()))
                    .collect()
            }
            _ => vec![true; self.visible_rows().len()],
        }
    }

    /// The nearest selectable row from `at`, looking in `step`'s direction first.
    fn nearest_selectable(selectable: &[bool], at: usize, step: isize) -> usize {
        let len = selectable.len() as isize;
        let first = if step < 0 { -1 } else { 1 };
        for dir in [first, -first] {
            let mut i = at as isize;
            while (0..len).contains(&i) {
                if selectable[i as usize] {
                    return i as usize;
                }
                i += dir;
            }
        }
        at
    }

    fn clamp_selection(&mut self) {
        let selectable = self.row_selectable();
        let len = selectable.len();
        let view = self.view_mut();
        if view.selected >= len {
            view.selected = len.saturating_sub(1);
        }
        if len > 0 && !selectable[view.selected] {
            view.selected = Self::nearest_selectable(&selectable, view.selected, 1);
        }
    }

    /// Every module heading and item of a course, in order.
    pub fn module_rows(&self, course_id: &str) -> Vec<ModuleRow<'_>> {
        let mut rows = Vec::new();
        for m in self
            .modules
            .get(course_id)
            .and_then(|r| r.data.as_deref())
            .unwrap_or(&[])
        {
            rows.push(ModuleRow::Module(m));
            rows.extend(m.items().iter().map(|i| ModuleRow::Item(m, i)));
        }
        rows
    }

    /// Module rows matching a search: matching items under their module's
    /// heading, or a whole module whose name matches.
    fn visible_modules(&self, course_id: &str) -> Vec<usize> {
        let modules = self
            .modules
            .get(course_id)
            .and_then(|r| r.data.as_deref())
            .unwrap_or(&[]);
        let mut out = Vec::new();
        let mut at = 0;
        for m in modules {
            out.push(at);
            out.extend((0..m.items().len()).map(|k| at + 1 + k));
            at += 1 + m.items().len();
        }
        out
    }

    /// The selected row of the Modules screen.
    pub fn selected_module_row(&self) -> Option<ModuleRow<'_>> {
        let ViewKind::Modules { course_id, .. } = &self.view().kind else {
            return None;
        };
        let index = *self.visible_rows().get(self.view().selected)?;
        self.module_rows(course_id).get(index).copied()
    }

    /// Every group heading and graded assignment of a course, in Canvas's
    /// order.
    pub fn grade_rows(&self, course_id: &str) -> Vec<GradeRow<'_>> {
        let mut rows = Vec::new();
        for g in self
            .grades
            .get(course_id)
            .and_then(|r| r.data.as_ref())
            .map(|g| g.groups.as_slice())
            .unwrap_or(&[])
        {
            rows.push(GradeRow::Group(g));
            rows.extend(g.assignments.iter().map(|a| GradeRow::Assignment(g, a)));
        }
        rows
    }

    /// Grade rows matching a search: matching assignments under their group's
    /// heading, or a whole group whose name matches.
    fn visible_grades(&self, course_id: &str) -> Vec<usize> {
        let groups = self
            .grades
            .get(course_id)
            .and_then(|r| r.data.as_ref())
            .map(|g| g.groups.as_slice())
            .unwrap_or(&[]);
        let mut out = Vec::new();
        let mut at = 0;
        for g in groups {
            out.push(at);
            out.extend((0..g.assignments.len()).map(|k| at + 1 + k));
            at += 1 + g.assignments.len();
        }
        out
    }

    /// The selected row of the Grades screen.
    pub fn selected_grade_row(&self) -> Option<GradeRow<'_>> {
        let ViewKind::Grades { course_id, .. } = &self.view().kind else {
            return None;
        };
        let index = *self.visible_rows().get(self.view().selected)?;
        self.grade_rows(course_id).get(index).copied()
    }

    /// The scores being tried on a course, if any.
    pub fn whatif_of(&self, course_id: &str) -> Option<&crate::grades::WhatIf> {
        self.whatif.get(course_id).filter(|w| !w.is_empty())
    }

    /// The score being tried on one assignment.
    pub fn whatif_score(&self, course_id: &str, assignment_id: &str) -> Option<f64> {
        self.whatif.get(course_id)?.get(assignment_id).copied()
    }

    /// The course percentage as it stands, and as the tried scores would make
    /// it. Both are this reader's own arithmetic over what Canvas sent, so
    /// they are comparable with each other; `Grades::computed` says why they
    /// can differ from the total Canvas shows.
    pub fn whatif_totals(&self, course_id: &str) -> Option<(Option<f64>, Option<f64>)> {
        let grades = self.grades.get(course_id)?.data.as_ref()?;
        let guesses = self.whatif_of(course_id)?;
        Some((
            grades.computed(&crate::grades::WhatIf::new()),
            grades.computed(guesses),
        ))
    }

    /// `w` on the Grades tab: start typing a score for the selected
    /// assignment, starting from whatever is being tried on it already.
    fn open_whatif(&mut self) {
        let ViewKind::Grades { course_id, .. } = &self.view().kind else {
            return;
        };
        let course_id = course_id.clone();
        let Some(GradeRow::Assignment(_, assignment)) = self.selected_grade_row() else {
            return;
        };
        let (assignment_id, title, possible) = (
            assignment.id.clone(),
            assignment.title().to_string(),
            assignment.points_possible,
        );
        let input = match self.whatif_score(&course_id, &assignment_id) {
            Some(score) => points(score),
            None => String::new(),
        };
        self.whatif_input = Some(WhatIfInput {
            course_id,
            assignment_id,
            title,
            points: possible,
            input,
        });
        self.dirty = true;
    }

    /// Keys while a score is being typed.
    fn whatif_act(&mut self, act: Act) -> Vec<Effect> {
        let Some(draft) = &mut self.whatif_input else {
            return vec![];
        };
        match act {
            Act::InputClear => draft.input.clear(),
            Act::InputCancel => self.whatif_input = None,
            Act::InputDone => {
                let draft = self.whatif_input.take().expect("the box is open");
                self.apply_whatif(draft);
            }
            _ => {}
        }
        self.dirty = true;
        vec![]
    }

    /// Take the typed score. An empty box puts the assignment back to
    /// whatever Canvas says, which is how a guess is undone one at a time.
    fn apply_whatif(&mut self, draft: WhatIfInput) {
        let typed = draft.input.trim();
        if typed.is_empty() {
            if let Some(course) = self.whatif.get_mut(&draft.course_id) {
                course.remove(&draft.assignment_id);
            }
            self.notice = Some(format!("{}: back to the real score.", draft.title));
            return;
        }
        let Ok(score) = typed.parse::<f64>() else {
            self.notice = Some("A score has to be a number.".into());
            return;
        };
        if !score.is_finite() || score < 0.0 {
            self.notice = Some("A score cannot be less than nothing.".into());
            return;
        }
        self.whatif
            .entry(draft.course_id)
            .or_default()
            .insert(draft.assignment_id, score);
        // Scoring above the points possible is extra credit, which Canvas
        // allows, so it is worth saying out loud rather than refusing.
        if draft.points.is_some_and(|p| p > 0.0 && score > p) {
            self.notice = Some(format!(
                "{} is more than {} is worth: counted as extra credit.",
                points(score),
                draft.title
            ));
        }
    }

    /// `W`: drop every score being tried on this course.
    fn clear_whatif(&mut self) {
        let ViewKind::Grades { course_id, .. } = &self.view().kind else {
            return;
        };
        let course_id = course_id.clone();
        let count = self
            .whatif
            .remove(&course_id)
            .map(|w| w.len())
            .unwrap_or_default();
        if count > 0 {
            self.notice = Some(match count {
                1 => "The score you tried is gone.".into(),
                n => format!("The {n} scores you tried are gone."),
            });
        }
        self.dirty = true;
    }

    /// Whether an assignment has instructor comments that have not been opened
    /// in canter yet. Canvas is never told what has been seen.
    pub fn unseen_feedback(&self, course_id: &str, assignment_id: &str) -> bool {
        self.grades
            .get(course_id)
            .and_then(|r| r.data.as_ref())
            .and_then(|g| g.feedback_for(assignment_id))
            .is_some()
            && !self
                .seen_feedback
                .contains(&(course_id.to_string(), assignment_id.to_string()))
    }

    /// Marks an assignment's instructor comments as seen here, which clears its
    /// mark on the Grades tab. Saved locally, like hidden courses.
    pub fn mark_feedback_seen(&mut self, course_id: &str, assignment_id: &str) {
        if !crate::model::valid_id(course_id) || !crate::model::valid_id(assignment_id) {
            return;
        }
        let key = (course_id.to_string(), assignment_id.to_string());
        if self.seen_feedback.insert(key) {
            self.dirty = true;
            if let Some(store) = &self.store {
                store.save_seen_feedback(&self.seen_feedback);
            }
        }
    }

    /// Turns the current grade on the course list on or off, for every course
    /// at once, and remembers the choice.
    pub fn toggle_course_grades(&mut self) -> Vec<Effect> {
        self.show_course_grades = !self.show_course_grades;
        self.dirty = true;
        if let Some(store) = &self.store {
            store.save_show_course_grades(self.show_course_grades);
        }
        self.notice = Some(if self.show_course_grades {
            "Showing each course's current grade.".into()
        } else {
            "Course grades hidden.".into()
        });
        if !self.show_course_grades {
            return vec![];
        }
        self.read_course_grades()
    }

    /// The grades the course list's grade column needs: one read per course on
    /// screen whose grades this run does not have yet. Saved copies are shown
    /// first, and a course already read is left alone, so entering the list
    /// again costs nothing. A course's own Grades tab is always read fresh.
    fn read_course_grades(&mut self) -> Vec<Effect> {
        if !self.show_course_grades || !matches!(self.view().kind, ViewKind::Courses) {
            return vec![];
        }
        let ids: Vec<String> = self
            .visible_rows()
            .iter()
            .filter_map(|&i| self.courses.data.as_ref()?.get(i).map(|c| c.id.clone()))
            .filter(|id| self.grades.get(id).and_then(|r| r.data.as_ref()).is_none())
            .collect();
        let mut effects = Vec::new();
        for id in ids {
            self.load_snapshot(&ResourceKey::Grades(id.clone()));
            if self
                .grades
                .get(&id)
                .and_then(|r| r.data.as_ref())
                .is_some_and(|g| g.total.is_some())
            {
                // A saved total is shown at once; it is replaced the next time
                // the course's own Grades tab is read.
                continue;
            }
            effects.extend(self.request(ResourceKey::Grades(id), false));
        }
        effects
    }

    /// The course's current grade for the course list: its letter grade, or
    /// its percentage where the course has no letter grades.
    pub fn course_grade(&self, course_id: &str) -> Option<String> {
        let total = self
            .grades
            .get(course_id)
            .and_then(|r| r.data.as_ref())
            .and_then(|g| g.total.as_ref())?;
        match (total.grade.as_deref(), total.score) {
            (Some(letter), Some(score)) => Some(format!("{letter} {}", percent(score))),
            (Some(letter), None) => Some(letter.to_string()),
            (None, Some(score)) => Some(percent(score)),
            (None, None) => None,
        }
    }

    /// The course sections Canvas shows, in its order. The course home comes
    /// first when it is modules or assignments, since Canvas shows those on
    /// Home even with their own tab hidden. Before the tabs are known, all.
    pub fn course_tabs(&self, course_id: &str) -> Vec<CourseTab> {
        // Canvas's Home shows the course's default view: a page, modules or
        // assignments (a syllabus or activity feed home is not shown here).
        let home = match self
            .courses
            .data
            .as_deref()
            .unwrap_or(&[])
            .iter()
            .find(|c| c.id == course_id)
            .and_then(|c| c.default_view.as_deref())
        {
            Some("wiki") => Some(CourseTab::Home),
            Some("modules") => Some(CourseTab::Modules),
            Some("assignments") => Some(CourseTab::Assignments),
            _ => None,
        };
        let mut out: Vec<CourseTab> = home.into_iter().collect();
        let Some(list) = self.tabs.get(course_id).and_then(|r| r.data.as_ref()) else {
            for tab in [
                CourseTab::Assignments,
                CourseTab::Files,
                CourseTab::Announcements,
                CourseTab::Modules,
                CourseTab::Grades,
                CourseTab::Pages,
            ] {
                if !out.contains(&tab) {
                    out.push(tab);
                }
            }
            return out;
        };
        if !list.iter().any(|t| t.id == "home") {
            out.clear();
        }
        for tab in list.iter().filter_map(|t| CourseTab::of_canvas(&t.id)) {
            if !out.contains(&tab) {
                out.push(tab);
            }
        }
        out
    }

    /// Where a course opens: its first section, or the empty course screen.
    fn course_view(&self, course_id: &str, course_name: &str) -> ViewKind {
        match self.course_tabs(course_id).first() {
            Some(tab) => tab.view(course_id.to_string(), course_name.to_string()),
            None => ViewKind::CourseHome {
                course_id: course_id.to_string(),
                course_name: course_name.to_string(),
            },
        }
    }

    /// Read a course's tabs: from the saved copy now, from Canvas once per run.
    fn read_tabs(&mut self, course_id: &str, explicit: bool) -> Vec<Effect> {
        let key = ResourceKey::Tabs(course_id.to_string());
        self.load_snapshot(&key);
        if !self.tabs_read.insert(course_id.to_string()) && !explicit {
            return vec![];
        }
        self.request(key, explicit).into_iter().collect()
    }

    /// After the tabs change: leave a section the instructor hid.
    fn follow_tabs(&mut self) -> Vec<Effect> {
        let kind = self.view().kind.clone();
        let Some((course_id, course_name)) = kind.course() else {
            return vec![];
        };
        let tabs = self.course_tabs(course_id);
        let shown = match kind.tab() {
            Some((tab, ..)) => tabs.contains(&tab),
            None => tabs.is_empty(),
        };
        if shown {
            return vec![];
        }
        let next = self.course_view(course_id, course_name);
        self.replace_tab(next)
    }

    /// Show another section of the same course in place of this one.
    fn replace_tab(&mut self, next: ViewKind) -> Vec<Effect> {
        while self.stack.len() > 1
            && matches!(
                self.stack[self.stack.len() - 2].kind,
                ViewKind::Files { .. }
            )
        {
            self.stack.pop();
        }
        *self.view_mut() = View::new(next);
        self.enter_current(false)
    }

    fn page(&self) -> usize {
        usize::from(self.size.1.saturating_sub(4)).max(1)
    }

    fn is_list(&self) -> bool {
        !matches!(
            self.view().kind,
            ViewKind::Detail { .. }
                | ViewKind::Announcement { .. }
                | ViewKind::WikiPage { .. }
                | ViewKind::Conversation { .. }
        )
    }

    fn move_selection(&mut self, delta: isize) {
        if self.is_list() {
            let selectable = self.row_selectable();
            let len = selectable.len();
            let view = self.view_mut();
            if len == 0 {
                view.selected = 0;
                return;
            }
            let target = (view.selected as isize + delta).clamp(0, len as isize - 1) as usize;
            view.selected = Self::nearest_selectable(&selectable, target, delta);
        } else {
            let view = self.view_mut();
            view.scroll = (view.scroll as isize + delta).max(0) as usize;
        }
    }

    // ---- Files and the local library ----

    /// The course's display title, from the course list or any open view.
    fn course_title(&self, course_id: &str) -> String {
        if let Some(c) = self
            .courses
            .data
            .as_ref()
            .and_then(|l| l.iter().find(|c| c.id == course_id))
        {
            return c.title().to_string();
        }
        self.stack
            .iter()
            .find_map(|v| match &v.kind {
                ViewKind::Assignments {
                    course_id: c,
                    course_name,
                }
                | ViewKind::Detail {
                    course_id: c,
                    course_name,
                    ..
                }
                | ViewKind::Files {
                    course_id: c,
                    course_name,
                    ..
                }
                | ViewKind::Modules {
                    course_id: c,
                    course_name,
                }
                | ViewKind::WikiPage {
                    course_id: c,
                    course_name,
                    ..
                }
                | ViewKind::Announcements {
                    course_id: c,
                    course_name,
                } if c == course_id => Some(course_name.clone()),
                _ => None,
            })
            .unwrap_or_else(|| format!("Course {course_id}"))
    }

    /// The course's folder in the library.
    fn course_dir(&self, course_id: &str, course_name: &str) -> String {
        match self
            .courses
            .data
            .as_ref()
            .and_then(|l| l.iter().find(|c| c.id == course_id))
        {
            Some(c) => library::course_folder(c.course_code.as_deref(), c.title()),
            None if !course_name.is_empty() => library::safe_name(course_name),
            None => library::safe_name(&self.course_title(course_id)),
        }
    }

    pub fn folder(&self, course_id: &str, folder_id: &str) -> Option<&CanvasFolder> {
        self.course_files
            .get(course_id)?
            .data
            .as_ref()?
            .folders
            .iter()
            .find(|f| f.id == folder_id)
    }

    /// Local folder for a Canvas folder (`None` = the course's Files root).
    pub fn class_dir(
        &self,
        course_id: &str,
        course_name: &str,
        folder_id: Option<&str>,
    ) -> Option<PathBuf> {
        let lib = self.library.as_ref()?;
        let mut dir = lib.class_files_dir(&self.course_dir(course_id, course_name));
        if let Some(full) = folder_id
            .and_then(|id| self.folder(course_id, id))
            .and_then(|f| f.full_name.as_deref())
        {
            dir.extend(library::class_folder_parts(full));
        }
        Some(dir)
    }

    fn file_dir(&self, course_id: &str, course_name: &str, file: &CanvasFile) -> Option<PathBuf> {
        self.class_dir(course_id, course_name, file.folder_id.as_deref())
    }

    /// Local folder for an assignment's files.
    pub fn detail_dir(&self, course_id: &str, assignment_id: &str) -> Option<PathBuf> {
        let lib = self.library.as_ref()?;
        let title = self
            .details
            .get(&(course_id.to_string(), assignment_id.to_string()))
            .and_then(|r| r.data.as_ref())
            .map(|d| d.base.title().to_string())
            .or_else(|| {
                self.stack.iter().find_map(|v| match &v.kind {
                    ViewKind::Detail {
                        course_id: c,
                        assignment_id: a,
                        title,
                        ..
                    } if c == course_id && a == assignment_id => Some(title.clone()),
                    _ => None,
                })
            })?;
        let course = self.course_dir(course_id, &self.course_title(course_id));
        Some(lib.assignment_dir(&course, &title))
    }

    /// Where a downloaded file can be found locally.
    pub fn saved_path(&self, file_id: &str) -> Option<PathBuf> {
        self.library
            .as_ref()?
            .entry(file_id)?
            .paths
            .into_iter()
            .next()
    }

    /// Files and links in an assignment's description.
    pub fn links_of(&self, course_id: &str, assignment_id: &str) -> Vec<Link> {
        self.details
            .get(&(course_id.to_string(), assignment_id.to_string()))
            .and_then(|r| r.data.as_ref())
            .and_then(|d| d.description.as_deref())
            .map(|html| crate::html::links(html, &self.origin))
            .unwrap_or_default()
    }

    /// Best known details of a file: its own read, else a course listing.
    pub fn file_meta(&self, file_id: &str) -> Option<&CanvasFile> {
        self.files
            .get(file_id)
            .and_then(|r| r.data.as_ref())
            .or_else(|| {
                self.course_files
                    .values()
                    .filter_map(|r| r.data.as_ref()?.files.as_ref())
                    .flatten()
                    .find(|f| f.id == file_id)
            })
    }

    /// Local state of a file, for display. Uses the library index only (no disk access).
    pub fn local_state(&self, file: &CanvasFile) -> Local {
        match self.transfers.get(&file.id) {
            Some(Transfer::Queued { .. }) => return Local::Queued,
            Some(Transfer::Running { received, total }) => {
                return Local::Running {
                    received: *received,
                    total: *total,
                };
            }
            Some(Transfer::Failed(m)) => return Local::Failed(m.clone()),
            None => {}
        }
        let known = self
            .library
            .as_ref()
            .map(|l| l.known_state(&file.id, &file.version()));
        match known {
            Some(LocalState::Current) => Local::Saved,
            Some(LocalState::Changed) => Local::Changed,
            _ if file.locked_for_user == Some(true) || file.url.is_none() => Local::Locked,
            _ => Local::Missing,
        }
    }

    /// The course's top folder (`course files`).
    fn root_folder(data: &CourseFilesData) -> Option<&CanvasFolder> {
        data.folders.iter().find(|f| f.parent_folder_id.is_none())
    }

    /// Rows of the current Files screen: subfolders, then files, by name.
    /// A filter searches the whole course instead of the current folder.
    pub fn file_rows(&self) -> Vec<FileRow<'_>> {
        let view = self.view();
        let ViewKind::Files {
            course_id,
            folder_id,
            ..
        } = &view.kind
        else {
            return vec![];
        };
        let Some(data) = self
            .course_files
            .get(course_id)
            .and_then(|r| r.data.as_ref())
        else {
            return vec![];
        };
        let Some(files) = data.files.as_ref() else {
            return vec![];
        };
        let here = folder_id
            .clone()
            .or_else(|| Self::root_folder(data).map(|f| f.id.clone()));
        let mut folders: Vec<&CanvasFolder> = data
            .folders
            .iter()
            .filter(|f| {
                f.parent_folder_id.is_some()
                    && f.parent_folder_id == here
                    && f.hidden_for_user != Some(true)
            })
            .collect();
        folders.sort_by_cached_key(|f| f.name.as_deref().unwrap_or("").to_lowercase());
        let mut list: Vec<&CanvasFile> = files
            .iter()
            .filter(|f| here.is_none() || f.folder_id == here)
            .collect();
        list.sort_by_cached_key(|f| f.name().to_lowercase());
        folders
            .into_iter()
            .map(FileRow::Folder)
            .chain(list.into_iter().map(FileRow::File))
            .collect()
    }

    /// `Lectures / Week 1` for the current Files folder (empty at the root).
    pub fn folder_path(&self) -> String {
        let ViewKind::Files {
            course_id,
            folder_id: Some(id),
            ..
        } = &self.view().kind
        else {
            return String::new();
        };
        self.folder(course_id, id)
            .and_then(|f| f.full_name.as_deref())
            .map(|full| full.split('/').skip(1).collect::<Vec<_>>().join(" / "))
            .unwrap_or_default()
    }

    /// Download every file in a folder and its subfolders.
    fn download_folder(
        &mut self,
        course_id: &str,
        course_name: &str,
        folder_id: Option<&str>,
    ) -> Vec<Effect> {
        let Some(data) = self
            .course_files
            .get(course_id)
            .and_then(|r| r.data.as_ref())
        else {
            return vec![];
        };
        let Some(files) = data.files.as_ref() else {
            self.notice = Some("The instructor has hidden Files for this course.".into());
            return vec![];
        };
        let start = folder_id
            .map(str::to_string)
            .or_else(|| Self::root_folder(data).map(|f| f.id.clone()));
        let mut inside: Vec<Option<String>> = vec![start.clone()];
        let mut i = 0;
        while i < inside.len() {
            let parent = inside[i].clone();
            inside.extend(
                data.folders
                    .iter()
                    .filter(|f| f.parent_folder_id.is_some() && f.parent_folder_id == parent)
                    .map(|f| Some(f.id.clone())),
            );
            i += 1;
        }
        let chosen: Vec<CanvasFile> = files
            .iter()
            .filter(|f| start.is_none() || inside.contains(&f.folder_id))
            .cloned()
            .collect();
        let batch: Vec<(String, PathBuf)> = chosen
            .iter()
            .filter_map(|f| Some((f.id.clone(), self.file_dir(course_id, course_name, f)?)))
            .collect();
        if batch.is_empty() && !chosen.is_empty() {
            self.notice = Some("The file library is unavailable.".into());
            return vec![];
        }
        self.download_batch(batch)
    }

    /// Place several files without a notice for each.
    fn download_batch(&mut self, batch: Vec<(String, PathBuf)>) -> Vec<Effect> {
        let total = batch.len();
        let mut effects = Vec::new();
        let mut bytes = 0u64;
        for (id, dir) in batch {
            let placed = self.place_file(
                &id,
                Intent {
                    dir,
                    open: false,
                    automatic: false,
                    quiet: true,
                },
            );
            for e in &placed {
                if let Effect::Download { file, .. } = e {
                    bytes += file.size.unwrap_or(0);
                }
            }
            effects.extend(placed);
        }
        let downloads = effects
            .iter()
            .filter(|e| matches!(e, Effect::Download { .. }))
            .count();
        let waiting = effects
            .iter()
            .filter(|e| matches!(e, Effect::Fetch { .. }))
            .count();
        self.notice = Some(match (downloads, waiting) {
            (0, 0) if total == 0 => "No files here.".into(),
            (0, 0) => format!("All {total} files are saved and up to date."),
            (d, 0) => format!("Downloading {d} of {total} files ({}).", human_size(bytes)),
            (d, w) => format!("Downloading {} of {total} files.", d + w),
        });
        effects
    }

    /// Put a file the user chose into `dir`, opening it afterwards if asked.
    fn place_from(&mut self, file_id: &str, dir: Option<PathBuf>, open: bool) -> Vec<Effect> {
        let Some(dir) = dir else {
            self.notice = Some("The file library is unavailable.".into());
            return vec![];
        };
        self.place_file(
            file_id,
            Intent {
                dir,
                open,
                automatic: false,
                quiet: false,
            },
        )
    }

    /// Make a file appear in a folder: link an up-to-date local copy, or
    /// download it. Waits for the file's details first if they are unknown
    /// or being read again.
    fn place_file(&mut self, file_id: &str, intent: Intent) -> Vec<Effect> {
        let key = ResourceKey::File(file_id.to_string());
        let in_flight = matches!(self.sync_of(&key), Some(SyncState::Syncing { .. }));
        let meta = if in_flight {
            None
        } else {
            self.file_meta(file_id).cloned()
        };
        let Some(file) = meta else {
            let list = self.waiting.entry(file_id.to_string()).or_default();
            if !list.contains(&intent) {
                list.push(intent);
            }
            return if in_flight {
                vec![]
            } else {
                self.request(key, false).into_iter().collect()
            };
        };
        let name = file.name().to_string();
        if file.locked_for_user == Some(true) || file.url.is_none() {
            if !intent.automatic {
                let why = file
                    .lock_explanation
                    .as_deref()
                    .map(|e| crate::html::render(e, 200, "").join(" "))
                    .filter(|e| !e.is_empty())
                    .unwrap_or_else(|| "Canvas does not offer it for download right now.".into());
                self.notice = Some(format!("{name} is locked. {why}"));
            }
            return vec![];
        }
        let Some(lib) = self.library.clone() else {
            if !intent.automatic {
                self.notice = Some("The file library is unavailable.".into());
            }
            return vec![];
        };
        let Some(target) = self.claim(&lib, &file, &intent.dir) else {
            if !intent.automatic {
                self.notice = Some(format!("No free name for {name} in its folder."));
            }
            return vec![];
        };
        let placement = Placement {
            target: target.clone(),
            open: intent.open,
            automatic: intent.automatic,
            quiet: intent.quiet,
        };
        if let Some(list) = self.pending.get_mut(file_id) {
            match list.iter_mut().find(|p| p.target == target) {
                Some(p) => {
                    p.open |= placement.open;
                    p.automatic &= placement.automatic;
                    p.quiet &= placement.quiet;
                }
                None => list.push(placement),
            }
            return vec![];
        }
        if lib.state(file_id, &file.version()) == LocalState::Current {
            match lib.reference(file_id, &target) {
                Ok(true) => {
                    if !intent.automatic {
                        lib.keep(file_id);
                    }
                    if intent.open {
                        lib.opened(file_id);
                        return vec![Effect::OpenPath(target)];
                    }
                    if !intent.quiet {
                        self.notice = Some(format!("{name} is saved and up to date."));
                    }
                    return vec![];
                }
                Ok(false) => {} // removed meanwhile: download again
                Err(_) => {
                    if !intent.automatic {
                        self.notice = Some(format!("Could not place {name} in its folder."));
                    }
                    return vec![];
                }
            }
        }
        self.pending.insert(file_id.to_string(), vec![placement]);
        self.transfers
            .insert(file_id.to_string(), Transfer::Queued { total: file.size });
        vec![Effect::Download {
            file: Box::new(file),
            target,
            automatic: intent.automatic,
        }]
    }

    /// A path in `dir` for the file that is free or already this file's.
    /// Two different files never share a name; a file the reader did not
    /// put there is never replaced.
    fn claim(&self, lib: &Library, file: &CanvasFile, dir: &std::path::Path) -> Option<PathBuf> {
        let free = |p: &std::path::Path| -> bool {
            let owner = lib.owner_of(p).or_else(|| {
                self.pending
                    .iter()
                    .find(|(_, list)| list.iter().any(|x| x.target == p))
                    .map(|(id, _)| id.clone())
            });
            match owner {
                Some(o) => o == file.id,
                None => !p.exists(),
            }
        };
        let name = library::safe_name(file.name());
        let plain = dir.join(&name);
        if free(&plain) {
            return Some(plain);
        }
        (1..50).find_map(|n| {
            let tag = if n == 1 {
                file.id.clone()
            } else {
                format!("{}-{n}", file.id)
            };
            let candidate = dir.join(library::safe_name(&library::disambiguate(&name, &tag)));
            free(&candidate).then_some(candidate)
        })
    }

    /// Drop the wrapped text of every reader screen, so the next draw builds
    /// it again. Only a finished download needs this, and re-wrapping one open
    /// page is cheap next to the read that fetched it.
    fn forget_renders(&mut self) {
        self.detail_render = None;
        self.announcement_render = None;
        self.wiki_render = None;
        self.conversation_render = None;
    }

    fn on_transfer(&mut self, file_id: String, event: TransferEvent) -> Vec<Effect> {
        let name = |app: &App, fallback: Option<&PathBuf>| {
            app.file_meta(&file_id)
                .map(|f| f.name().to_string())
                .or_else(|| {
                    fallback
                        .and_then(|p| p.file_name())
                        .map(|n| n.to_string_lossy().into_owned())
                })
                .unwrap_or_else(|| "the file".into())
        };
        match event {
            TransferEvent::Progress { received, total } => {
                if self.pending.contains_key(&file_id) {
                    self.transfers
                        .insert(file_id, Transfer::Running { received, total });
                }
                vec![]
            }
            TransferEvent::Done(path) => {
                self.transfers.remove(&file_id);
                let placements = self.pending.remove(&file_id).unwrap_or_default();
                // An image box is placed while the text is wrapped, from the
                // library's copy. That copy has just arrived, and the render
                // is kept until the resource, the width or the version
                // changes, none of which a download touches: without this the
                // picture would not appear until a refresh or a resize.
                self.forget_renders();
                if let Some(lib) = self.library.clone() {
                    if placements.iter().any(|p| !p.automatic) {
                        lib.keep(&file_id);
                    }
                    for p in placements.iter().filter(|p| p.target != path) {
                        let _ = lib.reference(&file_id, &p.target);
                    }
                }
                if let Some(p) = placements.iter().find(|p| p.open) {
                    if let Some(lib) = &self.library {
                        lib.opened(&file_id);
                    }
                    return vec![Effect::OpenPath(p.target.clone())];
                }
                if placements.iter().any(|p| !p.quiet) {
                    self.notice = Some(format!("Saved {}.", name(self, Some(&path))));
                }
                vec![]
            }
            TransferEvent::Failed {
                message,
                needs_login,
            } => {
                let placements = self.pending.remove(&file_id).unwrap_or_default();
                if needs_login {
                    self.login_required = true;
                }
                if placements.iter().any(|p| !p.automatic) {
                    self.notice = Some(format!(
                        "Could not download {}: {message}",
                        name(self, None)
                    ));
                }
                self.transfers.insert(file_id, Transfer::Failed(message));
                vec![]
            }
        }
    }

    /// After an assignment is read: read the details of the files it links
    /// to, and keep its images locally so they open instantly later.
    fn after_detail(&mut self, course_id: &str, assignment_id: &str) -> Vec<Effect> {
        let links = self.links_of(course_id, assignment_id);
        let dir = self.detail_dir(course_id, assignment_id);
        let mut effects = Vec::new();
        for link in links {
            let Target::File { id } = &link.target else {
                continue;
            };
            let key = ResourceKey::File(id.clone());
            self.load_snapshot(&key);
            effects.extend(self.request(key, false));
            if link.image
                && let Some(dir) = &dir
            {
                let intent = Intent {
                    dir: dir.clone(),
                    open: false,
                    automatic: true,
                    quiet: true,
                };
                let list = self.waiting.entry(id.clone()).or_default();
                if !list.iter().any(|i| i.dir == intent.dir) {
                    list.push(intent);
                }
            }
        }
        effects
    }

    /// Downloads waiting for a slot and running.
    pub fn transfer_counts(&self) -> (usize, usize) {
        let queued = self
            .transfers
            .values()
            .filter(|t| matches!(t, Transfer::Queued { .. }))
            .count();
        let running = self
            .transfers
            .values()
            .filter(|t| matches!(t, Transfer::Running { .. }))
            .count();
        (queued, running)
    }

    fn start_submission(&mut self, course: &str, assignment: &str, kind: DraftKind) -> Vec<Effect> {
        let Some(resource) = self
            .details
            .get(&(course.to_string(), assignment.to_string()))
        else {
            self.notice = Some("Load the assignment before submitting.".into());
            return vec![];
        };
        if resource.origin != Origin::Network || !matches!(resource.sync, SyncState::Idle) {
            self.notice = Some("Wait for a fresh assignment sync before submitting.".into());
            return vec![];
        }
        if !resource
            .fetched_at
            .is_some_and(|at| Utc::now() - at < chrono::Duration::minutes(2))
        {
            self.notice =
                Some("Checking current submission rules. Press s again when synced.".into());
            return self
                .request(
                    ResourceKey::Detail(course.to_string(), assignment.to_string()),
                    true,
                )
                .into_iter()
                .collect();
        }
        let Some(detail) = &resource.data else {
            return vec![];
        };
        if let Err(reason) = submission::eligible_for(detail, Utc::now(), kind) {
            self.notice = Some(reason);
            return vec![];
        }
        let start_directory = submission::starting_directory();
        let mut suggestions = submission::PathSuggestions::default();
        // The path box starts in the user's folder; the text box starts empty.
        let input = if kind == DraftKind::Files {
            suggestions.update(&start_directory);
            start_directory.clone()
        } else {
            String::new()
        };
        self.submission = Some(SubmissionDraft {
            input,
            start_directory,
            suggestions,
            files: Vec::new(),
            reviewing: false,
            busy: false,
            kind,
            content: String::new(),
            from_file: false,
            source: None,
        });
        vec![]
    }

    /// `R` on a conversation: reply to everyone in it. The conversation must
    /// be freshly read, so the review screen shows what is really there.
    fn start_reply(&mut self) -> Vec<Effect> {
        let ViewKind::Conversation {
            conversation_id, ..
        } = self.view().kind.clone()
        else {
            return vec![];
        };
        let Some(resource) = self.conversation_details.get(&conversation_id) else {
            self.notice = Some("Wait for the conversation to load before replying.".into());
            return vec![];
        };
        if resource.origin != Origin::Network
            || !matches!(resource.sync, SyncState::Idle)
            || !resource
                .fetched_at
                .is_some_and(|at| Utc::now() - at < chrono::Duration::minutes(2))
        {
            self.notice = Some("Reading the conversation again. Press R when synced.".into());
            return self
                .request(ResourceKey::Conversation(conversation_id), true)
                .into_iter()
                .collect();
        }
        let Some(detail) = &resource.data else {
            return vec![];
        };
        self.message = Some(MessageDraft {
            to: Audience::Conversation {
                conversation_id,
                subject: detail.base.subject().to_string(),
                who: detail.base.who(),
            },
            subject: detail.base.subject().to_string(),
            body: String::new(),
            field: MessageField::Body,
            reviewing: false,
            busy: false,
        });
        vec![]
    }

    /// `N` on the home screen: a new message to the selected course's
    /// teachers. The course comes from the course list, which the review
    /// screen names in full.
    fn start_message_to_teachers(&mut self) -> Vec<Effect> {
        if self.courses.origin != Origin::Network {
            self.notice = Some("Wait for the course list to sync, then press N.".into());
            return vec![];
        }
        let course = self
            .visible_rows()
            .get(self.view().selected)
            .and_then(|&i| self.courses.data.as_ref()?.get(i))
            .filter(|c| crate::model::valid_id(&c.id))
            .map(|c| (c.id.clone(), c.title().to_string()));
        let Some((course_id, course_name)) = course else {
            self.notice = Some("Select a course first: its teachers get the message.".into());
            return vec![];
        };
        self.message = Some(MessageDraft {
            to: Audience::Teachers {
                course_id,
                course_name,
            },
            subject: String::new(),
            body: String::new(),
            field: MessageField::Subject,
            reviewing: false,
            busy: false,
        });
        vec![]
    }

    /// Writing and reviewing a message. The request leaves only from the
    /// review screen, and only for `Act::Confirm`.
    fn message_act(&mut self, act: Act) -> Vec<Effect> {
        let draft = self.message.as_mut().expect("checked by the caller");
        if draft.busy {
            return vec![];
        }
        if draft.reviewing {
            match act {
                Act::InputCancel => draft.reviewing = false,
                Act::Confirm => {
                    let send = draft.send();
                    if let Some(problem) = send.problem() {
                        draft.reviewing = false;
                        self.notice = Some(problem.to_string());
                        return vec![];
                    }
                    draft.busy = true;
                    return vec![Effect::SendMessage(Box::new(send))];
                }
                _ => {}
            }
            return vec![];
        }
        let fields = draft.fields();
        match act {
            Act::InputCancel => self.message = None,
            Act::InputClear => draft.text_mut().clear(),
            Act::NextSection | Act::PrevSection => {
                let at = fields.iter().position(|f| *f == draft.field).unwrap_or(0);
                let n = fields.len();
                draft.field = fields[(at + if act == Act::NextSection { 1 } else { n - 1 }) % n];
            }
            // Enter is a new line in the message, and moves on from a subject.
            Act::InputDone => match draft.field {
                MessageField::Body if draft.body.chars().count() < crate::inbox::MAX_BODY => {
                    draft.body.push('\n')
                }
                MessageField::Body => {}
                MessageField::Subject => draft.field = MessageField::Body,
            },
            Act::InputComplete => match draft.send().problem() {
                Some(problem) => self.notice = Some(problem.to_string()),
                None => draft.reviewing = true,
            },
            _ => {}
        }
        vec![]
    }

    // ---- Keyboard ----
    //
    // Keys are looked up in `keys::BINDINGS` for the scopes active now, and the
    // action they name is carried out here. Nothing below matches on raw keys
    // except text typed into a text box.

    /// The key scopes active now.
    pub fn scopes(&self) -> Vec<Scope> {
        if self.help.is_some() {
            return vec![Scope::Help];
        }
        if let Some(menu) = &self.menu {
            return vec![if menu.search.is_some() {
                Scope::MenuSearch
            } else {
                Scope::Menu
            }];
        }
        if self.palette.is_some() {
            return vec![Scope::Palette];
        }
        self.screen_scopes()
    }

    /// Scopes of the screen under any open menu or help.
    pub fn screen_scopes(&self) -> Vec<Scope> {
        if let Some(draft) = &self.message {
            return if draft.busy {
                vec![]
            } else if draft.reviewing {
                vec![Scope::MessageReview]
            } else {
                vec![Scope::Compose]
            };
        }
        if let Some(draft) = &self.submission {
            return if draft.busy {
                vec![]
            } else if draft.reviewing {
                vec![Scope::Review]
            } else if draft.kind == DraftKind::Files || draft.from_file {
                vec![Scope::PathInput]
            } else {
                vec![Scope::TextInput]
            };
        }
        if self.whatif_input.is_some() {
            return vec![Scope::WhatIfInput];
        }
        let view = self.view();
        let mut scopes = vec![Scope::Screen];
        if self.login_required {
            scopes.push(Scope::SignedOut);
        }
        match &view.kind {
            ViewKind::Inbox { .. } => scopes.push(Scope::Inbox),
            ViewKind::Courses => match self.focus() {
                Section::Calendar => scopes.extend([Scope::Courses, Scope::Calendar]),
                Section::Day => scopes.extend([Scope::Courses, Scope::Day]),
                // The home screen shows a count in a box; the folders and the
                // conversations are on the Inbox screen behind it.
                Section::Inbox => scopes.extend([Scope::Courses, Scope::InboxWidget]),
                _ => scopes.extend([Scope::List, Scope::Courses]),
            },
            ViewKind::Grades { .. } => {
                scopes.extend([Scope::List, Scope::CourseTabs, Scope::Grades])
            }
            ViewKind::Assignments { .. }
            | ViewKind::Announcements { .. }
            | ViewKind::Pages { .. } => scopes.extend([Scope::List, Scope::CourseTabs]),
            // Modules share the Files keys: d, D and e work on module files.
            ViewKind::Files { .. } | ViewKind::Modules { .. } => {
                scopes.extend([Scope::List, Scope::CourseTabs, Scope::Files])
            }
            ViewKind::CourseHome { .. } => {}
            ViewKind::Detail { .. }
            | ViewKind::Announcement { .. }
            | ViewKind::WikiPage { .. }
            | ViewKind::Conversation { .. } => {
                scopes.push(Scope::Page);
                // On a course's Home, Tab moves between the course's tabs.
                scopes.push(if view.kind.tab().is_some() {
                    Scope::CourseTabs
                } else {
                    Scope::Sections
                });
                if matches!(view.kind, ViewKind::Detail { .. }) {
                    scopes.push(Scope::Assignment);
                }
                if !self.current_links().is_empty() {
                    scopes.push(Scope::HasLinks);
                }
                if matches!(view.kind, ViewKind::Conversation { .. }) {
                    scopes.push(Scope::Conversation);
                }
                match self.focus() {
                    Section::Links => scopes.push(Scope::Links),
                    Section::Submissions => scopes.push(Scope::Submissions),
                    Section::Feedback => scopes.push(Scope::Feedback),
                    Section::Text | Section::Calendar | Section::Day | Section::Inbox => {}
                }
            }
        }
        scopes
    }

    /// The instructor's feedback on one attempt: the comments Canvas tagged
    /// with that attempt (and, on the latest, the untagged ones).
    pub fn attempt_comments(
        &self,
        course_id: &str,
        assignment_id: &str,
        attempt: usize,
    ) -> Vec<&submission::Comment> {
        let Some(history) = self
            .histories
            .get(&(course_id.to_string(), assignment_id.to_string()))
            .and_then(|r| r.data.as_ref())
        else {
            return vec![];
        };
        let Some(entry) = history.attempts.get(attempt) else {
            return vec![];
        };
        history.comments_for(entry.attempt, attempt == 0)
    }

    /// The rubric criteria the instructor scored, with the score. Canvas keeps
    /// one rubric assessment per submission, so it is shown with the latest
    /// attempt only.
    pub fn rubric_scores(
        &self,
        course_id: &str,
        assignment_id: &str,
        attempt: usize,
    ) -> Vec<(&crate::model::RubricCriterion, &submission::CriterionScore)> {
        if attempt != 0 {
            return vec![];
        }
        let key = (course_id.to_string(), assignment_id.to_string());
        let Some(history) = self.histories.get(&key).and_then(|r| r.data.as_ref()) else {
            return vec![];
        };
        let Some(detail) = self.details.get(&key).and_then(|r| r.data.as_ref()) else {
            return vec![];
        };
        detail
            .rubric
            .iter()
            .filter_map(|c| {
                let score = history.rubric.get(c.id.as_deref()?)?;
                Some((c, score))
            })
            .collect()
    }

    /// The feedback section's selectable entries, in the order they are drawn.
    pub fn feedback_entries(
        &self,
        course_id: &str,
        assignment_id: &str,
        attempt: usize,
    ) -> Vec<FeedbackEntry> {
        let comments = self
            .attempt_comments(course_id, assignment_id, attempt)
            .len();
        let criteria = self.rubric_scores(course_id, assignment_id, attempt).len();
        (0..comments)
            .map(FeedbackEntry::Comment)
            .chain((0..criteria).map(FeedbackEntry::Criterion))
            .collect()
    }

    /// Whether the selected attempt has feedback to show at all.
    fn has_feedback(&self, course_id: &str, assignment_id: &str, attempt: usize) -> bool {
        !self
            .feedback_entries(course_id, assignment_id, attempt)
            .is_empty()
    }

    /// Where the files attached to instructor comments are kept:
    /// `Assignment Files\<assignment>\Feedback`.
    pub fn feedback_dir(&self, course_id: &str, assignment_id: &str) -> Option<PathBuf> {
        Some(self.detail_dir(course_id, assignment_id)?.join("Feedback"))
    }

    /// The instructor's comments for the selected attempt are on screen, so
    /// they count as read. This is the single place the Grades tab's local
    /// seen-store should be told: wire `mark_feedback_seen` in here.
    /// Returns the assignment when this run had not recorded it yet.
    /// Reading an assignment's feedback is what marks its instructor comments
    /// seen, which clears the mark the Grades tab puts on its row. The record
    /// is local, so Canvas is never told what has been read.
    fn note_feedback_read(&mut self) {
        let ViewKind::Detail {
            course_id,
            assignment_id,
            ..
        } = self.view().kind.clone()
        else {
            return;
        };
        if !self.has_feedback(&course_id, &assignment_id, self.view().attempt) {
            return;
        }
        self.mark_feedback_seen(&course_id, &assignment_id);
    }

    /// Sections the current page has, in order: text, files & links, submissions.
    /// Sections the current page has, in order: text, files & links,
    /// submissions. On the home screen: the courses, the calendar, the Inbox.
    pub fn sections(&self) -> Vec<Section> {
        if self.view().kind == ViewKind::Courses {
            return vec![Section::Text, Section::Calendar, Section::Inbox];
        }
        let Some(page) = self.view().kind.page() else {
            return vec![];
        };
        let mut out = vec![Section::Text];
        if !self.page_links(&page).is_empty() {
            out.push(Section::Links);
        }
        if let Page::Assignment(c, a) = &page
            && self.attempt_count(c, a) > 0
        {
            out.push(Section::Submissions);
            if self.has_feedback(c, a, self.view().attempt) {
                out.push(Section::Feedback);
            }
        }
        out
    }

    /// The focused section, falling back to the text if it went away.
    pub fn focus(&self) -> Section {
        let focus = self.view().focus;
        if focus == Section::Day {
            return if self.day_items(self.calendar_day).is_empty() {
                Section::Calendar
            } else {
                Section::Day
            };
        }
        if focus == Section::Text || self.sections().contains(&focus) {
            focus
        } else {
            Section::Text
        }
    }

    /// Files and links of the current page.
    pub fn current_links(&self) -> Vec<Link> {
        self.view()
            .kind
            .page()
            .map(|p| self.page_links(&p))
            .unwrap_or_default()
    }

    pub fn page_links(&self, page: &Page) -> Vec<Link> {
        match page {
            Page::Assignment(c, a) => self.links_of(c, a),
            Page::Announcement(c, a) => self.announcement_links(c, a),
            Page::Wiki(c, u) => self.wiki_links(c, u),
            Page::Conversation(id) => self.conversation_links(id),
        }
    }

    fn page_dir(&self, page: &Page) -> Option<PathBuf> {
        match page {
            Page::Assignment(c, a) => self.detail_dir(c, a),
            Page::Announcement(c, a) => self.announcement_dir(c, a),
            Page::Wiki(c, u) => self.wiki_dir(c, u),
            Page::Conversation(id) => self.conversation_dir(id),
        }
    }

    /// The selected entry of the files & links section, while it has focus.
    pub fn selected_link(&self) -> Option<Link> {
        if self.focus() != Section::Links {
            return None;
        }
        self.current_links().get(self.view().link).cloned()
    }

    /// Whether an action does something here (for the hint line and the menu).
    pub fn available(&self, act: Act) -> bool {
        if self.help.is_some()
            || self.menu.is_some()
            || self.submission.is_some()
            || self.message.is_some()
            || self.palette.is_some()
        {
            return true;
        }
        self.screen_available(act)
    }

    fn screen_available(&self, act: Act) -> bool {
        let page = self.view().kind.page();
        match act {
            Act::Open => match (page, self.focus()) {
                (Some(Page::Assignment(c, a)), Section::Feedback) => self
                    .selected_feedback(&c, &a)
                    .is_some_and(|e| matches!(e, FeedbackEntry::Comment(_))),
                (Some(_), focus) => focus == Section::Links,
                (None, Section::Calendar | Section::Day) => {
                    !self.day_items(self.calendar_day).is_empty()
                }
                // The box opens whether or not anything is waiting: the
                // folders are read by the screen it opens, not by the home
                // screen, so there is nothing here to be empty.
                (None, Section::Inbox) if self.view().kind == ViewKind::Courses => true,
                // On the Inbox screen itself, Enter opens the conversation
                // selected. Conversations are not `visible_rows`, so this
                // would otherwise read as nothing to open.
                (None, _) if matches!(self.view().kind, ViewKind::Inbox { .. }) => {
                    !self.inbox_rows().is_empty()
                }
                (None, _) => !self.visible_rows().is_empty(),
            },
            // A score can only be tried on an assignment, not on a group
            // heading, and there is nothing to clear until one has been.
            Act::WhatIf => matches!(
                self.selected_grade_row(),
                Some(GradeRow::Assignment(..))
            ),
            Act::ClearWhatIf => match &self.view().kind {
                ViewKind::Grades { course_id, .. } => self.whatif_of(course_id).is_some(),
                _ => false,
            },
            Act::Download if self.focus() == Section::Feedback => {
                !self.selected_feedback_files().is_empty()
            }
            Act::DownloadAll if self.focus() == Section::Feedback => {
                !self.feedback_files().is_empty()
            }
            Act::Download => match self.view().kind {
                ViewKind::Files { .. } => !self.file_rows().is_empty(),
                ViewKind::Modules { .. } => matches!(
                    self.selected_module_row(),
                    Some(ModuleRow::Item(_, i)) if i.kind() == ItemKind::File
                ),
                _ => matches!(
                    self.selected_link().map(|l| l.target),
                    Some(Target::File { .. })
                ),
            },
            Act::DownloadAll => match &self.view().kind {
                ViewKind::Files { .. } => !self.file_rows().is_empty(),
                ViewKind::Modules { course_id, .. } => self
                    .module_rows(course_id)
                    .iter()
                    .any(|r| matches!(r, ModuleRow::Item(_, i) if i.kind() == ItemKind::File)),
                _ => self
                    .current_links()
                    .iter()
                    .any(|l| matches!(l.target, Target::File { .. })),
            },
            Act::ShowFolder | Act::OpenLibrary => self.library.is_some(),
            Act::Submit => self.accepts(DraftKind::Files),
            Act::SubmitText => self.accepts(DraftKind::Text),
            Act::SubmitUrl => self.accepts(DraftKind::Url),
            Act::NextSection | Act::PrevSection => match self.view().kind.tab() {
                Some((_, course_id, _)) => self.course_tabs(course_id).len() > 1,
                None => self.sections().len() > 1,
            },
            Act::HideCourse => !self.visible_rows().is_empty(),
            Act::Reply => {
                matches!(&self.view().kind, ViewKind::Conversation { conversation_id, .. }
                if self.conversation_details.get(conversation_id).is_some_and(|r| r.data.is_some()))
            }
            Act::NewMessage => !self.visible_rows().is_empty(),
            Act::Search => self.is_list(),
            _ => true,
        }
    }

    /// Whether this assignment lists a submission type the reader can send.
    fn accepts(&self, kind: DraftKind) -> bool {
        match &self.view().kind {
            ViewKind::Detail {
                course_id,
                assignment_id,
                ..
            } => self
                .details
                .get(&(course_id.clone(), assignment_id.clone()))
                .and_then(|r| r.data.as_ref())
                .is_some_and(|d| {
                    d.base
                        .submission_types
                        .iter()
                        .any(|t| t == kind.canvas_type())
                }),
            _ => false,
        }
    }

    /// The feedback entry the cursor is on.
    pub fn selected_feedback(&self, course_id: &str, assignment_id: &str) -> Option<FeedbackEntry> {
        self.feedback_entries(course_id, assignment_id, self.view().attempt)
            .get(self.view().feedback)
            .copied()
    }

    /// Canvas ids of the files attached to the selected comment.
    fn selected_feedback_files(&self) -> Vec<String> {
        let Some(Page::Assignment(c, a)) = self.view().kind.page() else {
            return vec![];
        };
        match self.selected_feedback(&c, &a) {
            Some(FeedbackEntry::Comment(i)) => self
                .attempt_comments(&c, &a, self.view().attempt)
                .get(i)
                .map(|comment| comment.files().map(|f| f.id.clone()).collect())
                .unwrap_or_default(),
            _ => vec![],
        }
    }

    /// Canvas ids of every file attached to the feedback on this attempt.
    fn feedback_files(&self) -> Vec<String> {
        let Some(Page::Assignment(c, a)) = self.view().kind.page() else {
            return vec![];
        };
        let mut ids: Vec<String> = Vec::new();
        for comment in self.attempt_comments(&c, &a, self.view().attempt) {
            for file in comment.files() {
                if !ids.contains(&file.id) {
                    ids.push(file.id.clone());
                }
            }
        }
        ids
    }

    /// Why the selected feedback row has nothing to open or download.
    fn no_feedback_files(&self) -> String {
        let criterion = match self.view().kind.page() {
            Some(Page::Assignment(c, a)) => matches!(
                self.selected_feedback(&c, &a),
                Some(FeedbackEntry::Criterion(_))
            ),
            _ => false,
        };
        if criterion {
            "A rubric score has no files. Press o to open the submission.".into()
        } else {
            "No file is attached to this comment. Press o to open the submission.".into()
        }
    }

    /// Put a comment's files in the assignment's `Feedback` folder. Their
    /// metadata arrives inside the comment, so it is read again before use,
    /// as announcement attachments are.
    fn place_feedback_files(&mut self, ids: Vec<String>, open: bool) -> Vec<Effect> {
        let Some(Page::Assignment(course_id, assignment_id)) = self.view().kind.page() else {
            return vec![];
        };
        let dir = self.feedback_dir(&course_id, &assignment_id);
        if dir.is_none() {
            self.notice = Some("The file library is unavailable.".into());
            return vec![];
        }
        let single = ids.len() == 1;
        let mut effects = Vec::new();
        for id in &ids {
            effects.extend(self.place_announcement_file(id, dir.clone(), open && single));
        }
        if !single {
            self.notice = Some(format!(
                "Checking {} feedback files before downloading…",
                ids.len()
            ));
        }
        effects
    }

    /// The actions menu's entries for the screen under it, filtered by its search.
    pub fn menu_entries(&self) -> Vec<&'static keys::Binding> {
        let scopes = self.screen_scopes();
        let search = self
            .menu
            .as_ref()
            .and_then(|m| m.search.as_deref())
            .unwrap_or("")
            .to_lowercase();
        keys::menu(&scopes, &|a| self.screen_available(a))
            .into_iter()
            .filter(|b| search.is_empty() || self.title_of(b).to_lowercase().contains(&search))
            .collect()
    }

    /// An action's name in the menu and help, adjusted to what is selected.
    pub fn title_of(&self, binding: &keys::Binding) -> String {
        match binding.act {
            Act::HideCourse => {
                let hidden = self
                    .visible_rows()
                    .get(self.view().selected)
                    .and_then(|&i| self.courses.data.as_ref()?.get(i))
                    .is_some_and(|c| self.hidden_courses.contains(&c.id));
                if hidden {
                    "Restore this course"
                } else {
                    "Hide this course"
                }
                .into()
            }
            Act::ShowHidden if self.palette.is_some() && self.search_hidden => {
                "Stop searching hidden courses".into()
            }
            Act::ShowHidden if self.palette.is_some() => "Search hidden courses too".into(),
            Act::ShowHidden if self.show_hidden => "Stop showing hidden courses".into(),
            Act::ShowHidden => "Show hidden courses".into(),
            Act::ShowGrades if self.show_course_grades => "Hide course grades".into(),
            Act::ShowGrades => "Show each course's current grade".into(),
            _ => binding.title.into(),
        }
    }

    fn on_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && key.code == KeyCode::Char('c') {
            self.quit = true;
            return vec![];
        }
        let scopes = self.scopes();
        if let Some(binding) = keys::lookup(keys::key_of(&key), &scopes) {
            return self.perform(binding.act, &key);
        }
        if keys::typing(&scopes) {
            self.type_text(&key);
            return vec![];
        }
        // Letters in the actions menu run the entry they label.
        if scopes.contains(&Scope::Menu)
            && let KeyCode::Char(c) = key.code
            && !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
            && let Some(entry) = self.menu_entries().into_iter().find(|e| e.menu == Some(c))
        {
            self.menu = None;
            return self.perform(entry.act, &key);
        }
        vec![]
    }

    /// Text typed into the search box, the menu's search, or a file path.
    fn type_text(&mut self, key: &KeyEvent) {
        let typed = match key.code {
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                Some(c)
            }
            _ => None,
        };
        let erase = key.code == KeyCode::Backspace;
        if let Some(palette) = &mut self.palette {
            if erase {
                palette.query.pop();
            } else if let Some(c) = typed
                && palette.query.chars().count() < 60
            {
                palette.query.push(c);
            }
            // Any change to the query reorders everything, so the selection
            // goes back to the top rather than to whatever is now there.
            palette.selected = 0;
            return;
        }
        if let Some(draft) = &mut self.whatif_input {
            if erase {
                draft.input.pop();
            } else if let Some(c) = typed
                // A score is a number: letters would only be rejected later,
                // and a box that swallows them looks broken.
                && (c.is_ascii_digit() || (c == '.' && !draft.input.contains('.')))
                && draft.input.chars().count() < 8
            {
                draft.input.push(c);
            }
            return;
        }
        if let Some(menu) = &mut self.menu {
            let search = menu.search.get_or_insert_with(String::new);
            if erase {
                search.pop();
            } else if let Some(c) = typed
                && search.chars().count() < 40
            {
                search.push(c);
            }
            menu.selected = 0;
            return;
        }
        if let Some(draft) = &mut self.message {
            if draft.reviewing || draft.busy {
                return;
            }
            let limit = match draft.field {
                MessageField::Subject => crate::inbox::MAX_SUBJECT,
                MessageField::Body => crate::inbox::MAX_BODY,
            };
            // A held Backspace would wipe the field; one character per press.
            if erase && key.kind != KeyEventKind::Repeat {
                draft.text_mut().pop();
            } else if let Some(c) = typed
                && draft.text_mut().chars().count() < limit
            {
                draft.text_mut().push(c);
            }
            return;
        }
        if let Some(draft) = &mut self.submission {
            let typing_text = draft.kind != DraftKind::Files && !draft.from_file;
            let limit = match (typing_text, draft.kind) {
                (true, DraftKind::Text) => submission::MAX_TEXT,
                (true, DraftKind::Url) => submission::MAX_URL,
                _ => 1024,
            };
            // A held Backspace would wipe the path; one character per press.
            if erase && (typing_text || key.kind != KeyEventKind::Repeat) {
                draft.input.pop();
            } else if let Some(c) = typed
                && draft.input.chars().count() < limit
            {
                draft.input.push(c);
            } else {
                return;
            }
            if !typing_text {
                draft.suggestions.update(&draft.input);
            }
        }
    }

    /// Carry out an action.
    fn perform(&mut self, act: Act, key: &KeyEvent) -> Vec<Effect> {
        if self.help.is_some() {
            return self.help_act(act);
        }
        if self.menu.is_some() {
            return self.menu_act(act);
        }
        if self.submission.is_some() {
            return self.submission_act(act);
        }
        if self.message.is_some() {
            return self.message_act(act);
        }
        if self.palette.is_some() {
            return self.palette_act(act);
        }
        if self.whatif_input.is_some() {
            return self.whatif_act(act);
        }
        let page = self.view().kind.page();
        match act {
            Act::Quit => {
                self.quit = true;
                vec![]
            }
            Act::Up
            | Act::Down
            | Act::Left
            | Act::Right
            | Act::PageUp
            | Act::PageDown
            | Act::Top
            | Act::Bottom => self.navigate(act),
            Act::Open if self.focus() == Section::Feedback => {
                let files = self.selected_feedback_files();
                if files.is_empty() {
                    self.notice = Some(self.no_feedback_files());
                    return vec![];
                }
                self.place_feedback_files(files, true)
            }
            Act::Open => match &page {
                Some(page) => self.open_link(page),
                None => self.open_selected(),
            },
            Act::Back => self.go_back(),
            Act::NextSection | Act::PrevSection => {
                let forward = act == Act::NextSection;
                if self.view().kind.tab().is_some() {
                    self.switch_tab(!forward)
                } else {
                    self.cycle_section(forward);
                    vec![]
                }
            }
            // Only the actions menu searches itself; `/` opens the palette.
            Act::Search => vec![],
            Act::Refresh => self.refresh(),
            Act::Browser => self
                .browser_url()
                .map(Effect::OpenUrl)
                .into_iter()
                .collect(),
            Act::Menu => {
                self.menu = Some(MenuState::default());
                vec![]
            }
            Act::Palette => self.open_palette(),
            Act::WhatIf => {
                self.open_whatif();
                vec![]
            }
            Act::ClearWhatIf => {
                self.clear_whatif();
                vec![]
            }
            Act::Help => {
                self.help = Some(0);
                vec![]
            }
            Act::Download => self.download_selected(),
            Act::DownloadAll => self.download_everything(),
            Act::ShowFolder => self.show_folder(),
            Act::Submit | Act::SubmitText | Act::SubmitUrl => {
                let kind = match act {
                    Act::SubmitText => DraftKind::Text,
                    Act::SubmitUrl => DraftKind::Url,
                    _ => DraftKind::Files,
                };
                match self.view().kind.clone() {
                    ViewKind::Detail {
                        course_id,
                        assignment_id,
                        ..
                    } => self.start_submission(&course_id, &assignment_id, kind),
                    _ => vec![],
                }
            }
            Act::JumpToLink => {
                if let KeyCode::Char(c) = key.code
                    && let Some(n) = c.to_digit(10)
                {
                    self.jump_to_link(n as usize);
                }
                vec![]
            }
            Act::HideCourse => {
                self.toggle_selected_course_hidden();
                vec![]
            }
            Act::ShowGrades => self.toggle_course_grades(),
            Act::ShowHidden => {
                self.show_hidden = !self.show_hidden;
                self.notice = Some(if self.show_hidden {
                    "Showing hidden courses. Select one, then Space h to restore it.".into()
                } else {
                    "Hidden courses are hidden again.".into()
                });
                self.clamp_selection();
                vec![]
            }
            Act::OpenLibrary => {
                let root = self.library.as_ref().map(|l| l.root().to_path_buf());
                self.reveal(root)
            }
            Act::SignIn => vec![Effect::Login],
            Act::Reply => self.start_reply(),
            Act::NewMessage => self.start_message_to_teachers(),
            Act::InputDone
            | Act::InputCancel
            | Act::InputComplete
            | Act::InputClear
            | Act::LoadTextFile
            | Act::Confirm => vec![],
        }
    }

    fn help_act(&mut self, act: Act) -> Vec<Effect> {
        let page = self.page();
        let Some(scroll) = self.help.as_mut() else {
            return vec![];
        };
        match act {
            Act::Up => *scroll = scroll.saturating_sub(1),
            Act::Down => *scroll += 1,
            Act::PageUp => *scroll = scroll.saturating_sub(page),
            Act::PageDown => *scroll += page,
            Act::Top => *scroll = 0,
            Act::Bottom => *scroll = usize::MAX / 2,
            Act::Back | Act::Help => self.help = None,
            _ => {}
        }
        vec![]
    }

    /// Queue every list the palette wants, once per run.
    ///
    /// Searching can only find what has been read, and nothing reads a course
    /// you never open, so the lists are fetched in the background when the
    /// course list arrives. This is the one place the reader asks Canvas for
    /// something the user did not ask for, which is why it is bounded: each
    /// list once per run, nothing re-read, and the queue is dropped on logout
    /// with everything else.
    ///
    /// Pages and Files are not queued here. Whether a course has them is in
    /// its tab list, which is itself being read; `warm_tabs` adds them when
    /// the answer arrives, so no course is asked for a tab it does not show.
    fn start_warm_up(&mut self) -> Vec<Effect> {
        if !self.warm_up || self.warmed {
            return vec![];
        }
        self.warmed = true;
        let courses: Vec<String> = self
            .courses
            .data
            .as_deref()
            .unwrap_or(&[])
            .iter()
            .map(|c| c.id.clone())
            .collect();
        // Hidden courses are searched too: hiding one tidies the list, it does
        // not say to forget the course.
        for id in &courses {
            self.warm_queue
                .push_back(ResourceKey::Assignments(id.clone()));
            self.warm_queue
                .push_back(ResourceKey::Announcements(id.clone()));
            self.warm_queue.push_back(ResourceKey::Tabs(id.clone()));
        }
        for folder in crate::inbox::Folder::ALL {
            self.warm_queue
                .push_back(ResourceKey::Conversations(folder.id().into()));
        }
        // Every saved copy, now, before a single request. These are small
        // files on disk: reading them needs no slot and nothing to wait for,
        // so the box is useful at once and the reads that follow only bring
        // it up to date. Trickling them through the same two slots as the
        // requests, which is what this did at first, made a search wait on
        // the network to show an answer it already had.
        let queued: Vec<ResourceKey> = self.warm_queue.iter().cloned().collect();
        for key in queued {
            self.load_snapshot(&key);
        }
        self.warm_next()
    }

    /// Add a course's pages and files once its tab list says it has them.
    fn warm_tabs(&mut self, course_id: &str) {
        if !self.warmed {
            return;
        }
        for tab in self.course_tabs(course_id) {
            let key = match tab {
                CourseTab::Pages => ResourceKey::Pages(course_id.to_string()),
                CourseTab::Files => ResourceKey::Files(course_id.to_string()),
                _ => continue,
            };
            if !self.warm_queue.contains(&key) {
                self.warm_queue.push_back(key);
            }
        }
    }

    /// Start as many queued reads as the warm-up is allowed at once.
    ///
    /// The cap is what keeps this out of the way: the executor runs four
    /// requests at a time, so holding the warm-up to two leaves half the
    /// capacity for the screen the user is actually on. Without it, opening a
    /// course would queue behind fifty reads nobody asked for.
    fn warm_next(&mut self) -> Vec<Effect> {
        let mut effects = Vec::new();
        while self.warm_running < WARM_AT_ONCE {
            let Some(key) = self.warm_queue.pop_front() else {
                break;
            };
            // Read already this run, or being read for the screen in front
            // of us. A saved copy does not stop the read: it is already on
            // screen, and this brings it up to date, because a search that
            // quietly answers from last week is worse than one that waits.
            if self.read_this_run(&key)
                || matches!(self.sync_of(&key), Some(SyncState::Syncing { .. }))
            {
                continue;
            }
            if let Some(effect) = self.request(key.clone(), false) {
                self.warm_running += 1;
                self.warm_keys.push(key);
                effects.push(effect);
            }
        }
        effects
    }

    /// A warm-up read finished (or failed): free its slot and start the next.
    fn warm_done(&mut self, key: &ResourceKey) -> Vec<Effect> {
        if let Some(at) = self.warm_keys.iter().position(|k| k == key) {
            self.warm_keys.remove(at);
            self.warm_running = self.warm_running.saturating_sub(1);
            return self.warm_next();
        }
        vec![]
    }

    /// Whether this resource has already been read from Canvas in this run,
    /// by the warm-up or by the user opening the screen that shows it.
    fn read_this_run(&self, key: &ResourceKey) -> bool {
        let fresh = |origin: Option<Origin>| origin == Some(Origin::Network);
        match key {
            ResourceKey::Assignments(c) => fresh(self.assignments.get(c).map(|r| r.origin)),
            ResourceKey::Announcements(c) => fresh(self.announcements.get(c).map(|r| r.origin)),
            ResourceKey::Tabs(c) => fresh(self.tabs.get(c).map(|r| r.origin)),
            ResourceKey::Pages(c) => fresh(self.page_lists.get(c).map(|r| r.origin)),
            ResourceKey::Files(c) => fresh(self.course_files.get(c).map(|r| r.origin)),
            ResourceKey::Conversations(f) => fresh(self.conversations.get(f).map(|r| r.origin)),
            _ => false,
        }
    }

    /// Open the search palette, empty, with the first result selected.
    fn open_palette(&mut self) -> Vec<Effect> {
        self.palette = Some(PaletteState {
            query: String::new(),
            selected: 0,
        });
        self.dirty = true;
        // Nothing is read here: the palette shows what the warm-up has
        // already brought in, and says on screen how much that is.
        vec![]
    }

    /// Everything the palette could offer, before the query narrows it.
    ///
    /// Only what is already in hand: no reads happen while searching. The
    /// warm-up fills this in the background once per run, and the palette
    /// says how much of it has arrived, so a short list is never mistaken
    /// for an empty account.
    fn palette_pool(&self) -> Vec<crate::search::Hit> {
        use crate::search::{Hit, Kind, Target};
        let mut pool: Vec<Hit> = Vec::new();
        for course in self.courses.data.as_deref().unwrap_or(&[]) {
            if !self.searchable_course(&course.id) {
                continue;
            }
            let hidden = self.hidden_courses.contains(&course.id);
            let context = match (hidden, course.term.as_ref().and_then(|t| t.name.as_deref())) {
                (true, _) => "hidden".to_string(),
                (false, Some(term)) => term.to_string(),
                (false, None) => String::new(),
            };
            pool.push(Hit::new(
                Kind::Course,
                course.title(),
                context,
                Target::Course(course.id.clone()),
            ));
        }
        for (course_id, resource) in &self.assignments {
            if !self.searchable_course(course_id) {
                continue;
            }
            let course = self.course_title(course_id);
            for item in resource.data.as_deref().unwrap_or(&[]) {
                pool.push(Hit::new(
                    Kind::Assignment,
                    item.title(),
                    &course,
                    Target::Assignment(course_id.clone(), item.id.clone()),
                ));
            }
        }
        for (course_id, resource) in &self.announcements {
            if !self.searchable_course(course_id) {
                continue;
            }
            let course = self.course_title(course_id);
            for item in resource.data.as_deref().unwrap_or(&[]) {
                pool.push(Hit::new(
                    Kind::Announcement,
                    item.title(),
                    &course,
                    Target::Announcement(course_id.clone(), item.id.clone()),
                ));
            }
        }
        for (course_id, resource) in &self.page_lists {
            if !self.searchable_course(course_id) {
                continue;
            }
            let course = self.course_title(course_id);
            for page in resource.data.as_deref().unwrap_or(&[]) {
                let Some(slug) = page.slug() else {
                    continue;
                };
                pool.push(Hit::new(
                    Kind::Page,
                    page.title(),
                    &course,
                    Target::Page(course_id.clone(), slug.to_string()),
                ));
            }
        }
        for (course_id, resource) in &self.course_files {
            if !self.searchable_course(course_id) {
                continue;
            }
            let course = self.course_title(course_id);
            let files = resource.data.as_ref().and_then(|d| d.files.as_deref());
            for file in files.unwrap_or(&[]) {
                pool.push(Hit::new(
                    Kind::File,
                    file.name(),
                    &course,
                    Target::File(course_id.clone(), file.id.clone()),
                ));
            }
        }
        // One conversation can sit in several folders, so show it once.
        let mut seen: Vec<&str> = Vec::new();
        for resource in self.conversations.values() {
            for item in resource.data.as_deref().unwrap_or(&[]) {
                if seen.contains(&item.id.as_str()) {
                    continue;
                }
                seen.push(&item.id);
                pool.push(Hit::new(
                    Kind::Conversation,
                    item.subject(),
                    item.who(),
                    Target::Conversation(item.id.clone()),
                ));
            }
        }
        let scopes = self.screen_scopes();
        for binding in keys::menu(&scopes, &|a| self.screen_available(a)) {
            pool.push(Hit::new(
                Kind::Action,
                self.title_of(binding),
                // The badge already says this is an action.
                "",
                Target::Action(binding.act),
            ));
        }
        pool
    }

    /// The courses an `@` tag names, best match first.
    ///
    /// A tag is matched against course names and codes the same way anything
    /// else is, so `@cop46` is enough, and `@` with nothing after it names
    /// every course, which is what makes the suggestions useful.
    fn ranked_courses(&self, tag: &str) -> Vec<(i64, String)> {
        let mut found: Vec<(i64, String)> = Vec::new();
        for course in self.courses.data.as_deref().unwrap_or(&[]) {
            if !self.searchable_course(&course.id) {
                continue;
            }
            let code = course.course_code.as_deref().unwrap_or("");
            let best = crate::search::best_of(tag, &[course.title(), code]);
            if let Some(score) = best {
                found.push((score, course.id.clone()));
            }
        }
        found.sort_by_key(|a| std::cmp::Reverse(a.0));
        found
    }

    /// The class an `@` tag means: the best match, and anything tied with it.
    ///
    /// Not every class the letters appear in. `@comp` is a subsequence of
    /// both "Compilers" and "GPU Computing", and a tag that keeps both has
    /// not narrowed anything, which is the whole point of typing it. While
    /// the tag is still being typed the palette offers every match, ranked,
    /// so a tie is visible and can be settled with one more character.
    fn tagged_courses(&self, tag: &str) -> Vec<String> {
        let ranked = self.ranked_courses(tag);
        let Some(&(best, _)) = ranked.first() else {
            return vec![];
        };
        ranked
            .into_iter()
            .take_while(|(score, _)| *score == best)
            .map(|(_, id)| id)
            .collect()
    }

    /// Whether a course's contents belong in the search at all.
    fn searchable_course(&self, course_id: &str) -> bool {
        self.search_hidden || !self.hidden_courses.contains(course_id)
    }

    /// Include or exclude everything in hidden courses, and remember it.
    fn toggle_search_hidden(&mut self) {
        self.search_hidden = !self.search_hidden;
        if let Some(store) = &self.store {
            store.save_search_hidden(self.search_hidden);
        }
        if let Some(palette) = &mut self.palette {
            palette.selected = 0;
        }
        self.dirty = true;
    }

    /// The results to draw, best first.
    ///
    /// `@tag` narrows to the courses it names before anything is ranked, so
    /// the words after it compete only inside those courses.
    pub fn palette_hits(&self) -> Vec<crate::search::Hit> {
        let Some(palette) = &self.palette else {
            return vec![];
        };
        let query = crate::search::parse(&palette.query);
        let pool = self.palette_pool();
        // A tag still being typed: offer the courses that would finish it,
        // rather than the results of a class nobody has named yet.
        if query.tagging && query.text.is_empty() {
            let partial = query.tags.last().cloned().unwrap_or_default();
            // Every class the tag could mean, best first, so a tie between
            // two of them is on screen rather than silently resolved.
            let ranked = self.ranked_courses(&partial);
            let courses: Vec<crate::search::Hit> = pool
                .into_iter()
                .filter(|h| matches!(h.kind, crate::search::Kind::Course))
                .collect();
            return ranked
                .into_iter()
                .filter_map(|(_, id)| {
                    courses
                        .iter()
                        .find(|h| h.target.course() == Some(id.as_str()))
                        .cloned()
                })
                .take(PALETTE_RESULTS)
                .collect();
        }
        if query.tags.is_empty() {
            return crate::search::rank(&query.text, pool, PALETTE_RESULTS);
        }
        let wanted: Vec<String> = query
            .tags
            .iter()
            .flat_map(|t| self.tagged_courses(t))
            .collect();
        let narrowed: Vec<crate::search::Hit> = pool
            .into_iter()
            .filter(|h| {
                h.target
                    .course()
                    .is_some_and(|c| wanted.iter().any(|w| w == c))
            })
            .collect();
        crate::search::rank(&query.text, narrowed, PALETTE_RESULTS)
    }

    /// What the palette says under the box: the class it is narrowed to, and
    /// how much of the account it can see.
    ///
    /// A search that cannot find a course is confusing unless it admits it
    /// has not read everything yet, so the count stays on screen.
    pub fn palette_coverage(&self) -> String {
        let query = self
            .palette
            .as_ref()
            .map(|p| crate::search::parse(&p.query))
            .unwrap_or_default();
        if !query.tags.is_empty() {
            let names: Vec<String> = query
                .tags
                .iter()
                .flat_map(|t| self.tagged_courses(t))
                .map(|id| self.course_title(&id))
                .collect();
            return match names.len() {
                0 => format!(
                    "@{} matches no class you can search",
                    query.tags.join(", @")
                ),
                1 => format!("in {}", names[0]),
                n => format!("in {} and {} more", names[0], n - 1),
            };
        }
        let courses = self
            .courses
            .data
            .as_deref()
            .unwrap_or(&[])
            .iter()
            .filter(|c| self.searchable_course(&c.id))
            .count();
        let read = self
            .assignments
            .iter()
            .filter(|(id, r)| r.data.is_some() && self.searchable_course(id))
            .count();
        let hidden = if self.search_hidden {
            ", hidden included"
        } else {
            ""
        };
        if self.warm_queue.is_empty() && self.warm_running == 0 && read >= courses {
            format!("searching {courses} courses{hidden} \u{b7} @ a class to narrow")
        } else {
            format!("searching {read} of {courses} courses{hidden}, still reading")
        }
    }

    fn palette_act(&mut self, act: Act) -> Vec<Effect> {
        let count = self.palette_hits().len();
        let Some(palette) = self.palette.as_mut() else {
            return vec![];
        };
        match act {
            Act::Up => palette.selected = palette.selected.saturating_sub(1),
            Act::Down => palette.selected = (palette.selected + 1).min(count.saturating_sub(1)),
            Act::InputClear => {
                palette.query.clear();
                palette.selected = 0;
            }
            Act::InputCancel | Act::Palette => self.palette = None,
            Act::ShowHidden => self.toggle_search_hidden(),
            Act::Open => {
                let selected = palette.selected;
                let Some(hit) = self.palette_hits().into_iter().nth(selected) else {
                    return vec![];
                };
                self.palette = None;
                self.dirty = true;
                return self.open_hit(hit.target);
            }
            _ => {}
        }
        self.dirty = true;
        vec![]
    }

    /// Go where a result points. It opens on top of wherever the search was
    /// opened from, so Esc comes back to where you were.
    fn open_hit(&mut self, target: crate::search::Target) -> Vec<Effect> {
        use crate::search::Target;
        let next = match target {
            Target::Action(act) => {
                let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
                return self.perform(act, &enter);
            }
            Target::Course(id) => {
                let title = self.course_title(&id);
                // Open where the saved tabs say, before Canvas is asked again.
                self.load_snapshot(&ResourceKey::Tabs(id.clone()));
                self.course_view(&id, &title)
            }
            Target::Assignment(course_id, assignment_id) => {
                let title = self
                    .assignments
                    .get(&course_id)
                    .and_then(|r| r.data.as_ref())
                    .and_then(|list| list.iter().find(|a| a.id == assignment_id))
                    .map(|a| a.title().to_string())
                    .unwrap_or_default();
                ViewKind::Detail {
                    course_name: self.course_title(&course_id),
                    course_id,
                    assignment_id,
                    title,
                }
            }
            Target::Announcement(course_id, announcement_id) => {
                let title = self
                    .announcements
                    .get(&course_id)
                    .and_then(|r| r.data.as_ref())
                    .and_then(|list| list.iter().find(|a| a.id == announcement_id))
                    .map(|a| a.title().to_string())
                    .unwrap_or_default();
                ViewKind::Announcement {
                    course_name: self.course_title(&course_id),
                    course_id,
                    announcement_id,
                    title,
                }
            }
            Target::Page(course_id, page_url) => {
                let title = self
                    .page_lists
                    .get(&course_id)
                    .and_then(|r| r.data.as_ref())
                    .and_then(|list| list.iter().find(|p| p.slug() == Some(page_url.as_str())))
                    .map(|p| p.title().to_string())
                    .unwrap_or_default();
                ViewKind::WikiPage {
                    course_name: self.course_title(&course_id),
                    course_id,
                    page_url,
                    title,
                }
            }
            Target::Conversation(conversation_id) => {
                let subject = self
                    .conversations
                    .values()
                    .filter_map(|r| r.data.as_ref())
                    .flatten()
                    .find(|c| c.id == conversation_id)
                    .map(|c| c.subject().to_string())
                    .unwrap_or_default();
                ViewKind::Conversation {
                    conversation_id,
                    subject,
                }
            }
            Target::File(course_id, file_id) => {
                // A file is not a screen. Open the course's Files at the
                // folder holding it, with it selected, which is where every
                // other way of reaching a file lands.
                let folder_id = self
                    .course_files
                    .get(&course_id)
                    .and_then(|r| r.data.as_ref())
                    .and_then(|d| d.files.as_ref())
                    .and_then(|files| files.iter().find(|f| f.id == file_id))
                    .and_then(|f| f.folder_id.clone());
                ViewKind::Files {
                    course_name: self.course_title(&course_id),
                    course_id,
                    folder_id,
                }
            }
        };
        self.stack.push(View::new(next));
        self.enter_current(false)
    }
    fn menu_act(&mut self, act: Act) -> Vec<Effect> {
        let count = self.menu_entries().len();
        let Some(menu) = self.menu.as_mut() else {
            return vec![];
        };
        match act {
            Act::Up => menu.selected = menu.selected.saturating_sub(1),
            Act::Down => menu.selected = (menu.selected + 1).min(count.saturating_sub(1)),
            Act::Search => menu.search = Some(String::new()),
            Act::InputCancel => {
                menu.search = None;
                menu.selected = 0;
            }
            Act::Back | Act::Menu => self.menu = None,
            Act::Help => {
                self.menu = None;
                self.help = Some(0);
            }
            Act::Open => {
                let selected = menu.selected;
                let chosen = self.menu_entries().get(selected).map(|e| e.act);
                self.menu = None;
                if let Some(act) = chosen {
                    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
                    return self.perform(act, &enter);
                }
            }
            _ => {}
        }
        vec![]
    }

    fn submission_act(&mut self, act: Act) -> Vec<Effect> {
        let kind = self.view().kind.clone();
        let draft = self.submission.as_mut().expect("checked by the caller");
        if draft.busy {
            return vec![];
        }
        if draft.reviewing {
            match act {
                Act::InputCancel => draft.reviewing = false,
                Act::Confirm => {
                    if let ViewKind::Detail {
                        course_id,
                        assignment_id,
                        ..
                    } = &kind
                    {
                        draft.busy = true;
                        return vec![match draft.kind {
                            DraftKind::Files => Effect::Submit {
                                course_id: course_id.clone(),
                                assignment_id: assignment_id.clone(),
                                files: draft.files.clone(),
                            },
                            kind => Effect::SubmitContent {
                                course_id: course_id.clone(),
                                assignment_id: assignment_id.clone(),
                                kind,
                                content: draft.content.clone(),
                            },
                        }];
                    }
                }
                _ => {}
            }
            return vec![];
        }
        if draft.kind != DraftKind::Files && !draft.from_file {
            match act {
                Act::InputCancel => self.submission = None,
                Act::InputClear => draft.input.clear(),
                Act::LoadTextFile if draft.kind == DraftKind::Text => {
                    draft.from_file = true;
                    draft.input = draft.start_directory.clone();
                    draft.suggestions.update(&draft.input);
                }
                Act::InputDone => {
                    let chosen = match draft.kind {
                        DraftKind::Url => submission::choose_url(&draft.input),
                        _ => submission::choose_text(&draft.input),
                    };
                    match chosen {
                        Ok(content) => {
                            draft.content = content;
                            draft.source = None;
                            draft.reviewing = true;
                        }
                        Err(reason) => self.notice = Some(reason),
                    }
                }
                _ => {}
            }
            return vec![];
        }
        match act {
            Act::InputCancel if draft.from_file => {
                // Back to typing rather than out of the draft.
                draft.from_file = false;
                draft.input = draft.content.clone();
            }
            Act::InputCancel => self.submission = None,
            Act::InputClear => {
                draft.input.clear();
                draft.suggestions.update(&draft.input);
            }
            Act::Up => draft.suggestions.move_selection(-1),
            Act::Down => draft.suggestions.move_selection(1),
            Act::InputComplete => {
                if let Some(path) = draft.suggestions.complete() {
                    draft.input = path;
                    draft.suggestions.update(&draft.input);
                }
            }
            Act::InputDone if draft.from_file => match submission::read_text_file(&draft.input) {
                Ok((text, path)) => {
                    draft.content = text;
                    draft.source = Some(path);
                    draft.from_file = false;
                    draft.reviewing = true;
                }
                Err(reason) => self.notice = Some(reason),
            },
            Act::InputDone => {
                if draft.input.trim().is_empty() || draft.input == draft.start_directory {
                    if draft.files.is_empty() {
                        self.notice = Some("Add a file path first.".into());
                    } else {
                        draft.reviewing = true;
                    }
                } else if let ViewKind::Detail {
                    course_id,
                    assignment_id,
                    ..
                } = &kind
                    && let Some(detail) = self
                        .details
                        .get(&(course_id.clone(), assignment_id.clone()))
                        .and_then(|r| r.data.as_ref())
                {
                    match submission::choose_file(&draft.input, detail, &draft.files) {
                        Ok(file) => {
                            draft.files.push(file);
                            draft.input = draft.start_directory.clone();
                            draft.suggestions.update(&draft.input);
                        }
                        Err(reason) => self.notice = Some(reason),
                    }
                }
            }
            _ => {}
        }
        vec![]
    }

    /// Moving: rows in a list, lines of a page's text, or the entries of its
    /// focused section.
    fn navigate(&mut self, act: Act) -> Vec<Effect> {
        if self.view().kind == ViewKind::Courses && self.focus() == Section::Calendar {
            return self.move_day(act);
        }
        if matches!(self.view().kind, ViewKind::Inbox { .. })
            && matches!(act, Act::Left | Act::Right)
        {
            return self.move_folder(act == Act::Right);
        }
        // On a home-screen widget the arrows move between widgets. There is
        // one for now, so they do nothing; what they must not do is reach the
        // course list behind it.
        if self.view().kind == ViewKind::Courses && self.focus() == Section::Inbox {
            return vec![];
        }
        if matches!(act, Act::Left | Act::Right) {
            return vec![];
        }
        let page = self.page() as isize;
        let delta = match act {
            Act::Up => -1,
            Act::Down => 1,
            Act::PageUp => -page,
            Act::PageDown => page,
            Act::Top => isize::MIN / 2,
            _ => isize::MAX / 2,
        };
        if self.focus() == Section::Day {
            let n = self.day_items(self.calendar_day).len();
            self.day_item =
                (self.day_item as isize + delta).clamp(0, n.saturating_sub(1) as isize) as usize;
            return vec![];
        }
        if matches!(self.view().kind, ViewKind::Inbox { .. }) {
            let n = self.inbox_rows().len();
            self.inbox_item =
                (self.inbox_item as isize + delta).clamp(0, n.saturating_sub(1) as isize) as usize;
            return vec![];
        }
        if self.is_list() {
            self.move_selection(delta);
            return vec![];
        }
        let count = match (self.focus(), self.view().kind.page()) {
            (Section::Links, _) => self.current_links().len(),
            (Section::Submissions, Some(Page::Assignment(c, a))) => self.attempt_count(&c, &a),
            (Section::Feedback, Some(Page::Assignment(c, a))) => {
                self.feedback_entries(&c, &a, self.view().attempt).len()
            }
            _ => 0,
        };
        let focus = self.focus();
        let view = self.view_mut();
        let step =
            |at: usize| (at as isize + delta).clamp(0, count.saturating_sub(1) as isize) as usize;
        match focus {
            Section::Text => view.scroll = (view.scroll as isize + delta).max(0) as usize,
            Section::Links => view.link = step(view.link),
            Section::Submissions => {
                let next = step(view.attempt);
                if next != view.attempt {
                    view.attempt = next;
                    view.feedback = 0;
                }
            }
            Section::Feedback => view.feedback = step(view.feedback),
            Section::Calendar | Section::Day | Section::Inbox => {}
        }
        vec![]
    }

    /// Arrows on the month grid: a day left/right, a week up/down, a month
    /// with PgUp/PgDn, today with Home. A new month is read when shown.
    fn move_day(&mut self, act: Act) -> Vec<Effect> {
        use crate::calendar::shift_month;
        let day = self.calendar_day;
        let today = chrono::Local::now().date_naive();
        let next = match act {
            Act::Left => day - chrono::Duration::days(1),
            Act::Right => day + chrono::Duration::days(1),
            Act::Up => day - chrono::Duration::days(7),
            Act::Down => day + chrono::Duration::days(7),
            Act::PageUp => shift_month(day, -1),
            Act::PageDown => shift_month(day, 1),
            Act::Top => today,
            _ => day,
        };
        let new_month = crate::calendar::month_key(next) != crate::calendar::month_key(day);
        self.calendar_day = next;
        self.day_item = 0;
        if new_month {
            self.read_calendar(false)
        } else {
            vec![]
        }
    }

    /// Read the month shown (and the course colours) for the home screen.
    fn read_calendar(&mut self, explicit: bool) -> Vec<Effect> {
        let month = ResourceKey::Planner(crate::calendar::month_key(self.calendar_day));
        self.load_snapshot(&month);
        self.load_snapshot(&ResourceKey::Colors);
        let mut effects: Vec<Effect> = self.request(month, explicit).into_iter().collect();
        effects.extend(self.request(ResourceKey::Colors, explicit));
        effects
    }

    /// Read the Inbox folder shown on the home screen.
    fn read_inbox(&mut self, explicit: bool) -> Vec<Effect> {
        let key = ResourceKey::Conversations(self.inbox_folder.id().to_string());
        self.load_snapshot(&key);
        self.request(key, explicit).into_iter().collect()
    }

    /// Read the Unread folder, which is all the home screen's widget needs.
    fn read_unread(&mut self) -> Vec<Effect> {
        let key = ResourceKey::Conversations(crate::inbox::Folder::Unread.id().to_string());
        self.load_snapshot(&key);
        self.request(key, false).into_iter().collect()
    }

    /// The Unread folder's resource, for the widget's own loading state.
    pub fn unread_resource(&self) -> Option<&Resource<Vec<crate::inbox::Summary>>> {
        self.conversations.get(crate::inbox::Folder::Unread.id())
    }

    /// How many conversations are unread, for the home screen's widget.
    ///
    /// Canvas's own unread set, less the ones read here. Reading a
    /// conversation in canter deliberately tells Canvas nothing, so
    /// Canvas's count alone would never go down as you work, and this number
    /// is allowed to disagree with the one Canvas shows for that reason.
    pub fn unread_conversations(&self) -> usize {
        self.conversations
            .get(crate::inbox::Folder::Unread.id())
            .and_then(|r| r.data.as_deref())
            .unwrap_or(&[])
            .iter()
            .filter(|c| !self.conversation_reads.contains(&c.id))
            .count()
    }

    /// The conversations of the folder shown, newest activity first.
    pub fn inbox_rows(&self) -> &[crate::inbox::Summary] {
        self.conversations
            .get(self.inbox_folder.id())
            .and_then(|r| r.data.as_deref())
            .unwrap_or(&[])
    }

    pub fn inbox_resource(&self) -> Option<&Resource<Vec<crate::inbox::Summary>>> {
        self.conversations.get(self.inbox_folder.id())
    }

    pub fn selected_conversation(&self) -> Option<&crate::inbox::Summary> {
        self.inbox_rows().get(self.inbox_item)
    }

    /// `←`/`→` on the Inbox: another folder, read when it is shown.
    fn move_folder(&mut self, forward: bool) -> Vec<Effect> {
        self.inbox_folder = self.inbox_folder.step(forward);
        self.inbox_item = 0;
        // The view has to agree, because it is what names the resource the
        // screen shows and what the saved copy is looked up by.
        let now = self.inbox_folder;
        if let ViewKind::Inbox { folder } = &mut self.view_mut().kind {
            *folder = now;
        }
        self.read_inbox(false)
    }

    /// `Enter` on the home screen's box: the Inbox itself, at whichever
    /// folder was last looked at.
    fn open_inbox(&mut self) -> Vec<Effect> {
        self.inbox_item = 0;
        self.stack.push(View::new(ViewKind::Inbox {
            folder: self.inbox_folder,
        }));
        self.enter_current(false)
    }

    /// `Enter` on the Inbox: open the selected conversation here.
    fn open_conversation(&mut self) -> Vec<Effect> {
        let Some(conversation) = self.selected_conversation() else {
            return vec![];
        };
        let next = ViewKind::Conversation {
            conversation_id: conversation.id.clone(),
            subject: conversation.subject().to_string(),
        };
        self.stack.push(View::new(next));
        self.enter_current(false)
    }

    /// Next or previous section of a page; the text is always first.
    fn cycle_section(&mut self, forward: bool) {
        let sections = self.sections();
        if sections.len() < 2 {
            return;
        }
        let at = sections
            .iter()
            .position(|s| *s == self.focus())
            .unwrap_or(0);
        let n = sections.len();
        let next = sections[if forward {
            (at + 1) % n
        } else {
            (at + n - 1) % n
        }];
        let links = self.current_links().len();
        let entries = match self.view().kind.page() {
            Some(Page::Assignment(c, a)) => {
                self.feedback_entries(&c, &a, self.view().attempt).len()
            }
            _ => 0,
        };
        let view = self.view_mut();
        view.focus = next;
        view.reveal = next != Section::Text;
        if view.link >= links {
            view.link = 0;
        }
        if view.feedback >= entries {
            view.feedback = 0;
        }
        if next == Section::Feedback {
            self.note_feedback_read();
        }
    }

    fn jump_to_link(&mut self, n: usize) {
        if n >= 1 && n <= self.current_links().len() {
            let view = self.view_mut();
            view.focus = Section::Links;
            view.link = n - 1;
            view.reveal = true;
        }
    }

    /// `Esc`: leave a section, stop showing hidden courses, or go back.
    fn go_back(&mut self) -> Vec<Effect> {
        if self.view().kind == ViewKind::Courses && self.focus() == Section::Day {
            self.view_mut().focus = Section::Calendar;
            return vec![];
        }
        if (self.view().kind.page().is_some() || self.view().kind == ViewKind::Courses)
            && self.view().focus != Section::Text
        {
            self.view_mut().focus = Section::Text;
            return vec![];
        }
        if self.view().kind == ViewKind::Courses && self.show_hidden {
            self.show_hidden = false;
            self.clamp_selection();
            return vec![];
        }
        self.back()
    }

    fn back(&mut self) -> Vec<Effect> {
        if self.stack.len() <= 1 {
            return vec![];
        }
        let left = self.stack.pop().expect("checked above");
        // Going up a folder moves within data already read for this visit.
        if let (ViewKind::Files { course_id: a, .. }, ViewKind::Files { course_id: b, .. }) =
            (&left.kind, &self.view().kind)
            && a == b
        {
            self.dirty = true;
            return vec![];
        }
        // Returning to a view is a new access: show what we have, sync again.
        self.enter_current(false)
    }

    /// Short label for a course: its code, else its name.
    pub fn course_code(&self, course_id: &str) -> String {
        self.courses
            .data
            .as_deref()
            .unwrap_or(&[])
            .iter()
            .find(|c| c.id == course_id)
            .map(|c| {
                c.course_code
                    .clone()
                    .unwrap_or_else(|| c.title().to_string())
            })
            .unwrap_or_default()
    }

    /// The month shown on the calendar.
    pub fn calendar_month(&self) -> Option<&Resource<Vec<crate::calendar::Item>>> {
        self.planner
            .get(&crate::calendar::month_key(self.calendar_day))
    }

    /// One day's entries, in time order.
    pub fn day_items(&self, day: chrono::NaiveDate) -> Vec<crate::calendar::Item> {
        self.calendar_items()
            .into_iter()
            .filter(|i| i.day() == Some(day))
            .collect()
    }

    /// The shown month's entries, in date order, without hidden courses.
    pub fn calendar_items(&self) -> Vec<crate::calendar::Item> {
        self.calendar_month()
            .and_then(|r| r.data.as_deref())
            .unwrap_or(&[])
            .iter()
            .filter(|i| i.listed())
            .filter(|i| {
                self.show_hidden
                    || i.course_id
                        .as_ref()
                        .is_none_or(|c| !self.hidden_courses.contains(c))
            })
            .cloned()
            .collect()
    }

    /// The Canvas page of a calendar entry, kept on the Canvas origin.
    fn calendar_url(&self, item: &crate::calendar::Item) -> String {
        match item.url.as_deref() {
            Some(path) if path.starts_with('/') && !path.starts_with("//") => {
                format!("{}{path}", self.origin)
            }
            Some(url) if url.starts_with(&format!("{}/", self.origin)) => url.to_string(),
            _ => match &item.course_id {
                Some(c) => format!("{}/courses/{c}", self.origin),
                None => format!("{}/calendar", self.origin),
            },
        }
    }

    /// `Enter` on the calendar: on a day with several entries, choose one;
    /// assignments open in the reader, anything else in the browser.
    fn open_calendar_item(&mut self) -> Vec<Effect> {
        let items = self.day_items(self.calendar_day);
        let item = match (self.focus(), items.len()) {
            (_, 0) => return vec![],
            (Section::Calendar, n) if n > 1 => {
                self.view_mut().focus = Section::Day;
                self.day_item = 0;
                return vec![];
            }
            (Section::Day, _) => items[self.day_item.min(items.len() - 1)].clone(),
            _ => items[0].clone(),
        };
        if item.kind == crate::calendar::Kind::Assignment
            && let Some(course_id) = item.course_id.clone()
        {
            let course_name = self.course_title(&course_id);
            self.stack.push(View::new(ViewKind::Detail {
                course_id,
                course_name,
                assignment_id: item.id.clone(),
                title: item.title.clone(),
            }));
            return self.enter_current(false);
        }
        vec![Effect::OpenUrl(self.calendar_url(&item))]
    }

    /// `r`: read this screen again now (and an assignment's past submissions,
    /// or a course's tabs).
    fn refresh(&mut self) -> Vec<Effect> {
        let key = self.view().key();
        let mut effects: Vec<Effect> = match key {
            ResourceKey::Tabs(_) => vec![],
            key => self.request(key, true).into_iter().collect(),
        };
        if let Some((course_id, _)) = self.view().kind.course() {
            let course_id = course_id.to_string();
            effects.extend(self.read_tabs(&course_id, true));
        }
        if self.view().kind == ViewKind::Courses {
            effects.extend(self.read_calendar(true));
            effects.extend(self.read_inbox(true));
        }
        if let ViewKind::Detail {
            course_id,
            assignment_id,
            ..
        } = self.view().kind.clone()
            && (self.was_submitted(&course_id, &assignment_id)
                || self
                    .histories
                    .contains_key(&(course_id.clone(), assignment_id.clone())))
        {
            effects.extend(self.request(ResourceKey::History(course_id, assignment_id), true));
        }
        effects
    }

    /// `Enter` on a page: open the selected file or link.
    fn open_link(&mut self, page: &Page) -> Vec<Effect> {
        let Some(link) = self.selected_link() else {
            return vec![];
        };
        match page {
            Page::Assignment(c, a) => self.activate_link(c, a, link),
            Page::Announcement(c, a) => self.activate_announcement_link(c, a, link),
            Page::Wiki(c, u) => match &link.target {
                Target::File { id } => {
                    let (id, dir) = (id.clone(), self.wiki_dir(c, u));
                    self.place_announcement_file(&id, dir, true)
                }
                _ => self.activate_link(c, "", link),
            },
            // A conversation belongs to no course, so a file in it is placed
            // by the same lazy-metadata path as an announcement's.
            Page::Conversation(id) => match &link.target {
                Target::File { id: file_id } => {
                    let (file_id, dir) = (file_id.clone(), self.conversation_dir(id));
                    self.place_announcement_file(&file_id, dir, true)
                }
                _ => self.activate_link("", "", link),
            },
        }
    }

    /// `d`: download the selected file (or, in Files, folder).
    fn download_selected(&mut self) -> Vec<Effect> {
        if self.focus() == Section::Feedback {
            let files = self.selected_feedback_files();
            if files.is_empty() {
                self.notice = Some(self.no_feedback_files());
                return vec![];
            }
            return self.place_feedback_files(files, false);
        }
        if let ViewKind::Files {
            course_id,
            course_name,
            ..
        } = self.view().kind.clone()
        {
            let rows = self.file_rows();
            return match rows.get(self.view().selected).copied() {
                Some(FileRow::File(f)) => {
                    let id = f.id.clone();
                    let dir = self.file_dir(&course_id, &course_name, f);
                    self.place_from(&id, dir, false)
                }
                Some(FileRow::Folder(f)) => {
                    let id = f.id.clone();
                    self.download_folder(&course_id, &course_name, Some(&id))
                }
                None => vec![],
            };
        }
        if let ViewKind::Modules { course_id, .. } = self.view().kind.clone() {
            return match self.selected_module_row() {
                Some(ModuleRow::Item(m, i)) if i.kind() == ItemKind::File => {
                    match i.content().map(str::to_string) {
                        Some(id) => {
                            let module = m.name().to_string();
                            self.place_module_file(&course_id, &module, &id, false, false)
                        }
                        None => vec![],
                    }
                }
                _ => {
                    self.notice = Some("That item is not a file.".into());
                    vec![]
                }
            };
        }
        let Some(page) = self.view().kind.page() else {
            return vec![];
        };
        match self.selected_link().map(|l| l.target) {
            Some(Target::File { id }) => {
                let dir = self.page_dir(&page);
                match page {
                    Page::Assignment(..) => self.place_from(&id, dir, false),
                    Page::Announcement(..) | Page::Wiki(..) | Page::Conversation(..) => {
                        self.place_announcement_file(&id, dir, false)
                    }
                }
            }
            Some(_) => {
                self.notice = Some("That link is not a file.".into());
                vec![]
            }
            None => {
                self.notice = Some("Choose a file first: Tab to the files & links.".into());
                vec![]
            }
        }
    }

    /// `D`: download every file in this folder, or every file of this page.
    fn download_everything(&mut self) -> Vec<Effect> {
        if self.focus() == Section::Feedback {
            let files = self.feedback_files();
            if files.is_empty() {
                self.notice = Some("This feedback has no files.".into());
                return vec![];
            }
            return self.place_feedback_files(files, false);
        }
        if let ViewKind::Files {
            course_id,
            course_name,
            folder_id,
        } = self.view().kind.clone()
        {
            return self.download_folder(&course_id, &course_name, folder_id.as_deref());
        }
        if let ViewKind::Modules { course_id, .. } = self.view().kind.clone() {
            let files: Vec<(String, String)> = self
                .visible_rows()
                .iter()
                .filter_map(|&i| match self.module_rows(&course_id).get(i).copied() {
                    Some(ModuleRow::Item(m, item)) if item.kind() == ItemKind::File => {
                        Some((m.name().to_string(), item.content()?.to_string()))
                    }
                    _ => None,
                })
                .collect();
            if files.is_empty() {
                self.notice = Some("There are no files here.".into());
                return vec![];
            }
            let count = files.len();
            let mut effects = Vec::new();
            for (module, id) in files {
                effects.extend(self.place_module_file(&course_id, &module, &id, false, true));
            }
            self.notice = Some(format!("Checking {count} module files before downloading…"));
            return effects;
        }
        let Some(page) = self.view().kind.page() else {
            return vec![];
        };
        let ids: Vec<String> = self
            .page_links(&page)
            .iter()
            .filter_map(|l| match &l.target {
                Target::File { id } => Some(id.clone()),
                _ => None,
            })
            .collect();
        if ids.is_empty() {
            self.notice = Some("There are no files here.".into());
            return vec![];
        }
        let Some(dir) = self.page_dir(&page) else {
            self.notice = Some("The file library is unavailable.".into());
            return vec![];
        };
        match page {
            Page::Assignment(..) => {
                let batch = ids.into_iter().map(|id| (id, dir.clone())).collect();
                self.download_batch(batch)
            }
            Page::Announcement(..) | Page::Wiki(..) | Page::Conversation(..) => {
                let mut effects = Vec::new();
                for id in ids {
                    effects.extend(self.place_announcement_file(&id, Some(dir.clone()), false));
                }
                self.notice = Some("Checking files before downloading…".into());
                effects
            }
        }
    }

    /// `e`: show the selected file, or this place's folder, in File Explorer.
    fn show_folder(&mut self) -> Vec<Effect> {
        let path = match self.view().kind.clone() {
            ViewKind::Courses => {
                let course = self
                    .visible_rows()
                    .get(self.view().selected)
                    .and_then(|&i| self.courses.data.as_ref()?.get(i))
                    .map(|c| (c.id.clone(), c.title().to_string()));
                match (course, self.library.as_ref()) {
                    (Some((id, title)), Some(lib)) => {
                        Some(lib.root().join(self.course_dir(&id, &title)))
                    }
                    (None, Some(lib)) => Some(lib.root().to_path_buf()),
                    _ => None,
                }
            }
            ViewKind::Files {
                course_id,
                course_name,
                folder_id,
            } => {
                let rows = self.file_rows();
                let saved = match rows.get(self.view().selected) {
                    Some(FileRow::File(f)) => self.saved_path(&f.id),
                    _ => None,
                };
                saved.or_else(|| self.class_dir(&course_id, &course_name, folder_id.as_deref()))
            }
            ViewKind::Modules {
                course_id,
                course_name,
            } => {
                let saved = match self.selected_module_row() {
                    Some(ModuleRow::Item(_, i)) if i.kind() == ItemKind::File => {
                        i.content().and_then(|id| self.saved_path(id))
                    }
                    _ => None,
                };
                saved.or_else(|| {
                    let lib = self.library.as_ref()?;
                    Some(lib.root().join(self.course_dir(&course_id, &course_name)))
                })
            }
            kind => {
                let saved = match self.selected_link().map(|l| l.target) {
                    Some(Target::File { id }) => self.saved_path(&id),
                    _ => None,
                };
                saved.or_else(|| kind.page().and_then(|p| self.page_dir(&p)))
            }
        };
        self.reveal(path)
    }

    fn toggle_selected_course_hidden(&mut self) {
        let Some(&index) = self.visible_rows().get(self.view().selected) else {
            return;
        };
        let Some(course) = self.courses.data.as_ref().and_then(|c| c.get(index)) else {
            return;
        };
        let id = course.id.clone();
        let title = course.title().to_string();
        if self.hidden_courses.remove(&id) {
            self.notice = Some(format!("Showing {title}."));
        } else {
            self.hidden_courses.insert(id);
            self.notice = Some(format!("Hid {title}."));
        }
        if let Some(store) = &self.store {
            store.save_hidden_courses(&self.hidden_courses);
        }
        self.clamp_selection();
    }

    /// Cycle the course sections Canvas shows; leaving Files leaves its
    /// nested folders.
    fn switch_tab(&mut self, backwards: bool) -> Vec<Effect> {
        let Some((tab, course_id, course_name)) = self
            .view()
            .kind
            .tab()
            .map(|(t, c, n)| (t, c.to_string(), n.to_string()))
        else {
            return vec![];
        };
        let tabs = self.course_tabs(&course_id);
        let Some(&first) = tabs.first() else {
            return vec![];
        };
        let next = match tabs.iter().position(|t| *t == tab) {
            Some(i) => tabs[(i + if backwards { tabs.len() - 1 } else { 1 }) % tabs.len()],
            None => first,
        };
        if next == tab {
            return vec![];
        }
        self.replace_tab(next.view(course_id, course_name))
    }

    fn reveal(&mut self, path: Option<PathBuf>) -> Vec<Effect> {
        match path {
            Some(p) => vec![Effect::Reveal(p)],
            None => {
                self.notice = Some("The file library is unavailable.".into());
                vec![]
            }
        }
    }

    pub fn announcement_status(
        &self,
        course_id: &str,
        summary: &crate::announcements::Summary,
    ) -> &'static str {
        if self
            .announcement_reads
            .contains(&(course_id.into(), summary.id.clone()))
        {
            "Read"
        } else {
            summary.status()
        }
    }

    fn mark_current_announcement_read(&mut self) {
        let ViewKind::Announcement {
            course_id,
            announcement_id,
            ..
        } = &self.view().kind
        else {
            return;
        };
        let key = (course_id.clone(), announcement_id.clone());
        let readable = self
            .announcement_details
            .get(&key)
            .and_then(|r| r.data.as_ref())
            .is_some_and(|d| d.can_view() && d.message.is_some());
        if readable && self.announcement_reads.insert(key) {
            if let Some(store) = &self.store {
                store.save_announcement_reads(&self.announcement_reads);
            }
        }
    }

    /// What the Inbox shows for a conversation: read here, else Canvas's own
    /// state. Reading here never changes Canvas's.
    pub fn conversation_status(&self, summary: &crate::inbox::Summary) -> &'static str {
        if self.conversation_reads.contains(&summary.id) {
            "Read"
        } else {
            summary.status()
        }
    }

    /// An open conversation whose messages are on hand counts as read. This
    /// is recorded locally only; Canvas is never told.
    fn mark_current_conversation_read(&mut self) {
        let ViewKind::Conversation {
            conversation_id, ..
        } = &self.view().kind
        else {
            return;
        };
        let id = conversation_id.clone();
        let readable = self
            .conversation_details
            .get(&id)
            .and_then(|r| r.data.as_ref())
            .is_some_and(|d| !d.messages.is_empty());
        if readable
            && self.conversation_reads.insert(id)
            && let Some(store) = &self.store
        {
            store.save_conversation_reads(&self.conversation_reads);
        }
    }

    /// The conversation as one HTML document: the text and the files & links
    /// list are built from the same document, so their numbering agrees.
    pub fn conversation_document(&self, id: &str) -> String {
        self.conversation_details
            .get(id)
            .and_then(|r| r.data.as_ref())
            .map(crate::inbox::Detail::document)
            .unwrap_or_default()
    }

    pub fn conversation_links(&self, id: &str) -> Vec<Link> {
        let Some(detail) = self
            .conversation_details
            .get(id)
            .and_then(|r| r.data.as_ref())
        else {
            return vec![];
        };
        let mut links = crate::html::links(&detail.document(), &self.origin);
        for f in detail.attachments() {
            if crate::model::valid_id(&f.id)
                && !links
                    .iter()
                    .any(|l| matches!(&l.target, Target::File {id} if id == &f.id))
            {
                links.push(Link {
                    text: crate::html::sanitize(f.name()),
                    url: format!("{}/files/{}", self.origin, f.id),
                    target: Target::File { id: f.id.clone() },
                    image: false,
                });
            }
        }
        links
    }

    /// Where a conversation's files go. Conversations are not a course's, so
    /// they sit in one folder of their own under the library root.
    pub fn conversation_dir(&self, id: &str) -> Option<PathBuf> {
        let lib = self.library.as_ref()?;
        if !crate::model::valid_id(id) {
            return None;
        }
        Some(lib.root().join("Inbox Files").join(id))
    }

    pub fn announcement_links(&self, course_id: &str, id: &str) -> Vec<Link> {
        let Some(detail) = self
            .announcement_details
            .get(&(course_id.into(), id.into()))
            .and_then(|r| r.data.as_ref())
        else {
            return vec![];
        };
        if !detail.can_view() {
            return vec![];
        }
        let mut links = crate::html::links(detail.message.as_deref().unwrap_or(""), &self.origin);
        for f in &detail.attachments {
            if crate::model::valid_id(&f.id)
                && !links
                    .iter()
                    .any(|l| matches!(&l.target, Target::File {id} if id == &f.id))
            {
                links.push(Link {
                    text: crate::html::sanitize(f.name()),
                    url: format!("{}/courses/{course_id}/files/{}", self.origin, f.id),
                    target: Target::File { id: f.id.clone() },
                    image: false,
                });
            }
        }
        links
    }

    pub fn announcement_dir(&self, course_id: &str, id: &str) -> Option<PathBuf> {
        let lib = self.library.as_ref()?;
        Some(
            lib.root()
                .join(self.course_dir(course_id, &self.course_title(course_id)))
                .join("Announcement Files")
                .join(id),
        )
    }

    /// File metadata is lazy for announcements; validate it when a file is
    /// chosen so a saved URL or version never suppresses this fresh read.
    fn place_announcement_file(
        &mut self,
        id: &str,
        dir: Option<PathBuf>,
        open: bool,
    ) -> Vec<Effect> {
        if dir.is_none() {
            return self.place_from(id, dir, open);
        }
        let mut effects: Vec<_> = self
            .request(ResourceKey::File(id.into()), false)
            .into_iter()
            .collect();
        effects.extend(self.place_from(id, dir, open));
        effects
    }

    fn activate_announcement_link(&mut self, course_id: &str, id: &str, link: Link) -> Vec<Effect> {
        if let Target::File { id: file_id } = &link.target {
            let dir = self.announcement_dir(course_id, id);
            self.place_announcement_file(file_id, dir, true)
        } else {
            self.activate_link(course_id, id, link)
        }
    }

    /// Files and links in a wiki page.
    pub fn wiki_links(&self, course_id: &str, url: &str) -> Vec<Link> {
        self.wiki_pages
            .get(&(course_id.to_string(), url.to_string()))
            .and_then(|r| r.data.as_ref())
            .and_then(|p| p.body.as_deref())
            .map(|html| crate::html::links(html, &self.origin))
            .unwrap_or_default()
    }

    /// Local folder for a wiki page's files.
    pub fn wiki_dir(&self, course_id: &str, url: &str) -> Option<PathBuf> {
        let lib = self.library.as_ref()?;
        let title = self
            .wiki_pages
            .get(&(course_id.to_string(), url.to_string()))
            .and_then(|r| r.data.as_ref())
            .and_then(|p| p.title.clone())
            .unwrap_or_else(|| url.to_string());
        Some(
            lib.root()
                .join(self.course_dir(course_id, &self.course_title(course_id)))
                .join("Page Files")
                .join(library::safe_name(&title)),
        )
    }

    /// A module item's Canvas page, kept on the Canvas origin. Opening an
    /// item through its module page lets Canvas record a "view" requirement.
    fn module_item_url(&self, course_id: &str, item: &crate::modules::Item) -> String {
        item.html_url
            .clone()
            .filter(|u| u.starts_with(&format!("{}/", self.origin)))
            .unwrap_or_else(|| {
                if crate::model::valid_id(&item.id) {
                    format!(
                        "{}/courses/{course_id}/modules/items/{}",
                        self.origin, item.id
                    )
                } else {
                    format!("{}/courses/{course_id}/modules", self.origin)
                }
            })
    }

    /// `Enter` on a module item: assignments and pages open here, files go
    /// to the library, everything else (quizzes, discussions, links, tools)
    /// opens in the browser.
    fn open_module_item(
        &mut self,
        course_id: &str,
        course_name: &str,
        module: &str,
        item: crate::modules::Item,
    ) -> Vec<Effect> {
        let title = item.title().to_string();
        let next = match (item.kind(), item.content(), item.page()) {
            (ItemKind::Heading, ..) => return vec![],
            (ItemKind::Assignment, Some(id), _) => ViewKind::Detail {
                course_id: course_id.to_string(),
                course_name: course_name.to_string(),
                assignment_id: id.to_string(),
                title,
            },
            (ItemKind::Page, _, Some(url)) => ViewKind::WikiPage {
                course_id: course_id.to_string(),
                course_name: course_name.to_string(),
                page_url: url.to_string(),
                title,
            },
            (ItemKind::File, Some(id), _) => {
                let id = id.to_string();
                return self.place_module_file(course_id, module, &id, true, false);
            }
            _ => return vec![Effect::OpenUrl(self.module_item_url(course_id, &item))],
        };
        self.stack.push(View::new(next));
        self.enter_current(false)
    }

    /// Put a module file in the library. Module files are course files, so
    /// they go in Class Files under their Canvas folder; when Files is hidden
    /// the folder cannot be read, so they go in `Module Files/<module>`.
    /// The file's details are always read fresh first.
    fn place_module_file(
        &mut self,
        course_id: &str,
        module: &str,
        file_id: &str,
        open: bool,
        quiet: bool,
    ) -> Vec<Effect> {
        if self.library.is_none() {
            if !quiet {
                self.notice = Some("The file library is unavailable.".into());
            }
            return vec![];
        }
        self.module_waiting.insert(
            file_id.to_string(),
            ModuleFile {
                course_id: course_id.to_string(),
                module: module.to_string(),
                open,
                quiet,
            },
        );
        let mut effects: Vec<Effect> = self
            .request(ResourceKey::File(file_id.to_string()), false)
            .into_iter()
            .collect();
        if self.course_tabs(course_id).contains(&CourseTab::Files) {
            let key = ResourceKey::Files(course_id.to_string());
            self.load_snapshot(&key);
            let known = self
                .course_files
                .get(course_id)
                .is_some_and(|r| r.data.is_some() || matches!(r.sync, SyncState::Syncing { .. }));
            if !known {
                effects.extend(self.request(key, false));
            }
        }
        effects.extend(self.place_module_files());
        effects
    }

    /// Place the module files whose details (and course folders) are in.
    fn place_module_files(&mut self) -> Vec<Effect> {
        let busy = |app: &App, key: ResourceKey| {
            matches!(app.sync_of(&key), Some(SyncState::Syncing { .. }))
        };
        let ready: Vec<String> = self
            .module_waiting
            .iter()
            .filter(|(id, m)| {
                !busy(self, ResourceKey::File((*id).clone()))
                    && !busy(self, ResourceKey::Files(m.course_id.clone()))
                    && self.file_meta(id).is_some()
            })
            .map(|(id, _)| id.clone())
            .collect();
        let mut effects = Vec::new();
        for id in ready {
            let Some(m) = self.module_waiting.remove(&id) else {
                continue;
            };
            let Some(dir) = self.module_file_dir(&m.course_id, &m.module, &id) else {
                continue;
            };
            effects.extend(self.place_file(
                &id,
                Intent {
                    dir,
                    open: m.open,
                    automatic: false,
                    quiet: m.quiet,
                },
            ));
        }
        effects
    }

    pub fn module_file_dir(&self, course_id: &str, module: &str, file_id: &str) -> Option<PathBuf> {
        let name = self.course_title(course_id);
        let folder = self.file_meta(file_id).and_then(|f| f.folder_id.clone());
        if let Some(id) = folder.as_deref()
            && self
                .folder(course_id, id)
                .is_some_and(|f| f.full_name.is_some())
        {
            return self.class_dir(course_id, &name, Some(id));
        }
        let lib = self.library.as_ref()?;
        Some(
            lib.root()
                .join(self.course_dir(course_id, &name))
                .join("Module Files")
                .join(library::safe_name(module)),
        )
    }

    fn activate_link(&mut self, course_id: &str, assignment_id: &str, link: Link) -> Vec<Effect> {
        match link.target {
            Target::File { id } => {
                let dir = self.detail_dir(course_id, assignment_id);
                self.place_from(&id, dir, true)
            }
            Target::Assignment { course_id, id } => {
                let course_name = self.course_title(&course_id);
                let title = if link.text.is_empty() {
                    "Assignment".to_string()
                } else {
                    link.text
                };
                self.stack.push(View::new(ViewKind::Detail {
                    course_id,
                    course_name,
                    assignment_id: id,
                    title,
                }));
                self.enter_current(false)
            }
            Target::Page { course_id, url } => {
                let course_name = self.course_title(&course_id);
                let title = if link.text.is_empty() {
                    url.clone()
                } else {
                    link.text
                };
                self.stack.push(View::new(ViewKind::WikiPage {
                    course_id,
                    course_name,
                    page_url: url,
                    title,
                }));
                self.enter_current(false)
            }
            // Modules, discussions, tools and outside sites open in the browser.
            Target::Canvas | Target::Web | Target::Mail => vec![Effect::OpenUrl(link.url)],
        }
    }

    fn open_selected(&mut self) -> Vec<Effect> {
        if matches!(self.focus(), Section::Calendar | Section::Day) {
            return self.open_calendar_item();
        }
        if self.focus() == Section::Inbox && self.view().kind == ViewKind::Courses {
            return self.open_inbox();
        }
        if matches!(self.view().kind, ViewKind::Inbox { .. }) {
            return self.open_conversation();
        }
        let rows = self.visible_rows();
        let Some(&index) = rows.get(self.view().selected) else {
            return vec![];
        };
        let next = match &self.view().kind {
            ViewKind::Courses => {
                let Some(course) = self.courses.data.as_ref().and_then(|l| l.get(index)) else {
                    return vec![];
                };
                let (id, title) = (course.id.clone(), course.title().to_string());
                // Open where the saved tabs say, before Canvas is asked again.
                self.load_snapshot(&ResourceKey::Tabs(id.clone()));
                self.course_view(&id, &title)
            }
            ViewKind::Modules {
                course_id,
                course_name,
            } => {
                let (course_id, course_name) = (course_id.clone(), course_name.clone());
                let Some(ModuleRow::Item(m, item)) =
                    self.module_rows(&course_id).get(index).copied()
                else {
                    return vec![];
                };
                let (module, item) = (m.name().to_string(), item.clone());
                return self.open_module_item(&course_id, &course_name, &module, item);
            }
            ViewKind::Announcements {
                course_id,
                course_name,
            } => {
                let Some(a) = self
                    .announcements
                    .get(course_id)
                    .and_then(|r| r.data.as_ref())
                    .and_then(|l| l.get(index))
                else {
                    return vec![];
                };
                ViewKind::Announcement {
                    course_id: course_id.clone(),
                    course_name: course_name.clone(),
                    announcement_id: a.id.clone(),
                    title: a.title().to_string(),
                }
            }
            ViewKind::Assignments {
                course_id,
                course_name,
            } => {
                let Some(a) = self
                    .assignments
                    .get(course_id)
                    .and_then(|r| r.data.as_ref())
                    .and_then(|l| l.get(index))
                else {
                    return vec![];
                };
                ViewKind::Detail {
                    course_id: course_id.clone(),
                    course_name: course_name.clone(),
                    assignment_id: a.id.clone(),
                    title: a.title().to_string(),
                }
            }
            ViewKind::Grades {
                course_id,
                course_name,
            } => {
                let (course_id, course_name) = (course_id.clone(), course_name.clone());
                let Some(GradeRow::Assignment(_, a)) =
                    self.grade_rows(&course_id).get(index).copied()
                else {
                    return vec![];
                };
                let (assignment_id, title) = (a.id.clone(), a.title().to_string());
                // Opening the assignment is where its feedback is read, so its
                // mark on this tab is cleared.
                self.mark_feedback_seen(&course_id, &assignment_id);
                ViewKind::Detail {
                    course_id,
                    course_name,
                    assignment_id,
                    title,
                }
            }
            ViewKind::Pages {
                course_id,
                course_name,
            } => {
                let Some(p) = self
                    .page_lists
                    .get(course_id)
                    .and_then(|r| r.data.as_ref())
                    .and_then(|l| l.get(index))
                else {
                    return vec![];
                };
                let Some(slug) = p.slug() else { return vec![] };
                ViewKind::WikiPage {
                    course_id: course_id.clone(),
                    course_name: course_name.clone(),
                    page_url: slug.to_string(),
                    title: p.title().to_string(),
                }
            }
            ViewKind::Detail { .. }
            | ViewKind::Announcement { .. }
            | ViewKind::WikiPage { .. }
            | ViewKind::CourseHome { .. }
            | ViewKind::Conversation { .. }
            | ViewKind::Inbox { .. } => return vec![],
            ViewKind::Files {
                course_id,
                course_name,
                ..
            } => {
                let (course_id, course_name) = (course_id.clone(), course_name.clone());
                let rows = self.file_rows();
                return match rows.get(self.view().selected).copied() {
                    Some(FileRow::Folder(f)) => {
                        let id = f.id.clone();
                        self.stack.push(View::new(ViewKind::Files {
                            course_id,
                            course_name,
                            folder_id: Some(id),
                        }));
                        self.dirty = true;
                        vec![]
                    }
                    Some(FileRow::File(f)) => {
                        let id = f.id.clone();
                        let dir = self.file_dir(&course_id, &course_name, f);
                        self.place_from(&id, dir, true)
                    }
                    None => vec![],
                };
            }
        };
        self.stack.push(View::new(next));
        self.enter_current(false)
    }

    /// The page `o` opens: the selected row or link, or the current screen.
    /// Pages for Canvas items are built on the Canvas origin; links from a
    /// description may lead anywhere on the web.
    pub fn browser_url(&self) -> Option<String> {
        let same_origin = |u: &str| u.starts_with(&format!("{}/", self.origin));
        match &self.view().kind {
            ViewKind::Modules { course_id, .. } => Some(match self.selected_module_row() {
                Some(ModuleRow::Item(_, item)) => self.module_item_url(course_id, item),
                _ => format!("{}/courses/{course_id}/modules", self.origin),
            }),
            ViewKind::WikiPage {
                course_id,
                page_url,
                ..
            } => {
                if let Some(link) = self.selected_link() {
                    return Some(link.url);
                }
                let html = self
                    .wiki_pages
                    .get(&(course_id.clone(), page_url.clone()))
                    .and_then(|r| r.data.as_ref())
                    .and_then(|p| p.html_url.clone())
                    .filter(|u| same_origin(u));
                Some(html.unwrap_or_else(|| {
                    if page_url == crate::modules::FRONT_PAGE {
                        format!("{}/courses/{course_id}", self.origin)
                    } else {
                        format!("{}/courses/{course_id}/pages/{page_url}", self.origin)
                    }
                }))
            }
            ViewKind::CourseHome { course_id, .. } => {
                Some(format!("{}/courses/{course_id}", self.origin))
            }
            ViewKind::Pages { course_id, .. } => {
                let base = format!("{}/courses/{course_id}/pages", self.origin);
                let rows = self.visible_rows();
                let page = rows
                    .get(self.view().selected)
                    .and_then(|i| self.page_lists.get(course_id)?.data.as_ref()?.get(*i));
                Some(match page {
                    Some(p) => p
                        .html_url
                        .clone()
                        .filter(|u| same_origin(u))
                        .unwrap_or_else(|| match p.slug() {
                            Some(slug) => format!("{base}/{slug}"),
                            None => base.clone(),
                        }),
                    None => base,
                })
            }
            ViewKind::Grades { course_id, .. } => Some(match self.selected_grade_row() {
                Some(GradeRow::Assignment(_, a)) => a
                    .html_url
                    .clone()
                    .filter(|u| same_origin(u))
                    .unwrap_or_else(|| {
                        format!("{}/courses/{course_id}/assignments/{}", self.origin, a.id)
                    }),
                _ => format!("{}/courses/{course_id}/grades", self.origin),
            }),
            ViewKind::Conversation {
                conversation_id, ..
            } => {
                if let Some(link) = self.selected_link() {
                    return Some(link.url);
                }
                Some(format!("{}/conversations/{conversation_id}", self.origin))
            }
            // The home screen's box opens the Inbox itself; the screen opens
            // whichever conversation is selected.
            ViewKind::Inbox { .. } => Some(match self.selected_conversation() {
                Some(c) => format!("{}/conversations/{}", self.origin, c.id),
                None => format!("{}/conversations", self.origin),
            }),
            ViewKind::Courses if self.focus() == Section::Inbox => {
                Some(format!("{}/conversations", self.origin))
            }
            ViewKind::Announcements { course_id, .. } => {
                let base = format!("{}/courses/{course_id}/announcements", self.origin);
                let rows = self.visible_rows();
                let item = rows
                    .get(self.view().selected)
                    .and_then(|i| self.announcements.get(course_id)?.data.as_ref()?.get(*i));
                Some(
                    item.map(|a| {
                        format!(
                            "{}/courses/{course_id}/discussion_topics/{}",
                            self.origin, a.id
                        )
                    })
                    .unwrap_or(base),
                )
            }
            ViewKind::Announcement {
                course_id,
                announcement_id,
                ..
            } => {
                if let Some(link) = self.selected_link() {
                    return Some(link.url);
                }
                Some(format!(
                    "{}/courses/{course_id}/discussion_topics/{announcement_id}",
                    self.origin
                ))
            }

            // The attempts and the feedback are about the submission, so the
            // browser opens that page: it is where Canvas shows media comments
            // and feedback drawn on a submitted document.
            ViewKind::Detail {
                course_id,
                assignment_id,
                ..
            } if matches!(self.focus(), Section::Submissions | Section::Feedback) => Some(format!(
                "{}/courses/{course_id}/assignments/{assignment_id}/submissions/self",
                self.origin
            )),
            ViewKind::Detail {
                course_id,
                assignment_id,
                ..
            } if self.selected_link().is_some() => {
                let _ = (course_id, assignment_id);
                let link = self.selected_link()?;
                Some(match &link.target {
                    // The file's Canvas page rather than a direct download.
                    Target::File { id } => link.url.replacen(
                        &format!("/files/{id}/download"),
                        &format!("/files/{id}"),
                        1,
                    ),
                    _ => link.url.clone(),
                })
            }
            ViewKind::Files {
                course_id,
                folder_id,
                ..
            } => {
                let base = format!("{}/courses/{}/files", self.origin, course_id);
                let rows = self.file_rows();
                let folder = match rows.get(self.view().selected) {
                    Some(FileRow::File(f)) => return Some(format!("{base}/{}", f.id)),
                    Some(FileRow::Folder(f)) => Some(*f),
                    None => folder_id
                        .as_deref()
                        .and_then(|id| self.folder(course_id, id)),
                };
                let parts: Vec<String> = folder
                    .and_then(|f| f.full_name.as_deref())
                    .map(|full| full.split('/').skip(1).map(percent_encode).collect())
                    .unwrap_or_default();
                Some(if parts.is_empty() {
                    base
                } else {
                    format!("{base}/folder/{}", parts.join("/"))
                })
            }
            ViewKind::Courses if matches!(self.focus(), Section::Calendar | Section::Day) => {
                let items = self.day_items(self.calendar_day);
                let index = if self.focus() == Section::Day {
                    self.day_item
                } else {
                    0
                };
                Some(match items.get(index) {
                    Some(item) => self.calendar_url(item),
                    None => format!("{}/calendar", self.origin),
                })
            }
            ViewKind::Courses => {
                let rows = self.visible_rows();
                let index = *rows.get(self.view().selected)?;
                let course = self.courses.data.as_ref()?.get(index)?;
                Some(format!("{}/courses/{}", self.origin, course.id))
            }
            ViewKind::Assignments { course_id, .. } => {
                let rows = self.visible_rows();
                let index = *rows.get(self.view().selected)?;
                let a = self.assignments.get(course_id)?.data.as_ref()?.get(index)?;
                Some(
                    a.html_url
                        .clone()
                        .filter(|u| same_origin(u))
                        .unwrap_or_else(|| {
                            format!("{}/courses/{}/assignments/{}", self.origin, course_id, a.id)
                        }),
                )
            }
            ViewKind::Detail {
                course_id,
                assignment_id,
                ..
            } => {
                let html = self
                    .details
                    .get(&(course_id.clone(), assignment_id.clone()))
                    .and_then(|r| r.data.as_ref())
                    .and_then(|d| d.base.html_url.clone())
                    .filter(|u| same_origin(u));
                Some(html.unwrap_or_else(|| {
                    format!(
                        "{}/courses/{}/assignments/{}",
                        self.origin, course_id, assignment_id
                    )
                }))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> Msg {
        Msg::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn ctrl(c: char) -> Msg {
        Msg::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
    }

    fn course(id: &str, name: &str) -> Course {
        Course {
            id: id.into(),
            name: Some(name.into()),
            ..Default::default()
        }
    }

    /// `deliver`, but handing back the effects it produced.
    fn deliver_back(
        app: &mut App,
        key: ResourceKey,
        request: u64,
        payload: Payload,
    ) -> Vec<Effect> {
        app.update(Msg::Fetched {
            key,
            request,
            result: Ok((payload, FetchStats::default())),
            at: Utc::now(),
        })
    }

    fn fetch_of(effects: &[Effect]) -> (ResourceKey, u64) {
        match effects.iter().find(|e| matches!(e, Effect::Fetch { .. })) {
            Some(Effect::Fetch { key, request }) => (key.clone(), *request),
            _ => panic!("expected a fetch, got {effects:?}"),
        }
    }

    fn deliver(app: &mut App, key: ResourceKey, request: u64, payload: Payload) {
        app.update(Msg::Fetched {
            key,
            request,
            result: Ok((payload, FetchStats::default())),
            at: Utc::now(),
        });
    }

    fn app_with_courses() -> App {
        let mut app = App::new("https://ufl.instructure.com", None);
        let (k, r) = fetch_of(&app.start());
        deliver(
            &mut app,
            k,
            r,
            Payload::Courses(vec![
                course("10", "GPU Computing"),
                course("20", "Compilers"),
            ]),
        );
        app
    }

    #[test]
    fn start_fetches_courses_and_applies_result() {
        let app = app_with_courses();
        assert_eq!(app.courses.data.as_ref().unwrap().len(), 2);
        assert_eq!(app.courses.origin, Origin::Network);
        assert!(!app.current_syncing());
    }

    #[test]
    fn file_submission_requires_review_and_explicit_yes() {
        let mut app = App::new("https://ufl.instructure.com", None);
        app.stack.push(View::new(ViewKind::Detail {
            course_id: "10".into(),
            course_name: "GPU".into(),
            assignment_id: "20".into(),
            title: "Lab".into(),
        }));
        let mut detail = AssignmentDetail::default();
        detail.base.id = "20".into();
        detail.base.submission_types = vec!["online_upload".into()];
        let resource = app.details.entry(("10".into(), "20".into())).or_default();
        resource.data = Some(detail);
        resource.origin = Origin::Network;
        resource.fetched_at = Some(Utc::now());
        assert!(app.update(key(KeyCode::Char('s'))).is_empty());
        assert!(app.submission.is_some());
        let start = app.submission.as_ref().unwrap().start_directory.clone();
        assert!(start.ends_with(std::path::MAIN_SEPARATOR));
        assert_eq!(app.submission.as_ref().unwrap().input, start);
        app.update(Msg::Paste("\"C:\\work\\lab.pdf\"".into()));
        assert_eq!(
            app.submission.as_ref().unwrap().input,
            "\"C:\\work\\lab.pdf\""
        );
        for _ in 0..12 {
            app.update(Msg::Key(KeyEvent::new_with_kind(
                KeyCode::Backspace,
                KeyModifiers::NONE,
                ratatui::crossterm::event::KeyEventKind::Repeat,
            )));
        }
        assert_eq!(
            app.submission.as_ref().unwrap().input,
            "\"C:\\work\\lab.pdf\""
        );
        app.update(key(KeyCode::Backspace));
        assert_eq!(
            app.submission.as_ref().unwrap().input,
            "\"C:\\work\\lab.pdf"
        );
        app.update(Msg::Key(KeyEvent::new(
            KeyCode::Char('u'),
            KeyModifiers::CONTROL,
        )));
        assert!(app.submission.as_ref().unwrap().input.is_empty());
        app.submission.as_mut().unwrap().input = start;
        app.submission.as_mut().unwrap().files.push(FileChoice {
            path: PathBuf::from("C:/work/lab.pdf"),
            name: "lab.pdf".into(),
            size: 7,
        });
        assert!(app.update(key(KeyCode::Enter)).is_empty());
        assert!(app.submission.as_ref().unwrap().reviewing);
        assert!(app.update(key(KeyCode::Char('n'))).is_empty());
        assert!(!app.submission.as_ref().unwrap().reviewing);
        app.update(key(KeyCode::Enter));
        assert!(matches!(&app.update(key(KeyCode::Char('y')))[..],
            [Effect::Submit { course_id, assignment_id, files }] if course_id == "10" && assignment_id == "20" && files.len() == 1));
        assert!(app.update(key(KeyCode::Char('y'))).is_empty());
    }

    #[test]
    fn past_submissions_load_with_the_page_and_are_a_section_of_it() {
        let mut app = App::new("https://ufl.instructure.com", None);
        app.stack.push(View::new(ViewKind::Detail {
            course_id: "10".into(),
            course_name: "GPU".into(),
            assignment_id: "20".into(),
            title: "Lab".into(),
        }));
        let (k, r) = fetch_of(&app.enter_current(false));
        let mut detail = AssignmentDetail::default();
        detail.base.id = "20".into();
        detail.base.submission = Some(crate::model::Submission {
            submitted_at: Some(Utc::now()),
            attempt: Some(2),
            ..Default::default()
        });
        let effects = app.update(Msg::Fetched {
            key: k,
            request: r,
            result: Ok((Payload::Detail(Box::new(detail)), FetchStats::default())),
            at: Utc::now(),
        });
        // Something was submitted, so the attempts are read with the page.
        let (key1, request1) = fetch_of(&effects);
        assert_eq!(key1, ResourceKey::History("10".into(), "20".into()));
        assert_eq!(app.sections(), vec![Section::Text], "no attempts known yet");
        let history = submission::History::parse(serde_json::json!({
            "attempt": 2, "submitted_at": "2026-10-22T12:00:00Z",
            "submission_history": [
                {"attempt": 1, "submitted_at": "2026-10-20T12:00:00Z"},
                {"attempt": 2, "submitted_at": "2026-10-22T12:00:00Z"}
            ]
        }))
        .unwrap();
        deliver(&mut app, key1, request1, Payload::History(history));
        assert_eq!(app.attempt_count("10", "20"), 2);
        assert_eq!(app.sections(), vec![Section::Text, Section::Submissions]);
        // Tab moves to the attempts; arrows choose one; Esc returns to the text, then leaves.
        assert!(app.update(key(KeyCode::Tab)).is_empty());
        assert_eq!(app.focus(), Section::Submissions);
        app.update(key(KeyCode::Down));
        assert_eq!(app.view().attempt, 1);
        app.update(key(KeyCode::Down));
        assert_eq!(app.view().attempt, 1, "stays on the oldest");
        assert!(app.update(key(KeyCode::Esc)).is_empty());
        assert_eq!(app.focus(), Section::Text);
        assert_eq!(app.stack.len(), 2);
        // `h` is "back" here too, as everywhere else.
        app.update(key(KeyCode::Char('h')));
        assert_eq!(app.stack.len(), 1);
        // A new visit reads the attempts again, right away this time.
        app.stack.push(View::new(ViewKind::Detail {
            course_id: "10".into(),
            course_name: "GPU".into(),
            assignment_id: "20".into(),
            title: "Lab".into(),
        }));
        let effects = app.enter_current(false);
        let history_reads: Vec<u64> = effects
            .iter()
            .filter_map(|e| match e {
                Effect::Fetch {
                    key: ResourceKey::History(..),
                    request,
                } => Some(*request),
                _ => None,
            })
            .collect();
        assert!(
            matches!(history_reads[..], [r] if r > request1),
            "{effects:?}"
        );
    }

    #[test]
    fn unsubmitted_assignments_do_not_read_past_submissions() {
        let mut app = App::new("https://ufl.instructure.com", None);
        app.stack.push(View::new(ViewKind::Detail {
            course_id: "10".into(),
            course_name: "GPU".into(),
            assignment_id: "20".into(),
            title: "Lab".into(),
        }));
        let (k, r) = fetch_of(&app.enter_current(false));
        let effects = app.update(Msg::Fetched {
            key: k,
            request: r,
            result: Ok((Payload::Detail(Box::default()), FetchStats::default())),
            at: Utc::now(),
        });
        assert!(effects.is_empty(), "{effects:?}");
        assert!(app.update(key(KeyCode::Tab)).is_empty());
        assert_eq!(app.focus(), Section::Text, "nothing to move to");
    }

    #[test]
    fn hidden_courses_survive_refresh_and_restart_and_can_be_restored() {
        let root =
            std::env::temp_dir().join(format!("canter-hidden-courses-{}", std::process::id()));
        let store = Arc::new(SnapshotStore::new(&root, "one"));
        let mut app = App::new("https://ufl.instructure.com", Some(store.clone()));
        let (k, r) = fetch_of(&app.start());
        deliver(
            &mut app,
            k,
            r,
            Payload::Courses(vec![course("10", "GPU"), course("20", "Compilers")]),
        );
        // Space, then h: hide the selected course.
        assert!(app.update(key(KeyCode::Char(' '))).is_empty());
        assert!(app.menu.is_some());
        assert!(app.update(key(KeyCode::Char('h'))).is_empty());
        assert!(app.menu.is_none());
        assert_eq!(app.visible_rows(), [1]);
        let (k, r) = fetch_of(&app.update(key(KeyCode::Char('r'))));
        deliver(
            &mut app,
            k,
            r,
            Payload::Courses(vec![course("10", "GPU"), course("20", "Compilers")]),
        );
        assert_eq!(app.visible_rows(), [1]);
        let mut restarted = App::new("https://ufl.instructure.com", Some(store));
        let (k, r) = fetch_of(&restarted.start());
        deliver(
            &mut restarted,
            k,
            r,
            Payload::Courses(vec![course("10", "GPU"), course("20", "Compilers")]),
        );
        assert_eq!(restarted.visible_rows(), [1]);
        // Space, then H: show hidden courses; Space, then h on one restores it.
        restarted.update(key(KeyCode::Char(' ')));
        restarted.update(key(KeyCode::Char('H')));
        assert_eq!(restarted.visible_rows(), [0, 1]);
        restarted.update(key(KeyCode::Char(' ')));
        restarted.update(key(KeyCode::Char('h')));
        assert!(restarted.hidden_courses.is_empty());
        restarted.update(key(KeyCode::Esc));
        assert!(!restarted.show_hidden);
        assert_eq!(restarted.visible_rows(), [0, 1]);
        let other = App::new(
            "https://ufl.instructure.com",
            Some(Arc::new(SnapshotStore::new(&root, "other"))),
        );
        assert!(other.hidden_courses.is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn hiding_every_course_keeps_recovery_available() {
        let mut app = app_with_courses();
        for _ in 0..2 {
            app.update(key(KeyCode::Char(' ')));
            app.update(key(KeyCode::Char('h')));
        }
        assert!(app.visible_rows().is_empty());
        // The menu still offers a way back.
        app.update(key(KeyCode::Char(' ')));
        assert!(app.menu_entries().iter().any(|e| e.act == Act::ShowHidden));
        app.update(key(KeyCode::Char('H')));
        assert_eq!(app.visible_rows(), [0, 1]);
        app.update(key(KeyCode::Char(' ')));
        app.update(key(KeyCode::Char('h')));
        assert_eq!(app.hidden_courses.len(), 1);
        app.update(key(KeyCode::Esc));
        assert_eq!(app.visible_rows(), [0]);
    }

    #[test]
    fn stale_response_is_ignored_after_explicit_refresh() {
        let mut app = App::new("https://ufl.instructure.com", None);
        let (k, first) = fetch_of(&app.start());
        let (_, second) = fetch_of(&app.update(key(KeyCode::Char('r'))));
        assert_ne!(first, second);
        deliver(
            &mut app,
            k.clone(),
            first,
            Payload::Courses(vec![course("1", "old")]),
        );
        assert!(
            app.courses.data.is_none(),
            "older request must not satisfy the newer access"
        );
        deliver(
            &mut app,
            k,
            second,
            Payload::Courses(vec![course("2", "new")]),
        );
        assert_eq!(app.courses.data.as_ref().unwrap()[0].id, "2");
    }

    #[test]
    fn failure_keeps_data_and_marks_outdated() {
        let mut app = app_with_courses();
        let (k, r) = fetch_of(&app.update(key(KeyCode::Char('r'))));
        app.update(Msg::Fetched {
            key: k,
            request: r,
            result: Err(CanvasError::Network(crate::transport::NetError::Timeout)),
            at: Utc::now(),
        });
        assert_eq!(app.courses.data.as_ref().unwrap().len(), 2);
        assert!(matches!(app.courses.sync, SyncState::Failed(_)));
        assert!(!app.login_required);
    }

    #[test]
    fn expired_session_requests_login() {
        let mut app = App::new("https://ufl.instructure.com", None);
        let (k, r) = fetch_of(&app.start());
        let effects = app.update(Msg::Fetched {
            key: k,
            request: r,
            result: Err(CanvasError::Unauthenticated),
            at: Utc::now(),
        });
        assert_eq!(
            effects,
            vec![Effect::Login],
            "dead session at startup starts sign-in"
        );
        assert!(app.login_required);
        assert_eq!(app.update(key(KeyCode::Char('L'))), vec![Effect::Login]);
        let effects = app.update(Msg::LoginFinished {
            ok: true,
            message: "ok".into(),
        });
        assert_eq!(fetch_of(&effects).0, ResourceKey::Courses);
        assert!(!app.login_required);
    }

    #[test]
    fn later_expiry_waits_for_user() {
        let mut app = app_with_courses();
        let (k, r) = fetch_of(&app.update(key(KeyCode::Char('r'))));
        let effects = app.update(Msg::Fetched {
            key: k,
            request: r,
            result: Err(CanvasError::Unauthenticated),
            at: Utc::now(),
        });
        assert!(effects.is_empty());
        assert!(app.login_required);
        assert_eq!(app.courses.data.as_ref().unwrap().len(), 2);
    }

    #[test]
    fn navigation_fetches_each_access_and_back_resyncs() {
        let mut app = app_with_courses();
        let (k, r) = fetch_of(&app.update(key(KeyCode::Enter)));
        assert_eq!(k, ResourceKey::Assignments("10".into()));
        let a = AssignmentSummary {
            id: "7".into(),
            name: Some("Lab-2".into()),
            ..Default::default()
        };
        deliver(&mut app, k, r, Payload::Assignments(vec![a]));
        let (k, _) = fetch_of(&app.update(key(KeyCode::Enter)));
        assert_eq!(k, ResourceKey::Detail("10".into(), "7".into()));
        // Back to the list: the earlier list read completed, so a new one is required.
        let (k, _) = fetch_of(&app.update(key(KeyCode::Esc)));
        assert_eq!(k, ResourceKey::Assignments("10".into()));
        assert_eq!(app.stack.len(), 2);
    }

    #[test]
    fn recent_in_flight_read_satisfies_quick_reentry() {
        let mut app = app_with_courses();
        let first = app.update(key(KeyCode::Enter));
        fetch_of(&first);
        app.update(key(KeyCode::Esc));
        let again = app.update(key(KeyCode::Enter));
        assert!(
            again.is_empty(),
            "in-flight read from <2s ago is promoted, not duplicated"
        );
    }

    /// Type a query into the open palette.
    fn type_query(app: &mut App, text: &str) {
        for c in text.chars() {
            app.update(key(KeyCode::Char(c)));
        }
    }

    #[test]
    fn the_palette_opens_on_slash_and_on_ctrl_k() {
        let mut app = app_with_courses();
        app.update(key(KeyCode::Char('/')));
        assert!(app.palette.is_some(), "slash opens it");
        app.update(key(KeyCode::Esc));
        assert!(app.palette.is_none(), "Esc closes it");
        app.update(ctrl('k'));
        assert!(app.palette.is_some(), "Ctrl+K opens it too");
    }

    #[test]
    fn typing_narrows_the_results_and_enter_opens_the_course() {
        let mut app = app_with_courses();
        app.update(key(KeyCode::Char('/')));
        type_query(&mut app, "comp");
        // Both courses match: "Compilers" by name, "GPU Computing" inside a
        // word. The named one wins.
        let hits = app.palette_hits();
        let courses: Vec<&str> = hits
            .iter()
            .filter(|h| h.kind == crate::search::Kind::Course)
            .map(|h| h.title.as_str())
            .collect();
        assert_eq!(courses, ["Compilers", "GPU Computing"]);
        let (k, _) = fetch_of(&app.update(key(KeyCode::Enter)));
        assert_eq!(k, ResourceKey::Assignments("20".into()));
        assert!(app.palette.is_none(), "opening a result closes the palette");
    }

    #[test]
    fn the_selection_goes_back_to_the_top_when_the_query_changes() {
        // Otherwise the second keystroke opens whatever happens to have
        // moved under the cursor, which is how a palette loses your trust.
        let mut app = app_with_courses();
        app.update(key(KeyCode::Char('/')));
        app.update(key(KeyCode::Down));
        assert_eq!(app.palette.as_ref().unwrap().selected, 1);
        type_query(&mut app, "c");
        assert_eq!(app.palette.as_ref().unwrap().selected, 0);
    }

    #[test]
    fn the_palette_swallows_keys_that_mean_something_underneath() {
        // `q` quits and `r` refreshes on the course list; inside the palette
        // they are just letters.
        let mut app = app_with_courses();
        app.update(key(KeyCode::Char('/')));
        type_query(&mut app, "qr");
        assert!(!app.quit, "q typed into the box must not quit");
        assert_eq!(app.palette.as_ref().unwrap().query, "qr");
    }

    #[test]
    fn an_action_can_be_run_from_the_palette() {
        let mut app = app_with_courses();
        assert!(!app.show_hidden);
        app.update(key(KeyCode::Char('/')));
        type_query(&mut app, "hidden");
        let hit = app
            .palette_hits()
            .into_iter()
            .find(|h| h.kind == crate::search::Kind::Action)
            .expect("the hidden-courses action is offered");
        assert_eq!(hit.target, crate::search::Target::Action(Act::ShowHidden));
        app.update(key(KeyCode::Enter));
        assert!(app.show_hidden, "the action ran");
        assert!(app.palette.is_none());
    }

    #[test]
    fn the_palette_searches_work_pages_files_and_the_inbox() {
        let mut app = app_with_courses();
        let (k, r) = fetch_of(&app.update(key(KeyCode::Enter)));
        deliver(
            &mut app,
            k,
            r,
            Payload::Assignments(vec![AssignmentSummary {
                id: "7".into(),
                name: Some("Lab 2: Pipelining".into()),
                ..Default::default()
            }]),
        );
        app.page_lists.entry("10".into()).or_default().data = Some(vec![page_summary("intro", "Welcome", 1)]);
        app.conversations.entry("inbox".into()).or_default().data =
            Some(vec![conversation("5", "About your lab", "read")]);
        app.update(key(KeyCode::Char('/')));
        type_query(&mut app, "lab");
        let kinds: Vec<crate::search::Kind> =
            app.palette_hits().into_iter().map(|h| h.kind).collect();
        assert!(kinds.contains(&crate::search::Kind::Assignment));
        assert!(kinds.contains(&crate::search::Kind::Conversation));
    }

    #[test]
    fn a_result_opens_on_top_so_escape_comes_back() {
        let mut app = app_with_courses();
        let depth = app.stack.len();
        app.update(key(KeyCode::Char('/')));
        type_query(&mut app, "compilers");
        app.update(key(KeyCode::Enter));
        assert_eq!(app.stack.len(), depth + 1, "pushed, not replaced");
        app.update(key(KeyCode::Esc));
        assert_eq!(app.stack.len(), depth, "and Esc returns");
    }

    #[test]
    fn the_warm_up_reads_every_list_once_a_couple_at_a_time() {
        let mut app = App::new("https://ufl.instructure.com", None);
        app.warm_up = true;
        let (k, r) = fetch_of(&app.start());
        let effects = deliver_back(
            &mut app,
            k,
            r,
            Payload::Courses(vec![
                course("10", "GPU Computing"),
                course("20", "Compilers"),
            ]),
        );
        // Two at a time, so the screen the user is on keeps half the capacity.
        let started: Vec<ResourceKey> = effects
            .iter()
            .filter_map(|e| match e {
                Effect::Fetch { key, .. } => Some(key.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            started.len(),
            WARM_AT_ONCE,
            "no more than two warm-up reads in flight: {started:?}"
        );
        assert!(!app.warm_queue.is_empty(), "the rest is queued");
        // Each one that finishes starts the next, and nothing is asked twice.
        let mut asked = started.clone();
        let mut guard = 0;
        while let Some(next) = asked.pop() {
            guard += 1;
            assert!(guard < 200, "the queue should drain");
            let request = match app.sync_of(&next) {
                Some(SyncState::Syncing { request, .. }) => *request,
                _ => continue,
            };
            for effect in deliver_back(&mut app, next, request, Payload::Tabs(vec![])) {
                if let Effect::Fetch { key, .. } = effect {
                    assert!(
                        !asked.contains(&key),
                        "{key:?} was already being read"
                    );
                    asked.insert(0, key);
                }
            }
        }
        assert!(app.warm_queue.is_empty(), "every list was reached");
    }

    #[test]
    fn a_saved_copy_is_searchable_at_once_and_still_refreshed() {
        // Both halves matter. The saved copy has to be searchable before any
        // request comes back, or the box makes you wait for an answer it
        // already has; and it still has to be read, or a course you have not
        // opened in a week is searched as it was a week ago without saying so.
        let root = std::env::temp_dir().join(format!("canter-warm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let store = Arc::new(SnapshotStore::new(&root, "reader"));
        store.save(
            &ResourceKey::Assignments("10".into()),
            &vec![AssignmentSummary {
                id: "7".into(),
                name: Some("Lab 2".into()),
                ..Default::default()
            }],
            Utc::now(),
        );
        let mut app = App::new(ORIGIN, Some(store));
        app.warm_up = true;
        let (k, r) = fetch_of(&app.start());
        let effects = deliver_back(&mut app, k, r, Payload::Courses(vec![course("10", "GPU")]));
        // Searchable straight away, with nothing having come back yet.
        app.update(key(KeyCode::Char('/')));
        for c in "lab".chars() {
            app.update(key(KeyCode::Char(c)));
        }
        assert!(
            app.palette_hits().iter().any(|h| h.title == "Lab 2"),
            "the saved assignment is there before any read finished"
        );
        // And the read was still started.
        let asked: Vec<ResourceKey> = effects
            .iter()
            .filter_map(|e| match e {
                Effect::Fetch { key, .. } => Some(key.clone()),
                _ => None,
            })
            .collect();
        assert!(
            asked.contains(&ResourceKey::Assignments("10".into())),
            "the saved copy is brought up to date: {asked:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_warm_up_stays_off_unless_a_person_is_driving() {
        // The headless walk-through, the benchmark and these tests must say
        // exactly what they ask Canvas for.
        let mut app = App::new("https://ufl.instructure.com", None);
        let (k, r) = fetch_of(&app.start());
        let effects = deliver_back(&mut app, k, r, Payload::Courses(vec![course("10", "GPU")]));
        assert!(
            !effects.iter().any(|e| matches!(e, Effect::Fetch { .. })),
            "nothing is read that nobody asked for: {effects:?}"
        );
        assert!(app.warm_queue.is_empty());
    }

    /// A palette over two courses, each with one assignment of the same name,
    /// which is the case `@` exists for.
    fn tagged_app() -> App {
        let mut app = app_with_courses();
        for (course, id) in [("10", "7"), ("20", "9")] {
            app.assignments.entry(course.into()).or_default().data = Some(vec![AssignmentSummary {
                id: id.into(),
                name: Some("Project 1".into()),
                ..Default::default()
            }]);
        }
        app.update(key(KeyCode::Char('/')));
        app
    }

    #[test]
    fn an_at_tag_narrows_the_search_to_one_class() {
        let mut app = tagged_app();
        type_query(&mut app, "project");
        assert_eq!(
            app.palette_hits()
                .iter()
                .filter(|h| h.kind == crate::search::Kind::Assignment)
                .count(),
            2,
            "both courses have one"
        );
        // Naming the class leaves only its own.
        app.update(ctrl('u'));
        type_query(&mut app, "@compilers project");
        let hits = app.palette_hits();
        let found: Vec<&crate::search::Target> = hits
            .iter()
            .filter(|h| h.kind == crate::search::Kind::Assignment)
            .map(|h| &h.target)
            .collect();
        assert_eq!(
            found,
            [&crate::search::Target::Assignment("20".into(), "9".into())]
        );
        assert_eq!(app.palette_coverage(), "in Compilers");
    }

    #[test]
    fn a_tag_matches_a_class_the_way_everything_else_is_matched() {
        let mut app = tagged_app();
        // Part of the name is enough, and so is the course code.
        for tag in ["@comp", "@cmplrs", "@Compilers"] {
            app.update(ctrl('u'));
            type_query(&mut app, &format!("{tag} project"));
            let hits = app.palette_hits();
            assert!(
                hits.iter().any(|h| h.target
                    == crate::search::Target::Assignment("20".into(), "9".into())),
                "{tag} should reach Compilers"
            );
            assert!(
                !hits.iter().any(|h| h.target
                    == crate::search::Target::Assignment("10".into(), "7".into())),
                "{tag} should not reach the other course"
            );
        }
    }

    #[test]
    fn a_tag_being_typed_offers_the_classes_to_finish_it_with() {
        let mut app = tagged_app();
        type_query(&mut app, "@");
        let hits = app.palette_hits();
        assert!(
            hits.iter().all(|h| h.kind == crate::search::Kind::Course),
            "only classes are offered: {hits:?}"
        );
        assert_eq!(hits.len(), 2);
        type_query(&mut app, "comp");
        let hits = app.palette_hits();
        assert_eq!(hits.len(), 2, "both still match 'comp' as a subsequence");
        assert_eq!(hits[0].title, "Compilers", "the better match leads");
    }

    #[test]
    fn a_tag_that_names_no_class_says_so_rather_than_searching_everything() {
        let mut app = tagged_app();
        type_query(&mut app, "@zzzz project");
        assert!(
            app.palette_hits().is_empty(),
            "narrowing to nothing finds nothing, rather than quietly ignoring the tag"
        );
        assert_eq!(
            app.palette_coverage(),
            "@zzzz matches no class you can search"
        );
    }

    #[test]
    fn tags_narrow_sensibly_on_real_course_names() {
        // The names in the owner's account, including the pairs that make
        // this hard: two COP courses, and two courses whose names both begin
        // "Introduction to G".
        let names = [
            ("1", "COP4600 - Operating Systems - S2026 - Campus"),
            ("2", "COP5556-178C(27318) - Program Language Prin"),
            ("3", "EEL5934 - Introduction to GPU Computing"),
            ("4", "Introduction to Git and GitHub"),
            ("5", "CDA4102/5155: Computer Architecture"),
            ("6", "CISE Undergraduate Advising"),
            ("7", "First Year Engineering Advising 2024"),
        ];
        let mut app = App::new(ORIGIN, None);
        let (k, r) = fetch_of(&app.start());
        deliver(
            &mut app,
            k,
            r,
            Payload::Courses(names.iter().map(|(id, n)| course(id, n)).collect()),
        );
        let resolves = |app: &App, tag: &str| -> Vec<String> {
            app.tagged_courses(tag)
                .into_iter()
                .map(|id| app.course_title(&id))
                .collect()
        };
        assert_eq!(
            resolves(&app, "cop4600"),
            ["COP4600 - Operating Systems - S2026 - Campus"]
        );
        assert_eq!(
            resolves(&app, "cop5556"),
            ["COP5556-178C(27318) - Program Language Prin"]
        );
        assert_eq!(resolves(&app, "git"), ["Introduction to Git and GitHub"]);
        assert_eq!(resolves(&app, "gpu"), ["EEL5934 - Introduction to GPU Computing"]);
        assert_eq!(resolves(&app, "cda"), ["CDA4102/5155: Computer Architecture"]);
        assert_eq!(resolves(&app, "cise"), ["CISE Undergraduate Advising"]);
        // Two courses have "Advising" in the name. The tag resolves to the
        // better match rather than keeping both, and the coverage line names
        // which one, so the choice is never silent.
        assert_eq!(resolves(&app, "advising"), ["CISE Undergraduate Advising"]);
        app.update(key(KeyCode::Char('/')));
        for c in "@advising quiz".chars() {
            app.update(key(KeyCode::Char(c)));
        }
        assert_eq!(app.palette_coverage(), "in CISE Undergraduate Advising");
        // While the tag is still being typed, both are offered, in order, so
        // the other one is a keystroke away rather than hidden.
        app.update(ctrl('u'));
        for c in "@advising".chars() {
            app.update(key(KeyCode::Char(c)));
        }
        let offered: Vec<String> = app
            .palette_hits()
            .into_iter()
            .map(|h| h.title)
            .collect();
        assert_eq!(
            offered,
            [
                "CISE Undergraduate Advising",
                "First Year Engineering Advising 2024"
            ]
        );
    }

    #[test]
    fn hidden_courses_are_out_of_the_search_until_that_is_turned_on() {
        let mut app = tagged_app();
        app.hidden_courses.insert("20".into());
        type_query(&mut app, "project");
        let found = |app: &App| {
            app.palette_hits()
                .iter()
                .any(|h| h.target == crate::search::Target::Assignment("20".into(), "9".into()))
        };
        assert!(!found(&app), "work in a hidden course is not searched");
        assert!(
            !app.palette_hits()
                .iter()
                .any(|h| h.target == crate::search::Target::Course("20".into())),
            "nor is the course itself"
        );
        // Ctrl+H brings them in, and says so.
        app.update(ctrl('h'));
        assert!(found(&app), "and is searched once that is turned on");
        assert!(
            app.palette_coverage().contains("hidden included"),
            "{}",
            app.palette_coverage()
        );
        // An @ tag cannot reach around the switch either.
        app.update(ctrl('h'));
        app.update(ctrl('u'));
        type_query(&mut app, "@compilers project");
        assert!(app.palette_hits().is_empty());
    }

    #[test]
    fn the_hidden_switch_is_remembered() {
        let root = std::env::temp_dir().join(format!("canter-shid-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let store = Arc::new(SnapshotStore::new(&root, "reader"));
        let mut app = App::new(ORIGIN, Some(store.clone()));
        app.update(key(KeyCode::Char('/')));
        app.update(ctrl('h'));
        assert!(app.search_hidden);
        let next = App::new(ORIGIN, Some(store));
        assert!(next.search_hidden, "still on after a restart");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_palette_says_how_much_it_has_read() {
        let mut app = app_with_courses();
        assert_eq!(app.palette_coverage(), "searching 0 of 2 courses, still reading");
        let (k, r) = fetch_of(&app.update(key(KeyCode::Enter)));
        deliver(&mut app, k, r, Payload::Assignments(vec![]));
        app.update(key(KeyCode::Esc));
        assert_eq!(app.palette_coverage(), "searching 1 of 2 courses, still reading");
    }

    #[test]
    fn browser_urls_stay_on_origin() {
        let mut app = app_with_courses();
        assert_eq!(
            app.browser_url().unwrap(),
            "https://ufl.instructure.com/courses/10"
        );
        let (k, r) = fetch_of(&app.update(key(KeyCode::Enter)));
        let a = AssignmentSummary {
            id: "7".into(),
            html_url: Some("https://evil.example/x".into()),
            ..Default::default()
        };
        deliver(&mut app, k, r, Payload::Assignments(vec![a]));
        assert_eq!(
            app.browser_url().unwrap(),
            "https://ufl.instructure.com/courses/10/assignments/7"
        );
    }

    #[test]
    fn snapshot_is_shown_before_sync() {
        let root = std::env::temp_dir().join(format!("canter-app-{}", std::process::id()));
        let store = Arc::new(SnapshotStore::new(&root, "1"));
        store.save(
            &ResourceKey::Courses,
            &vec![course("5", "Saved")],
            Utc::now(),
        );
        let mut app = App::new("https://ufl.instructure.com", Some(store.clone()));
        let effects = app.start();
        assert_eq!(app.courses.origin, Origin::Cache);
        assert_eq!(app.courses.data.as_ref().unwrap()[0].id, "5");
        assert!(app.current_syncing());
        fetch_of(&effects);
        store.clear();
        let _ = std::fs::remove_dir_all(root);
    }

    // ---- Files and the files & links list ----

    const ORIGIN: &str = "https://ufl.instructure.com";

    fn temp_library(tag: &str) -> (Arc<Library>, PathBuf) {
        let root = std::env::temp_dir().join(format!("canter-app-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        (Arc::new(Library::open(&root, "ufl.instructure.com")), root)
    }

    fn canvas_file(id: &str, name: &str, folder: &str) -> CanvasFile {
        CanvasFile {
            id: id.into(),
            display_name: Some(name.into()),
            size: Some(5),
            updated_at: Some("2026-10-01T12:00:00Z".into()),
            url: Some(format!("{ORIGIN}/files/{id}/download?verifier=v")),
            folder_id: Some(folder.into()),
            ..Default::default()
        }
    }

    fn canvas_folder(id: &str, full: &str, parent: Option<&str>) -> CanvasFolder {
        CanvasFolder {
            id: id.into(),
            name: full.rsplit('/').next().map(str::to_string),
            full_name: Some(full.into()),
            parent_folder_id: parent.map(str::to_string),
            ..Default::default()
        }
    }

    fn course_files() -> CourseFilesData {
        CourseFilesData {
            folders: vec![
                canvas_folder("1", "course files", None),
                canvas_folder("2", "course files/Lectures", Some("1")),
                canvas_folder("3", "course files/Lectures/Week 1", Some("2")),
            ],
            files: Some(vec![
                canvas_file("100", "syllabus.pdf", "1"),
                canvas_file("101", "lecture1.pdf", "3"),
                canvas_file("102", "Notes.txt", "2"),
            ]),
        }
    }

    /// Courses -> GPU Computing -> Tab to Files, with the listing delivered.
    fn files_app(library: Option<Arc<Library>>) -> App {
        let mut app = app_with_courses();
        app.library = library;
        let (k, r) = fetch_of(&app.update(key(KeyCode::Enter)));
        deliver(&mut app, k, r, Payload::Assignments(vec![]));
        let (k, r) = fetch_of(&app.update(key(KeyCode::Tab)));
        assert_eq!(k, ResourceKey::Files("10".into()));
        deliver(&mut app, k, r, Payload::Files(Box::new(course_files())));
        app
    }

    fn row_names(app: &App) -> Vec<String> {
        app.file_rows()
            .iter()
            .map(|r| r.name().to_string())
            .collect()
    }

    /// What the runtime does for a download: write the file, install it, report.
    fn finish_download(app: &mut App, effect: &Effect, body: &str) -> Vec<Effect> {
        let Effect::Download {
            file,
            target,
            automatic,
        } = effect
        else {
            panic!("expected a download, got {effect:?}")
        };
        let lib = app.library.clone().unwrap();
        let partial = lib.temp_dir().join(format!("{}.part", file.id));
        std::fs::create_dir_all(partial.parent().unwrap()).unwrap();
        std::fs::write(&partial, body).unwrap();
        lib.install(&file.id, &partial, file.version(), target, *automatic)
            .unwrap();
        app.update(Msg::Transfer {
            file_id: file.id.clone(),
            event: TransferEvent::Done(target.clone()),
        })
    }

    #[test]
    fn files_tab_lists_folders_first_and_moves_between_folders_without_reading_again() {
        let mut app = files_app(None);
        assert_eq!(row_names(&app), vec!["Lectures", "syllabus.pdf"]);
        assert!(
            app.update(key(KeyCode::Enter)).is_empty(),
            "opening a folder uses the listing already read"
        );
        assert_eq!(row_names(&app), vec!["Week 1", "Notes.txt"]);
        assert_eq!(app.folder_path(), "Lectures");
        app.update(key(KeyCode::Enter));
        assert_eq!(row_names(&app), vec!["lecture1.pdf"]);
        assert!(app.update(key(KeyCode::Esc)).is_empty());
        assert!(app.update(key(KeyCode::Esc)).is_empty());
        assert_eq!(row_names(&app), vec!["Lectures", "syllabus.pdf"]);
        // Leaving Files for the course list is a new visit there.
        assert_eq!(
            fetch_of(&app.update(key(KeyCode::Esc))).0,
            ResourceKey::Courses
        );
    }

    #[test]
    fn tab_switches_tabs_and_leaves_all_open_folders() {
        let mut app = files_app(None);
        app.update(key(KeyCode::Enter));
        app.update(key(KeyCode::Enter));
        assert_eq!(app.stack.len(), 4);
        let (k, _) = fetch_of(&app.update(key(KeyCode::BackTab)));
        assert_eq!(k, ResourceKey::Assignments("10".into()));
        assert_eq!(app.stack.len(), 2);
        assert!(matches!(app.view().kind, ViewKind::Assignments { .. }));
        let (k, _) = fetch_of(&app.update(key(KeyCode::Tab)));
        assert_eq!(k, ResourceKey::Files("10".into()));
        assert_eq!(app.stack.len(), 2);
    }

    #[test]
    fn announcements_navigate_refresh_and_ignore_old_responses() {
        let mut app = files_app(None);
        let (k, r) = fetch_of(&app.update(key(KeyCode::Tab)));
        assert_eq!(k, ResourceKey::Announcements("10".into()));
        let a = crate::announcements::Summary {
            id: "8".into(),
            title: Some("Exam update".into()),
            ..Default::default()
        };
        deliver(&mut app, k.clone(), r, Payload::Announcements(vec![a]));
        assert_eq!(app.visible_rows(), [0]);
        let (detail_key, request) = fetch_of(&app.update(key(KeyCode::Enter)));
        assert_eq!(
            detail_key,
            ResourceKey::Announcement("10".into(), "8".into())
        );
        let detail = crate::announcements::Detail {
            base: crate::announcements::Summary {
                id: "8".into(),
                ..Default::default()
            },
            message: Some("<a href='/courses/10/files/9/download'>Notes</a>".into()),
            attachments: vec![
                canvas_file("9", "notes.pdf", "1"),
                canvas_file("11", "extra.txt", "1"),
            ],
            ..Default::default()
        };
        deliver(
            &mut app,
            detail_key,
            request,
            Payload::Announcement(Box::new(detail)),
        );
        assert_eq!(app.announcement_links("10", "8").len(), 2);
        app.update(key(KeyCode::Char('j')));
        assert_eq!(app.view().scroll, 1);
        app.update(key(KeyCode::Tab));
        assert!(app.browser_url().unwrap().ends_with("/files/9/download"));
        app.update(key(KeyCode::Esc)); // deselect link
        assert_eq!(fetch_of(&app.update(key(KeyCode::Esc))).0, k);
        let (_, newer) = fetch_of(&app.update(key(KeyCode::Char('r'))));
        deliver(&mut app, k.clone(), r, Payload::Announcements(vec![]));
        assert_eq!(app.visible_rows(), [0]);
        app.update(Msg::Fetched {
            key: k,
            request: newer,
            result: Err(CanvasError::NoAccess(403)),
            at: Utc::now(),
        });
        assert!(matches!(app.announcements["10"].sync, SyncState::Failed(_)));
        assert_eq!(app.visible_rows(), [0]);
        // Forward goes on to Modules; backward returns to Announcements.
        let effects = app.update(key(KeyCode::Tab));
        assert_eq!(fetch_of(&effects).0, ResourceKey::Modules("10".into()));
        let effects = app.update(key(KeyCode::BackTab));
        assert_eq!(
            fetch_of(&effects).0,
            ResourceKey::Announcements("10".into())
        );
    }

    #[test]
    fn local_announcement_reads_survive_refresh_restart_and_stay_account_scoped() {
        let root =
            std::env::temp_dir().join(format!("canter-local-reads-{}", std::process::id()));
        let store = Arc::new(SnapshotStore::new(&root, "reader"));
        let mut app = app_with_courses();
        app.store = Some(store.clone());
        app.update(key(KeyCode::Enter));
        app.update(key(KeyCode::BackTab)); // Pages, last before Canvas's tabs are known
        app.update(key(KeyCode::BackTab)); // Grades
        app.update(key(KeyCode::BackTab)); // Modules
        let (k, r) = fetch_of(&app.update(key(KeyCode::BackTab)));
        let summary = crate::announcements::Summary {
            id: "8".into(),
            read_state: Some("unread".into()),
            ..Default::default()
        };
        deliver(
            &mut app,
            k,
            r,
            Payload::Announcements(vec![summary.clone()]),
        );
        assert_eq!(app.announcement_status("10", &summary), "Unread");
        let (k, r) = fetch_of(&app.update(key(KeyCode::Enter)));
        assert_eq!(app.announcement_status("10", &summary), "Unread");
        let detail = crate::announcements::Detail {
            base: summary.clone(),
            message: Some("Read me".into()),
            locked_for_user: Some(true),
            ..Default::default()
        };
        deliver(&mut app, k, r, Payload::Announcement(Box::new(detail)));
        assert_eq!(app.announcement_status("10", &summary), "Read");
        let (k, r) = fetch_of(&app.update(key(KeyCode::Esc)));
        deliver(
            &mut app,
            k,
            r,
            Payload::Announcements(vec![summary.clone()]),
        );
        assert_eq!(app.announcement_status("10", &summary), "Read");
        assert_eq!(
            app.announcements["10"].data.as_ref().unwrap()[0]
                .read_state
                .as_deref(),
            Some("unread")
        );
        let restarted = App::new(&app.origin, Some(store));
        assert_eq!(restarted.announcement_status("10", &summary), "Read");
        assert_eq!(restarted.announcement_status("20", &summary), "Unread");
        let other = App::new(
            &app.origin,
            Some(Arc::new(SnapshotStore::new(&root, "other"))),
        );
        assert_eq!(other.announcement_status("10", &summary), "Unread");
        let _ = std::fs::remove_dir_all(root);
    }

    // ---- The Inbox ----

    fn conversation(id: &str, subject: &str, state: &str) -> crate::inbox::Summary {
        crate::inbox::Summary {
            id: id.into(),
            subject: Some(subject.into()),
            workflow_state: Some(state.into()),
            last_message_at: Some(Utc::now()),
            participants: vec![crate::inbox::Participant {
                id: "7".into(),
                name: Some("Dr Ada".into()),
            }],
            ..Default::default()
        }
    }

    /// The Inbox screen, opened the way a person opens it: Tab to the box on
    /// the home screen, then Enter.
    fn inbox_app(library: Option<Arc<Library>>) -> App {
        let mut app = app_with_courses();
        app.library = library;
        // Tab moves Courses -> Calendar -> the Inbox box.
        app.update(key(KeyCode::Tab));
        assert_eq!(app.focus(), Section::Calendar);
        app.update(key(KeyCode::Tab));
        assert_eq!(app.focus(), Section::Inbox);
        // The home screen only needs the count, so only Unread is read.
        let unread = match app.conversations["unread"].sync {
            SyncState::Syncing { request, .. } => request,
            ref other => panic!("expected Unread to be syncing, got {other:?}"),
        };
        deliver(
            &mut app,
            ResourceKey::Conversations("unread".into()),
            unread,
            Payload::Conversations(vec![]),
        );
        // Enter opens the Inbox itself, which reads the folder it shows.
        let (k, r) = fetch_of(&app.update(key(KeyCode::Enter)));
        assert_eq!(k, ResourceKey::Conversations("inbox".into()));
        deliver(
            &mut app,
            k,
            r,
            Payload::Conversations(vec![conversation("5", "Lab 3 feedback", "unread")]),
        );
        app
    }

    #[test]
    fn the_unread_count_falls_as_conversations_are_read_here() {
        let mut app = app_with_courses();
        app.update(key(KeyCode::Tab));
        app.update(key(KeyCode::Tab));
        let request = match app.conversations["unread"].sync {
            SyncState::Syncing { request, .. } => request,
            ref other => panic!("expected Unread to be syncing, got {other:?}"),
        };
        deliver(
            &mut app,
            ResourceKey::Conversations("unread".into()),
            request,
            Payload::Conversations(vec![
                conversation("5", "Lab 3 feedback", "unread"),
                conversation("6", "Office hours moved", "unread"),
            ]),
        );
        assert_eq!(app.unread_conversations(), 2);
        // Open the Inbox, open the first conversation, and read it.
        let (k, r) = fetch_of(&app.update(key(KeyCode::Enter)));
        deliver(
            &mut app,
            k,
            r,
            Payload::Conversations(vec![conversation("5", "Lab 3 feedback", "unread")]),
        );
        let (k, r) = fetch_of(&app.update(key(KeyCode::Enter)));
        let detail = crate::inbox::Detail {
            base: conversation("5", "Lab 3 feedback", "unread"),
            messages: vec![crate::inbox::Message {
                id: "9".into(),
                created_at: Some(Utc::now()),
                body: Some("Notes are attached.".into()),
                author_id: Some("7".into()),
                ..Default::default()
            }],
        };
        deliver(&mut app, k, r, Payload::Conversation(Box::new(detail)));
        // Canvas still calls it unread, and is never told otherwise; the
        // count on the home screen is the one that moved.
        assert_eq!(app.unread_conversations(), 1);
        assert_eq!(
            app.conversations["unread"]
                .data
                .as_ref()
                .map(|c| c.len())
                .unwrap_or(0),
            2
        );
    }

    #[test]
    fn the_inbox_is_a_screen_with_every_folder_and_opens_a_conversation() {
        let mut app = inbox_app(None);
        assert!(matches!(app.view().kind, ViewKind::Inbox { .. }));
        assert!(app.scopes().contains(&Scope::Inbox));
        assert_eq!(app.inbox_rows().len(), 1);
        assert_eq!(app.selected_conversation().unwrap().id, "5");
        assert!(app.browser_url().unwrap().ends_with("/conversations/5"));

        // Right and left reach every folder, each read as it is shown.
        let step = |app: &mut App, code: KeyCode| {
            let (k, r) = fetch_of(&app.update(key(code)));
            assert_eq!(k, ResourceKey::Conversations(app.inbox_folder.id().into()));
            deliver(app, k, r, Payload::Conversations(vec![]));
            app.inbox_folder.id().to_string()
        };
        let mut seen = vec!["inbox".to_string()];
        for _ in 1..crate::inbox::Folder::ALL.len() {
            seen.push(step(&mut app, KeyCode::Right));
        }
        assert_eq!(seen, ["inbox", "unread", "starred", "sent", "archived"]);
        // A folder with nothing in it has nothing to open.
        assert!(!app.screen_available(Act::Open));
        assert!(app.update(key(KeyCode::Enter)).is_empty());
        // Past the end it cycles round, and Left goes back the other way.
        assert_eq!(step(&mut app, KeyCode::Right), "inbox");
        assert_eq!(step(&mut app, KeyCode::Left), "archived");
        // Back to the Inbox, read again, listing its conversation.
        let (k, r) = fetch_of(&app.update(key(KeyCode::Right)));
        assert_eq!(k, ResourceKey::Conversations("inbox".into()));
        deliver(
            &mut app,
            k,
            r,
            Payload::Conversations(vec![conversation("5", "Lab 3 feedback", "unread")]),
        );

        // Enter opens the conversation as a page of its own.
        let (k, r) = fetch_of(&app.update(key(KeyCode::Enter)));
        assert_eq!(k, ResourceKey::Conversation("5".into()));
        assert_eq!(
            app.view().kind,
            ViewKind::Conversation {
                conversation_id: "5".into(),
                subject: "Lab 3 feedback".into()
            }
        );
        let detail = crate::inbox::Detail {
            base: conversation("5", "Lab 3 feedback", "unread"),
            messages: vec![crate::inbox::Message {
                id: "9".into(),
                created_at: Some(Utc::now()),
                body: Some("Notes are attached.".into()),
                author_id: Some("7".into()),
                attachments: vec![canvas_file("9", "notes.pdf", "1")],
                ..Default::default()
            }],
        };
        deliver(&mut app, k, r, Payload::Conversation(Box::new(detail)));
        assert!(app.scopes().contains(&Scope::Conversation));
        assert!(app.scopes().contains(&Scope::Sections));
        // The messages, their author and date, and the attachment.
        let text = app.conversation_document("5");
        assert!(text.contains("Dr Ada"), "{text}");
        assert!(text.contains("Notes are attached."), "{text}");
        let links = app.conversation_links("5");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target, Target::File { id: "9".into() });
        assert!(app.browser_url().unwrap().ends_with("/conversations/5"));
        // Esc leaves the conversation for the Inbox it was opened from, and
        // again for the home screen. Each is a new visit, so each is re-read.
        assert_eq!(
            fetch_of(&app.update(key(KeyCode::Esc))).0,
            ResourceKey::Conversations("inbox".into())
        );
        assert!(matches!(app.view().kind, ViewKind::Inbox { .. }));
        assert_eq!(
            fetch_of(&app.update(key(KeyCode::Esc))).0,
            ResourceKey::Courses
        );
        assert_eq!(app.focus(), Section::Inbox, "back on the Inbox box");
    }

    #[test]
    fn conversation_files_are_downloaded_only_when_chosen() {
        let (lib, root) = temp_library("inbox-files");
        let mut app = inbox_app(Some(lib));
        let (k, r) = fetch_of(&app.update(key(KeyCode::Enter)));
        let detail = crate::inbox::Detail {
            base: conversation("5", "Lab 3 feedback", "unread"),
            messages: vec![crate::inbox::Message {
                id: "9".into(),
                created_at: Some(Utc::now()),
                body: Some("<img src='/courses/10/files/12/preview'>".into()),
                author_id: Some("7".into()),
                attachments: vec![canvas_file("9", "notes.pdf", "1")],
                ..Default::default()
            }],
        };
        // Reading the conversation downloads nothing, not even its image.
        let effects = {
            deliver(&mut app, k, r, Payload::Conversation(Box::new(detail)));
            app.update(Msg::Tick)
        };
        assert!(
            !effects.iter().any(|e| matches!(e, Effect::Download { .. })),
            "{effects:?}"
        );
        assert_eq!(app.conversation_links("5").len(), 2);
        // Choosing one reads its details first, then downloads it.
        app.update(key(KeyCode::Tab)); // to the files & links
        assert_eq!(app.focus(), Section::Links);
        let effects = app.update(key(KeyCode::Char('d')));
        let (k, r) = fetch_of(&effects);
        assert_eq!(k, ResourceKey::File("12".into()));
        let effects = app.update(Msg::Fetched {
            key: k,
            request: r,
            result: Ok((
                Payload::File(Box::new(canvas_file("12", "chart.png", "1"))),
                FetchStats::default(),
            )),
            at: Utc::now(),
        });
        let Some(Effect::Download { target, .. }) = effects
            .iter()
            .find(|e| matches!(e, Effect::Download { .. }))
        else {
            panic!("expected a download, got {effects:?}")
        };
        assert!(
            target.starts_with(root.join("Inbox Files").join("5")),
            "{}",
            target.display()
        );
        // A conversation id that could escape the library gets no folder.
        assert!(app.conversation_dir("../etc").is_none());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn conversations_read_here_are_never_reported_to_canvas() {
        let root = std::env::temp_dir().join(format!("canter-inbox-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let store = Arc::new(SnapshotStore::new(&root, "reader"));
        let mut app = inbox_app(None);
        app.store = Some(store.clone());
        let summary = conversation("5", "Lab 3 feedback", "unread");
        assert_eq!(app.conversation_status(&summary), "Unread");
        // Opening it asks Canvas for the conversation and for nothing else:
        // no write, and no second request that would change its state.
        let effects = app.update(key(KeyCode::Enter));
        assert_eq!(effects.len(), 1, "{effects:?}");
        let (k, r) = fetch_of(&effects);
        assert_eq!(k, ResourceKey::Conversation("5".into()));
        // Still unread until its messages are actually on screen.
        assert_eq!(app.conversation_status(&summary), "Unread");
        let detail = crate::inbox::Detail {
            base: summary.clone(),
            messages: vec![crate::inbox::Message {
                id: "9".into(),
                body: Some("Read me".into()),
                ..Default::default()
            }],
        };
        deliver(&mut app, k, r, Payload::Conversation(Box::new(detail)));
        assert_eq!(app.conversation_status(&summary), "Read");
        // A later refresh still reports Canvas's own unread state, and the
        // reader keeps showing Read.
        // Esc returns to the Inbox, which re-reads the folder. Canvas still
        // reports the conversation as unread, and the reader keeps its own
        // mark alongside that.
        let (k, r) = fetch_of(&app.update(key(KeyCode::Esc)));
        assert_eq!(k, ResourceKey::Conversations("inbox".into()));
        deliver(&mut app, k, r, Payload::Conversations(vec![summary.clone()]));
        assert_eq!(app.conversation_status(&summary), "Read");
        assert_eq!(
            app.conversations["inbox"].data.as_ref().unwrap()[0]
                .workflow_state
                .as_deref(),
            Some("unread"),
            "Canvas's own state is left as Canvas reports it"
        );
        // It survives a restart and belongs to this account only.
        let restarted = App::new(&app.origin, Some(store));
        assert_eq!(restarted.conversation_status(&summary), "Read");
        let other = App::new(
            &app.origin,
            Some(Arc::new(SnapshotStore::new(&root, "other"))),
        );
        assert_eq!(other.conversation_status(&summary), "Unread");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn an_empty_or_failed_conversation_is_not_marked_read() {
        for fail in [false, true] {
            let mut app = inbox_app(None);
            let summary = conversation("5", "Lab 3 feedback", "unread");
            let (k, r) = fetch_of(&app.update(key(KeyCode::Enter)));
            if fail {
                app.update(Msg::Fetched {
                    key: k,
                    request: r,
                    result: Err(CanvasError::NoAccess(403)),
                    at: Utc::now(),
                });
            } else {
                let detail = crate::inbox::Detail {
                    base: summary.clone(),
                    messages: vec![],
                };
                deliver(&mut app, k, r, Payload::Conversation(Box::new(detail)));
            }
            assert_eq!(app.conversation_status(&summary), "Unread");
        }
    }

    /// The whole reply path: R opens a draft, typing fills it, the review
    /// names the recipients, and only y produces the request.
    #[test]
    fn a_reply_is_reviewed_and_sent_only_on_an_explicit_key() {
        let mut app = inbox_app(None);
        let summary = conversation("5", "Lab 3 feedback", "unread");
        let (k, r) = fetch_of(&app.update(key(KeyCode::Enter)));
        let detail = crate::inbox::Detail {
            base: summary.clone(),
            messages: vec![crate::inbox::Message {
                id: "9".into(),
                body: Some("How did the lab go?".into()),
                ..Default::default()
            }],
        };
        deliver(&mut app, k, r, Payload::Conversation(Box::new(detail)));

        assert!(app.update(key(KeyCode::Char('R'))).is_empty());
        assert!(app.message.is_some(), "R opens a draft");
        assert_eq!(app.scopes(), vec![Scope::Compose]);
        // An empty message cannot even be reviewed.
        app.update(ctrl('d'));
        assert!(!app.message.as_ref().unwrap().reviewing);
        for c in "It went well".chars() {
            app.update(key(KeyCode::Char(c)));
        }
        app.update(key(KeyCode::Enter));
        app.update(key(KeyCode::Char('.')));
        assert_eq!(app.message.as_ref().unwrap().body, "It went well\n.");

        // Reviewing shows what will be sent and sends nothing by itself.
        assert!(app.update(ctrl('d')).is_empty());
        assert!(app.message.as_ref().unwrap().reviewing);
        assert_eq!(app.scopes(), vec![Scope::MessageReview]);
        let draft = app.message.clone().unwrap();
        assert!(matches!(&draft.to, Audience::Conversation { who, .. } if who.contains("Dr Ada")));
        assert_eq!(
            draft.send(),
            crate::inbox::Send::Reply {
                conversation_id: "5".into(),
                body: "It went well\n.".into(),
            }
        );
        // Every other key leaves the request unmade.
        for code in [KeyCode::Char('x'), KeyCode::Enter, KeyCode::Down] {
            assert!(app.update(key(code)).is_empty(), "{code:?} sent something");
        }
        let effects = app.update(key(KeyCode::Char('y')));
        assert_eq!(effects.len(), 1, "{effects:?}");
        assert!(matches!(
            &effects[0],
            Effect::SendMessage(m) if **m == draft.send()
        ));
        assert!(app.message.as_ref().unwrap().busy);

        // Canvas accepting it clears the draft and reads the thread again.
        let effects = app.update(Msg::MessageSent {
            conversation_id: Some("5".into()),
            result: Ok("Reply sent.".into()),
        });
        assert!(app.message.is_none());
        let keys: Vec<ResourceKey> = effects
            .iter()
            .filter_map(|e| match e {
                Effect::Fetch { key, .. } => Some(key.clone()),
                _ => None,
            })
            .collect();
        assert!(
            keys.contains(&ResourceKey::Conversation("5".into())),
            "{keys:?}"
        );
    }

    /// Esc anywhere in the flow throws the message away without a request,
    /// and a new message to a course's teachers names the course it found.
    #[test]
    fn a_message_to_teachers_names_the_course_and_esc_sends_nothing() {
        let mut app = app_with_courses();
        assert!(app.update(key(KeyCode::Char('N'))).is_empty());
        let draft = app.message.clone().expect("N opens a draft");
        assert!(matches!(
            &draft.to,
            Audience::Teachers { course_id, course_name }
                if course_id == "10" && course_name == "GPU Computing"
        ));
        assert_eq!(draft.field, MessageField::Subject);
        for c in "Office hours".chars() {
            app.update(key(KeyCode::Char(c)));
        }
        // Enter moves on from the subject rather than adding a line to it.
        app.update(key(KeyCode::Enter));
        assert_eq!(app.message.as_ref().unwrap().subject, "Office hours");
        assert_eq!(app.message.as_ref().unwrap().field, MessageField::Body);
        for c in "Are you free Friday?".chars() {
            app.update(key(KeyCode::Char(c)));
        }
        assert_eq!(
            app.message.as_ref().unwrap().send(),
            crate::inbox::Send::ToTeachers {
                course_id: "10".into(),
                subject: "Office hours".into(),
                body: "Are you free Friday?".into(),
            }
        );
        app.update(ctrl('d'));
        assert!(app.message.as_ref().unwrap().reviewing);
        // Esc on the review goes back to writing; Esc again discards it.
        assert!(app.update(key(KeyCode::Esc)).is_empty());
        assert!(!app.message.as_ref().unwrap().reviewing);
        assert!(app.update(key(KeyCode::Esc)).is_empty());
        assert!(app.message.is_none(), "Esc discards the message");
    }

    #[test]
    fn unavailable_and_unseen_announcement_details_are_not_marked_read() {
        for (leave, restricted, fail) in [
            (true, false, false),
            (false, true, false),
            (false, false, true),
        ] {
            let mut app = app_with_courses();
            app.update(key(KeyCode::Enter));
            app.update(key(KeyCode::BackTab)); // Pages, last before Canvas's tabs are known
            app.update(key(KeyCode::BackTab)); // Grades
            app.update(key(KeyCode::BackTab)); // Modules
            let (k, r) = fetch_of(&app.update(key(KeyCode::BackTab)));
            let summary = crate::announcements::Summary {
                id: "8".into(),
                read_state: Some("unread".into()),
                ..Default::default()
            };
            deliver(
                &mut app,
                k,
                r,
                Payload::Announcements(vec![summary.clone()]),
            );
            let (k, r) = fetch_of(&app.update(key(KeyCode::Enter)));
            if leave {
                app.update(key(KeyCode::Esc));
            }
            let detail = crate::announcements::Detail {
                base: summary.clone(),
                message: Some("Message".into()),
                lock_info: restricted.then_some(crate::announcements::LockInfo {
                    can_view: Some(false),
                }),
                ..Default::default()
            };
            let result = if fail {
                Err(CanvasError::NoAccess(403))
            } else {
                Ok((
                    Payload::Announcement(Box::new(detail)),
                    FetchStats::default(),
                ))
            };
            app.update(Msg::Fetched {
                key: k,
                request: r,
                result,
                at: Utc::now(),
            });
            assert_eq!(app.announcement_status("10", &summary), "Unread");
            if leave {
                // The completed detail is now displayed immediately on reentry.
                app.update(key(KeyCode::Enter));
                assert_eq!(app.announcement_status("10", &summary), "Read");
            }
        }
    }

    #[test]
    fn announcement_file_choices_refresh_metadata_before_reusing_local_files() {
        let (library, root) = temp_library("announcement-file");
        let mut app = app_with_courses();
        app.library = Some(library);
        let dir = app.announcement_dir("10", "8");
        let effects = app.place_announcement_file("9", dir.clone(), true);
        assert_eq!(fetch_of(&effects).0, ResourceKey::File("9".into()));
        let effects = deliver_file(&mut app, canvas_file("9", "notes.pdf", "1"));
        let download = effects
            .iter()
            .find(|e| matches!(e, Effect::Download { .. }))
            .unwrap();
        let result = finish_download(&mut app, download, "notes");
        assert!(result.iter().any(|e| matches!(e, Effect::OpenPath(_))));
        // Even with a saved copy, check its current Canvas version on the next choice.
        let effects = app.place_announcement_file("9", dir, true);
        assert_eq!(fetch_of(&effects).0, ResourceKey::File("9".into()));
        assert!(!effects.iter().any(|e| matches!(e, Effect::OpenPath(_))));
        let effects = deliver_file(&mut app, canvas_file("9", "notes.pdf", "1"));
        assert!(effects.iter().any(|e| matches!(e, Effect::OpenPath(_))));
        assert!(!effects.iter().any(|e| matches!(e, Effect::Download { .. })));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn announcement_snapshots_show_while_syncing_and_closed_comments_keep_links() {
        let root =
            std::env::temp_dir().join(format!("canter-announcements-{}", std::process::id()));
        let store = Arc::new(SnapshotStore::new(&root, "test"));
        let key = ResourceKey::Announcements("10".into());
        store.save(
            &key,
            &vec![crate::announcements::Summary {
                id: "8".into(),
                title: Some("Saved notice".into()),
                ..Default::default()
            }],
            Utc::now(),
        );
        let mut app = App::new("https://ufl.instructure.com", Some(store));
        app.stack.push(View::new(ViewKind::Announcements {
            course_id: "10".into(),
            course_name: "Course".into(),
        }));
        let effects = app.enter_current(false);
        assert_eq!(fetch_of(&effects).0, key);
        assert_eq!(app.announcements["10"].origin, Origin::Cache);
        assert!(app.current_syncing());
        assert_eq!(app.visible_rows(), [0]);
        let (key, request) = fetch_of(&app.update(super::tests::key(KeyCode::Enter)));
        let d = crate::announcements::Detail {
            base: crate::announcements::Summary {
                id: "8".into(),
                ..Default::default()
            },
            message: Some("<a href='https://example.org'>Schedule</a>".into()),
            locked_for_user: Some(true),
            ..Default::default()
        };
        deliver(&mut app, key, request, Payload::Announcement(Box::new(d)));
        assert_eq!(app.announcement_links("10", "8").len(), 1);
        let d = app
            .announcement_details
            .get_mut(&("10".into(), "8".into()))
            .unwrap()
            .data
            .as_mut()
            .unwrap();
        d.lock_info = Some(crate::announcements::LockInfo {
            can_view: Some(false),
        });
        assert!(app.announcement_links("10", "8").is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_file_is_found_wherever_it_sits_in_the_course() {
        // The Files screen shows one folder at a time, and typing no longer
        // flattens it. Reaching a file by name is the palette's job now, so
        // this checks the capability moved rather than went away.
        let mut app = files_app(None);
        let here = row_names(&app);
        app.update(key(KeyCode::Char('/')));
        for c in "lecture".chars() {
            app.update(key(KeyCode::Char(c)));
        }
        let found: Vec<String> = app
            .palette_hits()
            .into_iter()
            .filter(|h| h.kind == crate::search::Kind::File)
            .map(|h| h.title)
            .collect();
        assert!(
            found.iter().any(|n| n == "lecture1.pdf"),
            "the palette finds it: {found:?}"
        );
        assert!(
            !here.iter().any(|n| *n == "lecture1.pdf"),
            "and it is not in the folder on screen, so it was really found"
        );
    }

    #[test]
    fn hidden_files_area_lists_nothing() {
        let mut app = app_with_courses();
        let (k, r) = fetch_of(&app.update(key(KeyCode::Enter)));
        deliver(&mut app, k, r, Payload::Assignments(vec![]));
        let (k, r) = fetch_of(&app.update(key(KeyCode::Tab)));
        let hidden = CourseFilesData {
            folders: course_files().folders,
            files: None,
        };
        deliver(&mut app, k, r, Payload::Files(Box::new(hidden)));
        assert!(app.file_rows().is_empty());
        assert!(app.update(key(KeyCode::Char('D'))).is_empty());
        assert!(app.notice.as_deref().unwrap_or("").contains("hidden"));
    }

    #[test]
    fn opening_a_file_downloads_it_once_then_opens_the_saved_copy() {
        let (lib, root) = temp_library("open");
        let mut app = files_app(Some(lib.clone()));
        app.update(key(KeyCode::Down));
        let effects = app.update(key(KeyCode::Enter));
        let target = root
            .join("GPU Computing")
            .join("Class Files")
            .join("syllabus.pdf");
        assert!(
            matches!(&effects[..], [Effect::Download { target: t, automatic: false, .. }] if *t == target),
            "{effects:?}"
        );
        assert_eq!(
            app.local_state(&canvas_file("100", "syllabus.pdf", "1")),
            Local::Queued
        );
        assert_eq!(
            finish_download(&mut app, &effects[0], "hello"),
            vec![Effect::OpenPath(target.clone())]
        );
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "hello");
        assert_eq!(
            app.local_state(&canvas_file("100", "syllabus.pdf", "1")),
            Local::Saved
        );
        // Up to date: opens straight away, no second download.
        assert_eq!(
            app.update(key(KeyCode::Enter)),
            vec![Effect::OpenPath(target)]
        );
        // A newer version on Canvas is shown as such.
        let mut newer = canvas_file("100", "syllabus.pdf", "1");
        newer.updated_at = Some("2026-10-02T12:00:00Z".into());
        assert_eq!(app.local_state(&newer), Local::Changed);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn download_all_takes_subfolders_and_keeps_canvas_folders() {
        let (lib, root) = temp_library("all");
        let mut app = files_app(Some(lib));
        let effects = app.update(key(KeyCode::Char('D')));
        let mut targets: Vec<PathBuf> = effects
            .iter()
            .map(|e| match e {
                Effect::Download { target, .. } => {
                    target.strip_prefix(&root).unwrap().to_path_buf()
                }
                other => panic!("unexpected {other:?}"),
            })
            .collect();
        targets.sort();
        let class = PathBuf::from("GPU Computing").join("Class Files");
        assert_eq!(
            targets,
            vec![
                class.join("Lectures").join("Notes.txt"),
                class.join("Lectures").join("Week 1").join("lecture1.pdf"),
                class.join("syllabus.pdf"),
            ]
        );
        assert!(
            app.notice
                .as_deref()
                .unwrap()
                .starts_with("Downloading 3 of 3 files")
        );
        // Pressing again while they run starts nothing new.
        assert!(app.update(key(KeyCode::Char('D'))).is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    fn detail_app(library: Option<Arc<Library>>, description: &str) -> App {
        let mut app = app_with_courses();
        app.library = library;
        let (k, r) = fetch_of(&app.update(key(KeyCode::Enter)));
        let a = AssignmentSummary {
            id: "7".into(),
            name: Some("Lab-2".into()),
            ..Default::default()
        };
        deliver(&mut app, k, r, Payload::Assignments(vec![a.clone()]));
        let (k, r) = fetch_of(&app.update(key(KeyCode::Enter)));
        let detail = AssignmentDetail {
            base: a,
            description: Some(description.into()),
            ..Default::default()
        };
        let effects = app.update(Msg::Fetched {
            key: k,
            request: r,
            result: Ok((Payload::Detail(Box::new(detail)), FetchStats::default())),
            at: Utc::now(),
        });
        assert_eq!(
            effects
                .iter()
                .filter(|e| matches!(
                    e,
                    Effect::Fetch {
                        key: ResourceKey::File(_),
                        ..
                    }
                ))
                .count(),
            3,
            "every linked file's details are read"
        );
        app.notice = None;
        app
    }

    fn deliver_file(app: &mut App, file: CanvasFile) -> Vec<Effect> {
        let key = ResourceKey::File(file.id.clone());
        let request = match app.sync_of(&key) {
            Some(SyncState::Syncing { request, .. }) => *request,
            other => panic!("no read in flight for {key:?}: {other:?}"),
        };
        app.update(Msg::Fetched {
            key,
            request,
            result: Ok((Payload::File(Box::new(file)), FetchStats::default())),
            at: Utc::now(),
        })
    }

    const DESCRIPTION: &str = r##"<p>Read <a href="/courses/10/files/22/download?verifier=v">the handout</a>,
        see <a href="https://github.com/a/b">the repo</a> and <a href="/courses/10/assignments/8">Lab-1</a>.</p>
        <p><img src="/courses/10/files/33/preview" alt="chart"><img src="/courses/10/files/34/preview" alt="chart 2"></p>"##;

    #[test]
    fn assignment_images_download_automatically_and_names_never_collide() {
        let (lib, root) = temp_library("images");
        let mut app = detail_app(Some(lib), DESCRIPTION);
        assert_eq!(app.links_of("10", "7").len(), 5);
        // The handout is only read; images are fetched too, each under its own name.
        assert!(deliver_file(&mut app, canvas_file("22", "handout.pdf", "1")).is_empty());
        let first = deliver_file(&mut app, canvas_file("33", "image.png", "1"));
        let second = deliver_file(&mut app, canvas_file("34", "image.png", "1"));
        let dir = root
            .join("GPU Computing")
            .join("Assignment Files")
            .join("Lab-2");
        assert!(
            matches!(&first[..], [Effect::Download { target, automatic: true, .. }] if *target == dir.join("image.png")),
            "{first:?}"
        );
        assert!(
            matches!(&second[..], [Effect::Download { target, automatic: true, .. }] if *target == dir.join("image (34).png")),
            "{second:?}"
        );
        assert!(
            finish_download(&mut app, &first[0], "a").is_empty(),
            "automatic downloads open nothing"
        );
        assert!(app.notice.is_none(), "and say nothing");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn links_list_selects_and_opens_each_kind() {
        let (lib, root) = temp_library("links");
        let mut app = detail_app(Some(lib), DESCRIPTION);
        // Tab moves to the files & links; the handout's details are still being read.
        app.update(key(KeyCode::Tab));
        assert_eq!(app.focus(), Section::Links);
        assert_eq!(app.view().link, 0);
        assert!(
            app.update(key(KeyCode::Enter)).is_empty(),
            "waits for the file's details"
        );
        let effects = deliver_file(&mut app, canvas_file("22", "handout.pdf", "1"));
        let target = root
            .join("GPU Computing")
            .join("Assignment Files")
            .join("Lab-2")
            .join("handout.pdf");
        assert!(
            matches!(&effects[..], [Effect::Download { target: t, automatic: false, .. }] if *t == target),
            "{effects:?}"
        );
        assert_eq!(
            finish_download(&mut app, &effects[0], "pdf"),
            vec![Effect::OpenPath(target)]
        );
        // A web link opens in the browser; o on a file opens its Canvas page.
        assert_eq!(
            app.browser_url().unwrap(),
            format!("{ORIGIN}/courses/10/files/22?verifier=v")
        );
        app.update(key(KeyCode::Down));
        assert_eq!(
            app.update(key(KeyCode::Enter)),
            vec![Effect::OpenUrl("https://github.com/a/b".into())]
        );
        // Another assignment opens in the reader.
        app.update(key(KeyCode::Char('3')));
        let (k, _) = fetch_of(&app.update(key(KeyCode::Enter)));
        assert_eq!(k, ResourceKey::Detail("10".into(), "8".into()));
        assert!(matches!(&app.view().kind, ViewKind::Detail { title, .. } if title == "Lab-1"));
        // Back again: the list keeps its place; Esc returns to the text, then leaves.
        app.update(key(KeyCode::Esc));
        assert_eq!(app.focus(), Section::Links);
        assert_eq!(app.view().link, 2);
        assert!(app.update(key(KeyCode::Esc)).is_empty());
        assert_eq!(app.focus(), Section::Text);
        assert_eq!(app.stack.len(), 3);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn failed_download_reports_and_flags_sign_in() {
        let (lib, root) = temp_library("fail");
        let mut app = files_app(Some(lib));
        app.update(key(KeyCode::Down));
        let effects = app.update(key(KeyCode::Char('d')));
        assert!(matches!(&effects[..], [Effect::Download { .. }]));
        app.update(Msg::Transfer {
            file_id: "100".into(),
            event: TransferEvent::Failed {
                message: "signed out or session expired".into(),
                needs_login: true,
            },
        });
        assert!(app.login_required);
        assert!(app.notice.as_deref().unwrap().contains("syllabus.pdf"));
        assert!(matches!(
            app.local_state(&canvas_file("100", "syllabus.pdf", "1")),
            Local::Failed(_)
        ));
        // Trying again starts a new download.
        assert!(matches!(
            &app.update(key(KeyCode::Char('d')))[..],
            [Effect::Download { .. }]
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_finished_download_lets_the_picture_be_placed() {
        // The image box is built while the text is wrapped, so the wrapped
        // text has to be dropped when the file it needs arrives. Nothing else
        // about the page changes, and before this the picture only showed
        // after a refresh or a resize.
        let mut app = files_app(None);
        let render = || DetailRender {
            key: ("10".into(), "1".into()),
            version: 1,
            width: 80,
            body: crate::images::Body::plain(vec!["text [1]".into()]),
        };
        app.detail_render = Some(render());
        app.announcement_render = Some(render());
        app.wiki_render = Some(render());
        app.conversation_render = Some(render());
        app.update(Msg::Transfer {
            file_id: "99".into(),
            event: TransferEvent::Done(PathBuf::from("image.png")),
        });
        assert!(app.detail_render.is_none());
        assert!(app.announcement_render.is_none());
        assert!(app.wiki_render.is_none());
        assert!(app.conversation_render.is_none());
    }

    #[test]
    fn locked_files_are_not_downloaded() {
        let (lib, root) = temp_library("locked");
        let mut app = files_app(Some(lib));
        if let Some(files) = app
            .course_files
            .get_mut("10")
            .and_then(|r| r.data.as_mut())
            .and_then(|d| d.files.as_mut())
        {
            files[0].url = None;
            files[0].locked_for_user = Some(true);
        }
        app.update(key(KeyCode::Down));
        assert!(app.update(key(KeyCode::Enter)).is_empty());
        assert!(app.notice.as_deref().unwrap().contains("locked"));
        let _ = std::fs::remove_dir_all(root);
    }

    // ---- Course tabs and modules ----

    fn tab(id: &str) -> crate::modules::Tab {
        crate::modules::Tab {
            id: id.into(),
            ..Default::default()
        }
    }

    fn fetches(effects: &[Effect]) -> Vec<ResourceKey> {
        effects
            .iter()
            .filter_map(|e| match e {
                Effect::Fetch { key, .. } => Some(key.clone()),
                _ => None,
            })
            .collect()
    }

    /// Deliver the read in flight for `key` and return what the app does next.
    fn answer(app: &mut App, key: ResourceKey, payload: Payload) -> Vec<Effect> {
        let request = match app.sync_of(&key) {
            Some(SyncState::Syncing { request, .. }) => *request,
            other => panic!("no read in flight for {key:?}: {other:?}"),
        };
        app.update(Msg::Fetched {
            key,
            request,
            result: Ok((payload, FetchStats::default())),
            at: Utc::now(),
        })
    }

    fn tabs_key() -> ResourceKey {
        ResourceKey::Tabs("10".into())
    }

    #[test]
    fn course_tabs_follow_canvas_and_are_read_once_per_run() {
        let mut app = app_with_courses();
        let effects = app.update(key(KeyCode::Enter));
        // Before Canvas's tabs are known the course opens on Assignments.
        assert!(matches!(app.view().kind, ViewKind::Assignments { .. }));
        assert!(fetches(&effects).contains(&tabs_key()));
        let effects = answer(
            &mut app,
            tabs_key(),
            Payload::Tabs(vec![tab("home"), tab("announcements"), tab("modules")]),
        );
        // Assignments is hidden on Canvas: the first section Canvas shows instead.
        assert!(matches!(app.view().kind, ViewKind::Announcements { .. }));
        assert_eq!(fetches(&effects), [ResourceKey::Announcements("10".into())]);
        assert_eq!(
            app.course_tabs("10"),
            [CourseTab::Announcements, CourseTab::Modules]
        );
        // Tab cycles only what Canvas shows.
        let effects = app.update(key(KeyCode::Tab));
        assert_eq!(fetch_of(&effects).0, ResourceKey::Modules("10".into()));
        app.update(key(KeyCode::Tab));
        assert!(matches!(app.view().kind, ViewKind::Announcements { .. }));
        // Coming back uses the tabs already read this run.
        app.update(key(KeyCode::Esc));
        let effects = app.update(key(KeyCode::Enter));
        assert!(matches!(app.view().kind, ViewKind::Announcements { .. }));
        assert!(!fetches(&effects).contains(&tabs_key()));
        // `r` reads them again.
        assert!(fetches(&app.update(key(KeyCode::Char('r')))).contains(&tabs_key()));
    }

    #[test]
    fn saved_tabs_are_used_at_once_and_read_again_once_per_run() {
        let root = std::env::temp_dir().join(format!("canter-tabs-{}", std::process::id()));
        let store = Arc::new(SnapshotStore::new(&root, "reader"));
        store.save(&tabs_key(), &vec![tab("files")], Utc::now());
        let mut app = app_with_courses();
        app.store = Some(store);
        let effects = app.update(key(KeyCode::Enter));
        assert!(matches!(app.view().kind, ViewKind::Files { .. }));
        assert_eq!(
            fetches(&effects),
            [ResourceKey::Files("10".into()), tabs_key()]
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_modules_home_counts_as_shown_and_no_sections_leaves_the_course_page() {
        let mut app = App::new(ORIGIN, None);
        let (k, r) = fetch_of(&app.start());
        let mut home = course("10", "GPU Computing");
        home.default_view = Some("modules".into());
        deliver(
            &mut app,
            k,
            r,
            Payload::Courses(vec![home, course("20", "Compilers")]),
        );
        app.update(key(KeyCode::Enter));
        answer(&mut app, tabs_key(), Payload::Tabs(vec![tab("home")]));
        assert!(matches!(app.view().kind, ViewKind::Modules { .. }));
        assert!(!app.available(Act::NextSection));
        app.update(key(KeyCode::Esc));
        app.update(key(KeyCode::Down));
        app.update(key(KeyCode::Enter));
        let other = ResourceKey::Tabs("20".into());
        answer(
            &mut app,
            other.clone(),
            Payload::Tabs(vec![tab("home"), tab("syllabus")]),
        );
        assert!(matches!(app.view().kind, ViewKind::CourseHome { .. }));
        assert_eq!(app.browser_url().unwrap(), format!("{ORIGIN}/courses/20"));
        // A refresh that finds a section shown again opens it.
        assert_eq!(
            fetches(&app.update(key(KeyCode::Char('r')))),
            [other.clone()]
        );
        answer(&mut app, other, Payload::Tabs(vec![tab("files")]));
        assert!(matches!(app.view().kind, ViewKind::Files { .. }));
    }

    const MODULES: &str = r#"[{"id":1,"name":"Week 1","state":"started","items":[
        {"id":11,"type":"SubHeader","title":"Read first"},
        {"id":12,"type":"Page","title":"Intro","page_url":"intro","completion_requirement":{"type":"must_view","completed":true}},
        {"id":13,"type":"Assignment","title":"Lab 1","content_id":55,"indent":1},
        {"id":14,"type":"File","title":"lecture1.pdf","content_id":101},
        {"id":15,"type":"Quiz","title":"Quiz 1","content_id":4,"html_url":"https://ufl.instructure.com/courses/10/modules/items/15"},
        {"id":16,"type":"ExternalUrl","title":"Reading","html_url":"https://example.com/x"}]},
        {"id":2,"name":"Week 2","state":"locked","items":[]}]"#;

    /// GPU Computing with Canvas showing `tabs`, on its Modules screen.
    fn modules_app(tabs: &[&str], library: Option<Arc<Library>>) -> App {
        let mut app = app_with_courses();
        app.library = library;
        app.update(key(KeyCode::Enter));
        let list = tabs.iter().map(|t| tab(t)).collect();
        answer(&mut app, tabs_key(), Payload::Tabs(list));
        while !matches!(app.view().kind, ViewKind::Modules { .. }) {
            app.update(key(KeyCode::Tab));
        }
        let modules = serde_json::from_str(MODULES).unwrap();
        answer(
            &mut app,
            ResourceKey::Modules("10".into()),
            Payload::Modules(modules),
        );
        app
    }

    fn download_target(effects: &[Effect]) -> PathBuf {
        effects
            .iter()
            .find_map(|e| match e {
                Effect::Download { target, .. } => Some(target.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("expected a download, got {effects:?}"))
    }

    #[test]
    fn modules_list_skips_headings_and_opens_each_kind() {
        let (library, root) = temp_library("modules");
        let mut app = modules_app(&["assignments", "modules"], Some(library));
        // Rows: Week 1, Read first, Intro, Lab 1, lecture1.pdf, Quiz 1, Reading, Week 2.
        assert_eq!(app.view().selected, 2);
        app.update(key(KeyCode::Up));
        assert_eq!(app.view().selected, 2);
        app.update(key(KeyCode::End));
        assert_eq!(app.view().selected, 6);
        app.update(key(KeyCode::Home));
        assert_eq!(app.view().selected, 2);

        // Assignments open in the reader.
        app.update(key(KeyCode::Down));
        let effects = app.update(key(KeyCode::Enter));
        assert_eq!(
            fetch_of(&effects).0,
            ResourceKey::Detail("10".into(), "55".into())
        );
        app.update(key(KeyCode::Esc));

        // Files is hidden, so a module file goes to Module Files/<module>.
        app.update(key(KeyCode::Down));
        assert!(app.available(Act::Download));
        let effects = app.update(key(KeyCode::Enter));
        assert_eq!(fetches(&effects), [ResourceKey::File("101".into())]);
        let effects = deliver_file(&mut app, canvas_file("101", "lecture1.pdf", "3"));
        assert!(
            download_target(&effects).ends_with(
                Path::new("GPU Computing")
                    .join("Module Files")
                    .join("Week 1")
                    .join("lecture1.pdf")
            )
        );

        // Quizzes open on Canvas; outside links go through their module item.
        app.update(key(KeyCode::Down));
        assert_eq!(
            app.update(key(KeyCode::Enter)),
            [Effect::OpenUrl(format!(
                "{ORIGIN}/courses/10/modules/items/15"
            ))]
        );
        app.update(key(KeyCode::Down));
        assert_eq!(
            app.browser_url().unwrap(),
            format!("{ORIGIN}/courses/10/modules/items/16")
        );

        // Pages are read here, with their files and links.
        app.update(key(KeyCode::Home));
        let effects = app.update(key(KeyCode::Enter));
        let page = ResourceKey::WikiPage("10".into(), "intro".into());
        assert_eq!(fetch_of(&effects).0, page);
        answer(
            &mut app,
            page,
            Payload::WikiPage(Box::new(crate::modules::WikiPage {
                title: Some("Intro".into()),
                body: Some(
                    r#"<p>See <a href="/courses/10/files/22">notes</a> and <a href="/courses/10/pages/week-2">next</a>.</p>"#.into(),
                ),
                ..Default::default()
            })),
        );
        let links = app.current_links();
        assert_eq!(links.len(), 2);
        assert_eq!(
            links[1].target,
            Target::Page {
                course_id: "10".into(),
                url: "week-2".into()
            }
        );
        assert!(
            app.wiki_dir("10", "intro")
                .unwrap()
                .ends_with(Path::new("GPU Computing").join("Page Files").join("Intro"))
        );
        app.update(key(KeyCode::Tab));
        app.update(key(KeyCode::Down));
        let effects = app.update(key(KeyCode::Enter));
        assert_eq!(
            fetch_of(&effects).0,
            ResourceKey::WikiPage("10".into(), "week-2".into())
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn module_files_mirror_their_canvas_folder_when_files_is_shown() {
        let (library, root) = temp_library("module-folders");
        let mut app = modules_app(&["modules", "files"], Some(library));
        for _ in 0..2 {
            app.update(key(KeyCode::Down));
        }
        let effects = app.update(key(KeyCode::Char('d')));
        assert_eq!(
            fetches(&effects),
            [
                ResourceKey::File("101".into()),
                ResourceKey::Files("10".into())
            ]
        );
        // The file's details alone are not enough while its folders are being read.
        let effects = deliver_file(&mut app, canvas_file("101", "lecture1.pdf", "3"));
        assert!(!effects.iter().any(|e| matches!(e, Effect::Download { .. })));
        let effects = answer(
            &mut app,
            ResourceKey::Files("10".into()),
            Payload::Files(Box::new(course_files())),
        );
        assert!(
            download_target(&effects).ends_with(
                Path::new("Class Files")
                    .join("Lectures")
                    .join("Week 1")
                    .join("lecture1.pdf")
            )
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn every_module_heading_and_item_is_listed() {
        // Narrowing this list in place is gone; the palette searches the
        // whole account instead. What is left to check is that the headings
        // and their items are all here, in order, and that a row still
        // resolves to the item it draws.
        let app = modules_app(&["modules"], None);
        let rows = app.visible_rows();
        assert_eq!(rows, (0..rows.len()).collect::<Vec<_>>());
        assert!(matches!(
            app.module_rows("10").first(),
            Some(ModuleRow::Module(_))
        ));
    }

    #[test]
    fn a_page_home_is_the_home_tab_where_tab_changes_tabs_and_digits_reach_links() {
        let mut app = App::new(ORIGIN, None);
        let (k, r) = fetch_of(&app.start());
        let mut wiki = course("10", "GPU Computing");
        wiki.default_view = Some("wiki".into());
        deliver(&mut app, k, r, Payload::Courses(vec![wiki]));
        let effects = app.update(key(KeyCode::Enter));
        let home = ResourceKey::WikiPage("10".into(), crate::modules::FRONT_PAGE.into());
        assert_eq!(fetches(&effects), [home.clone(), tabs_key()]);
        answer(
            &mut app,
            tabs_key(),
            Payload::Tabs(vec![tab("home"), tab("announcements")]),
        );
        assert_eq!(
            app.course_tabs("10"),
            [CourseTab::Home, CourseTab::Announcements]
        );
        answer(
            &mut app,
            home.clone(),
            Payload::WikiPage(Box::new(crate::modules::WikiPage {
                title: Some("Welcome".into()),
                body: Some(r#"<a href="/courses/10/pages/week-1">Week 1</a>"#.into()),
                ..Default::default()
            })),
        );
        let scopes = app.scopes();
        assert!(scopes.contains(&Scope::CourseTabs) && !scopes.contains(&Scope::Sections));
        assert_eq!(app.browser_url().unwrap(), format!("{ORIGIN}/courses/10"));
        // Digits reach the links; Enter opens a linked page in the reader.
        app.update(key(KeyCode::Char('1')));
        let effects = app.update(key(KeyCode::Enter));
        assert_eq!(
            fetch_of(&effects).0,
            ResourceKey::WikiPage("10".into(), "week-1".into())
        );
        // On that page Tab moves between sections again.
        assert!(app.scopes().contains(&Scope::Sections));
        app.update(key(KeyCode::Esc));
        assert!(matches!(app.view().kind.tab(), Some((CourseTab::Home, ..))));
        app.update(key(KeyCode::Esc)); // leave the links section
        let effects = app.update(key(KeyCode::Tab));
        assert_eq!(
            fetch_of(&effects).0,
            ResourceKey::Announcements("10".into())
        );
        app.update(key(KeyCode::Tab));
        assert!(matches!(app.view().kind.tab(), Some((CourseTab::Home, ..))));
    }

    fn graded(id: &str, name: &str, points: f64, score: Option<f64>) -> AssignmentSummary {
        AssignmentSummary {
            id: id.into(),
            name: Some(name.into()),
            points_possible: Some(points),
            submission: Some(crate::model::Submission {
                score,
                workflow_state: Some(
                    if score.is_some() {
                        "graded"
                    } else {
                        "unsubmitted"
                    }
                    .into(),
                ),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn grades_payload() -> Payload {
        Payload::Grades(Box::new(crate::grades::Grades {
            total: Some(crate::grades::Total {
                score: Some(91.5),
                grade: Some("A-".into()),
            }),
            weighted: true,
            groups: vec![
                crate::grades::Group {
                    id: "100".into(),
                    name: Some("Homework".into()),
                    group_weight: Some(40.0),
                    assignments: vec![
                        graded("1", "HW 1", 10.0, Some(9.0)),
                        graded("2", "HW 2", 10.0, None),
                    ],
                    ..Default::default()
                },
                crate::grades::Group {
                    id: "200".into(),
                    name: Some("Exams".into()),
                    group_weight: Some(60.0),
                    assignments: vec![graded("5", "Midterm", 100.0, Some(88.0))],
                    ..Default::default()
                },
            ],
            feedback: vec![crate::grades::Feedback {
                assignment_id: "5".into(),
                count: 2,
                latest: Some(Utc::now()),
            }],
        }))
    }

    /// Opens a course whose only Canvas tab is Grades, with grades delivered.
    fn grades_app(store: Option<Arc<SnapshotStore>>) -> App {
        let mut app = app_with_courses();
        app.store = store;
        app.update(key(KeyCode::Enter));
        answer(&mut app, tabs_key(), Payload::Tabs(vec![tab("grades")]));
        answer(&mut app, ResourceKey::Grades("10".into()), grades_payload());
        app
    }

    #[test]
    fn the_grades_tab_is_shown_only_where_canvas_shows_it() {
        let mut app = app_with_courses();
        app.update(key(KeyCode::Enter));
        answer(
            &mut app,
            tabs_key(),
            Payload::Tabs(vec![tab("assignments"), tab("grades")]),
        );
        assert_eq!(
            app.course_tabs("10"),
            [CourseTab::Assignments, CourseTab::Grades]
        );
        // A course that hides the tab does not get the section.
        app.update(key(KeyCode::Esc));
        app.update(key(KeyCode::Down));
        app.update(key(KeyCode::Enter));
        answer(
            &mut app,
            ResourceKey::Tabs("20".into()),
            Payload::Tabs(vec![tab("assignments")]),
        );
        assert_eq!(app.course_tabs("20"), [CourseTab::Assignments]);
    }

    #[test]
    fn grade_rows_group_assignments_and_skip_the_headings() {
        let mut app = grades_app(None);
        assert!(matches!(app.view().kind, ViewKind::Grades { .. }));
        // Two groups, three assignments.
        assert_eq!(app.grade_rows("10").len(), 5);
        // The cursor starts on the first assignment, not on its group heading.
        assert!(matches!(
            app.selected_grade_row(),
            Some(GradeRow::Assignment(_, a)) if a.id == "1"
        ));
        // Down moves over the next group's heading to its first assignment.
        app.update(key(KeyCode::Down));
        app.update(key(KeyCode::Down));
        assert!(matches!(
            app.selected_grade_row(),
            Some(GradeRow::Assignment(_, a)) if a.id == "5"
        ));
        // Every heading and every row under it is listed: narrowing a list
        // in place is gone, and the palette searches instead.
        assert_eq!(app.visible_rows(), [0, 1, 2, 3, 4]);
    }

    /// Type a score into the open what-if box.
    fn type_score(app: &mut App, score: &str) {
        for c in score.chars() {
            app.update(key(KeyCode::Char(c)));
        }
    }

    #[test]
    fn a_score_tried_on_one_assignment_moves_the_total_and_nothing_else() {
        let mut app = grades_app(None);
        // 40% homework (9 of 10 graded, 10 points ungraded) and 60% exams
        // (88 of 100) is 88.8 as the reader counts it.
        assert_eq!(app.whatif_totals("10"), None, "nothing tried yet");
        // `w` on the first assignment, then a score.
        app.update(key(KeyCode::Down)); // HW 2, the ungraded one
        assert!(matches!(
            app.selected_grade_row(),
            Some(GradeRow::Assignment(_, a)) if a.id == "2"
        ));
        let effects = app.update(key(KeyCode::Char('w')));
        assert!(effects.is_empty(), "a guess asks Canvas for nothing");
        assert!(app.scopes().contains(&Scope::WhatIfInput));
        type_score(&mut app, "10");
        let effects = app.update(key(KeyCode::Enter));
        assert!(effects.is_empty(), "and sends Canvas nothing");
        assert!(app.whatif_input.is_none(), "the box closes");
        assert_eq!(app.whatif_score("10", "2"), Some(10.0));
        // Homework becomes 19 of 20, so the total becomes 0.4*95 + 0.6*88.
        let (now, projected) = app.whatif_totals("10").expect("both totals");
        assert!((now.unwrap() - 88.8).abs() < 0.005, "{now:?}");
        assert!((projected.unwrap() - 90.8).abs() < 0.005, "{projected:?}");
        // What Canvas said is left exactly as Canvas said it.
        let grades = app.grades["10"].data.as_ref().unwrap();
        assert_eq!(grades.total.as_ref().unwrap().score, Some(91.5));
        assert_eq!(grades.groups[0].assignments[1].submission.as_ref().unwrap().score, None);
    }

    #[test]
    fn a_tried_score_can_be_taken_back_one_at_a_time_or_all_at_once() {
        let mut app = grades_app(None);
        app.update(key(KeyCode::Char('w')));
        type_score(&mut app, "7");
        app.update(key(KeyCode::Enter));
        app.update(key(KeyCode::Down));
        app.update(key(KeyCode::Char('w')));
        type_score(&mut app, "8");
        app.update(key(KeyCode::Enter));
        assert_eq!(app.whatif_of("10").map(|w| w.len()), Some(2));
        // The box opens on what is already being tried, and emptying it puts
        // the real score back.
        app.update(key(KeyCode::Char('w')));
        assert_eq!(app.whatif_input.as_ref().unwrap().input, "8");
        app.update(ctrl('u'));
        app.update(key(KeyCode::Enter));
        assert_eq!(app.whatif_score("10", "2"), None);
        assert_eq!(app.whatif_score("10", "1"), Some(7.0));
        // Esc leaves the score alone rather than clearing it.
        app.update(key(KeyCode::Char('w')));
        type_score(&mut app, "3");
        app.update(key(KeyCode::Esc));
        assert_eq!(app.whatif_score("10", "2"), None);
        // `W` drops the rest.
        app.update(key(KeyCode::Char('W')));
        assert_eq!(app.whatif_of("10"), None);
        assert_eq!(app.whatif_totals("10"), None);
    }

    #[test]
    fn the_box_takes_a_score_and_only_a_score() {
        let mut app = grades_app(None);
        app.update(key(KeyCode::Char('w')));
        // Letters are not scores, so they are not typed at all.
        type_score(&mut app, "9a.5b.2");
        assert_eq!(app.whatif_input.as_ref().unwrap().input, "9.52");
        app.update(ctrl('u'));
        // A lone point is not a number either, and says so.
        type_score(&mut app, ".");
        app.update(key(KeyCode::Enter));
        assert_eq!(app.notice.as_deref(), Some("A score has to be a number."));
        assert_eq!(app.whatif_score("10", "1"), None);
        // Above the points possible is extra credit, which Canvas allows.
        app.update(key(KeyCode::Char('w')));
        type_score(&mut app, "12");
        app.update(key(KeyCode::Enter));
        assert_eq!(app.whatif_score("10", "1"), Some(12.0));
        assert!(
            app.notice.as_deref().unwrap_or_default().contains("extra credit"),
            "{:?}",
            app.notice
        );
    }

    #[test]
    fn a_score_can_only_be_tried_on_an_assignment_and_cleared_when_one_has_been() {
        let mut app = grades_app(None);
        assert!(app.available(Act::WhatIf));
        assert!(!app.available(Act::ClearWhatIf), "nothing tried yet");
        // A group heading is not something to score. The cursor steps over
        // headings, so this is the one place that puts it on one.
        app.stack.last_mut().expect("a view").selected = 3;
        assert!(matches!(app.selected_grade_row(), Some(GradeRow::Group(_))));
        assert!(!app.available(Act::WhatIf));
        app.update(key(KeyCode::Char('w')));
        assert!(app.whatif_input.is_none(), "and the box does not open");
        // With a score tried, clearing becomes available.
        app.stack.last_mut().expect("a view").selected = 4;
        app.update(key(KeyCode::Char('w')));
        type_score(&mut app, "5");
        app.update(key(KeyCode::Enter));
        assert!(app.available(Act::ClearWhatIf));
    }

    #[test]
    fn a_tried_score_survives_a_refresh_but_not_a_restart() {
        let root = std::env::temp_dir().join(format!("canter-whatif-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let store = Arc::new(SnapshotStore::new(&root, "reader"));
        let mut app = grades_app(Some(store.clone()));
        app.update(key(KeyCode::Char('w')));
        type_score(&mut app, "5");
        app.update(key(KeyCode::Enter));
        assert_eq!(app.whatif_score("10", "1"), Some(5.0));
        // `r` re-reads the grades; the question being asked is still the same
        // question when the answer comes back.
        let (k, r) = fetch_of(&app.update(key(KeyCode::Char('r'))));
        deliver(&mut app, k, r, grades_payload());
        assert_eq!(app.whatif_score("10", "1"), Some(5.0));
        // Nothing was written down, so a later run starts from the real
        // grades rather than from last week's guesses.
        let next = App::new(ORIGIN, Some(store));
        assert!(next.whatif.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn opening_graded_work_reads_the_assignment_and_clears_its_feedback_mark() {
        let root = std::env::temp_dir().join(format!("canter-seen-{}", std::process::id()));
        let store = Arc::new(SnapshotStore::new(&root, "reader"));
        let mut app = grades_app(Some(store.clone()));
        // The midterm has instructor comments that have not been opened here.
        assert!(app.unseen_feedback("10", "5"));
        assert!(!app.unseen_feedback("10", "1"));
        app.update(key(KeyCode::Down));
        app.update(key(KeyCode::Down));
        let effects = app.update(key(KeyCode::Enter));
        assert_eq!(
            fetch_of(&effects).0,
            ResourceKey::Detail("10".into(), "5".into())
        );
        assert!(!app.unseen_feedback("10", "5"));
        assert_eq!(
            app.browser_url().unwrap(),
            format!("{ORIGIN}/courses/10/assignments/5")
        );
        // What was seen outlives the run, and Canvas is never told.
        let mut restarted = App::new(ORIGIN, Some(store));
        restarted.grades = std::mem::take(&mut app.grades);
        assert!(!restarted.unseen_feedback("10", "5"));
        let other = App::new(ORIGIN, Some(Arc::new(SnapshotStore::new(&root, "someone"))));
        assert!(other.seen_feedback.is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_group_heading_cannot_be_opened_and_o_falls_back_to_the_grades_page() {
        let mut app = grades_app(None);
        // The first row is a group heading. Moving never lands on one, so
        // put the selection there by hand to check what Enter does with it.
        app.view_mut().selected = 0;
        assert!(matches!(app.selected_grade_row(), Some(GradeRow::Group(_))));
        assert!(app.update(key(KeyCode::Enter)).is_empty());
        assert_eq!(
            app.browser_url().unwrap(),
            format!("{ORIGIN}/courses/10/grades")
        );
    }

    #[test]
    fn a_saved_grade_switch_fills_the_column_again_after_a_restart() {
        let root = std::env::temp_dir().join(format!("canter-cgrades2-{}", std::process::id()));
        let store = Arc::new(SnapshotStore::new(&root, "reader"));
        store.save_show_course_grades(true);
        // A restart with the switch on reads the grades of the courses listed,
        // so the column is not blank until a course is opened.
        let mut app = App::new(ORIGIN, Some(store));
        let (k, r) = fetch_of(&app.start());
        let effects = app.update(Msg::Fetched {
            key: k,
            request: r,
            result: Ok((
                Payload::Courses(vec![course("10", "GPU Computing")]),
                FetchStats::default(),
            )),
            at: Utc::now(),
        });
        assert_eq!(fetches(&effects), [ResourceKey::Grades("10".into())]);
        answer(&mut app, ResourceKey::Grades("10".into()), grades_payload());
        assert_eq!(app.course_grade("10").unwrap(), "A- 91.5%");
        // Refreshing the home screen does not read them a second time.
        let effects = app.update(key(KeyCode::Char('r')));
        assert!(fetches(&effects).contains(&ResourceKey::Courses));
        assert!(!fetches(&effects).contains(&ResourceKey::Grades("10".into())));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn one_switch_shows_every_course_grade_and_the_choice_is_saved() {
        let root = std::env::temp_dir().join(format!("canter-cgrades-{}", std::process::id()));
        let store = Arc::new(SnapshotStore::new(&root, "reader"));
        let mut app = app_with_courses();
        app.store = Some(store.clone());
        assert!(!app.show_course_grades);
        assert!(app.course_grade("10").is_none());
        // Space then g turns it on for every course at once, and reads them.
        app.update(key(KeyCode::Char(' ')));
        let effects = app.update(key(KeyCode::Char('g')));
        assert!(app.show_course_grades);
        assert_eq!(
            fetches(&effects),
            [
                ResourceKey::Grades("10".into()),
                ResourceKey::Grades("20".into())
            ]
        );
        answer(&mut app, ResourceKey::Grades("10".into()), grades_payload());
        assert_eq!(app.course_grade("10").unwrap(), "A- 91.5%");
        // The choice survives a restart, and turning it off asks for nothing.
        let restarted = App::new(ORIGIN, Some(store));
        assert!(restarted.show_course_grades);
        app.update(key(KeyCode::Char(' ')));
        let effects = app.update(key(KeyCode::Char('g')));
        assert!(!app.show_course_grades);
        assert!(fetches(&effects).is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_hidden_course_total_is_left_out_of_the_course_list() {
        let mut app = app_with_courses();
        app.show_course_grades = true;
        answer_grades_without_a_total(&mut app);
        assert!(app.course_grade("10").is_none());
    }

    fn answer_grades_without_a_total(app: &mut App) {
        let r = app.grades.entry("10".to_string()).or_default();
        r.data = Some(crate::grades::Grades {
            total: None,
            weighted: false,
            groups: vec![],
            feedback: vec![],
        });
        r.version += 1;
    }

    fn page_summary(url: &str, title: &str, day: u32) -> crate::modules::PageSummary {
        crate::modules::PageSummary {
            url: Some(url.into()),
            title: Some(title.into()),
            updated_at: Some(
                chrono::DateTime::parse_from_rfc3339(&format!("2026-10-{day:02}T12:00:00Z"))
                    .unwrap()
                    .with_timezone(&Utc),
            ),
            ..Default::default()
        }
    }

    #[test]
    fn the_pages_tab_lists_every_page_and_opens_one_in_the_reader() {
        let mut app = app_with_courses();
        app.update(key(KeyCode::Enter));
        answer(&mut app, tabs_key(), Payload::Tabs(vec![tab("pages")]));
        assert_eq!(app.course_tabs("10"), [CourseTab::Pages]);
        assert!(matches!(app.view().kind, ViewKind::Pages { .. }));
        answer(
            &mut app,
            ResourceKey::Pages("10".into()),
            Payload::Pages(vec![
                page_summary("week-2", "Week 2", 21),
                page_summary("week-1", "Week 1", 14),
            ]),
        );
        assert_eq!(app.visible_rows(), [0, 1]);
        assert_eq!(
            app.browser_url().unwrap(),
            format!("{ORIGIN}/courses/10/pages/week-2")
        );
        // The row chosen is the page read.
        app.update(key(KeyCode::Down));
        let effects = app.update(key(KeyCode::Enter));
        assert_eq!(
            fetch_of(&effects).0,
            ResourceKey::WikiPage("10".into(), "week-1".into())
        );
        assert!(matches!(app.view().kind, ViewKind::WikiPage { .. }));
        // Back returns to the list and reads it again, as every list does.
        let effects = app.update(key(KeyCode::Esc));
        assert_eq!(fetch_of(&effects).0, ResourceKey::Pages("10".into()));
    }

    #[test]
    fn the_page_list_is_saved_and_shown_at_once() {
        let root = std::env::temp_dir().join(format!("canter-pagelist-{}", std::process::id()));
        let store = Arc::new(SnapshotStore::new(&root, "reader"));
        store.save(
            &ResourceKey::Pages("10".into()),
            &vec![page_summary("week-1", "Week 1", 14)],
            Utc::now(),
        );
        let mut app = app_with_courses();
        app.store = Some(store);
        app.update(key(KeyCode::Enter));
        answer(&mut app, tabs_key(), Payload::Tabs(vec![tab("pages")]));
        // The saved list is there before Canvas answers.
        assert_eq!(app.visible_rows(), [0]);
        assert!(matches!(
            app.page_lists["10"].origin,
            crate::app::Origin::Cache
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    const FEEDBACK: &str = r#"{
        "attempt": 2, "submitted_at": "2026-10-22T12:00:00Z", "workflow_state": "graded",
        "submission_history": [
            {"attempt": 1, "submitted_at": "2026-10-20T12:00:00Z", "submission_type": "online_upload"},
            {"attempt": 2, "submitted_at": "2026-10-22T12:00:00Z", "submission_type": "online_upload",
             "attachments": [{"id": 90, "display_name": "final.pdf", "size": 20, "canvadoc_document_id": 7}]}
        ],
        "submission_comments": [
            {"id": 41, "author_name": "Dr Lee", "created_at": "2026-10-21T09:00:00Z",
             "comment": "Fix the first proof.", "attempt": 1},
            {"id": 42, "author_name": "Dr Lee", "created_at": "2026-10-23T09:00:00Z",
             "comment": "Much better.", "attempt": 2,
             "attachments": [{"id": 77, "display_name": "marked.pdf", "size": 400,
                              "updated_at": "2026-10-23T09:00:00Z"}]},
            {"id": 43, "author_name": "Dr Lee", "created_at": "2026-10-23T10:00:00Z", "comment": ""}
        ],
        "rubric_assessment": {"_1": {"points": 4.0, "comments": "Clear."}, "_2": {"points": 2.0}}
    }"#;

    /// An assignment with a rubric, two attempts and the instructor's feedback.
    fn feedback_app(library: Option<Arc<Library>>) -> App {
        let mut app = app_with_courses();
        app.library = library;
        let (k, r) = fetch_of(&app.update(key(KeyCode::Enter)));
        let base = AssignmentSummary {
            id: "7".into(),
            name: Some("Lab-2".into()),
            submission_types: vec!["online_upload".into()],
            submission: Some(crate::model::Submission {
                submitted_at: Some(Utc::now()),
                attempt: Some(2),
                ..Default::default()
            }),
            ..Default::default()
        };
        deliver(&mut app, k, r, Payload::Assignments(vec![base.clone()]));
        let (k, r) = fetch_of(&app.update(key(KeyCode::Enter)));
        let detail = AssignmentDetail {
            base,
            rubric: vec![
                crate::model::RubricCriterion {
                    id: Some("_1".into()),
                    description: Some("Correctness".into()),
                    points: Some(5.0),
                    ..Default::default()
                },
                crate::model::RubricCriterion {
                    id: Some("_2".into()),
                    description: Some("Write-up".into()),
                    points: Some(3.0),
                    ..Default::default()
                },
            ],
            rubric_settings: Some(crate::model::RubricSettings {
                title: Some("Lab rubric".into()),
                points_possible: Some(8.0),
            }),
            ..Default::default()
        };
        let effects = app.update(Msg::Fetched {
            key: k,
            request: r,
            result: Ok((Payload::Detail(Box::new(detail)), FetchStats::default())),
            at: Utc::now(),
        });
        let history = submission::History::parse(serde_json::from_str(FEEDBACK).unwrap()).unwrap();
        let (k, r) = fetch_of(&effects);
        assert_eq!(k, ResourceKey::History("10".into(), "7".into()));
        deliver(&mut app, k, r, Payload::History(history));
        app.notice = None;
        app
    }

    #[test]
    fn feedback_is_a_section_of_the_assignment_page_shown_per_attempt() {
        let mut app = feedback_app(None);
        assert_eq!(
            app.sections(),
            vec![Section::Text, Section::Submissions, Section::Feedback]
        );
        // Tab, Tab reaches the feedback for the latest attempt.
        app.update(key(KeyCode::Tab));
        assert_eq!(app.focus(), Section::Submissions);
        app.update(key(KeyCode::Tab));
        assert_eq!(app.focus(), Section::Feedback);
        assert!(app.scopes().contains(&Scope::Feedback));
        // The latest attempt shows its own comment, the untagged one, and the
        // two scored rubric criteria.
        let comments = app.attempt_comments("10", "7", 0);
        assert_eq!(
            comments
                .iter()
                .filter_map(|c| c.id.clone())
                .collect::<Vec<_>>(),
            ["42", "43"]
        );
        assert_eq!(
            app.feedback_entries("10", "7", 0),
            [
                FeedbackEntry::Comment(0),
                FeedbackEntry::Comment(1),
                FeedbackEntry::Criterion(0),
                FeedbackEntry::Criterion(1)
            ]
        );
        let scores = app.rubric_scores("10", "7", 0);
        assert_eq!(scores[0].0.title(), "Correctness");
        assert_eq!(scores[0].1.points, Some(4.0));
        // Reading it tells the Grades tab's seen-store, so the row loses its
        // mark; the store is local and Canvas is never told.
        assert!(!app.unseen_feedback("10", "7"));
        // A comment Canvas keeps no text for says where to read it.
        assert_eq!(
            comments[1].browser_only(),
            Some(crate::submission::BrowserOnly::Annotation)
        );
        // `o` opens the submission itself, which is where Canvas shows those.
        assert_eq!(
            app.browser_url().unwrap(),
            format!("{ORIGIN}/courses/10/assignments/7/submissions/self")
        );
        // The arrows move through the comments and then the criteria.
        app.update(key(KeyCode::Down));
        app.update(key(KeyCode::Down));
        assert_eq!(app.view().feedback, 2);
        for _ in 0..5 {
            app.update(key(KeyCode::Down));
        }
        assert_eq!(app.view().feedback, 3, "stays on the last entry");
        // Esc returns to the text, as in every other section.
        app.update(key(KeyCode::Esc));
        assert_eq!(app.focus(), Section::Text);

        // The earlier attempt has its own comment and no rubric.
        app.update(key(KeyCode::Tab));
        app.update(key(KeyCode::Down));
        assert_eq!(app.view().attempt, 1);
        assert_eq!(app.view().feedback, 0, "the feedback cursor starts over");
        assert_eq!(
            app.attempt_comments("10", "7", 1)
                .iter()
                .filter_map(|c| c.id.clone())
                .collect::<Vec<_>>(),
            ["41"]
        );
        assert!(
            app.rubric_scores("10", "7", 1).is_empty(),
            "Canvas keeps one rubric assessment, so it belongs to the latest attempt"
        );
    }

    #[test]
    fn comment_files_go_to_the_assignments_feedback_folder() {
        let (lib, root) = temp_library("feedback");
        let mut app = feedback_app(Some(lib));
        app.update(key(KeyCode::Tab));
        app.update(key(KeyCode::Tab));
        assert_eq!(app.focus(), Section::Feedback);
        assert!(app.available(Act::Download));
        assert!(app.available(Act::DownloadAll));
        // The comment's file metadata is read again before it is placed.
        let effects = app.update(key(KeyCode::Char('d')));
        assert_eq!(fetches(&effects), [ResourceKey::File("77".into())]);
        let effects = deliver_file(&mut app, canvas_file("77", "marked.pdf", "3"));
        assert!(
            download_target(&effects).ends_with(
                Path::new("GPU Computing")
                    .join("Assignment Files")
                    .join("Lab-2")
                    .join("Feedback")
                    .join("marked.pdf")
            ),
            "{effects:?}"
        );
        assert!(
            app.feedback_dir("10", "7")
                .unwrap()
                .ends_with(Path::new("Lab-2").join("Feedback"))
        );
        // The comment with no file says so instead of downloading anything.
        app.update(key(KeyCode::Down));
        assert!(!app.available(Act::Download));
        assert!(app.update(key(KeyCode::Char('d'))).is_empty());
        assert!(app.notice.take().unwrap().contains("No file"));
        assert!(app.update(key(KeyCode::Enter)).is_empty());
        assert!(app.notice.take().unwrap().contains("Press o"));
        let _ = std::fs::remove_dir_all(root);
    }

    /// An assignment that takes one submission kind, ready to submit.
    fn submit_app(submission_type: &str) -> App {
        let mut app = App::new(ORIGIN, None);
        app.stack.push(View::new(ViewKind::Detail {
            course_id: "10".into(),
            course_name: "GPU".into(),
            assignment_id: "20".into(),
            title: "Lab".into(),
        }));
        let mut detail = AssignmentDetail::default();
        detail.base.id = "20".into();
        detail.base.submission_types = vec![submission_type.into()];
        let resource = app.details.entry(("10".into(), "20".into())).or_default();
        resource.data = Some(detail);
        resource.origin = Origin::Network;
        resource.fetched_at = Some(Utc::now());
        app
    }

    #[test]
    fn text_submission_reviews_the_exact_text_and_needs_an_explicit_yes() {
        let mut app = submit_app("online_text_entry");
        // `s` is for files, which this assignment does not take.
        assert!(!app.available(Act::Submit));
        assert!(app.available(Act::SubmitText));
        assert!(!app.available(Act::SubmitUrl));
        assert!(app.update(key(KeyCode::Char('s'))).is_empty());
        assert!(app.submission.is_none());
        assert!(app.notice.take().unwrap().contains("file uploads"));
        // `t` opens the text box, not the path box.
        assert!(app.update(key(KeyCode::Char('t'))).is_empty());
        assert_eq!(app.screen_scopes(), vec![Scope::TextInput]);
        let draft = app.submission.as_ref().unwrap();
        assert_eq!(draft.kind, DraftKind::Text);
        assert!(draft.input.is_empty(), "the text box starts empty");
        // Letters are text here, including keys that are commands elsewhere.
        app.update(Msg::Paste("My answer".into()));
        app.update(key(KeyCode::Char('.')));
        assert_eq!(app.submission.as_ref().unwrap().input, "My answer.");
        // Enter reviews; nothing is sent yet.
        assert!(app.update(key(KeyCode::Enter)).is_empty());
        let draft = app.submission.as_ref().unwrap();
        assert!(draft.reviewing);
        assert_eq!(draft.content, "My answer.", "exactly what will be sent");
        assert_eq!(app.screen_scopes(), vec![Scope::Review]);
        // n goes back to editing, with the text kept.
        app.update(key(KeyCode::Char('n')));
        assert!(!app.submission.as_ref().unwrap().reviewing);
        assert_eq!(app.submission.as_ref().unwrap().input, "My answer.");
        app.update(key(KeyCode::Enter));
        // Only `y` sends it, and only once.
        let effects = app.update(key(KeyCode::Char('y')));
        assert!(
            matches!(&effects[..], [Effect::SubmitContent { course_id, assignment_id, kind, content }]
                if course_id == "10" && assignment_id == "20" && *kind == DraftKind::Text && content == "My answer."),
            "{effects:?}"
        );
        assert!(app.update(key(KeyCode::Char('y'))).is_empty());
        // Canvas's answer closes the draft and re-reads the assignment.
        let effects = app.update(Msg::Submitted {
            course_id: "10".into(),
            assignment_id: "20".into(),
            result: Ok("Submitted and verified your text on Canvas.".into()),
        });
        assert!(app.submission.is_none());
        assert!(app.notice.as_deref().unwrap().contains("verified"));
        assert!(fetches(&effects).contains(&ResourceKey::Detail("10".into(), "20".into())));
    }

    #[test]
    fn typed_text_can_come_from_a_file_and_an_empty_entry_is_refused() {
        let dir = std::env::temp_dir().join(format!("canter-app-text-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let answer = dir.join("answer.txt");
        std::fs::write(&answer, "Line one.\nLine two.\n").unwrap();
        let mut app = submit_app("online_text_entry");
        app.update(key(KeyCode::Char('t')));
        // An empty box cannot be reviewed.
        assert!(app.update(key(KeyCode::Enter)).is_empty());
        assert!(!app.submission.as_ref().unwrap().reviewing);
        assert!(app.notice.take().unwrap().contains("Type the text"));
        // Ctrl+O switches to naming a file, using the path box of the upload flow.
        app.update(Msg::Key(KeyEvent::new(
            KeyCode::Char('o'),
            KeyModifiers::CONTROL,
        )));
        assert_eq!(app.screen_scopes(), vec![Scope::PathInput]);
        app.submission.as_mut().unwrap().input = answer.to_string_lossy().into_owned();
        assert!(app.update(key(KeyCode::Enter)).is_empty());
        let draft = app.submission.as_ref().unwrap();
        assert!(draft.reviewing);
        assert_eq!(draft.content, "Line one.\nLine two.");
        assert!(draft.source.as_deref().unwrap().ends_with("answer.txt"));
        let effects = app.update(key(KeyCode::Char('y')));
        assert!(
            matches!(&effects[..], [Effect::SubmitContent { content, .. }]
            if content == "Line one.\nLine two.")
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn url_submission_shows_the_address_it_will_send() {
        let mut app = submit_app("online_url");
        assert!(app.available(Act::SubmitUrl));
        assert!(!app.available(Act::SubmitText));
        app.update(key(KeyCode::Char('u')));
        assert_eq!(app.submission.as_ref().unwrap().kind, DraftKind::Url);
        app.update(Msg::Paste("example.com/work".into()));
        app.update(key(KeyCode::Enter));
        let draft = app.submission.as_ref().unwrap();
        assert!(draft.reviewing);
        assert_eq!(
            draft.content, "https://example.com/work",
            "the review screen shows the address with the scheme Canvas needs"
        );
        let effects = app.update(key(KeyCode::Char('y')));
        assert!(
            matches!(&effects[..], [Effect::SubmitContent { kind, content, .. }]
            if *kind == DraftKind::Url && content == "https://example.com/work")
        );
        // An address that cannot be sent keeps the draft open.
        let mut app = submit_app("online_url");
        app.update(key(KeyCode::Char('u')));
        app.update(Msg::Paste("not a url".into()));
        app.update(key(KeyCode::Enter));
        assert!(!app.submission.as_ref().unwrap().reviewing);
        assert!(app.notice.take().unwrap().contains("spaces"));
        // Esc leaves without sending anything.
        app.update(key(KeyCode::Esc));
        assert!(app.submission.is_none());
    }
}
