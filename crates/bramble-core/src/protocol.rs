//! Messages exchanged between the node and its clients (TUI, web) over WebSocket.

use serde::{Deserialize, Serialize};

use crate::lesson::{Lesson, Source, Step};
use crate::pet::{Mood, Pet};
use crate::session::{LessonProgress, Observation};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMsg {
    State {
        v: u16,
        pet: PetView,
        lesson: Option<LessonView>,
        say: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMsg {
    Observe { v: u16, observation: Observation },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PetView {
    pub name: String,
    pub mood: Mood,
    pub energy: u8,
    pub curiosity: u8,
    pub xp: u32,
    pub level: u32,
}

impl From<&Pet> for PetView {
    fn from(pet: &Pet) -> Self {
        Self {
            name: pet.name.clone(),
            mood: pet.mood(),
            energy: pet.energy.round() as u8,
            curiosity: pet.curiosity.round() as u8,
            xp: pet.xp,
            level: pet.level(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LessonView {
    pub id: String,
    pub title: String,
    pub step_index: usize,
    pub step_count: usize,
    pub kind: StepKind,
    pub text: String,
    pub hint: Option<String>,
    pub completed: bool,
    pub sources: Vec<Source>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepKind {
    Explain,
    Task,
}

impl LessonView {
    pub fn build(lesson: &Lesson, progress: &LessonProgress) -> Self {
        let (kind, text, hint) = match lesson.steps.get(progress.step) {
            Some(Step::Explain { text }) => (StepKind::Explain, text.clone(), None),
            Some(Step::Task { prompt, hint, .. }) => (
                StepKind::Task,
                prompt.clone(),
                hint.clone().filter(|_| progress.hint_revealed),
            ),
            None => (
                StepKind::Explain,
                "Lesson complete! More of the Ledgers track arrives in the next update.".to_owned(),
                None,
            ),
        };
        Self {
            id: lesson.id.clone(),
            title: lesson.title.clone(),
            step_index: progress.step.min(lesson.steps.len()),
            step_count: lesson.steps.len(),
            kind,
            text,
            hint,
            completed: progress.completed,
            sources: lesson.sources.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_messages_round_trip_through_json() {
        let msg = ClientMsg::Observe {
            v: crate::PROTOCOL_VERSION,
            observation: Observation {
                cmd: "ls".into(),
                exit_code: 0,
                cwd: "/".into(),
                output: String::new(),
            },
        };
        let json = serde_json::to_string(&msg).expect("serialize");
        assert!(json.contains(r#""type":"observe""#));
        assert_eq!(serde_json::from_str::<ClientMsg>(&json).expect("parse"), msg);
    }
}
