use crate::gekiso::room;
use serde_json::{json, Value};
use std::cmp::Ordering;

#[derive(Clone, Copy)]
pub(crate) struct Draw {
    pub chart: usize,
    pub order: usize,
    pub seed: i32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Objective {
    MeanScore,
    SsRate,
}

/// The complete trace for sample zero is also the single-replay API result.
/// Every other sample contributes scores to the same fixed formation.
pub(super) struct SampleScores {
    pub first: room::Replay,
    pub count: usize,
    sum: i64,
    seat_sums: Vec<i64>,
    hits: usize,
    min: i64,
    max: i64,
}

impl SampleScores {
    pub fn first(first: room::Replay, hit: bool) -> Self {
        Self {
            count: 1,
            sum: first.total,
            seat_sums: first.scores.iter().map(|&score| i64::from(score)).collect(),
            hits: usize::from(hit),
            min: first.total,
            max: first.total,
            first,
        }
    }

    pub fn add(&mut self, replay: &room::Replay, hit: bool) {
        self.count += 1;
        self.sum += replay.total;
        self.hits += usize::from(hit);
        for (sum, &score) in self.seat_sums.iter_mut().zip(&replay.scores) {
            *sum += i64::from(score);
        }
        self.min = self.min.min(replay.total);
        self.max = self.max.max(replay.total);
    }

    pub fn cmp(&self, other: &Self, objective: Objective) -> Ordering {
        debug_assert_eq!(self.count, other.count);
        match objective {
            Objective::MeanScore => (self.sum, self.hits).cmp(&(other.sum, other.hits)),
            Objective::SsRate => (self.hits, self.sum).cmp(&(other.hits, other.sum)),
        }
    }

    fn mean(&self, sum: i64) -> f64 {
        sum as f64 / self.count as f64
    }

    pub fn objective_value(&self, objective: Objective) -> Value {
        match objective {
            // Preserve the existing single-replay score field's integer type.
            Objective::MeanScore if self.count == 1 => json!(self.sum),
            Objective::MeanScore => json!(self.mean(self.sum)),
            Objective::SsRate => json!(self.hits as f64 / self.count as f64),
        }
    }

    pub fn statistics(&self, ss: Option<i64>) -> Value {
        let mean_scores: Vec<_> = self.seat_sums.iter().map(|&sum| self.mean(sum)).collect();
        json!({
            "sampleCount":self.count, "scoreSum":self.sum, "meanTotal":self.mean(self.sum),
            "minTotal":self.min, "maxTotal":self.max, "meanScores":mean_scores,
            "ssHits":ss.map(|_| self.hits), "ssRate":ss.map(|_| self.hits as f64 / self.count as f64)
        })
    }
}
