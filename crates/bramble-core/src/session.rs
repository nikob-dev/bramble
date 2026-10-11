//! The lesson session state machine: turns lab observations into progress.

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::lesson::{Check, Expect, Lesson, PostingSpec, Step, Task};

/// After this many stumbles on one step, the pet shows a worked example (or the hint).
pub const ESCALATE_AFTER_STUMBLES: u32 = 2;

/// One command the learner ran in the lab shell, as reported by the TUI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Observation {
    pub cmd: String,
    pub exit_code: i32,
    pub cwd: String,
    pub output: String,
}

/// Live facts the session may check against (the pet-bank's ledger and published files).
pub trait Facts {
    fn balance(&self, account: &str) -> Option<i64>;
    fn max_entry_id(&self) -> i64;
    fn entry_exists_since(&self, since_entry_id: i64, postings: &[PostingSpec]) -> bool;
    /// The entry id posted by setup under idempotency key `key`.
    fn setup_entry_id(&self, key: &str) -> Option<i64>;
    /// A published value such as `sha256:batch-001.csv`.
    fn fact(&self, name: &str) -> Option<String>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LessonProgress {
    pub lesson_id: String,
    pub step: usize,
    pub hint_revealed: bool,
    #[serde(default)]
    pub worked_example_revealed: bool,
    pub hints_used: u32,
    #[serde(default, alias = "wrong_answers")]
    pub stumbles: u32,
    #[serde(default)]
    pub step_stumbles: u32,
    pub completed: bool,
    /// Ledger high-water mark when the current step began.
    #[serde(default)]
    pub watermark: i64,
}

impl LessonProgress {
    pub fn start(lesson: &Lesson, watermark: i64) -> Self {
        Self {
            lesson_id: lesson.id.clone(),
            step: 0,
            hint_revealed: false,
            worked_example_revealed: false,
            hints_used: 0,
            stumbles: 0,
            step_stumbles: 0,
            completed: false,
            watermark,
        }
    }

    /// No hints, stumbles, or wrong answers anywhere in the lesson so far.
    pub fn is_clean(&self) -> bool {
        self.hints_used == 0 && self.stumbles == 0
    }

