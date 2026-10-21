//! Card growth, slot bonuses, leader conditions and exact deck power.
use crate::{
    master::*,
    num::{f, imul},
};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Player {
    #[serde(default)]
    pub character_ranks: BTreeMap<i32, i32>,
    #[serde(default)]
    pub total_rank: i32,
    #[serde(default)]
    pub band_items: BTreeMap<i32, i32>,
    #[serde(default = "one")]
    pub vip_rank: i32,
    #[serde(default)]
    pub character_memory: BTreeMap<i32, i32>,
    #[serde(default)]
    pub music_memory: BTreeMap<i32, i32>,
}
fn one() -> i32 {
    1
}
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemberInput {
    pub key: String,
    pub card_id: i32,
    pub level: Option<i32>,
    pub awake: Option<i32>,
    pub rank: Option<i32>,
    pub live_skill_level: Option<i32>,
    pub gekiso_skill_level: Option<i32>,
    /** Resolve this card at full growth, whatever its fields and the unknown policy say. */
    #[serde(default)]
    pub maximize: bool,
}
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapInput {
    pub key: String,
    pub card_id: i32,
    pub level: Option<i32>,
    pub rank: Option<i32>,
    #[serde(default)]
    pub maximize: bool,
}
#[derive(Clone, PartialEq)]
pub struct MemberGrowth {
    pub level: i32,
    pub awake: i32,
    pub rank: i32,
    pub live_skill_level: i32,
    pub gekiso_skill_level: i32,
}
#[derive(Clone, PartialEq)]
pub struct SnapGrowth {
    pub level: i32,
    pub rank: i32,
}
#[derive(Clone)]
pub struct Member {
    pub key: String,
    pub card: MemberCard,
    pub growth: MemberGrowth,
    pub leader_skill_level: i32,
    pub music_type_rate: i32,
    pub music_tag_rate: i32,
    pub own: Stat3,
    pub live_categories: Vec<i32>,
    pub gekiso_categories: Vec<i32>,
    pub mission_type: i32,
}
#[derive(Clone)]
pub struct Snap {
    pub key: String,
    pub card: SnapCard,
    pub growth: SnapGrowth,
    pub percent: Stat3,
    pub type_link_rate: i32,
    pub skill_levels: [i32; 2],
    pub gekiso_skill_levels: [i32; 2],
}
#[derive(Clone)]
pub struct Music {
    pub music_type: i32,
    pub best_tags: Vec<i32>,
    pub memory_key: i32,
}
#[derive(Clone)]
pub struct SlotBase {
    pub common: Stat3,
    pub fixed_points: i64,
}
#[derive(Clone)]
pub struct LeaderProfile {
    pub effects: Vec<SkillEffect>,
    pub simple: bool,
}
pub struct TeamPower {
    pub total: i64,
    pub slots: Vec<i64>,
    pub leader_percents: Vec<Stat3>,
}
fn floor_f32_div(v: f64) -> i64 {
    f(f(v) / 10000.0).floor() as i64
}
pub fn pct_floor(base: &Stat3, pct: &Stat3) -> Stat3 {
    std::array::from_fn(|i| {
        floor_f32_div(((base[i] * pct[i]) as f64 / 10000.0_f64).trunc()).wrapping_mul(10000)
    })
}
pub fn points(bp: &Stat3) -> i64 {
    bp.iter()
        .map(|&v| (v as f64 / 10000.0).floor() as i64)
        .sum()
}
fn uniform(base: &Stat3, pct: i32) -> Stat3 {
    pct_floor(base, &[pct as i64; 3])
}
fn max_key<T>(table: Option<&BTreeMap<i32, T>>) -> i32 {
    table
        .and_then(|t| t.keys().next_back().copied())
        .unwrap_or(1)
}
pub fn member_level_cap(master: &Master, card: &MemberCard, awake: i32) -> i32 {
    card.level_limits
        .as_ref()
        .and_then(|t| t.get(&awake))
        .copied()
        .or_else(|| {
            if card.level_limits.is_none() {
                master
                    .member_level_limits
                    .get(&(card.rarity, awake))
                    .copied()
            } else {
                None
            }
        })
        .unwrap_or_else(|| max_key(master.member_levels.get(&card.level_group)))
}
pub fn snap_level_cap(master: &Master, card: &SnapCard, rank: i32) -> i32 {
    master
        .snap_ranks
        .get(&card.rank_group)
        .and_then(|r| r.get(&rank))
        .map(|r| r.level_limit)
        .unwrap_or_else(|| max_key(master.snap_levels.get(&card.level_group)))
}
fn max_skill(rows: Option<&Vec<SkillEffect>>) -> i32 {
    rows.into_iter()
        .flatten()
        .map(|r| r.level)
        .max()
        .unwrap_or(1)
        .max(1)
}
pub fn max_member_growth(master: &Master, card: &MemberCard) -> MemberGrowth {
    let awake = max_key(master.member_awake.get(&card.awake_group)).max(1);
    MemberGrowth {
        level: member_level_cap(master, card, awake),
        awake,
        rank: max_key(master.member_ranks.get(&card.rank_group)).max(1),
        live_skill_level: max_skill(master.live_skills.get(&card.live_skill_id)),
        gekiso_skill_level: max_skill(
            master
                .gekiso_skills
                .get(&card.gekiso_skill_id)
                .map(|s| &s.effects),
        ),
    }
}
pub fn max_snap_growth(master: &Master, card: &SnapCard) -> SnapGrowth {
    let rank = max_key(master.snap_ranks.get(&card.rank_group)).max(1);
    SnapGrowth {
        level: snap_level_cap(master, card, rank),
        rank,
    }
}
pub fn member_growth(
    master: &Master,
    card: &MemberCard,
    input: &MemberInput,
    maximum: bool,
) -> MemberGrowth {
    let max = max_member_growth(master, card);
    let maximum = maximum || input.maximize;
    let pick = |v: Option<i32>, high: i32| {
        v.filter(|_| !input.maximize)
            .unwrap_or(if maximum { high } else { 1 })
    };
    let awake = pick(input.awake, max.awake).min(max.awake);
    let cap = member_level_cap(master, card, awake);
    MemberGrowth {
        level: pick(input.level, cap).min(cap).max(1),
        awake,
        rank: pick(input.rank, max.rank).min(max.rank),
        live_skill_level: pick(input.live_skill_level, max.live_skill_level)
            .min(max.live_skill_level),
        gekiso_skill_level: pick(input.gekiso_skill_level, max.gekiso_skill_level)
            .min(max.gekiso_skill_level),
    }
}
pub fn snap_growth(
    master: &Master,
    card: &SnapCard,
    input: &SnapInput,
    maximum: bool,
) -> SnapGrowth {
    let max = max_snap_growth(master, card);
    let maximum = maximum || input.maximize;
    let rank = input
        .rank
        .filter(|_| !input.maximize)
        .unwrap_or(if maximum { max.rank } else { 1 })
        .min(max.rank);
    let cap = snap_level_cap(master, card, rank);
    SnapGrowth {
        level: input
            .level
            .filter(|_| !input.maximize)
            .unwrap_or(if maximum { cap } else { 1 })
            .min(cap)
            .max(1),
        rank,
    }
}
pub fn resolve_member(
    master: &Master,
    key: &str,
    card: &MemberCard,
    growth: MemberGrowth,
) -> Option<Member> {
    let level = master
        .member_levels
        .get(&card.level_group)?
        .get(&growth.level)?;
    let zero = [0; 3];
    let awake = master
        .member_awake
        .get(&card.awake_group)
        .and_then(|m| m.get(&growth.awake))
        .or(if growth.awake <= 1 { Some(&zero) } else { None })?;
    let rank = master
        .member_ranks
        .get(&card.rank_group)?
        .get(&growth.rank)?;
    let own = std::array::from_fn(|i| {
        let max = card.stat_max[i];
        let level = floor_f32_div((level[i] * max) as f64);
        let awake = f(f(awake[i] as f64 / 10000.0) * f(max as f64)).floor() as i64;
        let rank = floor_f32_div(imul(rank.rates[i] as i32, max as i32) as f64);
        (level + awake + rank) * 10000
    });
    Some(Member {
        key: key.into(),
        card: card.clone(),
        growth,
        leader_skill_level: rank.leader_skill_level,
        music_type_rate: rank.music_type_rate,
        music_tag_rate: rank.music_tag_rate,
        own,
        live_categories: master
            .live_categories
            .get(&card.live_skill_id)
            .cloned()
            .unwrap_or_default(),
        gekiso_categories: master
            .gekiso_categories
            .get(&card.gekiso_skill_id)
            .cloned()
            .unwrap_or_default(),
        mission_type: master
            .gekiso_skills
            .get(&card.gekiso_skill_id)
            .map(|s| s.mission_type)
            .unwrap_or(0),
    })
}
pub fn resolve_snap(
    master: &Master,
    key: &str,
    card: &SnapCard,
    growth: SnapGrowth,
) -> Option<Snap> {
    let level = master
        .snap_levels
        .get(&card.level_group)?
        .get(&growth.level)?;
    let rank = master.snap_ranks.get(&card.rank_group)?.get(&growth.rank)?;
    Some(Snap {
        key: key.into(),
        card: card.clone(),
        growth,
        percent: std::array::from_fn(|i| {
            floor_f32_div(imul(level[i] as i32, card.stat_max[i] as i32) as f64)
        }),
        type_link_rate: rank.type_link_rate,
        skill_levels: rank.skill_levels,
        gekiso_skill_levels: rank.gekiso_skill_levels,
    })
}
pub struct BoxCards {
    pub members: Vec<Member>,
    pub snaps: Vec<Snap>,
    pub unknown: Vec<String>,
}
pub fn resolve_box(
    master: &Master,
    members: &[MemberInput],
    snaps: &[SnapInput],
    maximum: bool,
) -> BoxCards {
    let mut out = BoxCards {
        members: Vec::new(),
        snaps: Vec::new(),
        unknown: Vec::new(),
    };
    for input in members {
        let member = master.members.get(&input.card_id).and_then(|card| {
            resolve_member(
                master,
                &input.key,
                card,
                member_growth(master, card, input, maximum),
            )
        });
        if let Some(member) = member {
            out.members.push(member);
        } else {
            out.unknown.push(input.key.clone());
        }
    }
    for input in snaps {
        let snap = master.snaps.get(&input.card_id).and_then(|card| {
            resolve_snap(
                master,
                &input.key,
                card,
                snap_growth(master, card, input, maximum),
            )
        });
        if let Some(snap) = snap {
            out.snaps.push(snap);
        } else {
            out.unknown.push(input.key.clone());
        }
    }
    out
}
pub fn is_target_member(member: &Member, target: &Target) -> bool {
    let card = &member.card;
    (target.band_id > 0 && target.band_id == card.band_id)
        || (target.card_type != 0 && target.card_type == card.card_type)
        || (target.character_id > 0 && target.character_id == card.character_id)
        || (target.tag_id > 0 && card.best_tags.contains(&target.tag_id))
        || target
            .live_categories
            .iter()
            .any(|c| member.live_categories.contains(c))
        || target
            .gekiso_categories
            .iter()
            .any(|c| member.gekiso_categories.contains(c))
        || (target.mission_type != 0 && target.mission_type == member.mission_type)
}
pub fn matches_targets(master: &Master, member: &Member, ids: &[i32]) -> bool {
    ids.iter().any(|id| {
        master
            .targets
            .get(id)
            .is_some_and(|target| is_target_member(member, target))
    })
}
fn accumulate(kind: i32, value: i32, out: &mut Stat3) {
    let components: &[usize] = match kind {
        1000 | 1500 => &[0, 1, 2],
        1001 | 1501 => &[1],
        1002 | 1502 => &[2],
        1003 | 1503 => &[0],
        _ => &[],
    };
    for &i in components {
        out[i] = (out[i] as i32).wrapping_add(value) as i64;
    }
}
fn cumulative(kind: i32) -> bool {
    kind & !3 == 1500
}
pub fn band_item_percent(master: &Master, player: &Player, member: &Member) -> Stat3 {
    let mut out = [0; 3];
    let card = &member.card;
    for (id, level) in &player.band_items {
        let Some(effects) = master
            .band_items
            .get(id)
            .and_then(|item| item.levels.get(level))
        else {
            continue;
        };
        for effect in effects {
            for id in &effect.targets {
                let Some(target) = master.targets.get(id) else {
                    continue;
                };
                let hits = i32::from(target.band_id > 0 && target.band_id == card.band_id)
                    + i32::from(
                        target.character_id > 0 && target.character_id == card.character_id,
                    )
                    + i32::from(target.card_type != 0 && target.card_type == card.card_type);
                let kind = if (1000..=1003).contains(&effect.kind) {
                    effect.kind
                } else {
                    -1
                };
                for _ in 0..hits {
                    accumulate(kind, effect.value, &mut out);
                }
            }
        }
    }
    out
}
fn condition_holds(master: &Master, group: i32, members: &[Member], music: Option<&Music>) -> bool {
    if group <= 0 {
        return true;
    }
    for set in master.condition_sets.get(&group).into_iter().flatten() {
        for id in set {
            let Some(condition) = master.conditions.get(id) else {
                continue;
            };
            if condition.kind == 0 || condition.targets.is_empty() {
                continue;
            }
            let hit = |m: &Member| matches_targets(master, m, &condition.targets);
            let holds = match condition.kind {
                3000 => members.iter().any(hit),
                3001 => members.iter().all(hit),
                4012 => music.is_some_and(|music| {
                    condition.targets.iter().any(|id| {
                        master
                            .targets
                            .get(id)
                            .is_some_and(|r| r.music_type != 0 && r.music_type == music.music_type)
                    })
                }),
                _ => true,
            };
            if !holds {
                return false;
            }
        }
    }
    true
}
fn cumulative_count(master: &Master, id: i32, members: &[Member], leader: usize) -> i32 {
    let Some(row) = master.cumulative.get(&id).filter(|_| id > 0) else {
        return 1;
    };
    let hit = |m: &Member| matches_targets(master, m, &row.targets);
    let count = match row.kind {
        3000 => members.iter().filter(|m| hit(m)).count(),
        3001 => members
            .iter()
            .enumerate()
            .filter(|(i, m)| *i != leader && hit(m))
            .count(),
        3002 => members
            .iter()
            .filter(|m| m.card.band_id == members[leader].card.band_id)
            .count(),
        3003 => members
            .iter()
            .filter(|m| m.card.band_id != members[leader].card.band_id)
            .count(),
        3004 => members
            .iter()
            .map(|m| m.card.band_id)
            .collect::<BTreeSet<_>>()
            .len(),
        3005 => members
            .iter()
            .map(|m| m.card.card_type)
            .collect::<BTreeSet<_>>()
            .len(),
        _ => return 1,
    } as i32;
    if row.cap < 1 {
        count
    } else {
        count.min(row.cap)
    }
}
pub fn leader_profile(master: &Master, member: &Member) -> LeaderProfile {
    let effects: Vec<_> = master
        .leader_skills
        .get(&member.card.leader_skill_id)
        .into_iter()
        .flatten()
        .filter(|r| r.level == member.leader_skill_level)
        .cloned()
        .collect();
    let simple = effects
        .iter()
        .all(|r| r.condition_group <= 0 && !cumulative(r.kind));
    LeaderProfile { effects, simple }
}
pub(crate) fn leader_identity(
    master: &Master,
    profile: &LeaderProfile,
    leader: &Member,
) -> Vec<Vec<i64>> {
    let mut rows: Vec<_> = profile
        .effects
        .iter()
        .map(|row| {
            let mut targets = row.targets.clone();
            targets.sort_unstable();
            let mut key = vec![
                row.kind as i64,
                row.value as i64,
                row.condition_group as i64,
                row.cumulative_id as i64,
                0,
                0,
                targets.len() as i64,
            ];
            if cumulative(row.kind) {
                if let Some(c) = master.cumulative.get(&row.cumulative_id) {
                    match c.kind {
                        3001 => key[4] = matches_targets(master, leader, &c.targets) as i64,
                        3002 | 3003 => key[5] = leader.card.band_id as i64,
                        _ => {}
                    }
                }
            }
            key.extend(targets.into_iter().map(|id| id as i64));
            key
        })
        .collect();
    rows.sort();
    rows
}
pub(crate) fn leader_replacement_equivalent(
    master: &Master,
    profile: &LeaderProfile,
    a: &Member,
    b: &Member,
) -> bool {
    profile.effects.iter().all(|row| {
        if matches_targets(master, a, &row.targets) != matches_targets(master, b, &row.targets) {
            return false;
        }
        for set in master
            .condition_sets
            .get(&row.condition_group)
            .into_iter()
            .flatten()
        {
            for id in set {
                if let Some(c) = master
                    .conditions
                    .get(id)
                    .filter(|c| c.kind == 3000 || c.kind == 3001)
                {
                    if matches_targets(master, a, &c.targets)
                        != matches_targets(master, b, &c.targets)
                    {
                        return false;
                    }
                }
            }
        }
        if cumulative(row.kind) {
            if let Some(c) = master.cumulative.get(&row.cumulative_id) {
                match c.kind {
                    3000 | 3001 => {
                        if matches_targets(master, a, &c.targets)
                            != matches_targets(master, b, &c.targets)
                        {
                            return false;
                        }
                    }
                    3002..=3004 => {
                        if a.card.band_id != b.card.band_id {
                            return false;
                        }
                    }
                    3005 if a.card.card_type != b.card.card_type => return false,
                    _ => {}
                }
            }
        }
        true
    })
}
pub fn leader_percents(
    master: &Master,
    profile: &LeaderProfile,
    members: &[Member],
    leader: usize,
    music: Option<&Music>,
) -> Vec<Stat3> {
    let mut out = vec![[0; 3]; members.len()];
    for effect in &profile.effects {
        if !condition_holds(master, effect.condition_group, members, music) {
            continue;
        }
        let value = if cumulative(effect.kind) {
            imul(
                effect.value,
                cumulative_count(master, effect.cumulative_id, members, leader),
            )
        } else {
            effect.value
        };
        for (i, member) in members.iter().enumerate() {
            if effect.targets.is_empty() || matches_targets(master, member, &effect.targets) {
                accumulate(effect.kind, value, &mut out[i]);
            }
        }
    }
    out
}
pub fn leader_percent_bound(master: &Master, profile: &LeaderProfile, member: &Member) -> Stat3 {
    let mut out = [0; 3];
    for effect in &profile.effects {
        if !effect.targets.is_empty() && !matches_targets(master, member, &effect.targets) {
            continue;
        }
        let mut value = effect.value;
        if cumulative(effect.kind) {
            let cap = master
                .cumulative
                .get(&effect.cumulative_id)
                .map(|r| r.cap)
                .unwrap_or(5);
            value = value.max(value * if cap >= 1 { cap.min(5) } else { 5 });
        }
        accumulate(
            effect.kind,
            if profile.simple { value } else { value.max(0) },
            &mut out,
        );
    }
    out
}
pub fn slot_base(
    master: &Master,
    player: &Player,
    member: &Member,
    music: Option<&Music>,
    event_pct: i32,
) -> SlotBase {
    let mut base = member.own;
    if event_pct != 0 {
        let bonus = uniform(&base, event_pct);
        for i in 0..3 {
            base[i] += bonus[i];
        }
    }
    let rank = *player
        .character_ranks
        .get(&member.card.character_id)
        .unwrap_or(&1);
    let cr = master
        .character_rank_bonus
        .iter()
        .filter(|r| r.0 <= rank)
        .map(|r| r.1)
        .next_back()
        .unwrap_or(0);
    let total = master
        .total_rank_bonus
        .iter()
        .filter(|r| r.0 <= player.total_rank)
        .map(|r| r.1)
        .next_back()
        .unwrap_or(0);
    let memory = player
        .character_memory
        .get(&member.card.character_id)
        .copied()
        .unwrap_or(0)
        + music
            .and_then(|m| player.music_memory.get(&m.memory_key).copied())
            .unwrap_or(0);
    let flat = (cr + total + memory) as i64 * 10000;
    let common = base.map(|v| v + flat);
    let mut fixed = points(&common)
        + points(&pct_floor(
            &common,
            &band_item_percent(master, player, member),
        ));
    if let Some(music) = music {
        let card = &member.card;
        if card.card_type == 99 || music.music_type == 99 || card.card_type == music.music_type {
            fixed += points(&uniform(
                &common,
                master.params.music_type_base + member.music_type_rate,
            ));
        }
        if card.best_tags.iter().any(|t| music.best_tags.contains(t)) {
            fixed += points(&uniform(
                &common,
                master.params.music_tag_base + member.music_tag_rate,
            ));
        }
    }
    let vip = master.vip_bonus.get(&player.vip_rank).copied().unwrap_or(0);
    if vip != 0 {
        fixed += points(&uniform(&common, vip));
    }
    SlotBase {
        common,
        fixed_points: fixed,
    }
}
pub fn snap_points(
    master: &Master,
    base: &SlotBase,
    member: &Member,
    snap: Option<&Snap>,
    event_pct: i32,
) -> i64 {
    let percent = snap
        .map(|s| s.percent.map(|p| p + event_pct as i64))
        .unwrap_or([0; 3]);
    let mut total = if snap.is_some() || event_pct != 0 {
        points(&pct_floor(&base.common, &percent))
    } else {
        0
    };
    if let Some(snap) = snap {
        if snap.card.card_type == member.card.card_type {
            total += points(&uniform(
                &base.common,
                master.params.type_link_base + snap.type_link_rate,
            ));
        }
    }
    total
}
pub fn leader_points(base: &SlotBase, percent: &Stat3) -> i64 {
    if percent.iter().any(|&v| v != 0) {
        points(&pct_floor(&base.common, percent))
    } else {
        0
    }
}
pub fn member_event_percent(effects: &[EventEffect], member: &Member, bonus_type: i32) -> i32 {
    let mut total = 0i32;
    let card = &member.card;
    for effect in effects {
        if effect.bonus_type != bonus_type
            || effect.resource_type != 2
            || (effect.character_id > 0 && effect.character_id != card.character_id)
            || (effect.band_id > 0 && effect.band_id != card.band_id)
            || (effect.card_type != 0 && effect.card_type != card.card_type)
            || (effect.tag_id > 0 && !card.best_tags.contains(&effect.tag_id))
            || (effect.member_id >= 1 && effect.member_id != card.id)
        {
            continue;
        }
        total = total.wrapping_add(
            effect
                .per_rank
                .get((member.growth.rank - 1) as usize)
                .copied()
                .unwrap_or(0),
        );
    }
    total
}
pub fn snap_event_percent(effects: &[EventEffect], snap: &Snap, bonus_type: i32) -> i32 {
    let mut total = 0i32;
    let card = &snap.card;
    for effect in effects {
        if effect.bonus_type != bonus_type
            || effect.resource_type != 3
            || (effect.character_id > 0 && !card.character_ids.contains(&effect.character_id))
            || (effect.band_id > 0 && !card.band_ids.contains(&effect.band_id))
            || (effect.card_type != 0 && effect.card_type != card.card_type)
            || effect.tag_id >= 1
            || (effect.snap_id >= 1 && effect.snap_id != card.id)
        {
            continue;
        }
        total = total.wrapping_add(
            effect
                .per_rank
                .get((snap.growth.rank - 1) as usize)
                .copied()
                .unwrap_or(0),
        );
    }
    total
}
pub fn team_power(
    master: &Master,
    player: &Player,
    members: &[Member],
    snaps: &[Option<Snap>],
    leader: usize,
    music: Option<&Music>,
    effects: &[EventEffect],
) -> Result<TeamPower, String> {
    if leader >= members.len() {
        return Err("invalid leader".into());
    }
    let percents = leader_percents(
        master,
        &leader_profile(master, &members[leader]),
        members,
        leader,
        music,
    );
    let slots: Vec<_> = members
        .iter()
        .enumerate()
        .map(|(i, member)| {
            let base = slot_base(
                master,
                player,
                member,
                music,
                member_event_percent(effects, member, 2),
            );
            let snap = snaps.get(i).and_then(Option::as_ref);
            let event = snap.map(|s| snap_event_percent(effects, s, 2)).unwrap_or(0);
            base.fixed_points
                + snap_points(master, &base, member, snap, event)
                + leader_points(&base, &percents[i])
        })
        .collect();
    Ok(TeamPower {
        total: slots.iter().sum(),
        slots,
        leader_percents: percents,
    })
}

