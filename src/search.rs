//! Matching and ranking for the search palette.
//!
//! One box finds anything the reader knows about: a course, an assignment, an
//! announcement, a page, a file, a conversation, or an action to run. This
//! module is only the matching and the ordering; what each hit opens is the
//! app's business, and reading the things to search is the warm-up's.
//!
//! The match is a subsequence, not a substring: `cop46 l2` finds "COP4600 —
//! Lab 2". That is what makes a single box usable when the thing you want is
//! named something you only half remember. Scoring prefers, in order, a word
//! start over the middle of a word, an earlier match over a later one, and a
//! run of adjacent characters over a scatter, so the obvious answer comes
//! first without the query having to be exact.

use crate::keys::Act;

/// How much better a word start is than a letter inside a word.
const WORD_START: i64 = 18;
/// Added for each character that directly follows the previous match.
///
/// Worth more than a word start, and deliberately so: the scores the
/// maximiser compares are built from these two, and if a word start paid
/// better then three scattered word starts would beat the literal word.
/// "lab" really did match "**L**ab Grade ... **a**nd ... **b**e" that way.
const ADJACENT: i64 = 28;
/// Taken off for each character skipped before the first match.
const LEADING_SKIP: i64 = 2;
/// The most a leading skip can cost, so a long title is not ruled out.
const MAX_LEADING_PENALTY: i64 = 40;
/// Added when the whole query appears as one run. This lands after the
/// maximiser has chosen, so it only separates finished candidates: the run
/// itself has to be worth finding, which is what `ADJACENT` is for.
const EXACT_RUN: i64 = 30;
/// The score of a hit whose title does not match at all, and which is only
/// here because the course or folder it sits in does. Below anything a real
/// title match can score.
const CONTEXT_ONLY: i64 = -1_000;

/// What a hit opens, which the app turns into a view or an action.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Course(String),
    Assignment(String, String),
    Announcement(String, String),
    /// A course page, by course id and the page's URL name.
    Page(String, String),
    /// A file in a course's Files tab, by course id and file id.
    File(String, String),
    Conversation(String),
    Action(Act),
}

impl Target {
    /// The course this result belongs to, where it belongs to one. An `@`
    /// tag narrows the search to the courses it names, so everything that
    /// does not sit in a course falls outside it.
    pub fn course(&self) -> Option<&str> {
        match self {
            Target::Course(id)
            | Target::Assignment(id, _)
            | Target::Announcement(id, _)
            | Target::Page(id, _)
            | Target::File(id, _) => Some(id),
            Target::Conversation(_) | Target::Action(_) => None,
        }
    }
}

/// A query split into its `@` tags and the words left over.
///
/// `@cop4600 lab 2` means "lab 2, in COP4600". The tag is matched against
/// course names and codes the same way everything else is matched, so a
/// couple of characters is enough, and several tags widen the net rather
/// than narrowing it to nothing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Query {
    /// The text after each `@`, in the order typed.
    pub tags: Vec<String>,
    /// Everything that was not part of a tag.
    pub text: String,
    /// Whether the query ends in a tag still being typed, in which case the
    /// palette offers courses to finish it with.
    pub tagging: bool,
}

pub fn parse(query: &str) -> Query {
    let mut out = Query::default();
    let mut words: Vec<&str> = Vec::new();
    for word in query.split_whitespace() {
        match word.strip_prefix('@') {
            Some(tag) => out.tags.push(tag.to_string()),
            None => words.push(word),
        }
    }
    // A tag is still being typed while the cursor is right after it: the last
    // thing typed starts with `@` and no space has followed.
    out.tagging = query
        .split_whitespace()
        .next_back()
        .is_some_and(|w| w.starts_with('@'))
        && !query.ends_with(char::is_whitespace);
    out.text = words.join(" ");
    out.tags.retain(|t| !t.is_empty());
    out
}

/// What kind of thing a hit is, which decides its badge and its tie-break.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Action,
    Course,
    Assignment,
    Announcement,
    Page,
    File,
    Conversation,
}

