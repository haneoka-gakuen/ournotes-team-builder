//! Whole-live simulation and appliers, in the client's exact phase order.
pub(crate) mod lottery;
use super::{
    conditions::{Checker, Context, Cumulative, Factory, GkView, Performer, Test},
    convert::{self, Key},
    core::{self, Combo, Life, Random},
    effects::{self, ConditionUpdater, FrameInput, State, Trigger},
    gekisou::{self, Controller, Environment, FeverUpdater, LuckHandle},
    play::{JudgedNote, LiveNote, Setup},
    score::{self, FactorCommand, Incremental},
};
use crate::{
    master::{Master, SkillEffect},
    num::f,
};
use rustc_hash::{FxHashMap as HashMap, FxHashSet as HashSet};
use std::{rc::Rc, sync::Arc};
#[derive(Clone)]
pub struct Params {
    pub music_type: i32,
    pub power: i32,
    pub level: f64,
    pub converted: i32,
    pub music_length: i32,
    pub assist: f64,
}
#[derive(Clone)]
pub struct LuckAction {
    pub frame: usize,
    pub range: i32,
    pub kind: i32,
    pub value: i32,
    pub limit: i32,
    pub chance: f64,
}
#[derive(Clone)]
pub struct LuckFrame {
    pub states: Arc<[i32]>,
    pub updates: Vec<usize>,
    pub playing: i32,
}
#[derive(Clone)]
pub struct LuckNote {
    pub frame: usize,
    pub range: usize,
    pub op: i32,
    pub judgement: i32,
    pub time: i32,
    pub speed: f64,
    pub buff: i32,
}
#[derive(Default, Clone)]
pub struct LuckRecord {
    pub actions: Vec<LuckAction>,
    pub frames: Vec<LuckFrame>,
    pub notes: Vec<LuckNote>,
}
#[derive(Clone)]
struct Row {
    id: i32,
    kind: i32,
    while_executing: bool,
    value: i32,
    maximum: i32,
    limit: i32,
    targets: Vec<i32>,
}
impl Row {
    fn applies(&self, state: i32) -> bool {
        state != effects::STAY && (state != effects::EXECUTING || self.while_executing)
    }
}
#[derive(Clone)]
struct LiveEffect {
    row: usize,
    act: f64,
    phase: i32,
    state: State,
    condition: Option<Checker>,
    release: Option<Checker>,
    cumulative: Option<Cumulative>,
}
#[derive(Clone)]
struct LiveSkill {
    key: i32,
    member: usize,
    index: usize,
    parent: i32,
    trigger: Trigger,
    effects: Vec<LiveEffect>,
}
#[derive(Clone)]
struct LivePool {
    key: i32,
    member: usize,
    available: Vec<usize>,
}
#[derive(Clone)]
struct CondSkill {
    member: usize,
    skill_type: i32,
    updater: ConditionUpdater,
}
#[derive(Clone)]
pub(crate) struct ComboSupportFrame {
    pub time: i32,
    pub combo: i32,
    pub states: Vec<[i32; 4]>,
}
#[derive(Clone)]
struct ComboObserver {
    updater: ConditionUpdater,
    frames: Vec<ComboSupportFrame>,
}
// Immutable input masks are shared by replay branches. Running one-shot effects
// remain scheduled until their updater returns them to the idle pool.
#[derive(Default)]
struct ConditionInputs {
    always: u64,
    ungated: u64,
    missions: HashMap<i32, u64>,
    members: HashMap<usize, u64>,
    ranges: u64,
    lottery: u64,
    judgements: u64,
    rank: u64,
}
#[derive(Clone)]
struct ConditionSchedule {
    inputs: Rc<ConditionInputs>,
    running: u64,
}
impl ConditionSchedule {
    fn new(conditions: &[CondSkill]) -> Option<Self> {
        if conditions.len() > u64::BITS as usize {
            return None;
        }
        let mut inputs = ConditionInputs::default();
        for (i, condition) in conditions.iter().enumerate() {
            let bit = 1u64 << i;
            match condition.updater.gate {
                None | Some(gekisou::MISSION_ALL) => inputs.ungated |= bit,
                Some(mission) => *inputs.missions.entry(mission).or_default() |= bit,
            }
            let Some(idle) = condition.updater.idle_inputs() else {
                inputs.always |= bit;
                continue;
            };
            for &member in &idle.members {
                *inputs.members.entry(member).or_default() |= bit;
            }
            if idle.ranges {
                inputs.ranges |= bit;
            }
            if idle.lottery {
                inputs.lottery |= bit;
            }
            if idle.judgements {
                inputs.judgements |= bit;
            }
            if idle.rank {
                inputs.rank |= bit;
            }
        }
        Some(Self {
            inputs: Rc::new(inputs),
            running: 0,
        })
    }
    fn select(
        &self,
        events: &[(usize, i32)],
        judged: bool,
        rank: Option<i32>,
        gk: Option<&GekisoLive>,
    ) -> u64 {
        let inputs = &self.inputs;
        let mut selected = inputs.always;
        for &(member, _) in events {
            selected |= inputs.members.get(&member).copied().unwrap_or(0);
        }
        if judged {
            selected |= inputs.judgements;
        }
        if rank.is_some() {
            selected |= inputs.rank;
        }
        if gk.is_none_or(|g| !g.controller.updates.is_empty()) {
            selected |= inputs.ranges;
        }
        if gk.is_none_or(|g| !g.previous_lots.is_empty()) {
            selected |= inputs.lottery;
        }
        // All conditions see these same mission gates before phase-one effects
        // are applied. Keep running one-shot effects eligible across gate changes.
        if let Some(g) = gk {
            let mut gated = inputs.ungated;
            for &i in &g.controller.updates {
                gated |= inputs
                    .missions
                    .get(&g.controller.ranges[i].mission)
                    .copied()
                    .unwrap_or(0);
            }
            if g.controller.current_playing >= 0 {
                let mission = g.controller.ranges[g.controller.current_playing as usize].mission;
                gated |= inputs.missions.get(&mission).copied().unwrap_or(0);
            }
            selected &= gated;
        }
        selected | self.running
    }
    fn update_running(&mut self, index: usize, running: bool) {
        let bit = 1u64 << index;
        if running {
            self.running |= bit;
        } else {
            self.running &= !bit;
        }
    }
}
#[derive(Clone)]
struct ConditionIndices {
    selected: Option<u64>,
    all: std::ops::Range<usize>,
}
impl Iterator for ConditionIndices {
    type Item = usize;
    #[inline]
    fn next(&mut self) -> Option<usize> {
        match &mut self.selected {
            Some(0) => None,
            Some(bits) => {
                let index = bits.trailing_zeros() as usize;
                *bits &= *bits - 1;
                Some(index)
            }
            None => self.all.next(),
        }
    }
}
#[derive(Clone, Copy)]
enum Listed {
    Live(usize, usize),
    Condition(usize, usize),
}
#[derive(Clone)]
pub struct ScoreControl<'a> {
    pub score: Incremental<'a>,
    counter: i32,
    commands: HashMap<i32, FactorCommand>,
}
impl<'a> ScoreControl<'a> {
    fn new(score: Incremental<'a>) -> Self {
        Self {
            score,
            counter: 0,
            commands: HashMap::default(),
        }
    }
    fn put(&mut self, command: FactorCommand) -> i32 {
        self.counter = self.counter.wrapping_add(1);
        self.commands.insert(self.counter, command.clone());
        self.score.add_factor(command);
        self.counter
    }
    fn take(&mut self, id: i32) -> Result<FactorCommand, String> {
        self.commands
            .remove(&id)
            .ok_or_else(|| format!("score factor {id} not found"))
    }
    fn add_note(&mut self, owner: i32, time: i32, factor: f64) -> i32 {
        self.put(FactorCommand {
            time,
            owner,
            note: score::note_factor_mill(factor),
            ..FactorCommand::default()
        })
    }
    fn disable_note(&mut self, time: i32, id: i32) -> Result<(), String> {
        let c = self.take(id)?;
        self.score.add_factor(FactorCommand {
            time,
            owner: c.owner,
            note: c.note.wrapping_neg(),
            ..FactorCommand::default()
        });
        Ok(())
    }
    fn add_combo(&mut self, owner: i32, time: i32, factor: f64) -> i32 {
        self.put(FactorCommand {
            time,
            owner,
            combo: score::judgement_factor_mill(factor),
            ..FactorCommand::default()
        })
    }
    fn disable_combo(&mut self, time: i32, id: i32) -> Result<(), String> {
        let c = self.take(id)?;
        self.score.add_factor(FactorCommand {
            time,
            owner: c.owner,
            combo: c.combo.wrapping_neg(),
            ..FactorCommand::default()
        });
        Ok(())
    }
    fn factor(&self, id: i32, combo: bool) -> f64 {
        let value = self
            .commands
            .get(&id)
            .map(|c| if combo { c.combo } else { c.note })
            .unwrap_or(0);
        if value == 0 {
            0.0
        } else {
            f(f(value as f64) / 100000.0)
        }
    }
}
impl LuckHandle for ScoreControl<'_> {
    fn add(&mut self, time: i32, percent: i32) -> Result<i32, String> {
        Ok(self.put(FactorCommand {
            time,
            owner: -1,
            luck: percent,
            ..FactorCommand::default()
        }))
    }
    fn disable(&mut self, time: i32, id: i32) -> Result<(), String> {
        let c = self.take(id)?;
        self.score.add_factor(FactorCommand {
            time,
            owner: -1,
            luck: c.luck.wrapping_neg(),
            ..FactorCommand::default()
        });
        Ok(())
    }
}
#[derive(Default, Clone)]
struct Appliers {
    ids: HashMap<Key, i32>,
    exhausted: HashSet<Key>,
    finished: HashMap<Key, i32>,
}
impl Appliers {
    fn add(&mut self, key: Key, id: i32) -> Result<(), String> {
        if self.ids.contains_key(&key) {
            return Err("effect state registered twice".into());
        }
        self.ids.insert(key, id);
        Ok(())
    }
    fn pop(&mut self, key: Key) -> Result<i32, String> {
        self.ids
            .remove(&key)
            .ok_or_else(|| "effect state not registered".into())
    }
    fn limit(&mut self, key: Key, state: &mut State) {
        if let Some(time) = self.finished.remove(&key) {
            state.state = effects::END;
            state.finish_ms = time;
        }
    }
}
#[derive(Clone)]
pub struct RankBonus {
    pub range: usize,
    pub rank: i32,
    pub bonus: i32,
    pub percent: i32,
}
#[derive(Clone)]
pub struct GekisoLive {
    pub controller: Controller,
    fever: FeverUpdater,
    pub factors: [[i32; 5]; 3],
    ranks: Vec<i32>,
    fever_updates: Vec<(usize, i32)>,
    previous_lots: Vec<i32>,
    previous_lot_ms: i32,
    pub rank_bonus: Vec<RankBonus>,
}
#[derive(Clone)]
pub struct Model<'a> {
    pub master: &'a Master,
    notes: Rc<HashMap<i32, LiveNote>>,
    events: Vec<(usize, i32)>,
    by_time: Vec<usize>,
    pub random: Random,
    pub life: Life,
    pub combo: Combo,
    pub score: ScoreControl<'a>,
    conversion: convert::Conversion,
    rows: Vec<Rc<Row>>,
    live: Vec<LiveSkill>,
    pools: Vec<LivePool>,
    enabled: Vec<usize>,
    condition: Vec<CondSkill>,
    condition_schedule: Option<ConditionSchedule>,
    guards: HashMap<Key, i32>,
    reductions: HashMap<Key, i32>,
    limits: HashMap<Key, i32>,
    frame_events: Vec<(usize, i32)>,
    judged: Vec<(i32, i32, i32)>,
    results: Vec<(LiveNote, i32)>,
    gk_judged: Vec<gekisou::GkNote>,
    fired: usize,
    pub trace: Vec<(i32, i32)>,
    frame_time: i32,
    frame_rank: Option<i32>,
    previous_rank: Option<i32>,
    previous_combo: i32,
    current_combo: i32,
    pub gk: Option<GekisoLive>,
    appliers: Appliers,
    params: Params,
    pub record: Option<LuckRecord>,
    pub record_trace: bool,
    rush_observers: Vec<ConditionUpdater>,
    combo_observer: Option<ComboObserver>,
    pub(crate) rush_support_commands: Vec<(i32, i32)>,
    /// Per-frame scratch, kept to avoid reallocating every frame.
    listed: Vec<Listed>,
    updated: Vec<usize>,
}
fn mission_group(mission: i32) -> Result<i32, String> {
    match mission {
        1..=3 => Ok(mission),
        4 => Ok(1),
        _ => Err(format!(
            "KeyNotFoundException: Gekisou mission type {mission}"
        )),
    }
}
pub(crate) fn approximate(a: f64, b: f64) -> bool {
    let tolerance = f(f(1e-6) * a.abs().max(b.abs())).max(f(1.401298464324817e-45 * 8.0));
    f(b - a).abs() < tolerance
}
impl<'a> Model<'a> {
    pub(crate) fn fixed_life(&mut self) {
        if self.rows.iter().any(|row| {
            matches!(row.kind, 3000..=3002)
                || row.kind == 13005 && self.master.live.damage[6] != 0.0
                || row.kind == 12006
                    && (!(1..=6).contains(&row.value)
                        || self.master.live.damage[row.value as usize] != 0.0)
        }) {
            return;
        }
        let life = self.master.live.life_base as i32;
        for skill in &mut self.live {
            for effect in &mut skill.effects {
                for checker in [&mut effect.condition, &mut effect.release]
                    .into_iter()
                    .flatten()
                {
                    checker.fixed_life(life);
                }
            }
        }
        for skill in &mut self.condition {
            skill.updater.fixed_life(life);
        }
        for observer in &mut self.rush_observers {
            observer.fixed_life(life);
        }
    }
    pub fn new(
        master: &'a Master,
        deck: &[Performer],
        notes: &[LiveNote],
        events: &[(usize, i32)],
        params: Params,
        setup: Option<&Setup>,
        mode: (i32, bool),
    ) -> Result<Self, String> {
        let (seed, record) = mode;
        let mut by_time: Vec<_> = (0..events.len()).collect();
        by_time.sort_by_key(|&i| (events[i].1, i));
        let live = &master.live;
        let gk = if let Some(setup) = setup {
            if setup.missions.len() < 3 {
                return Err("a Gekisou song has three missions".into());
            }
            let mut ranges = Vec::new();
            for (i, &(s, e)) in setup.fevers.iter().take(3).enumerate() {
                ranges.push((
                    s,
                    e,
                    *setup.missions.get(i).ok_or("fewer missions than fevers")?,
                ));
            }
            let mut controller = Controller::new(
                &ranges,
                &notes.iter().map(|n| (n.id, n.time_ms)).collect::<Vec<_>>(),
                &live.gekiso,
                live.gekiso.rush_percent,
                live.after_ms as i32,
            )?;
            controller.weighted = record;
            Some(GekisoLive {
                controller,
                fever: FeverUpdater::new(setup.fevers.clone()),
                factors: gekisou::ranking_factors(
                    &live.gekiso,
                    gekisou::mission_pattern(
                        setup.missions[0],
                        setup.missions[1],
                        setup.missions[2],
                    ),
                ),
                fever_updates: Vec::new(),
                previous_lots: Vec::new(),
                previous_lot_ms: 0,
                rank_bonus: Vec::new(),
                ranks: setup
                    .ranks
                    .iter()
                    .map(|r| r.clamp(&1, &5))
                    .copied()
                    .collect(),
            })
        } else {
            None
        };
        let calculator = score::Calculator::new(
            live,
            params.power,
            params.level,
            params.converted,
            params.assist,
        );
        let life_length = notes.iter().map(|n| n.time_ms).max().unwrap_or(0).max(0) + 1000;
        let mut model = Self {
            master,
            notes: Rc::new(notes.iter().map(|n| (n.id, n.clone())).collect()),
            events: events.to_vec(),
            by_time,
            random: Random::new(seed),
            life: Life::new(live.life_base as i32, live.damage, life_length),
            combo: Combo::default(),
            score: ScoreControl::new(Incremental::new(calculator, params.music_length)),
            conversion: convert::Conversion::new(
                live.timing_types
                    .iter()
                    .filter(|t| !live.just_types.contains(t))
                    .copied()
                    .collect(),
            ),
            rows: Vec::new(),
            live: Vec::new(),
            pools: Vec::new(),
            enabled: Vec::new(),
            condition: Vec::new(),
            condition_schedule: None,
            guards: HashMap::default(),
            reductions: HashMap::default(),
            limits: HashMap::default(),
            frame_events: Vec::new(),
            judged: Vec::new(),
            results: Vec::new(),
            gk_judged: Vec::new(),
            fired: 0,
            trace: Vec::new(),
            frame_time: 0,
            frame_rank: None,
            previous_rank: None,
            previous_combo: 0,
            current_combo: 0,
            gk,
            appliers: Appliers::default(),
            params,
            record: if record {
                Some(LuckRecord::default())
            } else {
                None
            },
            record_trace: false,
            rush_observers: Vec::new(),
            combo_observer: None,
            rush_support_commands: Vec::new(),
            listed: Vec::new(),
            updated: Vec::new(),
        };
        let factory = Factory {
            master,
            deck,
            initial_life: live.life_base as i32,
            music_type: model.params.music_type,
            initial_time: 0,
        };
        for (k, p) in deck.iter().enumerate() {
            let Some((id, level)) = p.live_skill else {
                continue;
            };
            let mut rows: Vec<_> = master
                .live_skills
                .get(&id)
                .into_iter()
                .flatten()
                .filter(|r| r.level == level)
                .collect();
            rows.sort_by_key(|r| r.id);
            let row_indexes = rows
                .iter()
                .map(|r| model.effect_row(r))
                .collect::<Result<Vec<_>, _>>()?;
            let key = id
                .wrapping_mul(1000)
                .wrapping_add(100)
                .wrapping_add(k as i32);
            let mut pool = Vec::new();
            for index in 0..5 {
                pool.push(model.live.len());
                let mut effects = Vec::new();
                for (i, row) in rows.iter().enumerate() {
                    let cumulative = factory.cumulative(row.cumulative_id, k)?;
                    let mut state = State::default();
                    if let Some(c) = &cumulative {
                        state.cumulative_count = c.initial();
                        state.cumulative_unit = c.unit();
                        state.cumulative_max = c.maximum();
                    }
                    effects.push(LiveEffect {
                        row: row_indexes[i],
                        act: f(row.seconds),
                        phase: model.phase(row.kind),
                        state,
                        condition: factory.group(row.condition_group, k)?,
                        release: factory.group(row.release_group, k)?,
                        cumulative,
                    });
                }
                model.live.push(LiveSkill {
                    key,
                    member: k,
                    index,
                    parent: effects::STAY,
                    trigger: Trigger::default(),
                    effects,
                });
            }
            model.pools.push(LivePool {
                key,
                member: k,
                available: pool,
            });
        }
        model.pools.sort_by_key(|p| p.key);
        for (k, p) in deck.iter().enumerate() {
            for &(id, level) in &p.support_skills {
                let rows = master
                    .support_skills
                    .get(&id)
                    .into_iter()
                    .flatten()
                    .filter(|r| r.level == level)
                    .collect::<Vec<_>>();
                model.condition_skill(rows, k, 3, None, &factory)?;
            }
        }
        if model.gk.is_some() {
            let active_missions = &setup.unwrap().missions;
            let missions = deck
                .iter()
                .map(|p| {
                    if let Some(m) = p.mission_only.filter(|&m| m != 0) {
                        return Ok(Some(m));
                    }
                    let Some((id, _)) = p.gekisou_skill else {
                        return Ok(None);
                    };
                    let skill = master
                        .gekiso_skills
                        .get(&id)
                        .ok_or_else(|| format!("unknown Gekisou skill {id}"))?;
                    mission_group(skill.mission_type)?;
                    Ok(Some(skill.mission_type))
                })
                .collect::<Result<Vec<_>, String>>()?;
            for mission in 1..=3 {
                for (k, p) in deck.iter().enumerate() {
                    let Some((id, level)) = p.gekisou_skill else {
                        continue;
                    };
                    let Some(kind) = missions[k] else {
                        continue;
                    };
                    if mission_group(kind)? != mission
                        || kind != gekisou::MISSION_ALL && !active_missions.contains(&kind)
                    {
                        continue;
                    }
                    let rows = master
                        .gekiso_skills
                        .get(&id)
                        .unwrap()
                        .effects
                        .iter()
                        .filter(|r| r.level == level)
                        .collect::<Vec<_>>();
                    model.condition_skill(rows, k, 4, Some(kind), &factory)?;
                }
            }
            for (k, p) in deck.iter().enumerate() {
                if missions[k].is_none() {
                    continue;
                }
                for &(id, level) in &p.gekisou_support_skills {
                    let skill = master
                        .gekiso_support_skills
                        .get(&id)
                        .ok_or_else(|| format!("unknown Gekisou support skill {id}"))?;
                    if skill.mission_type != gekisou::MISSION_ALL
                        && !active_missions.contains(&skill.mission_type)
                    {
                        continue;
                    }
                    model.condition_skill(
                        skill.effects.iter().filter(|r| r.level == level).collect(),
                        k,
                        5,
                        Some(skill.mission_type),
                        &factory,
                    )?;
                }
            }
        }
        model.condition_schedule = ConditionSchedule::new(&model.condition);
        Ok(model)
    }
    pub(crate) fn observe_rush_support(&mut self) -> Result<(), String> {
        for gate in [gekisou::MISSION_LUCK, gekisou::MISSION_ALL] {
            let effect = effects::Effect {
                id: 0,
                trigger_type: effects::SUSTAINED,
                act: 0.0,
                phase: self.phase(2000),
                trigger: Some(Checker::new(Test::Rush(false))),
                condition: None,
                execute_limit: 0,
                reset: None,
                row: 0,
                cumulative: None,
                release: None,
            };
            self.rush_observers
                .push(ConditionUpdater::new(vec![effect], Some(gate))?);
        }
        Ok(())
    }
    pub(crate) fn observe_combo_support(&mut self) -> Result<bool, String> {
        if self.phase(2000) != self.phase(2001) {
            return Ok(false);
        }
        let effect = effects::Effect {
            id: 1,
            trigger_type: effects::ONE_SHOT,
            act: 0.0,
            phase: self.phase(2000),
            trigger: Some(Checker::new(Test::RangeStart {
                missions: Vec::new(),
                triggered: Vec::new(),
            })),
            condition: None,
            execute_limit: 0,
            reset: None,
            row: 0,
            cumulative: None,
            release: Some(Checker::new(Test::RangeComplete)),
        };
        self.combo_observer = Some(ComboObserver {
            updater: ConditionUpdater::new(vec![effect], Some(gekisou::MISSION_COMBO))?,
            frames: Vec::new(),
        });
        Ok(true)
    }
    pub(crate) fn take_combo_support(&mut self) -> Option<Vec<ComboSupportFrame>> {
        self.combo_observer.take().map(|observer| observer.frames)
    }
    fn phase(&self, kind: i32) -> i32 {
        self.master.live.phases.get(&kind).copied().unwrap_or(1)
    }
    fn effect_row(&mut self, row: &SkillEffect) -> Result<usize, String> {
        let targets = row
            .targets
            .iter()
            .map(|id| {
                self.master
                    .targets
                    .get(id)
                    .map(|t| t.judgement)
                    .ok_or_else(|| format!("unknown skill target {id}"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let index = self.rows.len();
        self.rows.push(Rc::new(Row {
            id: row.id,
            kind: row.kind,
            while_executing: !matches!(
                row.kind,
                2000 | 2002
                    | 2004
                    | 2005
                    | 3000
                    | 3001
                    | 3002
                    | 3003
                    | 3004
                    | 15000
                    | 11000
                    | 11001
                    | 11002
                    | 11003
                    | 11004
                    | 12000
                    | 12002
                    | 12003
                    | 13000
                    | 13002
                    | 13003
                    | 13004
            ),
            value: row.value,
            maximum: row.max_value,
            limit: row.limit_count,
            targets,
        }));
        Ok(index)
    }
    fn condition_skill(
        &mut self,
        mut rows: Vec<&SkillEffect>,
        member: usize,
        skill_type: i32,
        gate: Option<i32>,
        factory: &Factory<'_>,
    ) -> Result<(), String> {
        rows.sort_by_key(|r| r.id);
        let mut effects = Vec::new();
        for row in rows {
            if row.trigger_type != effects::ONE_SHOT && row.trigger_type != effects::SUSTAINED {
                return Err(format!(
                    "ArgumentOutOfRangeException: skill trigger type {}",
                    row.trigger_type
                ));
            }
            effects.push(effects::Effect {
                id: row
                    .id
                    .wrapping_mul(100)
                    .wrapping_add(skill_type * 10)
                    .wrapping_add(member as i32),
                trigger_type: row.trigger_type,
                act: f(row.seconds),
                phase: self.phase(row.kind),
                trigger: factory.group(row.trigger_group, member)?,
                condition: factory.group(row.condition_group, member)?,
                execute_limit: row.execute_limit,
                reset: if row.execute_limit > 0 {
                    factory.group(row.reset_group, member)?
                } else {
                    None
                },
                row: self.effect_row(row)?,
                cumulative: factory.cumulative(row.cumulative_id, member)?,
                release: factory.group(row.release_group, member)?,
            });
        }
        self.condition.push(CondSkill {
            member,
            skill_type,
            updater: ConditionUpdater::new(effects, gate)?,
        });
        Ok(())
    }
    pub(crate) fn set_event_members(&mut self, events: &[(usize, i32)]) {
        for (target, source) in self.events.iter_mut().zip(events) {
            target.0 = source.0;
        }
    }
    pub fn frame(&mut self, time: i32, judged: &[JudgedNote], dt: f64) -> Result<(), String> {
        self.frame_time = time;
        self.frame_rank = self.previous_rank.take();
        if let Some(g) = &mut self.gk {
            g.fever_updates = g.fever.update(time);
            let current_score = self.score.score.score;
            let mut env = Environment {
                random: &mut self.random,
                handle: &mut self.score,
            };
            g.controller
                .before_update(dt, time, &g.fever_updates, current_score, &mut env)?;
            if let Some(record) = &mut self.record {
                let states = if let Some(previous) = record.frames.last().filter(|previous| {
                    previous.states.iter().copied().eq(g
                        .controller
                        .states
                        .iter()
                        .map(|state| state.state))
                }) {
                    Arc::clone(&previous.states)
                } else {
                    g.controller
                        .states
                        .iter()
                        .map(|state| state.state)
                        .collect()
                };
                record.frames.push(LuckFrame {
                    states,
                    updates: g.controller.updates.clone(),
                    playing: g.controller.current_playing,
                });
            }
        }
        self.frame_events.clear();
        let mut due = Vec::new();
        while self.fired < self.events.len() && self.events[self.by_time[self.fired]].1 <= time {
            due.push(self.by_time[self.fired]);
            self.fired += 1;
        }
        due.sort_unstable();
        self.frame_events
            .extend(due.into_iter().map(|i| self.events[i]));
        self.judged.clear();
        self.results.clear();
        let mut broken = false;
        for j in judged {
            let n = self
                .notes
                .get(&j.note_id)
                .ok_or_else(|| format!("unknown note {}", j.note_id))?;
            let converted = self
                .conversion
                .convert(j.judgement, n.judgement_type, j.time_ms)?;
            self.combo.add(n.time_ms, converted);
            self.results.push((n.clone(), converted));
            self.judged.push((n.id, converted, n.time_ms));
            if converted == 1 || converted == 2 {
                broken = true;
            }
        }
        let simulator_combo = self.combo.at(time);
        let added = (simulator_combo - self.previous_combo).max(0);
        self.previous_combo = simulator_combo;
        if broken {
            self.current_combo = 0;
        }
        self.current_combo = self.current_combo.wrapping_add(added);
        for (n, converted) in &self.results {
            self.life.note_damage(n.time_ms, *converted as usize);
            let life = self.life.at(n.time_ms);
            if self.master.live.note_percent.contains_key(&n.op) {
                let j = converted + 1;
                let score_type = if (0..9).contains(&j) {
                    [0, 0, 6, 5, 4, 3, 2, 1, 0][j as usize]
                } else {
                    0
                };
                self.score
                    .score
                    .add_note(n.time_ms, life, n.id, n.op, score_type);
            }
        }
        self.score.score.calculate(
            time,
            &self.combo,
            self.gk
                .as_ref()
                .map(|g| &g.controller as &dyn score::GekisoCombo),
        )?;
        let input = FrameInput {
            time,
            music_length: self.params.music_length,
            finished: false,
        };
        for &index in &self.enabled {
            self.live[index].trigger = Trigger::default();
        }
        let mut started = false;
        for pool in &mut self.pools {
            let Some(&(_, event_time)) = self
                .frame_events
                .iter()
                .rev()
                .find(|event| event.0 == pool.member)
            else {
                continue;
            };
            if pool.available.is_empty() {
                return Err("live skill pool is empty".into());
            }
            let index = pool.available.remove(0);
            self.live[index].trigger = Trigger {
                hit: true,
                time: event_time,
            };
            for effect in &mut self.live[index].effects {
                effect.state.state = effects::STAY;
            }
            self.enabled.push(index);
            started = true;
        }
        if started && self.enabled.len() > 1 {
            self.enabled
                .sort_by_key(|&i| self.live[i].key as i64 * 10 + self.live[i].index as i64);
        }
        let selected_conditions = self.condition_schedule.as_ref().map(|schedule| {
            schedule.select(
                &self.frame_events,
                !self.judged.is_empty(),
                self.frame_rank,
                self.gk.as_ref(),
            )
        });
        let condition_indices = ConditionIndices {
            selected: selected_conditions,
            all: 0..self.condition.len(),
        };
        for observer in &mut self.rush_observers {
            observer.begin_frame();
        }
        if let Some(observer) = &mut self.combo_observer {
            observer.updater.begin_frame();
        }
        let mut listed = std::mem::take(&mut self.listed);
        let mut updated = std::mem::take(&mut self.updated);
        for phase in 1..=2 {
            listed.clear();
            {
                let mut ctx = Context {
                    previous_rank: self.frame_rank,
                    life: &mut self.life,
                    random: &mut self.random,
                    time,
                    combo: self.current_combo,
                    judged: &self.judged,
                    events: &self.frame_events,
                    gk: self.gk.as_ref().map(|g| GkView {
                        controller: &g.controller,
                        previous_lots: &g.previous_lots,
                        previous_lot_ms: g.previous_lot_ms,
                    }),
                    record: self.record.is_some(),
                };
                for &index in &self.enabled {
                    let skill = &mut self.live[index];
                    if skill.parent == effects::END {
                        skill.parent = effects::STAY;
                        for effect in &mut skill.effects {
                            effect.state.state = effects::STAY;
                        }
                        continue;
                    }
                    let mut changed = false;
                    for effect in &mut skill.effects {
                        if effect.phase == phase {
                            changed = update_live_effect(effect, &input, skill.trigger, &mut ctx)?
                                || changed;
                        }
                    }
                    if changed {
                        skill.parent = aggregate(&skill.effects);
                    }
                    for (e, effect) in skill.effects.iter().enumerate() {
                        if effect.phase == phase
                            && self.rows[effect.row].applies(effect.state.state)
                        {
                            listed.push(Listed::Live(index, e));
                        }
                    }
                }
                for i in condition_indices.clone() {
                    let condition = &mut self.condition[i];
                    let updater = &mut condition.updater;
                    if phase == 1 {
                        updater.begin_frame();
                    }
                    updated.clear();
                    let result = updater.update(phase, &input, &mut ctx, &mut updated);
                    if let Some(schedule) = &mut self.condition_schedule {
                        schedule.update_running(i, updater.executing());
                    }
                    result?;
                    for &u in &updated {
                        let current = &updater.updaters[u];
                        let effect = &updater.effects[current.effect];
                        if !self.rows[effect.row].applies(current.state.state) {
                            continue;
                        }
                        if current.state.state == effects::EXECUTING
                            && effect.trigger_type == effects::ONE_SHOT
                            && !current.cumulative_changed
                            && [2001, 2003].contains(&self.rows[effect.row].kind)
                        {
                            continue;
                        }
                        listed.push(Listed::Condition(i, u));
                    }
                }
                for observer in &mut self.rush_observers {
                    updated.clear();
                    observer.update(phase, &input, &mut ctx, &mut updated)?;
                    for &index in &updated {
                        let state = &observer.updaters[index].state;
                        if state.state == effects::EXECUTE {
                            self.rush_support_commands.push((state.execute_ms, 1));
                        } else if state.state == effects::END {
                            self.rush_support_commands.push((state.finish_ms, -1));
                        }
                    }
                }
                if let Some(observer) = &mut self.combo_observer {
                    updated.clear();
                    observer
                        .updater
                        .update(phase, &input, &mut ctx, &mut updated)?;
                    if phase == observer.updater.effects[0].phase {
                        let combo = ctx
                            .gk
                            .as_ref()
                            .filter(|g| g.controller.current_playing >= 0)
                            .map_or(0, |g| {
                                g.controller.states[g.controller.current_playing as usize].combo
                            });
                        let state = |&index: &usize| {
                            let current = &observer.updater.updaters[index].state;
                            [
                                index as i32,
                                current.state,
                                current.execute_ms,
                                current.finish_ms,
                            ]
                        };
                        if observer.frames.last().is_none_or(|previous| {
                            previous.combo != combo
                                || previous.states.len() != updated.len()
                                || !previous
                                    .states
                                    .iter()
                                    .copied()
                                    .eq(updated.iter().map(state))
                        }) {
                            observer.frames.push(ComboSupportFrame {
                                time,
                                combo,
                                states: updated.iter().map(state).collect(),
                            });
                        }
                    }
                }
            }
            for &item in &listed {
                self.apply(item)?;
            }
        }
        self.listed = listed;
        self.updated = updated;
        for &index in &self.enabled {
            let skill = &self.live[index];
            if skill.parent == effects::STAY {
                let pool = self
                    .pools
                    .iter_mut()
                    .find(|p| p.key == skill.key && p.member == skill.member)
                    .ok_or("missing live skill pool")?;
                pool.available.push(index);
            }
        }
        self.enabled
            .retain(|&index| self.live[index].parent != effects::STAY);
        self.life.sync(time);
        self.score.score.calculate(
            time,
            &self.combo,
            self.gk
                .as_ref()
                .map(|g| &g.controller as &dyn score::GekisoCombo),
        )?;
        if self.gk.is_some() {
            self.after(time)?;
        }
        if self.record_trace {
            self.trace.push((time, self.score.score.score));
        }
        Ok(())
    }
    fn after(&mut self, time: i32) -> Result<(), String> {
        let g = self.gk.as_mut().unwrap();
        self.gk_judged.clear();
        self.gk_judged.extend(
            self.results
                .iter()
                .map(|(n, j)| (n.id, n.op, n.time_ms, *j)),
        );
        if let Some(record) = &mut self.record {
            let frame = record.frames.len() - 1;
            for (r, range) in g.controller.ranges.iter().enumerate() {
                if range.mission != 2 {
                    continue;
                }
                for &(id, op, time, judgement) in &self.gk_judged {
                    if range.targets.contains(&id) && judgement != 0 && judgement != 7 {
                        record.notes.push(LuckNote {
                            frame,
                            range: r,
                            op,
                            judgement,
                            time,
                            speed: g.controller.gauge_speed(time),
                            buff: g.controller.lot_buff(time),
                        });
                    }
                }
            }
        }
        let current = self.score.score.score;
        let mut env = Environment {
            random: &mut self.random,
            handle: &mut self.score,
        };
        g.controller
            .update(time, &self.gk_judged, &g.fever_updates, current, &mut env)?;
        g.previous_lots.clone_from(&g.controller.lot_results);
        g.previous_lot_ms = if g.previous_lots.is_empty() { 0 } else { time };
        let Some(index) = g
            .controller
            .updates
            .iter()
            .find(|&&i| g.controller.states[i].state == gekisou::COMPLETE)
            .copied()
        else {
            return Ok(());
        };
        let range = &g.controller.ranges[index];
        let s0 = self
            .score
            .score
            .calculate(range.start, &self.combo, Some(&g.controller))?;
        let s1 = self
            .score
            .score
            .calculate(range.end, &self.combo, Some(&g.controller))?;
        g.controller.states[index].start_score = s0;
        g.controller.states[index].end_score = s1;
        if index < g.factors.len() {
            let own = g.controller.states[index].score();
            let rank = g.ranks.get(index).copied().unwrap_or(1);
            let percent = g.factors[index][rank as usize - 1];
            let product = own as i64 * percent as i64;
            let bonus = crate::num::i32((product as f64 / 100.0).trunc());
            self.score.score.add_fixed(range.end, bonus);
            g.rank_bonus.push(RankBonus {
                range: index,
                rank,
                bonus,
                percent,
            });
            self.previous_rank = Some(rank);
        }
        Ok(())
    }
    fn state(&self, item: Listed) -> &State {
        match item {
            Listed::Live(skill, e) => &self.live[skill].effects[e].state,
            Listed::Condition(updater, u) => &self.condition[updater].updater.updaters[u].state,
        }
    }
    fn state_mut(&mut self, item: Listed) -> &mut State {
        match item {
            Listed::Live(skill, e) => &mut self.live[skill].effects[e].state,
            Listed::Condition(updater, u) => &mut self.condition[updater].updater.updaters[u].state,
        }
    }
    fn apply(&mut self, item: Listed) -> Result<(), String> {
        let (row_index, member, owner_type, skill_type, key) = match item {
            Listed::Live(skill, effect) => {
                let skill = &self.live[skill];
                let row = skill.effects[effect].row;
                (
                    row,
                    skill.member,
                    1,
                    1,
                    Key(
                        0,
                        self.rows[row].id,
                        skill.member as u32,
                        skill.index as u32,
                    ),
                )
            }
            Listed::Condition(index, u) => {
                let c = &self.condition[index];
                let updater = &c.updater.updaters[u];
                let effect = &c.updater.effects[updater.effect];
                (
                    effect.row,
                    c.member,
                    if c.skill_type == 4 { 1 } else { 2 },
                    c.skill_type,
                    Key(1, effect.id, updater.index as u32, c.member as u32),
                )
            }
        };
        let row = self.rows[row_index].clone();
        let mut state = self.state(item).clone();
        let owner = (member as i32).wrapping_mul(100).wrapping_add(owner_type);
        let gk_skill = skill_type == 4 || skill_type == 5;
        let kind = row.kind;
        let gk_type = [
            11000, 11001, 11002, 11003, 11004, 11005, 12000, 12002, 12003, 12004, 13000, 13002,
            13003, 13004,
        ]
        .contains(&kind);
        if (gk_type || kind == 13001) && !gk_skill && self.gk.is_none() {
            return Ok(());
        }
        if self.record.is_some() && (kind == 11003 || kind == 11005) {
            if state.state == effects::EXECUTE {
                if let Listed::Condition(index, u) = item {
                    let effect = self.condition[index].updater.updaters[u].effect;
                    let chance = {
                        let mut ctx = Context {
                            previous_rank: self.frame_rank,
                            life: &mut self.life,
                            random: &mut self.random,
                            time: self.frame_time,
                            combo: self.current_combo,
                            judged: &self.judged,
                            events: &self.frame_events,
                            gk: self.gk.as_ref().map(|g| GkView {
                                controller: &g.controller,
                                previous_lots: &g.previous_lots,
                                previous_lot_ms: g.previous_lot_ms,
                            }),
                            record: true,
                        };
                        if let Some(condition) =
                            &mut self.condition[index].updater.effects[effect].condition
                        {
                            condition.chance(&mut ctx)?
                        } else {
                            1.0
                        }
                    };
                    let range = self
                        .gk
                        .as_ref()
                        .ok_or("Gekisou effect without Gekisou")?
                        .controller
                        .playing_range();
                    if let Some(record) = self.record.as_mut() {
                        record.actions.push(LuckAction {
                            frame: record.frames.len() - 1,
                            range,
                            kind,
                            value: row.value,
                            limit: row.limit,
                            chance,
                        });
                    }
                }
            }
            return Ok(());
        }
        if (gk_skill || gk_type) && self.apply_gekiso(&row, &mut state, owner, key)? {
            let target = self.state_mut(item);
            target.state = state.state;
            target.finish_ms = state.finish_ms;
            return Ok(());
        }
        let value = row.value;
        match kind {
            0 | 1000..=1003 | 1500..=1503 => {}
            2000 | 2005 => {
                let mill = score::note_factor_mill(f(
                    f(value as f64) / if kind == 2005 { -10000.0 } else { 10000.0 }
                ));
                if state.state == effects::EXECUTE {
                    self.score.score.add_factor(FactorCommand {
                        time: state.execute_ms,
                        owner,
                        note: mill,
                        ..FactorCommand::default()
                    });
                } else if state.state == effects::END {
                    self.score.score.add_factor(FactorCommand {
                        time: state.finish_ms,
                        owner,
                        note: mill.wrapping_neg(),
                        ..FactorCommand::default()
                    });
                }
            }
            2001 | 2003 => self.cumulative_score(&row, &state, owner, key)?,
            2002 => {
                let mill = score::judgement_factor_mill(f(f(value as f64) / 10000.0));
                if state.state == effects::EXECUTE {
                    self.score.score.add_factor(FactorCommand {
                        time: state.execute_ms,
                        owner,
                        combo: mill,
                        ..FactorCommand::default()
                    });
                } else if state.state == effects::END {
                    self.score.score.add_factor(FactorCommand {
                        time: state.finish_ms,
                        owner,
                        combo: mill.wrapping_neg(),
                        ..FactorCommand::default()
                    });
                }
            }
            2004 => {
                let mill = score::judgement_factor_mill(f(f(value as f64) / 10000.0));
                for &judgement in &row.targets {
                    if state.state == effects::EXECUTE {
                        self.score.score.add_factor(FactorCommand {
                            time: state.execute_ms,
                            owner,
                            judgement,
                            judge: mill,
                            ..FactorCommand::default()
                        });
                    } else if state.state == effects::END {
                        self.score.score.add_factor(FactorCommand {
                            time: state.finish_ms,
                            owner,
                            judgement,
                            judge: mill.wrapping_neg(),
                            ..FactorCommand::default()
                        });
                    }
                }
            }
            15000 => {
                if state.state == effects::EXECUTE {
                    self.extend(member, f(value as f64));
                }
            }
            3000 => {
                if state.state == effects::EXECUTE {
                    if self.limits.contains_key(&key) {
                        return Err("duplicate life limit effect state".into());
                    }
                    let id = self.life.add_limit(value);
                    self.limits.insert(key, id);
                } else if state.state == effects::END {
                    let id = self
                        .limits
                        .remove(&key)
                        .ok_or("missing life limit effect state")?;
                    self.life.subtract_limit(id);
                }
            }
            3002 => {
                if state.state == effects::EXECUTE {
                    self.life.skill_damage(state.execute_ms, value, true);
                }
            }
            3004 => {
                if state.state == effects::EXECUTE {
                    if self.reductions.contains_key(&key) {
                        return Err("duplicate damage reduction effect state".into());
                    }
                    let id = self.life.enable_reduction(state.execute_ms, value);
                    self.reductions.insert(key, id);
                } else if state.state == effects::END {
                    if let Some(id) = self.reductions.remove(&key) {
                        self.life.disable_reduction(state.finish_ms, id);
                    }
                }
            }
            3001 => {
                if state.state == effects::EXECUTE {
                    self.life.recovery(state.execute_ms, value, true);
                }
            }
            3003 => {
                if state.state == effects::EXECUTE {
                    if self.guards.contains_key(&key) {
                        return Err("duplicate guard effect state".into());
                    }
                    let id = self.life.enable_guard(state.execute_ms);
                    self.guards.insert(key, id);
                } else if state.state == effects::END {
                    let id = self
                        .guards
                        .remove(&key)
                        .ok_or("missing guard effect state")?;
                    self.life.disable_guard(state.finish_ms, id);
                }
            }
            12006 | 13005 => {
                if kind != 13005 || self.gk.is_some() {
                    let time = self.conversion.update(
                        key,
                        state.state,
                        &convert::Effect {
                            kind,
                            value,
                            limit: row.limit,
                            targets: row.targets.clone(),
                            id: row.id,
                        },
                    )?;
                    if let Some(time) = time {
                        state.state = effects::END;
                        state.finish_ms = time;
                    }
                }
            }
            _ => return Err(format!("unsupported skill effect type {kind}")),
        }
        let target = self.state_mut(item);
        target.state = state.state;
        target.finish_ms = state.finish_ms;
        Ok(())
    }
    fn cumulative_score(
        &mut self,
        row: &Row,
        state: &State,
        owner: i32,
        key: Key,
    ) -> Result<(), String> {
        let combo = row.kind == 2003;
        let mut value = row.value.wrapping_mul(state.cumulative_count);
        if row.maximum > 0 {
            value = value.min(row.maximum);
        }
        let factor = f(f(value as f64) / 10000.0);
        if state.state == effects::EXECUTE {
            let id = if combo {
                self.score.add_combo(owner, state.execute_ms, factor)
            } else {
                self.score.add_note(owner, state.execute_ms, factor)
            };
            self.appliers.add(key, id)?;
        } else if state.state == effects::EXECUTING {
            let id = *self
                .appliers
                .ids
                .get(&key)
                .ok_or("effect state not registered")?;
            let old = self.score.factor(id, combo);
            if !approximate(old, factor) {
                if combo {
                    self.score.disable_combo(self.frame_time, id)?;
                } else {
                    self.score.disable_note(self.frame_time, id)?;
                }
                let id = if combo {
                    self.score.add_combo(owner, self.frame_time, factor)
                } else {
                    self.score.add_note(owner, self.frame_time, factor)
                };
                self.appliers.ids.insert(key, id);
            }
        } else if state.state == effects::END {
            let id = self.appliers.pop(key)?;
            if combo {
                self.score.disable_combo(state.finish_ms, id)?;
            } else {
                self.score.disable_note(state.finish_ms, id)?;
            }
        }
        Ok(())
    }
    fn apply_gekiso(
        &mut self,
        row: &Row,
        state: &mut State,
        owner: i32,
        key: Key,
    ) -> Result<bool, String> {
        let kind = row.kind;
        let value = row.value;
        let time = self.frame_time;
        let c = &mut self
            .gk
            .as_mut()
            .ok_or("Gekisou effect without Gekisou")?
            .controller;
        let phase = state.state;
        match kind {
            12000 | 13000 => {
                if phase == effects::EXECUTE {
                    let id = if kind == 12000 {
                        c.add_combo_bonus(state.execute_ms, f(value as f64) as i32)
                    } else {
                        c.add_just_bonus(state.execute_ms, f(value as f64) as i32)
                    };
                    self.appliers.add(key, id)?;
                } else if phase == effects::END {
                    let id = self.appliers.pop(key)?;
                    if kind == 12000 {
                        c.subtract_combo_bonus(state.finish_ms, id)?;
                    } else {
                        c.subtract_just_bonus(state.finish_ms, id)?;
                    }
                }
            }
            13002 => {
                if phase == effects::END {
                    if let Some(id) = self.appliers.ids.remove(&key) {
                        c.remove_cumulative(id);
                    }
                } else if phase == effects::EXECUTE && !self.appliers.ids.contains_key(&key) {
                    let id = c.add_cumulative(
                        state.cumulative_unit,
                        value,
                        state.cumulative_max,
                        row.maximum,
                    );
                    self.appliers.ids.insert(key, id);
                }
            }
            11000 | 11001 => {
                if phase == effects::EXECUTE {
                    let factor = f(f(value as f64) / 10000.0);
                    let id = if kind == 11000 {
                        c.add_lot_factor(state.execute_ms, factor)
                    } else {
                        c.add_gauge_factor(state.execute_ms, factor)
                    };
                    self.appliers.add(key, id)?;
                } else if phase == effects::END {
                    let id = self.appliers.pop(key)?;
                    if kind == 11000 {
                        c.subtract_lot_factor(state.finish_ms, id)?;
                    } else {
                        c.subtract_gauge_factor(state.finish_ms, id)?;
                    }
                }
            }
            11002 | 11004 | 12002 | 12003 | 13003 | 13004 => {
                if phase == effects::EXECUTE {
                    let mut count = value;
                    if kind == 11004 || kind == 12003 || kind == 13004 {
                        count = count.wrapping_mul(state.cumulative_count);
                        if row.maximum > 0 {
                            count = count.min(row.maximum);
                        }
                    }
                    if kind == 11002 || kind == 11004 {
                        c.add_luck_point(count);
                    } else if kind == 12002 || kind == 12003 {
                        c.add_combo(time, count as f64);
                    } else {
                        c.add_just(time, count as f64);
                    }
                }
            }
            11003 => {
                if phase == effects::EXECUTE {
                    let gauge = c.gauge_maximum().wrapping_mul(value);
                    c.add_gauge(core::floor_i32(f(f(gauge as f64) / 10000.0)))?;
                }
            }
            12004 => {
                self.appliers.limit(key, state);
                if state.state == effects::EXECUTE {
                    let mut mask = 0i32;
                    for &judgement in &row.targets {
                        if judgement != 0 && judgement != -1 {
                            mask |= 1i32.wrapping_shl((judgement & 31) as u32);
                        }
                    }
                    let id = c.enable_protection(state.execute_ms, row.limit, mask & 255);
                    self.appliers.add(key, id)?;
                } else if state.state == effects::END && !self.appliers.exhausted.remove(&key) {
                    if let Some(id) = self.appliers.ids.remove(&key) {
                        c.disable_protection(state.finish_ms, id);
                    }
                }
            }
            11005 => {
                self.appliers.limit(key, state);
                if state.state == effects::EXECUTE {
                    let result = if (0..=2).contains(&(value - 2)) {
                        value - 1
                    } else {
                        0
                    };
                    let id = c.machine.enable_minimum(result, row.limit);
                    self.appliers.add(key, id)?;
                } else if state.state == effects::EXECUTING {
                    if let Some(id) = self.appliers.ids.get(&key).copied() {
                        if !c.machine.minimum_active(id) {
                            let mut finished = c.machine.take_consumed(id);
                            if finished < 0 {
                                finished = time;
                            }
                            c.machine.disable_minimum(id);
                            self.appliers.ids.remove(&key);
                            self.appliers.exhausted.insert(key);
                            self.appliers.finished.insert(key, finished);
                        }
                    }
                } else if state.state == effects::END && !self.appliers.exhausted.remove(&key) {
                    if let Some(id) = self.appliers.ids.remove(&key) {
                        c.machine.disable_minimum(id);
                    }
                }
            }
            2001 => {
                let mut count = value.wrapping_mul(state.cumulative_count);
                if row.maximum > 0 && row.maximum < count {
                    count = row.maximum;
                }
                let factor = f(f(count as f64) / 10000.0);
                if phase == effects::EXECUTE {
                    let id = self.score.add_note(owner, state.execute_ms, factor);
                    self.appliers.add(key, id)?;
                } else if phase == effects::EXECUTING {
                    let id = *self
                        .appliers
                        .ids
                        .get(&key)
                        .ok_or("effect state not registered")?;
                    if !approximate(self.score.factor(id, false), factor) {
                        self.score.disable_note(time, id)?;
                        let id = self.score.add_note(owner, time, factor);
                        self.appliers.ids.insert(key, id);
                    }
                } else if phase == effects::END {
                    let id = self.appliers.pop(key)?;
                    self.score.disable_note(state.finish_ms, id)?;
                }
            }
            4004 => {}
            _ => return Ok(false),
        }
        Ok(true)
    }
    fn extend(&mut self, member: usize, ms: f64) {
        for skill in &mut self.live {
            if skill.member != member {
                continue;
            }
            for effect in &mut skill.effects {
                if effect.state.state == effects::EXECUTE
                    || effect.state.state == effects::EXECUTING
                {
                    effect.state.extended_ms = f(effect.state.extended_ms + ms);
                }
            }
        }
    }
}
fn update_live_effect(
    effect: &mut LiveEffect,
    input: &FrameInput,
    trigger: Trigger,
    ctx: &mut Context<'_>,
) -> Result<bool, String> {
    let before = effect.state.state;
    let changed = effects::effect_update(
        &mut effect.state,
        effect.act,
        input,
        trigger,
        (effect.condition.as_mut(), effect.release.as_mut()),
        false,
        ctx,
    )?;
    if let Some(c) = &mut effect.cumulative {
        let after = effect.state.state;
        if (before == effects::STAY && after == effects::EXECUTE)
            || before == effects::EXECUTE
            || (before == effects::EXECUTING && after == effects::EXECUTING)
        {
            effect.state.cumulative_count = c.update(ctx)?;
        } else if before == effects::END {
            c.reset();
        }
    }
    Ok(changed)
}
fn aggregate(effects: &[LiveEffect]) -> i32 {
    for state in [effects::EXECUTING, effects::EXECUTE, effects::END] {
        if effects.iter().any(|e| e.state.state == state) {
            return state;
        }
    }
    effects::STAY
}
