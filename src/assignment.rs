//! Reusable maximum-weight matching scratch, including the small top-column
//! search used by the website for up to five remaining snap slots.
pub(crate) fn flagged_capacity(flags: &[Vec<bool>]) -> usize {
    let mut reachable = 1u32;
    for column in 0..flags[0].len() {
        let edges = flags.iter().enumerate().fold(0usize, |mask, (slot, row)| {
            mask | (usize::from(row[column]) << slot)
        });
        if edges == 0 {
            continue;
        }
        let mut previous = reachable;
        while previous != 0 {
            let slots = previous.trailing_zeros() as usize;
            previous &= previous - 1;
            let mut available = edges & !slots;
            while available != 0 {
                let slot = available.trailing_zeros();
                available &= available - 1;
                reachable |= 1 << (slots | (1 << slot));
            }
        }
        if reachable & (1 << 31) != 0 {
            return 5;
        }
    }
    (0u32..32)
        .filter(|&slots| reachable & (1 << slots) != 0)
        .map(u32::count_ones)
        .max()
        .unwrap_or(0) as usize
}

pub(crate) fn event_prices(weights: &[[f64; 5]; 5]) -> [f64; 5] {
    let mut dp = [f64::NEG_INFINITY; 32];
    let mut previous = [0usize; 32];
    dp[0] = 0.0;
    for mask in 0usize..31 {
        let slot = mask.count_ones() as usize;
        for (event, &weight) in weights[slot].iter().enumerate() {
            if mask & (1 << event) != 0 {
                continue;
            }
            let next = mask | (1 << event);
            let value = dp[mask] + weight;
            if value > dp[next] {
                dp[next] = value;
                previous[next] = event;
            }
        }
    }
    if !dp[31].is_finite() {
        return [0.0; 5];
    }
    let mut order = [0usize; 5];
    let mut mask = 31;
    for slot in (0..5).rev() {
        order[slot] = previous[mask];
        mask ^= 1 << order[slot];
    }
    let mut prices = [0.0_f64; 5];
    for _ in 0..5 {
        for slot in 0..5 {
            let assigned = order[slot];
            for event in 0..5 {
                let value = prices[assigned] + weights[slot][event] - weights[slot][assigned];
                prices[event] = prices[event].max(value);
            }
        }
    }
    prices
}

pub(crate) fn minimum_order(weights: &[[f64; 5]; 5]) -> [usize; 5] {
    let mut dp = [f64::INFINITY; 32];
    let mut previous = [0; 32];
    dp[0] = 0.0;
    for mask in 0usize..31 {
        let slot = mask.count_ones() as usize;
        for (event, &weight) in weights[slot].iter().enumerate() {
            if mask & (1 << event) == 0 {
                let next = mask | (1 << event);
                let value = (dp[mask] + weight).next_up();
                if value < dp[next] {
                    dp[next] = value;
                    previous[next] = event;
                }
            }
        }
    }
    let mut order = [0; 5];
    let mut mask = 31;
    for slot in (0..5).rev() {
        order[slot] = previous[mask];
        mask ^= 1 << order[slot];
    }
    order
}

