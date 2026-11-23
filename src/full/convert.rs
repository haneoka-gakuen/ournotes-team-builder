//! Most-recent-first judgement conversion and activation limits.
use rustc_hash::{FxHashMap as HashMap, FxHashSet as HashSet};
use std::rc::Rc;
#[derive(Clone)]
pub struct Effect {
    pub kind: i32,
    pub value: i32,
    pub limit: i32,
    pub targets: Vec<i32>,
    pub id: i32,
}
#[derive(Clone)]
struct Parameter {
    key: Key,
    count: i32,
    maximum: i32,
    targets: Rc<[i32]>,
    to: i32,
}
/// Identifies one effect instance: a live skill row of a member's activation, or a condition updater.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Key(pub u8, pub i32, pub u32, pub u32);
#[derive(Clone)]
pub struct Conversion {
    no_just: Rc<HashSet<i32>>,
    active: Vec<Parameter>,
    finished: HashMap<Key, i32>,
    context: HashMap<i32, Rc<[i32]>>,
    pub converted: i32,
}
impl Conversion {
    pub fn new(no_just: HashSet<i32>) -> Self {
        Self {
            no_just: Rc::new(no_just),
            active: Vec::new(),
            finished: HashMap::default(),
            context: HashMap::default(),
            converted: 0,
        }
    }
    pub fn convert(
        &mut self,
        judgement: i32,
        judgement_type: i32,
        time: i32,
    ) -> Result<i32, String> {
        for index in (0..self.active.len()).rev() {
            let parameter = &mut self.active[index];
            if parameter.to == -1 || parameter.to == judgement || !parameter.targets.contains(&judgement)
                || (parameter.to == 6 && self.no_just.contains(&judgement_type))
            {
                continue;
            }
            parameter.count += 1;
            let to = parameter.to;
            if parameter.maximum > 0 && parameter.maximum <= parameter.count {
                let key = self.active.remove(index).key;
                self.finished.insert(key, time);
            }
            self.converted += 1;
            return Ok(to);
        }
        Ok(judgement)
    }
    pub fn update(
        &mut self,
        key: Key,
        mut state: i32,
        effect: &Effect,
    ) -> Result<Option<i32>, String> {
        let finished = self.finished.remove(&key);
        if finished.is_some() {
            state = 4;
        }
        if state == 4 {
            if let Some(index) = self.active.iter().position(|parameter| parameter.key == key) {
                self.active.remove(index);
            }
        } else if state == 2 {
            let targets = self
                .context
                .entry(effect.id)
                .or_insert_with(|| Rc::from(effect.targets.as_slice()))
                .clone();
            let to = if effect.kind == 13005 {
                6
            } else if (0..=5).contains(&(effect.value - 1)) {
                effect.value
            } else {
                -1
            };
            if self.active.iter().any(|parameter| parameter.key == key) {
                return Err("judgement convert state registered twice".into());
            }
            self.active.push(Parameter { key, count: 0, maximum: effect.limit, targets, to });
        }
        Ok(finished)
    }
}
