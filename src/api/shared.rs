//! Equal-weight objectives retain one formation across every selected chart.
use super::*;
use crate::{
    event::{EventObjective, Route, RouteKind},
    objectives::{Criterion, LiveCore, LiveObjective},
    reward_plan::{Context, Fraction, Metric, Plan, Ranked},
    search::{self, Exact, Objective, Team, Totals},
};

struct SongObjective<'a> {
    objective: Box<dyn Objective + 'a>,
    music: power::Music,
    bases: Vec<power::SlotBase>,
    snap_power: Arc<Vec<f64>>,
    skills: Vec<(f64, f64)>,
    cap: f64,
    linear: Option<(f64, f64)>,
    unbounded: bool,
    slots: Vec<Vec<f64>>,
    opening: Vec<i32>,
    member_base: Vec<f64>,
    domain_power: Vec<f64>,
}
struct Combined<'a> {
    master: &'a Master,
    player: &'a power::Player,
    members: &'a [power::Member],
    snaps: &'a [power::Snap],
    effects: &'a [master::EventEffect],
    songs: Vec<SongObjective<'a>>,
    options: Vec<Vec<i32>>,
    min_combo: i32,
    event: bool,
    plan: Option<Plan>,
    affine: Option<f64>,
    leader: usize,
    domains: Vec<Vec<i32>>,
    joint_members: Vec<f64>,
    joint_suffix: Vec<[f64; 6]>,
    joint_slots: Vec<Vec<f64>>,
    joint_matching: crate::assignment::Scratch,
    joint_zero: bool,
    priority: Vec<f64>,
    floor: f64,
    k: usize,
}
impl Combined<'_> {
    fn joint_linear(&self) -> bool {
        !self.event
            && self.songs.iter().all(|song| {
                song.linear.is_some_and(|(base, scale)| {
                    base.is_finite() && base >= 0.0 && scale.is_finite() && scale >= 0.0
                })
            })
    }
    fn joint_weight(&self, member: usize, snap: i32, caps: &[f64], slot: Option<usize>) -> f64 {
        let width = self.snaps.len() + 1;
        let col = if snap < 0 { width - 1 } else { snap as usize };
        let mut total = 0.0_f64;
        for (song, &cap) in self.songs.iter().zip(caps) {
            let (base, scale) = song.linear.unwrap();
            let power = slot.map_or_else(
                || song.member_base[member] + song.snap_power[member * width + col],
                |slot| song.slots[slot][col],
            );
            let skill = song.skills[member * width + col].1.max(0.0);
            let high =
                ((base * power).next_up() + (cap * (scale * skill).next_up()).next_up()).next_up();
            total = (total + high).next_up();
        }
        (((total / self.songs.len() as f64).next_up()) * (1.0 + 3e-12)).next_up()
    }
    fn joint_member_bound(&self, members: &[usize], from: usize, left: usize) -> Option<f64> {
        let suffix = self.joint_suffix.get(from)?[left];
        Some(members.iter().fold(suffix, |sum, &member| {
            (sum + self.joint_members[member]).next_up()
        }))
    }
    fn joint_assignment_bound(&mut self, assigned: &[i32], from: usize) -> Option<f64> {
        if self.joint_slots.is_empty() {
            return None;
        }
        let col = |snap: i32| {
            if snap < 0 {
                self.snaps.len()
            } else {
                snap as usize
            }
        };
        let independent = self
            .joint_slots
            .iter()
            .enumerate()
            .fold(0.0_f64, |sum, (slot, row)| {
                let value = if slot < from {
                    row[col(assigned[slot])]
                } else {
                    self.options[slot]
                        .iter()
                        .copied()
                        .filter(|&snap| snap < 0 || !assigned[..from].contains(&snap))
                        .map(|snap| row[col(snap)])
                        .fold(f64::NEG_INFINITY, f64::max)
                };
                (sum + value).next_up()
            });
        if independent < self.floor || from == 5 {
            return Some(independent);
        }
        let columns = self.snaps.len() + 5 - from;
        let weights: Vec<_> = (from..5)
            .map(|slot| {
                let mut row = vec![-1e100; columns];
                for &snap in &self.options[slot] {
                    if snap < 0 {
                        row[self.snaps.len()..].fill(self.joint_slots[slot][self.snaps.len()]);
                    } else if !assigned[..from].contains(&snap) {
                        row[snap as usize] = self.joint_slots[slot][snap as usize];
                    }
                }
                row
            })
            .collect();
        let fixed = (0..from).fold(0.0_f64, |sum, slot| {
            (sum + self.joint_slots[slot][col(assigned[slot])]).next_up()
        });
        Some(
            independent
                .min((fixed + self.joint_matching.maximum_upper(&weights, columns)).next_up()),
        )
    }
    fn eligible(&self, team: &Team) -> bool {
        self.songs.iter().all(|song| {
            song.opening.is_empty()
                || 1 + team.members.iter().map(|&m| song.opening[m]).sum::<i32>() >= self.min_combo
        })
    }
    fn upper(&mut self, members: &[usize], assigned: &[i32], from: usize) -> f64 {
        if self.joint_zero {
            return 0.0;
        }
        let joint = self
            .joint_assignment_bound(assigned, from)
            .unwrap_or(f64::INFINITY);
        if joint < self.floor {
            return joint;
        }
        let width = self.snaps.len() + 1;
        let mut sum = 0.0_f64;
        for song in &mut self.songs {
            let mut t = Totals::default();
            for (slot, &m) in members.iter().enumerate() {
                let choices: Vec<_> = if slot < from {
                    vec![assigned[slot]]
                } else {
                    self.options[slot]
                        .iter()
                        .copied()
                        .filter(|&s| s < 0 || !assigned[..from].contains(&s))
                        .collect()
                };
                let col = |s: i32| if s < 0 { width - 1 } else { s as usize };
                t.power = (t.power
                    + choices
                        .iter()
                        .map(|&s| song.slots[slot][col(s)])
                        .fold(f64::NEG_INFINITY, f64::max))
                .next_up();
                t.skill = (t.skill
                    + choices
                        .iter()
                        .map(|&s| song.skills[m * width + col(s)].1)
                        .fold(f64::NEG_INFINITY, f64::max))
                .next_up();
                t.skill_low += choices
                    .iter()
                    .map(|&s| song.skills[m * width + col(s)].0)
                    .fold(f64::INFINITY, f64::min);
                t.bonus += song.objective.member_bonus(m)
                    + choices
                        .iter()
                        .map(|&s| {
                            if s < 0 {
                                0.0
                            } else {
                                song.objective.snap_bonus(s as usize)
                            }
                        })
                        .fold(0.0, f64::max);
            }
            let mut bound = song.objective.bound(t).min(
                song.objective
                    .cheap_assignment_bound(members, t, assigned, from),
            );
            if let Some(value) = song.objective.assignment_bound(members, t, assigned, from) {
                bound = bound.min(value);
            }
            sum = (sum + bound).next_up();
        }
        joint.min((sum / self.songs.len() as f64).next_up())
    }
}
impl Objective for Combined<'_> {
    fn filter_member_dominates(&self, better: usize, worse: usize) -> bool { self.songs.iter().all(|song| song.objective.filter_member_dominates(better, worse)) }
    fn filter_permutation_invariant(&self) -> bool { self.songs.iter().all(|song| song.objective.filter_permutation_invariant()) }

    fn filter_snap_dominates(&self, member: usize, better: i32, worse: i32) -> bool {
        self.songs.iter().all(|song| song.objective.filter_snap_dominates(member, better, worse))
    }
    fn has_team_filter(&self) -> bool { self.songs.iter().any(|song| song.objective.has_team_filter()) }
    fn team_filter_impossible(&self) -> bool { self.songs.iter().any(|song| song.objective.team_filter_impossible()) }
    fn feasible_assignment(&self, members: &[usize], assigned: &[i32], from: usize, options: &[Vec<i32>]) -> bool {
        self.songs.iter().all(|song| song.objective.feasible_assignment(members, assigned, from, options))
    }
    fn feasible_team(&mut self, team: &Team, _power: f64) -> Result<bool, String> {
        let members: Vec<_> = team.members.iter().map(|&index| self.members[index].clone()).collect();
        let snaps: Vec<_> = team.snaps.iter().map(|&index| (index >= 0).then(|| self.snaps[index as usize].clone())).collect();
        for song in &mut self.songs {
            let power = power::team_power(self.master, self.player, &members, &snaps, 0, Some(&song.music), &[])?.total;
            if !song.objective.feasible_team(team, power as f64)? { return Ok(false); }
        }
        Ok(true)
    }
    fn prefer_snap(&self, member: usize, snap: i32) -> f64 {
        self.priority
            .get(
                member * (self.snaps.len() + 1)
                    + if snap < 0 {
                        self.snaps.len()
                    } else {
                        snap as usize
                    },
            )
            .copied()
            .unwrap_or(0.0)
    }
    fn observe_hits(&mut self, hits: &[search::Hit]) {
        self.floor = hits
            .get(self.k - 1)
            .map_or(f64::NEG_INFINITY, |hit| hit.key);
    }
    fn seed_objective(&self) -> Option<Box<dyn Objective>> {
        Some(Box::new(super::PowerObjective))
    }
    fn seed_ready(&self, detail: &Value) -> bool {
        detail["songs"].is_array()
    }
    fn canonical_leader(&self) -> bool {
        self.songs.iter().all(|s| s.objective.canonical_leader())
    }
    fn preserve_snap_options(&self) -> bool {
        self.event || self.songs.iter().any(|song| song.unbounded)
    }
    fn conditional_snap_dominance(&self) -> bool {
        !self.event && !self.songs.iter().any(|song| song.unbounded)
    }
    fn snap_dominance(&mut self, member: usize, snap: i32) -> Vec<f64> {
        let width = self.snaps.len() + 1;
        self.songs
            .iter_mut()
            .flat_map(|song| {
                let mut values = song.objective.snap_dominance(member, snap);
                values.push(
                    song.snap_power
                        [member * width + if snap < 0 { width - 1 } else { snap as usize }],
                );
                values.push(
                    song.skills[member * width + if snap < 0 { width - 1 } else { snap as usize }]
                        .1,
                );
                values
            })
            .collect()
    }
    fn score_ties(&self) -> bool {
        self.event
    }
    fn tie_rank(&self, hit: &search::Hit) -> [f64; 2] {
        [
            master::number(&hit.detail["otherMean"], 0.0),
            master::number(&hit.detail["scores"]["mean"], 0.0),
        ]
    }
    fn compare_rank(&self, a: &search::Hit, b: &search::Hit) -> Option<std::cmp::Ordering> {
        crate::reward_plan::compare(&a.detail["rewardRanking"], &b.detail["rewardRanking"])
    }
    fn skill(&mut self, m: usize, s: i32) -> (f64, f64) {
        let col = if s < 0 { self.snaps.len() } else { s as usize };
        if self.affine.is_some() {
            let high = self.songs.iter().fold(0.0_f64, |sum, song| {
                (sum + (song.skills[m * (self.snaps.len() + 1) + col].1.max(0.0)
                    * song.linear.unwrap().1)
                    .next_up())
                .next_up()
            });
            return (0.0, (high / self.songs.len() as f64).next_up());
        }
        (
            0.0,
            self.songs
                .iter()
                .map(|song| song.skills[m * (self.snaps.len() + 1) + col].1)
                .fold(0.0, f64::max),
        )
    }
    fn member_bonus(&self, m: usize) -> f64 {
        self.songs
            .iter()
            .map(|s| s.objective.member_bonus(m))
            .fold(0.0, f64::max)
    }
    fn snap_bonus(&self, s: usize) -> f64 {
        self.songs
            .iter()
            .map(|song| song.objective.snap_bonus(s))
            .fold(0.0, f64::max)
    }
    fn product_base(&self) -> Option<f64> {
        self.affine
    }
    fn joint_targets(&mut self, limit: f64) -> Option<Vec<search::JointTarget>> {
        self.affine?;
        Some(vec![search::JointTarget {
            bonus: 0.0,
            product: (limit.next_down() / (1.0 + 3e-12)).next_down(),
        }])
    }
    fn bound(&mut self, t: Totals) -> f64 {
        if self.joint_zero {
            return 0.0;
        }
        if let Some(base) = self.affine {
            return ((t.power * (base + t.skill).next_up()).next_up() * (1.0 + 3e-12)).next_up();
        }
        let sum = self.songs.iter_mut().fold(0.0_f64, |sum, song| {
            (sum + song.objective.bound(Totals {
                power: song.cap,
                ..t
            }))
            .next_up()
        });
        (sum / self.songs.len() as f64).next_up()
    }
    fn prepare_members(&mut self, groups: &[Vec<usize>]) {
        for song in &mut self.songs {
            song.objective.prepare_members(groups);
        }
        self.joint_members.clear();
        self.joint_suffix.clear();
        self.joint_zero = false;
        if !self.joint_linear()
            || self
                .songs
                .iter()
                .any(|song| song.domain_power.len() != self.members.len())
        {
            return;
        }
        let caps: Vec<_> = self
            .songs
            .iter()
            .map(|song| {
                let mut powers: Vec<_> = groups
                    .iter()
                    .map(|group| {
                        group
                            .iter()
                            .map(|&member| song.domain_power[member])
                            .fold(f64::NEG_INFINITY, f64::max)
                    })
                    .collect();
                powers.sort_unstable_by(|a, b| b.total_cmp(a));
                powers
                    .iter()
                    .take(4)
                    .fold(song.domain_power[self.leader], |sum, &power| {
                        (sum + power).next_up()
                    })
            })
            .collect();
        if caps
            .iter()
            .any(|cap| !(0.0..=i32::MAX as f64).contains(cap))
        {
            return;
        }
        let width = self.snaps.len() + 1;
        let domains = &self.domains;
        self.joint_zero = self.songs.iter_mut().zip(&caps).all(|(song, &power)| {
            domains.iter().enumerate().all(|(member, options)| {
                options.iter().all(|&snap| {
                    song.skills[member * width + if snap < 0 { width - 1 } else { snap as usize }].1
                        <= 0.0
                })
            }) && song.objective.bound(Totals {
                power,
                ..Totals::default()
            }) == 0.0
        });
        if self.joint_zero {
            return;
        }
        self.joint_members = (0..self.members.len())
            .map(|member| {
                self.domains[member]
                    .iter()
                    .map(|&snap| self.joint_weight(member, snap, &caps, None))
                    .fold(f64::NEG_INFINITY, f64::max)
            })
            .collect();
        self.joint_suffix = vec![[f64::NEG_INFINITY; 6]; groups.len() + 1];
        self.joint_suffix[groups.len()][0] = 0.0;
        for index in (0..groups.len()).rev() {
            self.joint_suffix[index] = self.joint_suffix[index + 1];
            let value = groups[index]
                .iter()
                .map(|&member| self.joint_members[member])
                .fold(f64::NEG_INFINITY, f64::max);
            for count in 1..=5 {
                let previous = self.joint_suffix[index + 1][count - 1];
                if previous.is_finite() {
                    self.joint_suffix[index][count] =
                        self.joint_suffix[index][count].max((previous + value).next_up());
                }
            }
        }
    }
    fn prepare_member_domains_at(&mut self, leader: usize, _power: &[f64], options: &[Vec<i32>]) {
        self.leader = leader;
        self.joint_zero = false;
        if !self.joint_linear() {
            return;
        }
        self.domains = options.to_vec();
        let profile = power::leader_profile(self.master, &self.members[leader]);
        let width = self.snaps.len() + 1;
        let mut valid = true;
        for song in &mut self.songs {
            song.member_base = self
                .members
                .iter()
                .enumerate()
                .map(|(member, card)| {
                    song.bases[member].fixed_points as f64
                        + power::leader_points(
                            &song.bases[member],
                            &power::leader_percent_bound(self.master, &profile, card),
                        ) as f64
                })
                .collect();
            song.domain_power = self
                .domains
                .iter()
                .enumerate()
                .map(|(member, options)| {
                    options
                        .iter()
                        .map(|&snap| {
                            let power = song.member_base[member]
                                + song.snap_power[member * width
                                    + if snap < 0 { width - 1 } else { snap as usize }];
                            valid &= (0.0..=i32::MAX as f64).contains(&power)
                                && song.skills[member * width
                                    + if snap < 0 { width - 1 } else { snap as usize }]
                                .1
                                .is_finite();
                            power
                        })
                        .fold(f64::NEG_INFINITY, f64::max)
                })
                .collect();
        }
        if !valid {
            for song in &mut self.songs {
                song.domain_power.clear();
            }
        }
    }
    fn early_member_bound(&mut self, members: &[usize], from: usize, _limit: f64) -> Option<f64> {
        self.joint_member_bound(members, from, 5 - members.len())
    }
    fn member_bound_at(
        &mut self,
        members: &[usize],
        _totals: Totals,
        left: usize,
        from: usize,
        _limit: f64,
    ) -> Option<f64> {
        self.joint_member_bound(members, from, left)
    }
    fn prepare_assignment(&mut self, members: &[usize], options: &[Vec<i32>]) {
        self.options = options.to_vec();
        let roster: Vec<_> = members.iter().map(|&m| self.members[m].clone()).collect();
        let profile = power::leader_profile(self.master, &roster[0]);
        let width = self.snaps.len() + 1;
        for song in &mut self.songs {
            let percents =
                power::leader_percents(self.master, &profile, &roster, 0, Some(&song.music));
            let bases: Vec<_> = members
                .iter()
                .enumerate()
                .map(|(slot, &m)| {
                    (song.bases[m].fixed_points
                        + power::leader_points(&song.bases[m], &percents[slot]))
                        as f64
                })
                .collect();
            song.slots = members
                .iter()
                .enumerate()
                .map(|(slot, &m)| {
                    (0..width)
                        .map(|s| bases[slot] + song.snap_power[m * width + s])
                        .collect()
                })
                .collect();
            song.objective
                .prepare_power(Arc::clone(&song.snap_power), width);
            song.objective.prepare_assignment_power(bases.iter().sum());
            song.objective.prepare_assignment(members, options);
        }
        self.joint_slots.clear();
        if self.joint_linear() {
            if self.songs.iter().any(|song| {
                options.iter().enumerate().any(|(slot, options)| {
                    options.iter().any(|&snap| {
                        let col = if snap < 0 { width - 1 } else { snap as usize };
                        !(0.0..=i32::MAX as f64).contains(&song.slots[slot][col])
                            || !song.skills[members[slot] * width + col].1.is_finite()
                    })
                })
            }) {
                self.joint_zero = false;
                return;
            }
            let caps: Vec<_> = self
                .songs
                .iter()
                .map(|song| {
                    options
                        .iter()
                        .enumerate()
                        .fold(0.0_f64, |sum, (slot, options)| {
                            (sum + options
                                .iter()
                                .map(|&snap| {
                                    song.slots[slot]
                                        [if snap < 0 { width - 1 } else { snap as usize }]
                                })
                                .fold(f64::NEG_INFINITY, f64::max))
                            .next_up()
                        })
                })
                .collect();
            if caps
                .iter()
                .any(|cap| !(0.0..=i32::MAX as f64).contains(cap))
            {
                self.joint_zero = false;
                return;
            }
            if self.joint_zero {
                return;
            }
            self.joint_slots = members
                .iter()
                .enumerate()
                .map(|(slot, &member)| {
                    (0..width)
                        .map(|col| {
                            self.joint_weight(
                                member,
                                if col == width - 1 { -1 } else { col as i32 },
                                &caps,
                                Some(slot),
                            )
                        })
                        .collect()
                })
                .collect();
        }
    }
    fn cheap_assignment_bound(
        &mut self,
        members: &[usize],
        _t: Totals,
        assigned: &[i32],
        from: usize,
    ) -> f64 {
        self.upper(members, assigned, from)
    }
    fn interval(&mut self, team: &Team, _t: Totals) -> (f64, f64) {
        let fallback = (
            if self.songs.iter().any(|song| song.unbounded) {
                f64::NEG_INFINITY
            } else {
                0.0
            },
            f64::INFINITY,
        );
        if !self.joint_linear() {
            return fallback;
        }
        let width = self.snaps.len() + 1;
        let (mut low, mut high) = (0.0_f64, 0.0_f64);
        let mut exact_zero = true;
        for song in &mut self.songs {
            if song.slots.len() != 5 {
                return fallback;
            }
            let mut totals = Totals::default();
            for (slot, (&member, &snap)) in team.members.iter().zip(&team.snaps).enumerate() {
                let col = if snap < 0 { width - 1 } else { snap as usize };
                let power = song.slots[slot][col];
                if !(0.0..=i32::MAX as f64).contains(&power) {
                    return fallback;
                }
                totals.power += power;
                let skill = song.skills[member * width + col];
                totals.skill_low = (totals.skill_low + skill.0).next_down();
                totals.skill = (totals.skill + skill.1).next_up();
            }
            if totals.power > i32::MAX as f64 {
                return fallback;
            }
            let range = song.objective.interval(team, totals);
            exact_zero &= range.0 == 0.0 && range.1 == 0.0;
            low = (low + range.0).next_down();
            high = (high + range.1).next_up();
        }
        if exact_zero {
            return (0.0, 0.0);
        }
        (
            (low / self.songs.len() as f64).next_down(),
            (high / self.songs.len() as f64).next_up(),
        )
    }
    fn exact(&mut self, team: &Team, _power: f64, _slots: &[f64]) -> Result<Exact, String> {
        if !self.eligible(team) {
            return Ok(Exact {
                key: f64::NEG_INFINITY,
                detail: Value::Null,
            });
        }
        let members: Vec<_> = team
            .members
            .iter()
            .map(|&m| self.members[m].clone())
            .collect();
        let snaps: Vec<_> = team
            .snaps
            .iter()
            .map(|&s| (s >= 0).then(|| self.snaps[s as usize].clone()))
            .collect();
        let mut parts = Vec::new();
        for song in &mut self.songs {
            let p = power::team_power(
                self.master,
                self.player,
                &members,
                &snaps,
                0,
                Some(&song.music),
                self.effects,
            )?;
            let mut exact = song.objective.exact(
                team,
                p.total as f64,
                &p.slots.iter().map(|&v| v as f64).collect::<Vec<_>>(),
            )?;
            if exact.detail["settlement"].is_object() {
                exact.detail["otherMean"] = exact.detail["settlement"]["otherMean"].clone();
            }
            parts.push(
                json!({"key":exact.key,"detail":exact.detail,"power":p.total,"slotPowers":p.slots}),
            );
        }
        let count = parts.len() as f64;
        let mean = |path: &[&str]| {
            parts
                .iter()
                .map(|part| {
                    let mut value = part;
                    for key in path {
                        value = &value[*key];
                    }
                    master::number(value, 0.0)
                })
                .sum::<f64>()
                / count
        };
        let mut detail = json!({"songs":parts,"otherMean":mean(&["detail","otherMean"]),"scores":{"mean":mean(&["detail","scores","mean"])}});
        if let Some(window) = crate::just_window::aggregate(parts.iter().map(|part| &part["detail"]["justWindow"])) { detail["justWindow"] = window; }
        let mut key = mean(&["key"]);
        if let Some(plan) = &self.plan {
            let ranks: Vec<_> = (0..plan.levels.len())
                .map(|i| {
                    Fraction::mean(
                        &parts
                            .iter()
                            .map(|p| Fraction::from_json(&p["detail"]["rewardRanking"][i]).unwrap())
                            .collect::<Vec<_>>(),
                    )
                })
                .collect();
            key = ranks[0].lower();
            detail["rewardRanking"] = json!(ranks.iter().map(Fraction::json).collect::<Vec<_>>());
            detail["rewardPlan"] = json!(plan);
            detail["planMean"] = json!(ranks[0].divided(plan.unit).value());
            detail["rewardValues"] = json!({});
            for metric in ["points", "challenge-points", "items"] {
                detail["rewardValues"][metric] = Fraction::mean(
                    &parts
                        .iter()
                        .map(|p| Fraction::from_json(&p["detail"]["rewardValues"][metric]).unwrap())
                        .collect::<Vec<_>>(),
                )
                .json();
            }
        }
        Ok(Exact { key, detail })
    }
}

