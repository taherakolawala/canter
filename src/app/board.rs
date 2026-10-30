//! The home screen's widgets as the app sees them: what each one lists, what
//! `Enter` opens on it, what it reads from Canvas, and arranging them.
//!
//! Where each widget goes is worked out in `crate::home`; this is everything
//! that needs the app's data.

use std::collections::HashSet;

use chrono::{DateTime, Duration, Local, NaiveDate, Utc};
use ratatui::layout::Rect;

use super::{App, CourseTab, Effect, Section, SyncState, View, ViewKind, WARM_AT_ONCE};
use crate::cache::ResourceKey;
use crate::calendar::{self, Item, Kind, Progress};
use crate::home::{self, Arranged, Side, Size, Tile, Widget};
use crate::keys::Act;
use crate::library::Recent;
use crate::model::Course;

/// To do looks this many days past today.
pub const TODO_DAYS: i64 = 7;
/// Announcements go back this many days, today included.
pub const NEWS_DAYS: i64 = 14;

/// Arranging the home screen: the selected place on the board, and whether
/// the widget there has been picked up to move.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HomeEdit {
    pub side: Side,
    /// The place on that side. On an empty side, the one place there is.
    pub index: usize,
    /// The arrows move the selected widget instead of the selection.
    pub holding: bool,
    /// The list of widgets to add, while it is open, and its selection.
    pub picker: Option<usize>,
    /// What had focus before, to go back to.
    before: Section,
}

impl HomeEdit {
    /// The widget at the selected place, if there is one.
    pub fn selected(&self, board: &home::Board) -> Option<Widget> {
        board.side(self.side).get(self.index).map(|t| t.widget)
    }
}

/// One thing a widget lists, and so what `Enter` opens there.
#[derive(Clone, Debug, PartialEq)]
pub enum HomeEntry {
    /// The Inbox screen itself.
    Inbox,
    Conversation(crate::inbox::Summary),
    /// Something from the planner or from the missing-work list.
    Item(Item),
    /// A course's grades, by its id.
    Course(String),
    Graded(Graded),
    File(Recent),
}

/// Where a widget's reads stand, for its box.
#[derive(Clone, Debug, Default)]
pub struct WidgetSync {
    /// A read for it is out.
    pub syncing: bool,
    /// The first of its reads that failed, if one did.
    pub failed: Option<crate::canvas::CanvasError>,
    /// Everything it lists has been read once, from Canvas or a saved copy.
    pub loaded: bool,
}

/// A score Canvas gave, for the Grades widget.
#[derive(Clone, Debug, PartialEq)]
pub struct Graded {
    pub course_id: String,
    pub assignment_id: String,
    pub title: String,
    pub score: Option<f64>,
    pub grade: Option<String>,
    pub points: Option<f64>,
    pub at: DateTime<Utc>,
}

impl App {
    /// The board as it is drawn at the terminal's size: the space between
    /// the top bar and the hint line, less the course list.
    pub fn arranged(&self) -> Arranged {
        let (w, h) = self.size;
        let main = Rect::new(0, 1, w, h.saturating_sub(2));
        let editing = self.home_edit.is_some();
        let (_, _, area) = home::split(main, self.course_rows(), &self.board, editing);
        home::arrange(&self.board, area)
    }

    /// Lines the course list would like: its header and a row per course.
    pub fn course_rows(&self) -> u16 {
        u16::try_from(self.visible_rows().len().max(1))
            .unwrap_or(u16::MAX - 1)
            .saturating_add(1)
    }

    pub fn widget_is_small(&self, widget: Widget) -> bool {
        self.board
            .tile(widget)
            .is_some_and(|t| t.size == Size::Small)
    }

    pub fn widget_size(&self, widget: Widget) -> Option<Size> {
        self.board.tile(widget).map(|t| t.size)
    }

    /// The courses the widgets cover: the course list's, with hidden ones
    /// only while those are being shown.
    pub fn home_courses(&self) -> Vec<&Course> {
        self.courses
            .data
            .as_deref()
            .unwrap_or(&[])
            .iter()
            .filter(|c| self.show_hidden || !self.hidden_courses.contains(&c.id))
            .collect()
    }

    fn shown_course(&self, course_id: Option<&str>) -> bool {
        self.show_hidden || course_id.is_none_or(|c| !self.hidden_courses.contains(c))
    }

