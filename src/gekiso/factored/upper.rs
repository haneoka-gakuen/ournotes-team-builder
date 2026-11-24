//! Mean score ceilings over recorded lottery inputs, before order settlement.
use super::*;

pub(crate) struct OrdinaryUpper {
    pub windows: Vec<OrdinaryWindow>,
    pub command_limit: usize,
    pub absolute: f64,
    pub releases_without_time: usize,
}

#[derive(Clone, Copy)]
pub(crate) struct OrdinaryWindow {
    pub actor: usize,
    pub event: usize,
    pub start: usize,
    pub end: usize,
    pub grade: usize,
    pub delta: f64,
}

pub(super) struct ProjectionInputs {
    live: Arc<crate::master::LiveTables>,
    positions: Vec<usize>,
    score_frames: Option<Vec<usize>>,
    note_prefix: Vec<f64>,
    combo_range: Vec<bool>,
    judge: [Option<f64>; 7],
    combos: [Vec<(usize, f64)>; 2],
    converted: f64,
}

impl ProjectionInputs {
    fn new(master: &Master, chart: &Chart) -> Option<Self> {
        let adjustment = master.live.adjustment;
        let difficulty = chart.difficulty_factor;
        let converted = chart.full.converted as f64;
        if [adjustment, difficulty, converted].iter().any(|&value| !value.is_finite() || value <= 0.0) {
            return None;
        }
        let prefix = (adjustment.next_up() * difficulty).next_up();
        let note_prefix = chart.note_frac.iter().map(|&value| (prefix * value).next_up()).collect();
        let combo_range = chart.notes.iter().map(|note| chart.setup.fevers.iter().zip(&chart.setup.missions).any(
            |(&(start, end), &mission)| mission == 1 && (start..=end).contains(&note.time_ms),
        )).collect();
        let mut positions = vec![0; chart.full.notes.len() + 1];
        for (position, &(id, _)) in chart.prepared_notes().chronological().iter().enumerate() {
            positions[id as usize] = position;
        }
        let judge = std::array::from_fn(|grade| master.live.judge_percent.get(&(grade as i32))
            .map(|value| value / 100.0).filter(|&value| value.is_finite() && value >= 0.0));
        let combos = std::array::from_fn(|kind| {
            let mut rows = master.live.combo.get(&(kind as i32)).cloned().unwrap_or_default();
            rows.sort_unstable_by_key(|row| row.0);
            let mut maximum = 0.0_f64;
            for (_, value) in &mut rows {
                maximum = maximum.max(*value);
                *value = (1.0 + maximum.min(1.0)).next_up();
            }
            rows
        });
        let frame_count = Rewinds::size(chart.music_length) + 1;
        let frames = chart.play.frames.iter().map(|frame| Rewinds::frame_at(frame.time_ms, frame_count)).collect::<Vec<_>>();
        let score_frames = frames.windows(2).all(|pair| pair[0] <= pair[1]).then_some(frames);
        Some(Self { live: Arc::clone(&master.live), positions, score_frames, note_prefix, combo_range, judge, combos, converted })
    }

    fn for_chart(master: &Master, chart: &Chart) -> Option<Arc<Self>> {
        let mut cache = chart.factored.lock().unwrap();
        if let Some(inputs) = cache.projection_inputs.as_ref() {
            if Arc::ptr_eq(&inputs.live, &master.live) {
                return Some(Arc::clone(inputs));
            }
        }
        let inputs = Arc::new(Self::new(master, chart)?);
        cache.projection_inputs = Some(Arc::clone(&inputs));
        Some(inputs)
    }

    #[inline]
    fn combo(&self, kind: usize, count: usize) -> f64 {
        let rows = &self.combos[kind];
        let end = rows.partition_point(|&(minimum, _)| minimum <= count);
        end.checked_sub(1).map_or(1.0_f64.next_up(), |index| rows[index].1)
    }
}

pub(crate) struct Projection {
    sums: Vec<[[f64; 2]; 5]>,
    base: f64,
    control_magnitude: f64,
    minimum: f64,
    control_commands: usize,
    local_commands: usize,
    local_rewinds: bool,
    control_applications: usize,
    rewinds: usize,
    control_frames: Vec<usize>,
    ordinary_previous: Vec<usize>,
}
impl Projection {
    fn from_draws(master: &Master, chart: &Chart, draws: &[Arc<ScoreTape>]) -> Option<Self> {
        let mut projection = Self {
            sums: vec![[[0.0; 2]; 5]; chart.notes.len() + 1],
            base: 0.0,
            control_magnitude: 0.0,
            minimum: 0.0,
            control_commands: 0,
            local_commands: 0,
            local_rewinds: true,
            control_applications: 0,
            rewinds: 0,
            control_frames: chart
                .luck
                .then(|| vec![0; Rewinds::size(chart.music_length) + 1])
                .unwrap_or_default(),
            ordinary_previous: chart
                .luck
                .then(|| vec![0; chart.play.frames.len()])
                .unwrap_or_default(),
        };
        let inputs = ProjectionInputs::for_chart(master, chart)?;
        for draw in draws {
            draw.project(chart, &inputs, &mut projection)?;
        }
        for index in 1..projection.control_frames.len() {
            projection.control_frames[index] += projection.control_frames[index - 1];
        }
        for index in 1..projection.sums.len() {
            for grade in 0..5 {
                let weight = projection.sums[index][grade][0];
                let [below, above] = projection.sums[index - 1][grade];
                projection.sums[index][grade] =
                    [(below + weight).next_down(), (above + weight).next_up()];
            }
        }
        Some(projection)
    }

