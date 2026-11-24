//! A single physical formation optimized over a complete set of Gekiso charts.
use super::*;

const SETTLEMENT_DEADLINE: &str = "shared-score-deadline";
const EXACT_POWER_COMPONENT: f64 = (1u64 << 52) as f64;

pub(crate) struct SharedInput<'a> {
    pub env: Environment<'a>,
    pub charts: &'a mut [Chart],
    pub music: &'a [Music],
    pub seeds: Vec<i32>,
    pub k: usize,
    pub deadline: f64,
    pub initial: Vec<Team>,
    pub shard: Option<(usize, usize)>,
    pub min_combo_bonus: i32,
    pub cooperative_floor: bool,
    pub seeded: bool,
    pub defer_warm_start: bool,
    pub floor: f64,
    pub on_candidate: Option<CandidateListener<'a>>,
    pub sections: Option<&'a sections::Plan>,
}

struct SongObjective<'k, 'a> {
    adapter: Adapter<'k, 'a>,
    music: &'a Music,
    bases: Arc<Vec<power::SlotBase>>,
    snap_power: Arc<Vec<f64>>,
    exact_snapshot_power: bool,
    weights: Vec<f64>,
    power_cap: f64,
    power_domain: usize,
    power_gap: Option<f64>,
    base: f64,
    roster_bonus: f64,
    slots: Arc<Vec<Vec<f64>>>,
    exact_prepared_power: bool,
    power_source: usize,
    member_power: Arc<Vec<f64>>,
    member_skill: Vec<f64>,
    free_power: Arc<Vec<[f64; 5]>>,
    free_skill: Vec<[f64; 5]>,
    power_choices: Arc<Vec<Vec<(i32, f64)>>>,
    skill_choices: Vec<Vec<(i32, f64)>>,
}

struct PowerBasis {
    bases: Arc<Vec<power::SlotBase>>,
    snaps: Arc<Vec<f64>>,
    exact: bool,
    variable_caps: Vec<f64>,
}

struct PowerSeed {
    bonuses: Vec<i32>,
    weights: Vec<f64>,
    width: usize,
    scale: f64,
}
impl PowerSeed {
    fn new(bonuses: Vec<i32>) -> Self {
        Self { bonuses, weights: Vec::new(), width: 0, scale: 0.0 }
    }

    fn weight(&self, member: usize, snap: i32) -> f64 {
        if self.weights.is_empty() { return 0.0; }
        let column = if snap < 0 { self.width - 1 } else { snap as usize };
        self.weights[member * self.width + column]
    }

}
struct WindowBudget {
    required: f64,
    values: [Vec<f64>; 5],
    choices: [[Vec<(i32, f64)>; 2]; 5],
}
struct PlainScoreBound {
    coefficient: f64,
    offset: f64,
    terms: Vec<[f64; 2]>,
    offers: [[Vec<(i32, f64)>; 2]; 5],
}
impl PlainScoreBound {
    fn new(width: usize) -> Self {
        Self {
            coefficient: 0.0,
            offset: 0.0,
            terms: vec![[0.0; 2]; 5 * width],
            offers: std::array::from_fn(|_| [Vec::new(), Vec::new()]),
        }
    }

    fn add_chart(&mut self, constant: f64, rows: &[f64], difference: f64, allowance: f64, count: f64) {
        self.coefficient = (self.coefficient + (constant / count).next_up()).next_up();
        self.offset = ((self.offset + (difference * constant).next_up()).next_up()
            + (allowance / count).next_up()).next_up();
        for (term, &row) in self.terms.iter_mut().zip(rows) {
            term[0] = (term[0] + (row / count).next_up()).next_up();
            term[1] = (term[1] + (difference * row).next_up()).next_up();
        }
    }

    fn prepare(&mut self, options: &[Vec<i32>]) {
        let width = self.terms.len() / 5;
        self.offers = std::array::from_fn(|slot| std::array::from_fn(|term| {
            Combined::best_offers(&options[slot], |snap| self.terms[slot * width + (snap + 1) as usize][term])
        }));
    }

    fn upper(&self, power: f64, assigned: &[i32], from: usize) -> f64 {
        let width = self.terms.len() / 5;
        let (mut coefficient, mut offset) = (self.coefficient, self.offset);
        for (slot, offers) in self.offers.iter().enumerate() {
            let [weight, extra] = if slot < from {
                self.terms[slot * width + (assigned[slot] + 1) as usize]
            } else {
                let used = &assigned[..from];
                [
                    Combined::first_available(&offers[0], used),
                    Combined::first_available(&offers[1], used),
                ]
            };
            coefficient = (coefficient + weight).next_up();
            offset = (offset + extra).next_up();
        }
        ((power * coefficient).next_up() + offset).next_up()
    }
}

struct SharedPowerBound {
    songs: Vec<usize>,
    bound: crate::rewards::FixedRegionBound,
    base_power: f64,
    coefficient: f64,
    profiles: Vec<[f64; 5]>,
    profile_choices: [Vec<(i32, f64)>; 5],
    offset: f64,
    offsets: [Vec<f64>; 5],
    offset_choices: [Vec<(i32, f64)>; 5],
    prices_offsets: bool,
    prices: Vec<SharedPrice>,
    plain_score: Option<PlainScoreBound>,
}
pub(super) struct SharedPrice {
    base: f64,
    fixed: Vec<[f64; 5]>,
    offers: [Vec<(i32, f64)>; 5],
    charges: Vec<(i32, f64)>,
    assignment: [i32; 5],
}
impl SharedPrice {
    pub(super) fn for_options(base_power: f64, base: f64, width: usize, profiles: &[[f64; 5]],
                   powers: &[f64], options: &[Vec<i32>]) -> Vec<Self> {
        if options.iter().filter(|choices| choices.len() > 1).count() < 2 {
            return Vec::new();
        }
        if !base.is_finite() || base < 0.0
            || profiles.iter().any(|row| !row[0].is_finite() || row[0] < 0.0)
        {
            return Vec::new();
        }
        let physical = width - 1;
        let power = |slot: usize, snap: i32| powers[slot * width
            + if snap < 0 { physical } else { snap as usize }];
        let weight = |slot: usize, snap: i32| profiles[slot * width + (snap + 1) as usize][0];
        let (mut power_cap, mut weight_cap) = (base_power, base);
        for (slot, choices) in options.iter().enumerate() {
            power_cap += choices.iter().map(|&snap| power(slot, snap)).fold(0.0_f64, f64::max);
            weight_cap += choices.iter().map(|&snap| weight(slot, snap)).fold(0.0_f64, f64::max);
        }
        let ratio = (weight_cap / power_cap).sqrt();
        if !ratio.is_finite() || ratio <= 0.0 { return Vec::new(); }
        let columns = physical + 5;
        let mut matching = crate::assignment::Scratch::default();
        let mut matrix = vec![vec![-1e100; columns]; 5];
        [1.0, 0.95, 1.05].into_iter().filter_map(|scale| {
            let lambda = ratio * scale;
            let mut fixed = vec![[0.0; 5]; width];
            let column = |slot: usize, snap: i32| {
                if snap < 0 { physical + slot } else { snap as usize }
            };
            for (slot, (row, choices)) in matrix.iter_mut().zip(options).enumerate() {
                row.fill(-1e100);
                for &snap in choices {
                    let value = ((lambda * power(slot, snap)).next_up()
                        + (weight(slot, snap) / lambda).next_up()).next_up();
                    fixed[(snap + 1) as usize][slot] = value;
                    row[column(slot, snap)] = value;
                }
            }
            let mut selected = [0; 5];
            if matching.maximum(&matrix, columns, Some(&mut selected)) < -1e50 { return None; }
            let assignment = selected.map(|column| if column < physical { column as i32 } else { -1 });
            // A column price is a charge shared by all slots using that
            // physical snapshot. Any nonnegative prices give a valid ceiling.
            let charges = matching.column_prices(columns);
            let offers = std::array::from_fn(|slot| {
                let mut row: Vec<_> = options[slot].iter().map(|&snap| {
                    let charge = if snap < 0 { 0.0 } else { charges[snap as usize] };
                    (snap, (fixed[(snap + 1) as usize][slot] - charge).next_up())
                }).collect();
                row.sort_unstable_by(|a, b| b.1.total_cmp(&a.1));
                row.truncate(5);
                row
            });
            let mut charges = charges.into_iter().take(physical).enumerate()
                .filter(|&(_, value)| value > 0.0)
                .map(|(snap, value)| (snap as i32, value)).collect::<Vec<_>>();
            charges.sort_unstable_by(|a, b| b.1.total_cmp(&a.1));
            let base = ((lambda * base_power).next_up() + (base / lambda).next_up()).next_up();
            Some(Self { base, fixed, offers, charges, assignment })
        }).collect()
    }

    pub(super) fn upper(&self, assigned: &[i32], from: usize) -> f64 {
        let used = &assigned[..from];
        let mut sum = self.charges.iter()
            .filter(|&(snap, _)| !used.contains(snap))
            .take(5 - from).fold(self.base, |sum, &(_, value)| (sum + value).next_up());
        for (slot, row) in self.offers.iter().enumerate() {
            let value = if slot < from {
                self.fixed[(assigned[slot] + 1) as usize][slot]
            } else {
                let Some(&(_, value)) = row.iter().find(|(snap, _)| *snap < 0 || !used.contains(snap))
                    else { return f64::NEG_INFINITY; };
                value
            };
            sum = (sum + value).next_up();
        }
        if sum.is_nan() { return f64::INFINITY; }
        let sum = sum.max(0.0);
        ((sum * sum).next_up() / 4.0).next_up()
    }
}
enum ChartSettlement {
    Upper(f64),
    Exact(Exact),
}
struct FormationBounds {
    members: [usize; 5],
    snapshots: [i32; 5],
    scores: Vec<f64>,
    refined: bool,
}
impl Objective for PowerSeed {
    fn skill(&mut self, member: usize, snap: i32) -> (f64, f64) {
        (0.0, self.weight(member, snap))
    }
    fn member_bonus(&self, member: usize) -> f64 {
        self.bonuses[member] as f64
    }
    fn linear_assignment(&self) -> Option<(f64, f64, f64)> {
        Some((1.0, self.scale, 0.0))
    }
    fn bound(&mut self, t: Totals) -> f64 {
        if self.scale == 0.0 { t.power } else { (t.power + (self.scale * t.skill).next_up()).next_up() }
    }
    fn interval(&mut self, _team: &Team, t: Totals) -> (f64, f64) {
        (t.power, t.power)
    }
    fn exact(&mut self, team: &Team, power: f64, _slots: &[f64]) -> Result<Exact, String> {
        let skill = team.members.iter().zip(&team.snaps).map(|(&member, &snap)| self.weight(member, snap)).sum::<f64>();
        Ok(Exact {
            key: power + self.scale * skill,
            detail: Value::Null,
        })
    }
}

#[derive(Default)]
struct PrefixCeilings {
    snapshots: Option<[i32; 5]>,
    scores: Vec<f64>,
    mean: f64,
}

struct Combined<'k, 'a> {
    songs: Vec<SongObjective<'k, 'a>>,
    player: &'a Player,
    same_levels: bool,
    same_power: bool,
    min_combo: i32,
    roster: Vec<usize>,
    affine: Option<AffineMean>,
    power_groups: Vec<(usize, AffineMean)>,
    warm_seed: bool,
    joint_seed_pending: bool,
    cooperative_floor: bool,
    completed: Vec<Team>,
    floor: f64,
    k: usize,
    last_bounds: Vec<f64>,
    last_totals: Vec<Totals>,
    last_prefix: Option<(usize, [i32; 5])>,
    prefix_ceilings: [PrefixCeilings; 5],
    formation_bounds: Option<FormationBounds>,
    settlement_gaps: Vec<Option<f64>>,
    refinement_order: Vec<usize>,
    joint: Option<Vec<SharedPowerBound>>,
    joint_last_prefix: Option<(usize, [i32; 5], f64)>,
    joint_uncovered: Vec<usize>,
    joint_options: Vec<Vec<i32>>,
    joint_domain: Vec<[bool; 5]>,
    joint_fixed_luck: bool,
    window_rows: [Option<(f64, Vec<f64>)>; 6],
    window_bounds: [Option<crate::rewards::WindowScoreBound>; 6],
    window_attempted: [bool; 6],
    window_converters: Option<Vec<bool>>,
    conversion_mask: Option<Vec<bool>>,
    open_converters: [bool; 5],
    window_options: [bool; 5],
    window_budgets: Vec<WindowBudget>,
    dominance_profiles: Option<(Vec<u32>, HashMap<Vec<u32>, u32>)>,
    roster_programs: HashMap<(usize, i32), u32>,
    roster_program_ids: HashMap<Vec<u32>, u32>,
    dominance_power: Vec<usize>,
    shape_charts: Vec<usize>,
    ordered_snapshot_programs: bool,
}

struct AffineMean {
    base: f64,
    gap: f64,
    absolute: f64,
    weights: Vec<f64>,
}

impl AffineMean {
    fn song_terms(song: &SongObjective<'_, '_>) -> Option<(f64, f64, f64)> {
        let converted = song.adapter.kernel.chart.full.converted as f64;
        let (base, scale, residual) = if let Some(bound) = &song.adapter.deterministic {
            (bound.base, (1.0 / converted).next_up(), 0.0)
        } else if let Some(coefficient) = song.adapter.coarse_coefficient {
            (coefficient, 1.0, 0.0)
        } else {
            let data = song.adapter.data.as_ref()?;
            (data.weights.base, ((1.0 + RELATIVE) / converted).next_up(),
                (song.adapter.kernel.chart.notes.len() + 4) as f64)
        };
        (converted >= 1.0 && base.is_finite() && scale.is_finite())
            .then_some(((base.max(0.0) * scale).next_up(), scale, residual))
    }

    fn upper(&self, totals: Totals) -> f64 {
        let base = (totals.power * self.base).next_up();
        let variable = ((totals.power + self.gap).next_up() * totals.skill).next_up();
        let sum = ((base + variable).next_up() + self.absolute).next_up();
        (sum * (1.0 + 3e-12)).next_up()
    }
}

