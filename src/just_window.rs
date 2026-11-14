//! Native JUST windows and per-range coverage; scoring judgements stay unchanged.
mod coverage;
pub use coverage::Policy;
pub(crate) use coverage::{Segment, Statistics};
use crate::{
    full::{
        conditions::{performer_matches, Performer},
        convert::Key,
        gekisou,
        model::Model,
    },
    gekiso::Chart,
    live,
    master::{Master, SkillEffect},
    power::{Member, Snap},
    search::{Constraints, Team},
};
use rustc_hash::{FxHashMap as HashMap, FxHashSet as HashSet};
use serde::Serialize;
use serde_json::Value;
use std::sync::Arc;

pub(crate) fn target(value: &Value) -> Result<Option<f64>, String> {
    if value.is_null() {
        return Ok(None);
    }
    match value.as_f64() {
        Some(0.0) => Ok(None),
        Some(value) if value.is_finite() && value > 0.0 => Ok(Some(value)),
        _ => Err("just-window-input".into()),
    }
}

pub(crate) fn delta(master: &Master, value: i32) -> i32 {
    let base = master
        .live
        .just_windows
        .values()
        .next()
        .map_or(0, |window| window.0);
    ((value as f32 / 10000.0) * base as f32).round_ties_even() as i32
}
#[derive(Clone)]
pub(crate) struct Trace {
    pub current: HashMap<i32, (i32, i32)>,
    active: HashMap<Key, Vec<(i32, i32, i32)>>,
    probes: Option<Arc<HashMap<usize,Vec<i32>>>>,
    pub(crate) minimum: Option<[i32;2]>,
    capture: Option<coverage::Capture>,
}
impl Trace {
    pub(crate) fn new(master: &Master) -> Self {
        Self {
            current: master
                .live
                .just_windows
                .iter()
                .map(|(&key, &value)| (key, value))
                .collect(),
            active: HashMap::default(),
            probes:None,
            minimum:None,
            capture:None,
        }
    }
    pub(crate) fn record_probe(&mut self,frame:usize) {
        if let Some(kinds)=self.probes.as_ref().and_then(|probes|probes.get(&frame)) {
            for kind in kinds {
                if let Some(&(before,after))=self.current.get(kind) {
                    let value=self.minimum.get_or_insert([before,after]);value[0]=value[0].min(before);value[1]=value[1].min(after);
                }
            }
        }
        if let Some(capture)=&mut self.capture {capture.point(frame,&self.current);}
    }
    pub(crate) fn enable(&mut self, master: &Master, key: Key, value: i32, targets: &[i32],time:i32) {
        if targets.first().copied().unwrap_or(3) > 6 {
            return;
        }
        if let Some(capture)=&mut self.capture {capture.advance(time,&self.current);}
        let percent = value as f32 / 10000.0;
        let mut changed = Vec::new();
        for (&kind, &(base_before, base_after)) in &master.live.just_windows {
            let &(cap_before, cap_after) = master.live.miss_windows.get(&kind).unwrap_or(&(0, 0));
            let current = self.current.get_mut(&kind).unwrap();
            let before = ((base_before as f32 * percent).round_ties_even() as i32)
                .min(cap_before.saturating_sub(current.0))
                .max(0);
            let after = ((base_after as f32 * percent).round_ties_even() as i32)
                .min(cap_after.saturating_sub(current.1))
                .max(0);
            current.0 = current.0.wrapping_add(before);
            current.1 = current.1.wrapping_add(after);
            changed.push((kind, before, after));
        }
        self.active.insert(key, changed);
    }
    pub(crate) fn disable(&mut self, key: Key,time:i32) {
        if let Some(changed) = self.active.remove(&key) {
            if let Some(capture)=&mut self.capture {capture.advance(time,&self.current);}
            for (kind, before, after) in changed {
                let current = self.current.get_mut(&kind).unwrap();
                current.0 = current.0.wrapping_sub(before);
                current.1 = current.1.wrapping_sub(after);
            }
        }
    }
    pub(crate) fn window(&self,policy:Policy)->Option<Window> {
        let [before_ms,after_ms]=self.minimum?;
        let mut window=Window{before_ms,after_ms,meets_target:false,coverage:self.capture.as_ref().map(|capture|capture.finish(&self.current))};
        window.meets_target=policy.allows(&window);Some(window)
    }
    pub(crate) fn result(&self)->Option<Window> {self.capture.as_ref().and_then(|capture|self.window(capture.policy))}
    pub(crate) fn relocate(&mut self,frames:&[crate::full::play::Frame]) {
        if let Some(capture)=&mut self.capture {
            capture.relocate(frames);
            self.probes=Some(Arc::new(capture.points.iter().map(|(&frame,points)|(frame,points.iter().map(|p|p.kind).collect())).collect()));
        }
    }
    fn chart(master:&Master,chart:&Chart,policy:Policy)->Self {
        let mut trace=Self::new(master);
        let notes:HashMap<_,_>=chart.full.notes.iter().map(|note|(note.id,note)).collect();
        let mut probes=Vec::new();
        for (frame,row) in chart.play.frames.iter().enumerate() {
            for judged in &row.judged {
                let note=notes[&judged.note_id];
                if !crate::full::play::is_judged(note.op)||!master.live.just_types.contains(&note.judgement_type) {continue;}
                for (range,&(start,end)) in chart.setup.fevers.iter().enumerate().take(3) {
                    if chart.setup.missions[range]==3 && row.time_ms>=start && row.time_ms<end {
                        let (before,after)=master.live.just_windows[&note.judgement_type];
                        probes.push(Probe{id:note.id,range,frame,kind:note.judgement_type,before,after,cap_before:0,cap_after:0,timed:0,activated:true,full_active:true});
                    }
                }
            }
        }
        trace.probes=Some(Arc::new(probes.iter().fold(HashMap::<_,Vec<_>>::default(),|mut map,p|{map.entry(p.frame).or_default().push(p.kind);map})));
        trace.capture=Some(coverage::Capture::new(policy,chart,&probes));trace
    }
}

