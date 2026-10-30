//! The home screen's widgets: which ones are on it, how big each one is, and
//! where each one goes.
//!
//! The course list keeps the top of the screen. The board under it has two
//! sides, left and right, and each side stacks its widgets top to bottom in
//! the order they were put there. Sizes are measured against the side, not a
//! grid:
//!
//! - Small is the box the Inbox count has always had, 22 by 4. Smalls next to
//!   each other on a side share a row while they fit.
//! - Medium is the whole width of its side, as tall as that widget needs.
//! - Large is the whole width too, and half the side's height or all of it.
//!   A whole one takes what the others leave; a half one stays half even with
//!   nothing under it, so it does not grow when a neighbour is taken away.
//!
//! Each size up shows more of the same thing. A widget that does not fit is
//! left out rather than squeezed, and the edit screen says which. Nothing here
//! draws or reads anything: the drawing code and the app work out the same
//! arrangement, the app because `Tab` must only stop on what is on screen.

use ratatui::layout::Rect;
use serde::{Deserialize, Serialize};

/// Something the home screen can show.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Widget {
    Calendar,
    Inbox,
    Todo,
    Announcements,
    Grades,
    RecentFiles,
}

impl Widget {
    /// In the order the picker offers them.
    pub const ALL: [Widget; 6] = [
        Widget::Calendar,
        Widget::Todo,
        Widget::Inbox,
        Widget::Announcements,
        Widget::Grades,
        Widget::RecentFiles,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Widget::Calendar => "Calendar",
            Widget::Inbox => "Inbox",
            Widget::Todo => "To do",
            Widget::Announcements => "Announcements",
            Widget::Grades => "Grades",
            Widget::RecentFiles => "Recent files",
        }
    }

    /// What it shows and what it asks Canvas for, for the picker. Every
    /// widget's reads are listed in `docs/request-policy.md`.
    pub fn about(self) -> &'static str {
        match self {
            Widget::Calendar => "What is due today, this week or this month. Reads the planner.",
            Widget::Inbox => "Unread messages, then the newest conversations. Reads the Inbox.",
            Widget::Todo => {
                "Missing work, then what is due in the next 7 days. Reads the planner and your missing work."
            }
            Widget::Announcements => {
                "Posts from the last two weeks, unread first. Reads the planner."
            }
            Widget::Grades => {
                "Every course's grade and the newest scores. Reads each course's grades in the background."
            }
            Widget::RecentFiles => {
                "Files you downloaded or opened here. Reads nothing from Canvas."
            }
        }
    }

    /// What the widget shows at a size, for the edit screen.
    pub fn shows(self, size: Size) -> &'static str {
        match (self, size) {
            (Widget::Calendar, Size::Small) => "today",
            (Widget::Calendar, Size::Medium) => "this week",
            (Widget::Calendar, _) => "the month",
            (Widget::Inbox, Size::Small) => "the unread count",
            (Widget::Inbox, Size::Medium) => "the newest three",
            (Widget::Inbox, _) => "the newest, with their last message",
            (Widget::Todo, Size::Small) => "what is due next",
            (Widget::Todo, Size::Medium) => "missing, then the next few",
            (Widget::Todo, _) => "missing, then the week by day",
            (Widget::Announcements, Size::Small) => "the unread count",
            (Widget::Announcements, Size::Medium) => "the newest four",
            (Widget::Announcements, _) => "two weeks, unread first",
            (Widget::Grades, Size::Small) => "the newest grade",
            (Widget::Grades, Size::Medium) => "every course's grade",
            (Widget::Grades, _) => "every grade and the newest scores",
            (Widget::RecentFiles, Size::Small) => "the last file",
            (Widget::RecentFiles, Size::Medium) => "the last four",
            (Widget::RecentFiles, _) => "the last dozen, with folders",
        }
    }

    /// Lines a Medium one takes, borders included.
    pub fn medium_height(self) -> u16 {
        match self {
            // The weekday row, then four lines of entries.
            Widget::Calendar => 7,
            // The unread count, then the newest three.
            Widget::Inbox => 6,
            Widget::Todo => 7,
            Widget::Announcements => 6,
            Widget::Grades => 6,
            Widget::RecentFiles => 6,
        }
    }
}

