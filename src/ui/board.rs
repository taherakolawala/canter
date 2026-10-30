//! The home screen as drawn: the course list, the rule under it, and the
//! widgets on the board, each at its size. Where each one goes is
//! `crate::home::arrange`'s work; the app works it out the same way, since
//! `Tab` must stop only on what is drawn.

use chrono::{DateTime, Datelike, Duration, Local, NaiveDate, Utc};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Padding, Paragraph, Wrap};
use unicode_width::UnicodeWidthStr;

use super::{age, centered, course_style, draw_courses, pad, relative, truncate};
use crate::app::{App, HomeEdit, HomeEntry, Section, SyncState, WidgetSync, human_size};
use crate::calendar::{self, Item, Kind, Progress};
use crate::home::{self, Arranged, Side, Size, Spot, Widget};
use crate::html;
use crate::model::{Course, Tone, fmt_num};
use crate::theme;

/// The home screen: the course list across the top, the widgets under it.
pub(super) fn draw_home(frame: &mut Frame, area: Rect, app: &mut App, now: DateTime<Utc>) {
    let editing = app.home_edit.is_some();
    let (top, rule, board) = home::split(area, app.course_rows(), &app.board, editing);
    draw_courses(frame, top, app);
    let arranged = home::arrange(&app.board, board);
    draw_rule(frame, rule, app, &arranged);
    for spot in &arranged.spots {
        match spot.widget {
            Widget::Calendar => calendar_widget(frame, *spot, app, now),
            Widget::Inbox => inbox_widget(frame, *spot, app, now),
            Widget::Todo => todo_widget(frame, *spot, app, now),
            Widget::Announcements => announcements_widget(frame, *spot, app, now),
            Widget::Grades => grades_widget(frame, *spot, app, now),
            Widget::RecentFiles => files_widget(frame, *spot, app, now),
        }
    }
    if let Some(edit) = &app.home_edit {
        draw_empty_sides(frame, board, app, edit);
        if let Some(at) = edit.picker {
            draw_picker(frame, area, app, at);
        }
    }
}

/// The line under the course list. While arranging it says so, and it names
/// any widget with no room on this screen, so none goes missing unexplained.
fn draw_rule(frame: &mut Frame, area: Rect, app: &App, arranged: &Arranged) {
    if area.height == 0 {
        return;
    }
    let editing = app.home_edit.is_some();
    let left = if editing {
        vec![
            Span::styled("── ", theme::faint()),
            Span::styled("Arranging the home screen", theme::heading()),
            Span::raw(" "),
        ]
    } else {
        vec![]
    };
    let mut right = vec![];
    if !arranged.left_out.is_empty() {
        let names: Vec<&str> = arranged.left_out.iter().map(|w| w.name()).collect();
        let verb = if names.len() == 1 { "doesn't" } else { "don't" };
        let hint = if editing { "" } else { " · E to arrange" };
        right.push(Span::styled(
            format!(" {} {verb} fit{hint} ", names.join(", ")),
            if editing {
                theme::alert()
            } else {
                theme::muted()
            },
        ));
        right.push(Span::styled("──", theme::faint()));
    }
    let line = spread(
        left,
        right,
        usize::from(area.width),
        Span::styled("─", theme::faint()),
    );
    frame.render_widget(Paragraph::new(line), area);
}

fn width_of(spans: &[Span]) -> usize {
    spans.iter().map(|s| s.content.width()).sum()
}

/// Spans cut to `width` columns, the one that crosses the edge shortened
/// with "…".
fn clip(spans: Vec<Span<'static>>, width: usize) -> Vec<Span<'static>> {
    let mut out = Vec::new();
    let mut used = 0;
    for span in spans {
        let w = span.content.width();
        if used + w <= width {
            used += w;
            out.push(span);
            continue;
        }
        if width > used {
            out.push(Span::styled(truncate(&span.content, width - used), span.style));
        }
        break;
    }
    out
}

/// A line `width` wide: `left`, then `right` against the far end, with
/// `fill` between them. The right part is left off rather than leave the
/// left fewer than a few columns.
fn spread(
    left: Vec<Span<'static>>,
    right: Vec<Span<'static>>,
    width: usize,
    fill: Span<'static>,
) -> Line<'static> {
    let right_w = width_of(&right);
    let keep = width_of(&left).min(8);
    let (left, right) = if right_w > 0 && right_w + 1 + keep <= width {
        (clip(left, width - right_w - 1), right)
    } else {
        (clip(left, width), vec![])
    };
    let gap = width.saturating_sub(width_of(&left) + width_of(&right));
    let mut spans = left;
    if gap > 0 {
        spans.push(Span::styled(fill.content.repeat(gap), fill.style));
    }
    spans.extend(right);
    Line::from(spans)
}

fn sp() -> Span<'static> {
    Span::raw(" ")
}

/// How a widget's box is drawn.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Look {
    Plain,
    Focused,
    /// Selected while arranging.
    Selected,
    /// Picked up to move.
    Held,
}

fn look(app: &App, widget: Widget) -> Look {
    match &app.home_edit {
        Some(edit) if edit.selected(&app.board) == Some(widget) => {
            if edit.holding {
                Look::Held
            } else {
                Look::Selected
            }
        }
        Some(_) => Look::Plain,
        None if app.focus().widget() == Some(widget) => Look::Focused,
        None => Look::Plain,
    }
}

/// What has focus in a widget: the calendar's days or a day's entries, or
/// `Text` when the widget does not have focus at all.
fn focus_in(app: &App, widget: Widget) -> Section {
    if look(app, widget) == Look::Focused {
        app.focus()
    } else {
        Section::Text
    }
}

