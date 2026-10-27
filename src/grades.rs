//! Course grades: Canvas's assignment groups with their weights, each
//! assignment's score, the course total where the instructor shows it, and
//! which assignments have instructor comments.
//!
//! Canvas leaves the computed scores out of the enrolment when the instructor
//! hides final grades, so an absent total means "not shown", not zero.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::canvas::{CanvasClient, CanvasError, FetchStats};
use crate::model::{AssignmentSummary, de_id, valid_id};

/// A course's grades as the reader keeps them.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Grades {
    /// `None` where the instructor hides the course total.
    pub total: Option<Total>,
    /// True when the groups' weights decide the total.
    #[serde(default)]
    pub weighted: bool,
    pub groups: Vec<Group>,
    /// Instructor comments, newest first, by assignment.
    #[serde(default)]
    pub feedback: Vec<Feedback>,
}

/// The course total Canvas computes for the student.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Total {
    #[serde(default)]
    pub score: Option<f64>,
    #[serde(default)]
    pub grade: Option<String>,
}

impl Total {
    /// Nothing to show: Canvas returned an enrolment without a total.
    fn empty(&self) -> bool {
        self.score.is_none() && self.grade.is_none()
    }
}

/// One Canvas assignment group, with the weight it carries in the total.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Group {
    #[serde(deserialize_with = "de_id")]
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub position: Option<i64>,
    /// Percent of the course total, where the course weights its groups.
    #[serde(default)]
    pub group_weight: Option<f64>,
    #[serde(default)]
    pub assignments: Vec<AssignmentSummary>,
}

impl Group {
    pub fn name(&self) -> &str {
        self.name.as_deref().unwrap_or("Assignments")
    }

    /// Points earned and points possible over the graded assignments, which
    /// is what Canvas uses for the group's own percentage. Excused and
    /// omitted work counts for neither side.
    ///
    /// A guess counts its assignment in even when Canvas has not graded it,
    /// or has excused it, because that is the question being asked: what the
    /// total becomes if this work scores that. Work the instructor omits from
    /// the final grade still counts for nothing, guess or no guess, since
    /// scoring it cannot move the total.
    pub fn earned_with(&self, guesses: &WhatIf) -> Option<(f64, f64)> {
        let mut score = 0.0;
        let mut possible = 0.0;
        let mut any = false;
        for a in &self.assignments {
            if a.omit_from_final_grade == Some(true) {
                continue;
            }
            let earned = match guesses.get(&a.id) {
                Some(guess) => *guess,
                None => {
                    let Some(s) = a.submission.as_ref() else {
                        continue;
                    };
                    if s.excused == Some(true) {
                        continue;
                    }
                    let Some(earned) = s.score else { continue };
                    earned
                }
            };
            any = true;
            score += earned;
            possible += a.points_possible.unwrap_or(0.0);
        }
        any.then_some((score, possible))
    }
}

/// Hypothetical scores, by assignment id: what the student is asking the
/// total would be if this work scored that. Never sent to Canvas, and never
/// written to disk.
pub type WhatIf = std::collections::HashMap<String, f64>;

/// Instructor comments on one assignment, as far as the reader knows.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Feedback {
    pub assignment_id: String,
    pub count: usize,
    /// When the newest instructor comment was left.
    #[serde(default)]
    pub latest: Option<DateTime<Utc>>,
}

/// Where the course total comes from: the student's own enrolment.
#[derive(Deserialize)]
struct CourseTotals {
    #[serde(default)]
    enrollments: Vec<Enrollment>,
    #[serde(default)]
    apply_assignment_group_weights: Option<bool>,
}

#[derive(Deserialize)]
struct Enrollment {
    #[serde(default)]
    computed_current_score: Option<f64>,
    #[serde(default)]
    computed_current_grade: Option<String>,
    #[serde(rename = "type", default)]
    kind: Option<String>,
}

/// One submission with its comments, from the multi-assignment read.
#[derive(Deserialize)]
struct CommentedSubmission {
    #[serde(default, deserialize_with = "crate::model::de_opt_id")]
    assignment_id: Option<String>,
    #[serde(default, deserialize_with = "crate::model::de_opt_id")]
    user_id: Option<String>,
    #[serde(default)]
    submission_comments: Vec<Comment>,
}