    fn append(&mut self, other: Self) {
        for (left, right) in self.sums.iter_mut().zip(other.sums) {
            for (left, right) in left.iter_mut().zip(right) {
                left[0] = (left[0] + right[0]).next_down();
                left[1] = (left[1] + right[1]).next_up();
            }
        }
        self.base = (self.base + other.base).next_up();
        self.control_magnitude = self.control_magnitude.max(other.control_magnitude);
        self.minimum = self.minimum.min(other.minimum);
        self.control_commands = self.control_commands.max(other.control_commands);
        self.local_commands = self.local_commands.max(other.local_commands);
        self.local_rewinds &= other.local_rewinds;
        self.control_applications = self.control_applications.max(other.control_applications);
        self.rewinds = self.rewinds.max(other.rewinds);
        for (left, right) in self.ordinary_previous.iter_mut().zip(other.ordinary_previous) {
            *left = (*left).max(right);
        }
        // Recover the per-frame maxima before joining the prefix totals.
        let (mut left_before, mut right_before, mut total) = (0, 0, 0);
        for (left, right) in self.control_frames.iter_mut().zip(other.control_frames) {
            total += (*left - left_before).max(right - right_before);
            left_before = *left;
            right_before = right;
            *left = total;
        }
    }

    fn round_base(&self, ordinary: &OrdinaryUpper, measured_rewinds: Option<usize>) -> Option<f64> {
        let commands = ordinary.command_limit;
        let unrestricted = if self.local_rewinds {
            ordinary.releases_without_time
        } else {
            commands
        };
        let coarse =
            self.control_commands * unrestricted + self.local_commands * (commands - unrestricted);
        let control_rewinds = measured_rewinds.map_or(coarse, |measured| coarse.min(measured));
        let applications =
            self.control_applications + control_rewinds + commands * (self.rewinds + 1 + commands);
        let n = (3.0 * applications as f64 * 2.0_f64.powi(-24)).next_up();
        if n >= 0.25 {
            return None;
        }
        // Applying and undoing a factor rounds both the accumulator and
        // the per-frame difference. Bound the error over the replay.
        let absolute = (self.control_magnitude + ordinary.absolute).next_up();
        let error = ((1.0 + 2.0 * absolute).next_up() * (n / (1.0 - n)).next_up()).next_up();
        if (1.0 + self.minimum).next_down() <= error {
            return None;
        }
        let error_weight = self.sums.last().unwrap()[0][1];
        Some((self.base + (error_weight * error).next_up()).next_up())
    }

    fn mean(
        &self,
        ordinary: &OrdinaryUpper,
        samples: usize,
        power: i32,
        orders: &[usize],
    ) -> Option<f64> {
        let base = self.round_base(ordinary, None)?;
        let shares = if orders.len() == 120 {
            [[0.2_f64.next_up(); 5]; 5]
        } else {
            let mut counts = [[0usize; 5]; 5];
            for &order in orders {
                for (event, &actor) in live::orders()[order].iter().enumerate() {
                    counts[event][actor] += 1;
                }
            }
            counts.map(|row| row.map(|count| (count as f64 / orders.len() as f64).next_up()))
        };
        let sum = ordinary.windows.iter().fold(base, |sum, row| {
            let weight =
                (self.sums[row.end][row.grade][1] - self.sums[row.start][row.grade][0]).next_up();
            let mean = (row.delta * shares[row.event][row.actor]).next_up();
            (sum + (weight * mean).next_up()).next_up()
        });
        let score = ((power as f64 * sum).next_up() / samples as f64).next_up();
        (score.is_finite() && score < i32::MAX as f64).then_some(score)
    }

    fn measured_mean(
        &self,
        chart: &Chart,
        ordinary: &OrdinaryUpper,
        measured: &OrdinaryOrders,
        samples: usize,
        power: i32,
        orders: &[usize],
    ) -> Option<f64> {
        let mut last_note = i32::MIN;
        let latest: Vec<_> = chart
            .play
            .frames
            .iter()
            .map(|frame| {
                for note in &frame.judged {
                    last_note = last_note.max(note.time_ms);
                }
                last_note
            })
            .collect();
        let end = self.sums.len() - 1;
        let mut average = 0.0_f64;
        let mut max_rewinds = 0usize;
        for &order in orders {
            let tape = measured[order].as_ref()?;
            if tape.len() > ordinary.command_limit {
                return None;
            }
            let mut bonus = 0.0_f64;
            let mut opens = Vec::new();
            let mut rewinds = 0usize;
            for &(frame, ref command) in tape.iter() {
                if let Some(&through) = self.ordinary_previous.get(frame) {
                    let first = Rewinds::frame_at(command.time, self.control_frames.len());
                    if first <= through {
                        rewinds += self.control_frames[through] - self.control_frames[first - 1];
                    }
                }
                if command.combo != 0
                    || command.luck != 0
                    || command.note != 0 && command.judge != 0
                {
                    return None;
                }
                let (value, grade) = if command.note != 0 {
                    (command.note, 0)
                } else if (3..=6).contains(&command.judgement) {
                    (command.judge, 7 - command.judgement as usize)
                } else {
                    continue;
                };
                if value == 0 {
                    continue;
                }
                let delta = f(f(value as f64) / 100000.0);
                let weight = if value > 0 {
                    opens.push((command.owner, grade, value, command.time));
                    let first = chart.times.partition_point(|&time| time < command.time);
                    (self.sums[end][grade][1] - self.sums[first][grade][0]).next_up()
                } else {
                    let match_index =
                        opens.iter().position(|&(owner, channel, amount, time)| {
                            owner == command.owner
                                && channel == grade
                                && amount == value.wrapping_neg()
                                && time <= command.time
                        })?;
                    opens.swap_remove(match_index);
                    // Notes delivered after the removal frame score with the
                    // negative command already installed. Other notes may
                    // retain the earlier factor after a rewind.
                    let through = latest.get(frame)?.saturating_add(1).max(command.time);
                    let first = chart.times.partition_point(|&time| time < through);
                    (self.sums[end][grade][0] - self.sums[first][grade][1])
                        .next_down()
                        .max(0.0)
                };
                bonus = (bonus + (weight * delta).next_up()).next_up();
            }
            max_rewinds = max_rewinds.max(rewinds);
            average = (average + (bonus / orders.len() as f64).next_up()).next_up();
        }
        let measured_rewinds = (!self.ordinary_previous.is_empty()).then_some(max_rewinds);
        let base = self.round_base(ordinary, measured_rewinds)?;
        let score =
            ((power as f64 * (base + average).next_up()).next_up() / samples as f64).next_up();
        (score >= 0.0 && score < i32::MAX as f64).then_some(score)
    }
}

