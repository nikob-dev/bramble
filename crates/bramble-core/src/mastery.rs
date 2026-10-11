//! Bayesian Knowledge Tracing per concept, plus a spaced-review schedule.
//!
//! BKT models "knows the concept" as a hidden yes/no. Each observed attempt
//! updates the belief (Bayes' rule, allowing for slips and lucky guesses),
//! then the learner may have learned from the attempt itself.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub const MASTERY_THRESHOLD: f64 = 0.85;
pub const REVIEW_INTERVALS_DAYS: [i64; 4] = [1, 3, 7, 21];
const DAY: i64 = 24 * 3600;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BktParams {
    /// P(known) before any evidence.
    pub p_init: f64,
    /// P(unknown → known) per attempt.
    pub p_learn: f64,
    /// P(wrong | known).
    pub p_slip: f64,
    /// P(right | unknown).
    pub p_guess: f64,
}

pub const DEFAULT_PARAMS: BktParams = BktParams {
    p_init: 0.2,
    p_learn: 0.3,
    p_slip: 0.1,
    p_guess: 0.2,
};

/// One BKT step: posterior given the observation, then the learning transition.
pub fn bkt_update(p_known: f64, correct: bool, params: BktParams) -> f64 {
    let BktParams {
        p_learn,
        p_slip,
        p_guess,
        ..
    } = params;
    let posterior = if correct {
        let known = p_known * (1.0 - p_slip);
        known / (known + (1.0 - p_known) * p_guess)
    } else {
        let known = p_known * p_slip;
        known / (known + (1.0 - p_known) * (1.0 - p_guess))
    };
    posterior + (1.0 - posterior) * p_learn
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConceptMastery {
    pub p_known: f64,
    pub observations: u32,
    #[serde(default)]
    pub review: Option<ReviewSchedule>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewSchedule {
    pub due_unix: i64,
    pub interval_index: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MasteryMap {
    concepts: BTreeMap<String, ConceptMastery>,
}

impl MasteryMap {
    pub fn get(&self, concept: &str) -> Option<&ConceptMastery> {
        self.concepts.get(concept)
    }

    pub fn p_known(&self, concept: &str) -> f64 {
        self.concepts
            .get(concept)
            .map_or(DEFAULT_PARAMS.p_init, |m| m.p_known)
    }

    pub fn is_mastered(&self, concept: &str) -> bool {
        self.p_known(concept) >= MASTERY_THRESHOLD
    }

    pub fn is_studied(&self, concept: &str) -> bool {
        self.concepts
            .get(concept)
            .is_some_and(|m| m.observations > 0)
    }

    pub fn review_due(&self, concept: &str, now_unix: i64) -> bool {
        self.concepts
            .get(concept)
            .and_then(|m| m.review)
            .is_some_and(|r| r.due_unix <= now_unix)
    }

    /// Records one attempt. The first time a concept crosses the mastery bar,
    /// its first spaced review is scheduled.
    pub fn observe(&mut self, concept: &str, correct: bool, now_unix: i64) {
        let entry = self
            .concepts
            .entry(concept.to_owned())
            .or_insert(ConceptMastery {
                p_known: DEFAULT_PARAMS.p_init,
                observations: 0,
                review: None,
            });
        entry.p_known = bkt_update(entry.p_known, correct, DEFAULT_PARAMS);
        entry.observations += 1;
        if entry.review.is_none() && entry.p_known >= MASTERY_THRESHOLD {
            entry.review = Some(schedule(0, now_unix));
        }
    }

    /// After a review session: a clean review stretches the interval, a shaky one resets it.
    pub fn finish_review(&mut self, concept: &str, clean: bool, now_unix: i64) {
        if let Some(entry) = self.concepts.get_mut(concept) {
            let next = match (clean, entry.review) {
                (true, Some(r)) => (r.interval_index + 1).min(REVIEW_INTERVALS_DAYS.len() - 1),
                _ => 0,
            };
            entry.review = Some(schedule(next, now_unix));
        }
    }
}

fn schedule(interval_index: usize, now_unix: i64) -> ReviewSchedule {
    ReviewSchedule {
        due_unix: now_unix + REVIEW_INTERVALS_DAYS[interval_index] * DAY,
        interval_index,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-3
    }

    #[test]
    fn mastery_bkt_update_matches_hand_computed_values() {
        // correct: posterior = .2*.9 / (.18 + .8*.2) = .5294; + (1-.5294)*.3 = .6706
        let one = bkt_update(0.2, true, DEFAULT_PARAMS);
        assert!(close(one, 0.6706), "{one}");
        // correct again: .6706*.9=.6035; /(.6035+.3294*.2=.0659) = .9016; + .0984*.3 = .9311
        let two = bkt_update(one, true, DEFAULT_PARAMS);
        assert!(close(two, 0.9311), "{two}");
        // wrong from .9311: .9311*.1=.0931; /(.0931+.0689*.8=.0551) = .6282; + .3718*.3 = .7398
        let slip = bkt_update(two, false, DEFAULT_PARAMS);
        assert!(close(slip, 0.7398), "{slip}");
    }

    #[test]
    fn mastery_two_clean_attempts_reach_mastery_and_schedule_review() {
        let mut map = MasteryMap::default();
        assert!(!map.is_studied("c"));
        map.observe("c", true, 0);
        assert!(!map.is_mastered("c"));
        assert!(map.get("c").expect("tracked").review.is_none());
        map.observe("c", true, 0);
        assert!(map.is_mastered("c"));
        let review = map.get("c").expect("tracked").review.expect("scheduled");
        assert_eq!(review.due_unix, DAY);
        assert!(!map.review_due("c", DAY - 1));
        assert!(map.review_due("c", DAY));
    }

    #[test]
    fn mastery_finish_review_stretches_or_resets_interval() {
        let mut map = MasteryMap::default();
        map.observe("c", true, 0);
        map.observe("c", true, 0);
        map.finish_review("c", true, 0);
        assert_eq!(
            map.get("c").expect("c").review.expect("r").due_unix,
            3 * DAY
        );
        map.finish_review("c", true, 0);
        map.finish_review("c", true, 0);
        map.finish_review("c", true, 0);
        assert_eq!(
            map.get("c").expect("c").review.expect("r").due_unix,
            21 * DAY,
            "caps at 21d"
        );
        map.finish_review("c", false, 0);
        assert_eq!(
            map.get("c").expect("c").review.expect("r").due_unix,
            DAY,
            "shaky review resets"
        );
    }
}
