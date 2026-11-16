//! Runtime clock adapter; algorithms accept an injected clock.
use std::cell::Cell;

#[derive(Clone, Copy)]
struct Deadlines {
    preparation: f64,
    timing: f64,
}

thread_local! {
    static DEADLINES: Cell<Deadlines> = const {
        Cell::new(Deadlines { preparation: f64::INFINITY, timing: f64::INFINITY })
    };
}

pub(crate) struct DeadlineScope(Deadlines);

impl Drop for DeadlineScope {
    fn drop(&mut self) {
        DEADLINES.with(|value| value.set(self.0));
    }
}

fn enter_deadline(deadline: f64, select: impl FnOnce(&mut Deadlines) -> &mut f64) -> DeadlineScope {
    DEADLINES.with(|value| {
        let mut next = value.get();
        let active = select(&mut next);
        *active = active.min(deadline);
        DeadlineScope(value.replace(next))
    })
}

pub(crate) fn preparation_deadline() -> f64 {
    DEADLINES.with(|value| value.get().preparation)
}

pub(crate) fn enter_preparation_deadline(deadline: f64) -> DeadlineScope {
    enter_deadline(deadline, |value| &mut value.preparation)
}

pub(crate) fn timing_deadline() -> f64 {
    DEADLINES.with(|value| value.get().timing)
}

pub(crate) fn enter_timing_deadline(deadline: f64) -> DeadlineScope {
    enter_deadline(deadline, |value| &mut value.timing)
}
pub(crate) fn preparation_expired_at(now:f64)->bool{now>=preparation_deadline()}
pub(crate) fn preparation_expired()->bool{deadline_expired(preparation_deadline())}
pub(crate) fn deadline_expired(deadline: f64) -> bool {
    deadline.is_finite() && milliseconds() >= deadline
}
#[cfg(not(target_arch = "wasm32"))]
pub fn milliseconds() -> f64 {
    use std::{
        sync::OnceLock,
        time::{Instant, SystemTime, UNIX_EPOCH},
    };
    static ORIGIN: OnceLock<(f64, Instant)> = OnceLock::new();
    let (epoch, steady) = ORIGIN.get_or_init(|| {
        (
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs_f64()
                * 1000.0,
            Instant::now(),
        )
    });
    epoch + steady.elapsed().as_secs_f64() * 1000.0
}
#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "env")]
extern "C" {
    fn haneoka_now_ms() -> f64;
}
#[cfg(target_arch = "wasm32")]
thread_local! { static COOPERATIVE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }
#[cfg(target_arch = "wasm32")]
pub(crate) fn cooperative(enabled: bool) { COOPERATIVE.with(|value| value.set(enabled)); }
#[cfg(target_arch = "wasm32")]
pub fn milliseconds() -> f64 {
    if COOPERATIVE.with(std::cell::Cell::get) { crate::yield_for_clock(); }
    unsafe { haneoka_now_ms() }
}
