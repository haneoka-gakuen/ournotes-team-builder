//! Watched timing conflicts and local propagation over immutable frame domains.
use super::{restrict_domain, FrameDomain};
use std::{collections::HashSet, rc::Rc};

type Terms = Vec<(usize, Vec<i32>)>;
struct Clause {
    terms: Rc<Terms>,
    watches: [Option<usize>; 2],
}

pub(super) struct Constraints {
    base: Vec<FrameDomain>,
    clauses: Vec<Clause>,
    known: HashSet<Rc<Terms>>,
    watches: Vec<Vec<usize>>,
    units: Vec<usize>,
    orderings: Vec<Vec<(usize, bool)>>,
    seeds: Vec<usize>,
    infeasible: bool,
}

fn inside(domain: &FrameDomain, forbidden: &[i32]) -> bool { domain.iter().all(|at| forbidden.contains(at)) }
fn outside(domain: &FrameDomain, forbidden: &[i32]) -> bool { domain.iter().all(|at| !forbidden.contains(at)) }

impl Constraints {
    pub(super) fn new(base: &[FrameDomain], pairs: &[(usize, usize)]) -> Self {
        let mut orderings = vec![Vec::new(); base.len()];
        let mut seeds = Vec::new();
        for &(first, second) in pairs {
            orderings[first].push((second, true));
            orderings[second].push((first, false));
            if base[first].last() > base[second].last() { seeds.push(second); }
            if base[second].first() < base[first].first() { seeds.push(first); }
        }
        seeds.sort_unstable();
        seeds.dedup();
        Self { base: base.to_vec(), clauses: Vec::new(), known: HashSet::new(), watches: vec![Vec::new(); base.len()],
            units: Vec::new(), orderings, seeds, infeasible: false }
    }

    pub(super) fn add(&mut self, mut terms: Terms) {
        if self.clauses.len() == 4096 { return; }
        terms.sort_by_key(|term| term.0);
        let terms = Rc::new(terms);
        if !self.known.insert(Rc::clone(&terms)) { return; }
        let mut watches = [None, None];
        for (position, (note, _)) in terms.iter().enumerate().filter(|(_, (note, forbidden))| !inside(&self.base[*note], forbidden)).take(2) {
            let slot = usize::from(watches[0].is_some());
            watches[slot] = Some(position);
            self.watches[*note].push(self.clauses.len());
        }
        if let Some(position) = watches[0] {
            if watches[1].is_none() { self.units.push(terms[position].0); }
        } else { self.infeasible = true; }
        self.clauses.push(Clause { terms, watches });
    }

    pub(super) fn propagate(&mut self, domains: &mut [FrameDomain]) -> bool {
        if self.infeasible || domains.iter().any(|domain| domain.is_empty()) { return false; }
        let mut queue = Vec::new();
        let mut queued = vec![false; domains.len()];
        let enqueue = |note: usize, queue: &mut Vec<usize>, queued: &mut [bool]| {
            if !queued[note] { queued[note] = true; queue.push(note); }
        };
        for (note, domain) in domains.iter().enumerate() {
            if !Rc::ptr_eq(domain, &self.base[note]) { enqueue(note, &mut queue, &mut queued); }
        }
        for &note in self.units.iter().chain(&self.seeds) { enqueue(note, &mut queue, &mut queued); }
        let mut cursor = 0;
        while cursor < queue.len() {
            let note = queue[cursor];
            cursor += 1;
            queued[note] = false;
            for &(other, forward) in &self.orderings[note] {
                let before = domains[other].len();
                if forward {
                    let minimum = domains[note][0];
                    restrict_domain(&mut domains[other], |at| at >= minimum);
                } else {
                    let maximum = *domains[note].last().unwrap();
                    restrict_domain(&mut domains[other], |at| at <= maximum);
                }
                if domains[other].is_empty() { return false; }
                if domains[other].len() != before { enqueue(other, &mut queue, &mut queued); }
            }
            let watching = std::mem::take(&mut self.watches[note]);
            for (position, &id) in watching.iter().enumerate() {
                let terms = Rc::clone(&self.clauses[id].terms);
                let watches = self.clauses[id].watches;
                let side = if terms[watches[0].unwrap()].0 == note { 0 } else { 1 };
                let watched = watches[side].unwrap();
                let other = watches[1 - side];
                let mut keep = true;
                let mut failed = false;
                if let Some(other) = other {
                    if inside(&domains[note], &terms[watched].1) && !outside(&domains[terms[other].0], &terms[other].1) {
                        let replacement = terms.iter().enumerate().position(|(candidate, (variable, forbidden))| candidate != other && !inside(&domains[*variable], forbidden));
                        if let Some(replacement) = replacement {
                            self.clauses[id].watches[side] = Some(replacement);
                            self.watches[terms[replacement].0].push(id);
                            keep = false;
                            if inside(&domains[terms[other].0], &terms[other].1) { enqueue(terms[other].0, &mut queue, &mut queued); }
                        } else {
                            let variable = terms[other].0;
                            if inside(&domains[variable], &terms[other].1) { failed = true; }
                            else {
                                restrict_domain(&mut domains[variable], |at| !terms[other].1.contains(&at));
                                enqueue(variable, &mut queue, &mut queued);
                            }
                        }
                    }
                } else if inside(&domains[note], &terms[watched].1) { failed = true; }
                else if !outside(&domains[note], &terms[watched].1) {
                    restrict_domain(&mut domains[note], |at| !terms[watched].1.contains(&at));
                    enqueue(note, &mut queue, &mut queued);
                }
                if failed {
                    self.watches[note].extend_from_slice(&watching[position..]);
                    return false;
                }
                if keep { self.watches[note].push(id); }
            }
        }
        true
    }
}