#[derive(Serialize)]
#[serde(rename_all="camelCase")]
pub(crate) struct WindowProfile {pub window:Window,pub segments:Vec<Segment>,pub sampled:bool}
pub(crate) struct ProfileInput<'a> {
    pub master:&'a Master,pub chart:&'a Chart,pub deck:&'a [Performer],pub power:i32,pub order:usize,
    pub seeds:&'a [i32],pub opponents:Option<&'a crate::gekiso::sections::OpponentSamples>,pub minimum:Option<Policy>,
}
pub(crate) fn describe(input:ProfileInput<'_>)->Result<Option<WindowProfile>,String> {
    let ProfileInput{master,chart,deck,power,order,seeds,opponents,minimum}=input;
    if !chart.setup.missions.contains(&3)||master.live.just_windows.is_empty() {return Ok(None);}
    let base=master.live.just_windows.get(&1).or_else(||master.live.just_windows.values().next()).unwrap();
    let policy=Policy{minimum_ms:minimum.map_or((base.0.min(base.1)+1) as f64,|p|p.minimum_ms),min_coverage:0.0,min_duration_ms:0.0,min_continuous_ms:0.0,partial:true};
    let mut template=Trace::chart(master,chart,policy);
    template.capture.as_mut().unwrap().curve=Some(Vec::new());
    let independent=deck.iter().all(|actor|!profile(master,chart,actor).dynamic);
    let runs=if independent||seeds.is_empty(){&[0][..]}else{seeds};
    let mut window:Option<Window>=None;let mut segments=Vec::new();
    for (sample,&seed) in runs.iter().enumerate() {
        let events:Vec<_>=chart.full.events.iter().map(|&(event,time)|(live::orders()[order][event],time)).collect();
        let mut params=chart.params();params.power=power;params.music_type=chart.music_type;
        let mut model=Model::new(master,deck,&chart.full.notes,&events,params,Some(&chart.setup),(seed,false))?;
        chart.fixed_life(&mut model);model.just_window=Some(template.clone());
        if let Some(cases)=opponents {model.gk.as_mut().unwrap().section_opponents=Some(Arc::new(cases[seed as usize%cases.len()].clone()));}
        let generation=crate::cancellation_generation();
        for (frame,row) in chart.play.frames.iter().enumerate() {
            if crate::cancellation_generation()!=generation {return Err("cancelled".into());}
            model.just_window.as_mut().unwrap().record_probe(frame);
            model.frame(row.time_ms,&row.judged,chart.play.dt[frame])?;
        }
        let trace=model.just_window.as_mut().unwrap();
        if sample==0 {let capture=trace.capture.as_mut().unwrap();capture.advance(capture.ranges.iter().map(|r|r.1).max().unwrap_or(0),&trace.current);segments=capture.curve.take().unwrap();}
        if let Some(value)=trace.result(){if let Some(window)=&mut window {window.merge(value);}else{window=Some(value);}}
    }
    Ok(window.map(|window|WindowProfile{window,segments,sampled:!independent&&crate::gekiso::replay_random(master,chart,deck)}))
}

#[derive(Clone, Copy, Debug, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Window {
    pub before_ms: i32,
    pub after_ms: i32,
    pub meets_target: bool,
    #[serde(default,skip_serializing_if="Option::is_none")]
    pub coverage:Option<Statistics>,
}
impl Window {
    pub(crate) fn merge(&mut self,other:Self) {
        self.before_ms=self.before_ms.min(other.before_ms);self.after_ms=self.after_ms.min(other.after_ms);self.meets_target&=other.meets_target;
        if let Some(other)=other.coverage {if let Some(own)=&mut self.coverage {own.merge(&other);}else{self.coverage=Some(other);}}
    }
}
pub(crate) fn aggregate<'a>(windows: impl Iterator<Item = &'a Value>) -> Option<Value> {
    let mut result: Option<Window> = None;
    for value in windows.filter(|window| window.is_object()) {
        let before = value["beforeMs"].as_i64().unwrap() as i32;
        let after = value["afterMs"].as_i64().unwrap() as i32;
        let meets = value["meetsTarget"].as_bool().unwrap();
        if let Some(result) = &mut result {
            result.before_ms = result.before_ms.min(before);
            result.after_ms = result.after_ms.min(after);
            result.meets_target &= meets;
        } else {
            result = Some(Window {
                before_ms: before,
                after_ms: after,
                meets_target: meets,
                coverage:None,
            });
        }
    }
    result.map(|window| serde_json::to_value(window).unwrap())
}

