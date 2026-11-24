//! Order-preserving maps and serial nested work scopes for native proofs.
//! WebAssembly maps remain sequential.

#[cfg(not(target_arch = "wasm32"))]
thread_local! {static SERIAL: std::cell::Cell<bool> = const {std::cell::Cell::new(false)};}

#[cfg(not(target_arch = "wasm32"))]
thread_local! { static BUDGET: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) }; }
#[cfg(not(target_arch = "wasm32"))]
pub(crate) struct WorkerBudget(Option<usize>);
#[cfg(not(target_arch = "wasm32"))]
impl Drop for WorkerBudget {
    fn drop(&mut self) {
        BUDGET.with(|budget| budget.set(self.0));
    }
}
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn enter_budget(value: Option<usize>) -> WorkerBudget {
    let previous = BUDGET.with(|budget| {
        let previous = budget.get();
        budget.set(match (previous, value) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        });
        previous
    });
    WorkerBudget(previous)
}
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn configured_limit(default: usize) -> usize {
    BUDGET.with(|budget| budget.get().unwrap_or(default))
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn serial<T>(run: impl FnOnce() -> T) -> T {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            SERIAL.with(|serial| serial.set(self.0));
        }
    }
    let previous = SERIAL.with(|serial| serial.replace(true));
    let _restore = Restore(previous);
    run()
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn workers(limit: usize) -> usize {
    if SERIAL.with(std::cell::Cell::get) {
        1
    } else {
        std::thread::available_parallelism()
            .map_or(1, |n| n.get())
            .min(limit)
            .min(configured_limit(usize::MAX))
    }
}
#[cfg(target_arch = "wasm32")]
pub(crate) fn workers(_limit: usize) -> usize {
    1
}

pub(crate) fn preparation_workers() -> usize {
    #[cfg(not(target_arch = "wasm32"))]
    {
        workers(configured_limit(4).min(8))
    }
    #[cfg(target_arch = "wasm32")]
    {
        workers(1)
    }
}

/// Applies `f` to every item in order, or returns the first failing item's error.
#[cfg(not(target_arch = "wasm32"))]
pub fn map<T, R, E, F>(items: &[T], f: F) -> Result<Vec<R>, E>
where
    T: Sync,
    R: Send,
    E: Send,
    F: Fn(&T) -> Result<R, E> + Sync,
{
    use std::sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Mutex,
    };
    let workers = workers(items.len());
    if workers <= 1 {
        return items.iter().map(&f).collect();
    }
    let next = AtomicUsize::new(0);
    let failed = AtomicBool::new(false);
    let slots: Vec<Mutex<Option<Result<R, E>>>> = items.iter().map(|_| Mutex::new(None)).collect();
    let cancel = crate::local_cancellation();
    let budget = BUDGET.with(std::cell::Cell::get);
    let deadline=crate::clock::preparation_deadline();
    let work = || loop {
        let i = next.fetch_add(1, Ordering::Relaxed);
        if i >= items.len() || failed.load(Ordering::Relaxed) {
            break;
        }
        let result = f(&items[i]);
        if result.is_err() {
            failed.store(true, Ordering::Relaxed);
        }
        *slots[i].lock().unwrap() = Some(result);
    };
    std::thread::scope(|scope| {
        for _ in 0..workers {
            let cancel = cancel.clone();
            scope.spawn(|| {
                let _budget = enter_budget(budget);
                let _deadline=crate::clock::enter_preparation_deadline(deadline);
                match cancel {
                    Some(generation) => crate::with_cancellation(generation, work),
                    None => work(),
                }
            });
        }
    });
    let mut out = Vec::with_capacity(items.len());
    for slot in slots {
        match slot.into_inner().unwrap() {
            Some(Ok(value)) => out.push(value),
            Some(Err(error)) => return Err(error),
            // Skipped after an earlier failure; that failure comes first in item order.
            None => {}
        }
    }
    Ok(out)
}

/// Keep a bounded window of results in flight and consume them in input order.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn visit_in_order<T, R, E>(
    items: &[T],
    workers: usize,
    f: impl Fn(&T) -> R + Sync,
    mut visit: impl FnMut(R) -> Result<bool, E>,
) -> Result<(), E>
where
    T: Sync,
    R: Send,
{
    use std::{panic::{catch_unwind, resume_unwind, AssertUnwindSafe}, sync::{
        atomic::{AtomicBool, Ordering}, mpsc, Mutex,
    }};

    let workers = workers.min(items.len());
    if workers <= 1 {
        for item in items {
            if !visit(f(item))? { break; }
        }
        return Ok(());
    }
    let (queue, tasks) = mpsc::channel::<usize>();
    let (send, results) = mpsc::channel();
    let tasks = Mutex::new(tasks);
    let stopped = AtomicBool::new(false);
    let cancel = crate::local_cancellation();
    let budget = BUDGET.with(std::cell::Cell::get);
    let deadline = crate::clock::preparation_deadline();
    std::thread::scope(|scope| {
        let queue = queue;
        for _ in 0..workers {
            let cancel = cancel.clone();
            let tasks = &tasks;
            let stopped = &stopped;
            let f = &f;
            let send = send.clone();
            scope.spawn(move || {
                let _budget = enter_budget(budget);
                let _deadline = crate::clock::enter_preparation_deadline(deadline);
                let work = || loop {
                    let Ok(index) = tasks.lock().unwrap().recv() else { break; };
                    if stopped.load(Ordering::Relaxed) { break; }
                    let result = catch_unwind(AssertUnwindSafe(|| f(&items[index])));
                    if send.send((index, result)).is_err() { break; }
                };
                match cancel {
                    Some(generation) => crate::with_cancellation(generation, work),
                    None => work(),
                }
            });
        }
        drop(send);
        let window = (workers * 2).min(items.len());
        let mut next = window;
        for index in 0..next { queue.send(index).unwrap(); }
        let mut pending = (0..window).map(|_| None).collect::<Vec<_>>();
        let mut current = 0;
        while current < items.len() {
            let (index, value) = results.recv().unwrap();
            pending[index % window] = Some(value);
            while let Some(value) = pending[current % window].take() {
                let value = value.unwrap_or_else(|panic| resume_unwind(panic));
                match visit(value) {
                    Ok(true) => {}
                    stop => {
                        stopped.store(true, Ordering::Relaxed);
                        return stop.map(|_| ());
                    }
                }
                current += 1;
                if next < items.len() {
                    queue.send(next).unwrap();
                    next += 1;
                }
                if current == items.len() { return Ok(()); }
            }
        }
        Ok(())
    })
}

#[cfg(target_arch = "wasm32")]
pub fn map<T, R, E, F>(items: &[T], f: F) -> Result<Vec<R>, E>
where
    F: Fn(&T) -> Result<R, E>,
{
    items.iter().map(f).collect()
}
