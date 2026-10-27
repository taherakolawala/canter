//! Every keyboard action, declared once.
//!
//! Key handling, the hint line, the `?` help screen and the `Space` actions
//! menu are all derived from [`BINDINGS`]. Tests check that no key does two
//! things in the same place, that the core keys mean the same thing on every
//! screen, and that the hint line always fits.
//!
//! Core keys (one meaning everywhere): `↑↓` move, `Enter` open, `Esc` back,
//! `Tab` next tab or section, `/` search, `r` refresh, `o` browser,
//! `Space` actions, `?` help, `q` quit. Text boxes are the one exception:
//! while typing, letters are text and `Tab` completes a path.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Something the user can do.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Act {
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
    Top,
    Bottom,
    Open,
    Back,
    NextSection,
    PrevSection,
    Search,
    /// Open the search palette.
    Palette,
    Refresh,
    Browser,
    Menu,
    Help,
    Quit,
    Download,
    DownloadAll,
    ShowFolder,
    Submit,
    SubmitText,
    SubmitUrl,
    JumpToLink,
    HideCourse,
    ShowHidden,
    ShowGrades,
    OpenLibrary,
    SignIn,
    Reply,
    NewMessage,
    InputDone,
    InputCancel,
    InputComplete,
    InputClear,
    LoadTextFile,
    Confirm,
}

impl Act {
    #[cfg(test)]
    pub const ALL: [Act; 39] = [
        Act::Up,
        Act::Down,
        Act::Left,
        Act::Right,
        Act::PageUp,
        Act::PageDown,
        Act::Top,
        Act::Bottom,
        Act::Open,
        Act::Back,
        Act::NextSection,
        Act::PrevSection,
        Act::Search,
        Act::Palette,
        Act::Refresh,
        Act::Browser,
        Act::Menu,
        Act::Help,
        Act::Quit,
        Act::Download,
        Act::DownloadAll,
        Act::ShowFolder,
        Act::Submit,
        Act::SubmitText,
        Act::SubmitUrl,
        Act::JumpToLink,
        Act::HideCourse,
        Act::ShowHidden,
        Act::ShowGrades,
        Act::OpenLibrary,
        Act::SignIn,
        Act::Reply,
        Act::NewMessage,
        Act::InputDone,
        Act::InputCancel,
        Act::InputComplete,
        Act::InputClear,
        Act::LoadTextFile,
        Act::Confirm,
    ];
}

/// Where a binding applies. The app reports the scopes that are active now.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Scope {
    /// Any screen while no menu, help, prompt or text box is open.
    Screen,
    /// Courses, assignments, announcements, files.
    List,
    Courses,
    /// A course's sections (Home, Assignments, Announcements, Files, Modules).
    CourseTabs,
    Files,
    /// An assignment, announcement or course page.
    Page,
    /// A page whose `Tab` moves between its sections: every page but a
    /// course's Home, where `Tab` moves between the course's tabs and
    /// `1`–`9` reach the links.
    Sections,
    /// A page that has files or links.
    HasLinks,
    /// The files & links section has focus.
    Links,
    /// The past submissions section has focus.
    Submissions,
    /// The instructor feedback section has focus.
    Feedback,
    /// The month grid on the course list has focus.
    Calendar,
    /// The selected day's entries have focus.
    Day,
    /// The Inbox screen: the folders and their conversations.
    Inbox,
    /// The home screen's Inbox box has focus. It shows a count and opens the
    /// Inbox screen; the folders and conversations are not here.
    InboxWidget,
    /// An open Inbox conversation.
    Conversation,
    Assignment,
    SignedOut,
    /// Typing in the search palette.
    Palette,
    /// Typing a file path to submit.
    PathInput,
    /// Typing the text or the web address to submit.
    TextInput,
    /// Confirming a submission.
    Review,
    /// Writing a message to send through the Canvas Inbox.
    Compose,
    /// Reviewing a message before it is sent.
    MessageReview,
    Menu,
    /// Typing in the actions menu's search box.
    MenuSearch,
    Help,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Code(KeyCode),
    Ctrl(char),
}

/// Sections of the actions menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Group {
    Selected,
    Here,
    App,
}

impl Group {
    pub fn title(self) -> &'static str {
        match self {
            Group::Selected => "Selected",
            Group::Here => "This screen",
            Group::App => "canter",
        }
    }
}