#[derive(Clone, Copy, Default)]
struct Profile {
    full: i32,
    timed: [i32; 4],
    dynamic: bool,
    random: bool,
}
impl Profile {
    fn extra(&self, mask: u8) -> i64 {
        (0..4)
            .filter(|&index| mask & (1 << index) != 0)
            .fold(self.full as i64, |sum, index| {
                sum + self.timed[index] as i64
            })
    }
    fn at(&self,phase:Probe)->i64 {
        (if phase.full_active {self.full as i64}else{0})+(0..4).filter(|&index|phase.timed&(1<<index)!=0).map(|index|self.timed[index] as i64).sum::<i64>()
    }
}
#[derive(Clone, Copy)]
struct Probe {
    id:i32,
    range:usize,
    frame: usize,
    before: i32,
    after: i32,
    cap_before: i32,
    cap_after: i32,
    kind: i32,
    timed: u8,
    activated: bool,
    full_active:bool,
}

#[derive(Clone,Copy)]
struct Span {range:usize,start:i32,end:i32,mask:u8}
fn group_phases(probes:&[Probe])->Vec<Probe> {
    let mut phases:Vec<Probe>=Vec::new();
    for &probe in probes {
        if !phases.iter().any(|p|same_phase(*p,probe)) {phases.push(probe);}
    }
    phases
}
fn same_phase(a:Probe,b:Probe)->bool {
    a.timed==b.timed&&a.activated==b.activated&&a.full_active==b.full_active&&a.cap_before==b.cap_before&&a.cap_after==b.cap_after&&a.before==b.before&&a.after==b.after
}

type WindowProgram = Vec<(u8, i32, i32, u64, i32)>;

pub(crate) struct Prepared {
    policy:Policy,
    minimum: f64,
    width: usize,
    members: Arc<Vec<Member>>,
    snaps: Arc<Vec<Snap>>,
    profiles: Vec<Profile>,
    programs: Vec<WindowProgram>,
    probes: Vec<Probe>,
    phases: Vec<Probe>,
    phase_values: Vec<Vec<f64>>,
    phase_notes:Vec<[u32;3]>,
    spans:Vec<Span>,
    assignment: Option<Assignment>,
    seeds: Vec<i32>,
    cache: HashMap<(Vec<usize>, Vec<i32>, i32), Option<Window>>,
    nominal: bool,
    dominance_safe: bool,
}
impl Prepared {
    pub(crate) fn capture_trace(&self,master:&Master,chart:&Chart)->Trace {
        let mut trace=Trace::new(master);
        let mut probes:HashMap<usize,Vec<i32>>=HashMap::default();
        for probe in &self.probes {probes.entry(probe.frame).or_default().push(probe.kind);}
        trace.probes=Some(Arc::new(probes));trace.capture=Some(coverage::Capture::new(self.policy,chart,&self.probes));trace
    }
}

struct Assignment {
    members: Vec<usize>,
    best: Vec<[Vec<(i32, f64)>; 5]>,
}

fn random_row(master: &Master, row: &SkillEffect) -> bool {
    [
        row.trigger_group,
        row.condition_group,
        row.release_group,
        row.reset_group,
    ]
    .iter()
    .flat_map(|group| master.condition_sets.get(group))
    .flatten()
    .flatten()
    .any(|id| {
        master
            .conditions
            .get(id)
            .is_some_and(|condition| condition.kind == 4011)
    })
}

fn static_condition(master: &Master, group: i32, actor: &Performer) -> Option<bool> {
    if group == 0 {
        return Some(true);
    }
    let sets = master.condition_sets.get(&group)?;
    let mut any = false;
    for set in sets {
        let mut all = true;
        for id in set {
            let condition = master.conditions.get(id)?;
            if condition.kind != 5000 || !condition.values.is_empty() {
                return None;
            }
            let mut hit = false;
            for target in &condition.targets {
                hit |= performer_matches(actor, master.targets.get(target)?);
            }
            all &= hit == condition.positive;
        }
        any |= all;
    }
    Some(any)
}

fn profile_row(
    master: &Master,
    row: &SkillEffect,
    actor: &Performer,
    out: &mut Profile,
) -> Option<()> {
    if master.live.phases.get(&4004) != Some(&2) {
        return None;
    }
    if row.targets.len() != 1
        || master.targets.get(&row.targets[0])?.judgement != 6
        || row.value < 0
    {
        return None;
    }
    let base = *master.live.just_windows.values().next()?;
    if base.0 != base.1
        || master
            .live
            .just_windows
            .values()
            .any(|&window| window != base)
    {
        return None;
    }
    if row.cumulative_id != 0
        || row.limit_count != 0
        || row.execute_limit != 0
        || row.max_value != 0
        || row.reset_group != 0
        || row.release_group != 0
    {
        return None;
    }
    let sets = master.condition_sets.get(&row.trigger_group)?;
    if sets.len() != 1 || sets[0].len() != 1 {
        return None;
    }
    let trigger = master.conditions.get(&sets[0][0])?;
    if !trigger.positive || !trigger.values.is_empty() || trigger.targets.is_empty() {
        return None;
    }
    for target in &trigger.targets {
        if master.targets.get(target)?.mission_type != 3 {
            return None;
        }
    }
    let timed = if row.trigger_type == 1
        && trigger.kind == 7010
        && [2.0, 3.0, 4.0, 5.0].contains(&row.seconds)
    {
        Some(row.seconds as usize - 2)
    } else if row.trigger_type == 2 && trigger.kind == 7020 && row.seconds == 0.0 {
        None
    } else {
        return None;
    };
    if static_condition(master, row.condition_group, actor)? {
        let amount = delta(master, row.value);
        if let Some(index) = timed {
            out.timed[index] = out.timed[index].checked_add(amount)?;
        } else {
            out.full = out.full.checked_add(amount)?;
        }
    }
    Some(())
}

