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
    Task {
        prompt: String,
        #[serde(default)]
        hint: Option<String>,
        #[serde(default)]
        success: Option<String>,
        check: Check,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Check {
    /// A successful command whose text matches `pattern`.
    CommandRan { pattern: String },
    /// A successful command whose output matches `pattern`.
    OutputMatches { pattern: String },
    /// A successful command whose output contains the account's current balance.
    /// `command`, if set, must also match the command text (e.g. require `sqlite3`).
    LedgerBalance {
        account: String,
        #[serde(default)]
        command: Option<String>,
    },
    /// `bramble answer <choice>` with the right choice (case-insensitive).
    QuizAnswer { answer: String },
}

#[derive(Debug, thiserror::Error)]
pub enum LessonError {
    #[error("lesson TOML is invalid: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("lesson {lesson:?} has no steps")]
    NoSteps { lesson: String },
    #[error("lesson {lesson:?} step {step}: invalid regex {pattern:?}: {source}")]
    BadPattern {
        lesson: String,
        step: usize,
        pattern: String,
        source: regex::Error,
    },
}

impl Lesson {
    pub fn from_toml(text: &str) -> Result<Self, LessonError> {
        let lesson: Lesson = toml::from_str(text)?;
        lesson.validate()?;
        Ok(lesson)
    }

    fn validate(&self) -> Result<(), LessonError> {
        if self.steps.is_empty() {
            return Err(LessonError::NoSteps {
                lesson: self.id.clone(),
            });
        }
        for (index, step) in self.steps.iter().enumerate() {
            let pattern = match step {
                Step::Task {
                    check: Check::CommandRan { pattern } | Check::OutputMatches { pattern },
                    ..
                } => Some(pattern),
                Step::Task {
                    check: Check::LedgerBalance { command, .. },
                    ..
                } => command.as_ref(),
                _ => None,
            };
            if let Some(pattern) = pattern {
                Regex::new(pattern).map_err(|source| LessonError::BadPattern {
                    lesson: self.id.clone(),
                    step: index,
                    pattern: pattern.clone(),
                    source,
                })?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lesson_from_toml_parses_explain_and_task_steps() {
        let lesson = Lesson::from_toml(
            r#"
            id = "t"
            title = "T"
            concept = "c"
            [[steps]]
            kind = "explain"
            text = "hello"
            [[steps]]
            kind = "task"
            prompt = "run ls"
            check = { type = "command_ran", pattern = "^ls" }
            "#,
        )
        .expect("valid lesson");
        assert_eq!(lesson.steps.len(), 2);
        assert!(matches!(&lesson.steps[1], Step::Task { check: Check::CommandRan { .. }, .. }));
    }

    #[test]
    fn lesson_from_toml_rejects_bad_regex() {
        let err = Lesson::from_toml(
            r#"
            id = "t"
            title = "T"
            concept = "c"
            [[steps]]
            kind = "task"
            prompt = "x"
            check = { type = "output_matches", pattern = "(" }
            "#,
        )
        .expect_err("bad regex");
        assert!(matches!(err, LessonError::BadPattern { step: 0, .. }));
    }

    #[test]
    fn lesson_content_files_all_parse_and_validate() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../content/lessons");
        let mut count = 0;
        for track in std::fs::read_dir(&dir).expect("lessons dir") {
            for file in std::fs::read_dir(track.expect("track").path()).expect("track dir") {
                let path = file.expect("file").path();
                let text = std::fs::read_to_string(&path).expect("read lesson");
                Lesson::from_toml(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
                count += 1;
            }
        }
        assert!(count > 0, "expected at least one lesson in {}", dir.display());
    }

    #[test]
    fn lesson_from_toml_rejects_empty_lessons() {
        let err = Lesson::from_toml("id = \"t\"\ntitle = \"T\"\nconcept = \"c\"\nsteps = []")
            .expect_err("no steps");
        assert!(matches!(err, LessonError::NoSteps { .. }));
    }
}
