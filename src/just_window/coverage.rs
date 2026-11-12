use super::*;
use serde::Deserialize;

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Policy {
    pub minimum_ms: f64,
    pub min_coverage: f64,
    pub min_duration_ms: f64,
    pub min_continuous_ms: f64,
    pub partial: bool,
}
impl From<f64> for Policy {
    fn from(minimum_ms: f64) -> Self {
        Self { minimum_ms, min_coverage: 1.0, min_duration_ms: 0.0, min_continuous_ms: 0.0, partial: false }
    }
}
impl Policy {
    pub(crate) fn read(options: &Value) -> Result<Option<Self>, String> {
        let value = &options["justWindow"];
        if value.is_null() { return Ok(target(&options["minJustWindowMs"])?.map(Self::from)); }
        let Some(minimum_ms) = target(&value["minimumMs"])? else { return Err("just-window-input".into()); };
        let min_coverage = value["minCoverage"].as_f64().unwrap_or(0.0);
        let min_duration_ms = value["minDurationMs"].as_f64().unwrap_or(0.0);
        let min_continuous_ms = value["minContinuousMs"].as_f64().unwrap_or(0.0);
        if !min_coverage.is_finite() || !(0.0..=1.0).contains(&min_coverage)
            || !min_duration_ms.is_finite() || !(0.0..=i32::MAX as f64).contains(&min_duration_ms)
            || !min_continuous_ms.is_finite() || !(0.0..=i32::MAX as f64).contains(&min_continuous_ms) {
            return Err("just-window-coverage-input".into());
        }
        Ok(Some(Self { minimum_ms, min_coverage, min_duration_ms, min_continuous_ms, partial: true }))
    }
    pub(crate) fn key(self) -> [u64; 5] {
        [self.minimum_ms.to_bits(), self.min_coverage.to_bits(), self.min_duration_ms.to_bits(), self.min_continuous_ms.to_bits(), u64::from(self.partial)]
    }
    pub(crate) fn allows(self, window: &Window) -> bool {
        if !self.partial {
            return window.before_ms.min(window.after_ms) as f64 >= self.minimum_ms;
        }
        window.coverage.as_ref().is_some_and(|stats| stats.ranges.iter().filter(|range|range.notes>0).all(|range| {
            range.minimum_coverage + 1e-12 >= self.min_coverage
                && range.minimum_covered_ms + 1e-8 >= self.min_duration_ms
                && range.minimum_longest_ms + 1e-8 >= self.min_continuous_ms
                && range.minimum_covered_notes >= 1
        }))
    }
}

#[derive(Clone, Copy, Default, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RangeStatistics {
    pub notes: u32,
    pub covered_notes: f64,
    pub minimum_covered_notes: u32,
    pub coverage: f64,
    pub minimum_coverage: f64,
    pub duration_ms: f64,
    pub covered_ms: f64,
    pub minimum_covered_ms: f64,
    pub longest_ms: f64,
    pub minimum_longest_ms: f64,
    pub peak_ms: i32,
    pub minimum_peak_ms: i32,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Statistics {
    pub target_ms: f64,
    pub plays: u64,
    pub ranges: [RangeStatistics; 3],
}
impl Statistics {
    pub(crate) fn merge(&mut self, other: &Self) {
        let count = self.plays + other.plays;
        let average = |a: f64, b: f64| (a * self.plays as f64 + b * other.plays as f64) / count as f64;
        for (a,b) in self.ranges.iter_mut().zip(other.ranges) {
            if b.notes==0 { continue; }
            if a.notes==0 { *a=b; continue; }
            a.covered_notes=average(a.covered_notes,b.covered_notes);
            a.coverage=average(a.coverage,b.coverage);
            a.covered_ms=average(a.covered_ms,b.covered_ms);
            a.longest_ms=average(a.longest_ms,b.longest_ms);
            a.minimum_covered_notes=a.minimum_covered_notes.min(b.minimum_covered_notes);
            a.minimum_coverage=a.minimum_coverage.min(b.minimum_coverage);
            a.minimum_covered_ms=a.minimum_covered_ms.min(b.minimum_covered_ms);
            a.minimum_longest_ms=a.minimum_longest_ms.min(b.minimum_longest_ms);
            a.peak_ms=a.peak_ms.max(b.peak_ms);
            a.minimum_peak_ms=a.minimum_peak_ms.min(b.minimum_peak_ms);
        }
        self.plays=count;
    }
}