/// A widget's box: its name in the top border with `extra` after it, how
/// its reads stand at the other end, and a border that shows focus or,
/// while arranging, the selection. Returns the space inside, less a column
/// each side when `padded`.
fn boxed(
    frame: &mut Frame,
    app: &App,
    spot: Spot,
    sync: &WidgetSync,
    name: &str,
    extra: Vec<Span<'static>>,
    padded: bool,
) -> Rect {
    let look = look(app, spot.widget);
    let (border, kind) = match look {
        Look::Plain => (theme::faint(), BorderType::Plain),
        Look::Focused => (theme::accent(), BorderType::Plain),
        Look::Selected => (theme::accent(), BorderType::Double),
        Look::Held => (theme::key(), BorderType::Thick),
    };
    let area = spot.area;
    frame.render_widget(Clear, area);
    let block = Block::bordered().border_type(kind).border_style(border);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    // Between the corners.
    let room = usize::from(area.width.saturating_sub(2));
    let mut title = vec![
        sp(),
        Span::styled(
            name.to_string(),
            if look == Look::Plain {
                theme::heading()
            } else {
                theme::tab_active()
            },
        ),
    ];
    title.extend(extra);
    if app.home_edit.is_some() {
        title.push(Span::styled(
            format!(" · {}", spot.size.label()),
            theme::muted(),
        ));
        if look == Look::Held {
            title.push(Span::styled(" · moving", theme::key()));
        }
    }
    title.push(sp());
    let title = clip(title, room.saturating_sub(1));
    let used = width_of(&title);
    frame.buffer_mut().set_line(
        area.x + 1,
        area.y,
        &Line::from(title),
        area.width.saturating_sub(2),
    );
    let status = match (&sync.failed, sync.syncing) {
        (_, true) => Some(("syncing…", "…", theme::muted())),
        (Some(_), false) if sync.loaded => Some(("! outdated", "!", theme::error())),
        (Some(_), false) => Some(("! not loaded", "!", theme::error())),
        (None, false) => None,
    };
    if let Some((long, short, style)) = status {
        let fits = |text: &str| used + text.width() + 3 <= room;
        let text = if fits(long) {
            Some(long)
        } else if fits(short) {
            Some(short)
        } else {
            None
        };
        if let Some(text) = text {
            let text = format!(" {text} ");
            let x = area.x + area.width - 1 - text.width() as u16;
            frame.buffer_mut().set_string(x, area.y, text, style);
        }
    }
    if padded && inner.width > 2 {
        Rect {
            x: inner.x + 1,
            width: inner.width - 2,
            ..inner
        }
    } else {
        inner
    }
}

/// What a widget says in place of its rows while it has none to show.
fn unloaded(sync: &WidgetSync) -> Option<Line<'static>> {
    if sync.loaded {
        return None;
    }
    Some(match &sync.failed {
        Some(e) if !sync.syncing => {
            Line::styled(format!("Could not load: {}", e.describe()), theme::error())
        }
        _ => Line::styled("Loading…", theme::muted()),
    })
}

fn note(frame: &mut Frame, area: Rect, line: Line<'static>) {
    frame.render_widget(Paragraph::new(line).wrap(Wrap { trim: true }), area);
}

/// A list widget's lines, each with the entry it belongs to (headings
/// belong to none).
#[derive(Default)]
struct Rows {
    lines: Vec<Line<'static>>,
    owner: Vec<Option<usize>>,
}

impl Rows {
    fn heading(&mut self, line: Line<'static>) {
        self.lines.push(line);
        self.owner.push(None);
    }

    fn entry(&mut self, entry: usize, line: Line<'static>) {
        self.lines.push(line);
        self.owner.push(Some(entry));
    }

    /// The lines that fit, scrolled so all of the selected entry shows, with
    /// its first line highlighted.
    fn draw(self, frame: &mut Frame, area: Rect, selected: Option<usize>) {
        let height = usize::from(area.height);
        let mut start = 0;
        let mut highlight = None;
        if let Some(selected) = selected
            && let Some(first) = self.owner.iter().position(|o| *o == Some(selected))
        {
            let last = self
                .owner
                .iter()
                .rposition(|o| *o == Some(selected))
                .unwrap_or(first);
            start = (last + 1).saturating_sub(height).min(first);
            highlight = Some(first);
        }
        let lines: Vec<Line> = self
            .lines
            .into_iter()
            .enumerate()
            .skip(start)
            .take(height)
            .map(|(i, line)| {
                if Some(i) == highlight {
                    line.style(theme::selected())
                } else {
                    line
                }
            })
            .collect();
        frame.render_widget(Paragraph::new(lines), area);
    }
}

/// When something happened: how long ago, within the week, else its date.
fn ago(at: DateTime<Utc>, now: DateTime<Utc>) -> String {
    if now - at < Duration::days(7) {
        age(at, now)
    } else {
        at.with_timezone(&Local).format("%b %-d").to_string()
    }
}

/// An entry's course, short: its code, else the name the planner gave.
fn item_course(app: &App, item: &Item) -> String {
    let code = item
        .course_id
        .as_deref()
        .map(|c| app.course_code(c))
        .unwrap_or_default();
    let text = if code.is_empty() {
        item.course.clone().unwrap_or_default()
    } else {
        code
    };
    html::sanitize(&text)
}

fn item_style(app: &App, item: &Item) -> Style {
    item.course_id
        .as_deref()
        .map(|c| course_style(app, c))
        .unwrap_or_else(theme::text)
}

fn title_span(app: &App, item: &Item) -> Span<'static> {
    Span::styled(html::sanitize(&item.title), item_style(app, item))
}

