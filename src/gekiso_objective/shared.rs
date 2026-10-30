//! A single physical formation optimized over a complete set of Gekiso charts.
use super::*;

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
    pub floor: f64,
    pub on_candidate: Option<CandidateListener<'a>>,
}

struct SongObjective<'k, 'a> {
    adapter: Adapter<'k, 'a>,
    music: &'a Music,
    bases: Vec<power::SlotBase>,
    snap_power: Arc<Vec<f64>>,
    weights: Vec<f64>,
    power_cap: f64,
    base: f64,
    slots: Vec<Vec<f64>>,
    domain_power: Vec<f64>,
    power_choices: Vec<Vec<(i32, f64)>>,
    skill_choices: Vec<Vec<(i32, f64)>>,
}

struct PowerSeed {
    bonuses: Vec<i32>,
}
impl Objective for PowerSeed {
    fn skill(&mut self, _member: usize, _snap: i32) -> (f64, f64) {
        (0.0, 0.0)
    }
    fn member_bonus(&self, member: usize) -> f64 {
        self.bonuses[member] as f64
    }
    fn linear_assignment(&self) -> Option<(f64, f64, f64)> {
        Some((1.0, 0.0, 0.0))
    }
    fn bound(&mut self, t: Totals) -> f64 {
        t.power
    }
    fn interval(&mut self, _team: &Team, t: Totals) -> (f64, f64) {
        (t.power, t.power)
    }
    fn exact(&mut self, _team: &Team, power: f64, _slots: &[f64]) -> Result<Exact, String> {
        Ok(Exact {
            key: power,
            detail: Value::Null,
        })
    }
}

struct Combined<'k, 'a> {
    songs: Vec<SongObjective<'k, 'a>>,
    player: &'a Player,
    same_levels: bool,
    min_combo: i32,
    roster: Vec<usize>,
    affine: Option<(f64, f64, Vec<f64>)>,
    warm_seed: bool,
    cooperative_floor: bool,
    completed: Vec<Team>,
    floor: f64,
    k: usize,
    last_bounds: Vec<f64>,
}