#[derive(Clone, Copy)]
pub(super) struct Point {
    pub id: i32,
    pub kind: i32,
    pub range: usize,
}
#[derive(Clone, Copy, Default)]
struct Accumulator {
    notes: u32,
    covered: u32,
    covered_ms: i32,
    longest: i32,
    consecutive: i32,
    last_end: i32,
    peak: i32,
}
#[derive(Clone)]
pub(super) struct Capture {
    pub policy: Policy,
    pub ranges: [(i32, i32); 3],
    pub missions: [i32; 3],
    pub points: Arc<HashMap<usize, Vec<Point>>>,
    values: [Accumulator; 3],
    clock: i32,
    pub curve: Option<Vec<Segment>>,
}
#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Segment {
    pub range: usize,
    pub start_ms: i32,
    pub end_ms: i32,
    pub before_ms: i32,
    pub after_ms: i32,
}
impl Capture {
    pub(super) fn new(policy: Policy, chart: &Chart, probes: &[Probe]) -> Self {
        let mut points: HashMap<_,Vec<_>>=HashMap::default();
        for probe in probes { points.entry(probe.frame).or_default().push(Point { id:probe.id,kind:probe.kind,range:probe.range }); }
        Self { policy, ranges:std::array::from_fn(|i|chart.setup.fevers.get(i).copied().unwrap_or((0,0))),
            missions:std::array::from_fn(|i|chart.setup.missions.get(i).copied().unwrap_or(0)),
            points:Arc::new(points),values:[Accumulator::default();3],clock:0,curve:None }
    }
    pub(super) fn point(&mut self, frame: usize, current: &HashMap<i32,(i32,i32)>) {
        if let Some(points)=self.points.get(&frame) {
            for point in points {
                let &(before,after)=current.get(&point.kind).unwrap();
                let value=&mut self.values[point.range];value.notes+=1;
                value.covered+=u32::from(before.min(after) as f64>=self.policy.minimum_ms);
                value.peak=value.peak.max(before.min(after));
            }
        }
    }
    pub(super) fn add_points(&mut self,range:usize,count:u32,before:i32,after:i32) {
        let value=&mut self.values[range];value.notes+=count;
        if before.min(after) as f64>=self.policy.minimum_ms {value.covered+=count;}
        if count>0 {value.peak=value.peak.max(before.min(after));}
    }
    pub(super) fn interval(&mut self,range:usize,first:i32,last:i32,before:i32,after:i32) {
        if first>=last {return;}
        let value=&mut self.values[range];
        if before.min(after) as f64>=self.policy.minimum_ms {
            value.covered_ms+=last-first;
            value.consecutive=if first==value.last_end {value.consecutive+last-first}else{last-first};
            value.longest=value.longest.max(value.consecutive);
        }else{value.consecutive=0;}
        value.last_end=last;
        if let Some(curve)=&mut self.curve {
            if let Some(previous)=curve.last_mut().filter(|previous|previous.range==range && previous.end_ms==first && previous.before_ms==before && previous.after_ms==after) {
                previous.end_ms=last;
            }else{curve.push(Segment{range,start_ms:first,end_ms:last,before_ms:before,after_ms:after});}
        }
    }
    pub(super) fn advance(&mut self,time:i32,current:&HashMap<i32,(i32,i32)>) {
        let Some(&(before,after))=current.get(&1).or_else(||current.values().next()) else {return};
        for range in 0..3 {
            if self.missions[range]!=3 {continue;}
            let (start,end)=self.ranges[range];
            let first=self.clock.max(start);
            let last=time.min(end);
            if first>=last {continue;}
            self.interval(range,first,last,before,after);
        }
        self.clock=time;
    }
    pub(super) fn complete_static(&mut self) {self.clock=self.ranges.iter().map(|r|r.1).max().unwrap_or(0);}
    pub(super) fn finish(&self,current:&HashMap<i32,(i32,i32)>)->Statistics {
        let mut copy=self.clone();
        copy.curve=None;
        copy.advance(self.ranges.iter().map(|r|r.1).max().unwrap_or(0),current);
        Statistics {target_ms:self.policy.minimum_ms,plays:1,ranges:std::array::from_fn(|i| {
            let value=copy.values[i];
            let coverage=if value.notes==0 {1.0}else{value.covered as f64/value.notes as f64};
            RangeStatistics{notes:value.notes,covered_notes:value.covered as f64,minimum_covered_notes:value.covered,
                coverage,minimum_coverage:coverage,duration_ms:if self.missions[i]==3 {(self.ranges[i].1-self.ranges[i].0) as f64}else{0.0},
                covered_ms:value.covered_ms as f64,minimum_covered_ms:value.covered_ms as f64,
                longest_ms:value.longest as f64,minimum_longest_ms:value.longest as f64,peak_ms:value.peak,minimum_peak_ms:value.peak}
        })}
    }
    pub(super) fn relocate(&mut self,frames:&[crate::full::play::Frame]) {
        let points: HashMap<_,_>=self.points.values().flatten().map(|p|(p.id,*p)).collect();
        let mut grouped:HashMap<_,Vec<_>>=HashMap::default();
        for (frame,row) in frames.iter().enumerate() {
            for note in &row.judged {
                if let Some(point)=points.get(&note.note_id) {grouped.entry(frame).or_default().push(*point);}
            }
        }
        self.points=Arc::new(grouped);
    }
}