/// The course column of a list row.
fn course_cell(app: &App, item: &Item) -> Span<'static> {
    Span::styled(
        pad(&truncate(&item_course(app, item), 9), 10),
        item_style(app, item),
    )
}

/// How long until it is due, coloured by how soon.
fn countdown(item: &Item, now: DateTime<Utc>) -> Span<'static> {
    Span::styled(
        item.at.map(|t| relative(t, now)).unwrap_or_default(),
        theme::due(item.at, false, now),
    )
}

fn points(item: &Item) -> Vec<Span<'static>> {
    item.points
        .filter(|p| *p > 0.0)
        .map(|p| vec![Span::styled(format!("{} pts", fmt_num(p)), theme::faint())])
        .unwrap_or_default()
}

// The calendar.

fn calendar_widget(frame: &mut Frame, spot: Spot, app: &App, now: DateTime<Utc>) {
    let sync = app.widget_sync(Widget::Calendar);
    let today = now.with_timezone(&Local).date_naive();
    let focus = focus_in(app, Widget::Calendar);
    match spot.size {
        // Today: how much is left of it, and what is next.
        Size::Small => {
            let extra = vec![Span::styled(
                today.format(" · %a %-d").to_string(),
                theme::muted(),
            )];
            let inner = boxed(frame, app, spot, &sync, "Calendar", extra, true);
            if let Some(line) = unloaded(&sync) {
                return note(frame, inner, line);
            }
            let width = usize::from(inner.width);
            let left = app
                .planner_between(today, today)
                .iter()
                .filter(|i| i.listed() && !i.progress.finished())
                .count();
            let first = match left {
                0 => Line::styled("Nothing left today", theme::muted()),
                n => Line::styled(format!("{n} left today"), theme::bold()),
            };
            let second = match app.widget_entries(Widget::Calendar, now).first() {
                Some(HomeEntry::Item(item)) => spread(
                    vec![title_span(app, item)],
                    vec![countdown(item, now)],
                    width,
                    sp(),
                ),
                _ => Line::styled("Clear for a week", theme::muted()),
            };
            frame.render_widget(Paragraph::new(vec![first, second]), inner);
        }
        // The week with the selected day, or that day's entries.
        Size::Medium => {
            let day = app.calendar_day;
            let start = day - Duration::days(i64::from(day.weekday().num_days_from_sunday()));
            let end = start + Duration::days(6);
            let range = if start.month() == end.month() {
                format!(" · {} – {}", start.format("%b %-d"), end.format("%-d"))
            } else {
                format!(" · {} – {}", start.format("%b %-d"), end.format("%b %-d"))
            };
            let extra = vec![Span::styled(range, theme::muted())];
            let inner = boxed(frame, app, spot, &sync, "Calendar", extra, false);
            let height = usize::from(inner.height);
            if focus == Section::Day {
                let lines = day_list(app, day, today, now, height, Some(app.day_item));
                frame.render_widget(Paragraph::new(lines), inner);
                return;
            }
            let col = (usize::from(inner.width).saturating_sub(1) / 7).max(4);
            // "Sun 25" where a column has room for it, else the names
            // get a line of their own.
            let named = col > 6;
            let mut lines = Vec::new();
            if !named {
                lines.push(weekday_names(col));
            }
            let days: Vec<NaiveDate> = (0..7).map(|d| start + Duration::days(d)).collect();
            let cursor = (focus == Section::Calendar).then_some(day);
            let rows = height.saturating_sub(lines.len() + 1);
            lines.extend(week_grid(
                app,
                &days,
                &app.calendar_items(),
                rows,
                col,
                |d| {
                    let mut text = if named {
                        d.format("%a %-d").to_string()
                    } else {
                        d.day().to_string()
                    };
                    if d == today && text.width() < col - 1 {
                        text.push('•');
                    }
                    (text, day_style(d, today, cursor, None))
                },
            ));
            frame.render_widget(Paragraph::new(lines), inner);
        }
        _ => {
            let title = app.calendar_day.format("%B %Y").to_string();
            let inner = boxed(frame, app, spot, &sync, &title, vec![], false);
            draw_month(frame, inner, app, now, focus);
        }
    }
}

fn day_style(day: NaiveDate, today: NaiveDate, cursor: Option<NaiveDate>, month: Option<u32>) -> Style {
    if cursor == Some(day) {
        theme::selected()
    } else if month.is_some_and(|m| day.month() != m) {
        theme::faint()
    } else if day == today {
        theme::accent().add_modifier(Modifier::BOLD)
    } else {
        theme::muted()
    }
}

fn grid_rule(i: usize) -> Span<'static> {
    Span::styled(if i == 0 { " " } else { "│" }, theme::faint())
}

fn weekday_names(col: usize) -> Line<'static> {
    let mut spans = Vec::new();
    for (i, name) in ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"]
        .iter()
        .enumerate()
    {
        spans.push(grid_rule(i));
        spans.push(Span::styled(pad(name, col - 1), theme::muted()));
    }
    Line::from(spans)
}

/// One entry in a calendar cell, in its course's colour; missing work is
/// marked, finished work dimmed.
fn entry_cell(app: &App, item: &Item, width: usize) -> Span<'static> {
    let title = html::sanitize(&item.title);
    if item.progress == Progress::Missing {
        return Span::styled(pad(&format!("!{title}"), width), theme::error());
    }
    let mut style = item_style(app, item);
    if item.progress.finished() {
        style = style.add_modifier(Modifier::DIM);
    }
    Span::styled(pad(&title, width), style)
}

