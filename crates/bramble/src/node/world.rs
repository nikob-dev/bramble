//! The node's in-memory world: pet, ledger, curriculum, mastery, and the current activity.

use bramble_core::curriculum::Curriculum;
use bramble_core::lesson::{Lesson, PostingSpec, setup_entry_key};
use bramble_core::mastery::MasteryMap;
use bramble_core::pet::{Pet, PetEvent};
use bramble_core::protocol::{ActivityView, ConceptView, LessonView, PetView, ServerMsg};
use bramble_core::session::{self, Facts, LessonProgress, Observation, Outcome};
use bramble_core::teacher::{self, ActivityKind, ActivityPlan, ActivitySummary};
use bramble_ledger::{Ledger, NewEntry, Posting};

use super::published::Published;
use super::store::{LearningEvent, PetStore};

const PET_DOC: &str = "pet";
const PROGRESS_DOC: &str = "lesson_progress";
const MASTERY_DOC: &str = "mastery";
const PLAN_DOC: &str = "activity_plan";
const SUMMARY_DOC: &str = "last_summary";

pub struct World {
    pub pet: Pet,
    curriculum: Curriculum,
    lessons: Vec<Lesson>,
    mastery: MasteryMap,
    plan: ActivityPlan,
    lesson: Option<Lesson>,
    progress: Option<LessonProgress>,
    /// How the finished activity went; consumed when the learner moves on.
    last_summary: Option<ActivitySummary>,
    step_started_unix: i64,
    pub ledger: Ledger,
    pub published: Published,
    store: PetStore,
}

struct NodeFacts<'a> {
    ledger: &'a Ledger,
    published: &'a Published,
}

impl Facts for NodeFacts<'_> {
    fn balance(&self, account: &str) -> Option<i64> {
        self.ledger.balance(account).ok()
    }

    fn max_entry_id(&self) -> i64 {
        self.ledger.max_entry_id().unwrap_or_else(|error| {
            tracing::error!(%error, "reading ledger high-water mark");
            0
        })
    }

    fn entry_exists_since(&self, since_entry_id: i64, postings: &[PostingSpec]) -> bool {
        let wanted: Vec<Posting> = postings
            .iter()
            .map(|p| Posting::new(p.account.clone(), p.amount_minor))
            .collect();
        match self.ledger.find_entry_since(since_entry_id, &wanted) {
            Ok(found) => found.is_some(),
            Err(error) => {
                tracing::error!(%error, "searching ledger for entry");
                false
            }
        }
    }

    fn setup_entry_id(&self, key: &str) -> Option<i64> {
        self.ledger
            .entry_id_by_idempotency_key(key)
            .unwrap_or_else(|error| {
                tracing::error!(%error, key, "looking up setup entry");
                None
            })
    }

    fn fact(&self, name: &str) -> Option<String> {
        self.published.fact(name)
    }
}

pub struct WorldParts {
    pub pet_name: String,
    pub curriculum: Curriculum,
    pub lessons: Vec<Lesson>,
    pub ledger: Ledger,
    pub published: Published,
    pub store: PetStore,
}

impl World {
    pub fn load(parts: WorldParts, now_unix: i64) -> anyhow::Result<Self> {
        let WorldParts {
            pet_name,
            curriculum,
            lessons,
            ledger,
            published,
            store,
        } = parts;
        let pet = store
            .load::<Pet>(PET_DOC)?
            .unwrap_or_else(|| Pet::new(&pet_name, now_unix))
            .tick(now_unix);
        let mastery = store.load::<MasteryMap>(MASTERY_DOC)?.unwrap_or_default();
        let last_summary = store
            .load::<Option<ActivitySummary>>(SUMMARY_DOC)?
            .flatten();
        let plan = store.load::<ActivityPlan>(PLAN_DOC)?.unwrap_or_else(|| {
            teacher::choose_next(&curriculum, &lessons, &mastery, None, now_unix)
        });

        let mut world = Self {
            pet,
            curriculum,
            lessons,
            mastery,
            plan: plan.clone(),
            lesson: None,
            progress: None,
            last_summary,
            step_started_unix: now_unix,
            ledger,
            published,
            store,
        };
        let saved = world.store.load::<LessonProgress>(PROGRESS_DOC)?;
        world.start_activity(plan, saved)?;
        Ok(world)
    }