impl Kind {
    /// The short word shown before the title.
    pub fn label(self) -> &'static str {
        match self {
            Kind::Action => "do",
            Kind::Course => "course",
            Kind::Assignment => "work",
            Kind::Announcement => "news",
            Kind::Page => "page",
            Kind::File => "file",
            Kind::Conversation => "inbox",
        }
    }

    /// Ordering between kinds when the scores are equal: what someone typing a
    /// couple of letters most likely meant.
    fn rank(self) -> i64 {
        match self {
            Kind::Course => 0,
            Kind::Assignment => 1,
            Kind::Action => 2,
            Kind::Page => 3,
            Kind::Announcement => 4,
            Kind::Conversation => 5,
            Kind::File => 6,
        }
    }
}

/// One thing the palette can show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    pub kind: Kind,
    pub title: String,
    /// Where it lives, shown after the title: a course name, a folder, a date.
    pub context: String,
    pub target: Target,
    /// Set by `rank`; the order the palette draws them in.
    pub score: i64,
    /// Which characters of the title the query matched, for highlighting.
    pub marks: Vec<usize>,
}

impl Hit {
    pub fn new(kind: Kind, title: impl Into<String>, context: impl Into<String>, target: Target) -> Self {
        Hit {
            kind,
            title: title.into(),
            context: context.into(),
            target,
            score: 0,
            marks: Vec::new(),
        }
    }
}

/// Where a query matched, and how well.
struct Match {
    score: i64,
    marks: Vec<usize>,
}

/// Score `query` against `text`, or `None` when the characters are not all
/// there in order.
///
/// Case is ignored, and whitespace in the query is dropped so `lab 2` and
/// `lab2` behave alike. The marks are character positions in `text`, so a
/// caller can underline them whatever the character width.
///
/// Every placement is considered, not the first one that fits. Taking each
/// character greedily is wrong in a way that shows up immediately: preferring
/// a word start would match `cop46` against "COP4600 Operating Systems" by
/// jumping to the capital of "Operating", and then fail to find the `4` left
/// behind it. So this keeps, for each character of the query and each place it
/// could sit, the best score of any match ending there, and reads the answer
/// off the end.
fn score(query: &str, text: &str) -> Option<Match> {
    let needle: Vec<char> = query
        .chars()
        .flat_map(|c| c.to_lowercase())
        .filter(|c| !c.is_whitespace())
        .collect();
    if needle.is_empty() {
        return Some(Match {
            score: 0,
            marks: Vec::new(),
        });
    }
    let hay: Vec<char> = text.chars().collect();
    let lower: Vec<char> = hay
        .iter()
        .map(|c| c.to_lowercase().next().unwrap_or(*c))
        .collect();
    if needle.len() > lower.len() {
        return None;
    }
    // `best[j]`: the score of the best match of the query so far whose latest
    // character sits at `j`. `from[i][j]`: where the character before it sat,
    // so the marks can be read back once the end is known.
    let mut best: Vec<Option<i64>> = vec![None; lower.len()];
    let mut from: Vec<Vec<usize>> = vec![vec![usize::MAX; lower.len()]; needle.len()];
    for (j, c) in lower.iter().enumerate() {
        if *c == needle[0] {
            let skipped = (j as i64) * LEADING_SKIP;
            let mut s = -skipped.min(MAX_LEADING_PENALTY);
            if starts_word(&lower, j) {
                s += WORD_START;
            }
            best[j] = Some(s);
        }
    }
    for i in 1..needle.len() {
        let mut next: Vec<Option<i64>> = vec![None; lower.len()];
        // The best place the previous character could have sat, anywhere
        // behind the one being considered. Carried along as we sweep.
        let mut behind: Option<(i64, usize)> = None;
        for j in 0..lower.len() {
            if lower[j] == needle[i] {
                // Either the best predecessor anywhere behind, or the one
                // directly behind, which earns the adjacency bonus.
                let mut pick = behind;
                if j > 0
                    && let Some(previous) = best[j - 1]
                    && pick.is_none_or(|(s, _)| previous + ADJACENT > s)
                {
                    pick = Some((previous + ADJACENT, j - 1));
                }
                if let Some((s, at)) = pick {
                    let bonus = if starts_word(&lower, j) { WORD_START } else { 0 };
                    next[j] = Some(s + bonus);
                    from[i][j] = at;
                }
            }
            if let Some(s) = best[j]
                && behind.is_none_or(|(b, _)| s > b)
            {
                behind = Some((s, j));
            }
        }
        best = next;
    }
    // Where the last character sat, for the best total; the earliest such
    // place when several tie, so the marks are the leftmost of the best.
    let (end, mut total) = best
        .iter()
        .enumerate()
        .filter_map(|(j, s)| s.map(|s| (j, s)))
        .max_by_key(|&(j, s)| (s, std::cmp::Reverse(j)))?;
    let mut marks = vec![0usize; needle.len()];
    let mut at = end;
    for i in (0..needle.len()).rev() {
        marks[i] = at;
        if i > 0 {
            at = from[i][at];
        }
    }
    // One unbroken run of the whole query is the clearest possible answer.
    if marks.len() > 1 && marks.windows(2).all(|w| w[1] == w[0] + 1) {
        total += EXACT_RUN;
    }
    // A short title that is almost all match beats a long one that merely
    // contains the letters.
    let spare = hay.len().saturating_sub(needle.len()) as i64;
    total -= spare.min(30) / 3;
    Some(Match {
        score: total,
        marks,
    })
}

