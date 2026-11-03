//! Native chart conversion, including endpoint merging and client combo notes.
use crate::{
    chart::{EnumerationNote, Note, NoteInput, Source},
    num::f,
};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    io::Read,
};
const MAX_LINES: usize = 20;
fn judged(op: i32) -> bool {
    ![0, 80, 82, 100, 103, 121, 122, 123].contains(&op)
}
fn begins(op: i32) -> bool {
    [20, 41, 61, 80, 104].contains(&op)
}
fn ends(op: i32) -> bool {
    [22, 42, 62, 82, 105].contains(&op)
}
fn judgement_type(op: i32, critical: bool) -> i32 {
    match op {
        0 | 21 => op,
        1 => {
            if critical {
                2
            } else {
                1
            }
        }
        20 => {
            if critical {
                15
            } else {
                10
            }
        }
        22 => 11,
        40 | 41 | 42 | 102 => 5,
        60 | 61 | 63 | 104 | 105 | 120 => 21,
        62 => 22,
        123 => 0,
        _ => 1,
    }
}
fn judgement_area_offset_type(op: i32, critical: bool) -> i32 {
    match op {
        1 => if critical { 8 } else { 0 },
        20 => if critical { 9 } else { 2 },
        21 | 120 | 121 => 1,
        22 => 3,
        40..=42 => 4,
        60..=63 | 80 | 82 | 104 | 105 => 5,
        _ => 0,
    }
}
fn round_int(x: f64) -> i32 {
    if !x.is_finite() {
        return i32::MIN;
    }
    x.round_ties_even() as i32
}
fn integer(v: &Value) -> i32 {
    if let Some(b) = v.as_bool() {
        return i32::from(b);
    }
    if let Some(n) = v.as_f64() {
        return if n.fract() == 0.0 {
            n as i32
        } else {
            round_int(n)
        };
    }
    let text = v.as_str().unwrap_or("").trim();
    let end = text
        .char_indices()
        .skip(usize::from(text.starts_with('-') || text.starts_with('+')))
        .find(|(_, c)| !c.is_ascii_digit())
        .map(|(i, _)| i)
        .unwrap_or(text.len());
    text[..end].parse().unwrap_or(0)
}
fn number(v: &Value) -> f64 {
    if v.is_null() {
        0.0
    } else if let Some(b) = v.as_bool() {
        f64::from(b)
    } else {
        v.as_f64()
            .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
            .unwrap_or(f64::NAN)
    }
}
fn truth(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|n| n != 0.0 && !n.is_nan()),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}
fn list(v: &Value) -> &[Value] {
    v.as_array().map(Vec::as_slice).unwrap_or_default()
}
#[derive(Clone)]
struct SsNote {
    kind: i32,
    t: i32,
    pos: f64,
    auto: bool,
    size: f64,
    critical: bool,
    direction: i32,
    ease_l: i32,
    ease_r: i32,
    visible: bool,
    nodes: Vec<SsNote>,
}
fn read_note(v: &Value) -> SsNote {
    let kind = match v["type"].as_str() {
        Some("flick") => 1,
        Some("trace") => 2,
        Some("long") => 3,
        Some("guide") => 4,
        Some("node") => 5,
        _ => 0,
    };
    let auto = v["pos"] == "auto";
    let easing = |v: &Value| match v.as_str() {
        Some("in") => 1,
        Some("out") => 2,
        _ => 0,
    };
    let (ease_l, ease_r) = if v["ease"].is_array() {
        (easing(&v["ease"][0]), easing(&v["ease"][1]))
    } else {
        let e = easing(&v["ease"]);
        (e, e)
    };
    SsNote {
        kind,
        t: integer(&v["t"]),
        pos: if auto { 0.0 } else { f(number(&v["pos"])) },
        auto,
        size: if v["size"].is_null() {
            f(6.0)
        } else {
            f(number(&v["size"]))
        },
        critical: truth(&v["crit"]),
        direction: match v["dir"].as_str() {
            Some("left") => 1,
            Some("right") => 2,
            Some("down") => 3,
            _ => 0,
        },
        ease_l,
        ease_r,
        visible: if v["visible"].is_null() {
            true
        } else {
            truth(&v["visible"])
        },
        nodes: list(&v["node"]).iter().map(read_note).collect(),
    }
}
#[derive(Clone, Copy)]
struct Position {
    bar: i32,
    progress: f64,
    ms: i32,
}
struct TickConverter {
    sig: Vec<(i32, i32, i32)>,
    bpm: Vec<(i32, i32, f64)>,
}
impl TickConverter {
    fn new(mut signatures: Vec<(i32, i32, i32)>, mut bpms: Vec<(i32, f64)>) -> Self {
        signatures.sort_by_key(|s| s.0);
        let mut sig = Vec::new();
        if signatures.first().is_none_or(|s| s.0 > 0) {
            sig.push((0, 0, 1920));
        }
        let (mut bar, mut previous, mut per_measure) = (0, 0, 1920);
        for (t, num, den) in signatures {
            let q = if per_measure != 0 {
                (t - previous) / per_measure
            } else {
                0
            };
            if t != previous && previous <= t {
                bar += q;
            }
            per_measure = if den != 0 { num * 1920 / den } else { 0 };
            sig.push((t, bar, per_measure));
            previous = t;
        }
        bpms.sort_by_key(|b| b.0);
        let mut bpm = Vec::new();
        if bpms.first().is_none_or(|b| b.0 > 0) {
            bpm.push((0, 0, f(120.0)));
        }
        let (mut acc, mut prev_t, mut prev_bpm) = (0.0, 0, f(120.0));
        for (t, value) in bpms {
            if t != prev_t && prev_t <= t {
                acc += (t - prev_t) as f64 * 60000.0 / f(prev_bpm * f(480.0));
            }
            bpm.push((t, round_int(acc), value));
            prev_t = t;
            prev_bpm = value;
        }
        Self { sig, bpm }
    }
    fn time_ms(&self, tick: i32) -> i32 {
        let (t0, ms0, bpm) = *self
            .bpm
            .iter()
            .rev()
            .find(|b| b.0 <= tick)
            .unwrap_or(&self.bpm[0]);
        ((tick - t0) as f64 * 60000.0 / f(bpm * f(480.0)) + ms0 as f64).floor() as i32
    }
    fn position(&self, tick: i32) -> Position {
        let (t0, bar0, per_measure) = *self
            .sig
            .iter()
            .rev()
            .find(|s| s.0 <= tick)
            .unwrap_or(&self.sig[0]);
        let q = if per_measure != 0 {
            (tick - t0) / per_measure
        } else {
            0
        };
        let r = tick - t0 - q * per_measure;
        Position {
            bar: bar0 + q,
            progress: f(f(r as f64) / f(per_measure as f64)),
            ms: self.time_ms(tick),
        }
    }
}
fn key(p: Position) -> f64 {
    f(p.progress + f(p.bar as f64))
}
fn bits(value: f64) -> u64 {
    if value == 0.0 {
        0
    } else if value.is_nan() {
        f64::NAN.to_bits()
    } else {
        value.to_bits()
    }
}
fn before(pos: Position, bar: i32, progress: f64) -> bool {
    pos.bar < bar || (pos.bar == bar && pos.progress < progress)
}
fn time_from_bar(
    bar: i32,
    progress: f64,
    bpms: &[(f64, Position)],
    bars: &[(f64, Position)],
) -> i32 {
    let progress = f(progress);
    let mut reference = Position {
        bar: 0,
        progress: 0.0,
        ms: 0,
    };
    let mut beats = f(4.0);
    for &(value, pos) in bars {
        if before(pos, bar, progress) {
            beats = value;
            if pos.ms > reference.ms {
                reference = pos;
            }
        }
    }
    let mut bpm = f(160.0);
    for &(value, pos) in bpms {
        if before(pos, bar, progress) {
            bpm = value;
            if pos.ms > reference.ms {
                reference = pos;
            }
        }
    }
    let spb = f(f(beats * f(60.0)) / bpm);
    let x = f(f(spb * f((bar - reference.bar) as f64)) + f(spb * f(progress - reference.progress)));
    reference.ms + f(x * f(1000.0)).floor() as i32
}
fn rhythm(bar: i32, bars: &[(f64, Position)]) -> f64 {
    let mut value = f(4.0);
    for &(beats, pos) in bars {
        if bar < pos.bar {
            return value;
        }
        value = beats;
    }
    value
}
fn bpm_at_ms(ms: i32, bpms: &[(f64, Position)]) -> f64 {
    let mut value = f(160.0);
    for &(bpm, pos) in bpms {
        if pos.ms <= ms {
            value = bpm;
        }
    }
    value
}
fn approximately(a: f64, b: f64) -> bool {
    let (a, b) = (f(a), f(b));
    let m = a.abs().max(b.abs());
    let tolerance = f(m * f(1e-6)).max(f(f(1.401298e-45) * f(8.0)));
    f(b - a).abs() < tolerance
}
fn same(a: Position, b: Position) -> bool {
    a.bar == b.bar && approximately(a.progress, b.progress)
}
fn ease(t: f64, kind: i32) -> f64 {
    let t = f(t);
    if kind == 0 {
        t
    } else if kind == 2 {
        f(t * t)
    } else {
        f(f(f(2.0) - t) * t)
    }
}
fn convert_ease(e: i32) -> i32 {
    if e == 1 {
        2
    } else {
        i32::from(e == 2)
    }
}
fn overlap(n: &SsNote) -> (i32, i32, i32) {
    (n.t, round_int(n.pos), round_int(n.size))
}
#[derive(Clone)]
struct Info {
    pos: Position,
    lane: i32,
    width: i32,
    op: i32,
    critical: bool,
    direction: i32,
    ease: i32,
    ease_r: i32,
    lines: Vec<usize>,
}
fn single(n: &SsNote, tc: &TickConverter) -> Info {
    Info {
        pos: tc.position(n.t),
        lane: round_int(f(n.pos)),
        width: round_int(n.size).max(1),
        op: if n.kind == 1 {
            40
        } else if n.kind == 2 {
            60
        } else {
            1
        },
        critical: n.critical,
        direction: if n.kind == 1 {
            if n.direction == 1 {
                1
            } else {
                i32::from(n.direction == 2) * 2
            }
        } else {
            0
        },
        ease: 0,
        ease_r: 0,
        lines: Vec::new(),
    }
}
fn long_op(nodes: &[SsNote], i: usize) -> i32 {
    let n = &nodes[i];
    if i == 0 {
        if !n.visible {
            80
        } else if n.kind == 1 {
            41
        } else if n.kind == 2 {
            61
        } else {
            20
        }
    } else if i == nodes.len() - 1 {
        if !n.visible {
            82
        } else if n.kind == 1 {
            42
        } else if n.kind == 2 {
            62
        } else {
            22
        }
    } else if n.auto {
        21
    } else if !n.visible {
        122
    } else if n.kind == 2 {
        63
    } else {
        21
    }
}
fn guide_op(nodes: &[SsNote], i: usize, map: &HashMap<(i32, i32, i32), SsNote>) -> i32 {
    let n = &nodes[i];
    if i == 0 {
        if !n.auto {
            if let Some(single) = map.get(&overlap(n)) {
                if single.kind < 3 {
                    return [101, 102, 104][single.kind as usize];
                }
            }
        }
        100
    } else if i == nodes.len() - 1 {
        if n.visible && n.kind == 2 {
            105
        } else {
            103
        }
    } else if n.auto || n.visible {
        63
    } else {
        122
    }
}
fn build_infos(notes: &[SsNote], tc: &TickConverter) -> Result<Vec<Info>, String> {
    let starts: HashSet<_> = notes
        .iter()
        .filter(|n| n.kind == 4 && !n.nodes.is_empty() && !n.nodes[0].auto)
        .map(|n| overlap(&n.nodes[0]))
        .collect();
    let mut guide_map = HashMap::new();
    let mut merged = HashSet::new();
    for (i, n) in notes.iter().enumerate() {
        if n.kind < 3 {
            let key = overlap(n);
            if starts.contains(&key) && !guide_map.contains_key(&key) {
                guide_map.insert(key, n.clone());
                merged.insert(i);
            }
        }
    }
    let mut out: Vec<_> = notes
        .iter()
        .enumerate()
        .filter(|(i, n)| !merged.contains(i) && n.kind < 3)
        .map(|(_, n)| single(n, tc))
        .collect();
    let mut in_use: Vec<(usize, i32)> = Vec::new();
    for kind in [3, 4] {
        let mut chains: Vec<_> = notes
            .iter()
            .enumerate()
            .filter(|(_, n)| n.kind == kind && n.nodes.len() > 1)
            .collect();
        chains.sort_by_key(|(i, n)| (n.nodes[0].t, *i));
        for (_, chain) in chains {
            let nodes = &chain.nodes;
            in_use.retain(|&(_, end)| end >= nodes[0].t);
            let line = (0..MAX_LINES)
                .find(|i| !in_use.iter().any(|(used, _)| used == i))
                .ok_or_else(|| format!("ss lineIndex {MAX_LINES} startTick {}", nodes[0].t))?;
            for (i, n) in nodes.iter().enumerate() {
                let op = if kind == 3 {
                    long_op(nodes, i)
                } else {
                    guide_op(nodes, i, &guide_map)
                };
                let (pos, size) = if !n.auto {
                    (n.pos, n.size)
                } else if i == 0 || i >= nodes.len() - 1 {
                    (nodes[0].pos, nodes[0].size)
                } else {
                    let mut j = i;
                    loop {
                        if j < 2 {
                            j = 0;
                            break;
                        }
                        j -= 1;
                        if !nodes[j].auto {
                            break;
                        }
                    }
                    let mut k = i + 1;
                    while k < nodes.len() - 1 && nodes[k].auto {
                        k += 1;
                    }
                    let (a, b) = (&nodes[j], &nodes[k]);
                    let span = b.t - a.t;
                    let progress = if span < 1 {
                        0.0
                    } else {
                        f(f((n.t - a.t) as f64) / f(span as f64))
                    };
                    let el = ease(progress, convert_ease(a.ease_l));
                    let er = ease(progress, convert_ease(a.ease_r));
                    let pos = f(a.pos + f(el * f(b.pos - a.pos)));
                    let right_a = f(a.pos + a.size);
                    let right = f(right_a + f(er * f(f(b.pos + b.size) - right_a)));
                    (pos, f(right - pos))
                };
                let direction = if kind == 4 && i == 0 && !n.auto {
                    guide_map.get(&overlap(n)).unwrap_or(n)
                } else {
                    n
                };
                let direction = if direction.kind == 1 {
                    if direction.direction == 1 {
                        1
                    } else {
                        i32::from(direction.direction == 2) * 2
                    }
                } else {
                    0
                };
                out.push(Info {
                    pos: tc.position(n.t),
                    lane: round_int(f(pos)),
                    width: round_int(size).max(1),
                    op,
                    critical: n.critical,
                    direction,
                    ease: convert_ease(n.ease_l),
                    ease_r: convert_ease(n.ease_r),
                    lines: vec![line],
                });
            }
            in_use.push((line, nodes.last().unwrap().t));
        }
    }
    Ok(out)
}
fn priority(op: i32) -> i32 {
    if [20, 41, 61, 80, 100, 101, 102, 104].contains(&op) {
        8
    } else if op == 122 {
        9
    } else {
        10
    }
}
type Lanes = Vec<(i32, Vec<Info>)>;
fn merged_infos(infos: Vec<Info>) -> Vec<Vec<Info>> {
    let mut index: HashMap<u64, usize> = HashMap::new();
    let mut entries: Vec<(f64, Lanes)> = Vec::new();
    for info in infos {
        let k = key(info.pos);
        let next = entries.len();
        let at = *index.entry(bits(k)).or_insert_with(|| {
            entries.push((k, Vec::new()));
            next
        });
        let lanes = &mut entries[at].1;
        let lane = lanes
            .iter()
            .position(|(lane, _)| *lane == info.lane)
            .unwrap_or_else(|| {
                lanes.push((info.lane, Vec::new()));
                lanes.len() - 1
            });
        let list = &mut lanes[lane].1;
        if [20, 22, 41, 42, 61, 62, 80, 82, 100, 101, 102, 103, 104, 105].contains(&info.op)
            && !info.lines.is_empty()
        {
            if let Some(existing) = list.iter_mut().find(|e| {
                e.op == info.op
                    && e.width == info.width
                    && e.critical == info.critical
                    && e.direction == info.direction
                    && e.ease == info.ease
                    && e.ease_r == info.ease_r
            }) {
                existing.lines.extend(info.lines);
                continue;
            }
        }
        list.push(info);
    }
    entries.sort_by(|a, b| a.0.total_cmp(&b.0));
    entries
        .into_iter()
        .map(|(_, lanes)| {
            let mut out: Vec<_> = lanes.into_iter().flat_map(|(_, list)| list).collect();
            out.sort_by_key(|i| priority(i.op));
            out
        })
        .collect()
}
#[derive(Clone)]
struct RuntimeNote {
    pos: Position,
    op: i32,
    critical: bool,
    lane: f64,
    width: f64,
    direction: i32,
    ease: i32,
    ease_right: i32,
    lines: Vec<i32>,
    view: Vec<usize>,
    combo: Vec<usize>,
    end_combo: Vec<usize>,
}
struct Creator {
    arena: Vec<RuntimeNote>,
    beginnings: HashMap<i32, usize>,
    line: [i32; MAX_LINES],
    guide: [i32; MAX_LINES],
    current_line: i32,
    current_guide: i32,
    bpms: Vec<(f64, Position)>,
    bars: Vec<(f64, Position)>,
}
impl Creator {
    fn line_shape(&self, begin: usize, points: &[usize], at: Position) -> (f64, f64) {
        let mut previous = &self.arena[begin];
        for &index in points {
            let next = &self.arena[index];
            if key(next.pos) < key(at) {
                previous = next;
                continue;
            }
            let span = f(key(next.pos) - key(previous.pos));
            let t = if span > 0.0 { f(f(key(at) - key(previous.pos)) / span) } else { 0.0 };
            let left = f(previous.lane + f(ease(t, previous.ease) * f(next.lane - previous.lane)));
            let right_a = f(previous.lane + previous.width);
            let right_b = f(next.lane + next.width);
            let right = f(right_a + f(ease(t, previous.ease_right) * f(right_b - right_a)));
            return (left, f(right - left));
        }
        (previous.lane, previous.width)
    }
    fn eighths(&self, start: Position, end: Position) -> Vec<Position> {
        let mut out = Vec::new();
        let (mut bar, mut progress) = (start.bar, start.progress);
        let mut r = rhythm(bar, &self.bars);
        let mut step = 1.0 / (r + r);
        loop {
            progress += step;
            if progress >= 1.0 {
                bar += 1;
                progress -= 1.0;
                r = rhythm(bar, &self.bars);
                step = 1.0 / (r + r);
            }
            let fp = f(progress);
            let ms = time_from_bar(bar, fp, &self.bpms, &self.bars);
            if end.ms <= ms {
                return out;
            }
            out.push(Position {
                bar,
                progress: fp,
                ms,
            });
        }
    }
    fn end_line(&mut self, end: usize) {
        let mut begins_list = Vec::new();
        let mut ok = true;
        for line_id in self.arena[end].lines.clone() {
            let Some(begin) = self.beginnings.get(&line_id).copied() else {
                ok = false;
                break;
            };
            begins_list.push(begin);
            let mut line_notes: Vec<_> = self.arena[begin]
                .view
                .iter()
                .copied()
                .filter(|&note| self.arena[note].lines.contains(&line_id))
                .collect();
            line_notes.sort_by(|&a, &b| {
                self.arena[a]
                    .pos
                    .bar
                    .cmp(&self.arena[b].pos.bar)
                    .then_with(|| {
                        self.arena[a]
                            .pos
                            .progress
                            .total_cmp(&self.arena[b].pos.progress)
                    })
            });
            line_notes.push(end);
            let mids: Vec<_> = line_notes
                .iter()
                .copied()
                .filter(|&note| note != end && judged(self.arena[note].op))
                .map(|note| self.arena[note].pos)
                .collect();
            let bounds: Vec<_> = std::iter::once(self.arena[begin].pos)
                .chain(mids.iter().copied())
                .chain(std::iter::once(self.arena[end].pos))
                .collect();
            let mut beats = Vec::new();
            for pair in bounds.windows(2) {
                beats.extend(self.eighths(pair[0], pair[1]));
            }
            for (k, &beat) in beats.iter().enumerate() {
                if line_notes
                    .iter()
                    .any(|&note| judged(self.arena[note].op) && same(self.arena[note].pos, beat))
                {
                    continue;
                }
                let window = 15000.0 / bpm_at_ms(beat.ms, &self.bpms);
                let mut skip = mids
                    .iter()
                    .any(|mid| mid.ms as f64 - window <= beat.ms as f64 && beat.ms < mid.ms);
                if k == 0 && window > (beat.ms - self.arena[begin].pos.ms) as f64 {
                    skip = true;
                }
                if k == beats.len() - 1 && window > (self.arena[end].pos.ms - beat.ms) as f64 {
                    skip = true;
                }
                let combo = self.arena.len();
                let (lane, width) = self.line_shape(begin, &line_notes, beat);
                self.arena.push(RuntimeNote {
                    pos: beat,
                    op: if skip { 121 } else { 120 },
                    critical: false,
                    lane,
                    width,
                    direction: 0,
                    ease: 0,
                    ease_right: 0,
                    lines: vec![line_id],
                    view: Vec::new(),
                    combo: Vec::new(),
                    end_combo: Vec::new(),
                });
                if begins(self.arena[begin].op) {
                    self.arena[begin].combo.push(combo);
                }
            }
            self.beginnings.remove(&line_id);
        }
        if ok {
            for begin in begins_list {
                if ends(self.arena[end].op) && begins(self.arena[begin].op) {
                    let combos = self.arena[begin].combo.clone();
                    self.arena[end].end_combo.extend(combos);
                }
            }
        }
    }
    fn create(&mut self, info: Info) -> Option<usize> {
        let op = info.op;
        let mut pos = info.pos;
        pos.ms = time_from_bar(pos.bar, pos.progress, &self.bpms, &self.bars);
        let mut lines = Vec::new();
        let index = self.arena.len();
        match op {
            1 | 40 | 60 => {}
            20 | 41 | 61 | 80 => {
                for &slot in &info.lines {
                    self.current_line += 1;
                    self.line[slot] = self.current_line;
                    lines.push(self.current_line);
                }
                for &id in &lines {
                    self.beginnings.insert(id, index);
                }
            }
            21 | 63 => {
                let id = info
                    .lines
                    .first()
                    .map(|&slot| {
                        if op == 21 {
                            self.line[slot]
                        } else {
                            self.guide[slot]
                        }
                    })
                    .unwrap_or(-1);
                lines.push(id);
                if let Some(&begin) = self.beginnings.get(&id) {
                    self.arena[begin].view.push(index);
                }
            }
            22 | 42 | 62 | 82 => {
                for &slot in &info.lines {
                    lines.push(self.line[slot]);
                    self.line[slot] = -1;
                }
            }
            100 | 101 | 102 | 104 => {
                for &slot in &info.lines {
                    let id = self.current_guide + 10001;
                    self.current_guide += 1;
                    self.guide[slot] = id;
                    lines.push(id);
                }
                for &id in &lines {
                    self.beginnings.insert(id, index);
                }
            }
            103 | 105 => {
                for &slot in &info.lines {
                    lines.push(self.guide[slot]);
                    self.guide[slot] = -1;
                }
            }
            122 => {
                let slot = *info.lines.first()?;
                let mut id = self.line[slot];
                if id < 1 {
                    id = self.guide[slot];
                    if id < 1 {
                        return None;
                    }
                }
                lines.push(id);
                if let Some(&begin) = self.beginnings.get(&id) {
                    self.arena[begin].view.push(index);
                }
            }
            _ => return None,
        }
        self.arena.push(RuntimeNote {
            pos,
            op,
            critical: info.critical,
            lane: info.lane as f64,
            width: info.width as f64,
            direction: info.direction,
            ease: info.ease,
            ease_right: info.ease_r,
            lines,
            view: Vec::new(),
            combo: Vec::new(),
            end_combo: Vec::new(),
        });
        if [22, 42, 62, 82, 103, 105].contains(&op) {
            self.end_line(index);
        }
        Some(index)
    }
}
pub fn convert(root: &Value) -> Result<Source, String> {
    let score = &root["score"];
    if !score.is_object() {
        return Err("ss score".into());
    }
    let events = &score["events"];
    let mut bpms: Vec<_> = list(&events["bpm"])
        .iter()
        .map(|b| (integer(&b["t"]), f(number(&b["bpm"]))))
        .collect();
    let mut signatures: Vec<_> = list(&events["sig"])
        .iter()
        .map(|s| {
            (
                integer(&s["t"]),
                integer(&s["sig"][0]),
                integer(&s["sig"][1]),
            )
        })
        .collect();
    let skill: Vec<_> = list(&events["skill"]).iter().map(integer).collect();
    let mut fever: Vec<_> = list(&events["fever"])
        .iter()
        .map(|p| (integer(&p[0]), integer(&p[1])))
        .collect();
    let notes: Vec<_> = list(&score["notes"]).iter().map(read_note).collect();
    let tc = TickConverter::new(signatures.clone(), bpms.clone());
    let infos = build_infos(&notes, &tc)?;
    bpms.sort_by_key(|b| b.0);
    signatures.sort_by_key(|s| s.0);
    fever.sort_by_key(|f| f.0);
    let mut creator = Creator {
        arena: Vec::new(),
        beginnings: HashMap::new(),
        line: [-1; MAX_LINES],
        guide: [-1; MAX_LINES],
        current_line: 0,
        current_guide: 0,
        bpms: bpms.iter().map(|&(t, bpm)| (bpm, tc.position(t))).collect(),
        bars: signatures
            .iter()
            .map(|&(t, num, den)| (f(f(f(num as f64) * f(4.0)) / f(den as f64)), tc.position(t)))
            .collect(),
    };
    let fever_ms = fever
        .into_iter()
        .map(|(a, b)| (tc.position(a).ms, tc.position(b).ms))
        .collect();
    let mut by_key: Vec<(f64, Vec<usize>)> = Vec::new();
    let mut key_index: HashMap<u64, usize> = HashMap::new();
    let mut append = |pos: Position, note: usize, dedupe: bool| {
        let key = key(pos);
        let next = by_key.len();
        let i = *key_index.entry(bits(key)).or_insert_with(|| {
            by_key.push((key, Vec::new()));
            next
        });
        if !dedupe || !by_key[i].1.contains(&note) {
            by_key[i].1.push(note);
        }
    };
    for group in merged_infos(infos) {
        for info in group {
            let Some(note) = creator.create(info) else {
                continue;
            };
            append(creator.arena[note].pos, note, false);
            if ends(creator.arena[note].op) {
                for &combo in &creator.arena[note].end_combo {
                    append(creator.arena[combo].pos, combo, true);
                }
            }
        }
    }
    let enumeration = Some(
        by_key
            .iter()
            .flat_map(|(_, list)| list.iter())
            .map(|&i| EnumerationNote {
                op: creator.arena[i].op,
                time_ms: creator.arena[i].pos.ms,
                judgement_type: judgement_type(creator.arena[i].op, creator.arena[i].critical),
            })
            .collect(),
    );
    by_key.sort_by(|a, b| a.0.total_cmp(&b.0));
    let positional: Vec<_> = by_key.into_iter().flat_map(|(_, list)| list).collect();
    let duration = positional
        .iter()
        .map(|&i| creator.arena[i].pos.ms)
        .max()
        .unwrap_or(0)
        .max(0)
        + 1000;
    let mut ordered: Vec<_> = positional
        .into_iter()
        .map(|i| {
            let n = &creator.arena[i];
            Note {
                time_ms: n.pos.ms,
                operate_type: n.op,
                judgement_type: judgement_type(n.op, n.critical),
                judged: judged(n.op),
                input: Some(NoteInput {
                    judgement_area_offset_type: judgement_area_offset_type(n.op, n.critical),
                    lane: n.lane,
                    width: n.width,
                    direction: n.direction,
                    ease: n.ease,
                    ease_right: n.ease_right,
                    lines: n.lines.clone(),
                }),
            }
        })
        .collect();
    ordered.sort_by_key(|n| n.time_ms);
    Ok(Source {
        duration_ms: duration,
        notes: ordered,
        skill_times_ms: skill.into_iter().map(|t| tc.position(t).ms).collect(),
        fever_ms,
        enumeration,
    })
}
pub fn from_bytes(bytes: &[u8]) -> Result<Source, String> {
    let data = if bytes.starts_with(&[0x1f, 0x8b]) {
        let mut out = Vec::new();
        flate2::read::GzDecoder::new(bytes)
            .read_to_end(&mut out)
            .map_err(|e| e.to_string())?;
        out
    } else {
        bytes.to_vec()
    };
    let root: Value = serde_json::from_slice(&data).map_err(|e| e.to_string())?;
    convert(&root)
}