/// How big a widget is. Every widget comes in every size.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Size {
    Small,
    Medium,
    /// Large, half the side's height.
    LargeHalf,
    /// Large, the side's whole height, less what the others on it take.
    LargeWhole,
}

impl Size {
    pub const ALL: [Size; 4] = [Size::Small, Size::Medium, Size::LargeHalf, Size::LargeWhole];

    pub fn label(self) -> &'static str {
        match self {
            Size::Small => "small",
            Size::Medium => "medium",
            Size::LargeHalf => "large, half",
            Size::LargeWhole => "large, whole",
        }
    }

    pub fn large(self) -> bool {
        matches!(self, Size::LargeHalf | Size::LargeWhole)
    }

    /// The next size up or down, if there is one.
    pub fn step(self, bigger: bool) -> Option<Size> {
        let at = Size::ALL.iter().position(|s| *s == self)?;
        if bigger {
            Size::ALL.get(at + 1).copied()
        } else {
            at.checked_sub(1).map(|i| Size::ALL[i])
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Side {
    Left,
    Right,
}

impl Side {
    pub fn other(self) -> Side {
        match self {
            Side::Left => Side::Right,
            Side::Right => Side::Left,
        }
    }
}

/// One widget on the board, at its size.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tile {
    pub widget: Widget,
    pub size: Size,
}

/// What the user put on the home screen, side by side, top to bottom.
/// Each widget is on it at most once. Saved locally, per account.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Board {
    #[serde(default)]
    pub left: Vec<Tile>,
    #[serde(default)]
    pub right: Vec<Tile>,
}

impl Default for Board {
    /// The home screen as it was before it had a choice: the month on the
    /// left, the Inbox count on the right.
    fn default() -> Self {
        Board {
            left: vec![Tile {
                widget: Widget::Calendar,
                size: Size::LargeWhole,
            }],
            right: vec![Tile {
                widget: Widget::Inbox,
                size: Size::Small,
            }],
        }
    }
}

impl Board {
    pub fn side(&self, side: Side) -> &[Tile] {
        match side {
            Side::Left => &self.left,
            Side::Right => &self.right,
        }
    }

    fn side_mut(&mut self, side: Side) -> &mut Vec<Tile> {
        match side {
            Side::Left => &mut self.left,
            Side::Right => &mut self.right,
        }
    }

