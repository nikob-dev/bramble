//! The lesson session state machine: turns lab observations into progress.

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::lesson::{Check, Lesson, Step};

/// One command the learner ran in the lab shell, as reported by the TUI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Observation {
    pub cmd: String,
    pub exit_code: i32,
    pub cwd: String,
    pub output: String,
}

/// Live facts the session may check against (e.g. the pet-bank's ledger).
pub trait Facts {
    fn balance(&self, account: &str) -> Option<i64>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LessonProgress {
    pub lesson_id: String,
    pub step: usize,
    pub hint_revealed: bool,
    pub hints_used: u32,
    pub wrong_answers: u32,
    pub completed: bool,
}

impl LessonProgress {
    pub fn start(lesson: &Lesson) -> Self {
        Self {
            lesson_id: lesson.id.clone(),
            step: 0,
            hint_revealed: false,
            hints_used: 0,
            wrong_answers: 0,
            completed: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Moved past an explanation.
    Continued,
    /// A task's check passed.
    Passed { success: Option<String> },
    HintRevealed,
    NoHint,
    WrongAnswer,
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

/// Applies one observation to the session. Commands that don't relate to the
/// current step produce no outcomes: exploring the lab is never penalised.
pub fn observe(
    lesson: &Lesson,
    progress: &LessonProgress,
    obs: &Observation,
    facts: &dyn Facts,
) -> (LessonProgress, Vec<Outcome>) {
    let mut next = progress.clone();
    let Some(step) = lesson.steps.get(progress.step).filter(|_| !progress.completed) else {
        return (next, Vec::new());
    };
    let control = control(&obs.cmd);

    let mut outcomes = match (step, control) {
        (Step::Explain { .. }, Some(Control::Next)) => vec![Outcome::Continued],
        (Step::Explain { .. }, _) => Vec::new(),
        (Step::Task { .. }, Some(Control::Next)) => vec![Outcome::TaskPending],
        (Step::Task { hint, .. }, Some(Control::Hint)) => match hint {
            Some(_) if !progress.hint_revealed => {
                next.hint_revealed = true;
                next.hints_used += 1;
                vec![Outcome::HintRevealed]
            }
            Some(_) => Vec::new(),
            None => vec![Outcome::NoHint],
        },
        (
            Step::Task {
                check: Check::QuizAnswer { answer },
                success,
                ..
            },
            Some(Control::Answer(given)),
        ) => {
            if given.eq_ignore_ascii_case(answer) {
                vec![Outcome::Passed {
                    success: success.clone(),
                }]
            } else {
                next.wrong_answers += 1;
                vec![Outcome::WrongAnswer]
            }
        }
        (Step::Task { check, success, .. }, None) if check_passes(check, obs, facts) => {
            vec![Outcome::Passed {
                success: success.clone(),
            }]
        }
        (Step::Task { .. }, _) => Vec::new(),
    };

    if matches!(
        outcomes.first(),
        Some(Outcome::Continued | Outcome::Passed { .. })
    ) {
        next.step += 1;
        next.hint_revealed = false;
        if next.step >= lesson.steps.len() {
            next.completed = true;
            outcomes.push(Outcome::Completed);
        }
    }
    (next, outcomes)
}

fn check_passes(check: &Check, obs: &Observation, facts: &dyn Facts) -> bool {
    if obs.exit_code != 0 {
        return false;
    }
    match check {
        Check::CommandRan { pattern } => regex_matches(pattern, &obs.cmd),
        Check::OutputMatches { pattern } => regex_matches(pattern, &obs.output),
        Check::LedgerBalance { account, command } => {
            command
                .as_ref()
                .is_none_or(|pattern| regex_matches(pattern, &obs.cmd))
                && facts
                    .balance(account)
                    .is_some_and(|balance| output_contains_amount(&obs.output, balance))
        }
        Check::QuizAnswer { .. } => false,
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

    struct Bank(i64);
    impl Facts for Bank {
        fn balance(&self, _account: &str) -> Option<i64> {
            Some(self.0)
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
            prompt = "list accounts"
            hint = "try bramble ledger accounts"
            check = { type = "command_ran", pattern = "^bramble ledger accounts" }
            [[steps]]
            kind = "task"
            prompt = "sum it"
            check = { type = "ledger_balance", account = "customer:you", command = "^sqlite3" }
            [[steps]]
            kind = "task"
            prompt = "quiz"
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

    fn run(lesson: &Lesson, progress: LessonProgress, cmd: &str, exit: i32, out: &str) -> (LessonProgress, Vec<Outcome>) {
        observe(lesson, &progress, &obs(cmd, exit, out), &Bank(-7150))
    }

    #[test]
    fn session_observe_walks_through_whole_lesson() {
        let lesson = lesson();
        let p = LessonProgress::start(&lesson);
        let (p, o) = run(&lesson, p, "bramble next", 0, "");
        assert_eq!(o, vec![Outcome::Continued]);
        let (p, o) = run(&lesson, p, "bramble ledger accounts", 0, "...");
        assert!(matches!(o[..], [Outcome::Passed { .. }]));
        let (p, o) = run(&lesson, p, "bramble ledger accounts", 0, "customer:you -7150\n");
        assert!(o.is_empty(), "right number from the wrong command doesn't count");
        let (p, o) = run(&lesson, p, "sqlite3 ledger.db 'select sum(...)'", 0, "-7150\n");
        assert!(matches!(o[..], [Outcome::Passed { .. }]));
        let (p, o) = run(&lesson, p, "bramble answer a", 0, "");
        assert_eq!(o, vec![Outcome::WrongAnswer]);
        assert_eq!(p.wrong_answers, 1);
        let (p, o) = run(&lesson, p, "bramble answer B", 0, "");
        assert!(matches!(o[..], [Outcome::Passed { .. }, Outcome::Completed]));
        assert!(p.completed);
        let (_, o) = run(&lesson, p, "bramble next", 0, "");
        assert!(o.is_empty(), "completed lessons ignore further input");
    }

    #[test]
    fn session_observe_ignores_unrelated_and_failed_commands() {
        let lesson = lesson();
        let p = LessonProgress { step: 1, ..LessonProgress::start(&lesson) };
        let (_, o) = run(&lesson, p.clone(), "ls -la", 0, "");
        assert!(o.is_empty());
        let (_, o) = run(&lesson, p, "bramble ledger accounts", 1, "");
        assert!(o.is_empty(), "a failing command never passes a check");
    }

    #[test]
    fn session_observe_hint_is_counted_once_per_step() {
        let lesson = lesson();
        let p = LessonProgress { step: 1, ..LessonProgress::start(&lesson) };
        let (p, o) = run(&lesson, p, "bramble hint", 0, "");
        assert_eq!(o, vec![Outcome::HintRevealed]);
        let (p, o) = run(&lesson, p, "bramble hint", 0, "");
        assert!(o.is_empty());
        assert_eq!(p.hints_used, 1);
        let (p, _) = run(&lesson, p, "bramble ledger accounts", 0, "");
        assert!(!p.hint_revealed, "hint resets on the next step");
        let (_, o) = run(&lesson, p, "bramble hint", 0, "");
        assert_eq!(o, vec![Outcome::NoHint]);
    }

    #[test]
    fn session_observe_next_on_unsolved_task_is_pending() {
        let lesson = lesson();
        let p = LessonProgress { step: 1, ..LessonProgress::start(&lesson) };
        let (p2, o) = run(&lesson, p.clone(), "bramble next", 0, "");
        assert_eq!(o, vec![Outcome::TaskPending]);
        assert_eq!(p2, p);
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
