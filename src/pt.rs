//! Strict AP reward qualification. Every practice/bonus input is explicit.
use crate::{
    chart::Source,
    master::{self, Master, SkillEffect},
    power::{self, MemberInput, SnapInput},
    skills,
};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
fn integer(v: &Value, min: i64) -> bool {
    v.as_i64().is_some_and(|n| n >= min && n <= i32::MAX as i64)
}
fn whole(n: f64, min: f64) -> bool {
    n.is_finite() && n.fract() == 0.0 && n >= min && n <= i32::MAX as f64
}
fn keys(v: &Value) -> BTreeSet<String> {
    master::rows(v)
        .iter()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect()
}
fn add(issues: &mut Vec<String>, code: &str, target: impl AsRef<str>) {
    let key = format!("{code}:{}", target.as_ref());
    if !issues.contains(&key) {
        issues.push(key)
    }
}
fn gekiso_effects(
    master: &Master,
    effects: &[SkillEffect],
    target: &str,
    issues: &mut Vec<String>,
) {
    for effect in effects {
        if ![1, 2].contains(&effect.trigger_type)
            || !effect.seconds.is_finite()
            || effect
                .targets
                .iter()
                .any(|id| !master.targets.contains_key(id))
        {
            add(issues, "skill", target)
        }
        for group in [
            effect.trigger_group,
            effect.condition_group,
            effect.release_group,
            effect.reset_group,
        ] {
            if group == 0 {
                continue;
            }
            let sets = master.condition_sets.get(&group);
            if sets.is_none_or(|sets| {
                sets.is_empty()
                    || sets.iter().flatten().any(|id| {
                        master.conditions.get(id).is_none_or(|c| {
                            c.targets.iter().any(|id| !master.targets.contains_key(id))
                        })
                    })
            }) {
                add(issues, "skill", format!("{target}:condition:{group}"));
            }
        }
        if effect.cumulative_id != 0
            && master
                .cumulative
                .get(&effect.cumulative_id)
                .is_none_or(|r| r.targets.iter().any(|id| !master.targets.contains_key(id)))
        {
            add(issues, "skill", format!("{target}:cumulative"))
        }
    }
}
pub fn validate(master: &Master, request: &Value) -> Result<Vec<String>, String> {
    if request["songRules"].is_object() {
        let scoped = crate::song_rules::view(master, &request["songRules"])?;
        let mut input = request.clone(); input["songRules"] = Value::Null;
        return validate(&scoped,&input);
    }

    let goal = &request["goal"];
    if !goal["rewardPlan"].is_null() {
        use crate::reward_plan::{Metric, Plan};
        let plan = Plan::parse(&goal["rewardPlan"])?;
        let mut issues = Vec::new();
        for metric in [Metric::Points, Metric::Items] {
            if !plan.uses(metric) && metric != plan.anchor() { continue; }
            let mut checked = request.clone();
            checked["goal"].as_object_mut().unwrap().remove("rewardPlan");
            checked["goal"]["measure"] = serde_json::json!(if metric == Metric::Points { "points" } else { "items" });
            checked["k"] = serde_json::json!(1);
            if plan.uses(Metric::ChallengePoints) && goal["route"] == "live" {
                checked["goal"]["cpExchange"] = serde_json::json!({"numerator":0,"denominator":1});
            }
            for issue in validate(master, &checked)? { if !issues.contains(&issue) { issues.push(issue); } }
        }
        return Ok(issues);
    }
    let mut issues = Vec::new();
    if goal["kind"] != "event"
        || goal["route"] == "skip"
        || !["points", "items"].contains(&goal["measure"].as_str().unwrap_or(""))
        || goal["ranking"] != "pt-only"
        || ["great", "good", "bad", "miss"]
            .iter()
            .any(|k| master::number(&goal["play"][k], 0.0) != 0.0)
        || master::number(&request["k"], 1.0) != 1.0
    {
        return Ok(vec!["mode:solo-ap-rewards".into()]);
    }
    let gk = &goal["gekiso"];
    let is_gk = !gk.is_null();
    let challenge = goal["route"] == "challenge";
    let items = goal["measure"] == "items";
    let cp = &goal["cpExchange"];
    let has_cp = !cp.is_null();
    if is_gk
        && (goal["route"] != "live"
            || !gk["just"]
                .as_f64()
                .is_some_and(|n| (0.0..=1.0).contains(&n))
            || master::number(&gk["luckSamples"], 0.0) < 1.0)
    {
        add(&mut issues, "mode", "gekiso-input")
    }
    if has_cp
        && (goal["route"] != "live"
            || master::number(&cp["numerator"], -1.0) < 0.0
            || master::number(&cp["denominator"], 0.0) < 1.0)
    {
        add(&mut issues, "rules", "cp-value")
    }
    let songs = master::rows(&goal["songs"]);
    if songs.is_empty() {
        add(
            &mut issues,
            "chart",
            if challenge {
                "challenge-selection"
            } else {
                "selection"
            },
        )
    }
    let member_inputs: Vec<MemberInput> =
        serde_json::from_value(request["members"].clone()).map_err(|e| e.to_string())?;
    let snap_inputs: Vec<SnapInput> =
        serde_json::from_value(request["snaps"].clone()).map_err(|e| e.to_string())?;
    if member_inputs
        .iter()
        .map(|m| &m.key)
        .collect::<BTreeSet<_>>()
        .len()
        != member_inputs.len()
        || member_inputs
            .iter()
            .map(|m| m.card_id)
            .collect::<BTreeSet<_>>()
            .len()
            != member_inputs.len()
    {
        add(&mut issues, "inventory", "member")
    }
    if snap_inputs
        .iter()
        .map(|s| &s.key)
        .collect::<BTreeSet<_>>()
        .len()
        != snap_inputs.len()
        || snap_inputs
            .iter()
            .map(|s| s.card_id)
            .collect::<BTreeSet<_>>()
            .len()
            != snap_inputs.len()
    {
        add(&mut issues, "inventory", "snap")
    }
    for row in &member_inputs {
        let Some(card) = master.members.get(&row.card_id) else {
            add(&mut issues, "inventory", &row.key);
            continue;
        };
        let growth = power::member_growth(master, card, row, false);
        for (field, entered, value) in [
            ("level", row.level, growth.level),
            ("awake", row.awake, growth.awake),
            ("rank", row.rank, growth.rank),
            (
                "liveSkillLevel",
                row.live_skill_level,
                growth.live_skill_level,
            ),
        ] {
            if entered.is_none_or(|n| n < 1 || n != value) {
                add(&mut issues, "practice", format!("{}.{field}", row.key))
            }
        }
        if is_gk
            && card.gekiso_skill_id != 0
            && row
                .gekiso_skill_level
                .is_none_or(|n| n < 1 || n != growth.gekiso_skill_level)
        {
            add(
                &mut issues,
                "practice",
                format!("{}.gekisoSkillLevel", row.key),
            )
        }
    }
    for row in &snap_inputs {
        let Some(card) = master.snaps.get(&row.card_id) else {
            add(&mut issues, "inventory", &row.key);
            continue;
        };
        let growth = power::snap_growth(master, card, row, false);
        for (field, entered, value) in [
            ("level", row.level, growth.level),
            ("rank", row.rank, growth.rank),
        ] {
            if entered.is_none_or(|n| n < 1 || n != value) {
                add(&mut issues, "practice", format!("{}.{field}", row.key))
            }
        }
    }
    let box_ = power::resolve_box(master, &member_inputs, &snap_inputs, false);
    for key in &box_.unknown {
        add(&mut issues, "inventory", key)
    }
    let members: BTreeMap<_, _> = box_.members.iter().map(|m| (m.key.clone(), m)).collect();
    let snaps: BTreeMap<_, _> = box_.snaps.iter().map(|s| (s.key.clone(), s)).collect();
    let c = &request["constraints"];
    let excluded = keys(&c["excludedMembers"]);
    let excluded_snaps = keys(&c["excludedSnaps"]);
    let usable: Vec<_> = box_
        .members
        .iter()
        .filter(|m| !excluded.contains(&m.key))
        .collect();
    if usable
        .iter()
        .map(|m| m.card.character_id)
        .collect::<BTreeSet<_>>()
        .len()
        < 5
    {
        add(&mut issues, "inventory", "five-characters")
    }
    let bindings = master::rows(&c["bindings"]);
    let mut required = keys(&c["requiredMembers"]);
    required.extend(
        bindings
            .iter()
            .filter_map(|b| b[0].as_str().map(str::to_owned)),
    );
    if let Some(key) = c["leader"].as_str() {
        required.insert(key.into());
    }
    for key in required.iter().chain(&excluded) {
        if !members.contains_key(key) || (required.contains(key) && excluded.contains(key)) {
            add(&mut issues, "constraint", key)
        }
    }
    if required.len() > 5
        || required
            .iter()
            .map(|k| members.get(k).map(|m| m.card.character_id))
            .collect::<BTreeSet<_>>()
            .len()
            != required.len()
    {
        add(&mut issues, "constraint", "required-members")
    }
    let bound: Vec<_> = bindings.iter().filter_map(|b| b[1].as_str()).collect();
    let mut required_snaps = keys(&c["requiredSnaps"]);
    required_snaps.extend(bound.iter().map(|s| s.to_string()));
    if required_snaps.len() > 5
        || bound.iter().collect::<BTreeSet<_>>().len() != bound.len()
        || bindings
            .iter()
            .map(|b| b[0].as_str())
            .collect::<BTreeSet<_>>()
            .len()
            != bindings.len()
    {
        add(&mut issues, "constraint", "photo-bindings")
    }
    for key in required_snaps.iter().chain(&excluded_snaps) {
        if !snaps.contains_key(key)
            || (required_snaps.contains(key)
                && (c["noSnaps"] == true || excluded_snaps.contains(key)))
        {
            add(&mut issues, "constraint", key)
        }
    }
    if !c["minBonusPercent"].is_null()
        && !c["minBonusPercent"]
            .as_f64()
            .is_some_and(|n| n.is_finite() && n >= 0.0)
    {
        add(&mut issues, "constraint", "minBonus")
    }
    let player = &request["player"];
    for id in usable
        .iter()
        .map(|m| m.card.character_id)
        .collect::<BTreeSet<_>>()
    {
        let key = id.to_string();
        if !integer(&player["characterRanks"][&key], 1)
            || master::int(&player["characterRanks"][&key]) > master.max_character_rank
        {
            add(&mut issues, "player", format!("cr.{id}"))
        }
        if !integer(&player["characterMemory"][&key], 0) {
            add(&mut issues, "player", format!("cm.{id}"))
        }
    }
    if !integer(&player["vipRank"], 1)
        || master.vip_bonus.is_empty()
        || (master::int(&player["vipRank"]) != 1
            && !master
                .vip_bonus
                .contains_key(&master::int(&player["vipRank"])))
    {
        add(&mut issues, "player", "p.vip")
    }
    for (id, item) in master.band_items.iter() {
        let level = &player["bandItems"][id.to_string()];
        if !integer(level, 0)
            || (master::int(level) != 0 && !item.levels.contains_key(&master::int(level)))
        {
            add(&mut issues, "player", format!("bi.{id}"))
        }
    }
    let event_id = master::int(&goal["eventId"]);
    let manual = if goal["eventEffects"].is_null() {
        None
    } else {
        Some(crate::event::resolve(master, goal)?)
    };
    let event = manual.as_deref().or_else(|| master.events.get(&event_id));
    let points = event.map(|e| {
        if challenge {
            &e.challenge_points
        } else {
            &e.live_points
        }
    });
    let rewards: Vec<_> = event
        .map(|e| {
            if challenge {
                &e.challenge_rewards
            } else {
                &e.live_rewards
            }
        })
        .into_iter()
        .flatten()
        .filter(|r| event.is_some_and(|e| r.resource_id == e.item_id))
        .collect();
    let consumption = master::int(&goal["consumption"]);
    let boost = if challenge {
        master
            .challenge_boosts
            .iter()
            .find(|b| b.consumed == consumption)
            .cloned()
    } else if ![0, 1, 2, 3, 4, 5, 10].contains(&consumption) {
        None
    } else if consumption == 0 {
        Some(master::Boost {
            consumed: 0,
            event_point_rate: 1.0,
            reward_rate: 1.0,
        })
    } else {
        master
            .boosts
            .iter()
            .find(|b| b.consumed == consumption)
            .cloned()
    };
    if consumption < if challenge { 1 } else { 0 }
        || boost.as_ref().is_none_or(|b| {
            !whole(b.event_point_rate, 1.0) || (items && !whole(b.reward_rate, 1.0))
        })
    {
        add(
            &mut issues,
            "event",
            if challenge {
                "challenge-consumption"
            } else {
                "consumption"
            },
        )
    }
    if !items && points.is_none_or(|p| p.is_empty()) {
        add(&mut issues, "event", "tables")
    }
    if items
        && (event.is_none_or(|e| e.item_id < 1)
            || rewards.is_empty()
            || rewards
                .iter()
                .map(|r| r.resource_type)
                .collect::<BTreeSet<_>>()
                .len()
                != 1
            || rewards
                .iter()
                .any(|r| r.probability != 10000.0 || r.count < 0))
    {
        add(&mut issues, "event", "guaranteed-medal-rewards")
    }
    let counts = |rank: i32| {
        let mut rows: Vec<_> = rewards
            .iter()
            .filter(|r| r.score_rank == rank)
            .map(|r| r.count)
            .collect();
        rows.sort_unstable();
        rows
    };
    let mut seen = BTreeSet::new();
    for song in songs {
        let id = master::int(&song["songId"]);
        let difficulty = master::int(&song["difficulty"]);
        let key = format!("{id}:{difficulty}");
        let target = if challenge {
            format!("challenge:{key}")
        } else {
            key.clone()
        };
        if !seen.insert(key.clone()) {
            add(&mut issues, "chart", &target)
        }
        let row = master.songs.get(&id);
        let diff = row.and_then(|s| s.difficulties.iter().find(|d| d.difficulty == difficulty));
        if diff.is_none_or(|d| d.file.is_empty() || d.note_count < 1 || !whole(d.play_level, 1.0)) {
            add(&mut issues, "chart", &target)
        }
        if challenge
            && !master
                .challenge_music
                .iter()
                .any(|s| s.event_id == event_id && s.music_id == id)
        {
            add(&mut issues, "chart", format!("challenge:{key}"))
        }
        let ranks = row.and_then(|s| master.score_ranks.get(&s.rank_group));
        if is_gk {
            if let Some(s) = row {
                let missions = &s.missions;
                if missions.len() != 3 || missions.iter().any(|&m| !(1..=3).contains(&m)) {
                    add(&mut issues, "chart", format!("gekiso:{key}"))
                } else {
                    let pattern = crate::full::gekisou::mission_pattern(
                        missions[0],
                        missions[1],
                        missions[2],
                    );
                    if (1..=3).any(|count| {
                        !master
                            .live
                            .gekiso
                            .ranks
                            .iter()
                            .any(|r| r[0] == pattern && r[1] == count && r[2] == 1 && r[3] >= 0)
                    }) {
                        add(&mut issues, "rules", "gekiso-rank-bonus")
                    }
                }
                if missions.contains(&3)
                    && (master.live.just_types.is_empty()
                        || !master.live.judge_percent.contains_key(&1))
                {
                    add(&mut issues, "rules", "gekiso-just")
                }
            }
        }
        if ranks.is_none_or(|r| r.is_empty() || r.iter().any(|r| r.required < 0)) {
            add(&mut issues, "chart", &target)
        }
        let rank_ids: BTreeSet<_> = std::iter::once(2)
            .chain(ranks.into_iter().flatten().map(|r| r.rank))
            .collect();
        for rank in rank_ids {
            if if items {
                counts(rank).is_empty()
            } else {
                points.and_then(|p| p.get(&rank)).is_none_or(|&p| p < 0)
            } {
                add(&mut issues, "event", format!("{event_id}:{rank}"))
            }
            if has_cp
                && master
                    .live_challenge_points
                    .get(&rank)
                    .is_none_or(|&p| p < 0)
            {
                add(&mut issues, "event", format!("cp:{rank}"))
            }
        }
        let mut ordered: Vec<_> = std::iter::once((2, 0))
            .chain(
                ranks
                    .into_iter()
                    .flatten()
                    .filter(|r| r.rank != 2)
                    .map(|r| (r.rank, r.required)),
            )
            .collect();
        ordered.sort_by_key(|r| r.0);
        let monotone = ordered.windows(2).all(|w| {
            let (a, b) = (w[0], w[1]);
            let (before, now) = (counts(a.0), counts(b.0));
            b.1 >= a.1
                && (if items {
                    now.len() == before.len() && now.iter().zip(before).all(|(&a, b)| a >= b)
                } else {
                    points.and_then(|p| p.get(&b.0)).copied().unwrap_or(0)
                        >= points.and_then(|p| p.get(&a.0)).copied().unwrap_or(0)
                })
                && (!has_cp
                    || master.live_challenge_points.get(&b.0).copied().unwrap_or(0)
                        >= master.live_challenge_points.get(&a.0).copied().unwrap_or(0))
        });
        if !monotone {
            add(&mut issues, "rules", "rank-monotonicity")
        }
        if !integer(&player["musicMemory"][id.to_string()], 0) {
            add(&mut issues, "player", format!("mm.{id}"))
        }
    }
    if let (Some(event), Some(boost)) = (event, boost) {
        if event
            .effects
            .iter()
            .any(|e| e.per_rank.iter().any(|&v| v < 0))
        {
            add(&mut issues, "rules", "event-bonus")
        }
        let top = |mut values: Vec<i32>| {
            values.sort_by(|a, b| b.cmp(a));
            values.into_iter().take(5).map(|v| v as f64).sum::<f64>()
        };
        let bonus = top(box_
            .members
            .iter()
            .map(|m| power::member_event_percent(&event.effects, m, if items { 1 } else { 0 }))
            .collect())
            + top(box_
                .snaps
                .iter()
                .map(|s| power::snap_event_percent(&event.effects, s, if items { 1 } else { 0 }))
                .collect());
        let max_base = if items {
            rewards.iter().map(|r| r.count).max().unwrap_or(0)
        } else {
            points
                .into_iter()
                .flat_map(|p| p.values())
                .copied()
                .max()
                .unwrap_or(0)
        }
        .max(0) as f64;
        let rate = if items {
            boost.reward_rate
        } else {
            boost.event_point_rate
        };
        // Settlement widens the product before dividing and caps the final row
        // at i32::MAX. Only an overflowing i64 product invalidates monotonicity.
        let product = (bonus as i128 + 10000)
            * crate::num::i32(rate) as i128
            * max_base as i128;
        if bonus > 1000000.0 || product > i64::MAX as i128 {
            add(&mut issues, "rules", "settlement-range")
        }
        if has_cp {
            let cp_value = master
                .live_challenge_points
                .values()
                .copied()
                .max()
                .unwrap_or(0)
                .max(0) as f64
                * boost.event_point_rate;
            let payout =
                (max_base * rate * 101.0).ceil() * if items { rewards.len() as f64 } else { 1.0 };
            let sum = 120.0
                * master::number(&gk["luckSamples"], 1.0)
                * (payout * master::number(&cp["denominator"], 0.0)
                    + cp_value * master::number(&cp["numerator"], 0.0));
            if !whole(cp_value, 0.0) || !sum.is_finite() || sum > 9007199254740991.0 / 2.0 {
                add(&mut issues, "rules", "cp-settlement-range")
            }
        }
    }
    for member in usable {
        let target = &member.key;
        if is_gk {
            let rows: Vec<_> = master
                .gekiso_skills
                .get(&member.card.gekiso_skill_id)
                .into_iter()
                .flat_map(|s| &s.effects)
                .filter(|r| r.level == member.growth.gekiso_skill_level)
                .cloned()
                .collect();
            if member.card.gekiso_skill_id != 0 && rows.is_empty() {
                add(&mut issues, "skill", format!("{target}.gekiso"))
            }
            gekiso_effects(master, &rows, &format!("{target}.gekiso"), &mut issues)
        }
        if member.card.live_skill_id != 0
            && master
                .live_skills
                .get(&member.card.live_skill_id)
                .is_none_or(|r| !r.iter().any(|r| r.level == member.growth.live_skill_level))
        {
            add(&mut issues, "skill", target)
        }
        let leaders = master.leader_skills.get(&member.card.leader_skill_id);
        if member.card.leader_skill_id != 0
            && leaders.is_none_or(|r| !r.iter().any(|r| r.level == member.leader_skill_level))
        {
            add(&mut issues, "skill", format!("{target}.leader"))
        }
        for effect in leaders
            .into_iter()
            .flatten()
            .filter(|r| r.level == member.leader_skill_level)
        {
            if ![1000, 1001, 1002, 1003, 1500, 1501, 1502, 1503].contains(&effect.kind)
                || effect.value < 0
                || effect
                    .targets
                    .iter()
                    .any(|id| !master.targets.contains_key(id))
            {
                add(&mut issues, "skill", format!("{target}.leader"))
            }
            if effect.condition_group > 0
                && master
                    .condition_sets
                    .get(&effect.condition_group)
                    .is_none_or(|sets| {
                        sets.is_empty()
                            || sets.iter().flatten().any(|id| {
                                master.conditions.get(id).is_none_or(|r| {
                                    ![0, 3000, 3001, 4012].contains(&r.kind)
                                        || r.targets
                                            .iter()
                                            .any(|id| !master.targets.contains_key(id))
                                })
                            })
                    })
            {
                add(&mut issues, "skill", format!("{target}.leader-condition"))
            }
            if effect.cumulative_id > 0
                && master
                    .cumulative
                    .get(&effect.cumulative_id)
                    .is_none_or(|r| {
                        !(3000..=3005).contains(&r.kind)
                            || r.targets.iter().any(|id| !master.targets.contains_key(id))
                    })
            {
                add(&mut issues, "skill", format!("{target}.leader-cumulative"))
            }
        }
        for photo in std::iter::once(None).chain(
            box_.snaps
                .iter()
                .filter(|s| c["noSnaps"] != true && !excluded_snaps.contains(&s.key))
                .map(Some),
        ) {
            let mut unsupported = Vec::new();
            let slot = skills::resolve(master, member, photo, &mut unsupported);
            if is_gk
                && slot
                    .convert
                    .as_ref()
                    .is_some_and(|c| c.judgements.contains(&6))
            {
                add(&mut issues, "skill", format!("{target}:just-to-perfect"))
            }
            for why in unsupported {
                add(
                    &mut issues,
                    "skill",
                    format!(
                        "{target}{}:{why}",
                        photo.map(|s| format!("/{}", s.key)).unwrap_or_default()
                    ),
                )
            }
            let Some(photo) = photo else {
                continue;
            };
            for (slot, &id) in photo.card.support_skills.iter().enumerate() {
                if id != 0
                    && master
                        .support_skills
                        .get(&id)
                        .is_none_or(|r| !r.iter().any(|r| r.level == photo.skill_levels[slot]))
                {
                    add(&mut issues, "skill", format!("{}:{id}", photo.key))
                }
            }
            if is_gk {
                for (slot, &id) in photo.card.gekiso_support_skills.iter().enumerate() {
                    let rows: Vec<_> = master
                        .gekiso_support_skills
                        .get(&id)
                        .into_iter()
                        .flat_map(|s| &s.effects)
                        .filter(|r| r.level == photo.gekiso_skill_levels[slot])
                        .cloned()
                        .collect();
                    let target = format!("{}.gekiso:{id}", photo.key);
                    if id != 0 && rows.is_empty() {
                        add(&mut issues, "skill", &target)
                    }
                    gekiso_effects(master, &rows, &target, &mut issues)
                }
            }
        }
    }
    for (id, item) in master.band_items.iter() {
        let level = master::int(&player["bandItems"][id.to_string()]);
        if level == 0 {
            continue;
        }
        for effect in item.levels.get(&level).into_iter().flatten() {
            if !(1000..=1003).contains(&effect.kind)
                || effect.condition_group > 0
                || effect.cumulative_id > 0
                || effect.value < 0
                || effect
                    .targets
                    .iter()
                    .any(|id| !master.targets.contains_key(id))
            {
                add(&mut issues, "skill", format!("bi.{id}"))
            }
        }
    }
    Ok(issues)
}
pub fn validate_chart(
    master: &Master,
    song_id: i32,
    difficulty: i32,
    source: &Source,
) -> Result<(), String> {
    let judged: Vec<_> = source.notes.iter().filter(|n| n.judged).collect();
    let ordered = judged.iter().enumerate().all(|(i, n)| {
        n.time_ms >= 0
            && (i == 0 || n.time_ms >= judged[i - 1].time_ms)
            && master.live.note_percent.contains_key(&n.operate_type)
            && master.live.timing_types.contains(&n.judgement_type)
    });
    if source.duration_ms <= 0
        || judged.is_empty()
        || source.skill_times_ms.len() != 5
        || source.skill_times_ms.iter().any(|&t| t < 0)
        || !ordered
        || !master.live.adjustment.is_finite()
        || master.live.adjustment <= 0.0
        || !master.live.judge_percent.contains_key(&2)
    {
        return Err(format!("pt-chart-unsupported:{song_id}:{difficulty}"));
    }
    Ok(())
}