    /// Every tile, the left side first, each side top to bottom.
    pub fn tiles(&self) -> impl Iterator<Item = Tile> + '_ {
        self.left.iter().chain(self.right.iter()).copied()
    }

    pub fn find(&self, widget: Widget) -> Option<(Side, usize)> {
        [Side::Left, Side::Right].into_iter().find_map(|side| {
            self.side(side)
                .iter()
                .position(|t| t.widget == widget)
                .map(|i| (side, i))
        })
    }

    pub fn tile(&self, widget: Widget) -> Option<Tile> {
        self.tiles().find(|t| t.widget == widget)
    }

    /// The board as loaded, less any widget listed twice (the first stays).
    pub fn cleaned(mut self) -> Board {
        let mut seen = Vec::new();
        for side in [Side::Left, Side::Right] {
            self.side_mut(side).retain(|t| {
                let fresh = !seen.contains(&t.widget);
                seen.push(t.widget);
                fresh
            });
        }
        self
    }

    /// Put a widget on a side at `at` (clamped). A widget already on the
    /// board is not added twice.
    pub fn add(&mut self, tile: Tile, side: Side, at: usize) -> bool {
        if self.find(tile.widget).is_some() {
            return false;
        }
        let list = self.side_mut(side);
        let at = at.min(list.len());
        list.insert(at, tile);
        true
    }

    pub fn remove(&mut self, widget: Widget) -> Option<(Side, usize)> {
        let (side, at) = self.find(widget)?;
        self.side_mut(side).remove(at);
        Some((side, at))
    }

    /// One size up or down; the size it has now, or `None` at the end.
    pub fn resize(&mut self, widget: Widget, bigger: bool) -> Option<Size> {
        let (side, at) = self.find(widget)?;
        let tile = &mut self.side_mut(side)[at];
        let next = tile.size.step(bigger)?;
        tile.size = next;
        Some(next)
    }

    /// Swap a widget with the one above or below it on its side.
    pub fn shift(&mut self, widget: Widget, up: bool) -> bool {
        let Some((side, at)) = self.find(widget) else {
            return false;
        };
        let list = self.side_mut(side);
        let to = if up { at.checked_sub(1) } else { Some(at + 1) };
        match to {
            Some(to) if to < list.len() => {
                list.swap(at, to);
                true
            }
            _ => false,
        }
    }

    /// Move a widget to the other side, at the same place in its order (or
    /// the end, if that side is shorter).
    pub fn cross(&mut self, widget: Widget) -> Option<(Side, usize)> {
        let (side, at) = self.find(widget)?;
        let tile = self.side_mut(side).remove(at);
        let other = side.other();
        let list = self.side_mut(other);
        let to = at.min(list.len());
        list.insert(to, tile);
        Some((other, to))
    }
}

/// A Small widget's width and height, borders included.
pub const SMALL: (u16, u16) = (22, 4);
/// The fewest lines a Large widget is drawn in. Shorter than this it shows
/// nothing worth having, so it is left out instead.
pub const MIN_LARGE: u16 = 6;
/// Lines a side with a Large widget asks for: what the month calendar has
/// always had (its title, the weekday names, and a few lines a week).
const LARGE_WANTS: u16 = 2 + 6 * 3;

/// Where one widget is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Spot {
    pub widget: Widget,
    pub size: Size,
    pub side: Side,
    pub area: Rect,
}

/// The board worked out for one screen size.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Arranged {
    /// Left side first, each side top to bottom: the order `Tab` follows.
    pub spots: Vec<Spot>,
    /// On the board but with no room on this screen.
    pub left_out: Vec<Widget>,
}

#[cfg(test)]
impl Arranged {
    pub fn spot(&self, widget: Widget) -> Option<&Spot> {
        self.spots.iter().find(|s| s.widget == widget)
    }
}

/// The two sides, with a column between them.
pub fn sides(area: Rect) -> (Rect, Rect) {
    let left = area.width.saturating_sub(1) / 2;
    let right = area.width.saturating_sub(left + 1);
    (
        Rect { width: left, ..area },
        Rect {
            x: area.x + left + 1,
            width: right,
            ..area
        },
    )
}

/// How many Smalls fit side by side in `width`.
fn per_row(width: u16) -> usize {
    if width < SMALL.0 {
        0
    } else {
        usize::from((width + 1) / (SMALL.0 + 1))
    }
}

/// A side's widgets in the blocks they stack as: each row of Smalls is one
/// block, every other widget is its own.
fn blocks(tiles: &[Tile], width: u16) -> Vec<Vec<Tile>> {
    let room = per_row(width).max(1);
    let mut out: Vec<Vec<Tile>> = Vec::new();
    for tile in tiles {
        let joins = tile.size == Size::Small
            && out
                .last()
                .is_some_and(|b| b[0].size == Size::Small && b.len() < room);
        if joins {
            out.last_mut().expect("checked").push(*tile);
        } else {
            out.push(vec![*tile]);
        }
    }
    out
}