impl<'k, 'a> Combined<'k, 'a> {
    fn new(
        adapters: Vec<Adapter<'k, 'a>>,
        player: &'a Player,
        music: &'a [Music],
        same_levels: bool,
        min_combo: i32,
        warm_seed: bool,
        prefer_joint_seed: bool,
        cooperative_floor: bool,
    ) -> Self {
        let master = adapters[0].kernel.master;
        let reference_type = music[0].music_type;
        let music_dependent_leader = master.leader_skills.values().flatten().any(|row| {
            master.condition_sets.get(&row.condition_group).into_iter().flatten().flatten()
                .any(|id| master.conditions.get(id).is_some_and(|condition| condition.kind == 4012))
        });
        let mut reference = None;
        let mut bases_by_music = HashMap::default();
        let profiles: Vec<_> = adapters[0].kernel.members.iter().map(|member| power::leader_profile(master, member)).collect();
        let mut power_bases = Vec::<PowerBasis>::new();
        let mut power_domains: HashMap<(usize, Vec<i64>), (usize, f64)> = HashMap::default();
        let defer_assignment_prices = adapters.len() >= 16;
        let mut songs: Vec<_> = adapters
            .into_iter()
            .zip(music)
            .map(|(mut adapter, music)| {
                let master = adapter.kernel.master;
                let members = adapter.kernel.members;
                let snaps = adapter.kernel.snaps;
                let width = snaps.len() + 1;
                let bases = Arc::clone(bases_by_music.entry(music.slot_base_key(player)).or_insert_with(|| {
                    Arc::new(members.iter().map(|member| power::slot_base(master, player, member, Some(music), 0)).collect::<Vec<_>>())
                }));
                let first = reference.get_or_insert_with(|| Arc::clone(&bases));
                let power_gap = if (!music_dependent_leader || music.music_type == reference_type)
                    && bases.iter().zip(first.iter()).all(|(a, b)| a.common == b.common)
                {
                    let mut characters = HashMap::default();
                    for ((a, b), member) in bases.iter().zip(first.iter()).zip(members) {
                        let gap = a.fixed_points.saturating_sub(b.fixed_points).max(0);
                        characters.entry(member.card.character_id).and_modify(|value: &mut i64| {
                            *value = (*value).max(gap);
                        }).or_insert(gap);
                    }
                    let mut gaps: Vec<_> = characters.into_values().collect();
                    gaps.sort_unstable_by(|a, b| b.cmp(a));
                    Some(gaps.into_iter().take(5).fold(0.0_f64, |sum, gap| (sum + gap as f64).next_up()))
                } else { None };
                let basis = power_bases.iter().position(|table| {
                    table.bases.iter().zip(bases.iter()).all(|(a, b)| a.common == b.common)
                }).unwrap_or_else(|| {
                    let points: Vec<_> = members.iter().zip(bases.iter()).flat_map(|(member, base)| {
                        snaps.iter().map(Some).chain(std::iter::once(None))
                            .map(move |snap| power::snap_points(master, base, member, snap, 0) as f64)
                    }).collect();
                    let variable_caps = members.iter().zip(bases.iter()).zip(points.chunks_exact(width))
                        .map(|((member, base), points)| {
                            let leader = profiles.iter().map(|profile| {
                                power::leader_points(base, &power::leader_percent_bound(master, profile, member))
                            }).max().unwrap_or(0);
                            leader as f64 + points.iter().copied().fold(0.0, f64::max)
                        }).collect();
                    let index = power_bases.len();
                    let exact = points.iter().all(|value| value.abs() <= EXACT_POWER_COMPONENT);
                    power_bases.push(PowerBasis {
                        bases: Arc::clone(&bases), snaps: Arc::new(points), exact, variable_caps,
                    });
                    index
                });
                let common = &power_bases[basis];
                let snap_power = Arc::clone(&common.snaps);
                let fixed: Vec<_> = bases.iter().map(|base| base.fixed_points).collect();
                let next = power_domains.len();
                let (power_domain, power_cap) = *power_domains.entry((basis, fixed)).or_insert_with_key(|(_, fixed)| {
                    let mut caps: Vec<_> = fixed.iter().zip(&common.variable_caps)
                        .map(|(&base, variable)| (base as f64 + variable).next_up()).collect();
                    caps.sort_unstable_by(|a, b| b.total_cmp(a));
                    let cap = caps.into_iter().take(5).fold(0.0_f64, |sum, value| (sum + value).next_up());
                    (next, cap)
                });
                let weights = if let Some(bound) = &adapter.deterministic {
                    bound.mean_weights()
                } else {
                    (0..members.len()).flat_map(|m| (0..width).map(move |j| (m, j))).map(|(m, j)| {
                        adapter.upper_skill(m, if j == width - 1 { -1 } else { j as i32 })
                    }).collect()
                };
                // The shared ceiling may rule out an entire domain without
                // needing each chart's separate snapshot frontiers.
                adapter.defer_assignment_prices = defer_assignment_prices && adapter.kernel.just_window.is_none();
                adapter.prepare_power(Arc::clone(&snap_power), width);
                adapter.prepare_dominance(power_cap);
                SongObjective {
                    adapter,
                    music,
                    bases,
                    snap_power,
                    exact_snapshot_power: common.exact,
                    weights,
                    power_cap,
                    power_domain,
                    power_gap,
                    base: 0.0,
                    roster_bonus: 0.0,
                    slots: Arc::new(Vec::new()),
                    exact_prepared_power: false,
                    power_source: 0,
                    member_power: Arc::new(Vec::new()),
                    member_skill: Vec::new(),
                    free_power: Arc::new(Vec::new()),
                    free_skill: Vec::new(),
                    power_choices: Arc::new(Vec::new()),
                    skill_choices: Vec::new(),
                }
            })
            .collect();
        let equal_power = songs.iter().all(|song| {
            song.music.music_type == songs[0].music.music_type
                && song
                    .bases
                    .iter()
                    .zip(songs[0].bases.iter())
                    .all(|(a, b)| a.common == b.common && a.fixed_points == b.fixed_points)
        });
        let affine = (|| {
            let mut base = 0.0_f64;
            let mut absolute = 0.0_f64;
            let mut gap = 0.0_f64;
            let mut weights = vec![0.0_f64; songs[0].weights.len()];
            for song in &songs {
                let song_gap = if equal_power { 0.0 } else { song.power_gap? };
                gap = gap.max(song_gap);
                let (fixed, scale, residual) = AffineMean::song_terms(song)?;
                base = (base + fixed).next_up();
                absolute = ((absolute + residual).next_up() + (fixed * song_gap).next_up()).next_up();
                for (weight, &row) in weights.iter_mut().zip(&song.weights) {
                    if !row.is_finite() {
                        return None;
                    }
                    *weight = (*weight + (row.max(0.0) * scale).next_up()).next_up();
                }
            }
            let count = songs.len() as f64;
            for weight in &mut weights { *weight = (*weight / count).next_up(); }
            Some(AffineMean {
                base: (base / count).next_up(), gap, absolute: (absolute / count).next_up(), weights,
            })
        })();
        let power_groups = if !equal_power && songs.len() >= 8 {
            let mut indices = HashMap::default();
            let mut groups = Vec::<(usize, Vec<usize>)>::new();
            for (index, song) in songs.iter().enumerate() {
                let next = groups.len();
                let group = *indices.entry((song.power_domain, song.music.music_type)).or_insert(next);
                if group == next { groups.push((index, Vec::new())); }
                groups[group].1.push(index);
            }
            if groups.len() * 3 <= songs.len() {
                groups.into_iter().map(|(first, indices)| {
                    let mut profile = AffineMean { base: 0.0, gap: 0.0, absolute: 0.0, weights: vec![0.0; songs[first].weights.len()] };
                    for index in indices {
                        let song = &songs[index];
                        let (base, scale, residual) = AffineMean::song_terms(song)?;
                        profile.base = (profile.base + base).next_up();
                        profile.absolute = (profile.absolute + residual).next_up();
                        for (value, &row) in profile.weights.iter_mut().zip(&song.weights) {
                            if !row.is_finite() { return None; }
                            *value = (*value + (row.max(0.0) * scale).next_up()).next_up();
                        }
                    }
                    Some((first, profile))
                }).collect::<Option<Vec<_>>>().unwrap_or_default()
            } else { Vec::new() }
        } else { Vec::new() };
        let window_converters = if equal_power && songs.iter().any(|song| song.adapter.kernel.has_team_filter()) {
            (|| {
                let mut mask = vec![false; songs[0].adapter.kernel.members.len() * (songs[0].adapter.kernel.snaps.len() + 1)];
                for song in &songs {
                    for (target, &value) in mask.iter_mut().zip(song.adapter.deterministic.as_ref()?.conversion_mask()) { *target |= value; }
                }
                Some(mask)
            })()
        } else { None };
        let entries = songs[0].snap_power.len();
        let dominance_profiles = (songs.len() > 1
            && entries.saturating_mul(songs.len()) <= 1_048_576)
            .then(|| (vec![u32::MAX; entries], HashMap::default()));
        let mut powers = HashSet::default();
        let dominance_power: Vec<_> = songs.iter().enumerate()
            .filter_map(|(index, song)| powers.insert(Arc::as_ptr(&song.snap_power)).then_some(index))
            .collect();
        let ordered_snapshot_programs = dominance_power.len() == 1 && songs.iter().all(|song| {
            let skill = song.weights.iter().copied().fold(0.0, f64::max) * 5.0;
            song.adapter.deterministic.as_ref().is_some_and(|bound| {
                song.adapter.kernel.healthy && bound.score(Totals {
                    power: song.power_cap, skill, ..Totals::default()
                }) < i32::MAX as f64
            })
        });
        let mut shape_scopes = HashMap::default();
        let share_actors = songs.len() >= 16;
        let shape_charts = songs.iter_mut().enumerate().filter_map(|(index, song)| {
            let kernel = &mut song.adapter.kernel;
            let mut missions = kernel.chart.setup.missions.clone();
            missions.sort_unstable();
            missions.dedup();
            // Shape IDs encode skill programs, not note weights. Charts sharing
            // these inputs have the same partition of member/snapshot pairs.
            let scope = (
                Arc::as_ptr(&kernel.slots),
                Arc::as_ptr(&kernel.support),
                missions,
                kernel.owner_local,
                kernel.chart.luck,
                kernel.healthy,
                kernel.seeds.is_empty(),
                kernel.just_grades_fixed,
                kernel.just_context.is_some(),
                kernel.just_window.is_some(),
                song.adapter.deterministic.as_ref()
                    .filter(|_| kernel.healthy && kernel.owner_local && kernel.just_context.is_some())
                    .map(|bound| bound.just_ceiling()),
            );
            let (source, actors) = shape_scopes.entry(scope).or_insert_with(|| {
                let actors = (share_actors && kernel.owner_local).then(|| {
                    Arc::new(std::sync::Mutex::new(HashMap::default()))
                });
                (index, actors)
            });
            kernel.shared_bound_actors = actors.clone();
            (*source == index).then_some(index)
        }).collect();
        let conversion_mask = if ordered_snapshot_programs && songs.len() >= 16 {
            let mut mask = vec![false; entries];
            for song in &songs {
                for (any, &converts) in mask.iter_mut().zip(song.adapter.deterministic.as_ref().unwrap().conversion_mask()) {
                    *any |= converts;
                }
            }
            mask.iter().any(|&converts| converts).then_some(mask)
        } else {
            None
        };
        let settlement_gaps = vec![None; songs.len()];
        let mut refinement_order: Vec<_> = (0..songs.len()).collect();
        // LUCK regions have the largest difference between their broad
        // ceilings and their resolved replay, so try those refinements first.
        refinement_order.sort_by_key(|&index| !songs[index].adapter.kernel.chart.luck);
        let joint_seed_pending = warm_seed && prefer_joint_seed && ordered_snapshot_programs;
        Self {
            songs,
            player,
            same_levels,
            same_power: equal_power,
            min_combo,
            roster: Vec::new(),
            affine,
            power_groups,
            warm_seed,
            joint_seed_pending,
            cooperative_floor,
            completed: Vec::new(),
            floor: f64::NEG_INFINITY,
            k: 1,
            last_bounds: Vec::new(),
            last_totals: Vec::new(),
            last_prefix: None,
            prefix_ceilings: std::array::from_fn(|_| PrefixCeilings::default()),
            formation_bounds: None,
            settlement_gaps,
            refinement_order,
            joint: None,
            joint_last_prefix: None,
            joint_uncovered: Vec::new(),
            joint_options: Vec::new(),
            joint_domain: Vec::new(),
            joint_fixed_luck: false,
            window_rows: std::array::from_fn(|_| None),
            window_bounds: std::array::from_fn(|_| None),
            window_attempted: [false; 6],
            window_converters,
            conversion_mask,
            open_converters: [true; 5],
            window_options: [false; 5],
            window_budgets: Vec::new(),
            dominance_profiles,
            roster_programs: HashMap::default(),
            roster_program_ids: HashMap::default(),
            dominance_power,
            shape_charts,
            ordered_snapshot_programs,
        }
    }

    fn snapshot_program(&mut self, member: usize, snap: i32) -> Option<(u32, usize)> {
        let (ids, profiles) = self.dominance_profiles.as_mut()?;
        let width = self.songs[0].adapter.kernel.snaps.len() + 1;
        let column = if snap < 0 { width - 1 } else { snap as usize };
        let slot = member * width + column;
        if ids[slot] == u32::MAX {
            let key = self.shape_charts.iter().map(|&index| {
                self.songs[index].adapter.shape_id(member, snap)
            }).collect();
            let id = profiles.len() as u32;
            ids[slot] = *profiles.entry(key).or_insert(id);
        }
        Some((ids[slot], slot))
    }