impl<'k, 'a> Combined<'k, 'a> {
    fn new(
        adapters: Vec<Adapter<'k, 'a>>,
        player: &'a Player,
        music: &'a [Music],
        same_levels: bool,
        min_combo: i32,
        warm_seed: bool,
        cooperative_floor: bool,
    ) -> Self {
        let songs: Vec<_> = adapters
            .into_iter()
            .zip(music)
            .map(|(mut adapter, music)| {
                let master = adapter.kernel.master;
                let members = adapter.kernel.members;
                let snaps = adapter.kernel.snaps;
                let width = snaps.len() + 1;
                let bases: Vec<_> = members
                    .iter()
                    .map(|m| power::slot_base(master, player, m, Some(music), 0))
                    .collect();
                let snap_power: Arc<Vec<f64>> = Arc::new(
                    members
                        .iter()
                        .enumerate()
                        .flat_map(|(i, member)| {
                            let base = &bases[i];
                            std::iter::once(None)
                                .chain(snaps.iter().map(Some))
                                .map(move |snap| {
                                    power::snap_points(master, base, member, snap, 0) as f64
                                })
                        })
                        .collect(),
                );
                // Search column n is the empty slot; the preparation above visits it first.
                let mut columns = (*snap_power).clone();
                for row in columns.chunks_mut(width) {
                    row.rotate_left(1);
                }
                let snap_power = Arc::new(columns);
                let profiles: Vec<_> = members
                    .iter()
                    .map(|m| power::leader_profile(master, m))
                    .collect();
                let mut caps: Vec<_> = members
                    .iter()
                    .enumerate()
                    .map(|(i, member)| {
                        let leader = profiles
                            .iter()
                            .map(|p| {
                                power::leader_points(
                                    &bases[i],
                                    &power::leader_percent_bound(master, p, member),
                                )
                            })
                            .max()
                            .unwrap_or(0);
                        (bases[i].fixed_points + leader) as f64
                            + snap_power[i * width..(i + 1) * width]
                                .iter()
                                .copied()
                                .fold(0.0, f64::max)
                    })
                    .collect();
                caps.sort_by(|a, b| b.total_cmp(a));
                let power_cap = caps
                    .into_iter()
                    .take(5)
                    .fold(0.0_f64, |sum, v| (sum + v).next_up());
                let weights = (0..members.len())
                    .flat_map(|m| (0..width).map(move |j| (m, j)))
                    .map(|(m, j)| {
                        adapter
                            .skill(m, if j == width - 1 { -1 } else { j as i32 })
                            .1
                    })
                    .collect();
                adapter.prepare_power(Arc::clone(&snap_power), width);
                SongObjective {
                    adapter,
                    music,
                    bases,
                    snap_power,
                    weights,
                    power_cap,
                    base: 0.0,
                    slots: Vec::new(),
                    domain_power: Vec::new(),
                    power_choices: Vec::new(),
                    skill_choices: Vec::new(),
                }
            })
            .collect();
        let equal_power = songs.iter().all(|song| {
            song.music.music_type == songs[0].music.music_type
                && song
                    .bases
                    .iter()
                    .zip(&songs[0].bases)
                    .all(|(a, b)| a.common == b.common && a.fixed_points == b.fixed_points)
        });
        let affine = equal_power
            .then(|| {
                let mut base = 0.0_f64;
                let mut absolute = 0.0_f64;
                let mut scales = Vec::new();
                for song in &songs {
                    let n = song.adapter.kernel.chart.full.converted as f64;
                    let (b, scale, residual) = if let Some(bound) = &song.adapter.deterministic {
                        (bound.base, (1.0 / n).next_up(), 0.0)
                    } else if let Some(data) = &song.adapter.data {
                        (
                            data.weights.base,
                            ((1.0 + RELATIVE) / n).next_up(),
                            (song.adapter.kernel.chart.notes.len() + 4) as f64,
                        )
                    } else {
                        return None;
                    };
                    base = (base + (b.max(0.0) * scale).next_up()).next_up();
                    absolute = (absolute + residual).next_up();
                    scales.push(scale);
                }
                let count = songs.len() as f64;
                Some((
                    (base / count).next_up(),
                    (absolute / count).next_up(),
                    scales,
                ))
            })
            .flatten();
        Self {
            songs,
            player,
            same_levels,
            min_combo,
            roster: Vec::new(),
            affine,
            warm_seed,
            cooperative_floor,
            completed: Vec::new(),
            floor: f64::NEG_INFINITY,
            k: 1,
            last_bounds: Vec::new(),
        }
    }

    fn settle(
        &mut self,
        team: &Team,
        cutoff: Option<search::Cutoff>,
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
        let powers: Vec<_> = self
            .songs
            .iter()
            .map(|song| {
                power::team_power(
                    song.adapter.kernel.master,
                    self.player,
                    &members,
                    &snaps,
                    0,
                    Some(song.music),
                    &[],
                )
            })
            .collect::<Result<_, _>>()?;
        let cutoff = cutoff.filter(|c| c.key.is_finite());
        let mut bounds = vec![f64::INFINITY; self.songs.len()];
        if cutoff.is_some() {
            let counters = crate::support_aliases::JustCounters::for_formation(
                kernel.master,
                &members,
                &snaps.iter().flatten().cloned().collect::<Vec<_>>(),
            );
            for (index, song) in self.songs.iter_mut().enumerate() {
                let width = song.adapter.kernel.snaps.len() + 1;
                let t = Totals {
                    power: powers[index].total as f64,
                    skill: team
                        .members
                        .iter()
                        .zip(&team.snaps)
                        .fold(0.0_f64, |sum, (&m, &s)| {
                            (sum + song.weights
                                [m * width + if s < 0 { width - 1 } else { s as usize }])
                            .next_up()
                        }),
                    bonus: team
                        .members
                        .iter()
                        .map(|&m| song.adapter.kernel.bonuses[m] as f64)
                        .sum(),
                    ..Totals::default()
                };
                bounds[index] = song.adapter.bound(t);
                if let Some(upper) = song
                    .adapter
                    .kernel
                    .shared_mean_upper(team, t.power, &counters)
                {
                    bounds[index] = bounds[index].min(upper);
                }
            }
        }
        let mut order: Vec<_> = (0..self.songs.len()).collect();
        order.sort_by(|&a, &b| bounds[b].total_cmp(&bounds[a]));
        let mut details = vec![Value::Null; self.songs.len()];
        let mut scores = vec![0.0; self.songs.len()];
        for index in order {
            let song = &mut self.songs[index];
            let mut score = song
                .adapter
                .kernel
                .exact(team, powers[index].total as i32, true)?;
            song.adapter.kernel.decorate_window(team, powers[index].total as i32, &mut score.detail)?;
            scores[index] = score.key;
            bounds[index] = score.key;
            details[index] = json!({"detail":score.detail,"power":powers[index].total,"slotPowers":powers[index].slots});
            if let Some(cutoff) = cutoff {
                let upper = (bounds.iter().fold(0.0_f64, |sum, v| (sum + v).next_up())
                    / bounds.len() as f64)
                    .next_up();
                let threshold = if self.cooperative_floor {
                    cutoff.key.max(crate::search_floor())
                } else {
                    cutoff.key
                };
                if upper < threshold || cutoff.prune_equal && upper == threshold {
                    return Ok(search::Evaluation::Pruned(upper));
                }
            }
        }
        let mut detail = json!({"songs":details});
        if let Some(window) = crate::just_window::aggregate(details.iter().map(|detail| &detail["detail"]["justWindow"])) { detail["justWindow"] = window; }
        Ok(search::Evaluation::Exact(Exact {
            key: scores.iter().sum::<f64>() / scores.len() as f64,
            detail,
        }))
    }