struct FormationComponent {
    projection: Arc<Projection>,
    samples: usize,
    orders: usize,
    occurrences: [[usize; 5]; 5],
}

pub(crate) struct FormationProjection(Vec<FormationComponent>);

impl FormationProjection {
    pub(crate) fn base_coefficient(&self, ordinary: &OrdinaryUpper) -> Option<f64> {
        self.0.iter().try_fold(0.0_f64, |sum, group| {
            let total = group.projection.round_base(ordinary, None)?;
            let weight = (group.orders as f64 / (120 * group.samples) as f64).next_up();
            Some((sum + (total * weight).next_up()).next_up())
        })
    }

    pub(crate) fn window_coefficient(&self, actor: usize, windows: &[OrdinaryWindow]) -> f64 {
        self.0.iter().fold(0.0_f64, |sum, group| {
            let total = windows.iter().fold(0.0_f64, |subtotal, row| {
                let count = group.occurrences[row.event][actor];
                if count == 0 { return subtotal; }
                let bounds = &group.projection.sums;
                let weight = (bounds[row.end][row.grade][1] - bounds[row.start][row.grade][0]).next_up();
                let factor = (row.delta * count as f64).next_up();
                (subtotal + (weight * factor).next_up()).next_up()
            });
            (sum + (total / (120 * group.samples) as f64).next_up()).next_up()
        })
    }
}

pub(crate) fn formation_projection(
    master: &Master,
    chart: &Chart,
    deck: &[Performer],
    seeds: &[i32],
    wide_domain: bool,
) -> Result<Option<FormationProjection>, String> {
    if !valid_chart(master, chart, seeds) {
        return Ok(None);
    }
    let Some(profiles) = scoring_profiles(master, chart, deck, seeds)? else {
        return Ok(None);
    };
    let Some(groups) = control_order_groups(&control_classes(master, chart, deck, &profiles), 5) else {
        return Ok(None);
    };
    if groups.len() > 1 && !wide_domain {
        return Ok(None);
    }
    let (control, _) = views(master, chart);
    let control_deck = control_deck(master, &control, chart, deck, seeds, &profiles);
    let mut samples = SampleCache::for_groups(chart, groups.len());
    let mut components = Vec::with_capacity(groups.len());
    for orders in groups {
        let Some(scene) = context(master, &control, chart, &control_deck,
            &live::orders()[orders[0]], &mut samples, None)?
        else { return Ok(None); };
        let Some(projection) = scene.projection
            .get_or_init(|| Projection::from_draws(master, chart, &scene.draws).map(Arc::new))
        else { return Ok(None); };
        let mut occurrences = [[0; 5]; 5];
        for &index in &orders {
            for (event, &actor) in live::orders()[index].iter().enumerate() {
                occurrences[event][actor] += 1;
            }
        }
        components.push(FormationComponent {
            projection: Arc::clone(projection),
            samples: scene.draws.len(),
            orders: orders.len(),
            occurrences,
        });
    }
    Ok(Some(FormationProjection(components)))
}

#[derive(Clone, Copy, Default)]
struct NoteUpper {
    score: Option<(i32, i32)>,
    gekiso: i32,
    rank: Option<RankControl>,
}

#[derive(Clone, Copy)]
enum RankControl {
    PendingNote,
    Scored { note: f64, luck: i64 },
}

#[derive(Clone, Copy, Default)]
struct FactorHistory {
    note: [f64; 2],
    note_extremes: [f64; 2],
    luck: i64,
    luck_extremes: [i64; 2],
    changed: bool,
}
impl FactorHistory {
    fn new(note: f64, luck: i32) -> Self {
        let luck = i64::from(luck);
        Self {
            note: [note, note],
            note_extremes: [note.min(0.0), note.max(0.0)],
            luck,
            luck_extremes: [luck.min(0), luck.max(0)],
            changed: true,
        }
    }