    fn settle(
        &mut self,
        team: &Team,
        cutoff: Option<search::Cutoff>,
        interruptible: bool,
    ) -> Result<search::Evaluation, String> {
        if !self.eligible(&team.members) {
            return Ok(search::Evaluation::Exact(Exact {
                key: f64::NEG_INFINITY,
                detail: Value::Null,
            }));
        }
        let kernel = &self.songs[0].adapter.kernel;
        let members: Vec<_> = team
            .members
            .iter()
            .map(|&m| kernel.members[m].clone())
            .collect();
        let snaps: Vec<_> = team
            .snaps
            .iter()
            .map(|&s| (s >= 0).then(|| kernel.snaps[s as usize].clone()))
            .collect();
        #[derive(Clone, Copy)]
        struct SettledPower {
            total: i64,
            slots: [i64; 5],
        }

        let prepared_roster = self.roster == team.members;
        let width = kernel.snaps.len() + 1;
        let mut power_domains = HashMap::default();
        let song_count = if self.same_power { 1 } else { self.songs.len() };
        let mut powers: Vec<SettledPower> = Vec::with_capacity(song_count);
        for (index, song) in self.songs.iter().take(song_count).enumerate() {
            let power = if prepared_roster && song.exact_prepared_power {
                if song.power_source < index {
                    powers[song.power_source]
                } else {
                    let slots = std::array::from_fn(|slot| {
                        let snap = team.snaps[slot];
                        song.slots[slot][if snap < 0 { width - 1 } else { snap as usize }] as i64
                    });
                    let total = slots.iter().sum();
                    #[cfg(debug_assertions)]
                    {
                        let original = power::team_power(
                            song.adapter.kernel.master, self.player, &members, &snaps, 0, Some(song.music), &[],
                        )?;
                        debug_assert_eq!(slots.as_slice(), original.slots.as_slice(), "prepared slot power");
                        debug_assert_eq!(total, original.total, "prepared total power");
                    }
                    SettledPower { total, slots }
                }
            } else {
                let key = (song.power_domain, song.music.music_type);
                if let Some(&power) = power_domains.get(&key) {
                    power
                } else {
                    let original = power::team_power(
                        song.adapter.kernel.master, self.player, &members, &snaps, 0, Some(song.music), &[],
                    )?;
                    let power = SettledPower {
                        total: original.total,
                        slots: std::array::from_fn(|slot| original.slots[slot]),
                    };
                    power_domains.insert(key, power);
                    power
                }
            };
            powers.push(power);
        }
        let cutoff = cutoff.filter(|c| c.key.is_finite());
        let cached = self.formation_bounds.take().filter(|known| {
            cutoff.is_some() && team.members == known.members && team.snaps == known.snapshots
        });
        let reused = cached.is_some();
        let refined = cached.as_ref().is_some_and(|known| known.refined);
        let mut bounds = cached.map_or_else(|| vec![f64::INFINITY; self.songs.len()], |known| known.scores);
        let counters = (cutoff.is_some() && !refined && self.songs.iter().any(|song| !song.adapter.kernel.chart.luck))
            .then(|| crate::support_aliases::JustCounters::for_formation(
                kernel.master,
                &members,
                &snaps.iter().flatten().cloned().collect::<Vec<_>>(),
            ));
        if cutoff.is_some() && !reused {
            for (index, song) in self.songs.iter_mut().enumerate() {
                let power = powers[if self.same_power { 0 } else { index }].total as f64;
                let width = song.adapter.kernel.snaps.len() + 1;
                let skill = team.members.iter().zip(&team.snaps).fold(0.0_f64, |sum, (&member, &snap)| {
                    let column = if snap < 0 { width - 1 } else { snap as usize };
                    (sum + song.weights[member * width + column]).next_up()
                });
                let bonus = team.members.iter().map(|&member| song.adapter.kernel.bonuses[member] as f64).sum();
                bounds[index] = song.adapter.bound(Totals { power, skill, bonus, ..Totals::default() });
            }
        }
        let mut order: Vec<_> = (0..self.songs.len()).collect();
        let expected_reduction = |index: usize| {
            bounds[index] * self.settlement_gaps[index].unwrap_or(0.25)
        };
        order.sort_by(|&a, &b| expected_reduction(b).total_cmp(&expected_reduction(a)));
        let count = bounds.len();
        let leaves = count.next_power_of_two();
        let mut remainder = cutoff.filter(|_| bounds.iter().all(|&value| value >= 0.0)).map(|_| {
            let mut sums = vec![0.0; leaves * 2];
            sums[leaves..leaves + count].copy_from_slice(&bounds);
            for node in (1..leaves).rev() {
                sums[node] = (sums[node * 2] + sums[node * 2 + 1]).next_up();
            }
            sums
        });
        let mut details = vec![Value::Null; self.songs.len()];
        let mut scores = vec![0.0; self.songs.len()];
        let mut pending = Vec::new();
        let exact_song = |song: &mut SongObjective<'_, '_>, power: i64| -> Result<Exact, String> {
            let mut score = song.adapter.kernel.exact(team, power as i32, true)?;
            song.adapter.kernel.decorate_window(team, power as i32, &mut score.detail)?;
            Ok(score)
        };
        let score_song = |song: &mut SongObjective<'_, '_>, power: i64, ceiling: Option<f64>| -> Result<ChartSettlement, String> {
            if interruptible && crate::clock::preparation_expired() {
                return Err(SETTLEMENT_DEADLINE.into());
            }
            if ceiling.is_none() && cutoff.is_some() {
                if let Some(upper) = counters.as_ref().and_then(|counters|
                    song.adapter.kernel.shared_mean_upper(team, power as f64, counters))
                {
                    return Ok(ChartSettlement::Upper(upper));
                }
                if let Some(upper) = song.adapter.kernel.nominal_mean_upper(team, power as i32)? {
                    return Ok(ChartSettlement::Upper(upper));
                }
            }
            let exact = exact_song(song, power)?;
            debug_assert!(ceiling.is_none_or(|upper| exact.key <= upper), "nominal average exceeds its unrounded upper bound");
            Ok(ChartSettlement::Exact(exact))
        };
        let same_power = self.same_power;
        let gaps = &mut self.settlement_gaps;
        let mut accept = |index: usize, result: ChartSettlement, pending: &mut Vec<(usize, f64)>| {
            let value = match result {
                ChartSettlement::Upper(upper) => {
                    pending.push((index, upper));
                    bounds[index].min(upper)
                }
                ChartSettlement::Exact(score) => {
                    debug_assert!(score.key <= bounds[index], "shared song {} score {} exceeds {}", index, score.key, bounds[index]);
                    scores[index] = score.key;
                    details[index] = score.detail;
                    score.key
                }
            };
            let bound = bounds[index];
            if bound.is_finite() && bound > 0.0 && value >= 0.0 {
                let gap = (1.0 - value / bound).clamp(0.0, 1.0);
                gaps[index] = Some(gaps[index].map_or(gap, |old| 0.75 * old + 0.25 * gap));
            }
            if value < 0.0 { remainder = None; }
            if let Some((cutoff, sums)) = cutoff.zip(remainder.as_mut()) {
                let mut node = leaves + index;
                sums[node] = value;
                while node > 1 {
                    node /= 2;
                    sums[node] = (sums[node * 2] + sums[node * 2 + 1]).next_up();
                }
                // Cover the rounding of the final sum in original song order.
                let sum = (sums[1] * (1.0 + count as f64 * f64::EPSILON)).next_up();
                let upper = (sum / count as f64).next_up();
                if upper < cutoff.key || cutoff.prune_equal && upper == cutoff.key {
                    return Some(upper);
                }
            }
            None
        };
        let mut visit = |songs: &mut [SongObjective<'_, '_>], order: &[(usize, Option<f64>)],
                         pending: &mut Vec<(usize, f64)>| -> Result<Option<f64>, String> {
            let power = |index| powers[if same_power { 0 } else { index }].total;
            #[cfg(not(target_arch = "wasm32"))]
            let workers = crate::parallel::workers(order.len());
            #[cfg(not(target_arch = "wasm32"))]
            if order.len() >= 8 && workers > 1 {
                let mut songs: Vec<_> = songs.iter_mut().map(Some).collect();
                let tasks: Vec<_> = order.iter().map(|&(index, ceiling)| {
                    (index, ceiling, std::sync::Mutex::new(songs[index].take().unwrap()))
                }).collect();
                let mut stopped = None;
                crate::parallel::visit_in_order(&tasks, workers, |&(index, ceiling, ref song)| {
                    let score = crate::parallel::serial(|| score_song(&mut **song.lock().unwrap(), power(index), ceiling));
                    (index, score)
                }, |(index, score)| {
                    stopped = accept(index, score?, pending);
                    Ok::<_, String>(stopped.is_none())
                })?;
                return Ok(stopped);
            }
            for &(index, ceiling) in order {
                let score = score_song(&mut songs[index], power(index), ceiling)?;
                if let Some(upper) = accept(index, score, pending) {
                    return Ok(Some(upper));
                }
            }
            Ok(None)
        };
        let order: Vec<_> = order.into_iter().map(|index| (index, None)).collect();
        if let Some(upper) = visit(&mut self.songs, &order, &mut pending)? {
            return Ok(search::Evaluation::Pruned(upper));
        }
        if !pending.is_empty() {
            let order: Vec<_> = std::mem::take(&mut pending).into_iter()
                .map(|(index, upper)| (index, Some(upper))).collect();
            if let Some(upper) = visit(&mut self.songs, &order, &mut pending)? {
                return Ok(search::Evaluation::Pruned(upper));
            }
        }
        let window = crate::just_window::aggregate(details.iter().map(|detail| &detail["justWindow"]));
        let songs: Vec<_> = details.into_iter().enumerate().map(|(index, detail)| {
            let power = &powers[if same_power { 0 } else { index }];
            json!({"detail":detail,"power":power.total,"slotPowers":power.slots})
        }).collect();
        let mut detail = json!({"songs":songs});
        if let Some(window) = window { detail["justWindow"] = window; }
        Ok(search::Evaluation::Exact(Exact {
            key: scores.iter().sum::<f64>() / scores.len() as f64,
            detail,
        }))
    }

    fn eligible(&self, members: &[usize]) -> bool {
        self.min_combo <= 1 || self.songs
            .iter()
            .all(|song| song.adapter.kernel.meets_combo(members, self.min_combo))
    }

    fn grouped_leaf_upper(&self, team: &Team) -> Option<f64> {
        if self.power_groups.is_empty() { return None; }
        let score = self.power_groups.iter().fold(0.0_f64, |sum, (first, profile)| {
            let song = &self.songs[*first];
            let width = song.adapter.kernel.snaps.len() + 1;
            let mut totals = Totals::default();
            for (slot, (&member, &snap)) in team.members.iter().zip(&team.snaps).enumerate() {
                let column = if snap < 0 { width - 1 } else { snap as usize };
                totals.power = (totals.power + song.slots[slot][column]).next_up();
                totals.skill = (totals.skill + profile.weights[member * width + column]).next_up();
            }
            (sum + profile.upper(totals)).next_up()
        });
        Some((score / self.songs.len() as f64).next_up())
    }

    fn joint_prefix_upper(&mut self, members: &[usize], assigned: &[i32], from: usize, reference_power: f64) -> Option<f64> {
        let groups = self.joint.as_ref()?;
        if self.joint_uncovered.len() * 4 > self.songs.len() { return None; }
        if let Some((len, previous, upper)) = self.joint_last_prefix {
            if len == from && previous[..from] == assigned[..from] { return Some(upper); }
        }
        let first = &self.songs[0];
        let power_cap = (reference_power - first.base).next_up();
        let width = first.adapter.kernel.snaps.len() + 1;
        let plain = self.conversion_mask.as_ref().is_some_and(|mask| {
            members.iter().enumerate().all(|(slot, &member)| {
                if slot < from { !mask[member * width + (assigned[slot] + 1) as usize] }
                else { !self.open_converters[slot] }
            })
        });
        let mut sum = 0.0_f64;
        for group in groups {
            let song = &self.songs[group.songs[0]];
            let actual = Self::assignment_power(song, assigned, from);
            let snapshot_power = (actual - song.base).next_up();
            let snapshot_power = if Arc::ptr_eq(&song.snap_power, &first.snap_power) {
                snapshot_power.min(power_cap)
            } else {
                snapshot_power
            };
            let power = (snapshot_power + group.base_power).next_up();
            let offset = group.offsets.iter().enumerate().fold(group.offset, |sum, (slot, row)| {
                let value = if slot < from { row[(assigned[slot] + 1) as usize] } else { Self::first_available(&group.offset_choices[slot], &assigned[..from]).max(0.0) };
                (sum + value).next_up()
            });
            let coefficient = (0..5).fold(group.coefficient, |sum, slot| {
                let value = if slot < from { group.profiles[(assigned[slot] + 1) as usize][slot] }
                    else { Self::first_available(&group.profile_choices[slot], &assigned[..from]).max(0.0) };
                (sum + value).next_up()
            });
            let mut upper = ((power * coefficient).next_up() + offset).next_up();
            if plain {
                if let Some(bound) = &group.plain_score {
                    upper = upper.min(bound.upper(power, assigned, from));
                }
            }
            if from < 5 {
                let priced_offset = if group.prices_offsets { group.offset } else { offset };
                for price in &group.prices {
                    upper = upper.min((price.upper(assigned, from) + priced_offset).next_up());
                }
            }
            sum = (sum + (upper * group.songs.len() as f64).next_up()).next_up();
        }
        for &index in &self.joint_uncovered {
            let song = &mut self.songs[index];
            let totals = Self::totals(song, members, assigned, from);
            sum = (sum + song.adapter.bound(totals)).next_up();
        }
        let count = self.songs.len() as f64;
        let upper = ((sum * (1.0 + count * f64::EPSILON)).next_up() / count).next_up();
        let mut prefix = [-1; 5];
        prefix[..from].copy_from_slice(&assigned[..from]);
        self.joint_last_prefix = Some((from, prefix, upper));
        Some(upper)
    }

    fn window_upper(&mut self, assigned: &[i32], from: usize) -> Option<f64> {
        let mask = self.window_converters.as_ref()?;
        let width = self.songs[0].adapter.kernel.snaps.len() + 1;
        let mut count = (0..5).filter(|&slot| if slot < from {
            mask[self.roster[slot] * width + (assigned[slot] + 1) as usize]
        } else { self.window_options[slot] }).count();
        let fixed = (0..from).filter(|&slot| mask[self.roster[slot] * width + (assigned[slot] + 1) as usize]).count();
        for budget in &self.window_budgets {
            let mut possible = [f64::NEG_INFINITY; 6];
            possible[fixed] = (0..from).fold(0.0_f64, |sum, slot|
                (sum + budget.values[slot][(assigned[slot] + 1) as usize]).next_up());
            for choices in budget.choices.iter().skip(from) {
                let best: [f64; 2] = std::array::from_fn(|kind| choices[kind].iter()
                    .find(|(snap, _)| *snap < 0 || !assigned[..from].contains(snap))
                    .map_or(f64::NEG_INFINITY, |(_, value)| *value));
                possible = std::array::from_fn(|number| {
                    let same = (possible[number] + best[0]).next_up();
                    if number == 0 { same } else { same.max((possible[number - 1] + best[1]).next_up()) }
                });
            }
            let Some(maximum) = (fixed..=count).rev().find(|&number| possible[number] >= budget.required) else {
                return Some(f64::NEG_INFINITY);
            };
            count = maximum;
        }
        if !self.window_attempted[count] {
            self.window_attempted[count] = true;
            if self.window_rows[count].is_none() {
                let songs = self.songs.len() as f64;
                let mut base = 0.0_f64;
                let mut rows = vec![0.0_f64; mask.len()];
                for song in &self.songs {
                    let (constant, coefficients) = song.adapter.deterministic.as_ref()?.normalized_window_rows(count)?;
                    base = (base + (constant / songs).next_up()).next_up();
                    for (row, coefficient) in rows.iter_mut().zip(coefficients) { *row = (*row + (coefficient / songs).next_up()).next_up(); }
                }
                self.window_rows[count] = Some((base, rows));
            }
            let (base, coefficients) = self.window_rows[count].as_ref().unwrap();
            let song = self.songs.iter().find(|song| song.adapter.kernel.has_team_filter())?;
            self.window_bounds[count] = crate::rewards::WindowScoreBound::prepare(
                song.adapter.kernel.just_window.as_ref().unwrap(), &self.roster, &self.joint_options,
                self.songs[0].base, crate::rewards::WindowScoreInput { base: *base, converted: 1.0, width,
                    snapshot_power: &self.songs[0].snap_power, all_coefficients: coefficients });
        }
        self.window_bounds[count].as_ref().map(|bound| bound.upper(assigned, from))
    }

    fn assignment_power(song: &SongObjective<'_, '_>, assigned: &[i32], from: usize) -> f64 {
        let width = song.adapter.kernel.snaps.len() + 1;
        (0..5).fold(0.0_f64, |sum, slot| {
            let value = if slot < from {
                let snap = assigned[slot];
                song.slots[slot][if snap < 0 { width - 1 } else { snap as usize }]
            } else {
                Self::first_available(&song.power_choices[slot], &assigned[..from])
            };
            (sum + value).next_up()
        })
    }

    fn best_offers(options: &[i32], value: impl Fn(i32) -> f64) -> Vec<(i32, f64)> {
        let mut offers: Vec<_> = options.iter().map(|&snap| (snap, value(snap))).collect();
        if offers.len() > 5 {
            offers.select_nth_unstable_by(4, |a, b| b.1.total_cmp(&a.1));
            offers.truncate(5);
        }
        // A prefix uses at most four snapshots while any slot is still open.
        offers.sort_unstable_by(|a, b| b.1.total_cmp(&a.1));
        offers
    }