#[derive(Deserialize)]
struct Comment {
    #[serde(default, deserialize_with = "crate::model::de_opt_id")]
    author_id: Option<String>,
    #[serde(default)]
    created_at: Option<DateTime<Utc>>,
}

fn check(course: &str) -> Result<(), CanvasError> {
    if valid_id(course) {
        Ok(())
    } else {
        Err(CanvasError::Invalid("invalid course ID"))
    }
}

fn add(total: &mut FetchStats, s: &FetchStats) {
    total.requests += s.requests;
    total.bytes += s.bytes;
    total.network_ms += s.network_ms;
    total.parse_ms += s.parse_ms;
}

/// The course's grades: its groups with scores, then its total, then the
/// instructor comments. A course that refuses the comment read keeps its
/// grades; only the comment marks are missing.
pub async fn read(
    client: &CanvasClient,
    course: &str,
) -> Result<(Grades, FetchStats), CanvasError> {
    check(course)?;
    let started = std::time::Instant::now();
    let (mut groups, mut stats): (Vec<Group>, _) = client
        .get_all_pages(
            &format!(
                "/api/v1/courses/{course}/assignment_groups\
                 ?include[]=assignments&include[]=submission\
                 &exclude_response_fields[]=description&exclude_response_fields[]=rubric\
                 &per_page=50"
            ),
            10,
        )
        .await?;
    groups.retain(|g| valid_id(&g.id));
    for g in &mut groups {
        g.assignments.retain(|a| valid_id(&a.id));
        crate::api::sort_by_due(&mut g.assignments);
    }
    groups.sort_by_key(|g| g.position.unwrap_or(i64::MAX));

    let (totals, s): (CourseTotals, _) = client
        .get_json(&format!(
            "/api/v1/courses/{course}?include[]=total_scores&include[]=current_grading_period_scores"
        ))
        .await?;
    add(&mut stats, &s);
    let total = totals
        .enrollments
        .iter()
        .find(|e| !matches!(e.kind.as_deref(), Some("teacher") | Some("ta")))
        .map(|e| Total {
            score: e.computed_current_score,
            grade: e.computed_current_grade.clone(),
        })
        .filter(|t| !t.empty());

    let feedback = match comments(client, course).await {
        Ok((list, s)) => {
            add(&mut stats, &s);
            list
        }
        // An expired session must still reach the user; a course that will not
        // show comments simply shows none.
        Err(e) if e.needs_login() => return Err(e),
        Err(_) => Vec::new(),
    };

    stats.ms = started.elapsed().as_secs_f64() * 1000.0;
    Ok((
        Grades {
            total,
            weighted: totals.apply_assignment_group_weights == Some(true),
            groups,
            feedback,
        },
        stats,
    ))
}

/// Instructor comments on every assignment of the course, in one read. The
/// student's own comments are left out: they are not feedback.
async fn comments(
    client: &CanvasClient,
    course: &str,
) -> Result<(Vec<Feedback>, FetchStats), CanvasError> {
    let (list, stats): (Vec<CommentedSubmission>, _) = client
        .get_all_pages(
            &format!(
                "/api/v1/courses/{course}/students/submissions\
                 ?student_ids[]=self&include[]=submission_comments&per_page=100"
            ),
            10,
        )
        .await?;
    let mut feedback: Vec<Feedback> = Vec::new();
    for s in list {
        let Some(assignment_id) = s.assignment_id.filter(|id| valid_id(id)) else {
            continue;
        };
        let theirs: Vec<&Comment> = s
            .submission_comments
            .iter()
            .filter(|c| c.author_id.is_some() && c.author_id != s.user_id)
            .collect();
        if theirs.is_empty() {
            continue;
        }
        feedback.push(Feedback {
            assignment_id,
            count: theirs.len(),
            latest: theirs.iter().filter_map(|c| c.created_at).max(),
        });
    }
    feedback.sort_by(|a, b| {
        b.latest
            .cmp(&a.latest)
            .then_with(|| a.assignment_id.cmp(&b.assignment_id))
    });
    Ok((feedback, stats))
}

impl Grades {
    /// The newest instructor comment on an assignment, with how many there are.
    pub fn feedback_for(&self, assignment_id: &str) -> Option<&Feedback> {
        self.feedback
            .iter()
            .find(|f| f.assignment_id == assignment_id)
    }