/// Lines a side asks for to show everything on it.
fn wanted(tiles: &[Tile], width: u16) -> u16 {
    let mut fixed = 0u16;
    let mut larges = 0u16;
    for block in blocks(tiles, width) {
        match block[0].size {
            Size::Small => fixed += SMALL.1,
            Size::Medium => fixed += block[0].widget.medium_height(),
            Size::LargeHalf | Size::LargeWhole => larges += 1,
        }
    }
    if larges == 0 {
        fixed
    } else {
        (fixed + MIN_LARGE * larges).max(LARGE_WANTS)
    }
}

/// How the home screen divides `area`: the course list, the rule under it,
/// and the board.
///
/// The board asks for what its taller side needs, and a side with a Large
/// widget asks for the twenty lines the month calendar always had. The course
/// list keeps at least a third of the screen whatever the board asks, and on
/// a very short screen it keeps all of it.
pub fn split(area: Rect, course_rows: u16, board: &Board, editing: bool) -> (Rect, Rect, Rect) {
    let (left, right) = sides(area);
    let mut wants = wanted(&board.left, left.width).max(wanted(&board.right, right.width));
    if editing {
        // Room to see where things go, even on an empty board.
        wants = wants.max(MIN_LARGE);
    }
    let short = if editing { 6 } else { 12 };
    let list = if wants == 0 || area.height <= short {
        area.height
    } else {
        course_rows.min(
            area.height
                .saturating_sub(wants + 1)
                .max(area.height / 3)
                .max(4),
        )
    };
    let rule = u16::from(area.height > list);
    let top = Rect {
        height: list,
        ..area
    };
    let line = Rect {
        y: area.y + list,
        height: rule,
        ..area
    };
    let rest = Rect {
        y: area.y + list + rule,
        height: area.height.saturating_sub(list + rule),
        ..area
    };
    (top, line, rest)
}

/// Where each widget on the board goes in `area`.
pub fn arrange(board: &Board, area: Rect) -> Arranged {
    let (left, right) = sides(area);
    let mut out = Arranged::default();
    arrange_side(&board.left, Side::Left, left, &mut out);
    arrange_side(&board.right, Side::Right, right, &mut out);
    out
}

