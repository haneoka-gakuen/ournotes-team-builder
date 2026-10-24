//! Order-preserving maps and serial nested work scopes for native proofs.
//! WebAssembly maps remain sequential.

#[cfg(not(target_arch = "wasm32"))]
thread_local! {static SERIAL: std::cell::Cell<bool> = const {std::cell::Cell::new(false)};}

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

/// Applies `f` to every item and returns the results in item order, or the error of the first failing item.
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
    let workers = if SERIAL.with(std::cell::Cell::get) {
        1
    } else {
        std::thread::available_parallelism()
            .map_or(1, |n| n.get())
            .min(items.len())
    };
    if workers <= 1 {
        return items.iter().map(&f).collect();
    }
    let next = AtomicUsize::new(0);
    let failed = AtomicBool::new(false);
    let slots: Vec<Mutex<Option<Result<R, E>>>> = items.iter().map(|_| Mutex::new(None)).collect();
    let cancel = crate::local_cancellation();
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
            scope.spawn(|| match cancel {
                Some(generation) => crate::with_cancellation(generation, work),
                None => work(),
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

#[cfg(target_arch = "wasm32")]
pub fn map<T, R, E, F>(items: &[T], f: F) -> Result<Vec<R>, E>
where
    F: Fn(&T) -> Result<R, E>,
{
    items.iter().map(f).collect()
}