/// A week of the grid: a row of day labels, then each day's entries a line
/// each, the last line saying how many more there are.
fn week_grid(
    app: &App,
    days: &[NaiveDate],
    items: &[Item],
    rows: usize,
    col: usize,
    label: impl Fn(NaiveDate) -> (String, Style),
) -> Vec<Line<'static>> {
    let mut spans = Vec::new();
    for (i, day) in days.iter().enumerate() {
        let (text, style) = label(*day);
        spans.push(grid_rule(i));
        spans.push(Span::styled(pad(&text, col - 1), style));
    }
    let mut lines = vec![Line::from(spans)];
    let per_day: Vec<Vec<&Item>> = days
        .iter()
        .map(|d| items.iter().filter(|i| i.day() == Some(*d)).collect())
        .collect();
    for line in 0..rows {
        let mut spans = Vec::new();
        for (i, entries) in per_day.iter().enumerate() {
            spans.push(grid_rule(i));
            let more = entries.len().saturating_sub(rows);
            spans.push(if more > 0 && line == rows - 1 {
                Span::styled(pad(&format!("+{} more", more + 1), col - 1), theme::muted())
            } else if let Some(item) = entries.get(line) {
                entry_cell(app, item, col - 1)
            } else {
                Span::raw(" ".repeat(col - 1))
            });
        }
        lines.push(Line::from(spans));
    }
    lines
}

/// A day's heading and its entries, `height` lines at most, keeping the
/// selected entry in view.
fn day_list(
    app: &App,
    day: NaiveDate,
    today: NaiveDate,
    now: DateTime<Utc>,
    height: usize,
    selected: Option<usize>,
) -> Vec<Line<'static>> {
    let mut lines = vec![Line::styled(
        format!(
            " {}",
            calendar::day_label(day, today) + &day.format(" · %A, %B %-d").to_string()
        ),
        theme::accent(),
    )];
    let items = app.day_items(day);
    if items.is_empty() {
        lines.push(Line::styled("   Nothing on this day.", theme::muted()));
        return lines;
    }
    let room = height.saturating_sub(1).max(1);
    let start = selected.map_or(0, |s| (s + 1).saturating_sub(room));
    for (i, item) in items.iter().enumerate().skip(start).take(room) {
        let time = item
            .at
            .map(|t| t.with_timezone(&Local).format("%-I:%M %p").to_string())
            .unwrap_or_default();
        let mut tags = Vec::new();
        if item.kind != Kind::Assignment {
            tags.push(item.kind.label());
        }
        if item.progress != Progress::Open {
            tags.push(item.progress.label());
        }
        let mut line = Line::from(vec![
            Span::styled(
                format!("   {time:>8}  "),
                theme::due(item.at, item.progress.finished(), now),
            ),
            Span::styled(
                format!("{}  ", truncate(&item_course(app, item), 14)),
                item_style(app, item),
            ),
            Span::raw(html::sanitize(&item.title)),
            Span::styled(
                if tags.is_empty() {
                    String::new()
                } else {
                    format!("  {}", tags.join(" · "))
                },
                if item.progress == Progress::Missing {
                    theme::error()
                } else {
                    theme::muted()
                },
            ),
        ]);
        if selected == Some(i) {
            line = line.style(theme::selected());
        }
        lines.push(line);
    }
    lines
}

/// The month grid, like Canvas's calendar: each day lists what is due or
/// happening, in the course's colour. With focus it has a day cursor, and
/// the day's entries are listed under it.
fn draw_month(frame: &mut Frame, area: Rect, app: &App, now: DateTime<Utc>, focus: Section) {
    let focused = matches!(focus, Section::Calendar | Section::Day);
    let day = app.calendar_day;
    let today = now.with_timezone(&Local).date_naive();
    let (start, weeks) = calendar::grid(day);
    let items = app.calendar_items();
    let col = (usize::from(area.width).saturating_sub(1) / 7).max(4);
    let strip = if focused {
        app.day_items(day).len().clamp(1, 4) + 1
    } else {
        0
    };
    let mut lines = vec![weekday_names(col)];
    // Each week: a rule line, the day numbers, then entries.
    let room = usize::from(area.height).saturating_sub(1 + strip);
    let per_week = (room.saturating_sub(weeks) / weeks).max(1);
    let week_rule = {
        let mut text = String::from("─");
        for i in 0..7 {
            if i > 0 {
                text.push('┼');
            }
            text.push_str(&"─".repeat(col - 1));
        }
        Line::styled(text, theme::faint())
    };
    let cursor = focused.then_some(day);
    for week in 0..weeks {
        lines.push(week_rule.clone());
        let days: Vec<NaiveDate> = (0..7)
            .map(|d| start + Duration::days((week * 7 + d) as i64))
            .collect();
        lines.extend(week_grid(app, &days, &items, per_week - 1, col, |d| {
            let text = if d == today {
                format!("{}•", d.day())
            } else {
                d.day().to_string()
            };
            (text, day_style(d, today, cursor, Some(day.month())))
        }));
    }
    if focused {
        // The day's list gets what the weeks leave, which may be more than
        // the lines kept for it.
        lines.truncate(usize::from(area.height).saturating_sub(strip));
        let rest = usize::from(area.height).saturating_sub(lines.len());
        let selected = (focus == Section::Day).then_some(app.day_item);
        lines.extend(day_list(app, day, today, now, rest, selected));
    }
    frame.render_widget(Paragraph::new(lines), area);
}

// The Inbox.

