//! Chooses what to teach next, and always says why.
//!
//! Rules, in priority order:
//! 1. Remediation: the concept just studied isn't mastered yet → practise it (at most twice in a row).
//! 2. Spaced review: a mastered concept's review is due → review it.
//! 3. Frontier: the first unstudied concept whose prerequisites are mastered → its lesson,
//!    skipping scaffold steps if the previous lesson was solved cleanly (fast-track).
//! 4. Revisit: an unmastered concept that's been studied before.
//! 5. Otherwise the track is complete.

use serde::{Deserialize, Serialize};

use crate::curriculum::{Concept, Curriculum};
use crate::lesson::{Lesson, Step};
use crate::mastery::MasteryMap;

pub const MAX_REMEDIATION_STREAK: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityKind {
    Lesson,
    Remediation,
    Review,
    TrackComplete,
}

/// What the learner is doing now. Persisted; the lesson itself is rebuilt from content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityPlan {
    pub kind: ActivityKind,
    pub concept: Option<String>,
    #[serde(default)]
    pub fast_track: bool,
    #[serde(default)]
    pub remediation_streak: u32,
    pub reason: String,
}

/// How the activity that just finished went.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivitySummary {
    pub concept: String,
    pub kind: ActivityKind,
    /// No hints, stumbles, or wrong answers anywhere in the activity.
    pub clean: bool,
    pub remediation_streak: u32,
}