pub struct Binding {
    pub act: Act,
    pub keys: &'static [Key],
    pub scopes: &'static [Scope],
    /// Short label for the hint line.
    pub label: &'static str,
    /// Name in the actions menu and the help screen.
    pub title: &'static str,
    /// Letter in the actions menu, if it is listed there.
    pub menu: Option<char>,
    pub group: Group,
    /// Place in the hint line, lower first; 0 = not shown there.
    pub hint: u8,
    /// How the keys are written in the hint line, if not just the first key.
    pub shown: Option<&'static str>,
}

const fn ch(c: char) -> Key {
    Key::Code(KeyCode::Char(c))
}
const UP: Key = Key::Code(KeyCode::Up);
const DOWN: Key = Key::Code(KeyCode::Down);
const LEFT: Key = Key::Code(KeyCode::Left);
const RIGHT: Key = Key::Code(KeyCode::Right);
const ENTER: Key = Key::Code(KeyCode::Enter);
const ESC: Key = Key::Code(KeyCode::Esc);
const TAB: Key = Key::Code(KeyCode::Tab);
const BACKTAB: Key = Key::Code(KeyCode::BackTab);
const BACKSPACE: Key = Key::Code(KeyCode::Backspace);
const HOME: Key = Key::Code(KeyCode::Home);
const END: Key = Key::Code(KeyCode::End);
const PGUP: Key = Key::Code(KeyCode::PageUp);
const PGDN: Key = Key::Code(KeyCode::PageDown);

const fn b(
    act: Act,
    keys: &'static [Key],
    scopes: &'static [Scope],
    label: &'static str,
) -> Binding {
    Binding {
        act,
        keys,
        scopes,
        label,
        title: label,
        menu: None,
        group: Group::Here,
        hint: 0,
        shown: None,
    }
}

impl Binding {
    const fn title(mut self, title: &'static str) -> Self {
        self.title = title;
        self
    }
    const fn menu(mut self, letter: char, group: Group) -> Self {
        self.menu = Some(letter);
        self.group = group;
        self
    }
    const fn hint(mut self, place: u8) -> Self {
        self.hint = place;
        self
    }
    const fn shown(mut self, keys: &'static str) -> Self {
        self.shown = Some(keys);
        self
    }
}

use Scope::*;

