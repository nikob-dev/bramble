//! The node's in-memory world: pet, ledger, lessons, and the learner's progress.

use bramble_core::lesson::Lesson;
use bramble_core::pet::{Pet, PetEvent};
use bramble_core::protocol::{LessonView, PetView, ServerMsg};
use bramble_core::session::{self, Facts, LessonProgress, Observation, Outcome};
use bramble_ledger::Ledger;

use super::store::PetStore;

const PET_KEY: &str = "pet";
const PROGRESS_KEY: &str = "lesson_progress";

pub struct World {
    pub pet: Pet,
    pub progress: Option<LessonProgress>,
    pub lessons: Vec<Lesson>,
    pub ledger: Ledger,
    pub store: PetStore,
}

struct LedgerFacts<'a>(&'a Ledger);

impl Facts for LedgerFacts<'_> {
    fn balance(&self, account: &str) -> Option<i64> {
        self.0.balance(account).ok()
    }
}

impl World {
    pub fn load(
        pet_name: &str,
        lessons: Vec<Lesson>,
        ledger: Ledger,
        store: PetStore,
        now_unix: i64,
    ) -> anyhow::Result<Self> {
        let pet = store
            .load::<Pet>(PET_KEY)?
            .unwrap_or_else(|| Pet::new(pet_name, now_unix))
            .tick(now_unix);
        let progress = store
            .load::<LessonProgress>(PROGRESS_KEY)?
            .filter(|p| lessons.iter().any(|l| l.id == p.lesson_id))
            .or_else(|| lessons.first().map(LessonProgress::start));
        let world = Self {
            pet,
            progress,
            lessons,
            ledger,
            store,
        };
        world.persist()?;
        Ok(world)
    }

    fn current_lesson(&self) -> Option<&Lesson> {
        let progress = self.progress.as_ref()?;
        self.lessons.iter().find(|l| l.id == progress.lesson_id)
    }

    pub fn snapshot(&self, say: Option<String>) -> ServerMsg {
        let lesson = self
            .current_lesson()
            .zip(self.progress.as_ref())
            .map(|(lesson, progress)| LessonView::build(lesson, progress));
        ServerMsg::State {
            v: bramble_core::PROTOCOL_VERSION,
            pet: PetView::from(&self.pet),
            lesson,
            say,
        }
    }

    /// Advances the pet's clock; returns true if anything visible changed.
    pub fn tick(&mut self, now_unix: i64) -> anyhow::Result<bool> {
        let before = PetView::from(&self.pet);
        self.pet = self.pet.tick(now_unix);
        self.persist()?;
        Ok(PetView::from(&self.pet) != before)
    }

    /// Applies one lab observation. Returns the new state to broadcast, if anything changed.
    pub fn observe(&mut self, observation: &Observation) -> anyhow::Result<Option<ServerMsg>> {
        let (Some(lesson), Some(progress)) = (self.current_lesson(), self.progress.as_ref()) else {
            return Ok(None);
        };
        let (next, outcomes) =
            session::observe(lesson, progress, observation, &LedgerFacts(&self.ledger));
        if outcomes.is_empty() {
            return Ok(None);
        }
        let title = lesson.title.clone();
        self.progress = Some(next);

        let mut say = None;
        for outcome in &outcomes {
            let event = match outcome {
                Outcome::Passed { success } => {
                    let (pet, line) = self.pet.apply(&PetEvent::StepPassed);
                    self.pet = pet;
                    say = Some(success.clone().unwrap_or(line));
                    continue;
                }
                Outcome::HintRevealed => PetEvent::HintUsed,
                Outcome::WrongAnswer => PetEvent::WrongAnswer,
                Outcome::Completed => PetEvent::LessonCompleted {
                    title: title.clone(),
                },
                Outcome::NoHint => {
                    say = Some("No hint for this one. Poke around, I believe in you.".into());
                    continue;
                }
                Outcome::TaskPending => {
                    say = Some("This one needs doing first! Stuck? Try `bramble hint`.".into());
                    continue;
                }
                Outcome::Continued => continue,
            };
            let (pet, line) = self.pet.apply(&event);
            self.pet = pet;
            say = Some(line);
        }
        self.persist()?;
        Ok(Some(self.snapshot(say)))
    }

    fn persist(&self) -> anyhow::Result<()> {
        self.store.save(PET_KEY, &self.pet)?;
        if let Some(progress) = &self.progress {
            self.store.save(PROGRESS_KEY, progress)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::seed::seed;

    fn world() -> World {
        let text = include_str!("../../../../content/lessons/ledgers/01-money-is-rows.toml");
        let lesson = Lesson::from_toml(text).expect("lesson");
        let mut ledger = Ledger::open_in_memory().expect("ledger");
        seed(&mut ledger).expect("seed");
        World::load(
            "pip",
            vec![lesson],
            ledger,
            PetStore::open_in_memory().expect("store"),
            0,
        )
        .expect("world")
    }

    fn cmd(world: &mut World, cmd: &str, output: &str) -> Option<ServerMsg> {
        world
            .observe(&Observation {
                cmd: cmd.into(),
                exit_code: 0,
                cwd: "/home/learner/lab".into(),
                output: output.into(),
            })
            .expect("observe")
    }

    #[test]
    fn world_observe_completes_money_is_rows_against_seeded_ledger() {
        let mut w = world();
        cmd(&mut w, "bramble next", "").expect("continued");
        cmd(&mut w, "bramble ledger accounts", "").expect("passed");
        cmd(&mut w, "bramble next", "").expect("continued");
        cmd(&mut w, "bramble ledger postings customer:you", "").expect("passed");
        assert!(
            cmd(&mut w, "ls", "").is_none(),
            "unrelated commands change nothing"
        );
        cmd(
            &mut w,
            "sqlite3 -readonly ledger.db 'SELECT SUM(amount_minor) ...'",
            "-7150\n",
        )
        .expect("passed");
        cmd(&mut w, "bramble answer b", "").expect("passed");
        let Some(ServerMsg::State {
            lesson, pet, say, ..
        }) = cmd(&mut w, "bramble next", "")
        else {
            panic!("expected a state update");
        };
        assert!(lesson.expect("lesson").completed);
        assert_eq!(pet.xp, 4 * 10 + 50);
        assert!(say.expect("pet speaks").contains("Money is rows"));
    }

    #[test]
    fn world_load_restores_persisted_progress() {
        let mut w = world();
        cmd(&mut w, "bramble next", "");
        let World { store, lessons, .. } = w;
        let mut ledger = Ledger::open_in_memory().expect("ledger");
        seed(&mut ledger).expect("seed");
        let restored = World::load("pip", lessons, ledger, store, 0).expect("reload");
        assert_eq!(restored.progress.expect("progress").step, 1);
    }
}