    fn apply(&mut self, next: Self) {
        self.note_extremes[0] =
            self.note_extremes[0].min((self.note[0] + next.note_extremes[0]).next_down());
        self.note_extremes[1] =
            self.note_extremes[1].max((self.note[1] + next.note_extremes[1]).next_up());
        self.note = [
            (self.note[0] + next.note[0]).next_down(),
            (self.note[1] + next.note[1]).next_up(),
        ];
        self.luck_extremes[0] = self.luck_extremes[0].min(self.luck + next.luck_extremes[0]);
        self.luck_extremes[1] = self.luck_extremes[1].max(self.luck + next.luck_extremes[1]);
        self.luck += next.luck;
        self.changed = true;
    }
}

/// Batch changes for whole blocks of notes and retain the intermediate
/// extremes needed by notes that were already present.
struct FactorTimeline {
    notes: Vec<FactorHistory>,
    pending: Vec<FactorHistory>,
}
impl FactorTimeline {
    const BLOCK: usize = 32;

    fn new(notes: usize) -> Self {
        Self {
            notes: vec![FactorHistory::default(); notes],
            pending: vec![FactorHistory::default(); notes.div_ceil(Self::BLOCK)],
        }
    }

    fn flush(&mut self, block: usize) {
        let pending = std::mem::take(&mut self.pending[block]);
        if pending.changed {
            let start = block * Self::BLOCK;
            let end = (start + Self::BLOCK).min(self.notes.len());
            for note in &mut self.notes[start..end] {
                note.apply(pending);
            }
        }
    }

    fn activate(&mut self, index: usize) {
        self.flush(index / Self::BLOCK);
        let note = &mut self.notes[index];
        note.note_extremes = note.note;
        note.luck_extremes = [note.luck; 2];
    }

    fn current(&mut self, index: usize) -> FactorHistory {
        self.flush(index / Self::BLOCK);
        self.notes[index]
    }

    fn push(&mut self, start: usize, change: FactorHistory) {
        let following = start.div_ceil(Self::BLOCK);
        if start < self.notes.len() && start % Self::BLOCK != 0 {
            self.flush(start / Self::BLOCK);
            let end = (following * Self::BLOCK).min(self.notes.len());
            for note in &mut self.notes[start..end] {
                note.apply(change);
            }
        }
        for pending in &mut self.pending[following..] {
            pending.apply(change);
        }
    }

    fn finish(mut self) -> Vec<FactorHistory> {
        for block in 0..self.pending.len() {
            self.flush(block);
        }
        self.notes
    }
}

#[derive(Clone, Copy, Default)]
struct FactorSum {
    total: [f64; 2],
    prefix: [f64; 2],
}

struct FactorMagnitude {
    leaves: usize,
    sums: Vec<FactorSum>,
    frame_magnitudes: Vec<f64>,
    prefix_peak: f64,
    frame_peak: f64,
}
impl FactorMagnitude {
    fn new(frames: usize) -> Self {
        let leaves = frames.next_power_of_two();
        Self {
            leaves,
            sums: vec![FactorSum::default(); 2 * leaves],
            frame_magnitudes: vec![0.0; frames],
            prefix_peak: 0.0,
            frame_peak: 0.0,
        }
    }

    fn push(&mut self, frame: usize, delta: f64) {
        let magnitude = &mut self.frame_magnitudes[frame];
        *magnitude = (*magnitude + delta.abs()).next_up();
        self.frame_peak = self.frame_peak.max(*magnitude);
        let mut index = self.leaves + frame;
        let leaf = &mut self.sums[index];
        let [low, high] = leaf.total;
        leaf.total = [(low + delta).next_down(), (high + delta).next_up()];
        leaf.prefix = [leaf.total[0].min(0.0), leaf.total[1].max(0.0)];
        while index > 1 {
            index /= 2;
            let (left, right) = (self.sums[index * 2], self.sums[index * 2 + 1]);
            self.sums[index] = FactorSum {
                total: [
                    (left.total[0] + right.total[0]).next_down(),
                    (left.total[1] + right.total[1]).next_up(),
                ],
                prefix: [
                    left.prefix[0].min((left.total[0] + right.prefix[0]).next_down()),
                    left.prefix[1].max((left.total[1] + right.prefix[1]).next_up()),
                ],
            };
        }
        let [low, high] = self.sums[1].prefix;
        self.prefix_peak = self.prefix_peak.max(high.max(-low));
    }

    fn upper(&self) -> f64 {
        // Two updates can have the same score frame. A partial application
        // can differ from the frame prefix by at most its absolute updates.
        (self.prefix_peak + self.frame_peak).next_up()
    }
}

struct Rewinds {
    previous: usize,
    before_previous: usize,
    first_input: usize,
    counts: Vec<i32>,
    factors: Vec<usize>,
    local: bool,
}
impl Rewinds {
    fn size(music_length: i32) -> usize {
        (core::frame(music_length) + 50).max(1) as usize + 1
    }

    fn new(music_length: i32) -> Self {
        let frames = Self::size(music_length);
        Self {
            previous: 0,
            before_previous: 0,
            first_input: usize::MAX,
            counts: vec![0; frames + 1],
            factors: Vec::new(),
            local: true,
        }
    }

    fn frame(&self, time: i32) -> usize {
        Self::frame_at(time, self.counts.len())
    }
    fn frame_at(time: i32, len: usize) -> usize {
        (core::frame(time) as usize + 1).min(len - 2)
    }

    fn input(&mut self, time: i32, factor: bool) {
        let frame = self.frame(time);
        self.first_input = self.first_input.min(frame);
        if factor {
            self.factors.push(frame);
        }
    }

    fn calculate(&mut self, time: i32) {
        self.calculate_frame(self.frame(time));
    }

