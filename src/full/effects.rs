//! Skill effect state machines and pooled condition-skill activations.
use super::{
    conditions::{Checker, Context, Cumulative, IdleInput, Test},
    core, gekisou,
};
use crate::num::f;
use rustc_hash::FxHashMap as HashMap;
pub const STAY: i32 = 0;
pub const EXECUTE: i32 = 2;
pub const EXECUTING: i32 = 3;
pub const END: i32 = 4;
pub const ONE_SHOT: i32 = 1;
pub const SUSTAINED: i32 = 2;
#[derive(Clone)]
pub struct State {
    pub state: i32,
    pub execute_ms: i32,
    pub finish_ms: i32,
    pub extended_ms: f64,
    pub cumulative_count: i32,
    pub cumulative_unit: i32,
    pub cumulative_max: i32,
}
impl Default for State {
    fn default() -> Self {
        Self {
            state: STAY,
            execute_ms: -1,
            finish_ms: -1,
            extended_ms: 0.0,
            cumulative_count: 0,
            cumulative_unit: 0,
            cumulative_max: 0,
        }
    }
}
#[derive(Clone, Copy)]
pub struct FrameInput {
    pub time: i32,
    pub music_length: i32,
    pub finished: bool,
}
#[derive(Clone, Copy, Default)]
pub struct Trigger {
    pub hit: bool,
    pub time: i32,
}
pub fn effect_update(
    state: &mut State,
    act: f64,
    input: &FrameInput,
    trigger: Trigger,
    checks: (Option<&mut Checker>, Option<&mut Checker>),
    finish_frame: bool,
    ctx: &mut Context<'_>,
) -> Result<bool, String> {
    let (condition, release) = checks;
    match state.state {
        STAY => {
            let ok = if let Some(condition) = condition {
                condition.check(ctx)?.0
            } else {
                true
            };
            if ok && trigger.hit {
                state.state = EXECUTE;
                state.execute_ms = trigger.time;
            }
            Ok(ok && trigger.hit)
        }
        EXECUTE => {
            if finish_frame {
                state.state = END;
                state.finish_ms = input.time;
            } else if release.is_some() {
                state.state = EXECUTING;
            } else {
                let duration = f(f(act * 1000.0) + state.extended_ms);
                let elapsed = f(input.time.wrapping_sub(state.execute_ms) as f64);
                if duration > elapsed {
                    state.state = EXECUTING;
                } else {
                    state.state = END;
                    state.finish_ms = input.time;
                }
            }
            Ok(true)
        }
        EXECUTING => {
            let mut release_time = None;
            let released = if let Some(release) = release {
                let hit = release.check(ctx)?.0;
                if hit {
                    release_time = release.override_time();
                }
                hit
            } else {
                false
            };
            let duration = f(f(act * 1000.0) + state.extended_ms);
            let elapsed = f(input.time.wrapping_sub(state.execute_ms) as f64);
            let by_time = duration < elapsed && act > 0.0;
            if !(released || finish_frame || by_time) {
                return Ok(false);
            }
            state.state = END;
            let mut finish = if !released && !finish_frame {
                state.execute_ms.wrapping_add(core::ceil_i32(duration))
            } else {
                release_time.unwrap_or(input.time)
            };
            if input.music_length > 0 && input.music_length <= finish {
                finish = input.music_length;
            }
            state.finish_ms = finish;
            Ok(true)
        }
        END => {
            state.state = STAY;
            Ok(true)
        }
        value => Err(format!("effect state {value} out of range")),
    }
}
fn timed(act: f64) -> bool {
    if act.is_nan() || act <= 0.0 {
        return false;
    }
    let big = f(i32::MAX as f64);
    let m = act.abs().max(big);
    let tolerance = f(m * f(1e-6)).max(f(1.401298464324817e-45 * 8.0));
    tolerance <= f(big - act).abs()
}
#[derive(Clone)]
pub struct Effect {
    pub id: i32,
    pub trigger_type: i32,
    pub act: f64,
    pub phase: i32,
    pub trigger: Option<Checker>,
    pub condition: Option<Checker>,
    pub execute_limit: i32,
    pub reset: Option<Checker>,
    pub row: usize,
    pub cumulative: Option<Cumulative>,
    pub release: Option<Checker>,
}
fn draws_random(checker: &Checker) -> bool {
    match &checker.test {
        Test::Probability(_) => true,
        Test::And(items) | Test::Or(items) => items.iter().any(draws_random),
        Test::Not(inner) => draws_random(inner),
        _ => false,
    }
}
fn inactive(effect: &Effect) -> bool {
    effect
        .condition
        .as_ref()
        .is_some_and(|checker| matches!(checker.test, Test::Fixed(false)))
        && !effect.trigger.as_ref().is_some_and(draws_random)
        && !effect.reset.as_ref().is_some_and(draws_random)
}
#[derive(Clone)]
pub struct Updater {
    pub state: State,
    pub cumulative_changed: bool,
    pub effect: usize,
    pub index: usize,
    pub release: Option<Checker>,
    pub phase: i32,
    pub cumulative: Option<Cumulative>,
}
impl Updater {
    fn new(
        effect: usize,
        index: usize,
        release: Option<Checker>,
        phase: i32,
        cumulative: Option<Cumulative>,
    ) -> Self {
        let mut state = State::default();
        if let Some(c) = &cumulative {
            state.cumulative_count = c.initial();
            state.cumulative_unit = c.unit();
            state.cumulative_max = c.maximum();
        }
        Self {
            state,
            cumulative_changed: false,
            effect,
            index,
            release,
            phase,
            cumulative,
        }
    }
    pub fn update(
        &mut self,
        act: f64,
        input: &FrameInput,
        trigger: Trigger,
        finish_frame: bool,
        ctx: &mut Context<'_>,
    ) -> Result<bool, String> {
        let before = self.state.state;
        let changed = effect_update(
            &mut self.state,
            act,
            input,
            trigger,
            (None, self.release.as_mut()),
            finish_frame,
            ctx,
        )?;
        if let Some(c) = &mut self.cumulative {
            let count = self.state.cumulative_count;
            let after = self.state.state;
            if (before == STAY && after == EXECUTE)
                || before == EXECUTE
                || (before == EXECUTING && after == EXECUTING)
            {
                self.state.cumulative_count = c.update(ctx)?;
            } else if before == END {
                c.reset();
            }
            self.cumulative_changed = count != self.state.cumulative_count;
        }
        Ok(changed)
    }
}
#[derive(Clone)]
struct Sustained {
    queue: Vec<usize>,
    current: Option<usize>,
    enabled: bool,
    timed: bool,
    executing: Vec<usize>,
}
#[inline(always)]
pub fn mission_gate(gate: Option<i32>, ctx: &Context<'_>) -> Result<bool, String> {
    let Some(gate) = gate else {
        return Ok(true);
    };
    if gate == gekisou::MISSION_ALL {
        return Ok(true);
    }
    let controller = ctx
        .gk
        .as_ref()
        .ok_or("Gekisou skill in a live without Gekisou")?
        .controller;
    if controller
        .updates
        .iter()
        .any(|&i| controller.ranges[i].mission == gate)
    {
        return Ok(true);
    }
    let i = controller.current_playing;
    Ok(i >= 0 && controller.ranges[i as usize].mission == gate)
}
#[derive(Clone, Default)]
struct IdleInputs {
    members: Vec<usize>,
    ranges: bool,
    lottery: bool,
    judgements: bool,
    rank: bool,
}
impl IdleInputs {
    fn insert(&mut self, input: IdleInput) {
        match input {
            IdleInput::Never => {}
            IdleInput::Member(member) => {
                if !self.members.contains(&member) {
                    self.members.push(member);
                }
            }
            IdleInput::Range => self.ranges = true,
            IdleInput::Lottery => self.lottery = true,
            IdleInput::Judgement => self.judgements = true,
            IdleInput::JudgementRange => {
                self.judgements = true;
                self.ranges = true;
            }
            IdleInput::Rank => self.rank = true,
        }
    }
    fn active(&self, ctx: &Context<'_>) -> bool {
        (self.ranges && IdleInput::Range.active(ctx))
            || (self.lottery && IdleInput::Lottery.active(ctx))
            || (self.judgements && IdleInput::Judgement.active(ctx))
            || (self.rank && IdleInput::Rank.active(ctx))
            || ctx
                .events
                .iter()
                .any(|event| self.members.contains(&event.0))
    }
}
#[derive(Clone)]
pub struct ConditionUpdater {
    pub effects: Vec<Effect>,
    pub gate: Option<i32>,
    pub updaters: Vec<Updater>,
    sorted: Vec<usize>,
    resets: Vec<usize>,
    stacks: Vec<Vec<usize>>,
    sustained: Vec<Option<Sustained>>,
    executing: Vec<usize>,
    execute_count: HashMap<i32, i32>,
    trigger_checked: bool,
    cache: Vec<Option<Trigger>>,
    idle_inputs: Option<IdleInputs>,
    second_phase: bool,
    first_phase: bool,
    quiet: bool,
}
impl ConditionUpdater {
    pub(crate) fn fixed_life(&mut self, life: i32) {
        for effect in &mut self.effects {
            for checker in [
                &mut effect.trigger,
                &mut effect.condition,
                &mut effect.release,
                &mut effect.reset,
            ]
            .into_iter()
            .flatten()
            {
                checker.fixed_life(life);
            }
        }
        for updater in &mut self.updaters {
            if let Some(checker) = &mut updater.release {
                checker.fixed_life(life);
            }
        }
    }
    pub fn new(effects: Vec<Effect>, gate: Option<i32>) -> Result<Self, String> {
        let mut ids: Vec<_> = effects.iter().map(|e| e.id).collect();
        ids.sort_unstable();
        if ids.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err("condition skill with a duplicate effect id".into());
        }
        let mut sorted: Vec<_> = (0..effects.len())
            .filter(|&index| !inactive(&effects[index]))
            .collect();
        sorted.sort_by_key(|&i| effects[i].id);
        let resets = effects
            .iter()
            .enumerate()
            .filter_map(|(index, effect)| {
                (!inactive(effect) && effect.reset.is_some()).then_some(index)
            })
            .collect();
        let mut stacks = vec![Vec::new(); effects.len()];
        let mut sustained = vec![None; effects.len()];
        let mut updaters = Vec::new();
        for (e, effect) in effects.iter().enumerate() {
            if inactive(effect) {
                continue;
            }
            if effect.trigger_type != ONE_SHOT && effect.trigger_type != SUSTAINED {
                continue;
            }
            let mut pool = Vec::with_capacity(5);
            for index in 0..5 {
                pool.push(updaters.len());
                updaters.push(Updater::new(
                    e,
                    index,
                    effect.release.clone(),
                    effect.phase,
                    effect.cumulative.clone(),
                ));
            }
            if effect.trigger_type == ONE_SHOT {
                stacks[e] = pool;
            } else {
                sustained[e] = Some(Sustained {
                    queue: pool,
                    current: None,
                    enabled: false,
                    timed: timed(effect.act),
                    executing: Vec::new(),
                });
            }
        }
        let mut idle = if effects.is_empty() {
            None
        } else {
            Some(IdleInputs::default())
        };
        for effect in &effects {
            let input = effect.trigger.as_ref().and_then(Checker::idle_input);
            if idle.is_none()
                || effect.trigger_type != ONE_SHOT
                || effect.reset.is_some()
                || input.is_none()
            {
                idle = None;
                break;
            }
            idle.as_mut().unwrap().insert(input.unwrap());
        }
        let count = effects.len();
        let second_phase = effects.iter().any(|effect| effect.phase == 2);
        let first_phase = effects.iter().any(|effect| effect.phase == 1);
        Ok(Self {
            effects,
            gate,
            updaters,
            sorted,
            resets,
            stacks,
            sustained,
            executing: Vec::new(),
            execute_count: HashMap::default(),
            trigger_checked: false,
            cache: vec![None; count],
            idle_inputs: idle,
            second_phase,
            first_phase,
            quiet: false,
        })
    }
    pub fn begin_frame(&mut self) {
        self.trigger_checked = false;
        self.quiet = false;
    }
    fn update_one(
        &mut self,
        index: usize,
        input: &FrameInput,
        trigger: Trigger,
        finish: bool,
        ctx: &mut Context<'_>,
    ) -> Result<bool, String> {
        let act = self.effects[self.updaters[index].effect].act;
        self.updaters[index].update(act, input, trigger, finish, ctx)
    }
    fn finish(&mut self, done: &[usize]) {
        for &u in done {
            self.stacks[self.updaters[u].effect].push(u);
            if let Some(i) = self.executing.iter().position(|&i| i == u) {
                self.executing.remove(i);
            }
        }
    }
    fn begin_checks(
        &mut self,
        input: &FrameInput,
        ctx: &mut Context<'_>,
    ) -> Result<Option<bool>, String> {
        let mut checked_gate = None;
        if !self.trigger_checked
            && self.executing.is_empty()
            && (self
                .idle_inputs
                .as_ref()
                .is_some_and(|inputs| !inputs.active(ctx))
                || input.finished
                || !{
                    let active = mission_gate(self.gate, ctx)?;
                    checked_gate = Some(active);
                    active
                })
        {
            self.trigger_checked = true;
            self.quiet = true;
        }
        if self.quiet {
            return Ok(checked_gate);
        }
        Ok(checked_gate)
    }
    fn prepare_triggers(
        &mut self,
        input: &FrameInput,
        ctx: &mut Context<'_>,
        checked_gate: Option<bool>,
    ) -> Result<bool, String> {
        self.trigger_checked = true;
        self.cache.fill(None);
        if input.finished || !checked_gate.map_or_else(|| mission_gate(self.gate, ctx), Ok)? {
            return Ok(false);
        }
        if self
            .idle_inputs
            .as_ref()
            .is_some_and(|inputs| !inputs.active(ctx))
        {
            for &index in &self.sorted {
                self.effects[index].trigger.as_mut().unwrap().override_time = None;
            }
            return Ok(false);
        }
        for &index in &self.resets {
            let effect = &mut self.effects[index];
            if effect.reset.as_mut().unwrap().check(ctx)?.0 {
                self.execute_count.remove(&effect.id);
            }
        }
        for &e in &self.sorted {
            self.cache[e] = Some(if let Some(checker) = &mut self.effects[e].trigger {
                let hit = checker.check(ctx)?.0;
                Trigger {
                    hit,
                    time: checker.override_time().unwrap_or(input.time),
                }
            } else {
                Trigger::default()
            });
        }
        Ok(true)
    }
    pub fn update(
        &mut self,
        phase: i32,
        input: &FrameInput,
        ctx: &mut Context<'_>,
        updated: &mut Vec<usize>,
    ) -> Result<(), String> {
        if phase == 2 && self.trigger_checked && !self.second_phase {
            return Ok(());
        }
        let checked_gate = self.begin_checks(input, ctx)?;
        if self.quiet {
            return Ok(());
        }
        if phase == 1 && !self.first_phase {
            if !self.trigger_checked {
                self.prepare_triggers(input, ctx, checked_gate)?;
            }
            return Ok(());
        }
        let mut done = Vec::new();
        for k in 0..self.executing.len() {
            let index = self.executing[k];
            if phase < 1 || self.updaters[index].phase == phase {
                self.update_one(index, input, Trigger::default(), false, ctx)?;
                if self.updaters[index].state.state == STAY {
                    done.push(index);
                } else {
                    updated.push(index);
                }
            }
        }
        if !self.trigger_checked && !self.prepare_triggers(input, ctx, checked_gate)? {
            self.finish(&done);
            return Ok(());
        }
        for k in 0..self.sorted.len() {
            let e = self.sorted[k];
            let Some(trigger) = self.cache[e] else {
                continue;
            };
            let effect = &self.effects[e];
            if effect.trigger_type == SUSTAINED {
                if phase < 1 || effect.phase == phase {
                    self.update_sustained(e, input, trigger, ctx, updated)?;
                }
                continue;
            }
            if effect.trigger_type != ONE_SHOT {
                return Err(format!(
                    "ArgumentOutOfRangeException: skill trigger type {}",
                    effect.trigger_type
                ));
            }
            if !trigger.hit {
                continue;
            }
            let Some(&top) = self.stacks[e].last() else {
                continue;
            };
            if phase >= 1 && self.updaters[top].phase != phase {
                continue;
            }
            if effect.execute_limit > 0
                && self
                    .execute_count
                    .get(&effect.id)
                    .is_some_and(|&count| effect.execute_limit <= count)
            {
                continue;
            }
            if let Some(condition) = &mut self.effects[e].condition {
                if !condition.check(ctx)?.0 {
                    continue;
                }
            }
            let u = self.stacks[e].pop().unwrap();
            self.update_one(
                u,
                input,
                Trigger {
                    hit: true,
                    time: trigger.time,
                },
                false,
                ctx,
            )?;
            self.executing.push(u);
            updated.push(u);
            if self.effects[e].execute_limit > 0 {
                *self.execute_count.entry(self.effects[e].id).or_default() += 1;
            }
        }
        self.finish(&done);
        Ok(())
    }
    fn update_sustained(
        &mut self,
        e: usize,
        input: &FrameInput,
        trigger: Trigger,
        ctx: &mut Context<'_>,
        updated: &mut Vec<usize>,
    ) -> Result<(), String> {
        if let Some(state) = &self.sustained[e] {
            if !trigger.hit
                && !state.enabled
                && state.current.is_none()
                && state.executing.is_empty()
            {
                return Ok(());
            }
            if !state.timed
                && trigger.hit
                && state.enabled
                && state
                    .current
                    .is_some_and(|index| self.updaters[index].state.state == EXECUTING)
                && self.effects[e]
                    .condition
                    .as_ref()
                    .is_none_or(|condition| matches!(condition.test, Test::Fixed(true)))
            {
                return Ok(());
            }
        }
        let mut state = self.sustained[e].take().ok_or("missing sustained state")?;
        let result = if state.timed {
            self.timed_step(&mut state, e, input, trigger, ctx, updated)
        } else {
            self.step(
                &mut state,
                e,
                if trigger.hit {
                    trigger.time
                } else {
                    input.time
                },
                input,
                trigger,
                ctx,
            )
            .map(|u| {
                if let Some(u) = u {
                    updated.push(u);
                }
            })
        };
        self.sustained[e] = Some(state);
        result
    }
    fn timed_step(
        &mut self,
        state: &mut Sustained,
        e: usize,
        input: &FrameInput,
        trigger: Trigger,
        ctx: &mut Context<'_>,
        updated: &mut Vec<usize>,
    ) -> Result<(), String> {
        if trigger.hit {
            let condition = self.effects[e]
                .condition
                .as_mut()
                .ok_or("timed sustained effect has no condition checker")?;
            if !state.enabled {
                condition.reset(ctx)?;
                state.enabled = true;
            }
            let mut fresh = None;
            if condition.check(ctx)?.0 {
                condition.reset(ctx)?;
                if state.queue.is_empty() {
                    return Err("sustained effect queue is empty".into());
                }
                let u = state.queue.remove(0);
                state.executing.push(u);
                fresh = Some(u);
            }
            let mut done = Vec::new();
            for &u in &state.executing {
                let tr = Trigger {
                    hit: fresh == Some(u),
                    time: if fresh == Some(u) {
                        trigger.time
                    } else {
                        input.time
                    },
                };
                if self.update_one(u, input, tr, false, ctx)? {
                    updated.push(u);
                    if self.updaters[u].state.state == STAY {
                        done.push(u);
                    }
                }
            }
            for u in done {
                state.queue.push(u);
                state.executing.retain(|&i| i != u);
            }
        } else {
            for &u in &state.executing {
                self.update_one(u, input, Trigger::default(), true, ctx)?;
                updated.push(u);
                state.queue.push(u);
            }
            state.executing.clear();
            state.enabled = false;
        }
        Ok(())
    }
    fn step(
        &mut self,
        state: &mut Sustained,
        e: usize,
        execute: i32,
        input: &FrameInput,
        trigger: Trigger,
        ctx: &mut Context<'_>,
    ) -> Result<Option<usize>, String> {
        let mut current = state.current;
        if current.is_some_and(|i| self.updaters[i].state.state == END) {
            let u = current.unwrap();
            self.update_one(u, input, Trigger::default(), false, ctx)?;
            state.queue.push(u);
            state.current = None;
            current = None;
        }
        if let Some(u) = current {
            if self.updaters[u].state.state == EXECUTE {
                self.updaters[u].state.state = EXECUTING;
            }
        }
        if trigger.hit {
            if !state.enabled {
                if let Some(condition) = &mut self.effects[e].condition {
                    condition.reset(ctx)?;
                }
                state.enabled = true;
            }
            if let Some(condition) = &mut self.effects[e].condition {
                if !condition.check(ctx)?.0 {
                    return Ok(None);
                }
            }
            if current.is_none() {
                if state.queue.is_empty() {
                    return Err("sustained effect queue is empty".into());
                }
                let u = state.queue.remove(0);
                state.current = Some(u);
                if self.update_one(
                    u,
                    input,
                    Trigger {
                        hit: true,
                        time: execute,
                    },
                    false,
                    ctx,
                )? {
                    return Ok(Some(u));
                }
            }
        } else {
            if let Some(u) = current {
                self.update_one(u, input, Trigger::default(), true, ctx)?;
                state.enabled = false;
                return Ok(Some(u));
            }
            state.enabled = false;
        }
        Ok(None)
    }
}
