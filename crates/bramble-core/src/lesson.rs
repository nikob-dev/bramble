//! Lessons are data (TOML). Checks are declarative so lesson content can never execute code.

use regex::Regex;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Lesson {
    pub id: String,
    pub title: String,
    pub concept: String,
    #[serde(default)]
    pub sources: Vec<Source>,
    pub steps: Vec<Step>,
    /// Short tasks used for remediation and spaced-review sessions.
    #[serde(default)]
    pub review: Vec<Task>,
    #[serde(default)]
    pub setup: Setup,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    pub title: String,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Step {
    /// Read-only explanation; the learner continues with `bramble next`.
    Explain { text: String },
    /// Something the learner must do in the lab before moving on.
    Task(Task),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Task {
    /// Stable within a lesson; used for event logs and test solutions.
    pub id: String,
    pub prompt: String,
    #[serde(default)]
    pub hint: Option<String>,
    /// Revealed automatically after repeated stumbles.
    #[serde(default)]
    pub worked_example: Option<String>,
    #[serde(default)]
    pub success: Option<String>,
    /// Warm-up step that fast learners skip.
    #[serde(default)]
    pub scaffold: bool,
    /// Whether the command must succeed or fail (e.g. "watch the database reject this").
    #[serde(default)]
    pub expect: Expect,
    pub check: Check,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Expect {
    #[default]
    Success,
    Failure,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Check {
    /// A command whose text matches `pattern`.
    CommandRan { pattern: String },
    /// Output matching `pattern`, optionally only from commands matching `command`.
    OutputMatches {
        pattern: String,
        #[serde(default)]
        command: Option<String>,
    },
    /// Output containing the account's current balance (minor units or decimal).
    LedgerBalance {
        account: String,
        #[serde(default)]
        command: Option<String>,
    },
    /// Output containing a node-published fact, e.g. `sha256:batch-001.csv`.
    OutputContainsFact {
        fact: String,
        #[serde(default)]
        command: Option<String>,
    },
    /// A posted entry, newer than the step's start, with exactly these postings.
    EntryExists { postings: Vec<PostingSpec> },
    /// `bramble answer <choice>` with the right choice (case-insensitive).
    QuizAnswer { answer: String },
    /// `bramble answer <entry id>` naming the entry planted by setup key `<lesson>:<key>`.
    AnswerEntry { setup_key: String },
}

impl Check {
    /// The command pattern that marks a command as an attempt at this check.
    pub fn command_pattern(&self) -> Option<&str> {
        match self {
            Self::CommandRan { pattern } => Some(pattern),
            Self::OutputMatches { command, .. }
            | Self::LedgerBalance { command, .. }
            | Self::OutputContainsFact { command, .. } => command.as_deref(),
            Self::EntryExists { .. } | Self::QuizAnswer { .. } | Self::AnswerEntry { .. } => None,
        }
    }

    fn patterns(&self) -> Vec<&str> {
        let mut patterns: Vec<&str> = self.command_pattern().into_iter().collect();
        if let Self::OutputMatches { pattern, .. } = self {
            patterns.push(pattern);
        }
        patterns
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostingSpec {
    pub account: String,
    pub amount_minor: i64,
}

/// Data the node prepares when a lesson starts. Never code.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Setup {
    /// Posted once, with idempotency key `setup:<lesson id>:<key>`.
    #[serde(default)]
    pub entries: Vec<SetupEntry>,
    /// Lines of the card network's settlement report.
    #[serde(default)]
    pub statement: Vec<StatementLine>,
    /// Files served at `/api/files/<name>`.
    #[serde(default)]
    pub files: Vec<SetupFile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetupEntry {
    pub key: String,
    pub memo: String,
    pub debit: String,
    pub credit: String,
    pub amount_minor: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatementLine {
    pub network_ref: String,
    pub description: String,
    pub amount_minor: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetupFile {
    pub name: String,
    pub content: String,
    #[serde(default)]
    pub sign: bool,
}

pub fn setup_entry_key(lesson_id: &str, key: &str) -> String {
    format!("setup:{lesson_id}:{key}")
}

#[derive(Debug, thiserror::Error)]
pub enum LessonError {
    #[error("lesson TOML is invalid: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("lesson {lesson:?} has no steps")]
    NoSteps { lesson: String },
    #[error("lesson {lesson:?} task {task:?}: invalid regex {pattern:?}: {source}")]
    BadPattern {
        lesson: String,
        task: String,
        pattern: String,
        source: regex::Error,
    },
    #[error("lesson {lesson:?}: duplicate task id {task:?}")]
    DuplicateTask { lesson: String, task: String },
    #[error("lesson {lesson:?}: {problem}")]
    BadSetup { lesson: String, problem: String },
}

impl Lesson {
    pub fn from_toml(text: &str) -> Result<Self, LessonError> {
        let lesson: Lesson = toml::from_str(text)?;
        lesson.validate()?;
        Ok(lesson)
    }

    pub fn tasks(&self) -> impl Iterator<Item = &Task> {
        self.steps
            .iter()
            .filter_map(|step| match step {
                Step::Task(task) => Some(task),
                Step::Explain { .. } => None,
            })
            .chain(&self.review)
    }

    fn validate(&self) -> Result<(), LessonError> {
        let lesson = || self.id.clone();
        if self.steps.is_empty() {
            return Err(LessonError::NoSteps { lesson: lesson() });
        }
        let mut seen = std::collections::HashSet::new();
        for task in self.tasks() {
            if !seen.insert(task.id.as_str()) {
                return Err(LessonError::DuplicateTask {
                    lesson: lesson(),
                    task: task.id.clone(),
                });
            }
            for pattern in task.check.patterns() {
                Regex::new(pattern).map_err(|source| LessonError::BadPattern {
                    lesson: lesson(),
                    task: task.id.clone(),
                    pattern: pattern.to_owned(),
                    source,
                })?;
            }
        }
        let bad_setup = |problem: String| LessonError::BadSetup {
            lesson: lesson(),
            problem,
        };
        for entry in &self.setup.entries {
            if entry.amount_minor <= 0 || entry.debit == entry.credit {
                return Err(bad_setup(format!(
                    "setup entry {:?} needs a positive amount and two different accounts",
                    entry.key
                )));
            }
        }
        for file in &self.setup.files {
            let safe = !file.name.is_empty()
                && !file.name.starts_with('.')
                && file
                    .name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'));
            if !safe {
                return Err(bad_setup(format!("unsafe file name {:?}", file.name)));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = r#"
        id = "t"
        title = "T"
        concept = "c"
        [[steps]]
        kind = "explain"
        text = "hello"
        [[steps]]
        kind = "task"
        id = "ls"
        prompt = "run ls"
        scaffold = true
        check = { type = "command_ran", pattern = "^ls" }
        [[review]]
        id = "quiz"
        prompt = "pick b"
        check = { type = "quiz_answer", answer = "b" }
        [[setup.entries]]
        key = "k"
        memo = "m"
        debit = "a"
        credit = "b"
        amount_minor = 5
        [[setup.files]]
        name = "batch-001.csv"
        content = "x"
        sign = true
    "#;

    #[test]
    fn lesson_from_toml_parses_tasks_review_and_setup() {
        let lesson = Lesson::from_toml(MINIMAL).expect("valid lesson");
        assert_eq!(lesson.steps.len(), 2);
        let Step::Task(task) = &lesson.steps[1] else {
            panic!("expected a task");
        };
        assert!(task.scaffold);
        assert_eq!(task.expect, Expect::Success);
        assert_eq!(lesson.review.len(), 1);
        assert_eq!(lesson.tasks().count(), 2);
        assert_eq!(lesson.setup.entries[0].amount_minor, 5);
        assert!(lesson.setup.files[0].sign);
    }

    #[test]
    fn lesson_from_toml_rejects_bad_regex_in_command_patterns() {
        let text = MINIMAL.replace(r#"pattern = "^ls""#, r#"pattern = "(""#);
        assert!(matches!(
            Lesson::from_toml(&text),
            Err(LessonError::BadPattern { .. })
        ));
        let text = MINIMAL.replace(
            r#"{ type = "command_ran", pattern = "^ls" }"#,
            r#"{ type = "ledger_balance", account = "x", command = "[" }"#,
        );
        assert!(matches!(
            Lesson::from_toml(&text),
            Err(LessonError::BadPattern { .. })
        ));
    }

    #[test]
    fn lesson_from_toml_rejects_duplicate_task_ids() {
        let text = MINIMAL.replace(r#"id = "quiz""#, r#"id = "ls""#);
        assert!(matches!(
            Lesson::from_toml(&text),
            Err(LessonError::DuplicateTask { .. })
        ));
    }

    #[test]
    fn lesson_from_toml_rejects_unsafe_setup() {
        let text = MINIMAL.replace("batch-001.csv", "../etc/passwd");
        assert!(matches!(
            Lesson::from_toml(&text),
            Err(LessonError::BadSetup { .. })
        ));
        let text = MINIMAL.replace("amount_minor = 5", "amount_minor = -5");
        assert!(matches!(
            Lesson::from_toml(&text),
            Err(LessonError::BadSetup { .. })
        ));
    }

    #[test]
    fn lesson_from_toml_rejects_empty_lessons() {
        let err = Lesson::from_toml("id = \"t\"\ntitle = \"T\"\nconcept = \"c\"\nsteps = []")
            .expect_err("no steps");
        assert!(matches!(err, LessonError::NoSteps { .. }));
    }
}