fn inbox_widget(frame: &mut Frame, spot: Spot, app: &App, now: DateTime<Utc>) {
    let sync = app.widget_sync(Widget::Inbox);
    let unread = app.unread_conversations();
    // A count of nothing and a count not yet read look the same, so say
    // which it is until the first read is in.
    let count = match app.unread_resource() {
        Some(r) if r.data.is_some() => match unread {
            0 => "all read".to_string(),
            1 => "1 unread".to_string(),
            n => format!("{n} unread"),
        },
        Some(r) if matches!(r.sync, SyncState::Failed(_)) => "not loaded".to_string(),
        _ => "…".to_string(),
    };
    let count_style = if unread > 0 {
        theme::bold()
    } else {
        theme::muted()
    };
    if spot.size == Size::Small {
        // The count, and who is in the newest unread conversation. Not the
        // conversations themselves: that is what the bigger sizes are for.
        let inner = boxed(frame, app, spot, &sync, "\u{2709} Inbox", vec![], true);
        let mut lines = vec![Line::styled(count, count_style)];
        if let Some(c) = app
            .unread_resource()
            .and_then(|r| r.data.as_deref())
            .unwrap_or(&[])
            .iter()
            .find(|c| app.conversation_status(c) == "Unread")
        {
            lines.push(Line::styled(
                truncate(&html::sanitize(&c.who()), usize::from(inner.width)),
                theme::muted(),
            ));
        }
        frame.render_widget(Paragraph::new(lines), inner);
        return;
    }
    let extra = if unread > 0 {
        vec![Span::styled(format!(" · {unread} unread"), theme::accent())]
    } else {
        vec![]
    };
    let inner = boxed(frame, app, spot, &sync, "\u{2709} Inbox", extra, true);
    let width = usize::from(inner.width);
    let large = spot.size.large();
    let entries = app.widget_entries(Widget::Inbox, now);
    let who_width = (width / 4).clamp(8, 22);
    let mut rows = Rows::default();
    for (i, entry) in entries.iter().enumerate() {
        match entry {
            HomeEntry::Inbox => rows.entry(
                i,
                spread(
                    vec![Span::styled(count.clone(), count_style)],
                    vec![Span::styled("all conversations", theme::faint())],
                    width,
                    sp(),
                ),
            ),
            HomeEntry::Conversation(c) => {
                let fresh = app.conversation_status(c) == "Unread";
                rows.entry(
                    i,
                    spread(
                        vec![
                            Span::styled(if fresh { "• " } else { "  " }, theme::accent()),
                            Span::styled(
                                pad(&html::sanitize(&c.who()), who_width) + "  ",
                                if fresh { theme::bold() } else { theme::muted() },
                            ),
                            Span::styled(
                                html::sanitize(c.subject()),
                                if fresh { theme::bold() } else { theme::text() },
                            ),
                        ],
                        vec![Span::styled(
                            c.date().map(|d| ago(d, now)).unwrap_or_default(),
                            theme::faint(),
                        )],
                        width,
                        sp(),
                    ),
                );
                if large {
                    let last = c
                        .last_message
                        .as_deref()
                        .map(html::sanitize)
                        .unwrap_or_default();
                    let place = c
                        .context_name
                        .as_deref()
                        .map(html::sanitize)
                        .unwrap_or_default();
                    rows.entry(
                        i,
                        spread(
                            vec![Span::raw("  "), Span::styled(last, theme::muted())],
                            vec![Span::styled(place, theme::faint())],
                            width,
                            sp(),
                        ),
                    );
                }
            }
            _ => {}
        }
    }
    if entries.len() <= 1 {
        rows.heading(
            unloaded(&sync).unwrap_or_else(|| Line::styled("No conversations.", theme::muted())),
        );
    }
    rows.draw(
        frame,
        inner,
        app.widget_selection(Widget::Inbox, entries.len()),
    );
}

// To do.

