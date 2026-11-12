//! Runtime clock adapter; algorithms accept an injected clock.
thread_local!{static PREPARATION_DEADLINE:std::cell::Cell<f64>=const{std::cell::Cell::new(f64::INFINITY)};}
pub(crate) struct PreparationDeadline(f64);
impl Drop for PreparationDeadline{fn drop(&mut self){PREPARATION_DEADLINE.with(|value|value.set(self.0));}}
pub(crate) fn preparation_deadline()->f64{PREPARATION_DEADLINE.with(std::cell::Cell::get)}
pub(crate) fn enter_preparation_deadline(deadline:f64)->PreparationDeadline{
    PreparationDeadline(PREPARATION_DEADLINE.with(|value|value.replace(value.get().min(deadline))))
}
pub(crate) fn preparation_expired_at(now:f64)->bool{now>=preparation_deadline()}
pub(crate) fn preparation_expired()->bool{let deadline=preparation_deadline();deadline.is_finite()&&milliseconds()>=deadline}
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
pub fn milliseconds() -> f64 {
    unsafe { haneoka_now_ms() }
}