    fn calculate_frame(&mut self, to: usize) {
        let first = self.first_input.min(to + 1);
        if first <= self.previous {
            self.counts[first] += 1;
            self.counts[self.previous + 1] -= 1;
        }
        self.before_previous = self.previous;
        self.previous = to;
        self.first_input = usize::MAX;
    }

    fn ordinary(&mut self, time: i32, frame: usize, projection: &mut Projection) {
        self.local &= self.previous <= self.frame(time);
        if let Some(before) = projection.ordinary_previous.get_mut(frame) {
            *before = (*before).max(self.previous);
        }
    }

    fn measure(mut self, projection: &mut Projection) {
        let (mut coverage, mut max) = (0, 0);
        for cell in &mut self.counts {
            coverage += *cell;
            *cell = coverage;
            max = max.max(coverage);
        }
        // Each control factor is applied once plus once after a rewind over
        // its native score frame. Ordinary commands add their own rewinds.
        let applications = self
            .factors
            .iter()
            .map(|&frame| self.counts[frame] as usize + 1)
            .sum::<usize>();
        projection.control_applications = projection.control_applications.max(applications);
        projection.control_commands = projection.control_commands.max(self.factors.len());
        projection.rewinds = projection.rewinds.max(max as usize);
        projection.local_rewinds &= self.local;
        if self.factors.is_empty() { return; }
        self.factors.sort_unstable();
        let mut start = 0;
        let mut same_frame = 0;
        for (end, &frame) in self.factors.iter().enumerate() {
            if frame != self.factors[same_frame] { same_frame = end; }
            if let Some(maximum) = projection.control_frames.get_mut(frame) {
                *maximum = (*maximum).max(end - same_frame + 1);
            }
            if self.counts.len() >= 4 {
                while frame - self.factors[start] >= 4 { start += 1; }
                projection.local_commands = projection.local_commands.max(end - start + 1);
            }
        }
    }
}