    fn first_available(choices: &[(i32, f64)], used: &[i32]) -> f64 {
        choices.iter().find(|(snap, _)| *snap < 0 || !used.contains(snap))
            .map_or(f64::NEG_INFINITY, |(_, value)| *value)
    }

    fn totals_with_power(song: &SongObjective<'_, '_>, members: &[usize], assigned: &[i32],
                         from: usize, power: f64) -> Totals {
        let width = song.adapter.kernel.snaps.len() + 1;
        let skill = members.iter().enumerate().fold(0.0_f64, |sum, (slot, &member)| {
            let value = if slot < from {
                let snap = assigned[slot];
                song.weights[member * width + if snap < 0 { width - 1 } else { snap as usize }]
            } else {
                Self::first_available(&song.skill_choices[slot], &assigned[..from])
            };
            (sum + value).next_up()
        });
        Totals { power, skill, bonus: song.roster_bonus, ..Totals::default() }
    }

    fn totals(song: &SongObjective<'_, '_>, members: &[usize], assigned: &[i32], from: usize) -> Totals {
        Self::totals_with_power(song, members, assigned, from, Self::assignment_power(song, assigned, from))
    }

    fn fixed_luck_assignments(&self, members: &[usize], options: &[Vec<i32>]) -> bool {
        let Some(song) = self.songs.iter().find(|song| song.adapter.kernel.fixed_luck_chart()) else { return false; };
        let kernel = &song.adapter.kernel;
        members.iter().zip(options).all(|(&member, choices)| kernel.luck_reference(member, choices).is_some())
    }

    fn prepare_joint(&mut self, build_combo: bool) {
        if self.joint.is_some() { return; }
        if crate::clock::preparation_expired() {
            self.joint_uncovered = (0..self.songs.len()).collect();
            self.joint = Some(Vec::new());
            return;
        }
        let width = self.songs[0].adapter.kernel.snaps.len() + 1;
        let mut by_power = HashMap::default();
        let mut domains: Vec<Vec<usize>> = Vec::new();
        for (index, song) in self.songs.iter().enumerate() {
            if song.adapter.combo_bound.is_none() && song.adapter.deterministic.is_none() {
                continue;
            }
            let next = domains.len();
            let group = *by_power.entry(Arc::as_ptr(&song.snap_power)).or_insert(next);
            if group == next { domains.push(Vec::new()); }
            domains[group].push(index);
        }
        let mut groups = Vec::new();
        for domain in domains.into_iter().filter(|domain| domain.len() > 1) {
            let rows = domain.into_iter().filter_map(|index| {
                let song = &mut self.songs[index];
                let precise = !crate::clock::deadline_expired(crate::clock::preparation_deadline() - 60.0);
                if precise {
                    if let Some((constant, profiles, allowance)) = song.adapter.kernel
                        .just_assignment_rows(&self.roster, &self.joint_options, &song.snap_power)
                    {
                        return Some((index, constant, profiles, allowance, None));
                    }
                    if let Some((constant, profiles)) = song.adapter.kernel
                        .luck_assignment_rows(&self.roster, &self.joint_options, song.power_cap)
                    {
                        return Some((index, constant, profiles, 0.0, None));
                    }
                    if build_combo && !song.adapter.combo_attempted
                        && (song.adapter.kernel.mixed_combo_safe
                            || song.adapter.kernel.fixed_luck_chart()
                            || song.adapter.kernel.chart.setup.missions.iter().all(|&mission| mission == 1))
                    {
                        // Lock the known roster's COMBO trajectory before
                        // merging the per-chart ceilings across the whole pool.
                        let _ = song.adapter.scripted_combo_bound(&self.roster, &[-1; 5], 0,
                            Totals { power: song.power_cap, ..Totals::default() }, f64::INFINITY);
                    }
                }
                let assemble = |bound: &crate::rewards::DeterministicBound| {
                    let (constant, profiles) = bound.shared_assignment_mean_rows(&self.roster)?;
                    let plain = self.conversion_mask.as_ref()
                        .filter(|_| bound.conversion_mask().iter().any(|&converts| converts))
                        .and_then(|_| bound.shared_mean_rows_for(&self.roster, 0));
                    Some((index, constant, profiles, 0.0, plain))
                };
                song.adapter.combo_bound.as_ref().and_then(|shared| {
                    assemble(&shared.lock().unwrap_or_else(|error| error.into_inner()))
                }).or_else(|| assemble(song.adapter.deterministic.as_ref()?))
            }).collect::<Vec<_>>();
            if rows.len() < 2 { continue; }
            let count = rows.len() as f64;
            let base_power = rows.iter().map(|(index, ..)| self.songs[*index].base)
                .fold(f64::INFINITY, f64::min);
            if !base_power.is_finite() || base_power < 0.0 { continue; }
            let mut indices = Vec::with_capacity(rows.len());
            let mut base = 0.0_f64;
            let mut combined = vec![[0.0_f64; 5]; width * 5];
            let mut offset = 0.0_f64;
            let mut offsets: [Vec<f64>; 5] = std::array::from_fn(|_| vec![0.0; width]);
            let mut plain_score = rows.iter().any(|(_, _, _, _, plain)| plain.is_some())
                .then(|| PlainScoreBound::new(width));
            for (index, constant, profiles, allowance, without_converters) in rows {
                indices.push(index);
                base = (base + (constant / count).next_up()).next_up();
                // Only the member base varies with the song. Its
                // excess over the shared base prices a separate linear term.
                let difference = ((self.songs[index].base - base_power).next_up() / count).next_up();
                if let Some(plain) = &mut plain_score {
                    let (constant, terms) = without_converters.as_ref()
                        .map_or((constant, profiles.as_slice()), |(base, rows)| (*base, rows.as_slice()));
                    plain.add_chart(constant, terms, difference, allowance, count);
                }
                offset = (offset + (difference * constant).next_up()).next_up();
                offset = (offset + (allowance / count).next_up()).next_up();
                for (slot, (row, profile)) in combined.iter_mut().zip(profiles).enumerate() {
                    let value = (row[0] + (profile / count).next_up()).next_up();
                    row.fill(value);
                    let entry = &mut offsets[slot / width][slot % width];
                    *entry = (*entry + (difference * profile).next_up()).next_up();
                }
            }
            if let Some(plain) = &mut plain_score { plain.prepare(&self.joint_options); }
            let profiles: Vec<[f64; 5]> = (0..width).map(|column|
                std::array::from_fn(|slot| combined[slot * width + column][0])).collect();
            let profile_choices = std::array::from_fn(|slot| {
                Self::best_offers(&self.joint_options[slot], |snap| profiles[(snap + 1) as usize][slot])
            });
            let song = &self.songs[indices[0]];
            let powers = Arc::new(self.roster.iter().flat_map(|&member|
                song.snap_power[member * width..(member + 1) * width].iter().copied()).collect::<Vec<_>>());
            let minimum_power = self.joint_options.iter().enumerate().fold(base_power, |sum, (slot, choices)| {
                let minimum = choices.iter().map(|&snap| powers[slot * width
                    + if snap < 0 { width - 1 } else { snap as usize }]).fold(f64::INFINITY, f64::min);
                (sum + minimum).next_down()
            });
            let prices_offsets = minimum_power.is_finite() && minimum_power > 0.0;
            if prices_offsets {
                // P >= minimum_power, so P * (coefficient + offset / minimum_power)
                // bounds P * coefficient + offset while pricing the same snapshot for both.
                for (index, row) in combined.iter_mut().enumerate() {
                    let coefficient = (row[0] + (offsets[index / width][index % width] / minimum_power).next_up()).next_up();
                    row.fill(coefficient);
                }
            }
            let prices = SharedPrice::for_options(base_power, base, width, &combined, &powers, &self.joint_options);
            let component = Arc::new(crate::rewards::FixedComponent { base, converted: 1.0, width, rows: combined });
            let Some(rows) = crate::rewards::FixedRegionRows::new(&[component], Arc::new(AtomicUsize::new(0)), 0) else { continue; };
            if let Some(bound) = crate::rewards::FixedRegionBound::new(Arc::new(rows), &[0, 1, 2, 3, 4], powers) {
                let offset_choices = std::array::from_fn(|slot| {
                    Self::best_offers(&self.joint_options[slot], |snap| offsets[slot][(snap + 1) as usize])
                });
                groups.push(SharedPowerBound { songs: indices, bound, base_power, coefficient: base,
                    profiles, profile_choices, offset, offsets, offset_choices, prices_offsets, prices, plain_score });
            }
        }
        let mut covered = vec![false; self.songs.len()];
        for group in &groups {
            for &song in &group.songs { covered[song] = true; }
        }
        self.joint_uncovered = covered.into_iter().enumerate()
            .filter_map(|(index, covered)| (!covered).then_some(index)).collect();
        if !groups.is_empty() {
            self.joint_fixed_luck = self.fixed_luck_assignments(&self.roster, &self.joint_options);
            self.joint_domain.resize(width, [false; 5]);
            self.joint_domain.fill([false; 5]);
            for (slot, choices) in self.joint_options.iter().enumerate() {
                for &snap in choices {
                    self.joint_domain[(snap + 1) as usize][slot] = true;
                }
            }
        }
        self.joint = Some(groups);
    }

    fn joint_bound(&mut self, assigned: &[i32], from: usize, totals: Totals, cutoff: f64, upper: f64) -> Option<f64> {
        if self.songs.len() < 2 { return None; }
        let single_domain = self.same_power && self.songs.iter().all(|song| song.adapter.combo_bound.is_some());
        let wide_root = from == 0 && self.ordered_snapshot_programs && self.songs.len() >= 16;
        if !single_domain && (!cutoff.is_finite() || cutoff <= 0.0 || !wide_root && upper > cutoff * 1.05) {
            return None;
        }
        self.prepare_joint(false);
        let groups = self.joint.as_mut()?;
        if groups.is_empty() { return None; }
        let mut bounds = self.last_bounds.clone();
        let count = self.songs.len() as f64;
        let mut total = f64::INFINITY;
        for group in groups {
            let group_size = group.songs.len() as f64;
            let first = group.songs[0];
            let song = &self.songs[first];
            let outside = bounds.iter().enumerate().filter(|(index, _)| group.songs.binary_search(index).is_err())
                .fold(0.0_f64, |sum, (_, value)| (sum + value).next_up());
            let offset = if group.prices_offsets { group.offset } else {
                (0..5).fold(group.offset, |sum, slot| {
                    let value = if slot < from { group.offsets[slot][(assigned[slot] + 1) as usize] }
                        else { Self::first_available(&group.offset_choices[slot], &assigned[..from]).max(0.0) };
                    (sum + value).next_up()
                })
            };
            let target = ((((cutoff * count).next_down() - outside).next_down() / group_size).next_down() - offset)
                .next_down().max(0.0);
            let mut domain = if self.same_power {
                totals
            } else {
                Self::totals(song, &self.roster, assigned, from)
            };
            domain.power = ((domain.power - song.base).next_up() + group.base_power).next_up();
            group.bound.prepare_prefix(&self.joint_options, assigned, from);
            let priced = group.bound.physical_score_ceiling(group.base_power, domain, Criterion::Mean, None, target);
            let cap = (group_size * (priced + offset).next_up()).next_up();
            let separate = group.songs.iter().fold(0.0_f64, |sum, &i| (sum + bounds[i]).next_up());
            if cap < separate {
                for &index in &group.songs { bounds[index] = 0.0; }
                bounds[first] = cap;
            }
            total = (bounds.iter().fold(0.0_f64, |sum, value| (sum + value).next_up()) / count).next_up();
            if total < cutoff { break; }
        }
        Some(total)
    }

    fn inherited_ceiling(&self, members: &[usize], assigned: &[i32], from: usize) -> Option<&PrefixCeilings> {
        if self.roster != members { return None; }
        self.prefix_ceilings.iter().enumerate().take(from).rev().find_map(|(depth, record)| {
            record.snapshots.filter(|prefix| prefix[..depth] == assigned[..depth]).map(|_| record)
        })
    }

    fn upper(
        &mut self,
        members: &[usize],
        assigned: &[i32],
        from: usize,
        refined: bool,
        limit: f64,
        reference_power: f64,
    ) -> f64 {
        if !self.eligible(members) {
            return f64::NEG_INFINITY;
        }
        let mut totals = std::mem::take(&mut self.last_totals);
        let mut bounds = std::mem::take(&mut self.last_bounds);
        let same_prefix = self.roster == members && bounds.len() == self.songs.len()
            && self.last_prefix.is_some_and(|(length, prefix)| {
                length == from && prefix[..from] == assigned[..from]
            });
        if !same_prefix {
            let first = &self.songs[0];
            let snapshot_cap = (reference_power - first.base).next_up();
            totals.clear();
            for (index, song) in self.songs.iter().enumerate() {
                let power = if song.power_source == index {
                    Self::assignment_power(song, assigned, from)
                } else {
                    totals[song.power_source].power
                };
                // Matching on the reference chart also bounds any chart whose
                // snapshot points differ only by its fixed member base.
                let power = if Arc::ptr_eq(&first.snap_power, &song.snap_power) {
                    power.min((snapshot_cap + song.base).next_up())
                } else {
                    power
                };
                totals.push(Self::totals_with_power(song, members, assigned, from, power));
            }
            bounds.clear();
            bounds.extend(self
                .songs
                .iter_mut()
                .zip(&totals)
                .map(|(song, &t)| {
                    song.adapter.bound(t).min(
                        song.adapter
                            .cheap_assignment_bound(members, t, assigned, from),
                    )
                })
                );
        }
        if !same_prefix {
            // A per-chart ceiling covers every extension of that assignment prefix.
            if let Some(ancestor) = self.inherited_ceiling(members, assigned, from) {
                for (upper, &previous) in bounds.iter_mut().zip(&ancestor.scores) {
                    *upper = upper.min(previous);
                }
            }
        }
        let count = bounds.len() as f64;
        let sum_bounds = |values: &[f64]| values.iter().fold(0.0_f64, |sum, value| (sum + value).next_up());
        let mut sum = sum_bounds(&bounds);
        if refined && (sum / count).next_up() >= limit {
            let team = (from == 5).then(|| Team { members: members.to_vec(), snaps: assigned.to_vec() });
            let deadline = crate::clock::preparation_deadline();
            'refine: for expensive in [false, true] {
                // For a complete assignment the direct score ceilings
                // already include its power and all fixed slot profiles.
                if !expensive && team.is_some() { continue; }
                for &index in &self.refinement_order {
                    if crate::clock::deadline_expired(deadline) { break 'refine; }
                    let song = &mut self.songs[index];
                    let previous = bounds[index];
                    let others = if sum.is_finite() && previous.is_finite() {
                        (sum - previous).next_up()
                    } else { bounds.iter().enumerate().filter(|(i, _)| *i != index)
                        .fold(0.0_f64, |sum, (_, value)| (sum + value).next_up()) };
                    let target = if limit.is_finite() { (limit * count - others).max(0.0) } else { 0.0 };
                    let t = totals[index];
                    let value = if expensive {
                        song.adapter.assignment_order_bound(members, t, assigned, from, target)
                    } else { song.adapter.shared_joint_upper(members, assigned, from, t, target) };
                    if let Some(upper) = value { bounds[index] = bounds[index].min(upper); }
                    if expensive && !crate::clock::deadline_expired(deadline) {
                        if let Some(upper) = song.adapter.assignment_bound(members, t, assigned, from) {
                            bounds[index] = bounds[index].min(upper);
                        }
                        if let Some(team) = &team {
                            if let Some(upper) = song.adapter.leaf_bound(team, t) { bounds[index] = bounds[index].min(upper); }
                        }
                    }
                    sum = if sum.is_finite() && previous.is_finite() && bounds[index].is_finite() {
                        ((sum - previous).next_up() + bounds[index]).next_up()
                    } else { sum_bounds(&bounds) };
                    if (sum / count).next_up() < limit { break; }
                }
                if (sum / count).next_up() < limit { break; }
            }
        }
        let upper = (sum / count).next_up();
        let mut prefix = [-1; 5];
        prefix[..from].copy_from_slice(&assigned[..from]);
        if refined && self.roster == members && from < self.prefix_ceilings.len() {
            let record = &mut self.prefix_ceilings[from];
            record.snapshots = Some(prefix);
            record.scores.clone_from(&bounds);
            record.mean = upper;
        }
        self.last_prefix = (self.roster == members).then_some((from, prefix));
        self.last_bounds = bounds;
        self.last_totals = totals;
        upper
    }
}