/// The whole keyboard. More specific rows come first: the first row that
/// matches a key in the active scopes wins (tests ensure there is only one
/// action per key per place anyway).
pub static BINDINGS: &[Binding] = &[
    // Moving.
    b(Act::Up, &[UP, ch('k')], &[Links], "select").title("Previous item"),
    b(Act::Down, &[DOWN, ch('j')], &[Links], "select")
        .title("Next item")
        .hint(3)
        .shown("↑↓"),
    b(Act::Up, &[UP, ch('k')], &[Feedback], "select").title("Previous comment or criterion"),
    b(Act::Down, &[DOWN, ch('j')], &[Feedback], "select")
        .title("Next comment or criterion")
        .hint(1)
        .shown("↑↓"),
    b(Act::Up, &[UP, ch('k')], &[Submissions], "attempt").title("Newer attempt"),
    b(Act::Down, &[DOWN, ch('j')], &[Submissions], "attempt")
        .title("Older attempt")
        .hint(1)
        .shown("↑↓"),
    b(Act::Left, &[LEFT, ch('h')], &[Calendar], "day").title("Previous day"),
    b(Act::Right, &[RIGHT, ch('l')], &[Calendar], "day")
        .title("Next day")
        .hint(1)
        .shown("←→↑↓"),
    b(Act::Up, &[UP, ch('k')], &[Calendar], "week").title("Previous week"),
    b(Act::Down, &[DOWN, ch('j')], &[Calendar], "week").title("Next week"),
    b(Act::PageUp, &[PGUP], &[Calendar], "month").title("Previous month"),
    b(Act::PageDown, &[PGDN], &[Calendar], "month")
        .title("Next month")
        .hint(3)
        .shown("PgUp/PgDn"),
    b(Act::Top, &[HOME, ch('g')], &[Calendar], "today")
        .title("Go to today")
        .hint(4)
        .shown("Home"),
    b(Act::Open, &[ENTER], &[Calendar], "open")
        .title("Open the day's entry, or choose one when there are several")
        .hint(2),
    b(Act::Back, &[ESC, BACKSPACE], &[Calendar], "courses")
        .title("Back to the course list")
        .hint(7),
    b(Act::Up, &[UP, ch('k')], &[Day], "select").title("Previous entry"),
    b(Act::Down, &[DOWN, ch('j')], &[Day], "select")
        .title("Next entry")
        .hint(1)
        .shown("↑↓"),
    b(Act::Open, &[ENTER, RIGHT, ch('l')], &[Day], "open")
        .title("Open the entry (assignments here, the rest in the browser)")
        .hint(2),
    b(
        Act::Back,
        &[ESC, LEFT, ch('h'), BACKSPACE],
        &[Day],
        "calendar",
    )
    .title("Back to the calendar")
    .hint(7),
    b(Act::Up, &[UP, ch('k')], &[Inbox], "select").title("Previous conversation"),
    b(Act::Down, &[DOWN, ch('j')], &[Inbox], "select")
        .title("Next conversation")
        .hint(1)
        .shown("↑↓"),
    b(Act::Left, &[LEFT, ch('h')], &[Inbox], "folder").title("Previous folder"),
    b(Act::Right, &[RIGHT, ch('l')], &[Inbox], "folder")
        .title("Next folder (Inbox, Unread, Starred, Sent, Archived)")
        .hint(3)
        .shown("←→"),
    b(Act::Open, &[ENTER], &[Inbox], "open")
        .title("Open the conversation")
        .hint(2),
    b(Act::Back, &[ESC, BACKSPACE], &[Inbox], "home")
        .title("Back to the home screen")
        .hint(7),
    b(Act::Open, &[ENTER, RIGHT, ch('l')], &[InboxWidget], "open")
        .title("Open the Inbox")
        .hint(1),
    // The home screen keeps room for more of these boxes, and the arrows are
    // how they will be moved between. With one box there is nowhere to go,
    // but the keys have to mean this rather than fall through to the course
    // list hidden behind the box.
    b(Act::Up, &[UP, ch('k')], &[InboxWidget], "widget").title("Previous widget"),
    b(Act::Down, &[DOWN, ch('j')], &[InboxWidget], "widget").title("Next widget"),
    b(Act::Back, &[ESC, BACKSPACE], &[InboxWidget], "courses")
        .title("Back to the course list")
        .hint(7),
    b(Act::Up, &[UP, ch('k')], &[List, Page, Help], "move").title("Move up"),
    b(Act::Down, &[DOWN, ch('j')], &[List], "move")
        .title("Move down")
        .hint(1)
        .shown("↑↓"),
    b(Act::Down, &[DOWN, ch('j')], &[Page, Help], "scroll")
        .title("Scroll down")
        .hint(1)
        .shown("↑↓"),
    b(Act::Up, &[UP], &[Menu, MenuSearch], "move").title("Move up"),
    b(Act::Down, &[DOWN], &[Menu, MenuSearch], "move").title("Move down"),
    b(Act::Up, &[UP], &[PathInput], "suggestions").title("Previous suggestion"),
    b(Act::Down, &[DOWN], &[PathInput], "suggestions")
        .title("Next suggestion")
        .hint(3)
        .shown("↑↓"),
    b(Act::PageUp, &[PGUP], &[Screen, Help], "page up").title("Page up"),
    b(Act::PageDown, &[PGDN], &[Screen, Help], "page down").title("Page down"),
    b(Act::Top, &[HOME, ch('g')], &[Screen, Help], "top").title("Go to the top"),
    b(Act::Bottom, &[END, ch('G')], &[Screen, Help], "bottom").title("Go to the bottom"),
    // Opening and going back.
    b(Act::Open, &[ENTER, RIGHT, ch('l')], &[Feedback], "open")
        .title("Open the file attached to the selected comment")
        .hint(2),
    b(Act::Open, &[ENTER, RIGHT, ch('l')], &[Links], "open")
        .title("Open the selected file or link")
        .hint(2),
    b(Act::Open, &[ENTER, RIGHT, ch('l')], &[List], "open")
        .title("Open the selected item")
        .hint(2),
    b(Act::Open, &[ENTER], &[Menu, MenuSearch], "run")
        .title("Run the selected action")
        .hint(1),
    b(
        Act::Back,
        &[ESC, LEFT, ch('h'), BACKSPACE],
        &[Links, Submissions, Feedback],
        "done",
    )
    .title("Back to the text")
    .hint(7),
    b(Act::Back, &[ESC, BACKSPACE], &[Screen], "back")
        .title("Go back")
        .hint(8),
    b(Act::Back, &[LEFT, ch('h')], &[List, Page], "back").title("Go back"),
    b(Act::Back, &[ESC], &[Menu, Help], "close")
        .title("Close")
        .hint(2),
    // Tabs and sections.
    b(Act::NextSection, &[TAB], &[CourseTabs], "next tab")
        .title("Next tab")
        .hint(6),
    b(Act::PrevSection, &[BACKTAB], &[CourseTabs], "previous tab").title("Previous tab"),
    b(Act::NextSection, &[TAB], &[Courses], "section")
        .title("Move between the courses, the calendar and the Inbox")
        .hint(6),
    b(Act::PrevSection, &[BACKTAB], &[Courses], "section")
        .title("Move back between the courses, the calendar and the Inbox"),
    b(Act::NextSection, &[TAB], &[Sections], "next section")
        .title("Next section (text, files & links, submissions, feedback)")
        .hint(5),
    b(
        Act::PrevSection,
        &[BACKTAB],
        &[Sections],
        "previous section",
    )
    .title("Previous section"),
    // Screen-wide.
    b(Act::Palette, &[ch('/'), Key::Ctrl('k')], &[Screen], "search")
        .title("Search for anything")
        .hint(7)
        .shown("/"),
    b(Act::Search, &[ch('/')], &[Menu], "search")
        .title("Search the actions")
        .hint(3),
    b(Act::Refresh, &[ch('r')], &[Screen], "refresh")
        .title("Refresh from Canvas")
        .menu('r', Group::Here)
        .hint(9),
    b(Act::Browser, &[ch('o')], &[Screen], "browser")
        .title("Open in the browser")
        .menu('o', Group::Here)
        .hint(10),
    b(Act::Menu, &[ch(' ')], &[Screen], "actions")
        .title("Actions for what's selected")
        .hint(98)
        .shown("Space"),
    b(Act::Menu, &[ch(' ')], &[Menu], "close").title("Close the actions menu"),
    b(Act::Help, &[ch('?')], &[Screen, Menu, Help], "help")
        .title("Keys and help")
        .menu('?', Group::App)
        .hint(99),
    b(Act::Quit, &[ch('q')], &[Screen], "quit")
        .title("Quit")
        .menu('q', Group::App),
    // Files.
    b(Act::Download, &[ch('d')], &[Feedback], "download")
        .title("Download the selected comment's files")
        .menu('d', Group::Selected)
        .hint(3),
    b(Act::DownloadAll, &[ch('D')], &[Feedback], "all feedback")
        .title("Download every file attached to this feedback")
        .menu('D', Group::Here)
        .hint(4),
    b(Act::Download, &[ch('d')], &[Files, Links], "download")
        .title("Download")
        .menu('d', Group::Selected)
        .hint(3),
    b(Act::DownloadAll, &[ch('D')], &[Files], "all")
        .title("Download everything here")
        .menu('D', Group::Here)
        .hint(4),
    b(Act::DownloadAll, &[ch('D')], &[HasLinks], "all files")
        .title("Download all files")
        .menu('D', Group::Here)
        .hint(6),
    b(Act::ShowFolder, &[ch('e')], &[Files, Links], "folder")
        .title("Show in File Explorer")
        .menu('e', Group::Selected)
        .hint(5),
    b(Act::ShowFolder, &[ch('e')], &[Courses, Page], "folder")
        .title("Show the folder in File Explorer")
        .menu('e', Group::Here),
    b(
        Act::JumpToLink,
        &[
            ch('1'),
            ch('2'),
            ch('3'),
            ch('4'),
            ch('5'),
            ch('6'),
            ch('7'),
            ch('8'),
            ch('9'),
        ],
        &[HasLinks],
        "link",
    )
    .title("Jump to file or link 1–9")
    .shown("1–9"),
    // Assignments.
    b(Act::Submit, &[ch('s')], &[Assignment], "submit")
        .title("Submit files")
        .menu('s', Group::Here)
        .hint(4),
    b(Act::SubmitText, &[ch('t')], &[Assignment], "text")
        .title("Submit typed text")
        .menu('t', Group::Here)
        .hint(5),
    b(Act::SubmitUrl, &[ch('u')], &[Assignment], "URL")
        .title("Submit a website URL")
        .menu('u', Group::Here)
        .hint(5),
    // Courses.
    b(Act::HideCourse, &[], &[Courses], "hide")
        .title("Hide or restore this course")
        .menu('h', Group::Selected),
    b(Act::ShowHidden, &[], &[Courses], "hidden")
        .title("Show or stop showing hidden courses")
        .menu('H', Group::Here),
    b(Act::ShowGrades, &[], &[Courses], "grades")
        .title("Show or hide the current grade of every course")
        .menu('g', Group::Here),
    // The app.
    b(Act::OpenLibrary, &[], &[Screen], "your files")
        .title("Open your canter folder")
        .menu('f', Group::App),
    b(Act::SignIn, &[ch('L')], &[SignedOut], "sign in")
        .title("Sign in")
        .hint(1),
    b(Act::SignIn, &[], &[Screen], "sign in")
        .title("Sign in again")
        .menu('L', Group::App),
    // The Inbox.
    b(Act::Reply, &[ch('R')], &[Conversation], "reply")
        .title("Reply to everyone in this conversation")
        .menu('R', Group::Here)
        .hint(5),
    b(Act::NewMessage, &[ch('N')], &[Courses], "message")
        .title("Write to this course's teachers")
        .menu('N', Group::Selected),
    // Text boxes and confirmation.
    b(Act::Open, &[ENTER], &[Palette], "go")
        .title("Open what is selected")
        .hint(1),
    b(Act::Up, &[UP], &[Palette], "move").title("Previous result"),
    b(Act::Down, &[DOWN], &[Palette], "move")
        .title("Next result")
        .hint(2)
        .shown("↑↓"),
    b(Act::InputClear, &[Key::Ctrl('u')], &[Palette], "clear")
        .title("Clear what is typed")
        .hint(3),
    b(Act::ShowHidden, &[Key::Ctrl('h')], &[Palette], "hidden")
        .title("Search inside hidden courses too")
        .hint(5),
    b(Act::InputCancel, &[ESC], &[Palette], "close")
        .title("Close the search")
        .hint(4),
    b(Act::InputDone, &[ENTER], &[PathInput], "add / review")
        .title("Add the file, or review when done")
        .hint(1),
    b(Act::InputComplete, &[TAB], &[PathInput], "complete")
        .title("Complete the path")
        .hint(2),
    b(
        Act::InputClear,
        &[Key::Ctrl('u')],
        &[PathInput],
        "clear path",
    )
    .title("Clear the path")
    .hint(4),
    b(Act::InputCancel, &[ESC], &[PathInput], "cancel")
        .title("Cancel submitting")
        .hint(5),
    b(Act::InputDone, &[ENTER], &[TextInput], "review")
        .title("Review what will be sent")
        .hint(1),
    b(
        Act::LoadTextFile,
        &[Key::Ctrl('o')],
        &[TextInput],
        "from a file",
    )
    .title("Read the text from a file instead")
    .hint(3),
    b(Act::InputClear, &[Key::Ctrl('u')], &[TextInput], "clear")
        .title("Clear what is typed")
        .hint(4),
    b(Act::InputCancel, &[ESC], &[TextInput], "cancel")
        .title("Cancel submitting")
        .hint(5),
    b(Act::InputCancel, &[ESC], &[MenuSearch], "stop searching")
        .title("Stop searching")
        .hint(2),
    b(Act::Confirm, &[ch('y')], &[Review], "submit now")
        .title("Submit to Canvas")
        .hint(1),
    b(Act::InputCancel, &[ESC, ch('n')], &[Review], "edit")
        .title("Go back and edit what will be sent")
        .hint(2)
        .shown("n/Esc"),
    // Writing a message, and the review that precedes sending it.
    b(Act::InputDone, &[ENTER], &[Compose], "new line")
        .title("Start a new line, or move on from the subject")
        .hint(1),
    b(Act::InputComplete, &[Key::Ctrl('d')], &[Compose], "review")
        .title("Review the message before it is sent")
        .hint(2),
    b(Act::NextSection, &[TAB], &[Compose], "next field")
        .title("Move to the next field")
        .hint(3),
    b(Act::PrevSection, &[BACKTAB], &[Compose], "previous field")
        .title("Move to the previous field")
        .hint(4),
    b(
        Act::InputClear,
        &[Key::Ctrl('u')],
        &[Compose],
        "clear field",
    )
    .title("Clear this field")
    .hint(5),
    b(Act::InputCancel, &[ESC], &[Compose], "discard")
        .title("Discard the message")
        .hint(6),
    b(Act::Confirm, &[ch('y')], &[MessageReview], "send now")
        .title("Send this message through the Canvas Inbox")
        .hint(1),
    b(
        Act::InputCancel,
        &[ESC, ch('n')],
        &[MessageReview],
        "keep writing",
    )
    .title("Go back and edit the message")
    .hint(2)
    .shown("n/Esc"),
];

