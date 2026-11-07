//! Lossless normalized history profiles with propagated difference relations.
use std::{collections::BTreeMap, ops::Deref, sync::Arc};

#[derive(Clone)]
pub(super) struct Offsets {
    pub ranks: Vec<Vec<i32>>,
    pub gears: Vec<Vec<i32>>,
    pub bonus: Vec<i32>,
}
impl Offsets {
    fn zero(records: usize, roles: usize, bands: usize) -> Self {
        Self {
            ranks: vec![vec![0; roles]; records],
            gears: vec![vec![0; bands]; records],
            bonus: vec![0; records],
        }
    }
    pub fn flat(&self, r: usize, j: usize) -> i32 {
        self.ranks[r][j] + self.bonus[r] / 5
    }
    pub fn non_gear(&self) -> bool {
        self.ranks.iter().flatten().any(|&v| v != 0) || self.bonus.iter().any(|&v| v != 0)
    }
    pub fn dimensions(&self) -> usize {
        (0..self.ranks[0].len())
            .filter(|&j| self.ranks.iter().any(|r| r[j] != 0))
            .count()
            + (0..self.gears[0].len())
                .filter(|&j| self.gears.iter().any(|r| r[j] != 0))
                .count()
            + usize::from(self.bonus.iter().any(|&v| v != 0))
    }
}
impl Deref for Offsets {
    type Target = [Vec<i32>];
    fn deref(&self) -> &Self::Target {
        &self.gears
    }
}

