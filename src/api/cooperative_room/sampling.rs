//! A room sample picks the play conditions once for every seat and all candidate formations.
use super::super::room_search::solver::Draw;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) enum SkillOrders {
    #[default]
    Fixed,
    Uniform,
}

#[derive(Clone, Copy, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) enum Stream {
    #[default]
    Search,
    Validation,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Sampling {
    pub count: usize,
    pub seed: u32,
    #[serde(default)]
    pub skill_orders: SkillOrders,
    #[serde(default)]
    pub stream: Stream,
}

impl Sampling {
    pub fn validation(self) -> bool {
        matches!(self.stream, Stream::Validation)
    }

    pub fn draws(
        self,
        charts: &[Vec<usize>],
        orders: &[usize],
        seat_seeds: &[i32],
    ) -> Vec<Vec<Draw>> {
        (0..self.count)
            .map(|sample| {
                charts
                    .iter()
                    .enumerate()
                    .map(|(seat, profiles)| {
                        // A separate counter per (draw, seat, random variable) keeps the same LUCK
                        // draws when a caller changes the order model or the number of profiles.
                        let key =
                            mix((u64::from(self.seed) << 32) | u64::from(seat_seeds[seat] as u32));
                        let counter = (sample as u64) << 16 | (seat as u64) << 8;
                        let stream = if self.validation() { 1 << 63 } else { 0 };
                        let word = |channel| key ^ (counter | channel | stream);
                        Draw {
                            chart: profiles[index(word(0), profiles.len())],
                            order: match self.skill_orders {
                                SkillOrders::Fixed => orders[seat],
                                SkillOrders::Uniform => index(word(1), crate::live::orders().len()),
                            },
                            seed: mix(word(2)) as i32,
                        }
                    })
                    .collect()
            })
            .collect()
    }
}

// SplitMix64 finalizer. Rejection avoids rounding the law for e.g. 120 skill orders.
fn mix(mut x: u64) -> u64 {
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
    x ^ (x >> 31)
}

fn index(mut key: u64, len: usize) -> usize {
    let n = len as u64;
    let threshold = n.wrapping_neg() % n;
    loop {
        let value = mix(key);
        if value >= threshold {
            return (value % n) as usize;
        }
        key = key.wrapping_add(0x9e3779b97f4a7c15);
    }
}

/// A binomial score interval for a locked formation in fresh validation draws.
pub(super) fn wilson95(hits: u64, count: u64) -> [f64; 2] {
    let n = count as f64;
    let p = hits as f64 / n;
    let z = 1.959963984540054;
    let den = 1.0 + z * z / n;
    let center = (p + z * z / (2.0 * n)) / den;
    let radius = z / den * (p * (1.0 - p) / n + z * z / (4.0 * n * n)).sqrt();
    [
        if hits == 0 {
            0.0
        } else {
            (center - radius).max(0.0)
        },
        if hits == count {
            1.0
        } else {
            (center + radius).min(1.0)
        },
    ]
}