fn todo_widget(frame: &mut Frame, spot: Spot, app: &App, now: DateTime<Utc>) {
    let sync = app.widget_sync(Widget::Todo);
    let missing = app.missing_work();
    let upcoming = app.upcoming_work(now);
    let small = spot.size == Size::Small;
    let extra = if missing.is_empty() || small {
        vec![]
    } else {
        vec![Span::styled(
            format!(" · {} missing", missing.len()),
            theme::error(),
        )]
    };
    let inner = boxed(frame, app, spot, &sync, "To do", extra, true);
    if let Some(line) = unloaded(&sync) {
        return note(frame, inner, line);
    }
    let width = usize::from(inner.width);
    if small {
        let first = match (upcoming.first(), missing.first()) {
            (Some(item), _) => spread(
                vec![title_span(app, item)],
                vec![countdown(item, now)],
                width,
                sp(),
            ),
            (None, Some(item)) => spread(
                vec![title_span(app, item)],
                vec![Span::styled("missing", theme::error())],
                width,
                sp(),
            ),
            (None, None) => Line::styled("All caught up", theme::tone(Tone::Good)),
        };
        let second = match (missing.len(), upcoming.len()) {
            (0, 0) => Line::styled("Nothing due soon", theme::muted()),
            (0, 1) => Line::styled("Nothing else due", theme::muted()),
            (0, n) => Line::styled(format!("{} more in 7 days", n - 1), theme::muted()),
            (n, _) => Line::styled(format!("! {n} missing"), theme::error()),
        };
        frame.render_widget(Paragraph::new(vec![first, second]), inner);
        return;
    }
    let entries = app.widget_entries(Widget::Todo, now);
    if entries.is_empty() {
        return note(
            frame,
            inner,
            Line::styled(
                "All caught up: nothing missing, nothing due in the next 7 days.",
                theme::muted(),
            ),
        );
    }
    let mut rows = Rows::default();
    if spot.size == Size::Medium {
        for (i, entry) in entries.iter().enumerate() {
            let HomeEntry::Item(item) = entry else {
                continue;
            };
            let (when, style) = if item.progress == Progress::Missing {
                (
                    "missing".to_string(),
                    theme::error().add_modifier(Modifier::BOLD),
                )
            } else {
                let c = countdown(item, now);
                (c.content.to_string(), c.style)
            };
            rows.entry(
                i,
                spread(
                    vec![
                        Span::styled(pad(&when, 10), style),
                        course_cell(app, item),
                        Span::raw(html::sanitize(&item.title)),
                    ],
                    points(item),
                    width,
                    sp(),
                ),
            );
        }
    } else {
        // A line of totals, the missing work, then the week a day at a time.
        let today = now.with_timezone(&Local).date_naive();
        let total: f64 = upcoming.iter().filter_map(|i| i.points).sum();
        let mut summary = Vec::new();
        if !missing.is_empty() {
            summary.push(Span::styled(
                format!("{} missing", missing.len()),
                theme::error(),
            ));
            summary.push(Span::styled(" · ", theme::faint()));
        }
        summary.push(Span::styled(
            format!("{} due in 7 days", upcoming.len()),
            theme::bold(),
        ));
        if total > 0.0 {
            summary.push(Span::styled(" · ", theme::faint()));
            summary.push(Span::styled(
                format!("{} points", fmt_num(total)),
                theme::muted(),
            ));
        }
        rows.heading(Line::from(summary));
        let mut day = None;
        for (i, entry) in entries.iter().enumerate() {
            let HomeEntry::Item(item) = entry else {
                continue;
            };
            if item.progress == Progress::Missing {
                if i == 0 {
                    rows.heading(Line::styled(
                        "Missing",
                        theme::error().add_modifier(Modifier::BOLD),
                    ));
                }
                let mut right = points(item);
                if let Some(at) = item.at {
                    right.push(Span::styled(
                        format!("  due {}", relative(at, now)),
                        theme::faint(),
                    ));
                }
                rows.entry(
                    i,
                    spread(
                        vec![
                            Span::styled("  ! ", theme::error()),
                            course_cell(app, item),
                            Span::raw(html::sanitize(&item.title)),
                        ],
                        right,
                        width,
                        sp(),
                    ),
                );
                continue;
            }
            if item.day() != day {
                day = item.day();
                rows.heading(Line::styled(
                    day.map(|d| calendar::day_label(d, today))
                        .unwrap_or_default(),
                    theme::accent(),
                ));
            }
            let time = item
                .at
                .map(|t| t.with_timezone(&Local).format("%-I:%M %p").to_string())
                .unwrap_or_default();
            rows.entry(
                i,
                spread(
                    vec![
                        Span::styled(format!("  {time:>8}  "), theme::due(item.at, false, now)),
                        course_cell(app, item),
                        Span::raw(html::sanitize(&item.title)),
                    ],
                    points(item),
                    width,
                    sp(),
                ),
            );
        }
    }
    rows.draw(
        frame,
        inner,
        app.widget_selection(Widget::Todo, entries.len()),
    );
}

// Announcements.

fn announcements_widget(frame: &mut Frame, spot: Spot, app: &App, now: DateTime<Utc>) {
    let sync = app.widget_sync(Widget::Announcements);
    let today = now.with_timezone(&Local).date_naive();
    let all = app.recent_announcements(today);
    let unread = all.iter().filter(|i| app.announcement_unread(i)).count();
    let small = spot.size == Size::Small;
    let extra = if unread > 0 && !small {
        vec![Span::styled(format!(" · {unread} unread"), theme::accent())]
    } else {
        vec![]
    };
    let inner = boxed(frame, app, spot, &sync, "Announcements", extra, true);
    if let Some(line) = unloaded(&sync) {
        return note(frame, inner, line);
    }
    let width = usize::from(inner.width);
    let entries = app.widget_entries(Widget::Announcements, now);
    if entries.is_empty() {
        return note(
            frame,
            inner,
            Line::styled("None in the last two weeks.", theme::muted()),
        );
    }
    let when = |item: &Item| {
        Span::styled(
            item.at.map(|t| ago(t, now)).unwrap_or_default(),
            theme::faint(),
        )
    };
    if small {
        // How many are unread, and which course posted the one `Enter` opens.
        let first = match unread {
            0 => Line::styled("all read", theme::muted()),
            n => Line::styled(format!("{n} unread"), theme::bold()),
        };
        let second = match &entries[0] {
            HomeEntry::Item(item) => spread(
                vec![Span::styled(item_course(app, item), item_style(app, item))],
                vec![when(item)],
                width,
                sp(),
            ),
            _ => Line::default(),
        };
        frame.render_widget(Paragraph::new(vec![first, second]), inner);
        return;
    }
    let mut rows = Rows::default();
    if spot.size.large() {
        rows.heading(unread_by_course(app, &all));
    }
    for (i, entry) in entries.iter().enumerate() {
        let HomeEntry::Item(item) = entry else {
            continue;
        };
        let fresh = app.announcement_unread(item);
        rows.entry(
            i,
            spread(
                vec![
                    Span::styled(if fresh { "• " } else { "  " }, theme::accent()),
                    course_cell(app, item),
                    Span::styled(
                        html::sanitize(&item.title),
                        if fresh { theme::bold() } else { theme::text() },
                    ),
                ],
                vec![when(item)],
                width,
                sp(),
            ),
        );
    }
    rows.draw(
        frame,
        inner,
        app.widget_selection(Widget::Announcements, entries.len()),
    );
}