#[derive(Clone, Eq, PartialEq, Ord, PartialOrd)]
pub(super) struct Relation {
    pub a: usize,
    pub b: usize,
    pub allowed: Vec<bool>,
}
pub(super) type AxisCache = BTreeMap<(i32, Vec<Relation>), Arc<Vec<Vec<i32>>>>;
#[derive(Clone)]
pub(super) struct Difference {
    pub a: usize,
    pub b: usize,
    pub target: i64,
    pub ranges: Vec<Vec<(i64, i64)>>,
}
pub(super) type Constraints = (Vec<Vec<Relation>>, Vec<Difference>);
#[derive(Clone)]
struct Axis {
    span: i32,
    low: i32,
    values: Vec<i32>,
    rules: Vec<Relation>,
    done: bool,
}
impl Axis {
    fn new(records: usize, span: i32, rules: &[Relation]) -> Option<Self> {
        let mut a = Self {
            span,
            low: -span,
            values: vec![-span; records],
            rules: rules.to_vec(),
            done: false,
        };
        a.values[0] = 0;
        a.seek();
        (!a.done).then_some(a)
    }
    fn bump(&mut self) {
        for i in (1..self.values.len()).rev() {
            if self.values[i] < self.low + self.span {
                self.values[i] += 1;
                return;
            }
            self.values[i] = self.low;
        }
        self.low += 1;
        if self.low > 0 {
            self.done = true;
            return;
        }
        self.values[1..].fill(self.low);
    }
    fn seek(&mut self) {
        while !self.done {
            if self.values.iter().min() == Some(&self.low)
                && self.values.iter().max() == Some(&(self.low + self.span))
                && self
                    .rules
                    .iter()
                    .all(|r| r.allowed[(self.values[r.a] - self.values[r.b] + self.span) as usize])
            {
                return;
            }
            self.bump();
        }
    }
    fn advance(&mut self) {
        self.bump();
        self.seek();
    }
    fn reset(&mut self) {
        self.low = -self.span;
        self.done = false;
        self.values.fill(-self.span);
        self.values[0] = 0;
        self.seek();
    }
}
#[derive(Clone)]
enum Source {
    Cached {
        values: Arc<Vec<Vec<i32>>>,
        index: usize,
    },
    Lazy(Axis),
}
impl Source {
    fn values(&self) -> &[i32] {
        match self {
            Self::Cached { values, index } => &values[*index],
            Self::Lazy(a) => &a.values,
        }
    }
    fn advance(&mut self) -> bool {
        match self {
            Self::Cached { values, index } => {
                *index += 1;
                *index < values.len()
            }
            Self::Lazy(a) => {
                a.advance();
                !a.done
            }
        }
    }
    fn reset(&mut self) {
        match self {
            Self::Cached { index, .. } => *index = 0,
            Self::Lazy(a) => a.reset(),
        }
    }
    fn new(records: usize, span: i32, rules: &[Relation], cache: &mut AxisCache) -> Option<Self> {
        let count = (0..records).fold(1usize, |n, _| n.saturating_mul((span + 1) as usize));
        if count > 250_000 {
            return Axis::new(records, span, rules).map(Self::Lazy);
        }
        let key = (span, rules.to_vec());
        let values = if let Some(values) = cache.get(&key) {
            Arc::clone(values)
        } else {
            let mut rows = Vec::new();
            if let Some(mut a) = Axis::new(records, span, rules) {
                while !a.done {
                    rows.push(a.values.clone());
                    a.advance();
                }
            }
            let values = Arc::new(rows);
            cache.insert(key, Arc::clone(&values));
            values
        };
        (!values.is_empty()).then_some(Self::Cached { values, index: 0 })
    }
}
#[derive(Clone)]
pub(super) struct Generator {
    axes: Vec<(usize, Source)>,
    current: Offsets,
    done: bool,
    spans: Vec<i32>,
    differences: Vec<Difference>,
    tails: Vec<Vec<(i64, i64)>>,
}
impl Generator {
    pub fn new(
        records: usize,
        roles: usize,
        bands: usize,
        spans: &[i32],
        constraints: Constraints,
        cache: &mut AxisCache,
    ) -> Option<Self> {
        let (rules, differences) = constraints;
        let mut axes = Vec::new();
        for (i, &span) in spans.iter().enumerate() {
            if span > 0 {
                axes.push((i, Source::new(records, span, &rules[i], cache)?));
            }
        }
        let tails = differences
            .iter()
            .map(|d| {
                let mut tail = vec![(0, 0); axes.len() + 1];
                for (i, (axis, _)) in axes.iter().enumerate().rev() {
                    tail[i] = (
                        tail[i + 1].0 + d.ranges[*axis].iter().map(|r| r.0).min().unwrap(),
                        tail[i + 1].1 + d.ranges[*axis].iter().map(|r| r.1).max().unwrap(),
                    );
                }
                tail
            })
            .collect();
        let mut g = Self {
            axes,
            current: Offsets::zero(records, roles, bands),
            done: false,
            spans: spans.to_vec(),
            differences,
            tails,
        };
        for i in 0..g.axes.len() {
            g.sync(i);
        }
        Some(g)
    }
    fn sync(&mut self, i: usize) {
        let (axis, state) = &self.axes[i];
        let roles = self.current.ranks[0].len();
        let bands = self.current.gears[0].len();
        for (r, &v) in state.values().iter().enumerate() {
            if *axis < roles {
                self.current.ranks[r][*axis] = v;
            } else if *axis < roles + bands {
                self.current.gears[r][*axis - roles] = v;
            } else {
                self.current.bonus[r] = v * 5;
            }
        }
    }
    pub fn current(&self) -> Option<&Offsets> {
        (!self.done).then_some(&self.current)
    }
    fn rejected_prefix(&self) -> Option<usize> {
        let mut first = None;
        for (d, tail) in self.differences.iter().zip(&self.tails) {
            let (mut lo, mut hi) = (0, 0);
            for (i, (axis, source)) in self.axes.iter().enumerate() {
                let values = source.values();
                let delta = values[d.a] - values[d.b] + self.spans[*axis];
                let range = d.ranges[*axis][delta as usize];
                lo += range.0;
                hi += range.1;
                if lo + tail[i + 1].0 > d.target || hi + tail[i + 1].1 < d.target {
                    first = Some(first.map_or(i, |old: usize| old.min(i)));
                    break;
                }
            }
        }
        first
    }
    pub fn feasible(&self) -> bool {
        self.rejected_prefix().is_none()
    }
    pub fn advance(&mut self) {
        if self.axes.is_empty() {
            self.done = true;
            return;
        }
        let last = self.rejected_prefix().unwrap_or(self.axes.len() - 1);
        for i in (0..=last).rev() {
            if self.axes[i].1.advance() {
                self.sync(i);
                for j in i + 1..self.axes.len() {
                    self.axes[j].1.reset();
                    self.sync(j);
                }
                return;
            }
            self.axes[i].1.reset();
            self.sync(i);
        }
        self.done = true;
    }
}
pub(super) fn spans(maxima: &[i32], weights: &[i32], budget: i32, emit: &mut impl FnMut(&[i32])) {
    fn visit(
        i: usize,
        left: i32,
        maxima: &[i32],
        weights: &[i32],
        v: &mut [i32],
        emit: &mut impl FnMut(&[i32]),
    ) {
        if i == maxima.len() {
            if left == 0 {
                emit(v);
            }
            return;
        }
        if left
            > maxima[i..]
                .iter()
                .zip(&weights[i..])
                .map(|(a, b)| a * b)
                .sum()
        {
            return;
        }
        for x in 0..=maxima[i].min(left / weights[i]) {
            v[i] = x;
            visit(i + 1, left - x * weights[i], maxima, weights, v, emit);
        }
    }
    visit(0, budget, maxima, weights, &mut vec![0; maxima.len()], emit);
}