impl Objective for Combined<'_, '_> {
    fn retain_budget_bound(&self) -> bool { false }

    fn filter_member_dominates(&self, better: usize, worse: usize) -> bool { self.songs.iter().all(|song| song.adapter.filter_member_dominates(better, worse)) }
    fn filter_permutation_invariant(&self) -> bool { self.songs.iter().all(|song| song.adapter.filter_permutation_invariant()) }

    fn filter_snap_dominates(&self, member: usize, better: i32, worse: i32) -> bool {
        self.songs.iter().all(|song| song.adapter.filter_snap_dominates(member, better, worse))
    }
    fn has_team_filter(&self) -> bool { self.songs.iter().any(|song| song.adapter.has_team_filter()) }
    fn team_filter_impossible(&self) -> bool { self.songs.iter().any(|song| song.adapter.team_filter_impossible()) }
    fn feasible_assignment(&self, members: &[usize], assigned: &[i32], from: usize, options: &[Vec<i32>]) -> bool {
        self.songs.iter().all(|song| song.adapter.feasible_assignment(members, assigned, from, options))
    }
    fn feasible_team(&mut self, team: &Team, _power: f64) -> Result<bool, String> {
        if !self.eligible(&team.members) { return Ok(false); }
        for song in &mut self.songs {
            let kernel = &song.adapter.kernel;
            let members: Vec<_> = team.members.iter().map(|&index| kernel.members[index].clone()).collect();
            let snaps: Vec<_> = team.snaps.iter().map(|&index| (index >= 0).then(|| kernel.snaps[index as usize].clone())).collect();
            let power = power::team_power(kernel.master, self.player, &members, &snaps, 0, Some(song.music), &[])?.total;
            if !song.adapter.feasible_team(team, power as f64)? { return Ok(false); }
        }
        Ok(true)
    }
    fn observe_hits(&mut self, hits: &[Hit]) {
        if let Some(hit) = hits.get(self.k - 1) {
            self.floor = self.floor.max(hit.key);
        }
    }
    fn assignment_domains(&mut self, members: &[usize], options: &[Vec<i32>]) -> Option<Vec<Vec<Vec<i32>>>> {
        if !self.ordered_snapshot_programs || self.songs.len() < 16 { return None; }
        let kernel = &self.songs.iter().find(|song| song.adapter.kernel.fixed_luck_chart())?.adapter.kernel;
        let mut split = false;
        let domains = options.iter().zip(members).map(|(choices, &member)| {
            let (plain, changing): (Vec<_>, Vec<_>) = choices.iter().copied()
                .partition(|&snap| kernel.plain_luck_support(member, snap));
            split |= !plain.is_empty() && !changing.is_empty();
            [plain, changing].into_iter().filter(|row| !row.is_empty()).collect()
        }).collect();
        split.then_some(domains)
    }
    fn partial_assignment_domains(&self) -> bool { false }
    fn refine_assignment_domains(&mut self, members: &[usize], options: &[Vec<i32>]) -> Option<Vec<Vec<Vec<i32>>>> {
        if !self.ordered_snapshot_programs || self.songs.len() < 16 { return None; }
        if let Some(mask) = &self.conversion_mask {
            let width = self.songs[0].adapter.kernel.snaps.len() + 1;
            let converts = |slot: usize, snap: i32| mask[members[slot] * width + (snap + 1) as usize];
            if options.iter().fold(1usize, |size, row| size.saturating_mul(row.len())) >= 256
                && options.iter().enumerate().all(|(slot, row)| row.iter().any(|&snap| !converts(slot, snap)))
            {
                if let Some(slot) = options.iter().enumerate().position(|(slot, row)| row.iter().any(|&snap| converts(slot, snap))) {
                    let domains = options.iter().enumerate().map(|(index, row)| {
                        if index != slot { return vec![row.clone()]; }
                        let (plain, changing): (Vec<_>, Vec<_>) = row.iter().copied().partition(|&snap| !converts(slot, snap));
                        if converts(slot, row[0]) { vec![changing, plain] } else { vec![plain, changing] }
                    }).collect();
                    return Some(domains);
                }
            }
        }
        let kernel = &self.songs.iter().find(|song| song.adapter.kernel.fixed_luck_chart())?.adapter.kernel;
        let mut unresolved = options.iter().zip(members).enumerate().filter_map(|(index, (choices, &member))|
            kernel.luck_reference(member, choices).is_none().then_some(index));
        let slot = unresolved.next()?;
        let member = members[slot];
        if unresolved.next().is_some() || kernel.gk_members[member].gekisou_skill.is_none() { return None; }
        let mut programs = Vec::new();
        let mut groups: Vec<Vec<i32>> = Vec::new();
        for &snap in &options[slot] {
            let program = (!kernel.plain_luck_support(member, snap)).then(|| kernel.luck_support_program(member, snap));
            if let Some(index) = programs.iter().position(|other| *other == program) {
                groups[index].push(snap);
            } else {
                if groups.len() == 8 { return None; }
                programs.push(program);
                groups.push(vec![snap]);
            }
        }
        if groups.len() <= 1 { return None; }
        Some(options.iter().enumerate().map(|(index, row)| {
            if index == slot { groups.clone() } else { vec![row.clone()] }
        }).collect())
    }
    fn live_floor(&self) -> bool {
        self.cooperative_floor
    }
    fn live_floor_exclusive(&self) -> bool {
        // A score from another shard cannot decide the identity of a tied result.
        !self.has_team_filter()
    }
    fn canonical_leader(&self) -> bool {
        self.songs
            .iter()
            .all(|song| song.adapter.canonical_leader())
    }
    fn assignment_proposal(&mut self) -> Option<[i32; 5]> {
        if !self.joint_seed_pending {
            return None;
        }
        let group = self.joint.as_ref()?.iter().max_by_key(|group| group.songs.len())?;
        let assignment = group.prices.first()?.assignment;
        // The matching uses a score ceiling. The search settles the
        // proposed formation normally before it can raise the incumbent.
        self.joint_seed_pending = false;
        Some(assignment)
    }
    fn seed_objective(&self) -> Option<Box<dyn Objective>> {
        if !self.warm_seed {
            return None;
        }
        let mut seed = PowerSeed::new(if self.same_levels {
                self.songs[0].adapter.kernel.bonuses.clone()
            } else {
                vec![0; self.songs[0].adapter.kernel.members.len()]
            });
        if let Some(affine) = self.affine.as_ref().filter(|affine| affine.base > 0.0) {
            seed.scale = self.songs[0].power_cap / affine.base;
            seed.width = self.songs[0].adapter.kernel.snaps.len() + 1;
            seed.weights.clone_from(&affine.weights);
        }
        Some(Box::new(seed))
    }
    fn seed_ready(&self, detail: &Value) -> bool {
        detail["songs"].is_array()
    }
    fn skill(&mut self, member: usize, snap: i32) -> (f64, f64) {
        if let Some(affine) = &self.affine {
            let width = self.songs[0].adapter.kernel.snaps.len() + 1;
            let column = if snap < 0 { width - 1 } else { snap as usize };
            return (0.0, affine.weights[member * width + column]);
        }
        (
            0.0,
            self.songs
                .iter()
                .map(|s| {
                    let width = s.adapter.kernel.snaps.len() + 1;
                    s.weights[member * width + if snap < 0 { width - 1 } else { snap as usize }]
                })
                .fold(0.0, f64::max),
        )
    }
    fn member_bonus(&self, member: usize) -> f64 {
        if self.same_levels {
            self.songs[0].adapter.kernel.bonuses[member] as f64
        } else {
            0.0
        }
    }
    fn conditional_snap_dominance(&self) -> bool {
        true
    }
    fn snap_dominance(&mut self, member: usize, snap: i32) -> Vec<f64> {
        if let Some((shape, slot)) = self.snapshot_program(member, snap) {
            let shape = shape as f64;
            // Each old [shape, -shape] pair required equality. A single
            // interned tuple and one power per shared table give the same test.
            return [shape, -shape].into_iter().chain(self.dominance_power.iter()
                .map(|&song| self.songs[song].snap_power[slot])).collect();
        }
        self.songs
            .iter_mut()
            .flat_map(|song| {
                let mut shape = song.adapter.snap_dominance(member, snap);
                let width = song.adapter.kernel.snaps.len() + 1;
                shape.push(
                    song.snap_power
                        [member * width + if snap < 0 { width - 1 } else { snap as usize }],
                );
                shape
            })
            .collect()
    }
    fn snap_program_class(&mut self, member: usize, snap: i32) -> Option<(u32, u8)> {
        if !self.ordered_snapshot_programs { return None; }
        if let Some(&id) = self.roster_programs.get(&(member, snap)) { return Some((id, 0)); }
        let key = self.shape_charts.iter().map(|&index| {
            self.songs[index].adapter.kernel.nominal_roster_shape_id(member, snap, &self.roster)
        }).collect::<Vec<_>>();
        let next = self.roster_program_ids.len() as u32;
        let id = *self.roster_program_ids.entry(key).or_insert(next);
        self.roster_programs.insert((member, snap), id);
        Some((id, 0))
    }
    fn numeric_snap_dominance(&mut self, member: usize, better: i32, worse: i32, _delta: f64) -> bool {
        if !self.ordered_snapshot_programs || !self.songs.iter().any(|song| song.adapter.kernel.just_context.is_some()) {
            return false;
        }
        for song in &mut self.songs {
            let width = song.adapter.kernel.snaps.len() + 1;
            let column = |snap: i32| if snap < 0 { width - 1 } else { snap as usize };
            let delta = song.snap_power[member * width + column(better)]
                - song.snap_power[member * width + column(worse)];
            if delta < 0.0 || !song.adapter.exchange_snap_equivalent(member, better, worse)
                && !song.adapter.numeric_snap_dominance(member, better, worse, delta)
            {
                return false;
            }
        }
        true
    }
    fn exchange_snap_equivalent(&mut self, member: usize, a: i32, b: i32) -> bool {
        self.snap_dominance(member, a) == self.snap_dominance(member, b)
    }
    fn bound(&mut self, t: Totals) -> f64 {
        if let Some(affine) = &self.affine {
            return affine.upper(t);
        }
        let count = self.songs.len() as f64;
        (self.songs.iter_mut().fold(0.0_f64, |sum, s| {
            (sum + s.adapter.bound(Totals {
                power: s.power_gap.map_or(s.power_cap, |gap| s.power_cap.min((t.power + gap).next_up())),
                ..t
            }))
            .next_up()
        }) / count)
            .next_up()
    }
    fn prepare_members(&mut self, characters: &[Vec<usize>]) {
        fn suffix(characters: &[Vec<usize>], values: &[f64]) -> Vec<[f64; 5]> {
            let mut rows = vec![[f64::NEG_INFINITY; 5]; characters.len() + 1];
            rows[characters.len()][0] = 0.0;
            for index in (0..characters.len()).rev() {
                let peak = characters[index].iter().map(|&member| values[member])
                    .fold(f64::NEG_INFINITY, f64::max);
                let next = rows[index + 1];
                rows[index] = std::array::from_fn(|count| {
                    if count == 0 { return 0.0; }
                    if peak == f64::NEG_INFINITY || next[count - 1] == f64::NEG_INFINITY {
                        return next[count];
                    }
                    next[count].max((next[count - 1] + peak).next_up())
                });
            }
            rows
        }
        let mut domains = HashMap::default();
        for song in &mut self.songs {
            song.adapter.prepare_members(characters);
            if !self.same_power || self.affine.is_none() {
                song.free_power = Arc::clone(domains.entry(Arc::as_ptr(&song.member_power))
                    .or_insert_with(|| Arc::new(suffix(characters, &song.member_power))));
                song.free_skill = suffix(characters, &song.member_skill);
            }
        }
    }
    fn product_base(&self) -> Option<f64> {
        self.affine.as_ref().filter(|affine| affine.gap == 0.0).map(|affine| affine.base)
    }
    fn joint_targets(&mut self, limit: f64) -> Option<Vec<search::JointTarget>> {
        let affine = self.affine.as_ref().filter(|affine| affine.gap == 0.0)?;
        if !limit.is_finite() || limit <= affine.absolute {
            return None;
        }
        Some(vec![search::JointTarget {
            bonus: 0.0,
            product: ((limit.next_down() - affine.absolute).next_down() / (1.0 + 3e-12)).next_down(),
        }])
    }
    fn prepare_member_domains_at(&mut self, leader: usize, _power: &[f64], options: &[Vec<i32>]) {
        let first = &self.songs[0].adapter.kernel;
        let profile = power::leader_profile(first.master, &first.members[leader]);
        let mut domains = HashMap::default();
        for song in &mut self.songs {
            let k = &song.adapter.kernel;
            let width = k.snaps.len() + 1;
            let (base, member_power) = domains.entry(song.power_domain).or_insert_with(|| {
                let base: Vec<_> = k.members.iter().enumerate().map(|(m, member)| {
                    (song.bases[m].fixed_points
                        + power::leader_points(
                            &song.bases[m],
                            &power::leader_percent_bound(k.master, &profile, member),
                        )) as f64
                }).collect();
                let totals: Vec<_> = options.iter().enumerate().map(|(member, choices)| {
                    let best = choices.iter().map(|&snap| song.snap_power[member * width + if snap < 0 { width - 1 } else { snap as usize }])
                        .fold(f64::NEG_INFINITY, f64::max);
                    (base[member] + best).next_up()
                }).collect();
                (base, Arc::new(totals))
            });
            song.adapter.prepare_member_domains_at(leader, base, options);
            song.member_power = Arc::clone(member_power);
            song.member_skill = options.iter().enumerate().map(|(member, choices)| choices.iter()
                .map(|&snap| song.weights[member * width + if snap < 0 { width - 1 } else { snap as usize }])
                .fold(f64::NEG_INFINITY, f64::max)).collect();
        }
    }
    fn member_bound_at(
        &mut self,
        members: &[usize],
        t: Totals,
        left: usize,
        from: usize,
        limit: f64,
    ) -> Option<f64> {
        if self.same_power && self.affine.is_some() {
            return Some(self.bound(t));
        }
        let totals: Vec<_> = self.songs.iter().map(|song| Totals {
            power: members.iter().fold(song.free_power[from][left], |sum, &member| (sum + song.member_power[member]).next_up())
                .min(song.power_gap.map_or(song.power_cap, |gap| song.power_cap.min((t.power + gap).next_up()))),
            skill: members.iter().fold(song.free_skill[from][left], |sum, &member| (sum + song.member_skill[member]).next_up()),
            ..t
        }).collect();
        let mut bounds: Vec<_> = self.songs.iter_mut().zip(&totals).map(|(song, &t)| song.adapter.bound(t)).collect();
        let count = bounds.len() as f64;
        let sum = |bounds: &[f64]| bounds.iter().fold(0.0_f64, |sum, value| (sum + value).next_up());
        let mut upper = sum(&bounds);
        for nominal in [false, true] {
            if (upper / count).next_up() < limit { break; }
            for (index, song) in self.songs.iter_mut().enumerate().filter(|(_, song)| song.adapter.kernel.chart.luck == nominal) {
                let old = bounds[index];
                let others = if upper.is_finite() && old.is_finite() { (upper - old).next_up() }
                    else { bounds.iter().enumerate().filter(|(at, _)| *at != index).fold(0.0_f64, |sum, (_, value)| (sum + value).next_up()) };
                let target = if limit.is_finite() { (limit * count - others).max(0.0) } else { 0.0 };
                if let Some(value) = song.adapter.member_bound_at(members, totals[index], left, from, target) { bounds[index] = bounds[index].min(value); }
                upper = if upper.is_finite() && old.is_finite() && bounds[index].is_finite() { ((upper - old).next_up() + bounds[index]).next_up() }
                    else { sum(&bounds) };
                if (upper / count).next_up() < limit { break; }
            }
        }
        Some((upper / count).next_up())
    }
    fn prepare_assignment(&mut self, members: &[usize], options: &[Vec<i32>]) {
        if let Some(mask) = &self.conversion_mask {
            let width = self.songs[0].adapter.kernel.snaps.len() + 1;
            self.open_converters = std::array::from_fn(|slot| {
                options[slot].iter().any(|&snap| mask[members[slot] * width + (snap + 1) as usize])
            });
        }
        self.last_prefix = None;
        for ceiling in &mut self.prefix_ceilings { ceiling.snapshots = None; }
        // These per-chart envelopes cover every child of their original
        // snapshot domain. Price the remaining physical choices in the child.
        let retains_joint = self.roster == members && !self.joint_domain.is_empty()
            && options.iter().enumerate().all(|(slot, choices)| {
                choices.iter().all(|&snap| self.joint_domain[(snap + 1) as usize][slot])
            });
        let new_luck_envelope = retains_joint && !self.joint_fixed_luck
            && self.joint_options != options && self.fixed_luck_assignments(members, options);
        if !retains_joint || new_luck_envelope {
            self.joint = None;
            self.joint_domain.clear();
            self.joint_fixed_luck = false;
        } else if self.joint_options != options {
            for group in self.joint.iter_mut().flatten() {
                group.profile_choices = std::array::from_fn(|slot| {
                    Self::best_offers(&options[slot], |snap| group.profiles[(snap + 1) as usize][slot])
                });
                group.offset_choices = std::array::from_fn(|slot| {
                    Self::best_offers(&options[slot], |snap| group.offsets[slot][(snap + 1) as usize])
                });
                if let Some(plain) = &mut group.plain_score { plain.prepare(options); }
            }
        }
        self.joint_last_prefix = None;
        self.window_bounds = std::array::from_fn(|_| None);
        self.window_attempted = [false; 6];
        self.window_budgets.clear();
        if let Some(mask) = &self.window_converters {
            let width = self.songs[0].adapter.kernel.snaps.len() + 1;
            self.window_options = std::array::from_fn(|slot| options[slot].iter().any(|&snap|
                mask[members[slot] * width + (snap + 1) as usize]));
            if let Some(window) = self.songs.iter().find_map(|song| song.adapter.kernel.just_window.as_ref()) {
                for (required, values) in window.pricing_constraints().unwrap_or_default() {
                    let rows: [Vec<f64>; 5] = std::array::from_fn(|slot| (0..width).map(|col|
                        values[members[slot] * width + if col == 0 { width - 1 } else { col - 1 }]).collect());
                    let choices = std::array::from_fn(|slot| std::array::from_fn(|kind| {
                        let mut row: Vec<_> = options[slot].iter().filter(|&&snap|
                            usize::from(mask[members[slot] * width + (snap + 1) as usize]) == kind)
                            .map(|&snap| (snap, rows[slot][(snap + 1) as usize])).collect();
                        row.sort_unstable_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
                        row.truncate(6);
                        row
                    }));
                    self.window_budgets.push(WindowBudget { required, values: rows, choices });
                }
            }
        }
        self.joint_options.resize_with(options.len(), Vec::new);
        for (stored, current) in self.joint_options.iter_mut().zip(options) {
            stored.clone_from(current);
        }
        let roster_changed = self.roster != members;
        if roster_changed {
            self.roster_programs.clear();
            self.roster_program_ids.clear();
            self.roster = members.to_vec();
        }
        let master = self.songs[0].adapter.kernel.master;
        let roster = roster_changed.then(|| {
            let roster = members.iter().map(|&m| self.songs[0].adapter.kernel.members[m].clone()).collect::<Vec<_>>();
            let profile = power::leader_profile(master, &roster[0]);
            (roster, profile)
        });
        let mut power_tables = HashMap::default();
        let mut leader_percents = HashMap::default();
        for index in 0..self.songs.len() {
            let previous = if let Some((roster, profile)) = &roster {
                let previous = if self.same_power && index > 0 {
                    Some(0)
                } else {
                    let song = &mut self.songs[index];
                    let kernel = &song.adapter.kernel;
                    let percents = leader_percents.entry(song.music.music_type)
                        .or_insert_with(|| power::leader_percents(master, profile, roster, 0, Some(song.music)));
                    let width = kernel.snaps.len() + 1;
                    let base: [f64; 5] = std::array::from_fn(|i| {
                        let m = members[i];
                        (song.bases[m].fixed_points + power::leader_points(&song.bases[m], &percents[i])) as f64
                    });
                    let key = (Arc::as_ptr(&song.snap_power), base.map(f64::to_bits));
                    if let Some(&known) = power_tables.get(&key) {
                        Some(known)
                    } else {
                        power_tables.insert(key, index);
                        song.base = base.iter().sum();
                        // The two integer components fit in binary64 and
                        // their sum has at most 53 significant bits.
                        song.exact_prepared_power = song.exact_snapshot_power
                            && base.iter().all(|value| value.abs() <= EXACT_POWER_COMPONENT);
                        song.slots = Arc::new(members.iter().enumerate().map(|(slot, &m)| {
                            (0..width).map(|j| base[slot] + song.snap_power[m * width + j]).collect()
                        }).collect());
                        None
                    }
                };
                self.songs[index].power_source = previous.unwrap_or(index);
                if let Some(known) = previous {
                    self.songs[index].slots = Arc::clone(&self.songs[known].slots);
                    self.songs[index].base = self.songs[known].base;
                    self.songs[index].exact_prepared_power = self.songs[known].exact_prepared_power;
                }
                previous
            } else {
                let known = self.songs[index].power_source;
                (known != index).then_some(known)
            };
            if let Some(known) = previous {
                self.songs[index].power_choices = Arc::clone(&self.songs[known].power_choices);
            }
            let song = &mut self.songs[index];
            if roster_changed {
                song.roster_bonus = members.iter().map(|&member| song.adapter.kernel.bonuses[member] as f64).sum();
            }
            let width = song.adapter.kernel.snaps.len() + 1;
            let best_five = |slot: usize, values: &[f64]| Self::best_offers(&options[slot], |snap| {
                values[if snap < 0 { width - 1 } else { snap as usize }]
            });
            if previous.is_none() {
                song.power_choices = Arc::new((0..5).map(|slot| best_five(slot, &song.slots[slot])).collect());
            }
            song.skill_choices = members
                .iter()
                .enumerate()
                .map(|(slot, &member)| {
                    best_five(slot, &song.weights[member * width..(member + 1) * width])
                })
                .collect();
            song.adapter.prepare_assignment_power(song.base);
            song.adapter.prepare_assignment(members, options);
        }
    }
    fn cheap_assignment_bound(
        &mut self,
        members: &[usize],
        t: Totals,
        assigned: &[i32],
        from: usize,
    ) -> f64 {
        if let Some(affine) = &self.affine {
            let upper = affine.upper(t);
            let upper = if upper >= self.floor {
                self.joint_prefix_upper(members, assigned, from, t.power).map_or(upper, |grouped| upper.min(grouped))
            } else { upper };
            if self.same_power || upper < self.floor { return upper; }
            return upper.min(self.upper(members, assigned, from, false, f64::INFINITY, t.power));
        }
        self.upper(members, assigned, from, false, f64::INFINITY, t.power)
    }
    fn assignment_order_bound(
        &mut self,
        members: &[usize],
        t: Totals,
        assigned: &[i32],
        from: usize,
        limit: f64,
    ) -> Option<f64> {
        let mut coarse = self.bound(t);
        if let Some(record) = self.inherited_ceiling(members, assigned, from) { coarse = coarse.min(record.mean); }
        if coarse < limit || crate::clock::preparation_expired() { return Some(coarse); }
        if self.joint.is_none() && from == 0 && self.ordered_snapshot_programs
            && self.songs.len() >= 16 && limit.is_finite() && limit > 0.0
            && self.joint_options.iter().fold(1usize, |count, row| count.saturating_mul(row.len())) >= 256
        {
            self.prepare_joint(true);
        }
        let grouped = self.joint_prefix_upper(members, assigned, from, t.power);
        if let Some(upper) = grouped {
            coarse = coarse.min(upper);
            if coarse < limit { return Some(coarse); }
        }
        let window = self.window_upper(assigned, from);
        if let Some(upper) = window.filter(|&upper| upper < limit) { return Some(coarse.min(upper)); }
        if from <= 1 && grouped.is_some() && limit.is_finite() && limit > 0.0 && coarse > limit * 1.2 {
            // A wide prefix far above the cutoff benefits from fixing a
            // snapshot before solving separate per-chart refinements.
            return Some(window.map_or(coarse, |upper| coarse.min(upper)));
        }
        let upper = coarse.min(self.upper(members, assigned, from, true, limit, t.power));
        let upper = window.map_or(upper, |window| upper.min(window));
        Some(if upper < limit || crate::clock::preparation_expired() {
            upper
        } else {
            self.joint_bound(assigned, from, t, limit, upper).map_or(upper, |joint| upper.min(joint))
        })
    }
    fn interval(&mut self, team: &Team, _t: Totals) -> (f64, f64) {
        self.formation_bounds = None;
        if self
            .completed
            .iter()
            .any(|known| known.members == team.members && known.snaps == team.snaps)
        {
            return (f64::NEG_INFINITY, f64::NEG_INFINITY);
        }
        if self.roster != team.members {
            return (0.0, f64::INFINITY);
        }
        let floor = if self.cooperative_floor {
            self.floor.max(crate::search_floor())
        } else {
            self.floor
        };
        if let Some(upper) = self.joint_prefix_upper(&team.members, &team.snaps, 5, f64::INFINITY).filter(|&upper| upper < floor) {
            return (0.0, upper);
        }
        if let Some(upper) = self.grouped_leaf_upper(team).filter(|&upper| upper < floor) {
            return (0.0, upper);
        }
        let coarse = self.upper(&team.members, &team.snaps, 5, true, floor, f64::INFINITY);
        if coarse <= floor {
            return (0.0, coarse);
        }
        let members_key = team.members.as_slice().try_into().unwrap();
        let snapshots_key = team.snaps.as_slice().try_into().unwrap();
        let mut bounds = std::mem::take(&mut self.last_bounds);
        if self.songs.iter().any(|song| song.adapter.kernel.chart.luck) {
            self.formation_bounds = Some(FormationBounds {
                members: members_key, snapshots: snapshots_key, scores: bounds, refined: false,
            });
            return (0.0, coarse);
        }
        let kernel = &self.songs[0].adapter.kernel;
        let members: Vec<_> = team
            .members
            .iter()
            .map(|&m| kernel.members[m].clone())
            .collect();
        let snaps: Vec<_> = team
            .snaps
            .iter()
            .filter(|&&s| s >= 0)
            .map(|&s| kernel.snaps[s as usize].clone())
            .collect();
        let counters =
            crate::support_aliases::JustCounters::for_formation(kernel.master, &members, &snaps);
        let mut upper = coarse;
        for (index, song) in self.songs.iter_mut().enumerate() {
            let width = song.adapter.kernel.snaps.len() + 1;
            let power = team
                .snaps
                .iter()
                .enumerate()
                .map(|(slot, &snap)| {
                    song.slots[slot][if snap < 0 { width - 1 } else { snap as usize }]
                })
                .sum::<f64>();
            let Some(value) = song
                .adapter
                .kernel
                .shared_mean_upper(team, power, &counters)
            else {
                self.formation_bounds = Some(FormationBounds {
                    members: members_key, snapshots: snapshots_key, scores: bounds, refined: false,
                });
                return (0.0, upper);
            };
            bounds[index] = bounds[index].min(value);
            upper = (bounds
                .iter()
                .fold(0.0_f64, |sum, value| (sum + value).next_up())
                / bounds.len() as f64)
                .next_up();
            if upper <= floor {
                return (0.0, upper);
            }
        }
        self.formation_bounds = Some(FormationBounds {
            members: members_key, snapshots: snapshots_key, scores: bounds, refined: true,
        });
        (0.0, upper)
    }
    fn exact(&mut self, team: &Team, _power: f64, _slots: &[f64]) -> Result<Exact, String> {
        match self.settle(team, None, false)? {
            search::Evaluation::Exact(value) => Ok(value),
            search::Evaluation::Pruned(_) => unreachable!(),
        }
    }
    fn try_exact(&mut self, team: &Team, _power: f64, _slots: &[f64]) -> Result<Option<Exact>, String> {
        match self.settle(team, None, true) {
            Ok(search::Evaluation::Exact(value)) => Ok(Some(value)),
            Ok(search::Evaluation::Pruned(_)) => unreachable!(),
            Err(error) if error == SETTLEMENT_DEADLINE => Ok(None),
            Err(error) => Err(error),
        }
    }
    fn evaluate(
        &mut self,
        team: &Team,
        _power: f64,
        _slots: &[f64],
        cutoff: search::Cutoff,
    ) -> Result<search::Evaluation, String> {
        self.settle(team, Some(cutoff), false)
    }
    fn try_evaluate(&mut self, team: &Team, _power: f64, _slots: &[f64], cutoff: search::Cutoff)
        -> Result<Option<search::Evaluation>, String> {
        match self.settle(team, Some(cutoff), true) {
            Ok(value) => Ok(Some(value)),
            Err(error) if error == SETTLEMENT_DEADLINE => Ok(None),
            Err(error) => Err(error),
        }
    }
}