    pub fn is_empty(&self) -> bool {
        self.groups.iter().all(|g| g.assignments.is_empty())
    }

    /// The course percentage this reader computes, with any hypothetical
    /// scores standing in. `None` when nothing counts towards it yet.
    ///
    /// Canvas's rules, as far as a reader can follow them: a weighted course
    /// scores each group on its own points and weights those percentages,
    /// leaving out groups with nothing graded and scaling the weights that
    /// remain back up to the whole, so a term whose first weeks hold only
    /// homework does not read as a fraction of the course. An unweighted
    /// course is every point earned over every point possible.
    ///
    /// This is arithmetic on what Canvas sent, not a number from Canvas. It
    /// can differ from the total Canvas shows — grading periods, dropped
    /// lowest scores and never-graded rules all live on Canvas's side — which
    /// is why the screen shows both and says which is which.
    pub fn computed(&self, guesses: &WhatIf) -> Option<f64> {
        let weighted = self.weighted
            && self
                .groups
                .iter()
                .any(|g| g.group_weight.unwrap_or(0.0) > 0.0);
        if weighted {
            let mut earned = 0.0;
            let mut counted = 0.0;
            for g in &self.groups {
                let weight = g.group_weight.unwrap_or(0.0);
                if weight <= 0.0 {
                    continue;
                }
                // A group of nothing but zero-point work has no percentage to
                // weigh, so it waits rather than counting as nought.
                let Some((score, possible)) = g.earned_with(guesses).filter(|(_, p)| *p > 0.0)
                else {
                    continue;
                };
                earned += weight * score / possible;
                counted += weight;
            }
            return (counted > 0.0).then(|| earned / counted * 100.0);
        }
        let mut score = 0.0;
        let mut possible = 0.0;
        for g in &self.groups {
            if let Some((s, p)) = g.earned_with(guesses) {
                score += s;
                possible += p;
            }
        }
        (possible > 0.0).then(|| score / possible * 100.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::tests::{FakeTransport, client, json};

    const GROUPS: &str = r#"[
      {"id":20,"name":"Exams","position":2,"group_weight":60.0,"assignments":[
        {"id":5,"name":"Midterm","points_possible":100,"due_at":"2026-10-10T23:59:00Z",
         "submission":{"score":88.0,"grade":"88","workflow_state":"graded"}}]},
      {"id":10,"name":"Homework","position":1,"group_weight":40.0,"assignments":[
        {"id":2,"name":"HW 2","points_possible":10,"due_at":"2026-10-20T23:59:00Z",
         "submission":{"score":9.0,"grade":"9","workflow_state":"graded","late":true}},
        {"id":1,"name":"HW 1","points_possible":10,"due_at":"2026-10-13T23:59:00Z",
         "submission":{"excused":true,"workflow_state":"graded"}},
        {"id":3,"name":"Survey","points_possible":5,"omit_from_final_grade":true,
         "submission":{"score":5.0,"workflow_state":"graded"}}]}]"#;

    const COURSE: &str = r#"{"id":1,"apply_assignment_group_weights":true,
      "enrollments":[{"type":"student","computed_current_score":91.5,"computed_current_grade":"A-"}]}"#;