    fn eligible(&self, members: &[usize]) -> bool {
        self.songs
            .iter()
            .all(|song| song.adapter.kernel.meets_combo(members, self.min_combo))
    }

    fn totals(
        song: &SongObjective<'_, '_>,
        members: &[usize],
        assigned: &[i32],
        from: usize,
    ) -> Totals {
        let width = song.adapter.kernel.snaps.len() + 1;
        let mut total = Totals::default();
        for (slot, &member) in members.iter().enumerate() {
            let column = |snap: i32| if snap < 0 { width - 1 } else { snap as usize };
            let (power, skill) = if slot < from {
                let index = column(assigned[slot]);
                (
                    song.slots[slot][index],
                    song.weights[member * width + index],
                )
            } else {
                let available = |choices: &[(i32, f64)]| {
                    choices
                        .iter()
                        .find(|(snap, _)| *snap < 0 || !assigned[..from].contains(snap))
                        .map_or(f64::NEG_INFINITY, |(_, value)| *value)
                };
                (
                    available(&song.power_choices[slot]),
                    available(&song.skill_choices[slot]),
                )
            };
            total.power = (total.power + power).next_up();
            total.skill = (total.skill + skill).next_up();
            total.bonus += song.adapter.kernel.bonuses[member] as f64;
        }
        total
    }

    fn upper(
        &mut self,
        members: &[usize],
        assigned: &[i32],
        from: usize,
        refined: bool,
        limit: f64,
    ) -> f64 {
        if !self.eligible(members) {
            return f64::NEG_INFINITY;
        }
        let totals: Vec<_> = self
            .songs
            .iter()
            .map(|song| Self::totals(song, members, assigned, from))
            .collect();
        let mut bounds: Vec<_> = self
            .songs
            .iter_mut()
            .zip(&totals)
            .map(|(song, &t)| {
                song.adapter.bound(t).min(
                    song.adapter
                        .cheap_assignment_bound(members, t, assigned, from),
                )
            })
            .collect();
        let count = bounds.len() as f64;
        if refined {
            for (index, song) in self.songs.iter_mut().enumerate() {
                let others = bounds
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| *i != index)
                    .fold(0.0_f64, |sum, (_, v)| (sum + v).next_up());
                let target = if limit.is_finite() {
                    (limit * count - others).max(0.0)
                } else {
                    0.0
                };
                let t = totals[index];
                if let Some(upper) = song
                    .adapter
                    .assignment_order_bound(members, t, assigned, from, target)
                {
                    bounds[index] = bounds[index].min(upper);
                }
                if let Some(upper) = song.adapter.assignment_bound(members, t, assigned, from) {
                    bounds[index] = bounds[index].min(upper);
                }
                if from == 5 {
                    let team = Team {
                        members: members.to_vec(),
                        snaps: assigned.to_vec(),
                    };
                    if let Some(upper) = song.adapter.leaf_bound(&team, t) {
                        bounds[index] = bounds[index].min(upper);
                    }
                }
                if (bounds.iter().fold(0.0_f64, |sum, v| (sum + v).next_up()) / count).next_up()
                    < limit
                {
                    break;
                }
            }
        }
        let upper = (bounds.iter().fold(0.0_f64, |sum, v| (sum + v).next_up()) / count).next_up();
        self.last_bounds = bounds;
        upper
    }
}