impl Default for Player {
    fn default() -> Self {
        Self {
            character_ranks: BTreeMap::new(),
            total_rank: 0,
            band_items: BTreeMap::new(),
            vip_rank: 1,
            character_memory: BTreeMap::new(),
            music_memory: BTreeMap::new(),
        }
    }
}
pub fn player_state(master: &Master, input: &serde_json::Value) -> Player {
    fn non_zero(v: &serde_json::Value) -> BTreeMap<i32, i32> {
        v.as_object()
            .into_iter()
            .flat_map(|m| m.iter())
            .filter_map(|(k, v)| {
                let value = int(v);
                if value == 0 {
                    None
                } else {
                    k.parse().ok().map(|k| (k, value))
                }
            })
            .collect()
    }
    let ranks = non_zero(&input["characterRanks"]);
    let total = if input["characterTotalRank"].is_null() {
        master
            .characters
            .keys()
            .map(|id| ranks.get(id).copied().unwrap_or(1))
            .sum()
    } else {
        int(&input["characterTotalRank"])
    };
    Player {
        character_ranks: ranks,
        total_rank: total,
        band_items: non_zero(&input["bandItems"]),
        vip_rank: if input["vipRank"].is_null() {
            1
        } else {
            int(&input["vipRank"])
        },
        character_memory: non_zero(&input["characterMemory"]),
        music_memory: non_zero(&input["musicMemory"]),
    }
}
pub fn music_view(
    master: &Master,
    song_id: i32,
    challenge_event: Option<i32>,
) -> Result<Music, String> {
    let song = master
        .songs
        .get(&song_id)
        .ok_or_else(|| format!("unknown-song:{song_id}"))?;
    let mut music_type = song.music_type;
    if let Some(event) = challenge_event {
        if let Some(challenge) = master
            .challenge_music
            .iter()
            .find(|r| r.event_id == event && r.music_id == song_id)
        {
            if challenge.music_type != 0 {
                music_type = challenge.music_type;
            }
        }
    }
    Ok(Music {
        music_type,
        best_tags: song.best_tags.clone(),
        memory_key: song_id,
    })
}