    const COMMENTS: &str = r#"[
      {"assignment_id":5,"user_id":77,"submission_comments":[
        {"author_id":9,"created_at":"2026-10-12T15:00:00Z"},
        {"author_id":77,"created_at":"2026-10-13T15:00:00Z"},
        {"author_id":9,"created_at":"2026-10-14T15:00:00Z"}]},
      {"assignment_id":2,"user_id":77,"submission_comments":[
        {"author_id":77,"created_at":"2026-10-21T15:00:00Z"}]},
      {"assignment_id":1,"user_id":77,"submission_comments":[]}]"#;

    fn course_reads() -> std::sync::Arc<FakeTransport> {
        FakeTransport::new(vec![
            json(200, GROUPS),
            json(200, COURSE),
            json(200, COMMENTS),
        ])
    }

    #[tokio::test]
    async fn groups_follow_canvas_order_and_carry_weights() {
        let (grades, stats) = read(&client(course_reads()), "1").await.unwrap();
        assert_eq!(stats.requests, 3);
        let names: Vec<&str> = grades.groups.iter().map(|g| g.name()).collect();
        assert_eq!(names, ["Homework", "Exams"]);
        assert_eq!(grades.groups[0].group_weight, Some(40.0));
        assert!(grades.weighted);
        // Due date first, as everywhere else in the reader (the survey has none).
        let ids: Vec<&str> = grades.groups[0]
            .assignments
            .iter()
            .map(|a| a.id.as_str())
            .collect();
        assert_eq!(ids, ["1", "2", "3"]);
    }

    #[tokio::test]
    async fn group_points_leave_out_excused_and_omitted_work() {
        let (grades, _) = read(&client(course_reads()), "1").await.unwrap();
        // HW 1 is excused and the survey is omitted from the final grade, so
        // only HW 2 counts: 9 of 10.
        assert_eq!(grades.groups[0].earned_with(&WhatIf::new()), Some((9.0, 10.0)));
        assert_eq!(grades.groups[1].earned_with(&WhatIf::new()), Some((88.0, 100.0)));
    }

    #[tokio::test]
    async fn total_comes_from_the_student_enrolment() {
        let (grades, _) = read(&client(course_reads()), "1").await.unwrap();
        let total = grades.total.unwrap();
        assert_eq!(total.score, Some(91.5));
        assert_eq!(total.grade.as_deref(), Some("A-"));
    }

    #[tokio::test]
    async fn a_hidden_total_is_absent_rather_than_zero() {
        let t = FakeTransport::new(vec![
            json(200, GROUPS),
            json(200, r#"{"id":1,"enrollments":[{"type":"student"}]}"#),
            json(200, COMMENTS),
        ]);
        let (grades, _) = read(&client(t), "1").await.unwrap();
        assert!(grades.total.is_none());
        assert!(!grades.weighted);
        assert!(!grades.is_empty());
    }

    #[tokio::test]
    async fn only_instructor_comments_count_as_feedback() {
        let (grades, _) = read(&client(course_reads()), "1").await.unwrap();
        assert_eq!(grades.feedback.len(), 1);
        let f = grades.feedback_for("5").unwrap();
        assert_eq!(f.count, 2);
        assert_eq!(f.latest.unwrap().to_rfc3339(), "2026-10-14T15:00:00+00:00");
        // The student's own comment on HW 2 is not feedback.
        assert!(grades.feedback_for("2").is_none());
    }

    #[tokio::test]
    async fn grades_survive_a_course_that_refuses_the_comment_read() {
        let t = FakeTransport::new(vec![
            json(200, GROUPS),
            json(200, COURSE),
            json(403, r#"{"status":"unauthorized"}"#),
        ]);
        let (grades, _) = read(&client(t), "1").await.unwrap();
        assert!(grades.feedback.is_empty());
        assert_eq!(grades.groups.len(), 2);
    }

    #[tokio::test]
    async fn an_expired_session_is_not_mistaken_for_hidden_comments() {
        let t = FakeTransport::new(vec![
            json(200, GROUPS),
            json(200, COURSE),
            json(
                401,
                r#"{"errors":[{"message":"user authorization required"}]}"#,
            ),
        ]);
        assert_eq!(
            read(&client(t), "1").await.unwrap_err(),
            CanvasError::Unauthenticated
        );
    }

    #[tokio::test]
    async fn course_ids_are_checked() {
        let t = FakeTransport::new(vec![]);
        assert_eq!(
            read(&client(t), "../1").await.unwrap_err(),
            CanvasError::Invalid("invalid course ID")
        );
    }

    /// A course with two weighted groups, one of them not yet graded.
    fn weighted() -> Grades {
        Grades {
            total: None,
            weighted: true,
            groups: vec![
                Group {
                    id: "10".into(),
                    name: Some("Homework".into()),
                    group_weight: Some(40.0),
                    assignments: vec![graded("1", 10.0, Some(8.0))],
                    ..Default::default()
                },
                Group {
                    id: "20".into(),
                    name: Some("Final".into()),
                    group_weight: Some(60.0),
                    assignments: vec![graded("2", 100.0, None)],
                    ..Default::default()
                },
            ],
            feedback: vec![],
        }
    }

    fn graded(id: &str, points: f64, score: Option<f64>) -> AssignmentSummary {
        AssignmentSummary {
            id: id.into(),
            name: Some(format!("Work {id}")),
            points_possible: Some(points),
            submission: Some(crate::model::Submission {
                score,
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn guess(pairs: &[(&str, f64)]) -> WhatIf {
        pairs.iter().map(|(id, s)| (id.to_string(), *s)).collect()
    }

    /// Percentages are compared to the hundredth: the arithmetic is binary
    /// floating point, and a test that spells out all seventeen digits tests
    /// the spelling rather than the sum.
    #[track_caller]
    fn near(got: Option<f64>, want: f64) {
        let got = got.expect("a total");
        assert!((got - want).abs() < 0.005, "{got} is not about {want}");
    }

    #[test]
    fn a_weighted_total_counts_only_the_groups_with_something_in_them() {
        let grades = weighted();
        // Only homework is graded, so its 40% is the whole of what counts:
        // 8 of 10 is 80%, not 32% of a course nobody has sat the final for.
        assert_eq!(grades.computed(&WhatIf::new()), Some(80.0));
        // Guessing the final brings its 60% in: 0.4*80 + 0.6*90 = 86.
        assert_eq!(grades.computed(&guess(&[("2", 90.0)])), Some(86.0));
        // A guess of nothing is still an answer, not an absence.
        assert_eq!(grades.computed(&guess(&[("2", 0.0)])), Some(32.0));
    }

    #[test]
    fn an_unweighted_total_is_every_point_over_every_point_possible() {
        let mut grades = weighted();
        grades.weighted = false;
        assert_eq!(grades.computed(&WhatIf::new()), Some(80.0));
        // 8 + 45 of 10 + 100.
        near(grades.computed(&guess(&[("2", 45.0)])), 48.18);
        // Weights that exist are ignored where the course does not apply them.
        grades.groups[1].group_weight = Some(99.0);
        near(grades.computed(&guess(&[("2", 45.0)])), 48.18);
    }

    #[test]
    fn a_guess_counts_work_canvas_has_not_graded_or_has_excused() {
        let mut grades = weighted();
        grades.weighted = false;
        // Excused work is outside the total until it is guessed at, which is
        // how "what if this had counted" gets asked.
        grades.groups[1].assignments[0].submission = Some(crate::model::Submission {
            excused: Some(true),
            ..Default::default()
        });
        assert_eq!(grades.computed(&WhatIf::new()), Some(80.0));
        near(grades.computed(&guess(&[("2", 100.0)])), 98.18);
        // Work the instructor omits from the final grade cannot move it,
        // guessed or not.
        grades.groups[1].assignments[0].omit_from_final_grade = Some(true);
        assert_eq!(grades.computed(&guess(&[("2", 100.0)])), Some(80.0));
    }

    #[test]
    fn a_guess_replaces_the_score_in_its_group_too() {
        let grades = weighted();
        let group = &grades.groups[0];
        assert_eq!(group.earned_with(&WhatIf::new()), Some((8.0, 10.0)));
        assert_eq!(group.earned_with(&guess(&[("1", 10.0)])), Some((10.0, 10.0)));
        // Guessing at work in another group leaves this one alone.
        assert_eq!(group.earned_with(&guess(&[("2", 10.0)])), Some((8.0, 10.0)));
    }

    #[test]
    fn a_course_with_nothing_graded_has_no_total_to_compute() {
        let mut grades = weighted();
        grades.groups[0].assignments[0].submission = None;
        assert_eq!(grades.computed(&WhatIf::new()), None);
        grades.weighted = false;
        assert_eq!(grades.computed(&WhatIf::new()), None);
        // And a group of zero-point work is waited on rather than called nought.
        let mut zero = weighted();
        zero.groups[0].assignments[0].points_possible = Some(0.0);
        zero.groups[1].assignments[0].submission = Some(crate::model::Submission {
            score: Some(0.0),
            ..Default::default()
        });
        assert_eq!(zero.computed(&WhatIf::new()), Some(0.0));
    }

    #[tokio::test]
    async fn what_the_reader_computes_is_near_what_canvas_says() {
        // The fixture is 40% homework (9 of 10) and 60% exams (88 of 100),
        // which is 88.8; Canvas says 91.5 for the same course, because the
        // total it sends follows rules a reader cannot see. Both are shown.
        let (grades, _) = read(&client(course_reads()), "1").await.unwrap();
        near(grades.computed(&WhatIf::new()), 88.8);
        assert_eq!(grades.total.unwrap().score, Some(91.5));
    }
}
