use super::*;
use crate::node::test_support::{world, world_with};

fn run(world: &mut World, cmd: &str, exit_code: i32, output: &str) -> Option<ServerMsg> {
    world
        .observe(
            &Observation {
                cmd: cmd.into(),
                exit_code,
                cwd: "/home/learner/lab".into(),
                output: output.into(),
            },
            0,
        )
        .expect("observe")
}

fn state(
    msg: Option<ServerMsg>,
) -> (
    Option<LessonView>,
    Option<String>,
    Vec<ConceptView>,
    ActivityView,
) {
    let Some(ServerMsg::State {
        lesson,
        say,
        mastery,
        activity,
        ..
    }) = msg
    else {
        panic!("expected a state update");
    };
    (lesson, say, mastery, activity.expect("activity"))
}

fn complete_money_is_rows_cleanly(world: &mut World) {
    run(world, "bramble next", 0, "").expect("continued");
    run(world, "bramble ledger accounts", 0, "").expect("passed");
    run(world, "bramble next", 0, "").expect("continued");
    run(world, "bramble ledger postings customer:you", 0, "").expect("passed");
    run(
        world,
        "sqlite3 -readonly ledger.db 'SELECT SUM(...)'",
        0,
        "-7150\n",
    )
    .expect("passed");
    run(world, "bramble answer b", 0, "").expect("passed");
    run(world, "bramble next", 0, "").expect("completed");
}

#[test]
fn world_starts_with_money_is_rows_and_says_why() {
    let w = world();
    let (lesson, _, mastery, activity) = state(Some(w.snapshot(None, 0)));
    assert_eq!(lesson.expect("lesson").id, "money-is-rows");
    assert_eq!(activity.kind, ActivityKind::Lesson);
    assert!(activity.reason.contains("beginning"));
    assert_eq!(mastery.len(), 5);
}

#[test]
fn world_clean_lesson_masters_concept_and_fast_tracks_next() {
    let mut w = world();
    complete_money_is_rows_cleanly(&mut w);
    assert!(w.mastery.is_mastered("money-is-rows"));

    let (lesson, say, mastery, activity) = state(run(&mut w, "bramble next", 0, ""));
    let lesson = lesson.expect("next lesson");
    assert_eq!(lesson.id, "double-entry");
    assert!(say.expect("reason").contains("skipping the warm-up"));
    assert_eq!(activity.kind, ActivityKind::Lesson);
    let full_steps = w
        .lessons
        .iter()
        .find(|l| l.id == "double-entry")
        .expect("lesson")
        .steps
        .len();
    assert_eq!(lesson.step_count, full_steps - 1, "scaffold step skipped");
    assert!(
        mastery.iter().any(|c| c.id == "money-is-rows"
            && c.status == bramble_core::protocol::ConceptStatus::Mastered)
    );
}

#[test]
fn world_entry_exists_check_sees_entries_posted_through_the_ledger() {
    let mut w = world();
    complete_money_is_rows_cleanly(&mut w);
    run(&mut w, "bramble next", 0, "").expect("double-entry starts");
    run(&mut w, "bramble next", 0, "").expect("past the explanation");
    w.ledger
        .post(&NewEntry {
            memo: "Pay Alex".into(),
            idempotency_key: None,
            postings: vec![
                Posting::new("customer:you", 1200),
                Posting::new("customer:alex", -1200),
            ],
        })
        .expect("post via API path");
    let (lesson, ..) = state(run(
        &mut w,
        "curl -s -X POST localhost:8080/api/entries ...",
        0,
        "{}",
    ));
    assert_eq!(
        lesson.expect("lesson").step_id.as_deref(),
        Some("try-to-cheat")
    );
}

#[test]
fn world_setup_entries_are_planted_once_when_the_lesson_starts() {
    let mut w = world();
    let key = setup_entry_key("idempotency", "bookstore-charge");
    assert_eq!(
        w.ledger.entry_id_by_idempotency_key(&key).expect("lookup"),
        None,
        "not before the lesson"
    );
    let plan = ActivityPlan {
        kind: ActivityKind::Lesson,
        concept: Some("idempotency".into()),
        fast_track: false,
        remediation_streak: 0,
        reason: String::new(),
    };
    w.start_activity(plan.clone(), None).expect("start");
    let count = w.ledger.entry_count().expect("count");
    assert!(
        w.ledger
            .entry_id_by_idempotency_key(&key)
            .expect("lookup")
            .is_some()
    );
    w.start_activity(plan, None).expect("restart");
    assert_eq!(
        w.ledger.entry_count().expect("count"),
        count,
        "setup is idempotent"
    );
}

#[test]
fn world_struggling_learner_gets_remediation_with_a_reason() {
    let mut w = world();
    run(&mut w, "bramble next", 0, "");
    run(&mut w, "bramble hint", 0, "");
    run(&mut w, "bramble ledger accounts", 0, "");
    run(&mut w, "bramble next", 0, "");
    run(&mut w, "bramble hint", 0, "");
    run(&mut w, "bramble ledger postings customer:you", 0, "");
    run(&mut w, "sqlite3 x 'oops'", 1, "Parse error");
    run(&mut w, "sqlite3 x 'oops'", 1, "Parse error");
    run(&mut w, "sqlite3 x 'SELECT SUM'", 0, "-7150");
    run(&mut w, "bramble answer a", 0, "");
    run(&mut w, "bramble answer b", 0, "");
    run(&mut w, "bramble next", 0, "");
    assert!(!w.mastery.is_mastered("money-is-rows"));

    let (lesson, say, _, activity) = state(run(&mut w, "bramble next", 0, ""));
    assert_eq!(activity.kind, ActivityKind::Remediation);
    assert!(say.expect("reason").contains("isn't solid yet"));
    assert!(lesson.expect("practice").title.starts_with("Practice: "));
}

#[test]
fn world_logs_learning_events_and_restores_after_restart() {
    let mut w = world();
    run(&mut w, "bramble next", 0, "");
    run(&mut w, "bramble ledger accounts", 1, "boom");
    run(&mut w, "bramble ledger accounts", 0, "");
    let outcomes = w.store.event_outcomes().expect("events");
    assert_eq!(outcomes, vec!["continued", "stumbled", "passed_assisted"]);

    let World { store, ledger, .. } = w;
    let restored = world_with(store, ledger);
    assert_eq!(restored.progress.expect("progress").step, 2);
}