/// Core keys and the one action each may have wherever it is bound
/// (text boxes excepted).
#[cfg(test)]
pub const CORE: &[(Key, Act)] = &[
    (UP, Act::Up),
    (DOWN, Act::Down),
    (ENTER, Act::Open),
    (ESC, Act::Back),
    (TAB, Act::NextSection),
    (BACKTAB, Act::PrevSection),
    (ch('/'), Act::Palette),
    (ch('r'), Act::Refresh),
    (ch('o'), Act::Browser),
    (ch(' '), Act::Menu),
    (ch('?'), Act::Help),
    (ch('q'), Act::Quit),
];

/// Scopes in which letters are text rather than commands.
pub fn typing(scopes: &[Scope]) -> bool {
    scopes
        .iter()
        .any(|s| matches!(s, Palette | PathInput | TextInput | MenuSearch | Compose))
}

pub fn key_of(event: &KeyEvent) -> Key {
    match event.code {
        KeyCode::Char(c) if event.modifiers.contains(KeyModifiers::CONTROL) => {
            Key::Ctrl(c.to_ascii_lowercase())
        }
        code => Key::Code(code),
    }
}

fn applies(binding: &Binding, scopes: &[Scope]) -> bool {
    binding.scopes.iter().any(|s| scopes.contains(s))
}

