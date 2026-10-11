//! Messages exchanged between the node and its clients (TUI, web) over WebSocket.

use serde::{Deserialize, Serialize};

use crate::curriculum::Curriculum;
use crate::lesson::{Lesson, Source, Step};
use crate::mastery::{MASTERY_THRESHOLD, MasteryMap};
use crate::pet::{Mood, Pet};
use crate::session::{LessonProgress, Observation};
use crate::teacher::{ActivityKind, ActivityPlan};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[allow(
    clippy::large_enum_variant,
    reason = "a wire message: serialized once and dropped, never stored in bulk"
)]
pub enum ServerMsg {
    State {
        v: u16,
        pet: PetView,
        lesson: Option<LessonView>,
        say: Option<String>,
        #[serde(default)]
        mastery: Vec<ConceptView>,
        #[serde(default)]
        activity: Option<ActivityView>,
        #[serde(default)]
        ledger: Option<LedgerView>,
        /// This bank's place in the payment network (M4). Absent from older nodes.
        #[serde(default)]
        network: Option<NetworkView>,
    },
    /// What the operator's dashboard shows: every bank's account and recent settlements.
    Operator { v: u16, operator: OperatorView },
}

/// A bank's position in the network, for its dashboard and the TUI.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkView {
    pub node_id: String,
    /// Our money held at the operator (`reserves:operator`).
    pub reserves_minor: i64,
    /// ACH payments queued for the next cutoff.
    pub ach_queued: usize,
    pub ach_queued_minor: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorView {
    /// Each member bank's balance at the operator, from the bank's side.
    pub banks: Vec<OperatorBankView>,
    pub ach_windows_settled: i64,
    /// Newest first.
    pub recent_settlements: Vec<EntryView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorBankView {
    pub bank: String,
    pub balance_minor: i64,
}

/// The pet-bank's books, for clients that display them (the TUI ignores this).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LedgerView {
    pub accounts: Vec<AccountView>,
    /// Newest first.
    pub recent_entries: Vec<EntryView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountView {
    pub account: String,
    pub kind: String,
    pub balance_minor: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryView {
    pub entry_id: i64,
    pub memo: String,
    pub postings: Vec<crate::lesson::PostingSpec>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActivityView {
    pub kind: ActivityKind,
    pub reason: String,
}

impl From<&ActivityPlan> for ActivityView {
    fn from(plan: &ActivityPlan) -> Self {
        Self {
            kind: plan.kind,
            reason: plan.reason.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConceptView {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub prereqs: Vec<String>,
    pub p_known: f32,
    pub status: ConceptStatus,
    pub review_due: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConceptStatus {
    /// Prerequisites not mastered yet.
    Locked,
    /// Unlocked, not studied yet.
    Ready,
    Learning,
    Mastered,
}

impl ConceptView {
    /// One view per concept, in curriculum (prerequisite-first) order.
    pub fn build_all(curriculum: &Curriculum, mastery: &MasteryMap, now_unix: i64) -> Vec<Self> {
        curriculum
            .concepts()
            .iter()
            .map(|concept| {
                let p_known = mastery.p_known(&concept.id);
                let status = if p_known >= MASTERY_THRESHOLD {
                    ConceptStatus::Mastered
                } else if !concept.prereqs.iter().all(|p| mastery.is_mastered(p)) {
                    ConceptStatus::Locked
                } else if mastery.is_studied(&concept.id) {
                    ConceptStatus::Learning
                } else {
                    ConceptStatus::Ready
                };
                Self {
                    id: concept.id.clone(),
                    title: concept.title.clone(),
                    prereqs: concept.prereqs.clone(),
                    p_known: p_known as f32,
                    status,
                    review_due: mastery.review_due(&concept.id, now_unix),
                }
            })
            .collect()
    }
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
    #[serde(default)]
    pub step_id: Option<String>,
    pub step_index: usize,
    pub step_count: usize,
    pub kind: StepKind,
    pub text: String,
    pub hint: Option<String>,
    #[serde(default)]
    pub worked_example: Option<String>,
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
        let (kind, step_id, text, hint, worked_example) = match lesson.steps.get(progress.step) {
            Some(Step::Explain { text }) => (StepKind::Explain, None, text.clone(), None, None),
            Some(Step::Task(task)) => (
                StepKind::Task,
                Some(task.id.clone()),
                task.prompt.clone(),
                task.hint.clone().filter(|_| progress.hint_revealed),
                task.worked_example
                    .clone()
                    .filter(|_| progress.worked_example_revealed),
            ),
            None => (
                StepKind::Explain,
                None,
                "Done! Run `bramble next` and I'll pick what we learn next.".to_owned(),
                None,
                None,
            ),
        };
        Self {
            id: lesson.id.clone(),
            title: lesson.title.clone(),
            step_id,
            step_index: progress.step.min(lesson.steps.len()),
            step_count: lesson.steps.len(),
            kind,
            text,
            hint,
            worked_example,
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
        assert_eq!(
            serde_json::from_str::<ClientMsg>(&json).expect("parse"),
            msg
        );
    }

    #[test]
    fn protocol_state_without_network_still_parses() {
        let older = r#"{"type":"state","v":1,"pet":{"name":"pip","mood":"curious","energy":80,"curiosity":40,"level":1,"xp":0},"lesson":null,"say":null}"#;
        let ServerMsg::State { network, .. } = serde_json::from_str(older).expect("parse") else {
            panic!("a state message");
        };
        assert_eq!(network, None);
    }

    #[test]
    fn protocol_operator_message_round_trips() {
        let msg = ServerMsg::Operator {
            v: crate::PROTOCOL_VERSION,
            operator: OperatorView {
                banks: vec![OperatorBankView {
                    bank: "pet-a".into(),
                    balance_minor: 1_000_000,
                }],
                ach_windows_settled: 2,
                recent_settlements: Vec::new(),
            },
        };
        let json = serde_json::to_string(&msg).expect("serialize");
        assert!(json.contains(r#""type":"operator""#), "{json}");
        assert_eq!(
            serde_json::from_str::<ServerMsg>(&json).expect("parse"),
            msg
        );
    }
}