fn profile(master: &Master, chart: &Chart, actor: &Performer) -> Profile {
    let dynamic = actor
        .live_skill
        .into_iter()
        .chain(actor.support_skills.iter().copied())
        .enumerate()
        .any(|(index, (id, level))| {
            let skills = if index == 0 && actor.live_skill.is_some() {
                &master.live_skills
            } else {
                &master.support_skills
            };
            skills.get(&id).is_some_and(|rows| {
                rows.iter()
                    .any(|row| row.level == level && row.kind == 4004)
            })
        });
    let mut out = Profile {
        dynamic,
        ..Profile::default()
    };
    out.random =
        actor
            .live_skill
            .into_iter()
            .filter_map(|(id, level)| master.live_skills.get(&id).map(|rows| (rows, level)))
            .chain(actor.support_skills.iter().filter_map(|&(id, level)| {
                master.support_skills.get(&id).map(|rows| (rows, level))
            }))
            .any(|(rows, level)| {
                rows.iter()
                    .any(|row| row.level == level && random_row(master, row))
            });
    if actor.gekisou_skill.is_none() && actor.mission_only.unwrap_or(0) == 0 {
        return out;
    }
    let mut window_keys = HashSet::default();
    out.dynamic |= actor.gekisou_support_skills.iter().filter_map(|&(id, level)| {
        master.gekiso_support_skills.get(&id).map(|skill| (skill, level))
    }).filter(|(skill, _)| skill.mission_type == 4 || chart.setup.missions.contains(&skill.mission_type))
        .flat_map(|(skill, level)| skill.effects.iter().filter(move |row| row.level == level && row.kind == 4004))
        .any(|row| !window_keys.insert(row.id));
    let rows = actor
        .gekisou_skill
        .into_iter()
        .filter_map(|(id, level)| master.gekiso_skills.get(&id).map(|skill| (skill, level)))
        .chain(
            actor
                .gekisou_support_skills
                .iter()
                .filter_map(|&(id, level)| {
                    master
                        .gekiso_support_skills
                        .get(&id)
                        .map(|skill| (skill, level))
                }),
        );
    for (skill, level) in rows {
        if skill.mission_type != 4 && !chart.setup.missions.contains(&skill.mission_type) {
            continue;
        }
        out.random |= skill
            .effects
            .iter()
            .any(|row| row.level == level && random_row(master, row));
        for row in skill
            .effects
            .iter()
            .filter(|row| row.level == level && row.kind == 4004)
        {
            if profile_row(master, row, actor, &mut out).is_none() {
                out.dynamic = true;
            }
        }
    }
    out
}

fn window_program(master: &Master, chart: &Chart, actor: &Performer) -> WindowProgram {
    if actor.gekisou_skill.is_none() && actor.mission_only.unwrap_or(0) == 0 {
        return Vec::new();
    }
    let mut program = Vec::new();
    for (source, id, level) in actor.gekisou_skill.into_iter().map(|(id, level)| (4, id, level))
        .chain(actor.gekisou_support_skills.iter().map(|&(id, level)| (5, id, level))) {
        let skills = if source == 4 { &master.gekiso_skills } else { &master.gekiso_support_skills };
        let Some(skill) = skills.get(&id) else { continue; };
        if skill.mission_type != 4 && !chart.setup.missions.contains(&skill.mission_type) { continue; }
        let mut rows: Vec<_> = skill.effects.iter().filter(|row| row.level == level && row.kind == 4004
            && static_condition(master, row.condition_group, actor) == Some(true)).collect();
        rows.sort_by_key(|row| row.id);
        program.extend(rows.into_iter().map(|row| (source, row.id, row.value, row.seconds.to_bits(), skill.mission_type)));
    }
    program
}