/// "3 unread: EEL5934 2 · CAP3027 1", or that everything is read.
fn unread_by_course(app: &App, items: &[Item]) -> Line<'static> {
    let mut counts: Vec<(String, usize, Style)> = Vec::new();
    for item in items.iter().filter(|i| app.announcement_unread(i)) {
        let course = item_course(app, item);
        match counts.iter_mut().find(|(c, _, _)| *c == course) {
            Some(count) => count.1 += 1,
            None => counts.push((course, 1, item_style(app, item))),
        }
    }
    if counts.is_empty() {
        return Line::styled("All read.", theme::muted());
    }
    let total: usize = counts.iter().map(|c| c.1).sum();
    let mut spans = vec![Span::styled(format!("{total} unread: "), theme::bold())];
    for (i, (course, n, style)) in counts.into_iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" · ", theme::faint()));
        }
        spans.push(Span::styled(course, style));
        spans.push(Span::styled(format!(" {n}"), theme::muted()));
    }
    Line::from(spans)
}

// Grades.

/// A score as Canvas gave it: out of the points where there are some.
fn score_text(score: Option<f64>, grade: Option<&str>, points: Option<f64>) -> String {
    match (score, points) {
        (Some(s), Some(p)) if p > 0.0 => format!("{}/{}", fmt_num(s), fmt_num(p)),
        (Some(s), _) => fmt_num(s),
        _ => grade.map(html::sanitize).unwrap_or_default(),
    }
}

/// A course's total, or why there is none to show.
fn total_text(app: &App, course_id: &str) -> String {
    if let Some(total) = app.course_grade(course_id) {
        return total;
    }
    match app.grades.get(course_id) {
        Some(r) if r.data.is_some() => "no total".into(),
        Some(r) if matches!(r.sync, SyncState::Failed(_)) => "unavailable".into(),
        _ => "…".into(),
    }
}

/// A course's line in the Grades widget: its code, its name, its total, and
/// `*` while it has feedback not yet seen.
fn course_row(app: &App, course: &Course, width: usize) -> Line<'static> {
    let style = course_style(app, &course.id);
    let star = if app.course_has_new_feedback(&course.id) {
        " *"
    } else {
        "  "
    };
    let right = vec![
        Span::styled(total_text(app, &course.id), theme::bold()),
        Span::styled(star, theme::accent()),
    ];
    let left = vec![
        Span::styled(pad(&truncate(&app.course_code(&course.id), 9), 10), style),
        Span::styled(html::sanitize(course.title()), theme::muted()),
    ];
    spread(left, right, width, sp())
}

fn grades_widget(frame: &mut Frame, spot: Spot, app: &App, now: DateTime<Utc>) {
    let sync = app.widget_sync(Widget::Grades);
    let courses = app.home_courses();
    let small = spot.size == Size::Small;
    let feedback = courses.iter().any(|c| app.course_has_new_feedback(&c.id));
    let extra = if feedback && !small {
        vec![Span::styled(" · * new feedback", theme::accent())]
    } else {
        vec![]
    };
    let inner = boxed(frame, app, spot, &sync, "Grades", extra, true);
    if let Some(line) = unloaded(&sync) {
        return note(frame, inner, line);
    }
    let width = usize::from(inner.width);
    let entries = app.widget_entries(Widget::Grades, now);
    if entries.is_empty() {
        return note(frame, inner, Line::styled("No courses.", theme::muted()));
    }
    if small {
        // The newest score, then where that leaves its course.
        let lines = match &entries[0] {
            HomeEntry::Graded(g) => vec![
                spread(
                    vec![Span::raw(html::sanitize(&g.title))],
                    vec![Span::styled(
                        score_text(g.score, g.grade.as_deref(), g.points),
                        theme::bold(),
                    )],
                    width,
                    sp(),
                ),
                spread(
                    vec![Span::styled(
                        app.course_code(&g.course_id),
                        course_style(app, &g.course_id),
                    )],
                    vec![Span::styled(total_text(app, &g.course_id), theme::muted())],
                    width,
                    sp(),
                ),
            ],
            HomeEntry::Course(id) => vec![
                spread(
                    vec![Span::styled(app.course_code(id), course_style(app, id))],
                    vec![Span::styled(total_text(app, id), theme::bold())],
                    width,
                    sp(),
                ),
                Line::styled("No scores yet", theme::muted()),
            ],
            _ => vec![],
        };
        frame.render_widget(Paragraph::new(lines), inner);
        return;
    }
    let large = spot.size.large();
    let mut rows = Rows::default();
    let mut scores = 0;
    for (i, entry) in entries.iter().enumerate() {
        match entry {
            HomeEntry::Course(id) => {
                if let Some(course) = courses.iter().find(|c| c.id == *id) {
                    rows.entry(i, course_row(app, course, width));
                }
            }
            HomeEntry::Graded(g) => {
                if scores == 0 {
                    rows.heading(Line::styled("Recently graded", theme::accent()));
                }
                scores += 1;
                rows.entry(
                    i,
                    spread(
                        vec![
                            Span::styled(
                                pad(&truncate(&app.course_code(&g.course_id), 9), 10),
                                course_style(app, &g.course_id),
                            ),
                            Span::raw(html::sanitize(&g.title)),
                        ],
                        vec![
                            Span::styled(
                                score_text(g.score, g.grade.as_deref(), g.points),
                                theme::bold(),
                            ),
                            Span::styled(format!("  {}", ago(g.at, now)), theme::faint()),
                        ],
                        width,
                        sp(),
                    ),
                );
            }
            _ => {}
        }
    }
    if large && scores == 0 {
        rows.heading(Line::styled("Recently graded", theme::accent()));
        rows.heading(Line::styled("Nothing graded yet.", theme::muted()));
    }
    rows.draw(
        frame,
        inner,
        app.widget_selection(Widget::Grades, entries.len()),
    );
}

// Recent files.

