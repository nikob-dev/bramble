//! The concept DAG: what can be learned, and what must be learned first.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::lesson::Lesson;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Concept {
    pub id: String,
    pub title: String,
    pub track: String,
    #[serde(default)]
    pub prereqs: Vec<String>,
    /// "The learner can…" statements, shown to the learner.
    #[serde(default)]
    pub objectives: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Curriculum {
    /// Topologically ordered: every concept appears after its prerequisites.
    concepts: Vec<Concept>,
}

#[derive(Deserialize)]
struct CurriculumFile {
    concepts: Vec<Concept>,
}

#[derive(Debug, thiserror::Error)]
pub enum CurriculumError {
    #[error("concepts TOML is invalid: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("duplicate concept {0:?}")]
    Duplicate(String),
    #[error("concept {concept:?} requires unknown concept {prereq:?}")]
    UnknownPrereq { concept: String, prereq: String },
    #[error("prerequisites form a cycle among: {0:?}")]
    Cycle(Vec<String>),
    #[error("lesson {lesson:?} teaches unknown concept {concept:?}")]
    UnknownLessonConcept { lesson: String, concept: String },
    #[error("concept {0:?} has no lesson")]
    NoLesson(String),
    #[error("concept {concept:?} needs at least {min} review tasks, found {found}")]
    TooFewReviews {
        concept: String,
        min: usize,
        found: usize,
    },
}

pub const MIN_REVIEW_TASKS: usize = 2;

impl Curriculum {
    pub fn from_toml(text: &str) -> Result<Self, CurriculumError> {
        let file: CurriculumFile = toml::from_str(text)?;
        Self::new(file.concepts)
    }

    pub fn new(concepts: Vec<Concept>) -> Result<Self, CurriculumError> {
        let mut ids = HashSet::new();
        for concept in &concepts {
            if !ids.insert(concept.id.as_str()) {
                return Err(CurriculumError::Duplicate(concept.id.clone()));
            }
        }
        for concept in &concepts {
            if let Some(prereq) = concept.prereqs.iter().find(|p| !ids.contains(p.as_str())) {
                return Err(CurriculumError::UnknownPrereq {
                    concept: concept.id.clone(),
                    prereq: prereq.clone(),
                });
            }
        }
        Ok(Self {
            concepts: topological_order(concepts)?,
        })
    }

    pub fn concepts(&self) -> &[Concept] {
        &self.concepts
    }

    pub fn concept(&self, id: &str) -> Option<&Concept> {
        self.concepts.iter().find(|c| c.id == id)
    }

    /// Every lesson teaches a known concept; every concept has a lesson with enough review tasks.
    pub fn validate_lessons(&self, lessons: &[Lesson]) -> Result<(), CurriculumError> {
        for lesson in lessons {
            if self.concept(&lesson.concept).is_none() {
                return Err(CurriculumError::UnknownLessonConcept {
                    lesson: lesson.id.clone(),
                    concept: lesson.concept.clone(),
                });
            }
        }
        for concept in &self.concepts {
            let lesson = lessons
                .iter()
                .find(|l| l.concept == concept.id)
                .ok_or_else(|| CurriculumError::NoLesson(concept.id.clone()))?;
            if lesson.review.len() < MIN_REVIEW_TASKS {
                return Err(CurriculumError::TooFewReviews {
                    concept: concept.id.clone(),
                    min: MIN_REVIEW_TASKS,
                    found: lesson.review.len(),
                });
            }
        }
        Ok(())
    }
}

/// Kahn's algorithm, stable with respect to file order so authors control tie-breaks.
fn topological_order(concepts: Vec<Concept>) -> Result<Vec<Concept>, CurriculumError> {
    let mut remaining: BTreeMap<usize, Concept> = concepts.into_iter().enumerate().collect();
    let mut placed: HashSet<String> = HashSet::new();
    let mut ordered = Vec::with_capacity(remaining.len());
    while !remaining.is_empty() {
        let ready = remaining
            .iter()
            .find(|(_, c)| c.prereqs.iter().all(|p| placed.contains(p)))
            .map(|(index, _)| *index);
        let Some(index) = ready else {
            return Err(CurriculumError::Cycle(
                remaining.into_values().map(|c| c.id).collect(),
            ));
        };
        let concept = remaining.remove(&index).expect("index came from the map");
        placed.insert(concept.id.clone());
        ordered.push(concept);
    }
    Ok(ordered)
}

/// Loads the curriculum and lessons shipped in `content/`. Shared by tests in this crate.
#[cfg(test)]
pub(crate) fn load_shipped_content() -> (Curriculum, Vec<Lesson>) {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../content");
    let concepts = std::fs::read_to_string(root.join("concepts.toml")).expect("concepts.toml");
    let curriculum = Curriculum::from_toml(&concepts).expect("valid curriculum");
    let mut paths = Vec::new();
    for track in std::fs::read_dir(root.join("lessons")).expect("lessons dir") {
        for file in std::fs::read_dir(track.expect("track").path()).expect("track dir") {
            paths.push(file.expect("file").path());
        }
    }
    paths.sort();
    let lessons = paths
        .iter()
        .map(|path| {
            let text = std::fs::read_to_string(path).expect("read lesson");
            Lesson::from_toml(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        })
        .collect();
    (curriculum, lessons)
}

/// Lessons indexed by concept, for callers that look them up repeatedly.
pub fn lessons_by_concept(lessons: &[Lesson]) -> HashMap<&str, &Lesson> {
    lessons.iter().map(|l| (l.concept.as_str(), l)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn concept(id: &str, prereqs: &[&str]) -> Concept {
        Concept {
            id: id.into(),
            title: id.into(),
            track: "t".into(),
            prereqs: prereqs.iter().map(|p| (*p).to_owned()).collect(),
            objectives: vec![],
        }
    }

    fn ids(curriculum: &Curriculum) -> Vec<&str> {
        curriculum
            .concepts()
            .iter()
            .map(|c| c.id.as_str())
            .collect()
    }

    #[test]
    fn curriculum_new_orders_prereqs_first_and_keeps_file_order_for_ties() {
        let curriculum = Curriculum::new(vec![
            concept("c", &["a"]),
            concept("a", &[]),
            concept("b", &["a"]),
            concept("d", &["b", "c"]),
        ])
        .expect("valid");
        assert_eq!(ids(&curriculum), vec!["a", "c", "b", "d"]);
    }

    #[test]
    fn curriculum_new_rejects_unknown_prereq() {
        let err = Curriculum::new(vec![concept("a", &["ghost"])]).expect_err("unknown");
        assert!(matches!(err, CurriculumError::UnknownPrereq { prereq, .. } if prereq == "ghost"));
    }

    #[test]
    fn curriculum_new_rejects_cycles() {
        let err = Curriculum::new(vec![
            concept("root", &[]),
            concept("a", &["b"]),
            concept("b", &["a"]),
        ])
        .expect_err("cycle");
        assert!(matches!(err, CurriculumError::Cycle(ids) if ids == vec!["a", "b"]));
    }

    #[test]
    fn curriculum_new_rejects_duplicates() {
        let err = Curriculum::new(vec![concept("a", &[]), concept("a", &[])]).expect_err("dup");
        assert!(matches!(err, CurriculumError::Duplicate(_)));
    }

    #[test]
    fn curriculum_shipped_content_is_consistent() {
        let (curriculum, lessons) = load_shipped_content();
        curriculum
            .validate_lessons(&lessons)
            .expect("every concept has a lesson with review tasks");
        assert_eq!(lessons.len(), curriculum.concepts().len());
    }
}
