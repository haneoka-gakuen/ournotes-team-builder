//! Release-bound, typed tables compiled from the public team-builder document.
use crate::num::f;
use serde_json::{Map, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
pub type Stat3 = [i64; 3];
pub type Table<T> = BTreeMap<i32, BTreeMap<i32, T>>;
pub fn number(v: &Value, default: f64) -> f64 {
    let n = v
        .as_f64()
        .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()));
    n.filter(|n| n.is_finite()).unwrap_or(default)
}
pub fn int(v: &Value) -> i32 {
    number(v, 0.0).trunc() as i32
}
pub fn ids(v: &Value) -> Vec<i32> {
    v.as_array()
        .map(|a| a.iter().filter(|v| number(v, 0.0) > 0.0).map(int).collect())
        .unwrap_or_default()
}
pub fn integers(v: &Value) -> Vec<i32> {
    v.as_array()
        .map(|a| a.iter().map(int).collect())
        .unwrap_or_default()
}
pub fn numbers(v: &Value) -> Vec<f64> {
    v.as_array()
        .map(|a| a.iter().map(|v| number(v, 0.0)).collect())
        .unwrap_or_default()
}
pub fn rows(v: &Value) -> Vec<&Value> {
    if let Some(a) = v.as_array() {
        a.iter().collect()
    } else if let Some(m) = v.as_object() {
        m.values().collect()
    } else {
        Vec::new()
    }
}
pub fn native(v: &Value) -> Value {
    let mut row = Map::new();
    if let Some(raw) = v["raw"].as_object() {
        for (k, v) in raw {
            row.insert(k.strip_prefix('_').unwrap_or(k).to_owned(), v.clone());
        }
    }
    if let Some(fields) = v.as_object() {
        for (k, v) in fields {
            if k != "raw" {
                row.insert(k.clone(), v.clone());
            }
        }
    }
    Value::Object(row)
}
fn field<'a>(row: &'a Value, a: &str, b: &str) -> &'a Value {
    if row[a].is_null() {
        &row[b]
    } else {
        &row[a]
    }
}
fn stat(row: &Value, keys: [&str; 3]) -> Stat3 {
    keys.map(|key| int(&row[key]) as i64)
}
fn nested<T>(rows: Vec<&Value>, outer: &str, inner: &str, parse: impl Fn(&Value) -> T) -> Table<T> {
    let mut out: BTreeMap<i32, BTreeMap<i32, T>> = BTreeMap::new();
    for r in rows {
        let r = native(r);
        out.entry(int(&r[outer]))
            .or_default()
            .insert(int(&r[inner]), parse(&r));
    }
    out
}
#[derive(Clone)]
pub struct Target {
    pub id: i32,
    pub band_id: i32,
    pub card_type: i32,
    pub character_id: i32,
    pub tag_id: i32,
    pub live_categories: Vec<i32>,
    pub gekiso_categories: Vec<i32>,
    pub mission_type: i32,
    pub music_type: i32,
    pub judgement: i32,
    pub skill_target_type: i32,
}
impl Target {
    fn from(r: &Value) -> Self {
        Self {
            id: int(&r["id"]),
            band_id: int(field(r, "bandID", "bandId")),
            card_type: int(&r["cardType"]),
            character_id: int(field(r, "characterID", "characterId")),
            tag_id: int(field(r, "tagID", "tagId")),
            live_categories: integers(&r["liveSkillCategories"]),
            gekiso_categories: integers(&r["gekisouSkillCategories"]),
            mission_type: int(&r["gekisouMissionType"]),
            music_type: int(&r["liveMusicType"]),
            judgement: number(&r["judgement"], -1.0) as i32,
            skill_target_type: int(&r["skillTargetType"]),
        }
    }
}
#[derive(Clone)]
pub struct Condition {
    pub id: i32,
    pub kind: i32,
    pub targets: Vec<i32>,
    pub values: Vec<f64>,
    pub positive: bool,
}
#[derive(Clone)]
pub struct Cumulative {
    pub id: i32,
    pub kind: i32,
    pub targets: Vec<i32>,
    pub values: Vec<f64>,
    pub cap: i32,
}
#[derive(Clone)]
pub struct SkillEffect {
    pub id: i32,
    pub level: i32,
    pub trigger_type: i32,
    pub execute_limit: i32,
    pub reset_group: i32,
    pub kind: i32,
    pub value: i32,
    pub seconds: f64,
    pub condition_group: i32,
    pub trigger_group: i32,
    pub release_group: i32,
    pub cumulative_id: i32,
    pub targets: Vec<i32>,
    pub limit_count: i32,
    pub max_value: i32,
}
impl SkillEffect {
    pub fn from(r: &Value) -> Self {
        Self {
            id: int(&r["id"]),
            level: int(&r["level"]),
            trigger_type: int(&r["skillTriggerType"]),
            execute_limit: int(&r["effectExecuteLimitCount"]),
            reset_group: int(&r["effectExecuteLimitResetConditionGroup"]),
            kind: int(field(r, "skillEffectType", "effectType")),
            value: int(&r["effectValue"]),
            seconds: number(&r["activationTimeSecond"], 0.0),
            condition_group: int(field(r, "skillConditionGroup", "conditionGroup")),
            trigger_group: int(field(
                r,
                "skillTriggerConditionGroup",
                "triggerConditionGroup",
            )),
            release_group: int(field(
                r,
                "skillReleaseConditionGroup",
                "releaseConditionGroup",
            )),
            cumulative_id: int(field(
                r,
                "skillCumulativeConditionID",
                "cumulativeConditionId",
            )),
            targets: ids(field(r, "skillTargetIDs", "targetIds")),
            limit_count: int(&r["effectLimitCount"]),
            max_value: int(&r["maxEffectValue"]),
        }
    }
}
#[derive(Clone)]
pub struct MemberCard {
    pub id: i32,
    pub character_id: i32,
    pub band_id: i32,
    pub rarity: i32,
    pub card_type: i32,
    pub best_tags: Vec<i32>,
    pub stat_max: Stat3,
    pub level_group: i32,
    pub awake_group: i32,
    pub rank_group: i32,
    pub live_skill_id: i32,
    pub gekiso_skill_id: i32,
    pub leader_skill_id: i32,
    pub level_limits: Option<BTreeMap<i32, i32>>,
}
#[derive(Clone)]
pub struct SnapCard {
    pub id: i32,
    pub character_ids: Vec<i32>,
    pub band_ids: Vec<i32>,
    pub rarity: i32,
    pub card_type: i32,
    pub stat_max: Stat3,
    pub level_group: i32,
    pub rank_group: i32,
    pub support_skills: [i32; 2],
    pub gekiso_support_skills: [i32; 2],
}
#[derive(Clone)]
pub struct MemberRank {
    pub rates: Stat3,
    pub leader_skill_level: i32,
    pub music_type_rate: i32,
    pub music_tag_rate: i32,
}
#[derive(Clone)]
pub struct SnapRank {
    pub type_link_rate: i32,
    pub level_limit: i32,
    pub skill_levels: [i32; 2],
    pub gekiso_skill_levels: [i32; 2],
}
#[derive(Clone)]
pub struct Difficulty {
    pub difficulty: i32,
    pub score_id: i32,
    pub play_level: f64,
    pub display_level: f64,
    pub note_count: i32,
    pub file: String,
}
#[derive(Clone)]
pub struct Song {
    pub id: i32,
    pub music_type: i32,
    pub best_tags: Vec<i32>,
    pub band_ids: Vec<i32>,
    pub rank_group: i32,
    pub difficulties: Vec<Difficulty>,
    pub missions: Vec<i32>,
}
#[derive(Clone)]
pub struct ScoreRank {
    pub rank: i32,
    pub required: i32,
    pub battle_required: i32,
}
#[derive(Clone)]
pub struct EventEffect {
    pub resource_type: i32,
    pub character_id: i32,
    pub band_id: i32,
    pub card_type: i32,
    pub tag_id: i32,
    pub member_id: i32,
    pub snap_id: i32,
    pub bonus_type: i32,
    pub per_rank: [i32; 5],
}
impl EventEffect {
    fn from(r: &Value) -> Self {
        Self {
            resource_type: int(&r["resourceTypeConstraint"]),
            character_id: int(&r["characterId"]),
            band_id: int(&r["bandId"]),
            card_type: int(&r["cardType"]),
            tag_id: int(&r["tagId"]),
            member_id: int(&r["memberCardId"]),
            snap_id: int(&r["supportCardId"]),
            bonus_type: int(&r["eventBonusType"]),
            per_rank: std::array::from_fn(|i| {
                if r["perRank"].is_array() {
                    rows(&r["perRank"])
                        .iter()
                        .find(|row| int(&row["rank"]) == i as i32 + 1)
                        .map(|row| int(&row["value"]))
                        .unwrap_or(0)
                } else {
                    int(&r[format!("rank{}EffectValue", i + 1)])
                }
            }),
        }
    }
}
#[derive(Clone)]
pub struct Reward {
    pub score_rank: i32,
    pub group: i32,
    pub resource_type: i32,
    pub resource_id: i32,
    pub count: i32,
    pub probability: f64,
}
impl Reward {
    fn from(r: &Value) -> Self {
        Self {
            score_rank: int(&r["scoreRank"]),
            group: int(&r["group"]),
            resource_type: int(&r["resourceType"]),
            resource_id: int(&r["resourceId"]),
            count: int(&r["resourceCount"]),
            probability: number(&r["probability"], 10000.0),
        }
    }
}
#[derive(Clone)]
pub struct Event {
    pub id: i32,
    pub start_at: Value,
    pub end_at: Value,
    pub item_id: i32,
    pub music_id: i32,
    pub effects: Vec<EventEffect>,
    pub live_points: BTreeMap<i32, i32>,
    pub challenge_points: BTreeMap<i32, i32>,
    pub live_rewards: Vec<Reward>,
    pub challenge_rewards: Vec<Reward>,
}
#[derive(Clone)]
pub struct Boost {
    pub consumed: i32,
    pub event_point_rate: f64,
    pub reward_rate: f64,
}
#[derive(Clone)]
pub struct ChallengeMusic {
    pub id: i32,
    pub event_id: i32,
    pub music_id: i32,
    pub music_type: i32,
}
#[derive(Clone)]
pub struct GekisoSkill {
    pub mission_type: i32,
    pub effects: Vec<SkillEffect>,
}
#[derive(Clone)]
pub struct GekisoTables {
    pub base: Vec<[i32; 4]>,
    pub lots: Vec<[i32; 3]>,
    pub ranks: Vec<[i32; 4]>,
    pub gauge_max: i32,
    pub gauge_max_rush: i32,
    pub rush_percent: i32,
}
#[derive(Clone)]
pub struct LiveTables {
    pub adjustment: f64,
    pub life_onus: f64,
    pub life_base: f64,
    pub note_percent: BTreeMap<i32, f64>,
    pub judge_percent: BTreeMap<i32, f64>,
    pub combo: BTreeMap<i32, Vec<(usize, f64)>>,
    pub assist_percent: f64,
    pub damage: [f64; 7],
    pub settings: BTreeMap<String, String>,
    pub just_types: BTreeSet<i32>,
    pub just_windows: BTreeMap<i32, (i32, i32)>,
    pub miss_windows: BTreeMap<i32, (i32, i32)>,
    pub timing_types: BTreeSet<i32>,
    pub perfect_after_ms: BTreeMap<i32, i32>,
    pub after_ms: f64,
    pub phases: BTreeMap<i32, i32>,
    pub gekiso: GekisoTables,
}
#[derive(Clone)]
pub struct Params {
    pub music_type_base: i32,
    pub music_tag_base: i32,
    pub type_link_base: i32,
    pub skip_rank: i32,
}
#[derive(Clone)]
pub struct BandItem {
    pub band_id: i32,
    pub levels: BTreeMap<i32, Vec<SkillEffect>>,
}
#[derive(Clone)]
struct MasterData {
    pub server: String,
    pub release_id: String,
    pub params: Params,
    pub live: LiveTables,
    pub characters: BTreeMap<i32, i32>,
    pub members: BTreeMap<i32, MemberCard>,
    pub snaps: BTreeMap<i32, SnapCard>,
    pub member_levels: Table<Stat3>,
    pub member_awake: Table<Stat3>,
    pub member_ranks: Table<MemberRank>,
    pub member_level_limits: BTreeMap<(i32, i32), i32>,
    pub snap_levels: Table<Stat3>,
    pub snap_ranks: Table<SnapRank>,
    pub character_rank_bonus: Vec<(i32, i32)>,
    pub total_rank_bonus: Vec<(i32, i32)>,
    pub max_character_rank: i32,
    pub vip_bonus: BTreeMap<i32, i32>,
    pub band_items: BTreeMap<i32, BandItem>,
    pub targets: BTreeMap<i32, Target>,
    pub conditions: BTreeMap<i32, Condition>,
    pub condition_sets: BTreeMap<i32, Vec<Vec<i32>>>,
    pub cumulative: BTreeMap<i32, Cumulative>,
    pub leader_skills: BTreeMap<i32, Vec<SkillEffect>>,
    pub live_skills: BTreeMap<i32, Vec<SkillEffect>>,
    pub support_skills: BTreeMap<i32, Vec<SkillEffect>>,
    pub gekiso_skills: BTreeMap<i32, GekisoSkill>,
    pub gekiso_support_skills: BTreeMap<i32, GekisoSkill>,
    pub live_categories: BTreeMap<i32, Vec<i32>>,
    pub gekiso_categories: BTreeMap<i32, Vec<i32>>,
    pub songs: BTreeMap<i32, Song>,
    pub score_ranks: BTreeMap<i32, Vec<ScoreRank>>,
    pub challenge_music: Vec<ChallengeMusic>,
    pub events: BTreeMap<i32, Event>,
    pub live_challenge_points: BTreeMap<i32, i32>,
    pub boosts: Vec<Boost>,
    pub challenge_boosts: Vec<Boost>,
}
#[derive(Clone)]
pub struct Master {
    pub server: String,
    pub release_id: String,
    pub params: Arc<Params>,
    pub live: Arc<LiveTables>,
    pub characters: Arc<BTreeMap<i32, i32>>,
    pub members: Arc<BTreeMap<i32, MemberCard>>,
    pub snaps: Arc<BTreeMap<i32, SnapCard>>,
    pub member_levels: Arc<Table<Stat3>>,
    pub member_awake: Arc<Table<Stat3>>,
    pub member_ranks: Arc<Table<MemberRank>>,
    pub member_level_limits: Arc<BTreeMap<(i32, i32), i32>>,
    pub snap_levels: Arc<Table<Stat3>>,
    pub snap_ranks: Arc<Table<SnapRank>>,
    pub character_rank_bonus: Arc<Vec<(i32, i32)>>,
    pub total_rank_bonus: Arc<Vec<(i32, i32)>>,
    pub max_character_rank: i32,
    pub vip_bonus: Arc<BTreeMap<i32, i32>>,
    pub band_items: Arc<BTreeMap<i32, BandItem>>,
    pub targets: Arc<BTreeMap<i32, Target>>,
    pub conditions: Arc<BTreeMap<i32, Condition>>,
    pub condition_sets: Arc<BTreeMap<i32, Vec<Vec<i32>>>>,
    pub cumulative: Arc<BTreeMap<i32, Cumulative>>,
    pub leader_skills: Arc<BTreeMap<i32, Vec<SkillEffect>>>,
    pub live_skills: Arc<BTreeMap<i32, Vec<SkillEffect>>>,
    pub support_skills: Arc<BTreeMap<i32, Vec<SkillEffect>>>,
    pub gekiso_skills: Arc<BTreeMap<i32, GekisoSkill>>,
    pub gekiso_support_skills: Arc<BTreeMap<i32, GekisoSkill>>,
    pub live_categories: Arc<BTreeMap<i32, Vec<i32>>>,
    pub gekiso_categories: Arc<BTreeMap<i32, Vec<i32>>>,
    pub songs: Arc<BTreeMap<i32, Song>>,
    pub score_ranks: Arc<BTreeMap<i32, Vec<ScoreRank>>>,
    pub challenge_music: Arc<Vec<ChallengeMusic>>,
    pub events: Arc<BTreeMap<i32, Event>>,
    pub live_challenge_points: Arc<BTreeMap<i32, i32>>,
    pub boosts: Arc<Vec<Boost>>,
    pub challenge_boosts: Arc<Vec<Boost>>,
}