pub(super) struct Basis {
    pub before:Vec<u8>,
    pub changes:Vec<(i32,u8)>,
}
pub(super) fn basis(master:&Master,chart:&Chart,members:&[Member],snaps:&[Snap],profiles:&[Profile])->Result<Basis,String> {
    let width=snaps.len()+1;
    let mut before=vec![0;chart.play.frames.len()];
    let mut after=vec![0;chart.play.frames.len()];
    for bit in 0..5 {
        let selected=profiles.iter().position(|profile|!profile.dynamic && if bit==4 {profile.full>0}else{profile.timed[bit]>0});
        let Some(selected)=selected else {continue};
        let actor=Performer::from_member(master,&members[selected/width],snaps.get(selected%width)).gekiso_only();
        let retain=|skills:&std::collections::BTreeMap<i32,crate::master::GekisoSkill>|skills.iter().map(|(&id,skill)| {
            let mut skill=skill.clone();
            skill.effects.retain(|row|row.kind==4004 && if bit==4 {row.seconds==0.0&&row.trigger_type==2}else{row.seconds==(bit+2) as f64&&row.trigger_type==1});
            (id,skill)
        }).collect();
        let mut view=master.clone();view.gekiso_skills=Arc::new(retain(&master.gekiso_skills));view.gekiso_support_skills=Arc::new(retain(&master.gekiso_support_skills));
        let mut model=Model::new(&view,&[actor],&chart.full.notes,&[],chart.params(),Some(&chart.setup),(0,false))?;
        chart.fixed_life(&mut model);model.just_window=Some(Trace::new(&view));
        let expanded=|model:&Model<'_>|model.just_window.as_ref().unwrap().current.iter().any(|(kind,&(early,late))| {
            let base=view.live.just_windows[kind];early>base.0||late>base.1
        });
        for (index,frame) in chart.play.frames.iter().enumerate() {
            if expanded(&model) {before[index]|=1<<bit;}
            model.count_frame(frame.time_ms,&frame.judged,chart.play.dt[index])?;
            if expanded(&model) {after[index]|=1<<bit;}
        }
    }
    let mut changes=Vec::new();let mut previous=0;
    for (index,&mask) in after.iter().enumerate() {
        if mask!=previous {changes.push((chart.play.frames[index].time_ms,mask));previous=mask;}
    }
    Ok(Basis{before,changes})
}