    fn step_is_clean(&self) -> bool {
        !self.hint_revealed && !self.worked_example_revealed && self.step_stumbles == 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Moved past an explanation.
    Continued,
    /// A task's check passed. `clean` = first try, no help.
    Passed {
        task_id: String,
        success: Option<String>,
        clean: bool,
    },
    HintRevealed,
    NoHint,
    /// A command aimed at the task (matched its command pattern) that didn't satisfy the check.
    Stumbled,
    WrongAnswer,
    /// Repeated stumbles: the pet stepped in with a worked example (or the hint).
    Escalated {
        worked_example: bool,
    },
    /// `bramble next` on a task that isn't solved yet.
    TaskPending,
    Completed,
}

enum Control<'a> {
    Next,
    Hint,
    Answer(&'a str),
}

fn control(cmd: &str) -> Option<Control<'_>> {
    let mut words = cmd.split_whitespace();
    if words.next() != Some("bramble") {
        return None;
    }
    match words.next()? {
        "next" => Some(Control::Next),
        "hint" => Some(Control::Hint),
        "answer" => Some(Control::Answer(words.next().unwrap_or(""))),
        _ => None,
    }
}

/// Applies one observation to the session. Commands unrelated to the current
/// step produce no outcomes: exploring the lab is never penalised.
pub fn observe(
    lesson: &Lesson,
    progress: &LessonProgress,
    obs: &Observation,
    facts: &dyn Facts,
) -> (LessonProgress, Vec<Outcome>) {
    let mut next = progress.clone();
    let Some(step) = lesson
        .steps
        .get(progress.step)
        .filter(|_| !progress.completed)
    else {
        return (next, Vec::new());
    };

    let mut outcomes = match (step, control(&obs.cmd)) {
        (Step::Explain { .. }, Some(Control::Next)) => vec![Outcome::Continued],
        (Step::Explain { .. }, _) => Vec::new(),
        (Step::Task(_), Some(Control::Next)) => vec![Outcome::TaskPending],
        (Step::Task(task), Some(Control::Hint)) => match task.hint {
            Some(_) if !progress.hint_revealed => {
                next.hint_revealed = true;
                next.hints_used += 1;
                vec![Outcome::HintRevealed]
            }
            Some(_) => Vec::new(),
            None => vec![Outcome::NoHint],
        },
        (Step::Task(task), Some(Control::Answer(given))) => {
            match answer_is_correct(&task.check, given, facts) {
                Some(true) => vec![passed(task, progress)],
                Some(false) => stumble(&mut next, task, Outcome::WrongAnswer),
                None => Vec::new(),
            }
        }
        (Step::Task(task), None) => {
            if check_passes(task, obs, progress.watermark, facts) {
                vec![passed(task, progress)]
            } else if task
                .check
                .command_pattern()
                .is_some_and(|pattern| regex_matches(pattern, &obs.cmd))
            {
                stumble(&mut next, task, Outcome::Stumbled)
            } else {
                Vec::new()
            }
        }
    };

    if matches!(
        outcomes.first(),
        Some(Outcome::Continued | Outcome::Passed { .. })
    ) {
        next.step += 1;
        next.hint_revealed = false;
        next.worked_example_revealed = false;
        next.step_stumbles = 0;
        next.watermark = facts.max_entry_id();
        if next.step >= lesson.steps.len() {
            next.completed = true;
            outcomes.push(Outcome::Completed);
        }
    }
    (next, outcomes)
}

fn passed(task: &Task, progress: &LessonProgress) -> Outcome {
    Outcome::Passed {
        task_id: task.id.clone(),
        success: task.success.clone(),
        clean: progress.step_is_clean(),
    }
}

fn stumble(next: &mut LessonProgress, task: &Task, outcome: Outcome) -> Vec<Outcome> {
    next.stumbles += 1;
    next.step_stumbles += 1;
    let mut outcomes = vec![outcome];
    if next.step_stumbles == ESCALATE_AFTER_STUMBLES {
        if task.worked_example.is_some() && !next.worked_example_revealed {
            next.worked_example_revealed = true;
            outcomes.push(Outcome::Escalated {
                worked_example: true,
            });
        } else if task.hint.is_some() && !next.hint_revealed {
            next.hint_revealed = true;
            next.hints_used += 1;
            outcomes.push(Outcome::Escalated {
                worked_example: false,
            });
        }
    }
    outcomes
}

/// `None` when the check isn't answered with `bramble answer`.
fn answer_is_correct(check: &Check, given: &str, facts: &dyn Facts) -> Option<bool> {
    match check {
        Check::QuizAnswer { answer } => Some(given.eq_ignore_ascii_case(answer)),
        Check::AnswerEntry { setup_key } => {
            let expected = facts.setup_entry_id(&format!("setup:{setup_key}"));
            Some(
                expected.is_some() && given.trim_start_matches('#').parse::<i64>().ok() == expected,
            )
        }
        _ => None,
    }
}

fn check_passes(task: &Task, obs: &Observation, watermark: i64, facts: &dyn Facts) -> bool {
    let exit_ok = match task.expect {
        Expect::Success => obs.exit_code == 0,
        Expect::Failure => obs.exit_code != 0,
    };
    if !exit_ok {
        return false;
    }
    let command_ok =
        |command: &Option<String>| command.as_ref().is_none_or(|p| regex_matches(p, &obs.cmd));
    match &task.check {
        Check::CommandRan { pattern } => regex_matches(pattern, &obs.cmd),
        Check::OutputMatches { pattern, command } => {
            command_ok(command) && regex_matches(pattern, &obs.output)
        }
        Check::LedgerBalance { account, command } => {
            command_ok(command)
                && facts
                    .balance(account)
                    .is_some_and(|balance| output_contains_amount(&obs.output, balance))
        }
        Check::OutputContainsFact { fact, command } => {
            command_ok(command)
                && facts
                    .fact(fact)
                    .is_some_and(|value| obs.output.contains(&value))
        }
        Check::EntryExists { postings } => facts.entry_exists_since(watermark, postings),
        Check::QuizAnswer { .. } | Check::AnswerEntry { .. } => false,
    }
}

fn regex_matches(pattern: &str, haystack: &str) -> bool {
    // Patterns are validated when lessons load, so a failure here means a logic bug upstream.
    Regex::new(pattern).is_ok_and(|re| re.is_match(haystack))
}

/// True if `output` contains `minor` as a standalone number, either as minor units
/// (`-7150`) or as a decimal major-unit amount (`-71.50`), with or without the sign.
fn output_contains_amount(output: &str, minor: i64) -> bool {
    let abs = minor.unsigned_abs();
    let candidates = [abs.to_string(), format!("{}.{:02}", abs / 100, abs % 100)];
    candidates.iter().any(|candidate| {
        let pattern = format!(r"(^|[^0-9.]){}($|[^0-9])", regex::escape(candidate));
        regex_matches(&pattern, output)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Bank {
        max_entry: i64,
        entry_after: Option<i64>,
    }

    impl Facts for Bank {
        fn balance(&self, _account: &str) -> Option<i64> {
            Some(-7150)
        }
        fn max_entry_id(&self) -> i64 {
            self.max_entry
        }
        fn entry_exists_since(&self, since: i64, _postings: &[PostingSpec]) -> bool {
            self.entry_after.is_some_and(|id| id > since)
        }
        fn setup_entry_id(&self, key: &str) -> Option<i64> {
            (key == "setup:recon:gas").then_some(42)
        }
        fn fact(&self, name: &str) -> Option<String> {
            (name == "sha256:batch.csv").then(|| "abc123".to_owned())
        }
    }

    fn lesson() -> Lesson {
        Lesson::from_toml(
            r#"
            id = "money-is-rows"
            title = "Money is rows"
            concept = "money-is-rows"
            [[steps]]
            kind = "explain"
            text = "intro"
            [[steps]]
            kind = "task"
            id = "accounts"
            prompt = "list accounts"
            hint = "try bramble ledger accounts"
            check = { type = "command_ran", pattern = "^bramble ledger accounts" }
            [[steps]]
            kind = "task"
            id = "sum"
            prompt = "sum it"
            worked_example = "sqlite3 ... SELECT SUM(...)"
            check = { type = "ledger_balance", account = "customer:you", command = "^sqlite3" }
            [[steps]]
            kind = "task"
            id = "quiz"
            prompt = "quiz"
            hint = "think liabilities"
            check = { type = "quiz_answer", answer = "b" }
            "#,
        )
        .expect("lesson")
    }

    fn obs(cmd: &str, exit_code: i32, output: &str) -> Observation {
        Observation {
            cmd: cmd.into(),
            exit_code,
            cwd: "/home/learner/lab".into(),
            output: output.into(),
        }
    }

    fn at(lesson: &Lesson, step: usize) -> LessonProgress {
        LessonProgress {
            step,
            ..LessonProgress::start(lesson, 0)
        }
    }

    fn run(
        lesson: &Lesson,
        progress: LessonProgress,
        cmd: &str,
        exit: i32,
        out: &str,
    ) -> (LessonProgress, Vec<Outcome>) {
        observe(lesson, &progress, &obs(cmd, exit, out), &Bank::default())
    }

    #[test]
    fn session_observe_walks_through_whole_lesson() {
        let lesson = lesson();
        let p = LessonProgress::start(&lesson, 0);
        let (p, o) = run(&lesson, p, "bramble next", 0, "");
        assert_eq!(o, vec![Outcome::Continued]);
        let (p, o) = run(&lesson, p, "bramble ledger accounts", 0, "...");
        assert!(
            matches!(&o[..], [Outcome::Passed { clean: true, task_id, .. }] if task_id == "accounts")
        );
        let (p, o) = run(
            &lesson,
            p,
            "bramble ledger accounts",
            0,
            "customer:you -7150\n",
        );
        assert!(
            o.is_empty(),
            "right number from the wrong command doesn't count"
        );
        let (p, o) = run(
            &lesson,
            p,
            "sqlite3 ledger.db 'select sum(...)'",
            0,
            "-7150\n",
        );
        assert!(matches!(o[..], [Outcome::Passed { clean: true, .. }]));
        let (p, o) = run(&lesson, p, "bramble answer a", 0, "");
        assert_eq!(o, vec![Outcome::WrongAnswer]);
        let (p, o) = run(&lesson, p, "bramble answer B", 0, "");
        assert!(matches!(
            o[..],
            [Outcome::Passed { clean: false, .. }, Outcome::Completed]
        ));
        assert!(p.completed);
        assert!(!p.is_clean());
        let (_, o) = run(&lesson, p, "bramble next", 0, "");
        assert!(o.is_empty(), "completed lessons ignore further input");
    }

    #[test]
    fn session_observe_ignores_unrelated_commands_but_flags_stumbles() {
        let lesson = lesson();
        let (_, o) = run(&lesson, at(&lesson, 1), "ls -la", 0, "");
        assert!(o.is_empty());
        let (p, o) = run(&lesson, at(&lesson, 1), "bramble ledger accounts", 1, "");
        assert_eq!(o, vec![Outcome::Stumbled], "an attempt that failed");
        assert_eq!(p.stumbles, 1);
    }

    #[test]
    fn session_observe_escalates_to_worked_example_after_two_stumbles() {
        let lesson = lesson();
        let p = at(&lesson, 2);
        let (p, o) = run(&lesson, p, "sqlite3 ledger.db 'oops'", 1, "Parse error");
        assert_eq!(o, vec![Outcome::Stumbled]);
        let (p, o) = run(&lesson, p, "sqlite3 ledger.db 'select 1'", 0, "1");
        assert_eq!(
            o,
            vec![
                Outcome::Stumbled,
                Outcome::Escalated {
                    worked_example: true
                }
            ]
        );
        assert!(p.worked_example_revealed);
        let (p, o) = run(&lesson, p, "sqlite3 ledger.db 'select sum'", 0, "-7150");
        assert!(matches!(o[..], [Outcome::Passed { clean: false, .. }]));
        assert!(!p.worked_example_revealed, "resets on the next step");
    }

    #[test]
    fn session_observe_escalates_to_hint_when_no_worked_example() {
        let lesson = lesson();
        let (p, _) = run(&lesson, at(&lesson, 3), "bramble answer a", 0, "");
        let (p, o) = run(&lesson, p, "bramble answer c", 0, "");
        assert_eq!(
            o,
            vec![
                Outcome::WrongAnswer,
                Outcome::Escalated {
                    worked_example: false
                }
            ]
        );
        assert!(p.hint_revealed);
        assert_eq!(p.hints_used, 1);
    }

    #[test]
    fn session_observe_hint_is_counted_once_per_step() {
        let lesson = lesson();
        let (p, o) = run(&lesson, at(&lesson, 1), "bramble hint", 0, "");
        assert_eq!(o, vec![Outcome::HintRevealed]);
        let (p, o) = run(&lesson, p, "bramble hint", 0, "");
        assert!(o.is_empty());
        assert_eq!(p.hints_used, 1);
        let (p, o) = run(&lesson, p, "bramble ledger accounts", 0, "");
        assert!(
            matches!(o[..], [Outcome::Passed { clean: false, .. }]),
            "hinted ≠ clean"
        );
        assert!(!p.hint_revealed, "hint resets on the next step");
        let (_, o) = run(&lesson, p, "bramble hint", 0, "");
        assert_eq!(o, vec![Outcome::NoHint]);
    }

    #[test]
    fn session_observe_next_on_unsolved_task_is_pending() {
        let lesson = lesson();
        let p = at(&lesson, 1);
        let (p2, o) = run(&lesson, p.clone(), "bramble next", 0, "");
        assert_eq!(o, vec![Outcome::TaskPending]);
        assert_eq!(p2, p);
    }

    fn single_task(check: &str, expect: &str) -> Lesson {
        Lesson::from_toml(&format!(
            r#"
            id = "t"
            title = "T"
            concept = "c"
            [[steps]]
            kind = "task"
            id = "only"
            prompt = "p"
            expect = "{expect}"
            check = {check}
            "#
        ))
        .expect("lesson")
    }

    #[test]
    fn session_observe_expect_failure_requires_nonzero_exit() {
        let lesson = single_task(
            r#"{ type = "output_matches", pattern = "unbalanced entry", command = "^sqlite3" }"#,
            "failure",
        );
        let p = LessonProgress::start(&lesson, 0);
        let (_, o) = run(
            &lesson,
            p.clone(),
            "sqlite3 scratch.db '...'",
            0,
            "unbalanced entry",
        );
        assert_eq!(
            o,
            vec![Outcome::Stumbled],
            "the database was supposed to refuse"
        );
        let (_, o) = run(
            &lesson,
            p,
            "sqlite3 scratch.db '...'",
            1,
            "Error: unbalanced entry",
        );
        assert!(matches!(
            o[..],
            [Outcome::Passed { .. }, Outcome::Completed]
        ));
    }

    #[test]
    fn session_observe_entry_exists_respects_watermark() {
        let lesson = single_task(
            r#"{ type = "entry_exists", postings = [{ account = "a", amount_minor = 1 }, { account = "b", amount_minor = -1 }] }"#,
            "success",
        );
        let p = LessonProgress::start(&lesson, 10);
        let old = Bank {
            max_entry: 10,
            entry_after: Some(10),
        };
        let (_, o) = observe(&lesson, &p, &obs("curl ...", 0, ""), &old);
        assert!(o.is_empty(), "entries from before the step don't count");
        let new = Bank {
            max_entry: 11,
            entry_after: Some(11),
        };
        let (p, o) = observe(&lesson, &p, &obs("curl ...", 0, ""), &new);
        assert!(matches!(
            o[..],
            [Outcome::Passed { .. }, Outcome::Completed]
        ));
        assert_eq!(p.watermark, 11);
    }

    #[test]
    fn session_observe_answer_entry_and_fact_checks() {
        let lesson = single_task(
            r#"{ type = "answer_entry", setup_key = "recon:gas" }"#,
            "success",
        );
        let p = LessonProgress::start(&lesson, 0);
        let (_, o) = run(&lesson, p.clone(), "bramble answer 41", 0, "");
        assert_eq!(o, vec![Outcome::WrongAnswer]);
        let (_, o) = run(&lesson, p, "bramble answer #42", 0, "");
        assert!(matches!(o[0], Outcome::Passed { .. }));

        let lesson = single_task(
            r#"{ type = "output_contains_fact", fact = "sha256:batch.csv", command = "sha256sum" }"#,
            "success",
        );
        let p = LessonProgress::start(&lesson, 0);
        let (_, o) = run(
            &lesson,
            p.clone(),
            "sha256sum batch.csv",
            0,
            "fff  batch.csv",
        );
        assert_eq!(o, vec![Outcome::Stumbled], "file was changed: hash differs");
        let (_, o) = run(&lesson, p, "sha256sum batch.csv", 0, "abc123  batch.csv");
        assert!(matches!(o[0], Outcome::Passed { .. }));
    }

    #[test]
    fn output_contains_amount_matches_minor_and_decimal_forms() {
        assert!(output_contains_amount("-7150", -7150));
        assert!(output_contains_amount("balance: 7150\n", -7150));
        assert!(output_contains_amount("-71.50", -7150));
        assert!(!output_contains_amount("71500", -7150));
        assert!(!output_contains_amount("17150", -7150));
        assert!(!output_contains_amount("71.5", -7150));
    }
}