fn initial_hit(env: &Environment<'_>, charts: &mut [Chart], music: &[Music], seeds: &[i32], team: &Team, minimum: i32, deadline: f64) -> Result<Option<Hit>, String> {
    if !search::valid_seed(env.members, env.snaps, &env.constraints, team) { return Ok(None); }
    let members: Vec<_> = team.members.iter().map(|&index| env.members[index].clone()).collect();
    let mut selected = Vec::new();
    let snaps: Vec<_> = team.snaps.iter().map(|&index| {
        if index < 0 { None } else {
            selected.push(env.snaps[index as usize].clone());
            Some(env.snaps[index as usize].clone())
        }
    }).collect();
    let local = Team { members: (0..5).collect(), snaps: team.snaps.iter().scan(0, |next, &index| {
        Some(if index < 0 { -1 } else { let value = *next; *next += 1; value })
    }).collect() };
    let (master, player, policy, generation) = (env.master, env.player, env.window_policy, env.generation);
    let (min_bonus, max_bonus) = (env.constraints.min_bonus, env.constraints.max_bonus);
    let mut powers_by_music = HashMap::default();
    let mut chart_powers = Vec::with_capacity(music.len());
    for music in music {
        let key = music.slot_base_key(player);
        if let Some(power) = powers_by_music.get(&key) {
            chart_powers.push(Arc::clone(power));
        } else {
            if (env.now)() >= deadline { return Ok(None); }
            let power = Arc::new(power::team_power(master, player, &members, &snaps, 0, Some(music), &[])?);
            powers_by_music.insert(key, Arc::clone(&power));
            chart_powers.push(power);
        }
    }
    let score_chart = |chart: &mut Chart, power: &power::TeamPower| -> Result<Option<Exact>, String> {
        if generation != crate::cancellation_generation() { return Ok(None); }
        let mut kernel = Kernel::new(master, &members, &selected, chart, seeds.to_vec(), Criterion::Mean);
        let bonus: f64 = kernel.bonuses.iter().map(|&value| value as f64).sum();
        if min_bonus.is_some_and(|minimum| bonus < minimum)
            || max_bonus.is_some_and(|maximum| bonus > maximum)
            || !kernel.meets_combo(&local.members, minimum.max(1)) { return Ok(None); }
        kernel.just_window = policy.map(|policy| crate::just_window::Prepared::new(master, kernel.chart, &members, &selected, policy, seeds)).transpose()?;
        if !kernel.feasible_team(&local, power.total as f64)? { return Ok(None); }
        let mut score = kernel.exact(&local, power.total as i32, true)?;
        kernel.decorate_window(&local, power.total as i32, &mut score.detail)?;
        Ok(Some(score))
    };
    let mut details = Vec::with_capacity(charts.len());
    let mut total = 0.0;
    let mut powers = 0.0;
    let mut slot_powers = vec![0.0; 5];
    let mut collect = |score: Exact, power: &power::TeamPower| {
        total += score.key;
        powers += power.total as f64;
        for (sum, value) in slot_powers.iter_mut().zip(&power.slots) { *sum += *value as f64; }
        details.push(json!({"detail":score.detail,"power":power.total,"slotPowers":power.slots}));
    };
    #[cfg(not(target_arch = "wasm32"))]
    let parallel = (16..=512).contains(&charts.len())
        && crate::parallel::workers(charts.len()) > 1;
    #[cfg(target_arch = "wasm32")]
    let parallel = false;
    if parallel {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let remaining = (deadline - (env.now)()).max(0.0);
            let access = std::time::Instant::now();
            let items: Vec<_> = charts.iter_mut().zip(&chart_powers).map(|(chart, power)| {
                (std::sync::Mutex::new(chart), power)
            }).collect();
            let scores = crate::parallel::map(&items, |(chart, power)| {
                if access.elapsed().as_secs_f64() * 1000.0 >= remaining {
                    Ok::<_, String>(Ok(None))
                } else {
                    Ok(crate::parallel::serial(|| score_chart(&mut **chart.lock().unwrap(), power)))
                }
            })?;
            for (result, power) in scores.into_iter().zip(&chart_powers) {
                let Some(result) = result? else { return Ok(None); };
                collect(result, power);
            }
            if (env.now)() >= deadline { return Ok(None); }
        }
    } else {
        for (chart, power) in charts.iter_mut().zip(&chart_powers) {
            if (env.now)() >= deadline { return Ok(None); }
            let Some(result) = score_chart(chart, power)? else { return Ok(None); };
            collect(result, power);
        }
    }
    let count = charts.len() as f64;
    for value in &mut slot_powers { *value /= count; }
    let mut detail = json!({"songs":details});
    if let Some(window) = crate::just_window::aggregate(details.iter().map(|part| &part["detail"]["justWindow"])) { detail["justWindow"] = window; }
    Ok(Some(Hit { team: team.clone(), power: powers/count, slot_powers, key: total/count, detail, totals: Totals::default() }))
}