impl Prepared {
    pub(crate) fn new<P:Into<Policy>>(
        master: &Master,
        chart: &Chart,
        members: &[Member],
        snaps: &[Snap],
        policy: P,
        seeds: &[i32],
    ) -> Result<Self, String> {
        let policy=policy.into();
        let minimum=policy.minimum_ms;
        let width = snaps.len() + 1;
        let mut probes = Vec::new();
        if chart.setup.missions.contains(&3) {
            let mut empty = Model::new(
                master,
                &[],
                &chart.full.notes,
                &[],
                chart.params(),
                Some(&chart.setup),
                (0, false),
            )?;
            let mut starts = [usize::MAX; 3];
            let notes: HashMap<_, _> = chart
                .full
                .notes
                .iter()
                .map(|note| (note.id, note))
                .collect();
            let mut per_range = [Vec::new(), Vec::new(), Vec::new()];
            for (frame_index, frame) in chart.play.frames.iter().enumerate() {
                empty.frame(frame.time_ms, &frame.judged, chart.play.dt[frame_index])?;
                if let Some(gk) = &empty.gk {
                    for (range, state) in gk.controller.states.iter().enumerate().take(3) {
                        if state.state == gekisou::START {
                            starts[range] = starts[range].min(frame_index);
                        }
                    }
                }
                for judged in &frame.judged {
                    let note = notes[&judged.note_id];
                    if !policy.partial && note.op != 1 || policy.partial && !crate::full::play::is_judged(note.op) {
                        continue;
                    }
                    for (range, &(start, end)) in chart.setup.fevers.iter().enumerate().take(3) {
                        if chart.setup.missions[range] != 3
                            || frame.time_ms < start
                            || frame.time_ms >= end
                        {
                            continue;
                        }
                        if master.live.just_windows.is_empty()
                            || !master.live.timing_types.contains(&note.judgement_type)
                        {
                            return Err("just-window-timing".into());
                        }
                        if !master.live.just_types.contains(&note.judgement_type) {
                            continue;
                        }
                        let &(before, after) = master
                            .live
                            .just_windows
                            .get(&note.judgement_type)
                            .ok_or("just-window-timing")?;
                        let activated = frame_index > starts[range];
                        let mut timed = 0u8;
                        for index in 0..4 {
                            let finish = start.saturating_add((index + 2) * 1000);
                            let end_frame =
                                chart.play.frames.partition_point(|f| f.time_ms < finish);
                            if frame_index <= end_frame {
                                timed |= 1 << index;
                            }
                        }
                        let (cap_before, cap_after) = master
                            .live
                            .miss_windows
                            .get(&note.judgement_type)
                            .copied()
                            .ok_or("just-window-timing")?;
                        per_range[range].push(Probe {
                            id:note.id,
                            range,
                            frame: frame_index,
                            before,
                            after,
                            cap_before,
                            cap_after,
                            kind: note.judgement_type,
                            timed,
                            activated,
                            full_active:activated,
                        });
                    }
                }
            }
            for rows in per_range {
                if !policy.partial && rows.iter().any(|row| row.activated) {
                    probes.extend(rows.into_iter().filter(|row| row.activated));
                } else {
                    probes.extend(rows);
                }
            }
        }
        let profiles: Vec<Profile> = members
            .iter()
            .flat_map(|member| {
                snaps
                    .iter()
                    .map(Some)
                    .chain(std::iter::once(None))
                    .map(move |snap| {
                        profile(master, chart, &Performer::from_member(master, member, snap))
                    })
            })
            .collect();
        let programs = members.iter().flat_map(|member| snaps.iter().map(Some).chain(std::iter::once(None))
            .map(move |snap| window_program(master, chart, &Performer::from_member(master, member, snap)))).collect();
        let mut spans=Vec::new();
        if policy.partial {
            let basis=coverage::basis(master,chart,members,snaps,&profiles)?;
            for probe in &mut probes {
                let mask=basis.before[probe.frame];probe.timed=mask&15;probe.full_active=mask&16!=0;probe.activated=mask!=0;
            }
            for (range,&(start,end)) in chart.setup.fevers.iter().enumerate().take(3).filter(|&(i,_)|chart.setup.missions[i]==3) {
                let mut at=start;let mut mask=0;
                for &(time,next) in &basis.changes {
                    if time>=end {break;}
                    if time>at {spans.push(Span{range,start:at,end:time,mask});at=time;}
                    mask=next;
                }
                if at<end {spans.push(Span{range,start:at,end,mask});}
            }
        }
        let mut phases=group_phases(&probes);
        if !policy.partial && profiles.iter().all(|profile| !profile.dynamic) {
            let mut distinct: Vec<Probe> = Vec::new();
            for phase in phases {
                let redundant = distinct.iter().any(|old| {
                    old.before == phase.before
                        && old.after == phase.after
                        && old.cap_before == phase.cap_before
                        && old.cap_after == phase.cap_after
                        && old.activated == phase.activated
                        && profiles
                            .iter()
                            .all(|row| row.at(*old) == row.at(phase))
                });
                if !redundant {
                    distinct.push(phase);
                }
            }
            phases = distinct;
        }
        let sustained_only = profiles.iter().all(|profile| profile.timed == [0; 4]);
        let dominance_safe = profiles.iter().all(|profile| !profile.dynamic)
            && (sustained_only || phases.iter().all(|phase| {
                let maximum = profiles
                    .iter()
                    .map(|row| {
                        if phase.activated {
                            row.at(*phase)
                        } else {
                            0
                        }
                    })
                    .max()
                    .unwrap_or(0);
                maximum >= 0
                    && maximum.checked_mul(5).is_some_and(|extra| {
                        extra <= i32::MAX as i64
                            && phase.before as i64 + extra <= phase.cap_before as i64
                            && phase.after as i64 + extra <= phase.cap_after as i64
                    })
            }));
        let phase_values: Vec<Vec<f64>> = phases
            .iter()
            .map(|phase| {
                profiles
                    .iter()
                    .map(|row| {
                        if row.dynamic {
                            f64::INFINITY
                        } else if !phase.activated {
                            0.0
                        } else {
                            row.at(*phase) as f64
                        }
                    })
                    .collect()
            })
            .collect();
        let mut phase_notes=vec![[0;3];phases.len()];
        if policy.partial {for probe in &probes {let index=phases.iter().position(|phase|same_phase(*phase,*probe)).unwrap();phase_notes[index][probe.range]+=1;}}
        Ok(Self {
            policy,
            minimum,
            width,
            members: Arc::new(members.to_vec()),
            snaps: Arc::new(snaps.to_vec()),
            profiles,
            programs,
            probes,
            phases,
            phase_values,
            phase_notes,
            spans,
            assignment: None,
            seeds: if seeds.is_empty() {
                vec![0]
            } else {
                seeds.to_vec()
            },
            cache: HashMap::default(),
            nominal: seeds.is_empty(),
            dominance_safe,
        })
    }
    pub(crate) fn measure_full(
        &mut self,
        master: &Master,
        chart: &Chart,
        team: &Team,
        power: i32,
    ) -> Result<Option<Window>, String> {
        self.measure(master, chart, team, power)
    }
    pub(crate) fn active(&self) -> bool {
        !self.probes.is_empty()
    }
    pub(crate) fn snap_dominates(&self, member: usize, better: i32, worse: i32) -> bool {
        if better == worse || !self.active() {
            return true;
        }
        let column = |snap: i32| {
            member * self.width
                + if snap < 0 {
                    self.width - 1
                } else {
                    snap as usize
                }
        };
        if !self.dominance_safe {
            let (better, worse) = (column(better), column(worse));
            return !self.profiles[better].dynamic && !self.profiles[worse].dynamic
                && self.programs[better] == self.programs[worse];
        }
        self.phase_values
            .iter()
            .all(|values| values[column(better)] >= values[column(worse)])
    }
    pub(crate) fn permutation_invariant(&self) -> bool {
        !self.active() || self.dominance_safe
    }
    pub(crate) fn member_dominates(&self, better: usize, worse: usize) -> bool {
        if better == worse || !self.active() {
            return true;
        }
        if (self.members[better].card.gekiso_skill_id == 0)
                != (self.members[worse].card.gekiso_skill_id == 0)
        {
            return false;
        }
        if !self.dominance_safe {
            return (0..self.width).all(|column| {
                let (a, b) = (better * self.width + column, worse * self.width + column);
                !self.profiles[a].dynamic && !self.profiles[b].dynamic && self.programs[a] == self.programs[b]
            });
        }
        self.phase_values.iter().all(|values| {
            (0..self.width).all(|column| {
                values[better * self.width + column] >= values[worse * self.width + column]
            })
        })
    }
    pub(crate) fn pricing_constraints(&self) -> Option<Vec<(f64, &[f64])>> {
        // Unclipped sums remain upper bounds when the cap changes expiration amounts.
        if self.policy.partial || self.profiles.iter().any(|profile| profile.dynamic) || !self.active() {
            return None;
        }
        let mut constraints = Vec::<(f64, &[f64])>::new();
        for (phase, values) in self.phases.iter().zip(&self.phase_values) {
            let required = (self.minimum - phase.before.min(phase.after) as f64).next_down();
            if required <= 0.0 { continue; }
            if let Some((previous, _)) = constraints.iter_mut().find(|(_, row)| *row == values.as_slice()) {
                *previous = previous.max(required);
            } else { constraints.push((required, values)); }
        }
        Some(constraints)
    }
    pub(crate) fn impossible(&self) -> bool {
        self.impossible_with(&Constraints::default())
    }
    fn partial_possible(&self,possible:&[bool])->bool {
        (0..3).all(|range| {
            let notes=self.phase_notes.iter().map(|counts|counts[range]).sum::<u32>();
            if notes==0 {return true;}
            let covered=self.phase_notes.iter().zip(possible).filter(|(_,possible)|**possible).map(|(counts,_)|counts[range]).sum::<u32>();
            let duration=self.spans.iter().filter(|span|span.range==range).map(|span|(span.end-span.start) as f64).sum::<f64>();
            covered>0 && covered as f64/notes as f64+1e-12>=self.policy.min_coverage
                && duration+1e-8>=self.policy.min_duration_ms.max(self.policy.min_continuous_ms)
        })
    }
    pub(crate) fn impossible_with(&self, constraints: &Constraints) -> bool {
        if self.phases.is_empty() { return false; }
        let mut fixed: HashSet<_> = constraints.required_members.iter().copied()
            .chain(constraints.bindings.keys().copied()).collect();
        if let Some(leader) = constraints.leader {
            if leader < 0 { return true; }
            fixed.insert(leader as usize);
        }
        let characters: HashSet<_> = fixed.iter().map(|&member| self.members[member].card.character_id).collect();
        if fixed.len() > 5 || characters.len() != fixed.len()
            || fixed.iter().any(|member| constraints.excluded_members.contains(member)) { return true; }
        let mut possible=self.policy.partial.then(||vec![true;self.phases.len()]);
        for (index,(phase, values)) in self.phases.iter().zip(&self.phase_values).enumerate() {
            if (phase.cap_before as f64) < self.minimum || (phase.cap_after as f64) < self.minimum {
                if self.policy.partial {possible.as_mut().unwrap()[index]=false;continue;}return true;
            }
            let mut fixed_upper = 0.0;
            let mut free = HashMap::<i32, f64>::default();
            let mut fixed_base = 0.0;
            let mut free_base = HashMap::<i32, f64>::default();
            let mut gains = vec![0.0_f64; self.width - 1];
            let mut finite_gains = true;
            for (member, card) in self.members.iter().enumerate() {
                if constraints.excluded_members.contains(&member) { continue; }
                let value = |snap: i32| {
                    if snap >= 0 && (constraints.no_snaps
                        || constraints.excluded_snaps.contains(&(snap as usize))) { return f64::NEG_INFINITY; }
                    values[member * self.width + if snap < 0 { self.width - 1 } else { snap as usize }]
                };
                let high = constraints.bindings.get(&member).map_or_else(|| {
                    (0..self.width).filter_map(|column| {
                        let snap = if column == self.width - 1 { -1 } else { column as i32 };
                        (!constraints.bindings.values().any(|&bound| bound >= 0 && bound == snap)).then(|| value(snap))
                    }).fold(f64::NEG_INFINITY, f64::max)
                }, |&snap| value(snap));
                let base = if constraints.bindings.contains_key(&member) { high } else { value(-1) };
                if fixed.contains(&member) {
                    if high == f64::NEG_INFINITY { return true; }
                    fixed_upper += high;
                    fixed_base += base;
                } else if high != f64::NEG_INFINITY && !characters.contains(&card.card.character_id) {
                    let best = free.entry(card.card.character_id).or_insert(f64::NEG_INFINITY);
                    *best = best.max(high);
                    let best = free_base.entry(card.card.character_id).or_insert(f64::NEG_INFINITY);
                    *best = best.max(base);
                }
                if !constraints.no_snaps && !constraints.bindings.contains_key(&member) {
                    for (snap, gain) in gains.iter_mut().enumerate() {
                        if constraints.excluded_snaps.contains(&snap)
                            || constraints.bindings.values().any(|&bound| bound == snap as i32) { continue; }
                        let extra = value(snap as i32) - base;
                        if extra.is_finite() { *gain = gain.max(extra); } else { finite_gains = false; }
                    }
                }
            }
            let remaining = 5 - fixed.len();
            if free.len() < remaining { return true; }
            let mut best: Vec<_> = free.into_values().collect();
            best.sort_unstable_by(|a, b| b.total_cmp(a));
            let mut ceiling = best.into_iter().take(remaining).fold(fixed_upper, |sum, value| sum + value);
            if finite_gains {
                // Separate character bases and distinct snapshot gains relax the same legal formation.
                let mut bases: Vec<_> = free_base.into_values().collect();
                bases.sort_unstable_by(|a, b| b.total_cmp(a));
                gains.sort_unstable_by(|a, b| b.total_cmp(a));
                let base = bases.into_iter().take(remaining).fold(fixed_base, |sum, value| sum + value);
                let snaps = gains.into_iter().take(5 - constraints.bindings.len()).sum::<f64>();
                ceiling = ceiling.min(base + snaps);
            }
            if ceiling >= i32::MIN as f64 && ceiling <= i32::MAX as f64
                && (phase.before as f64 + ceiling < self.minimum || phase.after as f64 + ceiling < self.minimum) {
                    if self.policy.partial {possible.as_mut().unwrap()[index]=false;}else{return true;}
                }
        }
        possible.as_ref().is_some_and(|possible|!self.partial_possible(possible))
    }
    pub(crate) fn prepare_assignment(&mut self, members: &[usize], options: &[Vec<i32>]) {
        self.assignment = None;
        if members.len() != 5 || options.len() != 5 || self.phases.is_empty() { return; }
        let best = self.phase_values.iter().map(|values| std::array::from_fn(|slot| {
            let mut row: Vec<_> = options[slot].iter().map(|&snap| {
                let col = if snap < 0 { self.width - 1 } else { snap as usize };
                (snap, values[members[slot] * self.width + col])
            }).collect();
            let compare = |a: &(i32, f64), b: &(i32, f64)| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0));
            if row.len() > 6 { row.select_nth_unstable_by(5, compare); row.truncate(6); }
            row.sort_unstable_by(compare); row
        })).collect();
        self.assignment = Some(Assignment { members: members.to_vec(), best });
    }
    pub(crate) fn possible_prepared(&self, members: &[usize], assigned: &[i32], from: usize, options: &[Vec<i32>]) -> bool {
        let Some(assignment) = self.assignment.as_ref().filter(|a| a.members == members) else {
            return self.possible(members, assigned, from, options);
        };
        let mut possible=self.policy.partial.then(||vec![true;self.phases.len()]);
        for (index,((phase, values), best)) in self.phases.iter().zip(&self.phase_values).zip(&assignment.best).enumerate() {
            let mut upper = 0.0;
            for (slot, &member) in members.iter().enumerate() {
                upper += if slot < from {
                    let snap = assigned[slot];
                    values[member * self.width + if snap < 0 { self.width - 1 } else { snap as usize }]
                } else {
                    best[slot].iter().find(|(snap, _)| *snap < 0 || !assigned[..from].contains(snap))
                        .map_or(f64::NEG_INFINITY, |(_, value)| *value)
                };
            }
            if upper < i32::MIN as f64 || upper > i32::MAX as f64 { continue; }
            if phase.before as f64 + upper < self.minimum || phase.after as f64 + upper < self.minimum {
                if self.policy.partial {possible.as_mut().unwrap()[index]=false;}else{return false;}
            }
        }
        possible.as_ref().is_none_or(|possible|self.partial_possible(possible))
    }
    pub(crate) fn possible(
        &self,
        members: &[usize],
        assigned: &[i32],
        from: usize,
        options: &[Vec<i32>],
    ) -> bool {
        if self.phases.is_empty() || options.len() != members.len() {
            return true;
        }
        let mut possible=self.policy.partial.then(||vec![true;self.phases.len()]);
        for (index,(phase, values)) in self.phases.iter().zip(&self.phase_values).enumerate() {
            let mut upper = 0.0;
            for (slot, &member) in members.iter().enumerate() {
                let value = |snap: i32| {
                    values[member * self.width
                        + if snap < 0 {
                            self.width - 1
                        } else {
                            snap as usize
                        }]
                };
                let high = if slot < from {
                    value(assigned[slot])
                } else {
                    options[slot]
                        .iter()
                        .copied()
                        .filter(|snap| *snap < 0 || !assigned[..from].contains(snap))
                        .map(value)
                        .fold(f64::NEG_INFINITY, f64::max)
                };
                upper += high;
            }
            if upper < i32::MIN as f64 || upper > i32::MAX as f64 {
                continue;
            }
            if phase.before as f64 + upper < self.minimum
                || phase.after as f64 + upper < self.minimum
            {
                if self.policy.partial {possible.as_mut().unwrap()[index]=false;}else{return false;}
            }
        }
        possible.as_ref().is_none_or(|possible|self.partial_possible(possible))
    }
    pub(crate) fn allows(
        &mut self,
        master: &Master,
        chart: &Chart,
        team: &Team,
        power: i32,
    ) -> Result<bool, String> {
        Ok(self
            .measure(master, chart, team, power)?
            .is_none_or(|window| window.meets_target))
    }
    pub(crate) fn measure(
        &mut self,
        master: &Master,
        chart: &Chart,
        team: &Team,
        power: i32,
    ) -> Result<Option<Window>, String> {
        if self.probes.is_empty() {
            return Ok(None);
        }
        let mut full = 0i32;
        let mut timed = [0i32; 4];
        let mut dynamic = false;
        let mut random = false;
        let mut maximum_extra=0_i64;
        for (&member, &snap) in team.members.iter().zip(&team.snaps) {
            let row = self.profiles[member * self.width
                + if snap < 0 {
                    self.width - 1
                } else {
                    snap as usize
                }];
            full = full.wrapping_add(row.full);
            for (sum, value) in timed.iter_mut().zip(row.timed) {
                *sum = sum.wrapping_add(value);
            }
            dynamic |= row.dynamic;
            random |= row.random;
            maximum_extra+=row.extra(15);
        }
        let order_dependent = dynamic;
        dynamic |= maximum_extra > i64::from(i32::MAX) || timed.iter().any(|&value| value != 0) && master.live.just_windows.iter().any(|(kind,&(early,late))| {
            let &(cap_early,cap_late)=master.live.miss_windows.get(kind).unwrap_or(&(0,0));
            maximum_extra>i64::from((cap_early-early).min(cap_late-late))
        });
        let (mut before, mut after) = (i32::MAX, i32::MAX);
        if !dynamic {
            let mut capture=self.policy.partial.then(||coverage::Capture::new(self.policy,chart,&[]));
            for (index,phase) in self.phases.iter().enumerate() {
                let extra = if !phase.activated {
                    0
                } else {
                    (0..4)
                        .filter(|&index| phase.timed & (1 << index) != 0)
                        .fold(if phase.full_active {full}else{0}, |sum, index| sum.wrapping_add(timed[index]))
                };
                let early=
                    phase.before.wrapping_add(
                        extra
                            .min(phase.cap_before.saturating_sub(phase.before))
                            .max(0),
                    );
                let late=
                    phase.after.wrapping_add(
                        extra
                            .min(phase.cap_after.saturating_sub(phase.after))
                            .max(0),
                    );
                before=before.min(early);after=after.min(late);
                if let Some(capture) = &mut capture {
                    for (range, &notes) in self.phase_notes[index].iter().enumerate() {
                        capture.add_points(range, notes, early, late);
                    }
                }
            }
            if let Some(capture)=&mut capture {
                let &(base_before,base_after)=master.live.just_windows.get(&1).or_else(||master.live.just_windows.values().next()).unwrap();
                let &(cap_before,cap_after)=master.live.miss_windows.get(&1).or_else(||master.live.miss_windows.values().next()).unwrap();
                for span in &self.spans {
                    let extra=(0..4).filter(|&index|span.mask&(1<<index)!=0).fold(if span.mask&16!=0 {full}else{0},|sum,index|sum.wrapping_add(timed[index]));
                    capture.interval(span.range,span.start,span.end,base_before.saturating_add(extra).min(cap_before),base_after.saturating_add(extra).min(cap_after));
                }
                capture.complete_static();
            }
            let mut result=Window{before_ms:before,after_ms:after,meets_target:false,
                coverage:capture.map(|capture|capture.finish(&HashMap::default()))};
            result.meets_target=self.policy.allows(&result);
            Ok(Some(result))
        } else {
            if order_dependent && self.nominal && (chart.luck || random) {
                return Err("just-window-random".into());
            }
            let key = (team.members.clone(), team.snaps.clone(), power);
            if let Some(value) = self.cache.get(&key) {
                return Ok(*value);
            }
            let deck: Vec<_> = team
                .members
                .iter()
                .zip(&team.snaps)
                .map(|(&member, &snap)| {
                    Performer::from_member(
                        master,
                        &self.members[member],
                        (snap >= 0).then(|| &self.snaps[snap as usize]),
                    )
                })
                .collect();
            let trace=self.capture_trace(master,chart);
            let mut result:Option<Window>=None;
            for &seed in self
                .seeds
                .iter()
                .take(if order_dependent { self.seeds.len() } else { 1 })
            {
                for order in live::orders().iter().take(if order_dependent { 120 } else { 1 }) {
                    let events: Vec<_> = chart
                        .full
                        .events
                        .iter()
                        .map(|&(event, time)| (order[event], time))
                        .collect();
                    let mut params = chart.params();
                    params.power = power;
                    let mut model = Model::new(
                        master,
                        &deck,
                        &chart.full.notes,
                        &events,
                        params,
                        Some(&chart.setup),
                        (seed, false),
                    )?;
                    chart.fixed_life(&mut model);
                    model.just_window = Some(trace.clone());
                    for (index, frame) in chart.play.frames.iter().enumerate() {
                        model.just_window.as_mut().unwrap().record_probe(index);
                        model.frame(frame.time_ms, &frame.judged, chart.play.dt[index])?;
                    }
                    if let Some(window)=model.just_window.as_ref().unwrap().window(self.policy) {
                        if let Some(result)=&mut result {result.merge(window);}else{result=Some(window);}
                    }
                }
            }
            if self.cache.len() >= 128 {
                self.cache.clear();
            }
            self.cache.insert(key, result);
            Ok(result)
        }
    }
}