/// "opened 2 h ago" or "downloaded 3 days ago": whichever happened last.
fn file_when(file: &crate::library::Recent, now: DateTime<Utc>) -> String {
    let verb = if file.opened_last() {
        "opened"
    } else {
        "downloaded"
    };
    match file.when() {
        Some(at) => format!("{verb} {}", ago(at, now)),
        None => verb.to_string(),
    }
}

/// The course a file was saved under: its code in its colour, found by the
/// folder canter names after it, else that folder's name.
fn file_course(app: &App, file: &crate::library::Recent) -> Span<'static> {
    let Some(folder) = file.folders.first() else {
        return Span::raw("");
    };
    let course = app.courses.data.as_deref().unwrap_or(&[]).iter().find(|c| {
        crate::library::course_folder(c.course_code.as_deref(), c.title()) == *folder
    });
    match course {
        Some(c) => Span::styled(app.course_code(&c.id), course_style(app, &c.id)),
        None => Span::styled(html::sanitize(folder), theme::faint()),
    }
}

fn files_widget(frame: &mut Frame, spot: Spot, app: &App, now: DateTime<Utc>) {
    let sync = app.widget_sync(Widget::RecentFiles);
    let inner = boxed(frame, app, spot, &sync, "Recent files", vec![], true);
    let width = usize::from(inner.width);
    let entries = app.widget_entries(Widget::RecentFiles, now);
    if entries.is_empty() {
        return note(
            frame,
            inner,
            Line::styled("Nothing downloaded yet.", theme::muted()),
        );
    }
    if spot.size == Size::Small {
        let HomeEntry::File(file) = &entries[0] else {
            return;
        };
        let lines = vec![
            Line::raw(truncate(&html::sanitize(&file.name()), width)),
            Line::styled(truncate(&file_when(file, now), width), theme::muted()),
        ];
        frame.render_widget(Paragraph::new(lines), inner);
        return;
    }
    let large = spot.size.large();
    let mut rows = Rows::default();
    for (i, entry) in entries.iter().enumerate() {
        let HomeEntry::File(file) = entry else {
            continue;
        };
        let mut left = vec![
            Span::raw(html::sanitize(&file.name())),
            sp(),
            sp(),
            file_course(app, file),
        ];
        // On a Large one, the folder it is in, where that is not the
        // course's own.
        if large
            && file.folders.len() > 1
            && let Some(folder) = file.folders.last()
        {
            left.push(Span::styled(
                format!("  {}", html::sanitize(folder)),
                theme::faint(),
            ));
        }
        let mut right = Vec::new();
        if large && let Some(size) = file.size {
            right.push(Span::styled(
                format!("{:>8}  ", human_size(size)),
                theme::muted(),
            ));
        }
        right.push(Span::styled(file_when(file, now), theme::muted()));
        rows.entry(i, spread(left, right, width, sp()));
    }
    rows.draw(
        frame,
        inner,
        app.widget_selection(Widget::RecentFiles, entries.len()),
    );
}

// Arranging.

/// While arranging, an empty side shows where a widget added there goes.
fn draw_empty_sides(frame: &mut Frame, board: Rect, app: &App, edit: &HomeEdit) {
    let (left, right) = home::sides(board);
    for (side, area) in [(Side::Left, left), (Side::Right, right)] {
        if !app.board.side(side).is_empty() || area.height < 3 || area.width < 6 {
            continue;
        }
        let selected = edit.side == side;
        let rect = Rect {
            height: area.height.min(4),
            ..area
        };
        let block = Block::bordered()
            .border_type(if selected {
                BorderType::Double
            } else {
                BorderType::Plain
            })
            .border_style(if selected {
                theme::accent()
            } else {
                theme::faint()
            });
        let inner = block.inner(rect);
        frame.render_widget(block, rect);
        let text = Line::from(vec![
            Span::styled("Empty. ", theme::muted()),
            Span::styled("a", theme::key()),
            Span::styled(" adds a widget here.", theme::muted()),
        ]);
        frame.render_widget(
            Paragraph::new(text).wrap(Wrap { trim: true }),
            Rect {
                x: inner.x + 1,
                width: inner.width.saturating_sub(2),
                ..inner
            },
        );
    }
}

/// The widgets not on the board yet, each with what it shows and reads.
fn draw_picker(frame: &mut Frame, area: Rect, app: &App, selected: usize) {
    let choices = app.addable();
    let width = area.width.saturating_sub(4).min(64);
    // Inside the border and the padding.
    let text = usize::from(width.saturating_sub(4));
    let mut lines = vec![
        Line::styled(
            "It goes under the selected widget, at medium size.",
            theme::muted(),
        ),
        Line::raw(""),
    ];
    let mut shown = (0, 0);
    for (i, widget) in choices.iter().enumerate() {
        let first = lines.len();
        let mut name = Line::styled(pad(widget.name(), text), theme::bold());
        if i == selected {
            name = name.style(theme::selected());
        }
        lines.push(name);
        for line in crate::submission::wrap(widget.about(), text.saturating_sub(2)) {
            lines.push(Line::styled(format!("  {line}"), theme::muted()));
        }
        if i == selected {
            shown = (first, lines.len() - 1);
        }
    }
    let height = (lines.len() as u16 + 2).min(area.height);
    let rect = centered(area, width, height);
    let inner = usize::from(rect.height.saturating_sub(2));
    let skip = (shown.1 + 1).saturating_sub(inner).min(shown.0);
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(lines.into_iter().skip(skip).collect::<Vec<_>>()).block(
            Block::bordered()
                .title(" Add a widget ")
                .border_style(theme::accent())
                .padding(Padding::horizontal(1)),
        ),
        rect,
    );
}