    /// Planner entries dated `from` to `to` (local days, both included) in
    /// the months read so far, each once, in time order, leaving out hidden
    /// courses.
    pub fn planner_between(&self, from: NaiveDate, to: NaiveDate) -> Vec<Item> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for month in calendar::months_covering(from, to) {
            let Some(items) = self.planner.get(&month).and_then(|r| r.data.as_deref()) else {
                continue;
            };
            for item in items {
                let Some(day) = item.day() else { continue };
                if day < from || day > to || !self.shown_course(item.course_id.as_deref()) {
                    continue;
                }
                if seen.insert((item.kind, item.id.clone())) {
                    out.push(item.clone());
                }
            }
        }
        out.sort_by(|a, b| a.at.cmp(&b.at).then_with(|| a.title.cmp(&b.title)));
        out
    }

    /// Missing work, most recently due first.
    pub fn missing_work(&self) -> Vec<Item> {
        self.missing
            .data
            .as_deref()
            .unwrap_or(&[])
            .iter()
            .filter(|i| self.shown_course(i.course_id.as_deref()))
            .cloned()
            .collect()
    }

    /// What is still to do from today to the end of the week ahead, in
    /// time order. Work Canvas already calls missing is in `missing_work`.
    pub fn upcoming_work(&self, now: DateTime<Utc>) -> Vec<Item> {
        let today = now.with_timezone(&Local).date_naive();
        let missing: HashSet<String> = self.missing_work().into_iter().map(|i| i.id).collect();
        self.planner_between(today, today + Duration::days(TODO_DAYS))
            .into_iter()
            .filter(|i| !matches!(i.kind, Kind::Announcement | Kind::Event))
            .filter(|i| !i.progress.finished() && i.progress != Progress::Missing)
            .filter(|i| !missing.contains(&i.id))
            .collect()
    }

    /// The last two weeks' announcements, newest first.
    pub fn recent_announcements(&self, today: NaiveDate) -> Vec<Item> {
        let mut items: Vec<Item> = self
            .planner_between(today - Duration::days(NEWS_DAYS - 1), today)
            .into_iter()
            .filter(|i| i.kind == Kind::Announcement)
            .collect();
        items.sort_by(|a, b| b.at.cmp(&a.at).then_with(|| a.title.cmp(&b.title)));
        items
    }

    /// Whether the student has yet to read an announcement: not if it was
    /// read here; otherwise as the course's announcement list has it, if that
    /// has been read; otherwise as the planner marks it.
    pub fn announcement_unread(&self, item: &Item) -> bool {
        let Some(course) = item.course_id.as_deref() else {
            return item.new;
        };
        if self
            .announcement_reads
            .contains(&(course.to_string(), item.id.clone()))
        {
            return false;
        }
        let listed = self
            .announcements
            .get(course)
            .and_then(|r| r.data.as_deref())
            .and_then(|list| list.iter().find(|a| a.id == item.id));
        match listed.and_then(|a| a.read_state.as_deref()) {
            Some("read") => false,
            Some("unread") => true,
            _ => item.new,
        }
    }

    /// The Inbox folder's conversations, newest activity first, whichever
    /// folder the Inbox screen was last left on.
    pub fn home_conversations(&self) -> &[crate::inbox::Summary] {
        self.conversations
            .get(crate::inbox::Folder::Inbox.id())
            .and_then(|r| r.data.as_deref())
            .unwrap_or(&[])
    }

    /// The newest scores across the courses, newest first.
    pub fn recent_grades(&self, limit: usize) -> Vec<Graded> {
        let mut out = Vec::new();
        for course in self.home_courses() {
            let Some(grades) = self.grades.get(&course.id).and_then(|r| r.data.as_ref()) else {
                continue;
            };
            for a in grades.groups.iter().flat_map(|g| &g.assignments) {
                let Some(s) = a.submission.as_ref() else { continue };
                let Some(at) = s.graded_at else { continue };
                if s.excused == Some(true) || (s.score.is_none() && s.grade.is_none()) {
                    continue;
                }
                out.push(Graded {
                    course_id: course.id.clone(),
                    assignment_id: a.id.clone(),
                    title: a.title().to_string(),
                    score: s.score,
                    grade: s.grade.clone(),
                    points: a.points_possible,
                    at,
                });
            }
        }
        out.sort_by(|a, b| b.at.cmp(&a.at).then_with(|| a.title.cmp(&b.title)));
        out.truncate(limit);
        out
    }

    /// Whether a course has instructor comments not yet seen here.
    pub fn course_has_new_feedback(&self, course_id: &str) -> bool {
        self.grades
            .get(course_id)
            .and_then(|r| r.data.as_ref())
            .is_some_and(|g| {
                g.feedback
                    .iter()
                    .any(|f| self.unseen_feedback(course_id, &f.assignment_id))
            })
    }

    /// Files downloaded or opened here, latest first.
    pub fn recent_files(&self, limit: usize) -> Vec<Recent> {
        self.library
            .as_ref()
            .map(|l| l.recent(limit))
            .unwrap_or_default()
    }

    /// Everything a widget lists at its size, in order: its rows, and what
    /// `Enter` opens on each. A Small widget lists the one thing it shows.
    pub fn widget_entries(&self, widget: Widget, now: DateTime<Utc>) -> Vec<HomeEntry> {
        let Some(size) = self.widget_size(widget) else {
            return vec![];
        };
        let today = now.with_timezone(&Local).date_naive();
        let small = size == Size::Small;
        let large = size.large();
        match widget {
            // The Medium and Large calendars have a day cursor instead.
            Widget::Calendar => self
                .planner_between(today, today + Duration::days(TODO_DAYS))
                .into_iter()
                .find(|i| i.listed() && !i.progress.finished())
                .map(HomeEntry::Item)
                .into_iter()
                .collect(),
            Widget::Inbox => {
                let mut out = vec![HomeEntry::Inbox];
                if !small {
                    let shown = if large { 8 } else { 3 };
                    out.extend(
                        self.home_conversations()
                            .iter()
                            .take(shown)
                            .cloned()
                            .map(HomeEntry::Conversation),
                    );
                }
                out
            }
            Widget::Todo => {
                let missing = self.missing_work();
                let upcoming = self.upcoming_work(now);
                if small {
                    return upcoming
                        .into_iter()
                        .next()
                        .or_else(|| missing.into_iter().next())
                        .map(HomeEntry::Item)
                        .into_iter()
                        .collect();
                }
                let (missing_shown, upcoming_shown) = if large { (usize::MAX, usize::MAX) } else { (2, 3) };
                missing
                    .into_iter()
                    .take(missing_shown)
                    .chain(upcoming.into_iter().take(upcoming_shown))
                    .map(HomeEntry::Item)
                    .collect()
            }
            Widget::Announcements => {
                let mut items = self.recent_announcements(today);
                if small {
                    let first = items
                        .iter()
                        .position(|i| self.announcement_unread(i))
                        .unwrap_or(0);
                    return items
                        .into_iter()
                        .nth(first)
                        .map(HomeEntry::Item)
                        .into_iter()
                        .collect();
                }
                if large {
                    // Unread first; the sort keeps each part newest first.
                    items.sort_by_key(|i| !self.announcement_unread(i));
                    items.truncate(12);
                } else {
                    items.truncate(4);
                }
                items.into_iter().map(HomeEntry::Item).collect()
            }
            Widget::Grades => {
                if small {
                    return match self.recent_grades(1).into_iter().next() {
                        Some(g) => vec![HomeEntry::Graded(g)],
                        None => self
                            .home_courses()
                            .first()
                            .map(|c| HomeEntry::Course(c.id.clone()))
                            .into_iter()
                            .collect(),
                    };
                }
                let mut out: Vec<HomeEntry> = self
                    .home_courses()
                    .iter()
                    .map(|c| HomeEntry::Course(c.id.clone()))
                    .collect();
                if large {
                    out.extend(self.recent_grades(6).into_iter().map(HomeEntry::Graded));
                }
                out
            }
            Widget::RecentFiles => {
                let shown = match size {
                    Size::Small => 1,
                    Size::Medium => 4,
                    _ => 12,
                };
                self.recent_files(shown)
                    .into_iter()
                    .map(HomeEntry::File)
                    .collect()
            }
        }
    }

    /// The entry selected in a widget (a Small one has just the one).
    pub fn selected_entry(&self, widget: Widget, now: DateTime<Utc>) -> Option<HomeEntry> {
        let entries = self.widget_entries(widget, now);
        let row = if self.widget_is_small(widget) {
            0
        } else {
            self.widget_row.min(entries.len().saturating_sub(1))
        };
        entries.into_iter().nth(row)
    }

    /// The row the focused widget shows as selected, if it is this one.
    pub fn widget_selection(&self, widget: Widget, entries: usize) -> Option<usize> {
        (self.view().kind == ViewKind::Courses
            && self.home_edit.is_none()
            && self.focus().widget() == Some(widget)
            && entries > 0)
            .then(|| self.widget_row.min(entries - 1))
    }

    pub(super) fn open_widget_entry(&mut self, focus: Section) -> Vec<Effect> {
        let Some(widget) = focus.widget() else {
            return vec![];
        };
        match self.selected_entry(widget, Utc::now()) {
            Some(entry) => self.open_entry(entry),
            None => vec![],
        }
    }

    fn open_entry(&mut self, entry: HomeEntry) -> Vec<Effect> {
        let next = match entry {
            HomeEntry::Inbox => return self.open_inbox(),
            HomeEntry::Item(item) => return self.open_item(item),
            HomeEntry::File(file) => return self.open_recent_file(file),
            HomeEntry::Conversation(c) => ViewKind::Conversation {
                conversation_id: c.id.clone(),
                subject: c.subject().to_string(),
            },
            HomeEntry::Course(id) => {
                let name = self.course_title(&id);
                CourseTab::Grades.view(id, name)
            }
            HomeEntry::Graded(g) => ViewKind::Detail {
                course_name: self.course_title(&g.course_id),
                course_id: g.course_id,
                assignment_id: g.assignment_id,
                title: g.title,
            },
        };
        self.stack.push(View::new(next));
        self.enter_current(false)
    }

    /// A planner entry: assignments and announcements open here, anything
    /// else (quizzes among them) in the browser.
    pub(super) fn open_item(&mut self, item: Item) -> Vec<Effect> {
        let next = match (item.kind, item.course_id.clone()) {
            (Kind::Assignment, Some(course_id)) => ViewKind::Detail {
                course_name: self.course_title(&course_id),
                course_id,
                assignment_id: item.id.clone(),
                title: item.title.clone(),
            },
            (Kind::Announcement, Some(course_id)) => ViewKind::Announcement {
                course_name: self.course_title(&course_id),
                course_id,
                announcement_id: item.id.clone(),
                title: item.title.clone(),
            },
            _ => return vec![Effect::OpenUrl(self.calendar_url(&item))],
        };
        self.stack.push(View::new(next));
        self.enter_current(false)
    }

    fn open_recent_file(&mut self, file: Recent) -> Vec<Effect> {
        let Some(lib) = self.library.clone() else {
            self.notice = Some("The file library is unavailable.".into());
            return vec![];
        };
        // The index can name a copy that has since been deleted by hand.
        let Some(entry) = lib.entry(&file.file_id) else {
            self.notice = Some(format!("{} is no longer in your canter folder.", file.name()));
            return vec![];
        };
        let path = if entry.paths.contains(&file.path) {
            file.path
        } else {
            entry.paths[0].clone()
        };
        lib.opened(&file.file_id);
        vec![Effect::OpenPath(path)]
    }

    /// The arrows on a widget: along its list, or on a Small one, to the
    /// widget before or after it. They never reach the course list behind.
    pub(super) fn move_in_widget(&mut self, focus: Section, act: Act) -> Vec<Effect> {
        let Some(widget) = focus.widget() else {
            return vec![];
        };
        if self.widget_is_small(widget) {
            let order: Vec<Section> = self.sections().into_iter().skip(1).collect();
            let at = order.iter().position(|s| *s == focus);
            let next = match (act, at) {
                (Act::Up, Some(i)) if i > 0 => Some(order[i - 1]),
                (Act::Down, Some(i)) if i + 1 < order.len() => Some(order[i + 1]),
                _ => None,
            };
            if let Some(next) = next {
                self.widget_row = 0;
                self.view_mut().focus = next;
            }
            return vec![];
        }
        let count = self.widget_entries(widget, Utc::now()).len();
        let delta: isize = match act {
            Act::Up => -1,
            Act::Down => 1,
            Act::PageUp => -5,
            Act::PageDown => 5,
            Act::Top => isize::MIN / 2,
            Act::Bottom => isize::MAX / 2,
            _ => 0,
        };
        self.widget_row = (self.widget_row.min(count.saturating_sub(1)) as isize + delta)
            .clamp(0, count.saturating_sub(1) as isize) as usize;
        vec![]
    }

    /// `o` on a widget: the Canvas page of what is selected in it.
    pub(super) fn widget_url(&self, now: DateTime<Utc>) -> String {
        let origin = &self.origin;
        let Some(widget) = self.focus().widget() else {
            return format!("{origin}/");
        };
        match self.selected_entry(widget, now) {
            Some(HomeEntry::Inbox) => format!("{origin}/conversations"),
            Some(HomeEntry::Conversation(c)) => format!("{origin}/conversations/{}", c.id),
            Some(HomeEntry::Item(item)) => self.calendar_url(&item),
            Some(HomeEntry::Course(id)) => format!("{origin}/courses/{id}/grades"),
            Some(HomeEntry::Graded(g)) => format!(
                "{origin}/courses/{}/assignments/{}",
                g.course_id, g.assignment_id
            ),
            Some(HomeEntry::File(f)) if crate::model::valid_id(&f.file_id) => {
                format!("{origin}/files/{}", f.file_id)
            }
            _ => match widget {
                Widget::Calendar => format!("{origin}/calendar"),
                Widget::Inbox => format!("{origin}/conversations"),
                Widget::Grades => format!("{origin}/grades"),
                _ => format!("{origin}/"),
            },
        }
    }

    /// What a widget reads at its size, besides the Grades widget's
    /// per-course reads (which `queue_home_grades` spreads out).
    fn widget_keys(&self, tile: Tile, today: NaiveDate) -> Vec<ResourceKey> {
        let months = |from: NaiveDate, to: NaiveDate| {
            calendar::months_covering(from, to)
                .into_iter()
                .map(ResourceKey::Planner)
                .collect::<Vec<_>>()
        };
        match tile.widget {
            Widget::Calendar => {
                let mut keys = match tile.size {
                    // Today, and the next thing due if not today.
                    Size::Small => months(today, today + Duration::days(TODO_DAYS)),
                    _ => vec![ResourceKey::Planner(calendar::month_key(self.calendar_day))],
                };
                keys.push(ResourceKey::Colors);
                keys
            }
            Widget::Todo => {
                let mut keys = months(today, today + Duration::days(TODO_DAYS));
                keys.push(ResourceKey::Colors);
                keys.push(ResourceKey::Missing);
                keys
            }
            Widget::Announcements => {
                let mut keys = months(today - Duration::days(NEWS_DAYS - 1), today);
                keys.push(ResourceKey::Colors);
                keys
            }
            Widget::Inbox => {
                let unread = crate::inbox::Folder::Unread.id().to_string();
                let mut keys = vec![ResourceKey::Conversations(unread)];
                if tile.size != Size::Small {
                    let inbox = crate::inbox::Folder::Inbox.id().to_string();
                    keys.push(ResourceKey::Conversations(inbox));
                }
                keys
            }
            Widget::Grades | Widget::RecentFiles => vec![],
        }
    }

    /// How a widget's reads stand. Course colours never hold one back. The
    /// Grades widget's course reads count while they are queued or out, but
    /// not when one fails: that course just shows no grade.
    pub fn widget_sync(&self, widget: Widget) -> WidgetSync {
        let Some(tile) = self.board.tile(widget) else {
            return WidgetSync::default();
        };
        let mut out = WidgetSync {
            loaded: true,
            ..Default::default()
        };
        for key in self.widget_keys(tile, Local::now().date_naive()) {
            if key == ResourceKey::Colors {
                continue;
            }
            match self.sync_of(&key) {
                Some(SyncState::Syncing { .. }) => out.syncing = true,
                Some(SyncState::Failed(e)) if out.failed.is_none() => out.failed = Some(e.clone()),
                _ => {}
            }
            out.loaded &= match &key {
                ResourceKey::Planner(m) => self.planner.get(m).is_some_and(|r| r.data.is_some()),
                ResourceKey::Conversations(f) => {
                    self.conversations.get(f).is_some_and(|r| r.data.is_some())
                }
                ResourceKey::Missing => self.missing.data.is_some(),
                _ => true,
            };
        }
        if widget == Widget::Grades {
            out.loaded = self.courses.data.is_some();
            out.syncing = !self.grade_queue.is_empty() || !self.grade_running.is_empty();
        }
        out
    }

    /// Read what the widgets on the board show: saved copies first, then a
    /// fresh read, as every screen does on each visit.
    pub(super) fn read_home(&mut self, explicit: bool) -> Vec<Effect> {
        let today = Local::now().date_naive();
        let mut keys: Vec<ResourceKey> = Vec::new();
        for tile in self.board.tiles().collect::<Vec<_>>() {
            for key in self.widget_keys(tile, today) {
                if !keys.contains(&key) {
                    keys.push(key);
                }
            }
        }
        let mut effects = Vec::new();
        for key in keys {
            self.load_snapshot(&key);
            effects.extend(self.request(key, explicit));
        }
        effects.extend(self.queue_home_grades(explicit));
        effects
    }

    /// Read what one widget needs as it arrives on the board or grows: what
    /// this run has read already is not read again.
    fn read_tile(&mut self, tile: Tile) -> Vec<Effect> {
        let today = Local::now().date_naive();
        let mut effects = Vec::new();
        for key in self.widget_keys(tile, today) {
            self.load_snapshot(&key);
            if !self.read_this_run(&key)
                && !matches!(self.sync_of(&key), Some(SyncState::Syncing { .. }))
            {
                effects.extend(self.request(key, false));
            }
        }
        if tile.widget == Widget::Grades {
            effects.extend(self.queue_home_grades(false));
        }
        effects
    }

    /// Bring every course's grades up to date for the Grades widget. The
    /// saved copies show at once; the reads go in the background, within the
    /// warm-up's allowance so the screen in front of the user is not kept
    /// waiting, and each course is read once per run, or again on `r`.
    pub(super) fn queue_home_grades(&mut self, explicit: bool) -> Vec<Effect> {
        if self.board.tile(Widget::Grades).is_none() {
            return vec![];
        }
        let ids: Vec<String> = self.home_courses().iter().map(|c| c.id.clone()).collect();
        for id in ids {
            let key = ResourceKey::Grades(id.clone());
            self.load_snapshot(&key);
            if (!explicit && self.read_this_run(&key))
                || self.grade_queue.contains(&id)
                || self.grade_running.contains(&id)
            {
                continue;
            }
            self.grade_queue.push_back(id);
        }
        self.grade_next()
    }

    /// Start as many queued grade reads as the background allowance has room for.
    pub(super) fn grade_next(&mut self) -> Vec<Effect> {
        let mut effects = Vec::new();
        while self.warm_running + self.grade_running.len() < WARM_AT_ONCE {
            let Some(id) = self.grade_queue.pop_front() else {
                break;
            };
            let key = ResourceKey::Grades(id.clone());
            // Already being read, for the course's own Grades tab.
            if matches!(self.sync_of(&key), Some(SyncState::Syncing { .. })) {
                continue;
            }
            if let Some(effect) = self.request(key, false) {
                self.grade_running.push(id);
                effects.push(effect);
            }
        }
        effects
    }

    /// `E`: arrange the widgets, starting at the one with focus.
    pub(super) fn start_editing(&mut self) {
        let before = self.focus();
        let start = before
            .widget()
            .and_then(|w| self.board.find(w))
            .or_else(|| {
                let first = self.board.tiles().next()?;
                self.board.find(first.widget)
            })
            .unwrap_or((Side::Left, 0));
        self.home_edit = Some(HomeEdit {
            side: start.0,
            index: start.1,
            holding: false,
            picker: None,
            before,
        });
    }

    /// Widgets that are not on the board, in the picker's order.
    pub fn addable(&self) -> Vec<Widget> {
        Widget::ALL
            .into_iter()
            .filter(|w| self.board.find(*w).is_none())
            .collect()
    }

    fn save_board(&self) {
        if let Some(store) = &self.store {
            store.save_home_board(&self.board);
        }
    }

    /// A key while arranging. Every change is saved as it is made, so there
    /// is nothing to lose by leaving.
    pub(super) fn edit_act(&mut self, act: Act) -> Vec<Effect> {
        let Some(mut edit) = self.home_edit.clone() else {
            return vec![];
        };
        let mut effects = Vec::new();
        if let Some(at) = edit.picker {
            let choices = self.addable();
            match act {
                Act::Up => edit.picker = Some(at.saturating_sub(1)),
                Act::Down => edit.picker = Some((at + 1).min(choices.len().saturating_sub(1))),
                Act::Back => edit.picker = None,
                Act::Open => {
                    edit.picker = None;
                    if let Some(&widget) = choices.get(at) {
                        let tile = Tile {
                            widget,
                            size: Size::Medium,
                        };
                        let place = if self.board.side(edit.side).is_empty() {
                            0
                        } else {
                            edit.index + 1
                        };
                        if self.board.add(tile, edit.side, place) {
                            edit.index = place.min(self.board.side(edit.side).len() - 1);
                            self.save_board();
                            self.notice = Some(format!(
                                "Added {}. + and - change its size.",
                                widget.name()
                            ));
                            effects = self.read_tile(tile);
                        }
                    }
                }
                _ => {}
            }
            self.home_edit = Some(edit);
            return effects;
        }
        let selected = edit.selected(&self.board);
        match act {
            Act::Up | Act::Down => {
                let up = act == Act::Up;
                match selected {
                    Some(widget) if edit.holding => {
                        if self.board.shift(widget, up) {
                            edit.index = if up { edit.index - 1 } else { edit.index + 1 };
                            self.save_board();
                        }
                    }
                    _ => {
                        let last = self.board.side(edit.side).len().saturating_sub(1);
                        edit.index = if up {
                            edit.index.saturating_sub(1)
                        } else {
                            (edit.index + 1).min(last)
                        };
                    }
                }
            }
            Act::Left | Act::Right => {
                let to = if act == Act::Left {
                    Side::Left
                } else {
                    Side::Right
                };
                if to != edit.side {
                    match selected {
                        Some(widget) if edit.holding => {
                            if let Some((side, index)) = self.board.cross(widget) {
                                edit.side = side;
                                edit.index = index;
                                self.save_board();
                            }
                        }
                        _ => {
                            edit.side = to;
                            let last = self.board.side(to).len().saturating_sub(1);
                            edit.index = edit.index.min(last);
                        }
                    }
                }
            }
            Act::Open => {
                if selected.is_some() {
                    edit.holding = !edit.holding;
                }
            }
            Act::Bigger | Act::Smaller => {
                if let Some(widget) = selected {
                    let bigger = act == Act::Bigger;
                    match self.board.resize(widget, bigger) {
                        Some(size) => {
                            self.save_board();
                            self.notice = Some(format!(
                                "{}, {}: {}.",
                                widget.name(),
                                size.label(),
                                widget.shows(size)
                            ));
                            effects = self.read_tile(Tile { widget, size });
                        }
                        None => {
                            self.notice = Some(format!(
                                "{} is as {} as it gets.",
                                widget.name(),
                                if bigger { "big" } else { "small" }
                            ))
                        }
                    }
                }
            }
            Act::AddWidget => {
                if self.addable().is_empty() {
                    self.notice = Some("Every widget is on the home screen already.".into());
                } else {
                    edit.holding = false;
                    edit.picker = Some(0);
                }
            }
            Act::RemoveWidget => {
                if let Some(widget) = selected {
                    self.board.remove(widget);
                    edit.holding = false;
                    let last = self.board.side(edit.side).len().saturating_sub(1);
                    edit.index = edit.index.min(last);
                    self.save_board();
                    self.notice = Some(format!("Took {} off. a adds it back.", widget.name()));
                }
            }
            Act::Back if edit.holding => edit.holding = false,
            Act::Back => {
                self.home_edit = None;
                // Back to what had focus, if it is still on screen.
                let before = edit.before;
                let focus = if self.sections().contains(&before) {
                    before
                } else {
                    Section::Text
                };
                self.view_mut().focus = focus;
                self.widget_row = 0;
                return vec![];
            }
            Act::Help => self.help = Some(0),
            _ => {}
        }
        self.home_edit = Some(edit);
        effects
    }
}