impl ScoreTape {
    fn project(
        &self,
        chart: &Chart,
        inputs: &ProjectionInputs,
        projection: &mut Projection,
    ) -> Option<()> {
        let positions = &inputs.positions;
        let mut notes = vec![NoteUpper::default(); chart.full.notes.len() + 1];
        let timeline = chart.prepared_notes().chronological();
        let mut factors = FactorTimeline::new(timeline.len());
        let mut combo = ComboPlayback {
            ranges: &chart.setup.fevers,
            missions: &chart.setup.missions,
            history: std::array::from_fn(|_| Vec::new()),
        };
        use crate::full::score::GekisoCombo;
        let (mut at, mut frame) = (0, 0);
        let mut rewinds = Rewinds::new(chart.music_length);
        let mut magnitude = None;
        let mut latest = i32::MIN;
        let mut luck_absolute = 0_i64;
        let mut last_command = u8::MAX;
        let mut consecutive_calculations = 0usize;
        while at < self.bytes.len() {
            let op = self.bytes[at];
            let prior_calculations = consecutive_calculations;
            consecutive_calculations = if matches!(op, 3 | 5 | 6) {
                prior_calculations + 1
            } else {
                0
            };
            last_command = op;
            at += 1;
            match op {
                0 => at += chart.play.frames[frame].judged.len(),
                1 => {
                    let id = self.i32(&mut at);
                    let life = self.i32(&mut at);
                    let grade = i32::from(self.bytes[at]);
                    at += 1;
                    let chart_note = &chart.full.notes[(id - 1) as usize];
                    rewinds.input(chart_note.time_ms, false);
                    let note = &mut notes[id as usize];
                    if note.score.replace((grade, life)).is_some() {
                        return None;
                    }
                    factors.activate(positions[id as usize]);
                    latest = latest.max(chart_note.time_ms);
                    note.gekiso = combo.combo(chart_note.time_ms).unwrap_or(0);
                    let op = chart_note.op;
                    if !play::is_judged(op) && *inputs.live.note_percent.get(&op)? != 0.0 {
                        return None;
                    }
                }
                2 => {
                    let time = self.i32(&mut at);
                    let _owner = self.i32(&mut at);
                    let note = self.i32(&mut at);
                    let combo = self.i32(&mut at);
                    let judgement = self.i32(&mut at);
                    let judge = self.i32(&mut at);
                    let luck = self.i32(&mut at);
                    if combo != 0 || judgement != 0 || judge != 0 {
                        return None;
                    }
                    rewinds.input(time, note != 0);
                    if note == 0 && luck == 0 {
                        continue;
                    }
                    let delta = f(f(note as f64) / 100000.0);
                    if note != 0 {
                        let (magnitude, frames) = magnitude.get_or_insert_with(|| {
                            let frames = chart.luck.then(|| {
                                let mut frames: Vec<_> = self.factor_times.iter().map(|&time| rewinds.frame(time)).collect();
                                frames.sort_unstable();
                                frames.dedup();
                                frames
                            });
                            let size = frames.as_ref().map_or(rewinds.counts.len(), Vec::len);
                            (FactorMagnitude::new(size), frames)
                        });
                        let frame = rewinds.frame(time);
                        let index = match frames {
                            Some(frames) => frames.binary_search(&frame).ok()?,
                            None => frame,
                        };
                        magnitude.push(index, delta);
                    }
                    let first = timeline.partition_point(|&(_, at)| at < time);
                    luck_absolute += i64::from(luck).abs();
                    if luck_absolute > i64::from(i32::MAX - 100) {
                        return None;
                    }
                    factors.push(first, FactorHistory::new(delta, luck));
                }
                3 => {
                    let time = self.i32(&mut at);
                    rewinds.calculate(time);
                }
                4 => {
                    let range = self.bytes[at] as usize;
                    at += 1;
                    let end = self.i32(&mut at);
                    let percent = self.i32(&mut at);
                    if chart.setup.fevers.get(range)?.1 != end
                        || chart.range_percent.get(range).copied()? != percent
                    {
                        return None;
                    }
                    let start_frame = rewinds.frame(chart.setup.fevers[range].0);
                    let end_frame = rewinds.frame(end);
                    if chart.luck
                        && chart.setup.missions.iter().all(|&mission| mission == 2)
                        && prior_calculations >= 2
                        && rewinds.before_previous == start_frame
                        && rewinds.previous == end_frame
                        && rewinds.first_input == usize::MAX
                    {
                        let first = timeline
                            .partition_point(|&(_, time)| rewinds.frame(time) <= start_frame);
                        let last =
                            timeline.partition_point(|&(_, time)| rewinds.frame(time) <= end_frame);
                        // The calculations bracket the rank subtotal before
                        // later factors can change the performance score.
                        for (index, &(id, _)) in timeline.iter().enumerate().take(last).skip(first)
                        {
                            let note = &mut notes[id as usize];
                            if note.rank.is_some() {
                                return None;
                            }
                            let rank = if note.score.is_some() {
                                let factor = factors.current(index);
                                RankControl::Scored {
                                    note: factor.note[1],
                                    luck: factor.luck,
                                }
                            } else {
                                RankControl::PendingNote
                            };
                            note.rank = Some(rank);
                        }
                    }
                }
                5 | 6 => {
                    let position = frame - usize::from(op == 6);
                    if let Some(frames) = &inputs.score_frames {
                        rewinds.calculate_frame(frames[position]);
                    } else {
                        rewinds.calculate(chart.play.frames[position].time_ms);
                    }
                }
                7 => {
                    let count =
                        u16::from_le_bytes(self.bytes[at..at + 2].try_into().unwrap()) as usize;
                    at += 2;
                    let end = frame + count;
                    if let Some(frames) = &inputs.score_frames {
                        // Only the first calculation can revisit pending input.
                        // The remaining frames advance monotonically.
                        let covered = &frames[frame..end];
                        rewinds.calculate_frame(*covered.first()?);
                        if !projection.ordinary_previous.is_empty() {
                            for (maximum, &position) in projection.ordinary_previous[frame..end].iter_mut().zip(covered) {
                                *maximum = (*maximum).max(position);
                            }
                        }
                        rewinds.previous = *covered.last()?;
                        rewinds.before_previous = rewinds.previous;
                    } else {
                        for (position, row) in chart.play.frames.iter().enumerate().take(end).skip(frame) {
                            rewinds.calculate(row.time_ms);
                            rewinds.ordinary(row.time_ms, position, projection);
                            rewinds.before_previous = rewinds.previous;
                        }
                    }
                    frame = end;
                }
                8 => {
                    let range = self.bytes[at] as usize;
                    at += 1;
                    let from = self.i32(&mut at) as usize;
                    let len = self.i32(&mut at) as usize;
                    let row = &mut combo.history[range];
                    let begin = row.get(from).map(|&(time, _)| time);
                    row.truncate(from);
                    row.extend((0..len).map(|_| (self.i32(&mut at), self.i32(&mut at))));
                    if let Some(begin) = begin
                        .into_iter()
                        .chain(row.get(from).map(|&(time, _)| time))
                        .min()
                    {
                        let start = timeline.partition_point(|&(_, time)| time < begin);
                        let through = latest.min(chart.setup.fevers[range].1);
                        for &(id, time) in timeline[start..]
                            .iter()
                            .take_while(|&&(_, time)| time <= through)
                        {
                            let note = &mut notes[id as usize];
                            if note.score.is_some() {
                                note.gekiso = note.gekiso.max(combo.combo(time).unwrap_or(0));
                            }
                        }
                    }
                }
                16..=31 => {
                    rewinds.ordinary(chart.play.frames[frame].time_ms, frame, projection);
                    at += (op & 15).count_ones() as usize * 4;
                    frame += 1;
                }
                _ => return None,
            }
        }
        let settled = matches!(last_command, 3 | 5 | 6 | 7)
            && rewinds.first_input == usize::MAX
            && rewinds.previous >= rewinds.frame(latest);
        rewinds.measure(projection);
        let factors = factors.finish();
        if let Some((magnitude, _)) = magnitude {
            projection.control_magnitude = projection.control_magnitude.max(magnitude.upper());
        }
        let rounding = (1.0 + f32::EPSILON as f64).powi(32);
        for (index, note) in chart.notes.iter().enumerate() {
            let id = note.id as usize;
            let state = notes[id];
            let Some((grade, life)) = state.score else {
                continue;
            };
            let factor = factors[positions[id]];
            if factor.luck_extremes[0] < -100 {
                return None;
            }
            projection.minimum = projection.minimum.min(factor.note_extremes[0]);
            let judge = inputs.judge.get(grade as usize).copied().flatten()?;
            let combo = self.combos.values[id];
            let combo = if combo >= 0 {
                combo
            } else {
                self.combos.changes[(-combo - 1) as usize]
                    .iter()
                    .map(|&(_, count)| count)
                    .max()?
            };
            let combo = inputs.combo(0, combo as usize);
            let gekiso = if inputs.combo_range[index] {
                inputs.combo(1, state.gekiso.max(0) as usize)
            } else {
                1.0
            };
            let rush = |points: i64| ((100 + points).clamp(100, 200) as f64 / 100.0).next_up();
            let peak_luck = rush(factor.luck_extremes[1]);
            let final_luck = if settled {
                rush(factor.luck)
            } else {
                peak_luck
            };
            let life = if life > 0 { 1.0 } else { inputs.live.life_onus };
            let range = chart.range_of[index];
            let rank = if range < 0 {
                0.0
            } else {
                (chart.range_percent[range as usize] as f64 / 100.0).next_up()
            };
            let product = [judge, combo, gekiso, life, rounding].into_iter()
                .fold(inputs.note_prefix[index], |product, term| (product * term).next_up());
            let base = (product / inputs.converted).next_up();
            let performance = (base * final_luck).next_up();
            let peak = (1.0 + factor.note_extremes[1].max(0.0)).next_up();
            let (rank_luck, rank_factor) = match state.rank {
                Some(RankControl::Scored { note, luck }) => {
                    (rush(luck), (1.0 + note.max(0.0)).next_up())
                }
                Some(RankControl::PendingNote) => (0.0, 0.0),
                None => (peak_luck, peak),
            };
            let reward = ((base * rank_luck).next_up() * rank).next_up();
            let weight = (performance + reward).next_up();
            let sums = &mut projection.sums[index + 1];
            sums[0][0] = (sums[0][0] + weight).next_up();
            if (1..=4).contains(&grade) {
                let slot = &mut sums[grade as usize][0];
                *slot = (*slot + weight).next_up();
            }
            // Later factors force a replay of their score frames. The rank
            // subtotal still uses the values recorded when it was awarded.
            let final_factor = if settled {
                (1.0 + factor.note[1].max(0.0)).next_up()
            } else {
                peak
            };
            projection.base = ((projection.base + (performance * final_factor).next_up())
                .next_up()
                + (reward * rank_factor).next_up())
            .next_up();
        }
        Some(())
    }
}