#[derive(Default)]
pub struct Scratch {
    u: Vec<f64>,
    v: Vec<f64>,
    p: Vec<usize>,
    way: Vec<usize>,
    minimum: Vec<f64>,
    used: Vec<bool>,
    small_columns: [i32; 25],
    small_values: [f64; 25],
    small_used: Vec<bool>,
}
impl Scratch {
    pub(crate) fn column_prices(&self, columns: usize) -> Vec<f64> {
        self.v[1..=columns]
            .iter()
            .map(|value| (-value).max(0.0))
            .collect()
    }
    pub fn maximum(
        &mut self,
        weights: &[Vec<f64>],
        columns: usize,
        assignment: Option<&mut [usize]>,
    ) -> f64 {
        let n = weights.len();
        if n == 0 {
            return 0.0;
        }
        if columns < n || weights.iter().any(|row| row.len() < columns) {
            return f64::NEG_INFINITY;
        }
        if assignment.is_none() && n <= 5 && columns <= 4096 {
            return self.small::<false>(weights, columns);
        }
        if weights
            .iter()
            .any(|row| row[..columns].iter().any(|v| !v.is_finite()))
        {
            return if assignment.is_some() {
                f64::NEG_INFINITY
            } else {
                f64::INFINITY
            };
        }
        let size = n.max(columns) + 1;
        if self.u.len() < size {
            let size = size * 2;
            self.u.resize(size, 0.0);
            self.v.resize(size, 0.0);
            self.p.resize(size, 0);
            self.way.resize(size, 0);
            self.minimum.resize(size, 0.0);
            self.used.resize(size, false);
        }
        self.u[..=n].fill(0.0);
        self.v[..=columns].fill(0.0);
        self.p[..=columns].fill(0);
        self.way[..=columns].fill(0);
        let inf = f64::MAX / 4.0;
        for i in 1..=n {
            self.p[0] = i;
            let mut j0 = 0;
            self.minimum[..=columns].fill(inf);
            self.used[..=columns].fill(false);
            loop {
                self.used[j0] = true;
                let i0 = self.p[j0];
                let (mut delta, mut j1) = (inf, 0);
                for j in 1..=columns {
                    if !self.used[j] {
                        let cur = -weights[i0 - 1][j - 1] - self.u[i0] - self.v[j];
                        if cur < self.minimum[j] {
                            self.minimum[j] = cur;
                            self.way[j] = j0;
                        }
                        if self.minimum[j] < delta {
                            delta = self.minimum[j];
                            j1 = j;
                        }
                    }
                }
                for j in 0..=columns {
                    if self.used[j] {
                        self.u[self.p[j]] += delta;
                        self.v[j] -= delta;
                    } else {
                        self.minimum[j] -= delta;
                    }
                }
                j0 = j1;
                if self.p[j0] == 0 {
                    break;
                }
            }
            loop {
                let j1 = self.way[j0];
                self.p[j0] = self.p[j1];
                j0 = j1;
                if j0 == 0 {
                    break;
                }
            }
        }
        let mut assignment = assignment;
        let mut total = 0.0;
        for j in 1..=columns {
            if self.p[j] > 0 {
                total += weights[self.p[j] - 1][j - 1];
                if let Some(out) = assignment.as_deref_mut() {
                    out[self.p[j] - 1] = j - 1;
                }
            }
        }
        total
    }
    pub(crate) fn maximum_upper(&mut self, weights: &[Vec<f64>], columns: usize) -> f64 {
        if weights.is_empty() {
            return 0.0;
        }
        if columns < weights.len() {
            return f64::NEG_INFINITY;
        }
        if columns == 5 && weights.len() == 5 {
            if weights
                .iter()
                .any(|row| row[..5].iter().any(|value| !value.is_finite()))
            {
                return f64::INFINITY;
            }
            let mut dp = [f64::NEG_INFINITY; 32];
            dp[0] = 0.0;
            const ROWS: [u8; 32] = {
                let mut rows = [0; 32];
                let mut mask = 1usize;
                while mask < 32 {
                    rows[mask] = (mask.count_ones() - 1) as u8;
                    mask += 1;
                }
                rows
            };
            for mask in 1usize..32 {
                let row = ROWS[mask] as usize;
                let mut available = mask;
                while available != 0 {
                    let column = available.trailing_zeros() as usize;
                    available &= available - 1;
                    dp[mask] =
                        dp[mask].max((dp[mask ^ (1 << column)] + weights[row][column]).next_up());
                }
            }
            return dp[31];
        }
        if weights.len() <= 5 && columns <= 4096 {
            return self.small::<true>(weights, columns);
        }
        weights.iter().fold(0.0, |sum, row| {
            (sum + row[..columns]
                .iter()
                .copied()
                .fold(f64::NEG_INFINITY, f64::max))
            .next_up()
        })
    }
    fn small<const UPPER: bool>(&mut self, weights: &[Vec<f64>], columns: usize) -> f64 {
        let n = weights.len();
        if self.small_used.len() < 4096 {
            self.small_used.resize(4096, false);
        }
        for (r, row) in weights.iter().enumerate() {
            let base = r * 5;
            self.small_columns[base..base + n].fill(-1);
            self.small_values[base..base + n].fill(f64::NEG_INFINITY);
            for (c, &value) in row.iter().enumerate().take(columns) {
                if !value.is_finite() {
                    return f64::INFINITY;
                }
                if value <= self.small_values[base + n - 1] {
                    continue;
                }
                let mut k = n - 1;
                while k > 0 && self.small_values[base + k - 1] < value {
                    self.small_values[base + k] = self.small_values[base + k - 1];
                    self.small_columns[base + k] = self.small_columns[base + k - 1];
                    k -= 1;
                }
                self.small_values[base + k] = value;
                self.small_columns[base + k] = c as i32;
            }
        }
        self.small_result::<UPPER>(n)
    }
    pub(crate) fn sparse_upper(&mut self, weights: &[Vec<(usize, f64)>; 5], columns: usize) -> f64 {
        if columns + 5 > 4096 {
            return weights.iter().fold(0.0, |sum, row| {
                (sum + row.iter().map(|cell| cell.1).fold(0.0, f64::max)).next_up()
            });
        }
        if self.small_used.len() < 4096 {
            self.small_used.resize(4096, false);
        }
        for (r, row) in weights.iter().enumerate() {
            let base = r * 5;
            for k in 0..5 {
                self.small_columns[base + k] = (columns + k) as i32;
                self.small_values[base + k] = -1e30;
            }
            for &(column, value) in row {
                if !value.is_finite() {
                    return f64::INFINITY;
                }
                if value <= self.small_values[base + 4] {
                    continue;
                }
                let mut k = 4;
                while k > 0 && self.small_values[base + k - 1] < value {
                    self.small_values[base + k] = self.small_values[base + k - 1];
                    self.small_columns[base + k] = self.small_columns[base + k - 1];
                    k -= 1;
                }
                self.small_values[base + k] = value;
                self.small_columns[base + k] = column as i32;
            }
        }
        self.small_result::<true>(5).max(0.0)
    }
    fn small_result<const UPPER: bool>(&mut self, n: usize) -> f64 {
        if n == 1 {
            return self.small_values[0];
        }
        if n == 2 {
            return if self.small_columns[0] != self.small_columns[5] {
                sum::<UPPER>(self.small_values[0], self.small_values[5])
            } else {
                sum::<UPPER>(self.small_values[0], self.small_values[6])
                    .max(sum::<UPPER>(self.small_values[1], self.small_values[5]))
            };
        }
        if (1..n).all(|row| {
            (0..row).all(|previous| self.small_columns[row * 5] != self.small_columns[previous * 5])
        }) {
            return (0..n).fold(0.0, |total, row| {
                sum::<UPPER>(total, self.small_values[row * 5])
            });
        }
        fn sum<const UPPER: bool>(a: f64, b: f64) -> f64 {
            if UPPER {
                (a + b).next_up()
            } else {
                a + b
            }
        }
        fn visit<const UPPER: bool>(
            s: &mut Scratch,
            n: usize,
            r: usize,
            total: f64,
            best: &mut f64,
        ) {
            if r == n {
                if total > *best {
                    *best = total;
                }
                return;
            }
            let upper = (r..n).fold(total, |value, row| {
                let base = row * 5;
                let first = (0..n)
                    .find(|&k| {
                        let column = s.small_columns[base + k];
                        column >= 0 && !s.small_used[column as usize]
                    })
                    .unwrap();
                sum::<UPPER>(value, s.small_values[base + first])
            });
            if upper <= *best {
                return;
            }
            if r + 2 == n {
                let two = |row: usize| {
                    let mut choices = (0..n).filter_map(|k| {
                        let at = row * 5 + k;
                        let column = s.small_columns[at];
                        (column >= 0 && !s.small_used[column as usize])
                            .then_some((column, s.small_values[at]))
                    });
                    [choices.next().unwrap(), choices.next().unwrap()]
                };
                let (a, b) = (two(r), two(r + 1));
                let value = if a[0].0 != b[0].0 {
                    sum::<UPPER>(sum::<UPPER>(total, a[0].1), b[0].1)
                } else {
                    sum::<UPPER>(sum::<UPPER>(total, a[0].1), b[1].1)
                        .max(sum::<UPPER>(sum::<UPPER>(total, a[1].1), b[0].1))
                };
                *best = best.max(value);
                return;
            }
            let base = r * 5;
            for k in 0..n {
                let c = s.small_columns[base + k];
                if c < 0 || s.small_used[c as usize] {
                    continue;
                }
                s.small_used[c as usize] = true;
                visit::<UPPER>(
                    s,
                    n,
                    r + 1,
                    sum::<UPPER>(total, s.small_values[base + k]),
                    best,
                );
                s.small_used[c as usize] = false;
            }
        }
        let mut best = f64::NEG_INFINITY;
        visit::<UPPER>(self, n, 0, 0.0, &mut best);
        best
    }
}