impl Engine {
    pub(super) fn run_shared(
        &mut self,
        input: &Value,
        started: f64,
        now: &dyn Fn() -> f64,
    ) -> Result<Value, String> {
        let request = &input["request"];
        let goal = &request["goal"];
        let songs: Vec<_> = master::rows(&goal["songs"]).into_iter().cloned().collect();
        if songs.is_empty() {
            return Err("shared-songs-missing".into());
        }
        if goal["kind"] == "gekiso" {
            return self.run_arena(input, started, now);
        }
        let event = goal["kind"] == "event";
        if !event && goal["kind"] != "score" || goal["route"] == "skip" {
            return Err("shared-goal-unsupported".into());
        }
        if !event && (goal["criterion"] != "mean" || goal["target"].is_object()) {
            return Err("shared-mean-required".into());
        }
        if event && goal["ranking"] == "pt-only" {
            let issues = crate::pt::validate(&self.master, request)?;
            if !issues.is_empty() {
                return Err(format!("pt-input:{}", issues.join(",")));
            }
        }
        let _scope = crate::objectives::slot_table_scope();
        let mut sources = Vec::new();
        let mut charts = Vec::new();
        for song in &songs {
            self.load_source(input, song)?;
            let key = (
                master::int(&song["songId"]),
                master::int(&song["difficulty"]),
            );
            if goal["ranking"] == "pt-only" {
                crate::pt::validate_chart(&self.master, key.0, key.1, &self.sources[&key])?;
            }
            sources.push(self.sources[&key].clone());
            charts.push(self.charts[&key].clone());
        }
        let cards = search_box(&self.master, request)?;
        crate::progress::keys(&cards.members, &cards.snaps);
        let player = power::player_state(&self.master, &request["player"]);
        let route_kind = if goal["route"] == "challenge" {
            RouteKind::Challenge
        } else {
            RouteKind::Live
        };
        let route = event
            .then(|| {
                Route::for_goal(
                    &self.master,
                    goal,
                    route_kind,
                    master::int(&goal["consumption"]),
                )
            })
            .transpose()?;
        let challenge = if route_kind == RouteKind::Challenge {
            Some(master::int(&goal["eventId"]))
        } else {
            goal["challengeEventId"].as_i64().map(|v| v as i32)
        };
        let effects = if let Some(id) = challenge {
            self.master
                .events
                .get(&id)
                .map(|e| e.effects.clone())
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let plan = (!goal["rewardPlan"].is_null())
            .then(|| Plan::parse(&goal["rewardPlan"]))
            .transpose()?;
        let k = master::number(&request["k"], 1.0).max(1.0) as usize;
        let battle = goal["gekiso"].is_object();
        let mut objectives = Vec::new();
        for ((song, chart), source) in songs.iter().zip(&charts).zip(&sources) {
            let music = power::music_view(&self.master, master::int(&song["songId"]), challenge)?;
            let anchor = plan.as_ref().map_or(
                if goal["measure"] == "items" {
                    Metric::Items
                } else {
                    Metric::Points
                },
                Plan::anchor,
            );
            let exchange = (plan.is_none() && goal["cpExchange"].is_object()).then(|| {
                (
                    master::number(&goal["cpExchange"]["numerator"], 0.0),
                    master::number(&goal["cpExchange"]["denominator"], 1.0),
                )
            });
            let mut linear = None;
            let unbounded;
            let mut objective: Box<dyn Objective> = if let Some(route) = &route {
                let raw = if battle {
                    let mut gk = crate::gekiso::Chart::new(
                        &self.master,
                        source,
                        chart.difficulty.play_level,
                        chart.song.missions.clone(),
                        crate::full::play::Accuracy {
                            just: master::number(&goal["gekiso"]["just"], 1.0),
                            ..Default::default()
                        },
                        vec![1; 3],
                    )?;
                    gk.music_type = music.music_type;
                    let mut objective = crate::rewards::RewardObjective::new(
                            &self.master,
                            &cards.members,
                            &cards.snaps,
                            chart,
                            gk,
                            route.clone(),
                            anchor == Metric::Points,
                            master::int(&goal["gekiso"]["luckSamples"]),
                            exchange,
                        );
                    if let Some(minimum) = crate::just_window::target(&goal["gekiso"]["minJustWindowMs"])? { objective.set_min_just_window(minimum)?; }
                    crate::reward_plan::Source::Gekiso(Box::new(objective))
                } else {
                    crate::reward_plan::Source::Ordinary(Box::new(EventObjective::new(
                        &self.master,
                        &cards.members,
                        &cards.snaps,
                        Some(chart),
                        route.clone(),
                        anchor == Metric::Points,
                        &goal["play"],
                        true,
                        0.0,
                        exchange,
                        f64::INFINITY,
                    )))
                };
                unbounded = match &raw {
                    crate::reward_plan::Source::Ordinary(event) => {
                        event.core.as_ref().is_some_and(|core| core.unbounded)
                    }
                    crate::reward_plan::Source::Gekiso(_) => false,
                };
                if let Some(plan) = &plan {
                    Box::new(Ranked::new(
                        raw,
                        Context {
                            plan: plan.clone(),
                            source_anchor: anchor,
                            route: route.clone(),
                            chart: Some(chart),
                            members: &cards.members,
                            snaps: &cards.snaps,
                            k,
                            floor: f64::NEG_INFINITY,
                            skip_rank: self.master.params.skip_rank,
                            observations: 120
                                * if battle {
                                    master::int(&goal["gekiso"]["luckSamples"]).max(1) as usize
                                } else {
                                    1
                                },
                        },
                    ))
                } else if goal["measure"] == "challenge-points" {
                    Box::new(crate::event::ChallengePoints::new(
                        LiveObjective {
                            core: LiveCore::new(
                                &self.master,
                                chart,
                                &cards.members,
                                &cards.snaps,
                                &goal["play"],
                            ),
                            criterion: Criterion::Mean,
                            target: None,
                        },
                        chart.clone(),
                        route.clone(),
                    ))
                } else {
                    match raw {
                        crate::reward_plan::Source::Ordinary(v) => v,
                        crate::reward_plan::Source::Gekiso(v) => v,
                    }
                }
            } else {
                let core = LiveCore::new(
                    &self.master,
                    chart,
                    &cards.members,
                    &cards.snaps,
                    &goal["play"],
                );
                let scale = ((1.0 + crate::model::RELATIVE_ERROR)
                    / core.input.converted_count as f64)
                    .next_up();
                unbounded = core.unbounded;
                linear = (!unbounded).then(|| ((core.high.total * scale).next_up(), scale));
                Box::new(LiveObjective {
                    core,
                    criterion: Criterion::Mean,
                    target: None,
                })
            };
            let width = cards.snaps.len() + 1;
            let bases: Vec<_> = cards
                .members
                .iter()
                .map(|m| {
                    power::slot_base(
                        &self.master,
                        &player,
                        m,
                        Some(&music),
                        power::member_event_percent(&effects, m, 2),
                    )
                })
                .collect();
            let mut snap_power = Vec::new();
            let mut skills = Vec::new();
            for (m, member) in cards.members.iter().enumerate() {
                for (s, snap) in cards
                    .snaps
                    .iter()
                    .map(Some)
                    .chain(std::iter::once(None))
                    .enumerate()
                {
                    snap_power.push(power::snap_points(
                        &self.master,
                        &bases[m],
                        member,
                        snap,
                        snap.map_or(0, |s| power::snap_event_percent(&effects, s, 2)),
                    ) as f64);
                    skills.push(objective.skill(m, if s == width - 1 { -1 } else { s as i32 }));
                }
            }
            let profiles: Vec<_> = cards
                .members
                .iter()
                .map(|m| power::leader_profile(&self.master, m))
                .collect();
            let mut caps: Vec<_> = cards
                .members
                .iter()
                .enumerate()
                .map(|(m, member)| {
                    (bases[m].fixed_points
                        + profiles
                            .iter()
                            .map(|p| {
                                power::leader_points(
                                    &bases[m],
                                    &power::leader_percent_bound(&self.master, p, member),
                                )
                            })
                            .max()
                            .unwrap_or(0)) as f64
                        + snap_power[m * width..(m + 1) * width]
                            .iter()
                            .copied()
                            .fold(0.0, f64::max)
                })
                .collect();
            caps.sort_by(|a, b| b.total_cmp(a));
            let cap = caps
                .iter()
                .take(5)
                .fold(0.0_f64, |sum, v| (sum + v).next_up());
            let snap_power = Arc::new(snap_power);
            objective.prepare_power(Arc::clone(&snap_power), width);
            let opening = if battle && chart.song.missions.iter().any(|m| *m == 1 || *m == 4) {
                crate::gekiso_objective::opening_combo_bonuses(
                    &self.master,
                    &cards.members,
                    &chart.song.missions,
                )
            } else {
                Vec::new()
            };
            objectives.push(SongObjective {
                objective,
                music,
                bases,
                snap_power,
                skills,
                cap,
                linear,
                unbounded,
                slots: Vec::new(),
                opening,
                member_base: Vec::new(),
                domain_power: Vec::new(),
            });
        }
        let affine = (effects.is_empty()
            && objectives.iter().all(|song| {
                song.linear.is_some()
                    && song.music.music_type == objectives[0].music.music_type
                    && song
                        .bases
                        .iter()
                        .zip(&objectives[0].bases)
                        .all(|(a, b)| a.common == b.common && a.fixed_points == b.fixed_points)
            }))
        .then(|| {
            (objectives.iter().fold(0.0_f64, |sum, song| {
                (sum + song.linear.unwrap().0).next_up()
            }) / objectives.len() as f64)
                .next_up()
        });
        let music = objectives[0].music.clone();
        let width = cards.snaps.len() + 1;
        let priority = if !event
            && objectives.iter().all(|song| {
                song.linear.is_some_and(|(base, scale)| {
                    base.is_finite() && base >= 0.0 && scale.is_finite() && scale >= 0.0
                }) && (0.0..=i32::MAX as f64).contains(&song.cap)
                    && song.skills.iter().all(|skill| skill.1.is_finite())
            }) {
            (0..cards.members.len())
                .flat_map(|member| {
                    let objectives = &objectives;
                    (0..width).map(move |col| {
                        objectives
                            .iter()
                            .map(|song| {
                                let (base, scale) = song.linear.unwrap();
                                base * (song.bases[member].fixed_points as f64
                                    + song.snap_power[member * width + col])
                                    + song.cap
                                        * scale
                                        * song.skills[member * width + col].1.max(0.0)
                            })
                            .sum()
                    })
                })
                .collect()
        } else {
            Vec::new()
        };
        let mut combined = Combined {
            master: &self.master,
            player: &player,
            members: &cards.members,
            snaps: &cards.snaps,
            effects: &effects,
            songs: objectives,
            options: Vec::new(),
            min_combo: master::int(&goal["gekiso"]["minComboBonus"]).max(1),
            event,
            plan,
            affine,
            leader: 0,
            domains: Vec::new(),
            joint_members: Vec::new(),
            joint_suffix: Vec::new(),
            joint_slots: Vec::new(),
            joint_matching: crate::assignment::Scratch::default(),
            joint_zero: false,
            priority,
            floor: f64::NEG_INFINITY,
            k,
        };
        let deadline = request["deadline"].as_f64().unwrap_or_else(|| {
            request["timeLimitMs"]
                .as_f64()
                .map_or(f64::INFINITY, |v| started + v)
        });
        let mut publish = |hits: &[search::Hit]| {
            crate::progress::emit_document(
                &json!({"song":null,"hits":hits.iter().map(|h|shared_hit(h,&songs,&charts,&cards.members,&cards.snaps,battle,event,goal)).collect::<Vec<_>>()}),
            )
        };
        let output = search::search(search::Input {
            master: &self.master,
            player: &player,
            members: &cards.members,
            snaps: &cards.snaps,
            music: Some(&music),
            objective: &mut combined,
            constraints: constraints(&request["constraints"], &cards.members, &cards.snaps),
            k,
            deadline,
            now,
            seeds: seed_teams(request, &Value::Null, &cards.members, &cards.snaps),
            shard: request["shard"].is_object().then(|| {
                (
                    master::int(&request["shard"]["index"]) as usize,
                    master::int(&request["shard"]["count"]) as usize,
                )
            }),
            owns: None,
            floor: f64::NEG_INFINITY,
            floor_exclusive: false,
            streaming: true,
            disable_snap_dominance: event,
            member_power_percent: None,
            snap_power_percent: None,
            on_candidate: Some(&mut publish),
            progress: None,
        })?;
        let hits: Vec<_> = output
            .hits
            .iter()
            .map(|h| {
                shared_hit(
                    h,
                    &songs,
                    &charts,
                    &cards.members,
                    &cards.snaps,
                    battle,
                    event,
                    goal,
                )
            })
            .collect();
        Ok(
            json!({"results":[{"song":null,"hits":hits,"proven":output.proven,"bound":output.bound,"stats":output.stats}],"overall":hits,"unknownCards":cards.unknown,"cachedCharts":self.cached_chart_keys(),"elapsedMs":now()-started,"stopReason":if output.proven {"complete"} else if now()>=deadline {"budget"} else {"cancelled"}}),
        )
    }
}

#[allow(clippy::too_many_arguments)]
fn shared_hit(
    hit: &search::Hit,
    songs: &[Value],
    charts: &[Chart],
    members: &[power::Member],
    snaps: &[power::Snap],
    battle: bool,
    event: bool,
    goal: &Value,
) -> Value {
    let parts: Vec<_> = master::rows(&hit.detail["songs"]).into_iter().enumerate().map(|(i,p)| {
        let child = search::Hit {team:hit.team.clone(),power:master::number(&p["power"],0.0),slot_powers:master::rows(&p["slotPowers"]).iter().map(|v|master::number(v,0.0)).collect(),key:master::number(&p["key"],0.0),detail:p["detail"].clone(),totals:Totals::default()};
        let mut row=hit_json(&child,&songs[i],members,snaps,Some(&charts[i]),battle);
        let d=&child.detail;
        row["comboBonus"]=d["comboBonus"].clone();
        if event {
            let settlement=&d["settlement"];
            let count=master::number(&settlement["observations"],120.0);
            let direct=if battle {master::number(&settlement["rewardSum"],0.0)/count} else {master::number(&d["directMean"],0.0)};
            let cp=if battle {master::number(&settlement["cpSum"],0.0)/count} else {master::number(&d["challengePoints"],0.0)};
            let mean=if goal["rewardPlan"].is_object() {0.0} else if battle {master::number(&settlement["comparisonSum"],child.key*count)/count/master::number(&goal["cpExchange"]["denominator"],1.0)} else {master::number(&d["mean"],child.key)/master::number(&goal["cpExchange"]["denominator"],1.0)};
            row["event"]=json!({"mean":mean,"bonusPercent":master::number(&d["bonus"],0.0)/100.0,"challengePoints":cp,"directMean":direct,"otherMean":if battle {&settlement["otherMean"]} else {&d["otherMean"]},"perPlayMin":if battle {&settlement["directMin"]} else {&d["directMin"]},"perPlayMax":if battle {&settlement["directMax"]} else {&d["directMax"]}});
            if battle { row["score"]["ranks"]=settlement["ranks"].clone(); }
            crate::reward_plan::decorate(&mut row,d);
        }
        row
    }).collect();
    let count = parts.len() as f64;
    let avg = |path: &[&str]| {
        parts
            .iter()
            .map(|p| {
                let mut v = p;
                for key in path {
                    v = &v[*key];
                }
                master::number(v, 0.0)
            })
            .sum::<f64>()
            / count
    };
    let mut row = hit_json(hit, &Value::Null, members, snaps, None, battle);
    row["power"] = json!(avg(&["power"]));
    row["slotPowers"] = json!((0..5)
        .map(|i| parts
            .iter()
            .map(|p| master::number(&p["slotPowers"][i], 0.0))
            .sum::<f64>()
            / count)
        .collect::<Vec<_>>());
    row["score"] = json!({"mean":avg(&["score","mean"]),"min":avg(&["score","min"]),"max":avg(&["score","max"]),"median":avg(&["score","median"]),"orders":[],"ranks":{},"bestOrder":[],"worstOrder":[]});
    if event {
        row["event"] = json!({});
        for key in [
            "mean",
            "bonusPercent",
            "challengePoints",
            "directMean",
            "otherMean",
            "perPlayMin",
            "perPlayMax",
        ] {
            row["event"][key] = json!(avg(&["event", key]));
        }
        crate::reward_plan::decorate(&mut row, &hit.detail);
    }
    row["aggregate"] = json!({"kind":"mean","songs":parts});
    row
}