impl From<MasterData> for Master {
    fn from(data: MasterData) -> Self {
        Self {
            server: data.server,
            release_id: data.release_id,
            params: Arc::new(data.params),
            live: Arc::new(data.live),
            characters: Arc::new(data.characters),
            members: Arc::new(data.members),
            snaps: Arc::new(data.snaps),
            member_levels: Arc::new(data.member_levels),
            member_awake: Arc::new(data.member_awake),
            member_ranks: Arc::new(data.member_ranks),
            member_level_limits: Arc::new(data.member_level_limits),
            snap_levels: Arc::new(data.snap_levels),
            snap_ranks: Arc::new(data.snap_ranks),
            character_rank_bonus: Arc::new(data.character_rank_bonus),
            total_rank_bonus: Arc::new(data.total_rank_bonus),
            max_character_rank: data.max_character_rank,
            vip_bonus: Arc::new(data.vip_bonus),
            band_items: Arc::new(data.band_items),
            targets: Arc::new(data.targets),
            conditions: Arc::new(data.conditions),
            condition_sets: Arc::new(data.condition_sets),
            cumulative: Arc::new(data.cumulative),
            leader_skills: Arc::new(data.leader_skills),
            live_skills: Arc::new(data.live_skills),
            support_skills: Arc::new(data.support_skills),
            gekiso_skills: Arc::new(data.gekiso_skills),
            gekiso_support_skills: Arc::new(data.gekiso_support_skills),
            live_categories: Arc::new(data.live_categories),
            gekiso_categories: Arc::new(data.gekiso_categories),
            songs: Arc::new(data.songs),
            score_ranks: Arc::new(data.score_ranks),
            challenge_music: Arc::new(data.challenge_music),
            events: Arc::new(data.events),
            live_challenge_points: Arc::new(data.live_challenge_points),
            boosts: Arc::new(data.boosts),
            challenge_boosts: Arc::new(data.challenge_boosts),
        }
    }
}