pub(crate) struct SampleCeilings<'a> {
    pub(crate) orders: &'a [f64; 120],
    pub(crate) with_life: Option<&'a dyn Fn(i32, usize) -> f64>,
}

pub(crate) fn mean_upper(
    master: &Master,
    chart: &Chart,
    deck: &[Performer],
    seeds: &[i32],
    power: i32,
    ordinary: &OrdinaryUpper,
    limit: Option<f64>,
    order_ceilings: Option<SampleCeilings<'_>>,
) -> Result<Option<f64>, String> {
    if !valid_chart(master, chart, seeds) || power <= 0 {
        return Ok(None);
    }
    let Some(profiles) = scoring_profiles(master, chart, deck, seeds)? else {
        return Ok(None);
    };
    mean_with_profiles(master, chart, deck, seeds, power, ordinary, limit, order_ceilings, profiles)
}

fn valid_chart(master: &Master, chart: &Chart, seeds: &[i32]) -> bool {
    if chart.luck && seeds.len() < 8 {
        return false;
    }
    let mut cache = chart.factored.lock().unwrap();
    if let Some((tables, valid)) = &cache.projection_eligibility {
        if Arc::ptr_eq(tables, &master.live) {
            return *valid;
        }
    }
    let valid = !(chart
        .setup
        .missions
        .iter()
        .any(|&mission| !matches!(mission, 1 | 2))
        || chart.range_percent.iter().any(|&percent| percent < 0)
        || !(0.0..=1.0).contains(&master.live.life_onus)
        || chart
            .setup
            .fevers
            .windows(2)
            .any(|ranges| core::frame(ranges[0].1) >= core::frame(ranges[1].0))
        || chart
            .notes
            .iter()
            .any(|note| note.time_ms < 0 || note.time_ms > chart.music_length)
        || chart
            .note_frac
            .iter()
            .any(|&value| !value.is_finite() || value < 0.0)
        || master
            .live
            .combo
            .values()
            .flatten()
            .any(|&(_, value)| !value.is_finite() || value < 0.0));
    cache.projection_eligibility = Some((Arc::clone(&master.live), valid));
    valid
}