fn arrange_side(tiles: &[Tile], side: Side, area: Rect, out: &mut Arranged) {
    let blocks = blocks(tiles, area.width);
    let half = area.height / 2;
    let mut fixed = 0u16;
    let mut wholes = 0u16;
    for block in &blocks {
        match block[0].size {
            Size::Small => fixed += SMALL.1,
            Size::Medium => fixed += block[0].widget.medium_height(),
            Size::LargeHalf => fixed += half,
            Size::LargeWhole => wholes += 1,
        }
    }
    // Whole ones share what the rest leave; the lowest gets any odd line.
    let spare = area.height.saturating_sub(fixed);
    let mut nth_whole = 0u16;
    let bottom = area.y + area.height;
    let mut y = area.y;
    let room = per_row(area.width);
    for block in blocks {
        let first = block[0];
        if first.size == Size::Small {
            if room == 0 || y + SMALL.1 > bottom {
                out.left_out.extend(block.iter().map(|t| t.widget));
                continue;
            }
            for (i, tile) in block.iter().enumerate() {
                out.spots.push(Spot {
                    widget: tile.widget,
                    size: tile.size,
                    side,
                    area: Rect {
                        x: area.x + i as u16 * (SMALL.0 + 1),
                        y,
                        width: SMALL.0,
                        height: SMALL.1,
                    },
                });
            }
            y += SMALL.1;
            continue;
        }
        let left = bottom.saturating_sub(y);
        let height = match first.size {
            Size::Medium => first.widget.medium_height(),
            // A Large one gives up lines rather than leave the board, as long
            // as it keeps enough to be worth drawing.
            Size::LargeHalf => half.min(left),
            _ => {
                let share = spare / wholes + u16::from(nth_whole >= wholes - spare % wholes);
                nth_whole += 1;
                share.min(left)
            }
        };
        let least = if first.size.large() { MIN_LARGE } else { 3 };
        if height < least || height > left || area.width < SMALL.0 {
            out.left_out.push(first.widget);
            continue;
        }
        out.spots.push(Spot {
            widget: first.widget,
            size: first.size,
            side,
            area: Rect {
                x: area.x,
                y,
                width: area.width,
                height,
            },
        });
        y += height;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tile(widget: Widget, size: Size) -> Tile {
        Tile { widget, size }
    }

    fn board(left: &[Tile], right: &[Tile]) -> Board {
        Board {
            left: left.to_vec(),
            right: right.to_vec(),
        }
    }

    #[test]
    fn the_default_is_the_home_screen_as_it_was() {
        let area = Rect::new(0, 3, 112, 24);
        let placed = arrange(&Board::default(), area);
        assert!(placed.left_out.is_empty());
        let calendar = placed.spot(Widget::Calendar).unwrap();
        assert_eq!((calendar.area.x, calendar.area.y), (0, 3));
        assert_eq!((calendar.area.width, calendar.area.height), (55, 24));
        let inbox = placed.spot(Widget::Inbox).unwrap();
        assert_eq!(inbox.area, Rect::new(56, 3, 22, 4));
        assert_eq!(
            placed.spots.iter().map(|s| s.widget).collect::<Vec<_>>(),
            vec![Widget::Calendar, Widget::Inbox],
            "Tab goes down the left side, then the right"
        );
    }

    #[test]
    fn smalls_share_a_row_while_they_fit_and_mediums_take_the_width() {
        let b = board(
            &[],
            &[
                tile(Widget::Inbox, Size::Small),
                tile(Widget::Todo, Size::Small),
                tile(Widget::Grades, Size::Small),
                tile(Widget::Announcements, Size::Medium),
            ],
        );
        // A 50-wide side holds two Smalls a row.
        let placed = arrange(&b, Rect::new(0, 0, 101, 30));
        let at = |w| placed.spot(w).unwrap().area;
        assert_eq!(at(Widget::Inbox), Rect::new(51, 0, 22, 4));
        assert_eq!(at(Widget::Todo), Rect::new(74, 0, 22, 4));
        assert_eq!(at(Widget::Grades), Rect::new(51, 4, 22, 4), "the third wraps");
        assert_eq!(at(Widget::Announcements), Rect::new(51, 8, 50, 6));
        // On a narrow side each Small has a row of its own.
        let narrow = arrange(&b, Rect::new(0, 0, 60, 30));
        assert_eq!(narrow.spot(Widget::Todo).unwrap().area.y, 4);
    }

    #[test]
    fn a_half_large_stays_half_and_a_whole_one_takes_what_is_left() {
        let half = board(&[tile(Widget::Grades, Size::LargeHalf)], &[]);
        let placed = arrange(&half, Rect::new(0, 0, 80, 20));
        assert_eq!(placed.spot(Widget::Grades).unwrap().area.height, 10);
        let whole = board(
            &[
                tile(Widget::Calendar, Size::Medium),
                tile(Widget::Grades, Size::LargeWhole),
            ],
            &[],
        );
        let placed = arrange(&whole, Rect::new(0, 0, 80, 20));
        assert_eq!(placed.spot(Widget::Grades).unwrap().area, Rect::new(0, 7, 39, 13));
        // Two whole ones share it.
        let two = board(
            &[
                tile(Widget::Grades, Size::LargeWhole),
                tile(Widget::Todo, Size::LargeWhole),
            ],
            &[],
        );
        let placed = arrange(&two, Rect::new(0, 0, 80, 21));
        assert_eq!(placed.spot(Widget::Grades).unwrap().area.height, 10);
        assert_eq!(placed.spot(Widget::Todo).unwrap().area, Rect::new(0, 10, 39, 11));
    }

    #[test]
    fn what_does_not_fit_is_left_out_and_named() {
        let b = board(
            &[
                tile(Widget::Todo, Size::Medium),
                tile(Widget::Calendar, Size::Medium),
                tile(Widget::Inbox, Size::Small),
                tile(Widget::Grades, Size::LargeWhole),
            ],
            &[],
        );
        let placed = arrange(&b, Rect::new(0, 0, 80, 13));
        assert!(placed.spot(Widget::Todo).is_some());
        assert_eq!(placed.left_out, vec![Widget::Calendar, Widget::Grades]);
        assert!(
            placed.spot(Widget::Inbox).is_some(),
            "a later one that fits still goes in"
        );
        // Nothing fits a side narrower than a Small.
        let tiny = arrange(&b, Rect::new(0, 0, 30, 40));
        assert!(tiny.spots.is_empty());
    }

    #[test]
    fn the_course_list_keeps_its_share_whatever_the_board_asks() {
        let area = Rect::new(0, 1, 112, 28);
        let (list, rule, rest) = split(area, 4, &Board::default(), false);
        assert_eq!((list.height, rule.height, rest.height), (4, 1, 23));
        // Many courses: the board still gets its twenty lines, no more.
        let (list, _, rest) = split(area, 40, &Board::default(), false);
        assert_eq!((list.height, rest.height), (9, 18));
        // Smalls alone ask for less, so the list may have more.
        let smalls = board(&[tile(Widget::Inbox, Size::Small)], &[]);
        let (list, _, rest) = split(area, 40, &smalls, false);
        assert_eq!((list.height, rest.height), (23, 4));
        // An empty board leaves the whole screen to the list.
        let empty = board(&[], &[]);
        assert_eq!(split(area, 40, &empty, false).0.height, 28);
        assert!(split(area, 40, &empty, true).2.height >= MIN_LARGE);
    }

    #[test]
    fn widgets_move_resize_and_come_and_go() {
        let mut b = Board::default();
        assert_eq!(b.resize(Widget::Inbox, true), Some(Size::Medium));
        assert_eq!(b.resize(Widget::Calendar, true), None, "already the biggest");
        assert_eq!(b.resize(Widget::Calendar, false), Some(Size::LargeHalf));
        assert!(b.add(tile(Widget::Todo, Size::Small), Side::Right, 0));
        assert!(!b.add(tile(Widget::Todo, Size::Medium), Side::Left, 0), "once only");
        assert!(b.shift(Widget::Todo, false));
        assert_eq!(b.find(Widget::Todo), Some((Side::Right, 1)));
        assert!(!b.shift(Widget::Todo, false), "already at the bottom");
        assert_eq!(b.cross(Widget::Todo), Some((Side::Left, 1)));
        assert_eq!(b.remove(Widget::Calendar), Some((Side::Left, 0)));
        assert_eq!(
            b.tiles().map(|t| t.widget).collect::<Vec<_>>(),
            vec![Widget::Todo, Widget::Inbox]
        );
    }

    #[test]
    fn a_saved_board_round_trips_and_repeats_are_dropped() {
        let b = board(
            &[tile(Widget::RecentFiles, Size::LargeHalf)],
            &[tile(Widget::Announcements, Size::Small)],
        );
        let text = serde_json::to_string(&b).unwrap();
        assert!(text.contains("recent-files") && text.contains("large-half"), "{text}");
        assert_eq!(serde_json::from_str::<Board>(&text).unwrap(), b);
        let twice: Board = serde_json::from_str(
            r#"{"left":[{"widget":"inbox","size":"small"}],"right":[{"widget":"inbox","size":"medium"}]}"#,
        )
        .unwrap();
        assert_eq!(twice.cleaned().tiles().count(), 1);
        assert!(serde_json::from_str::<Board>(r#"{"left":[{"widget":"weather","size":"small"}]}"#).is_err());
    }
}