fn skill_map(v: &Value) -> BTreeMap<i32, Vec<SkillEffect>> {
    v.as_object()
        .into_iter()
        .flat_map(|m| m.iter())
        .filter_map(|(k, v)| {
            k.parse().ok().map(|id| {
                (
                    id,
                    rows(&v["effects"])
                        .iter()
                        .map(|r| SkillEffect::from(&native(r)))
                        .collect(),
                )
            })
        })
        .collect()
}
fn gekiso_map(v: &Value) -> BTreeMap<i32, GekisoSkill> {
    v.as_object()
        .into_iter()
        .flat_map(|m| m.iter())
        .filter_map(|(k, v)| {
            k.parse().ok().map(|id| {
                (
                    id,
                    GekisoSkill {
                        mission_type: int(&v["gekisouMissionType"]),
                        effects: rows(&v["effects"])
                            .iter()
                            .map(|r| SkillEffect::from(&native(r)))
                            .collect(),
                    },
                )
            })
        })
        .collect()
}
fn categories(v: &Value) -> BTreeMap<i32, Vec<i32>> {
    v.as_object()
        .into_iter()
        .flat_map(|m| m.iter())
        .filter_map(|(k, v)| k.parse().ok().map(|id| (id, integers(&v["categories"]))))
        .collect()
}
fn boosts(v: &Value, key: &str) -> Vec<Boost> {
    let mut out: Vec<_> = rows(v)
        .iter()
        .map(|r| {
            let r = native(r);
            Boost {
                consumed: int(&r[key]),
                event_point_rate: number(&r["eventPointRate"], 0.0),
                reward_rate: number(&r["liveMusicRewardRate"], 0.0),
            }
        })
        .collect();
    out.sort_by_key(|r| r.consumed);
    out
}
fn text(v: &Value) -> String {
    v.as_str().map(str::to_owned).unwrap_or_else(|| {
        if v.is_null() {
            String::new()
        } else {
            v.to_string()
        }
    })
}
pub fn compile(data: &Value) -> Result<Master, String> {
    if !data.is_object() {
        return Err("team data must be an object".into());
    }
    let identity = &data["identity"];
    let runtime = &data["runtimeRules"]["tables"];
    let tools = &data["liveTools"];
    let progression = &data["progression"];
    let reference = &data["skillReference"];
    let skills = &data["skills"];
    let parameters = rows(&runtime["parameters"]["rows"]);
    let parameter = |key: &str, default: f64| {
        parameters
            .iter()
            .find(|r| r["id"] == key)
            .map(|r| number(&r["value"], default))
            .unwrap_or(default)
    };
    let settings: Vec<_> = rows(&tools["liveSettings"])
        .into_iter()
        .map(native)
        .collect();
    let setting = |key: &str, default: f64| {
        settings
            .iter()
            .find(|r| r["key"] == key)
            .map(|r| number(&r["value"], default))
            .unwrap_or(default)
    };
    let mut judge = BTreeMap::new();
    let mut damage = [0.0; 7];
    for r in rows(&tools["judgementParameters"]) {
        let r = native(r);
        let j = number(&r["noteSimulateJudgement"], -1.0) as i32;
        if (1..=6).contains(&j) {
            judge.insert(7 - j, number(&r["scorePercent"], 0.0));
            damage[j as usize] = number(&r["damage"], 0.0);
        }
    }
    let mut combo_rows: BTreeMap<i32, Vec<Value>> = BTreeMap::new();
    for r in rows(&tools["comboScoreBonuses"]) {
        let r = native(r);
        combo_rows
            .entry(int(&r["comboBonusType"]))
            .or_default()
            .push(r);
    }
    let combo = combo_rows
        .into_iter()
        .map(|(key, mut rows)| {
            rows.sort_by_key(|r| int(&r["requiredComboCount"]));
            let mut sum = 0.0;
            let out = rows
                .iter()
                .map(|r| {
                    sum = f(sum + f(number(&r["bonusFactor"], 0.0)));
                    (int(&r["requiredComboCount"]) as usize, sum)
                })
                .collect();
            (key, out)
        })
        .collect();
    let timings: Vec<_> = rows(&tools["judgementTiming"])
        .into_iter()
        .map(native)
        .collect();
    let non_playable: BTreeSet<_> = rows(&data["masterCharacterRoster"]["rows"])
        .into_iter()
        .map(native)
        .filter(|r| r["isNonPlayable"].as_bool() == Some(true))
        .map(|r| int(field(&r, "characterId", "id")))
        .collect();
    let characters: BTreeMap<_, _> = rows(&data["characters"])
        .iter()
        .filter(|r| {
            r["isNonPlayable"].as_bool() != Some(true)
                && !non_playable.contains(&int(field(r, "characterId", "id")))
        })
        .map(|r| {
            (
                int(field(r, "characterId", "id")),
                int(field(r, "bandId", "bandID")),
            )
        })
        .collect();
    let members = rows(&data["members"])
        .iter()
        .map(|r| {
            let id = int(&r["id"]);
            (
                id,
                MemberCard {
                    id,
                    character_id: int(&r["characterId"]),
                    band_id: int(&r["bandId"]),
                    rarity: int(&r["rarity"]),
                    card_type: int(&r["attribute"]),
                    best_tags: integers(&r["bestMusicTagIds"]),
                    stat_max: stat(&r["statMax"], ["performance", "technique", "visual"]),
                    level_group: int(&r["levelGroup"]),
                    awake_group: int(&r["trainingGroup"]),
                    rank_group: int(&r["awakeningGroup"]),
                    live_skill_id: int(&r["liveSkillId"]),
                    gekiso_skill_id: int(&r["gekisoSkillId"]),
                    leader_skill_id: int(&r["leaderSkillId"]),
                    level_limits: r["levelLimits"].as_object().map(|m| {
                        m.iter()
                            .filter_map(|(k, v)| k.parse().ok().map(|k| (k, int(v))))
                            .collect()
                    }),
                },
            )
        })
        .collect();
    let snaps = rows(&data["snapshots"])
        .iter()
        .map(|r| {
            let id = int(&r["id"]);
            let character_ids = integers(&r["characterIds"]);
            let mut band_ids = Vec::new();
            for id in &character_ids {
                let b = int(&data["characters"][id.to_string()]["bandId"]);
                if b != 0 && !band_ids.contains(&b) {
                    band_ids.push(b);
                }
            }
            let support = integers(&r["supportSkillIds"]);
            let gekiso = integers(&r["gekisoSupportSkillIds"]);
            (
                id,
                SnapCard {
                    id,
                    character_ids,
                    band_ids,
                    rarity: int(&r["rarity"]),
                    card_type: int(&r["attribute"]),
                    stat_max: stat(&r["statMax"], ["performance", "technique", "visual"]),
                    level_group: int(&r["levelGroup"]),
                    rank_group: int(&r["awakeningGroup"]),
                    support_skills: std::array::from_fn(|i| support.get(i).copied().unwrap_or(0)),
                    gekiso_support_skills: std::array::from_fn(|i| {
                        gekiso.get(i).copied().unwrap_or(0)
                    }),
                },
            )
        })
        .collect();
    let mut condition_sets: BTreeMap<i32, Vec<Vec<i32>>> = BTreeMap::new();
    for r in rows(&reference["conditionSets"]) {
        let r = native(r);
        condition_sets
            .entry(int(&r["group"]))
            .or_default()
            .push(ids(&r["conditionIds"]));
    }
    let mut score_ranks: BTreeMap<i32, Vec<ScoreRank>> = BTreeMap::new();
    for r in rows(&tools["scoreRanks"]) {
        let r = native(r);
        score_ranks
            .entry(int(&r["group"]))
            .or_default()
            .push(ScoreRank {
                rank: int(&r["liveScoreRank"]),
                required: int(&r["requiredScore"]),
                battle_required: int(&r["battleLiveRequiredScore"]),
            });
    }
    for r in score_ranks.values_mut() {
        r.sort_by_key(|r| r.required);
    }
    let songs = rows(&data["songs"])
        .iter()
        .map(|r| {
            let id = int(&r["musicId"]);
            let band_ids = if !r["bandIds"].is_null() {
                ids(&r["bandIds"])
            } else {
                ids(&serde_json::json!([r["bandId"]]))
            };
            (
                id,
                Song {
                    id,
                    music_type: int(&r["musicType"]),
                    best_tags: ids(&r["bestMusicTagIds"]),
                    band_ids,
                    rank_group: int(&r["liveScoreRankGroup"]),
                    difficulties: rows(&r["difficulty"])
                        .iter()
                        .map(|r| Difficulty {
                            difficulty: int(&r["difficulty"]),
                            score_id: int(&r["scoreId"]),
                            play_level: number(&r["playLevel"], 0.0),
                            display_level: number(field(r, "displayLevel", "playLevel"), 0.0),
                            note_count: int(&r["noteCount"]),
                            file: text(&r["file"]),
                        })
                        .collect(),
                    missions: integers(&r["gekisou"]["missionTypes"]),
                },
            )
        })
        .collect();
    let mut events = BTreeMap::new();
    for event in rows(&data["events"]) {
        let id = int(&event["id"]);
        let tables = &event["tables"];
        let table = |name: &str| -> Vec<Value> {
            if !tables[name].is_null() {
                rows(&tables[name]).into_iter().map(native).collect()
            } else {
                rows(&data["eventRules"][name])
                    .into_iter()
                    .map(native)
                    .filter(|r| {
                        r["eventId"].is_null()
                            || int(&r["eventId"]) == 0
                            || int(&r["eventId"]) == id
                    })
                    .collect()
            }
        };
        let points = |name: &str, group: &str| -> BTreeMap<i32, i32> {
            let group_id = int(field(&event["groups"], group, group));
            let group_id = if event["groups"][group].is_null() {
                int(&event[group])
            } else {
                group_id
            };
            table(name)
                .iter()
                .filter(|r| {
                    group_id == 0
                        || r["group"].is_null()
                        || int(&r["group"]) == 0
                        || int(&r["group"]) == group_id
                })
                .map(|r| (int(&r["scoreRank"]), int(&r["value"])))
                .collect()
        };
        let effects = if event["effects"].is_array() {
            rows(&event["effects"]).into_iter().map(native).collect()
        } else {
            table("MasterEventEffect")
        };
        let item = &event["eventItem"];
        let item_id = if !item["resourceId"].is_null() {
            int(&item["resourceId"])
        } else if !item["id"].is_null() {
            int(&item["id"])
        } else {
            int(&event["eventItemId"])
        };
        events.insert(
            id,
            Event {
                id,
                start_at: event["startAt"].clone(),
                end_at: event["endAt"].clone(),
                item_id,
                music_id: int(&event["musicId"]),
                effects: effects.iter().map(EventEffect::from).collect(),
                live_points: points("MasterLiveEventPoint", "liveEventPointGroup"),
                challenge_points: points(
                    "MasterChallengeLiveEventPoint",
                    "challengeLiveEventPointGroup",
                ),
                live_rewards: table("MasterLiveEventReward")
                    .iter()
                    .map(Reward::from)
                    .collect(),
                challenge_rewards: table("MasterChallengeLiveEventReward")
                    .iter()
                    .map(Reward::from)
                    .collect(),
            },
        );
    }
    let mut character_rank_bonus: Vec<_> = rows(&progression["characterRanks"])
        .iter()
        .map(|r| {
            let r = native(r);
            (int(&r["rank"]), int(&r["bonus"]))
        })
        .collect();
    character_rank_bonus.sort_by_key(|r| r.0);
    let mut total_rank_bonus: Vec<_> = rows(&progression["characterTotalRanks"])
        .iter()
        .map(|r| {
            let r = native(r);
            (int(&r["totalRank"]), int(&r["bonus"]))
        })
        .collect();
    total_rank_bonus.sort_by_key(|r| r.0);
    let mut band_items = BTreeMap::new();
    for r in rows(&data["bandItems"]) {
        let mut levels: BTreeMap<i32, Vec<SkillEffect>> = BTreeMap::new();
        for effect in rows(&r["effects"]) {
            let e = SkillEffect::from(&native(effect));
            levels.entry(e.level).or_default().push(e);
        }
        band_items.insert(
            int(&r["bandItemId"]),
            BandItem {
                band_id: int(&r["bandId"]),
                levels,
            },
        );
    }
    let skip = parameters
        .iter()
        .find(|r| r["id"] == "live_skip_result_score_rank")
        .map(|r| text(&r["value"]))
        .unwrap_or_else(|| "C".into());
    let skip_rank = match skip.trim() {
        "None" => 0,
        "E" => 1,
        "D" => 2,
        "C" => 3,
        "B" => 4,
        "A" => 5,
        "S" => 6,
        "SS" => 7,
        _ => 3,
    };
    let challenge = if !data["crossServer"]["challengeMusics"].is_null() {
        &data["crossServer"]["challengeMusics"]
    } else {
        &data["challengeMusicTable"]["rows"]
    };
    Ok(Master::from(MasterData {
        server: text(&identity["server"]),
        release_id: text(&identity["releaseId"]),
        params: Params {
            music_type_base: parameter("music_type_base_bonus_rate", 500.0) as i32,
            music_tag_base: parameter("music_tag_base_bonus_rate", 500.0) as i32,
            type_link_base: parameter("type_link_base_bonus_rate", 500.0) as i32,
            skip_rank,
        },
        live: LiveTables {
            adjustment: f(setting("note_score_adjustment_factor", 3.0)),
            life_onus: f(setting("note_score_life_onus_factor", 0.3)),
            life_base: setting("life_base", 1000.0),
            note_percent: rows(&tools["noteParameters"])
                .iter()
                .map(|r| {
                    let r = native(r);
                    (int(&r["noteOperateType"]), number(&r["scorePercent"], 0.0))
                })
                .collect(),
            judge_percent: judge,
            combo,
            assist_percent: setting("assist_score_percent", 90.0),
            damage,
            settings: settings
                .iter()
                .map(|r| (text(&r["key"]), text(&r["value"])))
                .collect(),
            just_types: timings
                .iter()
                .filter(|r| int(&r["noteSimulateJudgement"]) == 6)
                .map(|r| int(&r["noteJudgementType"]))
                .collect(),
            just_windows: timings
                .iter()
                .filter(|r| int(&r["assistLevel"]) == 0 && int(&r["noteSimulateJudgement"]) == 6)
                .map(|r| (int(&r["noteJudgementType"]), (int(&r["beforeMs"]), int(&r["afterMs"]))))
                .collect(),
            miss_windows: timings
                .iter()
                .filter(|r| int(&r["assistLevel"]) == 0 && int(&r["noteSimulateJudgement"]) == 1)
                .map(|r| (int(&r["noteJudgementType"]), (int(&r["beforeMs"]), int(&r["afterMs"]))))
                .collect(),
            timing_types: timings
                .iter()
                .map(|r| int(&r["noteJudgementType"]))
                .collect(),
            perfect_after_ms: timings
                .iter()
                .filter(|r| int(&r["assistLevel"]) == 0 && int(&r["noteSimulateJudgement"]) == 5)
                .map(|r| {
                    (
                        int(&r["noteJudgementType"]),
                        int(field(r, "afterMs", "after")),
                    )
                })
                .collect(),
            after_ms: timings
                .iter()
                .map(|r| number(field(r, "afterMs", "after"), 0.0))
                .fold(0.0, f64::max),
            phases: rows(&reference["effectSettings"])
                .iter()
                .map(|r| {
                    let r = native(r);
                    (int(&r["skillEffectType"]), number(&r["phase"], 1.0) as i32)
                })
                .collect(),
            gekiso: GekisoTables {
                base: rows(&tools["gekisouLuckBasePoints"])
                    .iter()
                    .map(|r| {
                        let r = native(r);
                        [
                            int(&r["noteCategory"]),
                            int(&r["noteSimulateJudgement"]),
                            int(&r["weight"]),
                            int(&r["basePoint"]),
                        ]
                    })
                    .collect(),
                lots: rows(&tools["gekisouLuckBonusLots"])
                    .iter()
                    .map(|r| {
                        let r = native(r);
                        [
                            int(&r["chanceLotType"]),
                            int(&r["lotResult"]),
                            int(&r["weight"]),
                        ]
                    })
                    .collect(),
                ranks: rows(&tools["gekisouRankingScoreBonuses"])
                    .iter()
                    .map(|r| {
                        let r = native(r);
                        [
                            int(&r["missionPattern"]),
                            int(&r["count"]),
                            int(&r["rank"]),
                            int(&r["scoreBonusPercent"]),
                        ]
                    })
                    .collect(),
                gauge_max: setting("gekisou_luck_gauge_max", 100.0) as i32,
                gauge_max_rush: setting("gekisou_luck_gauge_max_rush", 50.0) as i32,
                rush_percent: setting("gekisou_luck_rush_score_bonus_percent", 0.0) as i32,
            },
        },
        characters,
        members,
        snaps,
        member_levels: nested(
            rows(&progression["memberCardLevels"]),
            "group",
            "level",
            |r| stat(r, ["performanceRate", "technicRate", "visualRate"]),
        ),
        member_awake: nested(
            rows(&progression["memberCardAwake"]),
            "group",
            "awakeCount",
            |r| stat(r, ["performanceRate", "technicRate", "visualRate"]),
        ),
        member_ranks: nested(
            rows(&progression["memberCardRanks"]),
            "group",
            "rank",
            |r| MemberRank {
                rates: stat(r, ["performanceRate", "technicRate", "visualRate"]),
                leader_skill_level: number(&r["leaderSkillLevel"], 1.0) as i32,
                music_type_rate: int(&r["musicTypeBonusRate"]),
                music_tag_rate: int(&r["musicTagBonusRate"]),
            },
        ),
        member_level_limits: rows(&progression["memberCardLevelLimits"])
            .iter()
            .map(|r| {
                let r = native(r);
                (
                    (int(&r["rarity"]), int(&r["awakeCount"])),
                    int(&r["limitLevel"]),
                )
            })
            .collect(),
        snap_levels: nested(
            rows(&progression["supportCardLevels"]),
            "group",
            "level",
            |r| stat(r, ["performanceRate", "technicRate", "visualRate"]),
        ),
        snap_ranks: nested(
            rows(&progression["supportCardRanks"]),
            "group",
            "rank",
            |r| SnapRank {
                type_link_rate: int(&r["cardTypeLinkBonusRate"]),
                level_limit: int(&r["limitLevel"]),
                skill_levels: [
                    int(&r["supportSkill01Level"]),
                    int(&r["supportSkill02Level"]),
                ],
                gekiso_skill_levels: [
                    int(&r["gekisouSupportSkill01Level"]),
                    int(&r["gekisouSupportSkill02Level"]),
                ],
            },
        ),
        max_character_rank: character_rank_bonus.iter().map(|r| r.0).max().unwrap_or(1),
        character_rank_bonus,
        total_rank_bonus,
        vip_bonus: rows(&runtime["vipRankBonuses"]["rows"])
            .iter()
            .filter(|r| int(&r["vipBonusType"]) == 7)
            .map(|r| (int(&r["vipRank"]), int(&r["value"])))
            .collect(),
        band_items,
        targets: rows(&reference["targets"])
            .iter()
            .map(|r| {
                let t = Target::from(&native(r));
                (t.id, t)
            })
            .collect(),
        conditions: rows(&reference["conditions"])
            .iter()
            .map(|r| {
                let r = native(r);
                let id = int(&r["id"]);
                (
                    id,
                    Condition {
                        id,
                        kind: int(&r["conditionType"]),
                        targets: ids(&r["conditionTargetIDs"]),
                        values: numbers(&r["conditionValues"]),
                        positive: r["isPositive"] != false,
                    },
                )
            })
            .collect(),
        condition_sets,
        cumulative: rows(&reference["cumulativeConditions"])
            .iter()
            .map(|r| {
                let r = native(r);
                let id = int(&r["id"]);
                (
                    id,
                    Cumulative {
                        id,
                        kind: int(field(&r, "skillCumulativeConditionType", "conditionType")),
                        targets: ids(&r["conditionTargetIDs"]),
                        values: numbers(&r["conditionValues"]),
                        cap: int(&r["maxCumulativeCount"]),
                    },
                )
            })
            .collect(),
        leader_skills: skill_map(&skills["leader"]),
        live_skills: skill_map(&skills["live"]),
        support_skills: skill_map(&skills["support"]),
        gekiso_skills: gekiso_map(&skills["gekiso"]),
        gekiso_support_skills: gekiso_map(&skills["gekisoSupport"]),
        live_categories: categories(&skills["live"]),
        gekiso_categories: categories(&skills["gekiso"]),
        songs,
        score_ranks,
        challenge_music: rows(challenge)
            .iter()
            .map(|r| ChallengeMusic {
                id: int(&r["id"]),
                event_id: int(&r["eventId"]),
                music_id: int(&r["liveMusicId"]),
                music_type: int(&r["musicType"]),
            })
            .collect(),
        events,
        live_challenge_points: rows(&data["challengePointTable"]["rows"])
            .iter()
            .map(|r| (int(&r["scoreRank"]), int(&r["value"])))
            .collect(),
        boosts: boosts(&tools["liveBoostBonuses"], "consumedLiveBoostCount"),
        challenge_boosts: boosts(
            &tools["challengeBoostBonuses"],
            "consumedChallengePointCount",
        ),
    }))
}