impl Objective for Combined<'_, '_> {
    fn has_team_filter(&self) -> bool { self.songs.iter().any(|song| song.adapter.has_team_filter()) }
    fn team_filter_impossible(&self) -> bool { self.songs.iter().any(|song| song.adapter.team_filter_impossible()) }
    fn feasible_assignment(&self, members: &[usize], assigned: &[i32], from: usize, options: &[Vec<i32>]) -> bool {
        self.songs.iter().all(|song| song.adapter.feasible_assignment(members, assigned, from, options))
    }
    fn feasible_team(&mut self, team: &Team, _power: f64) -> Result<bool, String> {
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
    fn live_floor(&self) -> bool {
        self.cooperative_floor
    }
    fn canonical_leader(&self) -> bool {
        self.songs
            .iter()
            .all(|song| song.adapter.canonical_leader())
    }
    fn seed_objective(&self) -> Option<Box<dyn Objective>> {
        if !self.warm_seed {
            return None;
        }
        Some(Box::new(PowerSeed {
            bonuses: if self.same_levels {
                self.songs[0].adapter.kernel.bonuses.clone()
            } else {
                vec![0; self.songs[0].adapter.kernel.members.len()]
            },
        }))
    }
    fn seed_ready(&self, detail: &Value) -> bool {
        detail["songs"].is_array()
    }
    fn skill(&mut self, member: usize, snap: i32) -> (f64, f64) {
        if let Some((_, _, scales)) = &self.affine {
            let high = self
                .songs
                .iter()
                .zip(scales)
                .fold(0.0_f64, |sum, (song, scale)| {
                    let width = song.adapter.kernel.snaps.len() + 1;
                    (sum + (song.weights
                        [member * width + if snap < 0 { width - 1 } else { snap as usize }]
                    .max(0.0)
                        * scale)
                        .next_up())
                    .next_up()
                });
            return (0.0, (high / self.songs.len() as f64).next_up());
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
    fn exchange_snap_equivalent(&mut self, member: usize, a: i32, b: i32) -> bool {
        self.snap_dominance(member, a) == self.snap_dominance(member, b)
    }
    fn bound(&mut self, t: Totals) -> f64 {
        if let Some((base, absolute, _)) = &self.affine {
            return ((t.power * (base + t.skill).next_up()).next_up() * (1.0 + 3e-12) + absolute)
                .next_up();
        }
        let count = self.songs.len() as f64;
        (self.songs.iter_mut().fold(0.0_f64, |sum, s| {
            (sum + s.adapter.bound(Totals {
                power: s.power_cap,
                ..t
            }))
            .next_up()
        }) / count)
            .next_up()
    }
    fn prepare_members(&mut self, characters: &[Vec<usize>]) {
        for song in &mut self.songs {
            song.adapter.prepare_members(characters);
        }
    }
    fn product_base(&self) -> Option<f64> {
        self.affine.as_ref().map(|v| v.0)
    }
    fn joint_targets(&mut self, limit: f64) -> Option<Vec<search::JointTarget>> {
        let (_, absolute, _) = self.affine.as_ref()?;
        if !limit.is_finite() || limit <= *absolute {
            return None;
        }
        Some(vec![search::JointTarget {
            bonus: 0.0,
            product: ((limit.next_down() - absolute).next_down() / (1.0 + 3e-12)).next_down(),
        }])
    }
    fn prepare_member_domains_at(&mut self, leader: usize, _power: &[f64], options: &[Vec<i32>]) {
        for song in &mut self.songs {
            let k = &song.adapter.kernel;
            let profile = power::leader_profile(k.master, &k.members[leader]);
            song.domain_power = k
                .members
                .iter()
                .enumerate()
                .map(|(m, member)| {
                    (song.bases[m].fixed_points
                        + power::leader_points(
                            &song.bases[m],
                            &power::leader_percent_bound(k.master, &profile, member),
                        )) as f64
                })
                .collect();
            song.adapter
                .prepare_member_domains_at(leader, &song.domain_power, options);
        }
    }
    fn member_bound_at(
        &mut self,
        members: &[usize],
        t: Totals,
        left: usize,
        from: usize,
        _limit: f64,
    ) -> Option<f64> {
        if self.affine.is_some() {
            return Some(self.bound(t));
        }
        let mut upper = 0.0_f64;
        for song in &mut self.songs {
            let t = Totals {
                power: song.power_cap,
                ..t
            };
            let coarse = song.adapter.bound(t);
            upper = (upper
                + song
                    .adapter
                    .member_bound_at(members, t, left, from, 0.0)
                    .unwrap_or(coarse)
                    .min(coarse))
            .next_up();
        }
        Some((upper / self.songs.len() as f64).next_up())
    }
    fn prepare_assignment(&mut self, members: &[usize], options: &[Vec<i32>]) {
        self.roster = members.to_vec();
        for song in &mut self.songs {
            let kernel = &song.adapter.kernel;
            let master = kernel.master;
            let roster: Vec<_> = members.iter().map(|&m| kernel.members[m].clone()).collect();
            let profile = power::leader_profile(master, &roster[0]);
            let percents = power::leader_percents(master, &profile, &roster, 0, Some(song.music));
            let width = kernel.snaps.len() + 1;
            let base: Vec<_> = members
                .iter()
                .enumerate()
                .map(|(i, &m)| {
                    (song.bases[m].fixed_points
                        + power::leader_points(&song.bases[m], &percents[i]))
                        as f64
                })
                .collect();
            song.base = base.iter().sum();
            song.slots = members
                .iter()
                .enumerate()
                .map(|(slot, &m)| {
                    (0..width)
                        .map(|j| base[slot] + song.snap_power[m * width + j])
                        .collect()
                })
                .collect();
            let best_five = |slot: usize, values: &[f64]| {
                let mut choices: Vec<_> = options[slot]
                    .iter()
                    .map(|&snap| {
                        (
                            snap,
                            values[if snap < 0 { width - 1 } else { snap as usize }],
                        )
                    })
                    .collect();
                choices.sort_unstable_by(|a, b| b.1.total_cmp(&a.1));
                // At most four distinct snapshots are occupied before an unassigned slot.
                choices.truncate(5);
                choices
            };
            song.power_choices = (0..5)
                .map(|slot| best_five(slot, &song.slots[slot]))
                .collect();
            song.skill_choices = members
                .iter()
                .enumerate()
                .map(|(slot, &member)| {
                    best_five(slot, &song.weights[member * width..(member + 1) * width])
                })
                .collect();
            song.adapter
                .prepare_power(Arc::clone(&song.snap_power), width);
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
        if self.affine.is_some() {
            self.bound(t)
        } else {
            self.upper(members, assigned, from, false, f64::INFINITY)
        }
    }
    fn assignment_order_bound(
        &mut self,
        members: &[usize],
        t: Totals,
        assigned: &[i32],
        from: usize,
        limit: f64,
    ) -> Option<f64> {
        let coarse = self.bound(t);
        Some(if coarse < limit {
            coarse
        } else {
            coarse.min(self.upper(members, assigned, from, true, limit))
        })
    }
    fn interval(&mut self, team: &Team, _t: Totals) -> (f64, f64) {
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
        let coarse = self.upper(&team.members, &team.snaps, 5, true, floor);
        if coarse <= floor || self.songs.iter().any(|song| song.adapter.kernel.chart.luck) {
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
        let mut bounds = std::mem::take(&mut self.last_bounds);
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
                return (0.0, upper);
            };
            bounds[index] = bounds[index].min(value);
            upper = (bounds
                .iter()
                .fold(0.0_f64, |sum, value| (sum + value).next_up())
                / bounds.len() as f64)
                .next_up();
            if upper <= floor {
                break;
            }
        }
        (0.0, upper)
    }
    fn exact(&mut self, team: &Team, _power: f64, _slots: &[f64]) -> Result<Exact, String> {
        match self.settle(team, None)? {
            search::Evaluation::Exact(value) => Ok(value),
            search::Evaluation::Pruned(_) => unreachable!(),
        }
    }
    fn evaluate(
        &mut self,
        team: &Team,
        _power: f64,
        _slots: &[f64],
        cutoff: search::Cutoff,
    ) -> Result<search::Evaluation, String> {
        self.settle(team, Some(cutoff))
    }
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
        floor,
        mut on_candidate,
    } = input;
    let started = (env.now)();
    let mut windows = Vec::with_capacity(charts.len());
    for chart in charts.iter() {
        let window = env.min_just_window_ms.map(|minimum| crate::just_window::Prepared::new(env.master, chart, env.members, env.snaps, minimum, &seeds)).transpose()?;
        if window.as_ref().is_some_and(crate::just_window::Prepared::impossible) {
            return Ok(search::Output { hits: Vec::new(), proven: true, bound: None, stats: search::Stats::default() });
        }
        windows.push(window);
    }
    let mut kernels: Vec<_> = charts
        .iter_mut()
        .zip(windows)
        .map(|(chart, window)| {
            let mut kernel = Kernel::new(
                env.master,
                env.members,
                env.snaps,
                chart,
                seeds.clone(),
                Criterion::Mean,
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
    let mut hits = Vec::new();
    let mut stats = search::Stats::default();
    if !seeded && (initial.is_empty() || levels.len() > 1) {
        let mut objective = PowerSeed {
            bonuses: kernels[0].bonuses.clone(),
        };
        let warm = env.run(
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
        )?;
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
        let mut adapters = Vec::new();
        for kernel in &mut kernels {
            let own_level = if same_levels {
                level
            } else {
                kernel.maximum_level
            };
            let Some(adapter) = Adapter::new(kernel, own_level, false, false, deadline, env.now)
            else {
                proven = false;
                break;
            };
            adapters.push(adapter);
        }
        if !proven {
            break;
        }
        let current = ranked(&hits, k);
        let mut objective = Combined::new(
            adapters,
            env.player,
            music,
            same_levels,
            min_combo_bonus.max(1),
            !seeded && current.len() < k,
            cooperative_floor,
        );
        objective.k = k;
        objective.floor = current.get(k - 1).map_or(floor, |hit| hit.key.max(floor));
        if seeded {
            objective.completed = initial.clone();
        }
        let constraints = if same_levels {
            level_constraints(&env.constraints, level, maximum)
        } else {
            env.constraints.clone()
        };
        let mut seeds = if seeded { Vec::new() } else { initial.clone() };
        seeds.extend(current.iter().map(|hit| hit.team.clone()));
        let mut accept = |candidates: &[Hit]| {
            for hit in candidates.iter().filter(|hit| hit.key.is_finite()) {
                offer(&mut hits, env.members, hit.clone());
            }
            if let Some(callback) = on_candidate.as_deref_mut() {
                callback(&ranked(&hits, k));
            }
        };
        let out = env.run(
            &mut objective,
            Run {
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
            },
            None,
            Some(&mut accept),
        )?;
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

impl Kernel<'_> {
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
        if !self.healthy || !self.owner_local || self.chart.luck || !self.seeds.is_empty() {
            return None;
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
        let coefficients =
            crate::rewards::order_score_coefficients(self.master, self.chart, &weights, &slots)?;
        let mean = (coefficients
            .into_iter()
            .fold(0.0_f64, |sum, value| (sum + value).next_up())
            / 120.0)
            .next_up();
        Some(
            ((power * mean).next_up() * (1.0 + 3e-12) + (self.chart.notes.len() + 4) as f64)
                .next_up(),
        )
    }
}
