//! Pair-aware skill-role limits, prepared only for constrained searches.
use crate::{
    full::conditions::{performer_matches, Performer},
    master::{Master, SkillEffect},
    power::{Member, Snap},
};
use serde_json::Value;
#[derive(Clone)]
pub struct Limits {
    pub masks: Vec<Vec<u8>>,
    pub bounds: Vec<(u8, u8, u8)>,
}
fn possible(master: &Master, group: i32, actor: &Performer) -> bool {
    group == 0
        || master.condition_sets.get(&group).is_some_and(|sets| {
            sets.iter().any(|set| {
                set.iter().all(|id| {
                    master.conditions.get(id).is_none_or(|condition| {
                        condition.kind != 5000
                            || condition.targets.iter().any(|id| {
                                master
                                    .targets
                                    .get(id)
                                    .is_some_and(|target| performer_matches(actor, target))
                            }) == condition.positive
                    })
                })
            })
        })
}
fn roles(
    master: &Master,
    rows: &[SkillEffect],
    level: i32,
    actor: &Performer,
    missions: &[i32],
) -> u8 {
    rows.iter()
        .filter(|row| {
            row.level == level
                && possible(master, row.condition_group, actor)
                && possible(master, row.trigger_group, actor)
        })
        .filter(|row| {
            let mission = match row.kind {
                11000..=11005 => 2,
                12000..=12006 => 1,
                13000..=13005 | 4004 => 3,
                _ => 0,
            };
            mission == 0 || missions.contains(&mission)
        })
        .fold(0, |mask, row| {
            mask | match row.kind {
                11000..=11005 | 12000 | 12002 | 12003 | 13000 | 13002 | 13003 | 13004 => 1,
                4004 => 2,
                12006 | 13005 => 4,
                2000 | 2001 | 2004 => 8,
                _ => 0,
            }
        })
}
impl Limits {
    pub fn prepare(
        master: &Master,
        input: &Value,
        members: &[Member],
        snaps: &[Snap],
        selected_missions: &Value,
    ) -> Option<Self> {
        let mut bounds = Vec::new();
        for (kind, offset) in [("member", 0), ("snap", 4)] {
            for (role, bit) in [("count", 0), ("window", 1), ("conversion", 2), ("score", 3)] {
                let range = &input[kind][role];
                let min = range[0].as_u64().unwrap_or(0).min(6) as u8;
                let max = range[1].as_u64().unwrap_or(5).min(5) as u8;
                if min > 0 || max < 5 {
                    bounds.push((1 << (bit + offset), min, max));
                }
            }
        }
        if bounds.is_empty() {
            return None;
        }
        let missions = if selected_missions.is_array() {
            crate::master::integers(selected_missions)
        } else {
            vec![1, 2, 3]
        };
        let masks = members
            .iter()
            .map(|member| {
                (0..=snaps.len())
                    .map(|index| {
                        let snap = snaps.get(index);
                        let actor = Performer::from_member(master, member, snap);
                        let member_roles = actor
                            .gekisou_skill
                            .and_then(|(id, level)| {
                                master.gekiso_skills.get(&id).map(|skill| {
                                    if skill.mission_type == 4
                                        || missions.contains(&skill.mission_type)
                                    {
                                        roles(master, &skill.effects, level, &actor, &missions)
                                    } else {
                                        0
                                    }
                                })
                            })
                            .unwrap_or(0);
                        let snap_roles =
                            actor
                                .gekisou_support_skills
                                .iter()
                                .fold(0, |mask, &(id, level)| {
                                    mask | master.gekiso_support_skills.get(&id).map_or(
                                        0,
                                        |skill| {
                                            if skill.mission_type == 4
                                                || missions.contains(&skill.mission_type)
                                            {
                                                roles(
                                                    master,
                                                    &skill.effects,
                                                    level,
                                                    &actor,
                                                    &missions,
                                                )
                                            } else {
                                                0
                                            }
                                        },
                                    )
                                });
                        member_roles | (snap_roles << 4)
                    })
                    .collect()
            })
            .collect();
        Some(Self { masks, bounds })
    }
    pub fn mask(&self, member: usize, snap: i32) -> u8 {
        let row = &self.masks[member];
        row[if snap < 0 {
            row.len() - 1
        } else {
            snap as usize
        }]
    }
    pub fn feasible(
        &self,
        members: &[usize],
        assigned: &[i32],
        from: usize,
        options: &[Vec<i32>],
    ) -> bool {
        let (mut guaranteed, mut possible) = ([0u8; 5], [0u8; 5]);
        for (slot, &member) in members.iter().enumerate() {
            if slot < from {
                guaranteed[slot] = self.mask(member, assigned[slot]);
                possible[slot] = guaranteed[slot];
            } else {
                guaranteed[slot] = u8::MAX;
                let mut any = false;
                for &snap in &options[slot] {
                    if snap >= 0 && assigned[..from].contains(&snap) {
                        continue;
                    }
                    let mask = self.mask(member, snap);
                    guaranteed[slot] &= mask;
                    possible[slot] |= mask;
                    any = true;
                }
                if !any {
                    return false;
                }
            }
        }
        self.bounds.iter().all(|&(bit, minimum, maximum)| {
            let low = guaranteed.iter().filter(|&&mask| mask & bit != 0).count();
            let high = possible.iter().filter(|&&mask| mask & bit != 0).count();
            low <= maximum as usize && high >= minimum as usize
        })
    }
    pub fn accepts(&self, members: &[usize], snaps: &[i32]) -> bool {
        self.feasible(members, snaps, 5, &[])
    }
}