/// The binding a key press triggers here, if any.
pub fn lookup(key: Key, scopes: &[Scope]) -> Option<&'static Binding> {
    BINDINGS
        .iter()
        .find(|b| applies(b, scopes) && b.keys.contains(&key))
}

/// Every binding that applies here, one per action (the most specific).
pub fn here(scopes: &[Scope]) -> Vec<&'static Binding> {
    let mut seen = Vec::new();
    let mut out = Vec::new();
    for binding in BINDINGS.iter().filter(|b| applies(b, scopes)) {
        // Rows without keys only add a menu entry; keep the keyed row for display.
        let key = (binding.act, binding.keys.is_empty());
        if !seen.contains(&key) {
            seen.push(key);
            out.push(binding);
        }
    }
    out
}

/// The actions menu: entries with letters, grouped, one per action.
pub fn menu(scopes: &[Scope], available: &dyn Fn(Act) -> bool) -> Vec<&'static Binding> {
    let mut entries: Vec<&'static Binding> = Vec::new();
    for binding in BINDINGS
        .iter()
        .filter(|b| applies(b, scopes) && b.menu.is_some())
    {
        if available(binding.act) && !entries.iter().any(|e| e.act == binding.act) {
            entries.push(binding);
        }
    }
    // Within a group: what is particular to this screen first, then what works
    // everywhere, and Quit last.
    entries.sort_by_key(|e| {
        let general = e.scopes.contains(&Scope::Screen);
        (e.group, e.act == Act::Quit, general)
    });
    entries
}

