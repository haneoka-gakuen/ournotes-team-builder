//! Resolve normal-live effects and snap support conditions before scoring.
use crate::{
    live::{Conversion, Effect, Gate, Slot},
    master::{Condition, Master, SkillEffect},
    num::{f, judgement_delta, score_delta},
    power::{matches_targets, Member, Snap},
};
mod prepared;
pub(crate) use prepared::{prepare, Prepared};

enum Value {
    Static(bool),
    Life,
}
fn condition(master: &Master, row: &Condition, member: &Member) -> Option<Value> {
    match row.kind {
        5000 => {
            let value = matches_targets(master, member, &row.targets);
            Some(Value::Static(if row.positive { value } else { !value }))
        }
        4010 | 5020 => {
            if row.positive {
                Some(Value::Static(true))
            } else {
                None
            }
        }
        2001 => Some(Value::Life),
        _ => None,
    }
}
fn group<'a>(
    master: &'a Master,
    id: i32,
    member: &Member,
) -> Option<(bool, Option<&'a Condition>)> {
    if id <= 0 {
        return Some((true, None));
    }
    let sets = master.condition_sets.get(&id)?;
    if sets.is_empty() {
        return None;
    }
    let mut any = false;
    let mut life = None;
    for set in sets {
        let mut all = true;
        for id in set {
            let row = master.conditions.get(id)?;
            match condition(master, row, member)? {
                Value::Life => life = Some(row),
                Value::Static(false) => all = false,
                Value::Static(true) => {}
            }
        }
        if all {
            any = true;
        }
    }
    Some((any, life))
}
fn judgements(master: &Master, ids: &[i32]) -> Vec<usize> {
    ids.iter()
        .filter_map(|id| master.targets.get(id))
        .map(|r| r.judgement)
        .filter(|j| (3..=6).contains(j))
        .map(|j| j as usize)
        .collect()
}
fn at_level(rows: Option<&Vec<SkillEffect>>, level: i32) -> impl Iterator<Item = &SkillEffect> {
    rows.into_iter().flatten().filter(move |r| r.level == level)
}
pub fn resolve(
    master: &Master,
    member: &Member,
    snap: Option<&Snap>,
    unsupported: &mut Vec<String>,
) -> Slot {
    let mut effects = Vec::new();
    for row in at_level(
        master.live_skills.get(&member.card.live_skill_id),
        member.growth.live_skill_level,
    ) {
        if row.kind != 2000 && row.kind != 2004 {
            unsupported.push(format!("live:{}:{}", member.card.live_skill_id, row.kind));
            continue;
        }
        let Some((holds, life)) = group(master, row.condition_group, member) else {
            unsupported.push(format!("live-condition:{}", row.condition_group));
            continue;
        };
        if !holds {
            continue;
        }
        effects.push(Effect {
            kind: row.kind,
            delta: if row.kind == 2000 {
                score_delta(row.value)
            } else {
                judgement_delta(row.value)
            },
            seconds: f(row.seconds),
            gate: life
                .map(|r| Gate {
                    kind: 1,
                    value: r.values.first().copied().unwrap_or(0.0),
                    positive: r.positive,
                })
                .unwrap_or(Gate {
                    kind: 0,
                    value: 0.0,
                    positive: true,
                }),
            judgements: if row.kind == 2004 {
                judgements(master, &row.targets)
            } else {
                Vec::new()
            },
        });
    }
    let mut extension_ms = f(0.0);
    let mut recovery = 0;
    let mut convert = None;
    if let Some(snap) = snap {
        for (slot, &id) in snap.card.support_skills.iter().enumerate() {
            if id == 0 {
                continue;
            }
            for row in at_level(master.support_skills.get(&id), snap.skill_levels[slot]) {
                let enabled = group(master, row.condition_group, member);
                let trigger = group(master, row.trigger_group, member);
                let (Some((enabled, _)), Some(_)) = (enabled, trigger) else {
                    unsupported.push(format!("support-condition:{id}"));
                    continue;
                };
                if !enabled {
                    continue;
                }
                match row.kind {
                    15000 => extension_ms = f(extension_ms + f(row.value as f64)),
                    3001 => recovery += row.value,
                    12006 => {
                        convert = Some(Conversion {
                            judgements: judgements(master, &row.targets),
                            limit: if row.limit_count != 0 {
                                Some(row.limit_count as f64)
                            } else {
                                None
                            },
                        })
                    }
                    3003 => {}
                    kind => unsupported.push(format!("support:{id}:{kind}")),
                }
            }
        }
    }
    Slot {
        effects,
        extension_ms,
        recovery,
        convert,
    }
}