/// Whether position `i` begins a word: the start, or after a separator.
fn starts_word(lower: &[char], i: usize) -> bool {
    if i == 0 {
        return true;
    }
    let before = lower[i - 1];
    !before.is_alphanumeric()
}

/// The best score the query gets against any of `texts`, or `None` when it
/// matches none of them. Used for `@` tags, which are matched against both a
/// course's name and its code.
///
/// An empty query matches everything, which is what makes `@` on its own
/// offer every class.
pub fn best_of(query: &str, texts: &[&str]) -> Option<i64> {
    texts
        .iter()
        .filter(|t| !t.is_empty())
        .filter_map(|t| score(query, t).map(|m| m.score))
        .max()
}

/// Keep the hits whose title or context matches, best first.
///
/// An empty query keeps everything in kind order, which is what the palette
/// shows before anything is typed.
pub fn rank(query: &str, hits: Vec<Hit>, limit: usize) -> Vec<Hit> {
    let query = query.trim();
    let mut scored: Vec<Hit> = hits
        .into_iter()
        .filter_map(|mut hit| {
            if query.is_empty() {
                hit.score = 0;
                hit.marks.clear();
                return Some(hit);
            }
            // The title is what someone aims at; a match only in the context
            // (the course a thing is in) still counts, but for less.
            match score(query, &hit.title) {
                Some(m) => {
                    hit.score = m.score;
                    hit.marks = m.marks;
                    Some(hit)
                }
                None => {
                    // The context is matched as plain text, not as a
                    // subsequence. Spread-out letters are nearly always
                    // findable in something like a conversation's list of
                    // seventy participants, which turns every long context
                    // into a match for almost any query.
                    let needle = query.to_lowercase();
                    if !hit.context.to_lowercase().contains(needle.trim()) {
                        return None;
                    }
                    hit.score = CONTEXT_ONLY;
                    hit.marks.clear();
                    Some(hit)
                }
            }
        })
        .collect();
    // Stable, so an empty query keeps the order the caller built: courses as
    // the course list shows them, and so on. Ordering those by title length
    // would be arbitrary and would look like a shuffle.
    let empty = query.is_empty();
    scored.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| a.kind.rank().cmp(&b.kind.rank()))
            .then_with(|| {
                if empty {
                    std::cmp::Ordering::Equal
                } else {
                    a.title
                        .len()
                        .cmp(&b.title.len())
                        .then_with(|| a.title.cmp(&b.title))
                }
            })
    });
    scored.truncate(limit);
    scored
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(kind: Kind, title: &str, context: &str) -> Hit {
        Hit::new(kind, title, context, Target::Course("1".into()))
    }

    fn titles(hits: &[Hit]) -> Vec<&str> {
        hits.iter().map(|h| h.title.as_str()).collect()
    }

    #[test]
    fn letters_may_be_spread_through_the_title() {
        // The point of the whole feature: half-remembered names still land.
        assert!(score("cop46", "COP4600 Operating Systems").is_some());
        assert!(score("lab2", "Lab 2: Pipelining").is_some());
        assert!(score("opsys", "COP4600 Operating Systems").is_some());
        assert!(score("zebra", "COP4600 Operating Systems").is_none());
    }

    #[test]
    fn a_word_start_is_not_taken_at_the_cost_of_the_rest() {
        // The bug that made this a maximiser instead of a greedy walk: the
        // capital of "Operating" is a tempting place for the `o`, and taking
        // it leaves the `4` with nowhere to go.
        let m = score("cop46", "COP4600 Operating Systems").expect("matches");
        assert_eq!(m.marks, [0, 1, 2, 3, 4]);
        // And the run it finds is the whole query in one piece, so it beats a
        // title where the same letters are scattered.
        let ranked = rank(
            "cop46",
            vec![
                hit(Kind::Course, "COP4600 Operating Systems", "Fall"),
                hit(Kind::Course, "Civic Online Participation 4-6", "Fall"),
            ],
            10,
        );
        assert_eq!(titles(&ranked)[0], "COP4600 Operating Systems");
    }

    #[test]
    fn a_space_in_the_query_is_ignored() {
        assert_eq!(
            score("lab 2", "Lab 2").map(|m| m.marks),
            score("lab2", "Lab 2").map(|m| m.marks),
        );
    }

    #[test]
    fn case_is_ignored_both_ways() {
        assert!(score("COP", "cop4600").is_some());
        assert!(score("cop", "COP4600").is_some());
    }

    #[test]
    fn an_empty_query_matches_everything() {
        let m = score("", "anything").expect("empty matches");
        assert_eq!(m.score, 0);
        assert!(m.marks.is_empty());
    }

    #[test]
    fn initials_take_the_word_starts() {
        let m = score("pdr", "Professional Development: Resume").expect("matches");
        let chars: Vec<char> = "Professional Development: Resume".chars().collect();
        let picked: String = m.marks.iter().map(|&i| chars[i]).collect();
        assert_eq!(picked, "PDR", "each letter should land on a word start");
    }

    #[test]
    fn a_whole_word_beats_scattered_letters() {
        let ranked = rank(
            "lab",
            vec![
                hit(Kind::Assignment, "Lab 2", "COP4600"),
                hit(Kind::Assignment, "Learning About Bees", "ZOO1010"),
            ],
            10,
        );
        assert_eq!(titles(&ranked)[0], "Lab 2");
    }

    #[test]
    fn an_earlier_match_beats_a_later_one() {
        let ranked = rank(
            "git",
            vec![
                hit(Kind::Page, "Downloading Files and Viewing Git", "Intro"),
                hit(Kind::Page, "Git Activity 1", "Intro"),
            ],
            10,
        );
        assert_eq!(titles(&ranked)[0], "Git Activity 1");
    }

    #[test]
    fn a_course_wins_a_tie_against_a_file() {
        let ranked = rank(
            "cop4600",
            vec![
                hit(Kind::File, "COP4600", "Class Files"),
                hit(Kind::Course, "COP4600", "Fall 2026"),
            ],
            10,
        );
        assert_eq!(ranked[0].kind, Kind::Course);
    }

    #[test]
    fn matching_only_the_course_still_finds_the_item_but_ranks_it_lower() {
        let ranked = rank(
            "cop4600",
            vec![
                hit(Kind::Assignment, "Lab 2", "COP4600"),
                hit(Kind::Course, "COP4600 Operating Systems", "Fall 2026"),
            ],
            10,
        );
        assert_eq!(ranked.len(), 2, "the assignment matches through its course");
        assert_eq!(ranked[0].kind, Kind::Course, "the named thing comes first");
        assert!(
            ranked[1].marks.is_empty(),
            "nothing in the title matched, so nothing is underlined"
        );
    }

    #[test]
    fn the_context_must_really_contain_the_query() {
        // What live data caught: scattered letters are nearly always findable
        // in a long context, so "lab" matched a conversation called
        // "Lecture" through "Silva, Adam Benali" in its participants.
        let ranked = rank(
            "lab",
            vec![
                hit(Kind::Conversation, "Lecture", "Catia Silva, Adam Benali"),
                hit(Kind::Assignment, "Lab 2", "COP4600"),
            ],
            10,
        );
        assert_eq!(titles(&ranked), ["Lab 2"]);
        // A context that really holds the text still counts.
        let ranked = rank(
            "cop4600",
            vec![hit(Kind::Assignment, "Lab 2", "COP4600 Operating Systems")],
            10,
        );
        assert_eq!(titles(&ranked), ["Lab 2"]);
    }

    #[test]
    fn a_title_match_always_outranks_a_context_match() {
        let ranked = rank(
            "git",
            vec![
                hit(Kind::Course, "Introduction to Git", "Ongoing"),
                hit(Kind::Assignment, "Activity 1", "Introduction to Git"),
            ],
            10,
        );
        assert_eq!(titles(&ranked)[0], "Introduction to Git");
    }

    #[test]
    fn an_empty_query_keeps_the_order_it_was_given() {
        let given = ["Zebra Studies", "A course", "Middle"];
        let ranked = rank(
            "",
            given
                .iter()
                .map(|t| hit(Kind::Course, t, "Fall"))
                .collect(),
            10,
        );
        assert_eq!(titles(&ranked), given);
    }

    #[test]
    fn the_literal_word_beats_scattered_word_starts() {
        // From live data: the maximiser preferred "**L**ab Grade ... **a**nd
        // ... **b**e", three word starts, over the word "Lab" itself. The
        // run bonus could not fix it, because it is added after the choice
        // is made; adjacency has to pay better than a word start.
        let m = score("lab", "Lab Grade Based on Points and Letter Grade").expect("matches");
        assert_eq!(m.marks, [0, 1, 2], "the word itself");
        let ranked = rank(
            "lab",
            vec![
                hit(Kind::Conversation, "Physics Laboratory Schedule", "A"),
                hit(Kind::Conversation, "Lab Grade Based on Points and Letter Grade", "A"),
            ],
            10,
        );
        assert_eq!(
            titles(&ranked)[0],
            "Lab Grade Based on Points and Letter Grade",
            "and the one that starts with it comes first"
        );
    }

    #[test]
    fn nothing_matching_gives_nothing() {
        let ranked = rank("qqqq", vec![hit(Kind::Course, "COP4600", "Fall")], 10);
        assert!(ranked.is_empty());
    }

    #[test]
    fn an_empty_query_keeps_everything_in_kind_order() {
        let ranked = rank(
            "",
            vec![
                hit(Kind::File, "notes.pdf", "Class Files"),
                hit(Kind::Course, "COP4600", "Fall"),
                hit(Kind::Assignment, "Lab 2", "COP4600"),
            ],
            10,
        );
        assert_eq!(
            ranked.iter().map(|h| h.kind).collect::<Vec<_>>(),
            [Kind::Course, Kind::Assignment, Kind::File]
        );
    }

    #[test]
    fn the_limit_is_respected() {
        let many: Vec<Hit> = (0..50)
            .map(|i| hit(Kind::Assignment, &format!("Lab {i}"), "COP4600"))
            .collect();
        assert_eq!(rank("lab", many, 8).len(), 8);
    }

    #[test]
    fn marks_point_at_the_characters_that_matched() {
        let ranked = rank("lb2", vec![hit(Kind::Assignment, "Lab 2", "COP")], 10);
        let chars: Vec<char> = "Lab 2".chars().collect();
        let picked: String = ranked[0].marks.iter().map(|&i| chars[i]).collect();
        assert_eq!(picked, "Lb2");
    }

    #[test]
    fn a_short_title_beats_a_long_one_holding_the_same_letters() {
        let ranked = rank(
            "lab2",
            vec![
                hit(Kind::Assignment, "Lab 2 submission guidelines and rubric", "A"),
                hit(Kind::Assignment, "Lab 2", "A"),
            ],
            10,
        );
        assert_eq!(titles(&ranked)[0], "Lab 2");
    }
}