pub fn show(key: Key) -> String {
    match key {
        Key::Ctrl(c) => format!("Ctrl+{}", c.to_ascii_uppercase()),
        Key::Code(code) => match code {
            KeyCode::Char(' ') => "Space".into(),
            KeyCode::Char(c) => c.to_string(),
            KeyCode::Enter => "Enter".into(),
            KeyCode::Esc => "Esc".into(),
            KeyCode::Tab => "Tab".into(),
            KeyCode::BackTab => "Shift+Tab".into(),
            KeyCode::Backspace => "Backspace".into(),
            KeyCode::Up => "↑".into(),
            KeyCode::Down => "↓".into(),
            KeyCode::Left => "←".into(),
            KeyCode::Right => "→".into(),
            KeyCode::Home => "Home".into(),
            KeyCode::End => "End".into(),
            KeyCode::PageUp => "PgUp".into(),
            KeyCode::PageDown => "PgDn".into(),
            other => format!("{other:?}"),
        },
    }
}

/// All of a binding's keys, for the help screen.
pub fn show_all(binding: &Binding) -> String {
    if let Some(shown) = binding.shown
        && binding.act == Act::JumpToLink
    {
        return shown.to_string();
    }
    binding
        .keys
        .iter()
        .map(|k| show(*k))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The hint line: the most useful keys here that fit in `width`, always
/// ending with `Space actions` and `? help` when those apply.
pub fn hint_line(
    scopes: &[Scope],
    available: &dyn Fn(Act) -> bool,
    width: usize,
) -> Vec<(String, &'static str)> {
    let mut candidates: Vec<&'static Binding> = here(scopes)
        .into_iter()
        .filter(|b| b.hint > 0 && !b.keys.is_empty() && available(b.act))
        .collect();
    candidates.sort_by_key(|b| b.hint);
    let shown = |b: &Binding| {
        b.shown
            .map(str::to_string)
            .unwrap_or_else(|| show(b.keys[0]))
    };
    let size = |b: &Binding| shown(b).chars().count() + 1 + b.label.chars().count();
    let fixed: Vec<&'static Binding> = candidates
        .iter()
        .copied()
        .filter(|b| b.hint >= 98)
        .collect();
    let mut used: usize = fixed.iter().map(|b| size(b) + 2).sum();
    let mut chosen: Vec<&'static Binding> = Vec::new();
    for binding in candidates.iter().copied().filter(|b| b.hint < 98) {
        let need = size(binding) + 2;
        if used + need <= width {
            used += need;
            chosen.push(binding);
        }
    }
    chosen.extend(fixed);
    chosen.into_iter().map(|b| (shown(b), b.label)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every combination of scopes the app can be in.
    pub fn contexts() -> Vec<Vec<Scope>> {
        let mut out = vec![
            vec![Screen, List, Courses],
            vec![Screen, Courses, Calendar],
            vec![Screen, Courses, Day],
            vec![Screen, Courses, InboxWidget],
            vec![Screen, Inbox],
            vec![Screen, Page, Sections, Conversation],
            vec![Screen, Page, Sections, Conversation, HasLinks],
            vec![Screen, Page, Sections, Conversation, HasLinks, Links],
            vec![Screen, List, CourseTabs],
            vec![Screen, List, CourseTabs, Files],
            vec![Screen, Page, Sections],
            vec![Screen, Page, Sections, HasLinks],
            vec![Screen, Page, Sections, HasLinks, Links],
            vec![Screen, Page, Sections, Assignment],
            vec![Screen, Page, Sections, Assignment, HasLinks],
            vec![Screen, Page, Sections, Assignment, HasLinks, Links],
            vec![Screen, Page, Sections, Assignment, Submissions],
            vec![Screen, Page, Sections, Assignment, HasLinks, Submissions],
            vec![Screen, Page, Sections, Assignment, Feedback],
            vec![Screen, Page, Sections, Assignment, HasLinks, Feedback],
            vec![Screen, Page, CourseTabs],
            vec![Screen, Page, CourseTabs, HasLinks],
            vec![Screen, Page, CourseTabs, HasLinks, Links],
        ];
        let signed_out: Vec<Vec<Scope>> = out
            .iter()
            .map(|c| c.iter().copied().chain([SignedOut]).collect())
            .collect();
        out.extend(signed_out);
        out.extend([
            vec![Palette],
            vec![PathInput],
            vec![TextInput],
            vec![Review],
            vec![Compose],
            vec![MessageReview],
            vec![Menu],
            vec![MenuSearch],
            vec![Help],
        ]);
        out
    }

    fn all_keys() -> Vec<Key> {
        let mut keys: Vec<Key> = Vec::new();
        for b in BINDINGS {
            for k in b.keys {
                if !keys.contains(k) {
                    keys.push(*k);
                }
            }
        }
        keys
    }

    #[test]
    fn no_key_does_two_things_in_one_place() {
        for scopes in contexts() {
            for key in all_keys() {
                let acts: Vec<Act> = BINDINGS
                    .iter()
                    .filter(|b| applies(b, &scopes) && b.keys.contains(&key))
                    .map(|b| b.act)
                    .collect();
                assert!(
                    acts.windows(2).all(|w| w[0] == w[1]),
                    "{} means {acts:?} in {scopes:?}",
                    show(key)
                );
            }
        }
    }

    #[test]
    fn core_keys_keep_one_meaning() {
        for scopes in contexts()
            .into_iter()
            // The confirmation screens bind Esc to leaving the draft alone,
            // and the open actions menu binds `/` to searching itself rather
            // than opening the palette. Both are overlays with their own box
            // on screen, so neither can be mistaken for the screen beneath.
            .filter(|s| {
                !typing(s)
                    && !s.contains(&Review)
                    && !s.contains(&MessageReview)
                    && !s.contains(&Menu)
            })
        {
            for (key, act) in CORE {
                if let Some(binding) = lookup(*key, &scopes) {
                    assert_eq!(binding.act, *act, "{} in {scopes:?}", show(*key));
                }
            }
        }
    }

    #[test]
    fn core_keys_work_on_every_screen() {
        for scopes in contexts().into_iter().filter(|s| s.contains(&Screen)) {
            for key in [ESC, ch(' '), ch('?'), ch('q'), ch('r'), ch('o'), UP, DOWN] {
                assert!(
                    lookup(key, &scopes).is_some(),
                    "{} unbound in {scopes:?}",
                    show(key)
                );
            }
        }
    }

    #[test]
    fn menu_letters_are_unique_and_do_not_clash_with_menu_keys() {
        for scopes in contexts().into_iter().filter(|s| s.contains(&Screen)) {
            let entries = menu(&scopes, &|_| true);
            for (i, a) in entries.iter().enumerate() {
                for b in &entries[i + 1..] {
                    assert_ne!(
                        a.menu, b.menu,
                        "{:?} and {:?} share a letter in {scopes:?}",
                        a.act, b.act
                    );
                }
                // Keys the open menu uses itself must mean the same as the entry.
                if let Some(own) = lookup(ch(a.menu.unwrap()), &[Menu]) {
                    assert_eq!(own.act, a.act, "menu letter {:?} clashes", a.menu);
                }
            }
        }
    }

    #[test]
    fn every_action_is_reachable_and_listed_in_help() {
        for act in Act::ALL {
            assert!(
                BINDINGS.iter().any(|b| b.act == act),
                "{act:?} has no binding"
            );
            assert!(
                BINDINGS
                    .iter()
                    .any(|b| b.act == act && (!b.keys.is_empty() || b.menu.is_some())),
                "{act:?} cannot be triggered"
            );
        }
    }

    #[test]
    fn hint_line_fits_80_columns_and_keeps_actions_and_help() {
        for scopes in contexts() {
            let line = hint_line(&scopes, &|_| true, 80);
            let width: usize = line
                .iter()
                .map(|(k, l)| k.chars().count() + 1 + l.chars().count() + 2)
                .sum();
            assert!(width <= 82, "{width} columns in {scopes:?}: {line:?}");
            if scopes.contains(&Screen) {
                let labels: Vec<&str> = line.iter().map(|(_, l)| *l).collect();
                assert!(
                    labels.ends_with(&["actions", "help"]),
                    "{scopes:?}: {labels:?}"
                );
            }
        }
    }

    #[test]
    fn shift_letters_and_ctrl_are_distinct_keys() {
        let d = KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE);
        let big_d = KeyEvent::new(KeyCode::Char('D'), KeyModifiers::SHIFT);
        let ctrl_u = KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL);
        assert_eq!(
            lookup(key_of(&d), &[Screen, List, CourseTabs, Files])
                .unwrap()
                .act,
            Act::Download
        );
        assert_eq!(
            lookup(key_of(&big_d), &[Screen, List, CourseTabs, Files])
                .unwrap()
                .act,
            Act::DownloadAll
        );
        assert_eq!(
            lookup(key_of(&ctrl_u), &[PathInput]).unwrap().act,
            Act::InputClear
        );
    }
}
