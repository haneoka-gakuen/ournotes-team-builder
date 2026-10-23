//! Stateful skill condition trees and cumulative-counter factories.
use super::{
    core::{self, Life, Random, STREAM_SKILL},
    gekisou::{self, Controller, State},
};
use crate::{
    master::{Master, Target},
    num::f,
    power::{Member, Snap},
};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
pub struct GkView<'a> {
    pub controller: &'a Controller,
    pub previous_lots: &'a [i32],
    pub previous_lot_ms: i32,
}
pub struct Context<'a> {
    pub previous_rank: Option<i32>,
    pub life: &'a mut Life,
    pub random: &'a mut Random,
    pub time: i32,
    pub combo: i32,
    pub judged: &'a [(i32, i32, i32)],
    pub events: &'a [(usize, i32)],
    pub gk: Option<GkView<'a>>,
    pub record: bool,
}
impl Context<'_> {
    fn controller(&self, kind: i32) -> Result<&Controller, String> {
        self.gk.as_ref().map(|g| g.controller).ok_or_else(|| {
            format!("condition type {kind} reads the Gekisou state of a live without Gekisou")
        })
    }
    fn playing(&self) -> Option<&State> {
        let g = self.gk.as_ref()?;
        let i = g.controller.current_playing;
        if i < 0 {
            None
        } else {
            g.controller.states.get(i as usize)
        }
    }
}
#[derive(Clone)]
pub enum Test {
    Fixed(bool),
    And(Vec<Checker>),
    Or(Vec<Checker>),
    Not(Box<Checker>),
    Life {
        kind: i32,
        value: Option<f64>,
    },
    LifeChanged(i32),
    LifePercent {
        threshold: i32,
        previous: i32,
    },
    LifeDelta {
        threshold: i32,
        increase: bool,
        previous: i32,
        count: i32,
    },
    LiveMultiple {
        threshold: i32,
        previous: i32,
    },
    LiveAtLeast(i32),
    Elapsed {
        period: i32,
        previous: i32,
        elapsed: i32,
    },
    SnapStart(i32),
    SameMember(usize),
    Probability(f64),
    JudgementMatch {
        kind: i32,
        targets: Vec<i32>,
    },
    JudgementCount {
        n: f64,
        consecutive: bool,
        targets: Vec<i32>,
        count: i32,
    },
    LotResult(f64),
    ComboAtLeast(f64),
    GkInterval {
        kind: i32,
        n: i32,
        previous: i32,
    },
    JustAtLeast(i32),
    JustEdge {
        raw: bool,
        threshold: i32,
        previous: i32,
        last_range: i32,
    },
    Once {
        rank: Option<f64>,
        triggered: bool,
    },
    RangeStart {
        missions: Vec<i32>,
        triggered: Vec<usize>,
    },
    RangeComplete,
    RangePlaying {
        missions: Vec<i32>,
        active: Vec<(usize, i32)>,
    },
    Rush(bool),
}
#[derive(Clone)]
pub struct Checker {
    pub test: Test,
    pub override_time: Option<i32>,
    idle: Option<IdleInput>,
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum IdleInput {
    Never,
    Member(usize),
    Range,
    Lottery,
    Judgement,
    JudgementRange,
    Rank,
}
impl IdleInput {
    pub(crate) fn active(self, ctx: &Context<'_>) -> bool {
        match self {
            Self::Never => false,
            Self::Member(member) => ctx.events.iter().any(|event| event.0 == member),
            Self::Range => ctx
                .gk
                .as_ref()
                .is_none_or(|g| !g.controller.updates.is_empty()),
            Self::Lottery => ctx.gk.as_ref().is_none_or(|g| !g.previous_lots.is_empty()),
            Self::Judgement => !ctx.judged.is_empty(),
            Self::JudgementRange => !ctx.judged.is_empty() || Self::Range.active(ctx),
            Self::Rank => ctx.previous_rank.is_some(),
        }
    }
}
impl Checker {
    pub fn new(test: Test) -> Self {
        let idle = Self::classify_input(&test);
        Self {
            test,
            override_time: None,
            idle,
        }
    }
    pub fn override_time(&self) -> Option<i32> {
        if let Test::Not(inner) = &self.test {
            inner.override_time()
        } else {
            self.override_time
        }
    }
    pub fn count_resettable(&self) -> bool {
        matches!(
            self.test,
            Test::Not(_)
                | Test::LifeDelta { .. }
                | Test::Elapsed { .. }
                | Test::JudgementCount { .. }
        )
    }
    pub(crate) fn idle_input(&self) -> Option<IdleInput> {
        self.idle
    }
    fn classify_input(test: &Test) -> Option<IdleInput> {
        match test {
            Test::Fixed(false) => Some(IdleInput::Never),
            Test::SameMember(member) => Some(IdleInput::Member(*member)),
            Test::RangeStart { .. } | Test::RangeComplete => Some(IdleInput::Range),
            Test::LotResult(_) => Some(IdleInput::Lottery),
            Test::JudgementMatch { .. } | Test::JudgementCount { .. } => Some(IdleInput::Judgement),
            Test::And(items)
                if items.len() == 2
                    && items
                        .iter()
                        .any(|item| matches!(item.test, Test::RangePlaying { .. }))
                    && items.iter().any(|item| {
                        matches!(
                            item.test,
                            Test::JudgementMatch { .. } | Test::JudgementCount { .. }
                        )
                    }) =>
            {
                Some(IdleInput::JudgementRange)
            }
            Test::Once { rank: Some(_), .. } => Some(IdleInput::Rank),
            _ => None,
        }
    }
    pub fn check(&mut self, ctx: &mut Context<'_>) -> Result<(bool, i32), String> {
        if self.idle.is_some_and(|input| !input.active(ctx)) {
            self.override_time = None;
            return Ok((false, 0));
        }
        let result = match &mut self.test {
            Test::Fixed(ok) => (*ok, i32::from(*ok)),
            Test::And(items) => {
                let mut hit = 0;
                let mut all = true;
                for i in 0..items.len() {
                    let (ok, h) = items[i].check(ctx)?;
                    hit = hit.max(h);
                    if !ok {
                        if !items[i].count_resettable() {
                            for item in items.iter_mut() {
                                if item.count_resettable() {
                                    item.reset_count(ctx)?;
                                }
                            }
                        }
                        all = false;
                        break;
                    }
                }
                if all {
                    (true, hit)
                } else {
                    (false, 0)
                }
            }
            Test::Or(items) => {
                let mut hit = 0;
                let mut any = false;
                for item in items {
                    let (ok, h) = item.check(ctx)?;
                    hit = hit.max(h);
                    if ok {
                        any = true;
                        break;
                    }
                }
                if any {
                    (true, hit)
                } else {
                    (false, 0)
                }
            }
            Test::Not(inner) => {
                let (ok, h) = inner.check(ctx)?;
                (!ok, h)
            }
            Test::Life { kind, value } => {
                let value = value.ok_or("skill condition without a value")?;
                let life = ctx.life.at(ctx.time) as f64;
                let ok = match *kind {
                    2000 => value < life,
                    2001 => value <= life,
                    2002 => life < value,
                    _ => life <= value,
                };
                (ok, i32::from(ok))
            }
            Test::LifeChanged(previous) => {
                let life = ctx.life.at(ctx.time);
                let ok = *previous != life;
                *previous = life;
                (ok, 0)
            }
            Test::LifePercent {
                threshold,
                previous,
            } => {
                let life = ctx.life.at(ctx.time);
                let ok = *previous > *threshold && life <= *threshold;
                *previous = life;
                (ok, i32::from(ok))
            }
            Test::LifeDelta {
                threshold,
                increase,
                previous,
                count,
            } => {
                let life = ctx.life.at(ctx.time);
                let change = if *increase {
                    life.wrapping_sub(*previous)
                } else {
                    previous.wrapping_sub(life)
                };
                *previous = life;
                if change > 0 {
                    *count = count.wrapping_add(change);
                }
                if *threshold <= 0 {
                    return Err("nonpositive life change threshold".into());
                }
                let hits = if *count >= *threshold {
                    core::sdiv(*count, *threshold)
                } else {
                    0
                };
                if hits > 0 {
                    *count = core::srem(*count, *threshold);
                }
                (hits > 0, hits)
            }
            Test::LiveMultiple {
                threshold,
                previous,
            } => {
                let combo = ctx.combo;
                if combo < 1 || *threshold < 1 {
                    *previous = 0;
                    (false, 0)
                } else if combo <= *previous {
                    *previous = combo;
                    (false, 0)
                } else {
                    let hit = core::sdiv(combo, *threshold) - core::sdiv(*previous, *threshold);
                    *previous = combo;
                    (hit > 0, hit)
                }
            }
            Test::LiveAtLeast(threshold) => {
                let hit = *threshold > 0 && ctx.combo >= *threshold;
                (hit, i32::from(hit))
            }
            Test::Elapsed {
                period,
                previous,
                elapsed,
            } => {
                *elapsed = elapsed.wrapping_add(ctx.time).wrapping_sub(*previous);
                *previous = ctx.time;
                let hit = *elapsed >= *period;
                if hit {
                    *elapsed = elapsed.wrapping_sub(*period);
                }
                (hit, i32::from(hit))
            }
            Test::SnapStart(previous) => {
                let mut hit = false;
                if let Some(g) = &ctx.gk {
                    for &i in &g.controller.updates {
                        if g.controller.states[i].state == gekisou::START && i as i32 != *previous {
                            *previous = i as i32;
                            hit = true;
                            break;
                        }
                    }
                }
                (hit, i32::from(hit))
            }
            Test::SameMember(member) => {
                let hit = ctx.events.iter().any(|event| event.0 == *member);
                (hit, i32::from(hit))
            }
            Test::Probability(rate) => {
                if ctx.record {
                    (true, 0)
                } else {
                    (ctx.random.value(STREAM_SKILL) < *rate, 0)
                }
            }
            Test::JudgementMatch { kind, targets } => {
                self.override_time = None;
                let mut hits = 0;
                for &(id, j, time) in ctx.judged {
                    if *kind == 1020 && (j == -1 || j == 7) {
                        continue;
                    }
                    for &target in targets.iter() {
                        let hit = if *kind == 1000 {
                            target == j
                        } else {
                            if !(1..=6).contains(&target) || !(1..=6).contains(&j) {
                                return Err("unknown judgement comparison rank".into());
                            }
                            if *kind == 1010 {
                                j >= target
                            } else {
                                j <= target
                            }
                        };
                        if hit {
                            hits += 1;
                            self.override_time = if id >= 0 { Some(time) } else { None };
                            break;
                        }
                    }
                }
                (hits > 0, hits)
            }
            Test::JudgementCount {
                n,
                consecutive,
                targets,
                count,
            } => {
                self.override_time = None;
                let (mut triggered, mut triggered_ms, mut hits) = (-1, 0, 0);
                let mut ok = false;
                for &(id, j, time) in ctx.judged {
                    for &target in targets.iter() {
                        if target == j {
                            *count = count.wrapping_add(1);
                            if *count as f64 >= *n {
                                ok = true;
                                hits += 1;
                                *count = 0;
                                triggered = id;
                                triggered_ms = time;
                            }
                        } else if *consecutive {
                            *count = 0;
                        }
                    }
                }
                if ok && triggered >= 0 {
                    self.override_time = Some(triggered_ms);
                }
                (ok, hits)
            }
            Test::LotResult(target) => {
                self.override_time = None;
                let hits = ctx
                    .gk
                    .as_ref()
                    .map(|g| {
                        g.previous_lots
                            .iter()
                            .filter(|&&result| result as f64 == *target)
                            .count()
                    })
                    .unwrap_or(0) as i32;
                if hits > 0 {
                    self.override_time =
                        Some(ctx.gk.as_ref().map(|g| g.previous_lot_ms).unwrap_or(0));
                }
                (hits > 0, hits)
            }
            Test::ComboAtLeast(threshold) => {
                self.override_time = None;
                let c = ctx.controller(7005)?;
                let i = c.current_playing;
                if i < 0 || *threshold <= 0.0 {
                    (false, 0)
                } else {
                    let state = &c.states[i as usize];
                    if (state.combo as f64) < *threshold {
                        (false, 0)
                    } else {
                        if state.last_combo_ms >= 0 {
                            self.override_time = Some(state.last_combo_ms);
                        }
                        (true, 1)
                    }
                }
            }
            Test::GkInterval { kind, n, previous } => {
                self.override_time = None;
                let state = ctx.playing();
                let mut hits = 0;
                if *n > 0 {
                    if let Some(state) = state {
                        let current = if *kind == 7001 {
                            state.combo
                        } else if *kind == 7002 {
                            state.just
                        } else {
                            state.raw_just
                        };
                        if current <= 0 {
                            *previous = 0;
                        } else {
                            let old = *previous;
                            *previous = current;
                            if current > old {
                                hits = (core::sdiv(current, *n) - core::sdiv(old, *n)).max(0);
                            }
                        }
                    }
                }
                if hits > 0 {
                    if let Some(state) = state {
                        let time = if *kind == 7001 {
                            state.last_combo_ms
                        } else {
                            state.last_just_ms
                        };
                        self.override_time = if time >= 0 { Some(time) } else { None };
                    }
                }
                (hits > 0, hits)
            }
            Test::JustAtLeast(threshold) => {
                self.override_time = None;
                let state = ctx.playing();
                let hit = *threshold > 0 && state.is_some_and(|s| s.just >= *threshold);
                if hit {
                    let time = state.unwrap().last_just_ms;
                    if time >= 0 {
                        self.override_time = Some(time);
                    }
                }
                (hit, i32::from(hit))
            }
            Test::JustEdge {
                raw,
                threshold,
                previous,
                last_range,
            } => {
                self.override_time = None;
                let index = ctx
                    .gk
                    .as_ref()
                    .map(|g| g.controller.current_playing)
                    .unwrap_or(-1);
                let state = ctx.playing();
                if *threshold <= 0 {
                    (false, 0)
                } else if let Some(state) = state {
                    if index != *last_range {
                        *previous = 0;
                        *last_range = index;
                    }
                    let count = if *raw { state.raw_just } else { state.just };
                    let hit = *previous < *threshold && count >= *threshold;
                    *previous = count;
                    if hit && state.last_just_ms >= 0 {
                        self.override_time = Some(state.last_just_ms);
                    }
                    (hit, i32::from(hit))
                } else {
                    *previous = 0;
                    *last_range = -1;
                    (false, 0)
                }
            }
            Test::Once { rank, triggered } => {
                let eligible = if let Some(threshold) = rank {
                    ctx.previous_rank
                        .is_some_and(|rank| rank > 0 && rank as f64 <= *threshold)
                } else {
                    ctx.gk.is_some()
                };
                let hit = !*triggered && eligible;
                if hit {
                    *triggered = true;
                }
                (hit, i32::from(hit))
            }
            Test::RangeStart {
                missions,
                triggered,
            } => {
                self.override_time = None;
                let c = ctx.controller(7010)?;
                let mut hit = false;
                for &i in &c.updates {
                    let state = c.states[i].state;
                    if state == gekisou::FINISH {
                        triggered.retain(|&v| v != i);
                    } else if state == gekisou::START {
                        if !mission_ok(missions, c.ranges[i].mission) {
                            continue;
                        }
                        if !triggered.contains(&i) {
                            triggered.push(i);
                            self.override_time = Some(c.ranges[i].start);
                            hit = true;
                            break;
                        }
                    }
                }
                (hit, i32::from(hit))
            }
            Test::RangeComplete => {
                self.override_time = None;
                let c = ctx.controller(7013)?;
                let ended = c
                    .updates
                    .iter()
                    .find(|&&i| c.states[i].state == gekisou::END);
                let hit = if let Some(&i) = ended {
                    self.override_time = Some(c.ranges[i].end.wrapping_add(1));
                    true
                } else {
                    false
                };
                (hit, i32::from(hit))
            }
            Test::RangePlaying { missions, active } => {
                self.override_time = None;
                let c = ctx.controller(7020)?;
                let mut first = -1;
                for &i in &c.updates {
                    let state = c.states[i].state;
                    if state == gekisou::START || state == gekisou::PLAYING {
                        if !active.iter().any(|entry| entry.0 == i) {
                            active.push((i, c.ranges[i].mission));
                            if first < 0 {
                                first = c.ranges[i].start;
                            }
                        }
                    } else if state == gekisou::END
                        || state == gekisou::COMPLETE
                        || state == gekisou::FINISH
                    {
                        active.retain(|entry| entry.0 != i);
                    }
                }
                let hit = !active.is_empty()
                    && (missions.is_empty()
                        || missions.contains(&gekisou::MISSION_ALL)
                        || active.iter().any(|entry| missions.contains(&entry.1)));
                if hit && first >= 0 {
                    self.override_time = Some(first);
                }
                (hit, i32::from(hit))
            }
            Test::Rush(rush) => {
                if ctx.record {
                    (false, 0)
                } else {
                    let c = ctx.controller(7021)?;
                    let i = c.current_playing;
                    if i >= 0 && c.ranges[i as usize].mission == gekisou::MISSION_LUCK {
                        *rush = c.states[i as usize].luck.rush != 0;
                    }
                    for &i in &c.updates {
                        let state = c.states[i].state;
                        if c.ranges[i].mission == gekisou::MISSION_LUCK
                            && (state == gekisou::COMPLETE || state == gekisou::FINISH)
                        {
                            *rush = false;
                        }
                    }
                    (*rush, i32::from(*rush))
                }
            }
        };
        Ok(result)
    }
    pub fn reset(&mut self, ctx: &mut Context<'_>) -> Result<(), String> {
        match &mut self.test {
            Test::And(items) | Test::Or(items) => {
                for item in items {
                    item.reset(ctx)?;
                }
            }
            Test::Not(inner) => inner.reset(ctx)?,
            Test::LifeDelta {
                previous, count, ..
            } => {
                *count = 0;
                *previous = ctx.life.at(ctx.time);
            }
            Test::LiveMultiple { previous, .. } => *previous = 0,
            Test::Elapsed {
                previous, elapsed, ..
            } => {
                *elapsed = 0;
                *previous = ctx.time;
            }
            Test::SnapStart(previous) => *previous = -1,
            Test::JudgementMatch { .. }
            | Test::LotResult(_)
            | Test::ComboAtLeast(_)
            | Test::JustAtLeast(_) => self.override_time = None,
            Test::JudgementCount { count, .. } => *count = 0,
            Test::GkInterval { previous, .. } => {
                *previous = 0;
                self.override_time = None;
            }
            Test::JustEdge {
                previous,
                last_range,
                ..
            } => {
                *previous = 0;
                *last_range = -1;
                self.override_time = None;
            }
            Test::Once { triggered, .. } => *triggered = false,
            Test::RangeStart { triggered, .. } => {
                triggered.clear();
                self.override_time = None;
            }
            Test::RangePlaying { active, .. } => {
                active.clear();
                self.override_time = None;
            }
            Test::Rush(rush) => *rush = false,
            _ => {}
        }
        Ok(())
    }
    pub fn reset_count(&mut self, ctx: &mut Context<'_>) -> Result<(), String> {
        if let Test::Not(inner) = &mut self.test {
            if inner.count_resettable() {
                inner.reset_count(ctx)?;
            }
            Ok(())
        } else if self.count_resettable() {
            self.reset(ctx)
        } else {
            Ok(())
        }
    }
    pub fn chance(&mut self, ctx: &mut Context<'_>) -> Result<f64, String> {
        match &mut self.test {
            Test::Probability(rate) => Ok(*rate),
            Test::And(items) => {
                let mut probability = 1.0;
                for item in items {
                    probability *= item.chance(ctx)?;
                }
                Ok(probability)
            }
            Test::Or(items) => {
                let mut probability = 1.0;
                for item in items {
                    probability *= 1.0 - item.chance(ctx)?;
                }
                Ok(1.0 - probability)
            }
            Test::Not(inner) => Ok(1.0 - inner.chance(ctx)?),
            _ => Ok(f64::from(self.check(ctx)?.0)),
        }
    }
}
fn mission_ok(missions: &[i32], mission: i32) -> bool {
    missions.is_empty() || missions.contains(&gekisou::MISSION_ALL) || missions.contains(&mission)
}
#[derive(Clone, Default, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
pub struct Performer {
    pub live_skill: Option<(i32, i32)>,
    pub support_skills: Vec<(i32, i32)>,
    pub band_id: i32,
    pub character_id: i32,
    pub card_type: i32,
    pub tag_ids: Vec<i32>,
    pub live_skill_categories: Vec<i32>,
    pub gekisou_skill_categories: Vec<i32>,
    pub gekisou_mission_type: i32,
    pub gekisou_skill: Option<(i32, i32)>,
    pub gekisou_support_skills: Vec<(i32, i32)>,
    pub mission_only: Option<i32>,
}
impl Performer {
    pub fn from_member(master: &Master, member: &Member, snap: Option<&Snap>) -> Self {
        let pairs = |ids: [i32; 2], levels: [i32; 2]| {
            ids.into_iter()
                .zip(levels)
                .filter(|(id, _)| *id != 0)
                .collect()
        };
        Self {
            live_skill: Some((member.card.live_skill_id, member.growth.live_skill_level)),
            support_skills: snap
                .map(|s| pairs(s.card.support_skills, s.skill_levels))
                .unwrap_or_default(),
            band_id: member.card.band_id,
            character_id: member.card.character_id,
            card_type: member.card.card_type,
            tag_ids: member.card.best_tags.clone(),
            live_skill_categories: member.live_categories.clone(),
            gekisou_skill_categories: member.gekiso_categories.clone(),
            gekisou_mission_type: member.mission_type,
            gekisou_skill: if member.card.gekiso_skill_id != 0
                && master
                    .gekiso_skills
                    .contains_key(&member.card.gekiso_skill_id)
            {
                Some((
                    member.card.gekiso_skill_id,
                    member.growth.gekiso_skill_level,
                ))
            } else {
                None
            },
            gekisou_support_skills: snap
                .map(|s| pairs(s.card.gekiso_support_skills, s.gekiso_skill_levels))
                .unwrap_or_default(),
            mission_only: None,
        }
    }
    pub fn gekiso_only(&self) -> Self {
        let mut p = self.clone();
        p.live_skill = None;
        p.support_skills.clear();
        if p.gekisou_skill.is_none() && p.mission_only.unwrap_or(0) == 0 {
            p.gekisou_support_skills.clear();
        }
        p
    }
    pub fn support_only(&self) -> Self {
        let mut p = self.gekiso_only();
        p.gekisou_skill = None;
        p.mission_only = Some(1);
        p
    }
}
pub fn performer_matches(p: &Performer, t: &Target) -> bool {
    let categories =
        |targets: &[i32], values: &[i32]| targets.iter().any(|&c| c != 0 && values.contains(&c));
    (t.band_id > 0 && t.band_id == p.band_id)
        || (t.card_type != 0 && t.card_type == p.card_type)
        || (t.character_id > 0 && t.character_id == p.character_id)
        || (t.tag_id > 0 && p.tag_ids.contains(&t.tag_id))
        || categories(&t.live_categories, &p.live_skill_categories)
        || categories(&t.gekiso_categories, &p.gekisou_skill_categories)
        || (t.mission_type != 0 && t.mission_type == p.gekisou_mission_type)
}
#[derive(Clone)]
pub enum Cumulative {
    Fixed(i32),
    Combo {
        n: i32,
        empty: bool,
        maximum: i32,
        just: bool,
    },
    Life {
        initial: i32,
        n: i32,
        empty: bool,
        maximum: i32,
        limit: bool,
    },
    Judgements {
        n: i32,
        empty: bool,
        targets: Vec<i32>,
        maximum: i32,
        kind: i32,
        count: i32,
    },
    Elapsed {
        period: i32,
        previous: i32,
        maximum: i32,
        elapsed: i32,
        count: i32,
    },
}
impl Cumulative {
    pub fn initial(&self) -> i32 {
        match self {
            Self::Fixed(value) => *value,
            Self::Life { initial, .. } => *initial,
            _ => 0,
        }
    }
    pub fn unit(&self) -> i32 {
        match self {
            Self::Combo { n, .. } | Self::Judgements { n, .. } => *n,
            _ => 0,
        }
    }
    pub fn maximum(&self) -> i32 {
        match self {
            Self::Combo { maximum, .. } | Self::Judgements { maximum, .. } => *maximum,
            _ => 0,
        }
    }
    pub fn update(&mut self, ctx: &mut Context<'_>) -> Result<i32, String> {
        match self {
            Self::Fixed(value) => Ok(*value),
            Self::Combo {
                n,
                empty,
                maximum,
                just,
            } => {
                let Some(g) = &ctx.gk else {
                    return Ok(0);
                };
                let i = g.controller.current_playing;
                let mut value = 0;
                if i >= 0 {
                    if *empty {
                        return Err("skill condition without a value".into());
                    }
                    let state = &g.controller.states[i as usize];
                    value = core::floor_i32(f(f(
                        if *just { state.just } else { state.combo } as f64
                    ) / f(*n as f64)));
                }
                Ok(value.min(*maximum))
            }
            Self::Life {
                n,
                empty,
                maximum,
                limit,
                ..
            } => {
                if *empty {
                    return Err("skill condition without a value".into());
                }
                let life = if *limit {
                    ctx.life.maximum()
                } else {
                    ctx.life.at(ctx.time)
                };
                Ok(core::floor_i32(f(f(life as f64) / f(*n as f64))).min(*maximum))
            }
            Self::Judgements {
                n,
                empty,
                targets,
                maximum,
                kind,
                count,
            } => {
                for &(_, j, _) in ctx.judged {
                    if targets.iter().any(|&t| {
                        if *kind == 1001 {
                            j >= t
                        } else if *kind == 1002 {
                            j <= t
                        } else {
                            j == t
                        }
                    }) {
                        *count = count.wrapping_add(1);
                    }
                }
                if *empty {
                    return Err("skill condition without a value".into());
                }
                Ok(core::floor_i32(f(f(*count as f64) / f(*n as f64))).min(*maximum))
            }
            Self::Elapsed {
                period,
                previous,
                maximum,
                elapsed,
                count,
            } => {
                *elapsed = elapsed.wrapping_add(ctx.time).wrapping_sub(*previous);
                *previous = ctx.time;
                if *elapsed >= *period {
                    *elapsed = elapsed.wrapping_sub(*period);
                    *count = count.wrapping_add(1);
                }
                Ok((*count).min(*maximum))
            }
        }
    }
    pub fn reset(&mut self) {
        match self {
            Self::Judgements { count, .. } => *count = 0,
            Self::Elapsed { elapsed, count, .. } => {
                *elapsed = 0;
                *count = 0;
            }
            _ => {}
        }
    }
}
pub struct Factory<'a> {
    pub master: &'a Master,
    pub deck: &'a [Performer],
    pub initial_life: i32,
    pub music_type: i32,
    pub initial_time: i32,
}
impl Factory<'_> {
    fn target(&self, id: i32) -> Result<&Target, String> {
        self.master
            .targets
            .get(&id)
            .ok_or_else(|| format!("unknown skill target {id}"))
    }
    fn one(&self, id: i32, index: usize) -> Result<Option<Checker>, String> {
        let c = self
            .master
            .conditions
            .get(&id)
            .ok_or_else(|| format!("unknown skill condition {id}"))?;
        let v0 = c.values.first().copied();
        let need = || v0.ok_or_else(|| "skill condition without a value".to_owned());
        let targets = || {
            c.targets
                .iter()
                .map(|&id| self.target(id))
                .collect::<Result<Vec<_>, _>>()
        };
        let missions = || {
            Ok::<Vec<i32>, String>(
                targets()?
                    .iter()
                    .filter(|t| t.skill_target_type == 5 && t.mission_type != 0)
                    .map(|t| t.mission_type)
                    .collect(),
            )
        };
        let test = match c.kind {
            0 => return Ok(None),
            2000..=2003 => Test::Life {
                kind: c.kind,
                value: v0,
            },
            2004 => Test::LifeChanged(self.initial_life),
            4007 => Test::LifePercent {
                threshold: core::ceil_i32(f(f(f(need()?) / 10.0) * f(self.initial_life as f64))),
                previous: 0,
            },
            4008 | 4009 => Test::LifeDelta {
                threshold: crate::num::i32(need()?),
                increase: c.kind == 4008,
                previous: self.initial_life,
                count: 0,
            },
            3000 | 3001 => {
                let targets = targets()?;
                let count = self
                    .deck
                    .iter()
                    .filter(|p| targets.iter().any(|t| performer_matches(p, t)))
                    .count();
                Test::Fixed(if c.kind == 3000 {
                    count > 0
                } else {
                    count == self.deck.len()
                })
            }
            4001 => Test::LiveMultiple {
                threshold: crate::num::i32(need()?),
                previous: 0,
            },
            4002 => Test::LiveAtLeast(crate::num::i32(need()?)),
            4000 => Test::Elapsed {
                period: crate::num::i32(need()?).wrapping_mul(1000),
                previous: self.initial_time,
                elapsed: 0,
            },
            4010 | 5020 => Test::SameMember(index),
            5021 => Test::SnapStart(-1),
            4012 => Test::Fixed(targets()?.iter().any(|t| t.music_type == self.music_type)),
            4011 => Test::Probability(f(f(need()?) / 100.0)),
            5000 => {
                let matches = if let Some(p) = self.deck.get(index) {
                    targets()?.iter().any(|t| performer_matches(p, t))
                } else {
                    false
                };
                Test::Fixed(matches)
            }
            8000 => Test::Fixed(false),
            1000 | 1010 | 1020 | 1030 | 1040 => {
                let targets = targets()?
                    .iter()
                    .filter(|t| t.judgement != -1)
                    .map(|t| t.judgement)
                    .collect();
                if c.kind == 1030 || c.kind == 1040 {
                    Test::JudgementCount {
                        n: need()?,
                        consecutive: c.kind == 1040,
                        targets,
                        count: 0,
                    }
                } else {
                    Test::JudgementMatch {
                        kind: c.kind,
                        targets,
                    }
                }
            }
            7000 => Test::LotResult(need()?),
            7001 | 7002 | 7004 => Test::GkInterval {
                kind: c.kind,
                n: crate::num::i32(need()?),
                previous: 0,
            },
            7003 => Test::JustAtLeast(crate::num::i32(need()?)),
            7006 | 7007 => Test::JustEdge {
                raw: c.kind == 7007,
                threshold: crate::num::i32(need()?),
                previous: 0,
                last_range: -1,
            },
            7011 => Test::Once {
                rank: None,
                triggered: false,
            },
            7012 => Test::Once {
                rank: Some(v0.unwrap_or(1.0)),
                triggered: false,
            },
            7005 => Test::ComboAtLeast(need()?),
            7010 => Test::RangeStart {
                missions: missions()?,
                triggered: Vec::new(),
            },
            7013 => Test::RangeComplete,
            7020 => Test::RangePlaying {
                missions: missions()?,
                active: Vec::new(),
            },
            7021 => Test::Rush(false),
            kind => return Err(format!("unsupported skill condition type {kind}")),
        };
        let checker = Checker::new(test);
        Ok(Some(if c.positive {
            checker
        } else {
            Checker::new(Test::Not(Box::new(checker)))
        }))
    }
    pub fn group(&self, id: i32, index: usize) -> Result<Option<Checker>, String> {
        if id == 0 {
            return Ok(None);
        }
        let mut ors = Vec::new();
        for set in self.master.condition_sets.get(&id).into_iter().flatten() {
            let items = set
                .iter()
                .map(|&id| self.one(id, index))
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .flatten()
                .collect::<Vec<_>>();
            if items.len() == 1 {
                ors.push(items.into_iter().next().unwrap());
            } else if items.len() > 1 {
                ors.push(Checker::new(Test::And(items)));
            }
        }
        Ok(if ors.is_empty() {
            None
        } else if ors.len() == 1 {
            ors.into_iter().next()
        } else {
            Some(Checker::new(Test::Or(ors)))
        })
    }
    pub fn cumulative(&self, id: i32, index: usize) -> Result<Option<Cumulative>, String> {
        if id == 0 {
            return Ok(None);
        }
        let c = self
            .master
            .cumulative
            .get(&id)
            .ok_or_else(|| format!("unknown cumulative condition {id}"))?;
        let maximum = if c.cap < 1 { i32::MAX } else { c.cap };
        let n = c.values.first().copied().unwrap_or(0.0) as i32;
        let empty = c.values.is_empty();
        let value = match c.kind {
            7000 | 7001 => Cumulative::Combo {
                n,
                empty,
                maximum,
                just: c.kind == 7000,
            },
            2000 | 2001 => {
                if c.kind == 2000 && empty {
                    return Err("skill condition without a value".into());
                }
                Cumulative::Life {
                    initial: if c.kind == 2000 {
                        core::floor_i32(f(f(self.initial_life as f64) / f(n as f64)))
                    } else {
                        0
                    },
                    n,
                    empty,
                    maximum,
                    limit: c.kind == 2001,
                }
            }
            1000..=1002 => Cumulative::Judgements {
                n,
                empty,
                targets: c
                    .targets
                    .iter()
                    .map(|&id| self.target(id))
                    .collect::<Result<Vec<_>, _>>()?
                    .iter()
                    .filter(|t| t.judgement != -1)
                    .map(|t| t.judgement)
                    .collect(),
                maximum,
                kind: c.kind,
                count: 0,
            },
            3000..=3005 => {
                let count = if c.kind <= 3001 {
                    let targets = c
                        .targets
                        .iter()
                        .map(|&id| self.target(id))
                        .collect::<Result<Vec<_>, _>>()?;
                    self.deck
                        .iter()
                        .enumerate()
                        .filter(|(i, p)| {
                            (c.kind != 3001 || *i != index)
                                && targets.iter().any(|t| performer_matches(p, t))
                        })
                        .count()
                } else if c.kind <= 3003 {
                    let band = self
                        .deck
                        .get(index)
                        .ok_or("cumulative self member index out of range")?
                        .band_id;
                    if c.kind == 3002 {
                        self.deck.iter().filter(|p| p.band_id == band).count()
                    } else {
                        self.deck
                            .iter()
                            .filter(|p| p.band_id != band)
                            .map(|p| p.band_id)
                            .collect::<HashSet<_>>()
                            .len()
                    }
                } else {
                    self.deck
                        .iter()
                        .map(|p| {
                            if c.kind == 3004 {
                                p.band_id
                            } else {
                                p.card_type
                            }
                        })
                        .collect::<HashSet<_>>()
                        .len()
                };
                Cumulative::Fixed((count as i32).min(maximum))
            }
            6000 => {
                if empty {
                    return Err("skill condition without a value".into());
                }
                Cumulative::Elapsed {
                    period: n.wrapping_mul(1000),
                    previous: self.initial_time,
                    maximum,
                    elapsed: 0,
                    count: 0,
                }
            }
            kind => {
                return Err(format!(
                    "ArgumentOutOfRangeException: cumulative condition type {kind}"
                ))
            }
        };
        Ok(Some(value))
    }
}
