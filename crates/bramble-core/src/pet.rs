use serde::{Deserialize, Serialize};

const MAX_STAT: f32 = 100.0;
const ENERGY_DECAY_PER_HOUR: f32 = 2.0;
const CURIOSITY_DECAY_PER_HOUR: f32 = 4.0;
const XP_PER_LEVEL: u32 = 100;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pet {
    pub name: String,
    pub energy: f32,
    pub curiosity: f32,
    pub xp: u32,
    pub last_tick_unix: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mood {
    Happy,
    Curious,
    Bored,
    Sleepy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PetEvent {
    StepPassed,
    HintUsed,
    WrongAnswer,
    LessonCompleted { title: String },
}

impl Pet {
    pub fn new(name: impl Into<String>, now_unix: i64) -> Self {
        Self {
            name: name.into(),
            energy: 80.0,
            curiosity: 50.0,
            xp: 0,
            last_tick_unix: now_unix,
        }
    }

    pub fn level(&self) -> u32 {
        1 + self.xp / XP_PER_LEVEL
    }

    pub fn mood(&self) -> Mood {
        if self.energy < 25.0 {
            Mood::Sleepy
        } else if self.curiosity < 20.0 {
            Mood::Bored
        } else if self.curiosity >= 60.0 {
            Mood::Happy
        } else {
            Mood::Curious
        }
    }

    /// Advances the pet's clock. Time running backwards (clock skew) is treated as no time passing.
    pub fn tick(&self, now_unix: i64) -> Self {
        let hours = (now_unix - self.last_tick_unix).max(0) as f32 / 3600.0;
        Self {
            energy: clamp(self.energy - ENERGY_DECAY_PER_HOUR * hours),
            curiosity: clamp(self.curiosity - CURIOSITY_DECAY_PER_HOUR * hours),
            last_tick_unix: now_unix.max(self.last_tick_unix),
            ..self.clone()
        }
    }

    /// Applies an event, returning the new pet and what it says about it.
    pub fn apply(&self, event: &PetEvent) -> (Self, String) {
        let mut next = self.clone();
        let line = match event {
            PetEvent::StepPassed => {
                next.curiosity = clamp(next.curiosity + 10.0);
                next.energy = clamp(next.energy + 2.0);
                next.xp += 10;
                pick(
                    &[
                        "Ooh! I didn't know that. Writing it in my ledger of knowledge.",
                        "You're good at this. My vault feels sturdier already.",
                        "*scribbles furiously* Again! What's next?",
                    ],
                    next.xp,
                )
            }
            PetEvent::HintUsed => {
                next.curiosity = clamp(next.curiosity + 3.0);
                pick(
                    &[
                        "Here's a nudge. I had to look it up too.",
                        "Hints are just borrowed knowledge. We'll pay it back.",
                    ],
                    next.xp,
                )
            }
            PetEvent::WrongAnswer => {
                next.curiosity = clamp(next.curiosity + 2.0);
                pick(
                    &[
                        "Hmm, not quite. Wrong answers are cheap here — try again.",
                        "Close! Let's look at the rows again.",
                    ],
                    next.xp,
                )
            }
            PetEvent::LessonCompleted { title } => {
                next.xp += 50;
                next.energy = clamp(next.energy + 20.0);
                return (
                    next,
                    format!(
                        "We finished \"{title}\"! I understand my own bank a little better now."
                    ),
                );
            }
        };
        (next, line.to_owned())
    }
}

fn clamp(value: f32) -> f32 {
    value.clamp(0.0, MAX_STAT)
}

fn pick(lines: &[&'static str], seed: u32) -> &'static str {
    lines[seed as usize % lines.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pet_tick_decays_stats_over_time() {
        let pet = Pet::new("pip", 0);
        let later = pet.tick(10 * 3600);
        assert_eq!(later.energy, 60.0);
        assert_eq!(later.curiosity, 10.0);
        assert_eq!(later.mood(), Mood::Bored);
    }

    #[test]
    fn pet_tick_is_deterministic_and_ignores_clock_skew() {
        let pet = Pet::new("pip", 1_000);
        assert_eq!(pet.tick(5_000), pet.tick(5_000));
        assert_eq!(pet.tick(0), pet);
    }

    #[test]
    fn pet_tick_clamps_at_zero() {
        let pet = Pet::new("pip", 0).tick(1_000 * 3600);
        assert_eq!((pet.energy, pet.curiosity), (0.0, 0.0));
        assert_eq!(pet.mood(), Mood::Sleepy);
    }

    #[test]
    fn pet_apply_step_and_lesson_award_xp_and_level() {
        let pet = Pet::new("pip", 0);
        let (pet, _) = pet.apply(&PetEvent::StepPassed);
        let (pet, line) = pet.apply(&PetEvent::LessonCompleted {
            title: "Money is rows".into(),
        });
        assert_eq!(pet.xp, 60);
        assert!(line.contains("Money is rows"));
        let (pet, _) = (0..4).fold((pet, String::new()), |(p, _), _| {
            p.apply(&PetEvent::StepPassed)
        });
        assert_eq!(pet.level(), 2);
    }
}