    /// Builds the plan's lesson, runs its setup, and restores saved progress if it matches.
    fn start_activity(
        &mut self,
        plan: ActivityPlan,
        saved: Option<LessonProgress>,
    ) -> anyhow::Result<()> {
        let lesson = teacher::build_lesson(&plan, &self.curriculum, &self.lessons);
        if let Some(lesson) = &lesson {
            self.run_setup(lesson)?;
        }
        let watermark = self.ledger.max_entry_id()?;
        self.progress = lesson.as_ref().map(|lesson| {
            saved
                .filter(|p| p.lesson_id == lesson.id)
                .unwrap_or_else(|| LessonProgress::start(lesson, watermark))
        });
        self.lesson = lesson;
        self.plan = plan;
        self.persist()
    }

    /// Plants the lesson's setup entries. Idempotent: each has a fixed idempotency key.
    fn run_setup(&mut self, lesson: &Lesson) -> anyhow::Result<()> {
        for entry in &lesson.setup.entries {
            self.ledger.post(&NewEntry {
                memo: entry.memo.clone(),
                idempotency_key: Some(setup_entry_key(&lesson.id, &entry.key)),
                postings: vec![
                    Posting::new(entry.debit.clone(), entry.amount_minor),
                    Posting::new(entry.credit.clone(), -entry.amount_minor),
                ],
            })?;
        }
        Ok(())
    }

    pub fn snapshot(&self, say: Option<String>, now_unix: i64) -> ServerMsg {
        let lesson = self
            .lesson
            .as_ref()
            .zip(self.progress.as_ref())
            .map(|(lesson, progress)| LessonView::build(lesson, progress));
        ServerMsg::State {
            v: bramble_core::PROTOCOL_VERSION,
            pet: PetView::from(&self.pet),
            lesson,
            say,
            mastery: ConceptView::build_all(&self.curriculum, &self.mastery, now_unix),
            activity: Some(ActivityView::from(&self.plan)),
        }
    }

    /// Advances the clock. Returns what the pet says if anything visible changed.
    pub fn tick(&mut self, now_unix: i64) -> anyhow::Result<Option<Option<String>>> {
        let before = PetView::from(&self.pet);
        self.pet = self.pet.tick(now_unix);
        let mut say = None;
        // A finished track wakes up when a spaced review comes due.
        if self.plan.kind == ActivityKind::TrackComplete {
            let next = teacher::choose_next(
                &self.curriculum,
                &self.lessons,
                &self.mastery,
                None,
                now_unix,
            );
            if next.kind == ActivityKind::Review {
                say = Some(next.reason.clone());
                self.start_activity(next, None)?;
                self.step_started_unix = now_unix;
            }
        }
        self.persist()?;
        Ok((say.is_some() || PetView::from(&self.pet) != before).then_some(say))
    }