pub fn choose_next(
    curriculum: &Curriculum,
    lessons: &[Lesson],
    mastery: &MasteryMap,
    last: Option<&ActivitySummary>,
    now_unix: i64,
) -> ActivityPlan {
    let title = |id: &str| {
        curriculum
            .concept(id)
            .map_or(id, |c| c.title.as_str())
            .to_owned()
    };
    let plan = |kind, concept: &str, reason: String| ActivityPlan {
        kind,
        concept: Some(concept.to_owned()),
        fast_track: false,
        remediation_streak: 0,
        reason,
    };

    if let Some(last) = last
        && matches!(last.kind, ActivityKind::Lesson | ActivityKind::Remediation)
        && !mastery.is_mastered(&last.concept)
        && last.remediation_streak < MAX_REMEDIATION_STREAK
    {
        let percent = mastery.p_known(&last.concept) * 100.0;
        return ActivityPlan {
            remediation_streak: last.remediation_streak + 1,
            ..plan(
                ActivityKind::Remediation,
                &last.concept,
                format!(
                    "{} isn't solid yet (I'm about {percent:.0}% sure you've got it). \
                     Let's practise once more before moving on.",
                    title(&last.concept)
                ),
            )
        };
    }

    let due = curriculum
        .concepts()
        .iter()
        .filter(|c| mastery.review_due(&c.id, now_unix))
        .min_by_key(|c| {
            mastery
                .get(&c.id)
                .and_then(|m| m.review)
                .map(|r| r.due_unix)
        });
    if let Some(concept) = due {
        return plan(
            ActivityKind::Review,
            &concept.id,
            format!(
                "It's been a while since {}. A quick review now keeps it from fading. \
                 That's spaced repetition.",
                concept.title
            ),
        );
    }

    let unlocked = |c: &Concept| c.prereqs.iter().all(|p| mastery.is_mastered(p));
    let fresh: Vec<&Concept> = curriculum
        .concepts()
        .iter()
        .filter(|c| !mastery.is_mastered(&c.id) && !mastery.is_studied(&c.id) && unlocked(c))
        .collect();
    if let Some(next) = fresh.first() {
        // Only claim a fast-track when there is warm-up to skip.
        let has_scaffold = lessons
            .iter()
            .filter(|l| l.concept == next.id)
            .flat_map(|l| &l.steps)
            .any(|step| matches!(step, Step::Task(task) if task.scaffold));
        let fast_track =
            has_scaffold && last.is_some_and(|l| l.clean && l.kind == ActivityKind::Lesson);
        let mut reason = match (last, fast_track) {
            (None, _) => format!("Let's start at the very beginning: {}.", next.title),
            (_, true) => format!(
                "You flew through that without a single hint, so I'm skipping the warm-up steps in {}.",
                next.title
            ),
            _ if next.prereqs.is_empty() => format!("Next up: {}.", next.title),
            _ => format!(
                "Next up: {}. It builds on what you've mastered: {}.",
                next.title,
                next.prereqs
                    .iter()
                    .map(|p| title(p))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        };
        if fresh.len() > 1 {
            let others: Vec<&str> = fresh[1..].iter().map(|c| c.title.as_str()).collect();
            reason.push_str(&format!(" (Also unlocked: {}.)", others.join(", ")));
        }
        return ActivityPlan {
            fast_track,
            ..plan(ActivityKind::Lesson, &next.id, reason)
        };
    }

    let revisit = curriculum
        .concepts()
        .iter()
        .find(|c| !mastery.is_mastered(&c.id) && unlocked(c));
    if let Some(concept) = revisit {
        return plan(
            ActivityKind::Remediation,
            &concept.id,
            format!(
                "Everything else builds on {}, so let's revisit it before anything new.",
                concept.title
            ),
        );
    }

    ActivityPlan {
        kind: ActivityKind::TrackComplete,
        concept: None,
        fast_track: false,
        remediation_streak: 0,
        reason: "That's the whole track! I'll call you back when a review comes due.".to_owned(),
    }
}

/// Builds the lesson for a plan from shipped content. `None` means nothing to do.
pub fn build_lesson(
    plan: &ActivityPlan,
    curriculum: &Curriculum,
    lessons: &[Lesson],
) -> Option<Lesson> {
    let concept = plan.concept.as_deref()?;
    let lesson = lessons.iter().find(|l| l.concept == concept)?;
    let title = curriculum
        .concept(concept)
        .map_or(lesson.title.as_str(), |c| c.title.as_str());
    match plan.kind {
        ActivityKind::Lesson => {
            let mut lesson = lesson.clone();
            if plan.fast_track {
                lesson
                    .steps
                    .retain(|step| !matches!(step, Step::Task(task) if task.scaffold));
            }
            Some(lesson)
        }
        ActivityKind::Remediation | ActivityKind::Review => {
            let label = if plan.kind == ActivityKind::Review {
                "Review"
            } else {
                "Practice"
            };
            Some(Lesson {
                id: format!("{}:{}", label.to_lowercase(), lesson.id),
                title: format!("{label}: {title}"),
                concept: concept.to_owned(),
                sources: lesson.sources.clone(),
                steps: lesson.review.iter().cloned().map(Step::Task).collect(),
                review: Vec::new(),
                setup: Default::default(),
            })
        }
        ActivityKind::TrackComplete => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::curriculum::Concept;

    fn curriculum() -> Curriculum {
        let concept = |id: &str, title: &str, prereqs: &[&str]| Concept {
            id: id.into(),
            title: title.into(),
            track: "ledgers".into(),
            prereqs: prereqs.iter().map(|p| (*p).to_owned()).collect(),
            objectives: vec![],
        };
        Curriculum::new(vec![
            concept("rows", "Money is rows", &[]),
            concept("double", "Double-entry", &["rows"]),
            concept("hash", "Hashing", &["rows"]),
            concept("idem", "Idempotency", &["double"]),
        ])
        .expect("curriculum")
    }

    /// One lesson per concept; only "double" has a scaffold (warm-up) step.
    fn lessons() -> Vec<Lesson> {
        ["rows", "double", "hash", "idem"]
            .iter()
            .map(|concept| {
                let scaffold = *concept == "double";
                Lesson::from_toml(&format!(
                    r#"
                    id = "{concept}"
                    title = "{concept}"
                    concept = "{concept}"
                    [[steps]]
                    kind = "task"
                    id = "t"
                    prompt = "p"
                    scaffold = {scaffold}
                    check = {{ type = "quiz_answer", answer = "a" }}
                    "#
                ))
                .expect("lesson")
            })
            .collect()
    }

    fn master(map: &mut MasteryMap, concept: &str) {
        map.observe(concept, true, 0);
        map.observe(concept, true, 0);
    }

    fn summary(concept: &str, clean: bool) -> ActivitySummary {
        ActivitySummary {
            concept: concept.into(),
            kind: ActivityKind::Lesson,
            clean,
            remediation_streak: 0,
        }
    }

    #[test]
    fn teacher_choose_next_starts_at_the_root() {
        let plan = choose_next(&curriculum(), &lessons(), &MasteryMap::default(), None, 0);
        assert_eq!(plan.kind, ActivityKind::Lesson);
        assert_eq!(plan.concept.as_deref(), Some("rows"));
        assert!(plan.reason.contains("beginning"));
    }

    #[test]
    fn teacher_choose_next_advances_frontier_and_names_alternatives() {
        let mut mastery = MasteryMap::default();
        master(&mut mastery, "rows");
        let plan = choose_next(
            &curriculum(),
            &lessons(),
            &mastery,
            Some(&summary("rows", false)),
            0,
        );
        assert_eq!(plan.concept.as_deref(), Some("double"));
        assert!(!plan.fast_track);
        assert!(plan.reason.contains("Money is rows"), "{}", plan.reason);
        assert!(
            plan.reason.contains("Also unlocked: Hashing"),
            "{}",
            plan.reason
        );
    }

    #[test]
    fn teacher_choose_next_fast_tracks_after_a_clean_lesson() {
        let mut mastery = MasteryMap::default();
        master(&mut mastery, "rows");
        let plan = choose_next(
            &curriculum(),
            &lessons(),
            &mastery,
            Some(&summary("rows", true)),
            0,
        );
        assert!(plan.fast_track);
        assert!(plan.reason.contains("skipping the warm-up"));
    }

    #[test]
    fn teacher_choose_next_never_claims_fast_track_without_warm_up_steps() {
        let mut mastery = MasteryMap::default();
        master(&mut mastery, "rows");
        master(&mut mastery, "double");
        let last = ActivitySummary {
            concept: "double".into(),
            ..summary("double", true)
        };
        let plan = choose_next(&curriculum(), &lessons(), &mastery, Some(&last), 0);
        assert_eq!(plan.concept.as_deref(), Some("hash"));
        assert!(!plan.fast_track, "hash has no scaffold steps to skip");
        assert!(!plan.reason.contains("skipping"), "{}", plan.reason);
    }

    #[test]
    fn teacher_choose_next_remediates_unmastered_concept_up_to_the_cap() {
        let mut mastery = MasteryMap::default();
        mastery.observe("rows", false, 0);
        let plan = choose_next(
            &curriculum(),
            &lessons(),
            &mastery,
            Some(&summary("rows", false)),
            0,
        );
        assert_eq!(plan.kind, ActivityKind::Remediation);
        assert_eq!(plan.remediation_streak, 1);
        assert!(plan.reason.contains("isn't solid yet"));

        let capped = ActivitySummary {
            kind: ActivityKind::Remediation,
            remediation_streak: MAX_REMEDIATION_STREAK,
            ..summary("rows", false)
        };
        let plan = choose_next(&curriculum(), &lessons(), &mastery, Some(&capped), 0);
        assert_eq!(
            plan.kind,
            ActivityKind::Remediation,
            "nothing else is unlocked yet"
        );
        assert_eq!(
            plan.remediation_streak, 0,
            "cap reached: falls through to revisit"
        );
        assert!(plan.reason.contains("revisit"));
    }

    #[test]
    fn teacher_choose_next_prefers_due_reviews_over_new_material() {
        let mut mastery = MasteryMap::default();
        master(&mut mastery, "rows");
        let day = 24 * 3600;
        let plan = choose_next(&curriculum(), &lessons(), &mastery, None, day);
        assert_eq!(plan.kind, ActivityKind::Review);
        assert!(plan.reason.contains("spaced repetition"));
        let plan = choose_next(&curriculum(), &lessons(), &mastery, None, day - 1);
        assert_eq!(plan.kind, ActivityKind::Lesson, "not due yet");
    }

    #[test]
    fn teacher_choose_next_reports_track_complete() {
        let mut mastery = MasteryMap::default();
        for concept in ["rows", "double", "hash", "idem"] {
            master(&mut mastery, concept);
        }
        let plan = choose_next(&curriculum(), &lessons(), &mastery, None, 0);
        assert_eq!(plan.kind, ActivityKind::TrackComplete);
        assert_eq!(plan.concept, None);
    }

    #[test]
    fn teacher_build_lesson_strips_scaffold_and_builds_review_sessions() {
        let (curriculum, lessons) = crate::curriculum::load_shipped_content();
        let concept = lessons
            .iter()
            .find(|l| {
                l.steps
                    .iter()
                    .any(|s| matches!(s, Step::Task(t) if t.scaffold))
            })
            .map(|l| l.concept.clone())
            .expect("shipped content has at least one scaffold step");
        let plan = |kind, fast_track| ActivityPlan {
            kind,
            concept: Some(concept.clone()),
            fast_track,
            remediation_streak: 0,
            reason: String::new(),
        };
        let full = build_lesson(&plan(ActivityKind::Lesson, false), &curriculum, &lessons)
            .expect("lesson");
        let fast =
            build_lesson(&plan(ActivityKind::Lesson, true), &curriculum, &lessons).expect("lesson");
        assert!(fast.steps.len() < full.steps.len());

        let review = build_lesson(&plan(ActivityKind::Review, false), &curriculum, &lessons)
            .expect("review");
        assert!(review.id.starts_with("review:"));
        assert!(review.title.starts_with("Review: "));
        assert!(review.steps.len() >= crate::curriculum::MIN_REVIEW_TASKS);
        assert!(
            review.setup.entries.is_empty(),
            "reviews never re-run setup"
        );
    }
}