pub(crate) fn search_shared(input: SharedInput<'_>) -> Result<search::Output, String> {
    let SharedInput {
        env,
        charts,
        music,
        seeds,
        k,
        deadline,
        mut initial,
        shard,
        min_combo_bonus,
        cooperative_floor,
        seeded,
        defer_warm_start,
        floor,
        mut on_candidate,
        sections,
    } = input;
    let started = (env.now)();
    let weighted_scout = charts.len() >= 48 && k == 1 && initial.is_empty() && env.window_policy.is_none()
        && env.constraints.leader.is_none() && env.constraints.bindings.is_empty()
        && env.constraints.required_members.is_empty();
    let mut early = None;
    if !seeded && !defer_warm_start && !weighted_scout && sections.is_none() {
        let constraints = &env.constraints;
        let locked = constraints.leader.filter(|&leader| leader >= 0).is_some_and(|leader| {
            let members: BTreeSet<_> = constraints.required_members.iter().copied()
                .chain(constraints.bindings.keys().copied()).chain(std::iter::once(leader as usize)).collect();
            members.len() == 5 && members.iter().all(|member| constraints.bindings.contains_key(member))
        }) && shard.is_none() && k == 1;
        let mut sole_formation = false;
        if initial.is_empty() && k == 1 && charts.len() >= 8 && env.window_policy.is_some()
            && !locked
            && seeds.is_empty() && charts.iter().all(|chart| chart.accuracy.great == 0.0
                && chart.accuracy.miss_every == 0 && chart.setup.missions.iter().all(|&mission| mission == 3)) {
            let scout = search_scored(Input {
                env: Environment { master: env.master, player: env.player, members: env.members,
                    snaps: env.snaps, music: Some(&music[0]), constraints: env.constraints.clone(),
                    now: env.now, generation: env.generation, window_policy: env.window_policy },
                chart: &mut charts[0], seeds: Vec::new(), criterion: Criterion::Mean, k: 1,
                deadline: deadline.min(started + 800.0), initial: Vec::new(), seed_only: false,
                seed_variant: None, defer_scores: false,
                cooperative_floor: false, min_combo_bonus, seeded: false, shard: None,
                floor: f64::NEG_INFINITY, on_candidate: None, progress: None,
            }, &[])?;
            if let Some(hit) = scout.hits.first() { initial.push(hit.team.clone()); }
        }
        if initial.is_empty() {
            let mut objective = PowerSeed::new(combo_bonuses(env.master, env.members, &charts[0].setup.missions));
            let mut trial = env.constraints.clone();
            if !locked && charts.len()>8 && charts.iter().all(|chart|!chart.luck&&chart.setup.missions.iter().all(|&mission|mission==3))
                && trial.required_snaps.is_empty() && trial.bindings.values().all(|&snap|snap<0) {
                let eligible: Vec<_> = env.members.iter().map(|member| gekiso::seed_actor(env.master,&charts[0],&Performer::from_member(env.master,member,None))).collect();
                if trial.required_members.iter().chain(trial.bindings.keys()).all(|&index|eligible[index])
                    && trial.leader.is_none_or(|index|index<0||eligible[index as usize]) {
                    trial.no_snaps=true;
                    trial.excluded_members.extend(eligible.iter().enumerate().filter_map(|(index,&yes)|(!yes).then_some(index)));
                }
            }
            let warm = crate::progress::with_callback(None, || env.run(&mut objective, Run { constraints: trial, k: 1,
                deadline: deadline.min(started + 40.0), seeds: Vec::new(), shard: None, floor: f64::NEG_INFINITY }, None, None))?;
            if let Some(hit) = warm.hits.first() {
                sole_formation = locked && warm.proven;
                initial.push(hit.team.clone());
            }
        }
        if let Some(team) = initial.first() {
            early = initial_hit(&env, charts, music, &seeds, team, min_combo_bonus, deadline)?;
            if let (Some(hit), Some(callback)) = (&early, on_candidate.as_deref_mut()) { callback(std::slice::from_ref(hit)); }
            if sole_formation && early.is_some() && (env.now)() < deadline && env.generation == crate::cancellation_generation() {
                return Ok(search::Output { hits: early.into_iter().collect(), proven: true, bound: None,
                    stats: search::Stats { elapsed_ms: (env.now)() - started, ..Default::default() } });
            }
        }
    }
    let mut hits = Vec::new();
    if let Some(hit) = early { offer(&mut hits, env.members, hit); }
    if let Some(plan) = sections {
        for hit in plan.incumbents.borrow().iter().chain(plan.reference_hit.iter()) {
            offer(&mut hits, env.members, hit.clone());
        }
    }
    if (env.now)() >= deadline {
        return Ok(search::Output { hits: ranked(&hits, k), proven: false, bound: None, stats: search::Stats { elapsed_ms: (env.now)()-started, ..Default::default() } });
    }
    let section_charts = sections.map(|_| charts.iter().map(|chart| chart.converted_judgements(&chart.judgements).unwrap()).collect::<Vec<_>>());
    let shared_lottery_bound = sections.is_none() && charts.len() >= 16;
    let mut windows = Vec::with_capacity(charts.len());
    for chart in charts.iter() {
        let window = env.window_policy.map(|minimum| crate::just_window::Prepared::new(env.master, chart, env.members, env.snaps, minimum, &seeds)).transpose()?;
        if window.as_ref().is_some_and(|window| window.impossible_with(&env.constraints)) {
            return Ok(search::Output { hits: Vec::new(), proven: true, bound: None, stats: search::Stats::default() });
        }
        windows.push(window);
    }
    let mut preparation = KernelPreparation::new(env.master, env.members, env.snaps);
    let mut kernels: Vec<_> = charts
        .iter_mut()
        .zip(windows)
        .map(|(chart, window)| {
            let mut kernel = Kernel::with_preparation(
                env.master,
                env.members,
                env.snaps,
                chart,
                seeds.clone(),
                Criterion::Mean,
                &mut preparation,
            );
            kernel.just_window = window;
            kernel
        })
        .collect();
    let same_levels = kernels
        .iter()
        .all(|kernel| kernel.bonuses == kernels[0].bonuses);
    let maximum = kernels[0].maximum_level;
    let mut levels = if same_levels {
        reachable_levels(&env, &kernels[0].bonuses, 0, maximum)
    } else {
        vec![0]
    };
    let level_shard = same_levels && levels.len() > 1 && shard.is_some_and(|(_, count)| count > 1);
    let owned_levels: Option<std::collections::BTreeSet<_>> = level_shard.then(|| {
        let (index, count) = shard.unwrap();
        levels
            .iter()
            .enumerate()
            .filter(|(position, _)| position % count == index)
            .map(|(_, &level)| level)
            .collect()
    });
    let supplied = initial.len();
    initial.retain(|team| search::valid_seed(env.members, env.snaps, &env.constraints, team));
    if seeded
        && (initial.len() != supplied
            || initial.iter().any(|team| {
                let bonus = if same_levels {
                    team.members
                        .iter()
                        .map(|&member| kernels[0].bonuses[member] as f64)
                        .sum()
                } else {
                    0.0
                };
                env.constraints
                    .min_bonus
                    .is_some_and(|minimum| bonus < minimum)
                    || env
                        .constraints
                        .max_bonus
                        .is_some_and(|maximum| bonus > maximum)
                    || kernels
                        .iter()
                        .any(|kernel| !kernel.meets_combo(&team.members, min_combo_bonus.max(1)))
            }))
    {
        return Err("shared-seed-invalid".into());
    }
    if same_levels {
        if let Some(team) = initial.first() {
            let preferred = team
                .members
                .iter()
                .map(|&m| kernels[0].bonuses[m])
                .sum::<i32>();
            levels.sort_by_key(|&level| (level != preferred, std::cmp::Reverse(level)));
        }
    }
    if let Some(callback)=on_candidate.as_deref_mut().filter(|_|!hits.is_empty()) {callback(&ranked(&hits,k));}
    let mut stats = search::Stats::default();
    if !seeded && !defer_warm_start && !weighted_scout
        && (initial.is_empty() || levels.len() > 1 && kernels.iter().any(|kernel| kernel.chart.luck))
    {
        let mut objective = PowerSeed::new(kernels[0].bonuses.clone());
        let warm = crate::progress::with_callback(None, || env.run(
            &mut objective,
            Run {
                constraints: env.constraints.clone(),
                k: 1,
                deadline,
                seeds: Vec::new(),
                shard: None,
                floor: f64::NEG_INFINITY,
            },
            None,
            None,
        ))?;
        stats.add(&warm.stats);
        if let Some(hit) = warm.hits.first() {
            initial.push(hit.team.clone());
            let preferred: i32 = hit
                .team
                .members
                .iter()
                .map(|&m| kernels[0].bonuses[m])
                .sum();
            levels.sort_by_key(|&level| (level != preferred, std::cmp::Reverse(level)));
        }
    }
    if let Some(owned) = owned_levels {
        levels.retain(|level| owned.contains(level));
    }
    let mut proven = true;
    let mut bound: Option<f64> = None;
    for level in levels {
        if (env.now)() >= deadline || env.generation != crate::cancellation_generation() {
            proven = false;
            break;
        }
        let prepare = |kernel, now: &dyn Fn() -> f64| {
            prepare_shared_adapter(kernel, level, same_levels, shared_lottery_bound, deadline, now)
        };
        #[cfg(not(target_arch = "wasm32"))]
        let parallel = kernels.len() >= 16 && crate::parallel::workers(kernels.len()) > 1;
        #[cfg(target_arch = "wasm32")]
        let parallel = false;
        let adapters: Option<Vec<_>> = if parallel {
            #[cfg(not(target_arch = "wasm32"))]
            {
                let started = (env.now)();
                let instant = std::time::Instant::now();
                let now = || started + instant.elapsed().as_secs_f64() * 1000.0;
                let tasks: Vec<_> = kernels.iter_mut().map(|kernel| std::sync::Mutex::new(Some(kernel))).collect();
                crate::parallel::map(&tasks, |task| {
                    let kernel = task.lock().unwrap().take().unwrap();
                    Ok::<_, String>(crate::parallel::serial(|| prepare(kernel, &now)))
                })?.into_iter().collect::<Option<Vec<_>>>()
            }
            #[cfg(target_arch = "wasm32")]
            unreachable!()
        } else {
            kernels.iter_mut().map(|kernel| prepare(kernel, env.now)).collect()
        };
        let Some(adapters) = adapters.filter(|_| (env.now)() < deadline) else {
            proven=false;
            break;
        };
        let current = ranked(&hits, k);
        let mut objective = Combined::new(
            adapters,
            env.player,
            music,
            same_levels,
            min_combo_bonus.max(1),
            !seeded && !defer_warm_start && current.len() < k,
            weighted_scout && deadline == f64::INFINITY && !env.constraints.no_snaps && env.snaps.len() >= 10,
            cooperative_floor,
        );
        objective.k = k;
        objective.floor = if sections.is_some() { f64::NEG_INFINITY } else { current.get(k - 1).map_or(floor, |hit| hit.key.max(floor)) };
        if seeded {
            objective.completed = initial.clone();
        } else if sections.is_none() && k == 1 {
            // The retained incumbent has already replayed every requested chart
            // and skill order with this physical formation.
            objective.completed.extend(current.iter()
                .filter(|hit| hit.key.is_finite())
                .map(|hit| hit.team.clone()));
        }
        let constraints = if same_levels {
            level_constraints(&env.constraints, level, maximum)
        } else {
            env.constraints.clone()
        };
        let seeds = if seeded { Vec::new() } else { initial.iter().filter(|team| !current.iter()
            .any(|hit| hit.team.members == team.members && hit.team.snaps == team.snaps)).cloned().collect() };
        let mut accept = |candidates: &[Hit]| {
            for hit in candidates.iter().filter(|hit| hit.key.is_finite()) {
                offer(&mut hits, env.members, hit.clone());
            }
            if let Some(callback) = on_candidate.as_deref_mut() {
                callback(&ranked(&hits, k));
            }
        };
        let run = Run {
                constraints,
                k,
                deadline,
                seeds,
                shard: if level_shard { None } else { shard },
                floor: if current.len() >= k {
                    current[k - 1].key.max(floor)
                } else {
                    floor
                },
            };
        let out = if let Some(plan) = sections {
            let floor_rank=current.get(k-1).map(|hit|[hit.key,hit.detail["sectionTie"][0].as_f64().unwrap_or(0.0),hit.detail["sectionTie"][1].as_f64().unwrap_or(0.0)]);
            let mut target = sections::Wrapper::new(&mut objective, &env, section_charts.as_ref().unwrap(), music, plan, floor_rank);
            env.run(&mut target, run, None, Some(&mut accept))?
        } else { env.run(&mut objective, run, None, Some(&mut accept))? };
        stats.add(&out.stats);
        if !out.proven {
            proven = false;
            bound = Some(
                bound
                    .unwrap_or(f64::NEG_INFINITY)
                    .max(out.bound.unwrap_or(f64::INFINITY)),
            );
        }
        for hit in out.hits.into_iter().filter(|hit| hit.key.is_finite()) {
            offer(&mut hits, env.members, hit);
        }
        if let Some(callback) = on_candidate.as_deref_mut() {
            callback(&ranked(&hits, k));
        }
    }
    stats.elapsed_ms = (env.now)() - started;
    if !proven {
        bound = Some(f64::INFINITY);
    }
    Ok(search::Output {
        hits: ranked(&hits, k),
        proven,
        bound,
        stats,
    })
}