    /// Applies one lab observation. Returns the new state to broadcast, if anything changed.
    pub fn observe(
        &mut self,
        observation: &Observation,
        now_unix: i64,
    ) -> anyhow::Result<Option<ServerMsg>> {
        let finished = self.progress.as_ref().is_none_or(|p| p.completed);
        if finished {
            if !is_next(&observation.cmd) || self.plan.kind == ActivityKind::TrackComplete {
                return Ok(None);
            }
            let next = teacher::choose_next(
                &self.curriculum,
                &self.lessons,
                &self.mastery,
                self.last_summary.as_ref(),
                now_unix,
            );
            let reason = next.reason.clone();
            self.last_summary = None;
            self.start_activity(next, None)?;
            self.step_started_unix = now_unix;
            return Ok(Some(self.snapshot(Some(reason), now_unix)));
        }

        let (Some(lesson), Some(progress)) = (self.lesson.as_ref(), self.progress.as_ref()) else {
            return Ok(None);
        };
        let facts = NodeFacts {
            ledger: &self.ledger,
            published: &self.published,
        };
        let (next, outcomes) = session::observe(lesson, progress, observation, &facts);
        if outcomes.is_empty() {
            return Ok(None);
        }
        let lesson_id = lesson.id.clone();
        let title = lesson.title.clone();
        let concept = lesson.concept.clone();
        let task_id = current_task_id(lesson, progress.step);
        let elapsed_ms = (now_unix - self.step_started_unix).max(0) * 1000;
        self.progress = Some(next);

        let mut say = None;
        for outcome in &outcomes {
            self.store.log_event(&LearningEvent {
                ts: now_unix,
                concept: &concept,
                lesson_id: &lesson_id,
                task_id: task_id.as_deref(),
                outcome: outcome_name(outcome),
                elapsed_ms,
            })?;
            let (event, line) = match outcome {
                Outcome::Passed { success, clean, .. } => {
                    self.mastery.observe(&concept, *clean, now_unix);
                    self.step_started_unix = now_unix;
                    let (pet, line) = self.pet.apply(&PetEvent::StepPassed);
                    self.pet = pet;
                    say = Some(success.clone().unwrap_or(line));
                    continue;
                }
                Outcome::Continued => {
                    self.step_started_unix = now_unix;
                    continue;
                }
                Outcome::HintRevealed => (Some(PetEvent::HintUsed), None),
                Outcome::WrongAnswer => (Some(PetEvent::WrongAnswer), None),
                Outcome::Stumbled => (
                    Some(PetEvent::WrongAnswer),
                    Some(
                        "Not quite: that didn't do it yet. Read the output, then try again."
                            .to_owned(),
                    ),
                ),
                Outcome::Escalated {
                    worked_example: true,
                } => (
                    None,
                    Some(
                        "You've hit this twice, so here's a worked example. Seeing one solved \
                         is often quicker than another hint."
                            .to_owned(),
                    ),
                ),
                Outcome::Escalated {
                    worked_example: false,
                } => (
                    None,
                    Some("You've hit this twice, so I've put the hint up for you.".to_owned()),
                ),
                Outcome::NoHint => (
                    None,
                    Some("No hint for this one. Poke around, I believe in you.".to_owned()),
                ),
                Outcome::TaskPending => (
                    None,
                    Some("This one needs doing first! Stuck? Try `bramble hint`.".to_owned()),
                ),
                Outcome::Completed => {
                    self.finish_activity(&concept, now_unix);
                    (
                        Some(PetEvent::LessonCompleted {
                            title: title.clone(),
                        }),
                        None,
                    )
                }
            };
            if let Some(event) = event {
                let (pet, pet_line) = self.pet.apply(&event);
                self.pet = pet;
                say = Some(pet_line);
            }
            if line.is_some() {
                say = line;
            }
        }
        self.persist()?;
        Ok(Some(self.snapshot(say, now_unix)))
    }

    fn finish_activity(&mut self, concept: &str, now_unix: i64) {
        let clean = self.progress.as_ref().is_some_and(LessonProgress::is_clean);
        if self.plan.kind == ActivityKind::Review {
            self.mastery.finish_review(concept, clean, now_unix);
        }
        self.last_summary = Some(ActivitySummary {
            concept: concept.to_owned(),
            kind: self.plan.kind,
            clean,
            remediation_streak: self.plan.remediation_streak,
        });
    }

    fn persist(&self) -> anyhow::Result<()> {
        self.store.save(PET_DOC, &self.pet)?;
        self.store.save(MASTERY_DOC, &self.mastery)?;
        self.store.save(PLAN_DOC, &self.plan)?;
        self.store.save(SUMMARY_DOC, &self.last_summary)?;
        if let Some(progress) = &self.progress {
            self.store.save(PROGRESS_DOC, progress)?;
        }
        Ok(())
    }
}

fn is_next(cmd: &str) -> bool {
    cmd.split_whitespace().take(2).eq(["bramble", "next"])
}

fn current_task_id(lesson: &Lesson, step: usize) -> Option<String> {
    match lesson.steps.get(step)? {
        bramble_core::lesson::Step::Task(task) => Some(task.id.clone()),
        bramble_core::lesson::Step::Explain { .. } => None,
    }
}

fn outcome_name(outcome: &Outcome) -> &'static str {
    match outcome {
        Outcome::Continued => "continued",
        Outcome::Passed { clean: true, .. } => "passed_clean",
        Outcome::Passed { clean: false, .. } => "passed_assisted",
        Outcome::HintRevealed => "hint",
        Outcome::NoHint => "no_hint",
        Outcome::Stumbled => "stumbled",
        Outcome::WrongAnswer => "wrong_answer",
        Outcome::Escalated { .. } => "escalated",
        Outcome::TaskPending => "task_pending",
        Outcome::Completed => "completed",
    }
}

#[cfg(test)]
mod tests;