fn mean_with_profiles(
    master: &Master,
    chart: &Chart,
    deck: &[Performer],
    seeds: &[i32],
    power: i32,
    ordinary: &OrdinaryUpper,
    limit: Option<f64>,
    order_ceilings: Option<SampleCeilings<'_>>,
    profiles: Vec<(bool, bool)>,
) -> Result<Option<f64>, String> {
    let Some(groups) = control_order_groups(&control_classes(master, chart, deck, &profiles), 5) else {
        return Ok(None);
    };
    let (control, ordinary_master) = views(master, chart);
    let mut samples = SampleCache::for_groups(chart, groups.len());
    let mut combined = 0.0_f64;
    let sample_count = seeds.len() as f64;
    let sample_caps = if let Some((ceilings, limit)) = order_ceilings
        .filter(|values| values.orders.iter().all(|value| value.is_finite() && *value >= 0.0))
        .zip(limit.filter(|limit| chart.luck && limit.is_finite() && *limit > 0.0))
    {
        let caps: Vec<_> = groups.iter().map(|orders|
            orders.iter().fold(0.0_f64, |sum, &order| (sum + ceilings.orders[order]).next_up())
        ).collect();
        let mut tail = vec![0.0_f64; caps.len() + 1];
        for index in (0..caps.len()).rev() {
            tail[index] = (tail[index + 1] + caps[index]).next_up();
        }
        let mean = (tail[0] / 120.0).next_up();
        if mean < limit {
            return Ok(Some(mean));
        }
        Some((limit, caps, tail, ceilings.with_life))
    } else {
        None
    };
    let control_deck = control_deck(master, &control, chart, deck, seeds, &profiles);
    for (index, orders) in groups.iter().enumerate() {
        const BATCH: usize = 8;
        let mut certified = None;
        let mut projected = 0.0_f64;
        let mut processed = 0usize;
        let mut projected_draws: Option<Projection> = None;
        let mut batch = Vec::with_capacity(BATCH);
        let mut active = sample_caps.as_ref().is_some_and(|(limit, _, tail, _)| {
            (*limit * 120.0 * 2.0) >= combined + tail[index]
        });
        let mut group_cap = sample_caps.as_ref().map_or(f64::INFINITY, |(_, caps, _, _)| caps[index]);
        let with_life = sample_caps.as_ref().and_then(|(_, _, _, apply)| *apply);
        let mut checked_life = false;
        let mut observe = |draw: &Arc<ScoreTape>, shared_inputs: bool| {
            if shared_inputs && !checked_life {
                checked_life = true;
                if let Some(apply) = with_life {
                    let (limit, _, tail, _) = sample_caps.as_ref().unwrap();
                    let upper = orders.iter().fold(0.0_f64, |sum, &order| {
                        (sum + apply(draw.living_through, order)).next_up()
                    });
                    group_cap = group_cap.min(upper);
                    let sum = ((combined + group_cap).next_up() + tail[index + 1]).next_up();
                    let mean = (sum / 120.0).next_up();
                    if mean < *limit {
                        certified = Some(mean);
                        return Ok(false);
                    }
                    active |= *limit * 240.0 >= sum;
                }
            }
            if !active || processed + BATCH >= seeds.len() {
                return Ok(true);
            }
            batch.push(Arc::clone(draw));
            if batch.len() < BATCH {
                return Ok(true);
            }
            let Some((bounds, score)) = Projection::from_draws(master, chart, &batch)
                .and_then(|bounds| bounds.mean(ordinary, BATCH, power, orders).map(|score| (bounds, score)))
            else {
                active = false;
                return Ok(true);
            };
            batch.clear();
            processed += BATCH;
            let (limit, _, tail, _) = sample_caps.as_ref().unwrap();
            let contribution = ((score * orders.len() as f64).next_up() * BATCH as f64).next_up();
            projected = (projected + contribution).next_up();
            let remaining = (group_cap * (seeds.len() - processed) as f64).next_up();
            let current = ((projected + remaining).next_up() / sample_count).next_up();
            let sum = ((combined + current).next_up() + tail[index + 1]).next_up();
            let mean = (sum / 120.0).next_up();
            if mean < *limit {
                certified = Some(mean);
                return Ok(false);
            }
            if let Some(projected) = &mut projected_draws {
                projected.append(bounds);
            } else {
                projected_draws = Some(bounds);
            }
            Ok(true)
        };
        let observer = sample_caps.is_some().then_some(&mut observe as &mut DrawObserver<'_>);
        let Some(scene) = context(
            master,
            &control,
            chart,
            &control_deck,
            &live::orders()[orders[0]],
            &mut samples,
            observer,
        )?
        else {
            return Ok(certified);
        };
        let Some(projection) = scene.projection.get_or_init(|| {
            let tail = Projection::from_draws(master, chart, &scene.draws[processed..])?;
            if let Some(mut prefix) = projected_draws {
                prefix.append(tail);
                Some(Arc::new(prefix))
            } else {
                Some(Arc::new(tail))
            }
        }).as_ref()
        else {
            return Ok(None);
        };
        let mut upper = projection.mean(ordinary, scene.draws.len(), power, orders);
        if scene.shared_inputs && limit.is_some_and(|limit| {
            if chart.luck {
                groups.len() > 1 || upper.is_none_or(|upper| upper >= limit)
            } else {
                groups.len() == 1 && upper.is_some_and(|upper| upper >= limit && upper <= 1.25 * limit)
            }
        }) {
            // Record only the ordinary factors for one sample. Every
            // remaining seed in this control group reuses the same inputs.
            let branch = OrderBranch::for_chart(chart, orders)?;
            let mut measured = std::array::from_fn(|_| None);
            if score_orders::<false>(
                master, chart, deck, &scene.draws[0], power, (orders, &branch),
                (&ordinary_master, Some(&mut measured), &mut None), (0, &mut None, None), None,
            )?.is_some() {
                let refined = projection.measured_mean(
                    chart, ordinary, &measured, scene.draws.len(), power, orders,
                );
                upper = upper.into_iter().chain(refined).min_by(f64::total_cmp);
                if chart.luck && measured.iter().flatten().map(|tape| tape.len()).sum::<usize>() <= 8192 {
                    chart.factored.lock().unwrap().measured_ordinary = Some(MeasuredOrdinary {
                        input: Arc::clone(&scene.draws[0]),
                        deck: deck.to_vec(),
                        orders: orders.clone(),
                        factors: measured,
                    });
                }
            }
        }
        let Some(upper) = upper else {
            return Ok(None);
        };
        combined = (combined + group_cap.min((upper * orders.len() as f64).next_up())).next_up();
        if let Some((limit, _, tail, _)) = &sample_caps {
            let mean = ((combined + tail[index + 1]).next_up() / 120.0).next_up();
            if mean < *limit { return Ok(Some(mean)); }
        }
    }
    Ok(Some((combined / 120.0).next_up()))
}