fn prepare_shared_adapter<'k, 'a>(
    kernel: &'k mut Kernel<'a>,
    level: i32,
    same_levels: bool,
    lottery_bound: bool,
    deadline: f64,
    now: &dyn Fn() -> f64,
) -> Option<Adapter<'k, 'a>> {
    if now() >= deadline { return None; }
    let level = if same_levels { level } else { kernel.maximum_level };
    Adapter::with_lottery_bound(kernel, level, false, false, lottery_bound, deadline, now)
}

impl Kernel<'_> {
    fn just_assignment_rows(&mut self, members: &[usize], options: &[Vec<i32>], power: &[f64])
        -> Option<(f64, Vec<f64>, f64)> {
        if !self.just_grades_fixed || !self.owner_local || self.just_window.is_some() || !self.seeds.is_empty()
        { return None; }
        let mut grades = [false; 7];
        for &grade in &self.chart.judgements { *grades.get_mut(grade)? = true; }
        for (&member, choices) in members.iter().zip(options) {
            for &snap in choices {
                if self.slot(member, snap).convert.as_ref().is_some_and(|conversion| {
                    conversion.judgements.iter().any(|&grade| grades.get(grade).copied() == Some(true))
                }) { return None; }
                // JUST converters cannot change any of this chart's raw grades.
                // Keep the normalized actor for its remaining score program.
                self.bound_actor_of(member, snap);
            }
        }
        let slots = &self.slots;
        let actors = &self.bound_actors;
        let width = self.snaps.len() + 1;
        let rows = std::array::from_fn(|slot| options[slot].iter().map(|&snap| {
            let start = members[slot] * width;
            let column = (snap + 1) as usize;
            let index = start + column;
            let physical = if snap < 0 { width - 1 } else { snap as usize };
            (column, crate::just_bound::Choice {
                actor: actors[index].as_ref().unwrap(), slot: &slots[index], index, power: power[start + physical],
            })
        }).collect());
        self.just_bounds.assignment_mean_rows(self.master, self.chart, &rows, width).ok().flatten()
    }

    pub(super) fn fixed_luck_chart(&self) -> bool {
        self.healthy && self.owner_local && self.chart.luck && self.seeds.is_empty()
            && self.chart.setup.missions.iter().all(|&mission| mission == 2)
    }

    fn plain_luck_support(&self, member: usize, snap: i32) -> bool {
        let Some(actor) = self.support_ref(member, snap) else { return true; };
        let mut count = 0;
        actor.gekisou_support_skills.iter().all(|&(id, level)| {
            let skill = &self.master.gekiso_support_skills[&id];
            if !matches!(skill.mission_type, 2 | 4) { return true; }
            skill.effects.iter().filter(|row| row.level == level).all(|row| {
                count += 1;
                count <= 8 && nominal_rush_row(self.master, row)
            })
        })
    }

    fn luck_support_program(&self, member: usize, snap: i32) -> (i32, &[(i32, i32)]) {
        let actor = self.support_ref(member, snap).unwrap_or(&self.gk_members[member]);
        (actor.band_id, &actor.gekisou_support_skills)
    }

    fn luck_reference(&self, member: usize, options: &[i32]) -> Option<i32> {
        let &first = options.first()?;
        if options.iter().all(|&snap| self.plain_luck_support(member, snap)) {
            return Some(-1);
        }
        let actor = &self.gk_members[member];
        let active = actor.mission_only.is_some_and(|mission| mission != 0)
            || actor.gekisou_skill.is_some_and(|(id, _)| matches!(self.master.gekiso_skills[&id].mission_type, 2 | 4));
        let program = self.luck_support_program(member, first);
        (active && options.iter().all(|&snap| self.luck_support_program(member, snap) == program)).then_some(first)
    }

    fn luck_assignment_rows(&mut self, members: &[usize], options: &[Vec<i32>], cap: f64) -> Option<(f64, Vec<f64>)> {
        if !self.fixed_luck_chart() { return None; }
        let mut choices: [Vec<_>; 5] = std::array::from_fn(|slot| Vec::with_capacity(options[slot].len()));
        let mut reference = [-1; 5];
        for (index, (&member, options)) in members.iter().zip(options).enumerate() {
            reference[index] = self.luck_reference(member, options)?;
        }
        let roster: Vec<_> = members.iter().zip(reference).map(|(&member, snap)| self.actor_of(member, snap)).collect();
        if !self.lottery_order_is_stable(&roster, &reference) { return None; }
        for (index, &member) in members.iter().enumerate() {
            for &snap in &options[index] {
                let equipped = self.actor_of(member, snap);
                if !self.member_conditions_match(&roster[index], &equipped) { return None; }
                // The score reader observes the normalized skill and owner.
                let rush = if reference[index] >= 0 || equipped.gekisou_support_skills.is_empty() {
                    0.0
                } else {
                    let (mut normalized, _) = self.deck_key(&[equipped], 0);
                    normalized.pop().map_or(Some(0.0), |mut actor| {
                        actor.gekisou_support_skills.retain(|&(id, _)| matches!(self.master.gekiso_support_skills[&id].mission_type, 2 | 4));
                        self.rush_factor(&actor)
                    })?
                };
                choices[index].push(((snap + 1) as usize, snap, rush));
            }
        }
        let context = self.context_of(&roster, None, 0);
        let slots = std::array::from_fn(|index| choices[index].iter().map(|&(column, snap, rush)| {
            let member = members[index];
            let role = self.ordinary_roles[self.slot_index(member, snap)];
            (column, role, self.slot(member, snap), rush)
        }).collect());
        let width = self.snaps.len() + 1;
        let (base, rows) = gekiso::nominal_assignment_rows(self.master, self.chart, &context, width, slots)?;
        let coefficient = (0..5).fold(base, |sum, index| {
            let max = rows[index * width..(index + 1) * width].iter().copied().fold(0.0, f64::max);
            (sum + max).next_up()
        });
        ((coefficient * cap).next_up() < i32::MAX as f64).then_some((base, rows))
    }

    fn nominal_mean_upper(&mut self, team: &Team, power: i32) -> Result<Option<f64>, String> {
        if !self.chart.luck || !self.seeds.is_empty() {
            return Ok(None);
        }
        if !self.owner_local || !self.healthy {
            return self.nominal_formation_upper(team, power, None);
        }
        if self.nominal_mean_members.as_slice() != team.members {
            self.nominal_mean_members.copy_from_slice(&team.members);
            self.nominal_mean_cache.clear();
            self.nominal_mean_coefficients.clear();
        }
        let shapes = std::array::from_fn(|slot| {
            self.nominal_roster_shape_id(team.members[slot], team.snaps[slot], &team.members)
        });
        if let Some(upper) = self.nominal_mean_coefficients.get(&shapes)
            .and_then(|&coefficient| self.nominal_coefficient_upper(power, coefficient))
        {
            return Ok(Some(upper));
        }
        let key = (shapes, power);
        if let Some(&score) = self.nominal_mean_cache.get(&key) {
            return Ok(score);
        }
        let score = self.nominal_formation_upper(team, power, Some(shapes))?;
        if self.nominal_mean_cache.len() >= 256 { self.nominal_mean_cache.clear(); }
        self.nominal_mean_cache.insert(key, score);
        Ok(score)
    }

    fn nominal_coefficient_upper(&self, power: i32, coefficient: f64) -> Option<f64> {
        let upper = ((power as f64 * coefficient).next_up() * (1.0 + RELATIVE)
            + (self.chart.notes.len() + 4) as f64).next_up();
        (power > 0 && upper < i32::MAX as f64).then_some(upper)
    }

    fn nominal_formation_upper(&mut self, team: &Team, power: i32, shapes: Option<[u32; 5]>) -> Result<Option<f64>, String> {
        let deck = self.deck_of(team);
        let context = self.context_of(&deck, None, 0);
        let slots: Vec<_> = team.members.iter().zip(&team.snaps)
            .map(|(&member, &snap)| self.slot(member, snap).clone()).collect();
        if self.healthy && self.steady_life && power > 0
            && self.chart.range_percent.iter().all(|&percent| percent >= 0)
            && context.judgement.iter().all(|&grade| grade == 5 || grade == 6)
            && context.gk.iter().chain(&context.luck).chain(&context.up)
                .chain(&self.chart.note_frac).chain(&self.chart.judge_frac)
                .chain([&self.master.live.adjustment, &self.chart.difficulty_factor])
                .all(|&value| value.is_finite() && value >= 0.0)
            && context.nominal.as_ref().is_some_and(|nominal| {
                (0..self.chart.notes.len()).all(|index| {
                    let mass = nominal.probabilities.iter().map(|row| row[index]).sum::<f64>();
                    (1.0 - 1e-10..=1.0 + 1e-10).contains(&mass)
                })
            })
            && slots.iter().all(|slot| slot.convert.is_none() && slot.cumulative.is_empty()
                && slot.effects.iter().all(|effect| effect.delta >= 0))
        {
            let weights = note_weights(self.master, self.chart, &context);
            let slots: Vec<_> = slots.iter().cloned().map(Arc::new).collect();
            if let Some(coefficient) = crate::rewards::mean_score_coefficient(self.master, self.chart, &weights, &slots) {
                if let Some(shapes) = shapes {
                    if self.nominal_mean_coefficients.len() >= 1024 { self.nominal_mean_coefficients.clear(); }
                    self.nominal_mean_coefficients.insert(shapes, coefficient);
                }
                if let Some(upper) = self.nominal_coefficient_upper(power, coefficient) { return Ok(Some(upper)); }
            }
        }
        gekiso::nominal_mean_upper(self.master, self.chart, &context, power, &slots)
    }

    fn meets_combo(&self, members: &[usize], minimum: i32) -> bool {
        !self
            .chart
            .setup
            .missions
            .iter()
            .any(|mission| *mission == 1 || *mission == 4)
            || 1 + members
                .iter()
                .map(|&member| self.opening_bonuses[member])
                .sum::<i32>()
                >= minimum
    }
    fn shared_mean_upper(
        &mut self,
        team: &Team,
        power: f64,
        counters: &crate::support_aliases::JustCounters,
    ) -> Option<f64> {
        if !self.steady_life || !self.owner_local || self.chart.luck || !self.seeds.is_empty() { return None; }
        if !self.healthy && (!self.chart.setup.missions.iter().all(|&mission| mission == 1)
            || team.members.iter().zip(&team.snaps).any(|(&member, &snap)| self.slot(member, snap).convert.is_some())
            || self.has_converter(team)) { return None; }
        let key = std::array::from_fn(|slot| self.shape_id(team.members[slot], team.snaps[slot]));
        if let Some(&coefficient) = self.shared_mean_coefficients.get(&key) {
            return Some(((power * coefficient).next_up() * (1.0 + 3e-12) + (self.chart.notes.len() + 4) as f64).next_up());
        }
        let mut deck = self.deck_of(team);
        for actor in &mut deck {
            counters.normalize(actor);
        }
        let weights = self.weights_of(&deck, None, 0);
        let slots: Vec<_> = team
            .members
            .iter()
            .enumerate()
            .map(|(i, &m)| Arc::new(self.slot(m, team.snaps[i]).clone()))
            .collect();
        let mean = crate::rewards::mean_score_coefficient(self.master, self.chart, &weights, &slots)?;
        if self.shared_mean_coefficients.len() >= 4096 { self.shared_mean_coefficients.clear(); }
        self.shared_mean_coefficients.insert(key, mean);
        Some(
            ((power * mean).next_up() * (1.0 + 3e-12) + (self.chart.notes.len() + 4) as f64)
                .next_up(),
        )
    }
}
