//! Portable team-building core. Native and WASM share algorithms and numeric
//! semantics; process management, network access and Flutter stay outside.
pub mod api;
pub mod assignment;
pub mod chart;
pub mod clock;
pub mod event;
pub mod full;
pub mod gekiso;
pub mod gekiso_objective;
mod just_bound;
pub mod live;
pub mod luck;
pub mod master;
pub mod model;
pub mod num;
pub mod objectives;
pub mod parallel;
mod plan;
pub mod power;
pub mod progress;
pub mod pt;
mod reward_trace;
pub mod rewards;
pub mod runtime_chart;
pub mod search;
pub mod session;
pub mod skills;
mod support_aliases;

use std::cell::RefCell;
use std::ffi::{c_char, CStr, CString};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
static GENERATION: AtomicU64 = AtomicU64::new(0);
#[cfg(target_arch = "wasm32")]
static SEARCH_FLOOR: AtomicU64 = AtomicU64::new(f64::NEG_INFINITY.to_bits());
#[cfg(not(target_arch = "wasm32"))]
pub(crate) struct SearchFloor {
    value: AtomicU64,
    hit: std::sync::Mutex<Option<search::Hit>>,
    proofs: std::sync::Mutex<rewards::SharedRewardProofs>,
}
#[cfg(not(target_arch = "wasm32"))]
impl Default for SearchFloor {
    fn default() -> Self {
        Self {
            value: AtomicU64::new(f64::NEG_INFINITY.to_bits()),
            hit: std::sync::Mutex::new(None),
            proofs: std::sync::Mutex::new(rewards::SharedRewardProofs::default()),
        }
    }
}
#[cfg(not(target_arch = "wasm32"))]
thread_local! { static LOCAL_FLOOR: RefCell<Option<Arc<SearchFloor>>> = const { RefCell::new(None) }; }
#[cfg(not(target_arch = "wasm32"))]
pub(crate) struct SearchFloorScope(Option<Arc<SearchFloor>>);
#[cfg(not(target_arch = "wasm32"))]
impl Drop for SearchFloorScope {
    fn drop(&mut self) {
        LOCAL_FLOOR.with(|local| {
            local.replace(self.0.take());
        });
    }
}
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn enter_search_floor(floor: Arc<SearchFloor>) -> SearchFloorScope {
    SearchFloorScope(LOCAL_FLOOR.with(|local| local.replace(Some(floor))))
}
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn local_search_floor() -> Option<Arc<SearchFloor>> {
    LOCAL_FLOOR.with(|local| local.borrow().clone())
}
pub(crate) fn search_floor() -> f64 {
    #[cfg(target_arch = "wasm32")]
    {
        f64::from_bits(SEARCH_FLOOR.load(Ordering::Relaxed))
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        LOCAL_FLOOR.with(|local| {
            local.borrow().as_ref().map_or(f64::NEG_INFINITY, |floor| {
                f64::from_bits(floor.value.load(Ordering::Acquire))
            })
        })
    }
}
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn shared_search_hit() -> Option<search::Hit> {
    LOCAL_FLOOR.with(|local| {
        local
            .borrow()
            .as_ref()
            .and_then(|floor| floor.hit.lock().unwrap().clone())
    })
}
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn shared_reward_proof(key: &rewards::SharedRewardKey) -> Option<(f64, f64)> {
    LOCAL_FLOOR.with(|local| {
        local
            .borrow()
            .as_ref()
            .and_then(|floor| floor.proofs.lock().unwrap().get(key))
    })
}
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn publish_reward_proof(key: rewards::SharedRewardKey, power: f64, limit: f64) {
    LOCAL_FLOOR.with(|local| {
        if let Some(floor) = local.borrow().as_ref() {
            floor.proofs.lock().unwrap().record(key, power, limit);
        }
    });
}
pub(crate) fn reset_search_floor() {
    #[cfg(target_arch = "wasm32")]
    SEARCH_FLOOR.store(f64::NEG_INFINITY.to_bits(), Ordering::Relaxed);
}
pub(crate) fn publish_search_hit(hit: &search::Hit) {
    if !hit.key.is_finite() || hit.key < 0.0 {
        return;
    }
    #[cfg(target_arch = "wasm32")]
    haneoka_search_floor(hit.key);
    #[cfg(not(target_arch = "wasm32"))]
    LOCAL_FLOOR.with(|local| {
        if let Some(floor) = local.borrow().as_ref() {
            if hit.key > f64::from_bits(floor.value.load(Ordering::Relaxed)) {
                let mut best = floor.hit.lock().unwrap();
                if hit.key > f64::from_bits(floor.value.load(Ordering::Relaxed)) {
                    *best = Some(hit.clone());
                    floor.value.store(hit.key.to_bits(), Ordering::Release);
                }
            }
        }
    });
}
#[cfg(target_arch = "wasm32")]
#[no_mangle]
pub extern "C" fn haneoka_search_floor(value: f64) {
    if !value.is_finite() || value < 0.0 {
        return;
    }
    let mut previous = SEARCH_FLOOR.load(Ordering::Relaxed);
    while value > f64::from_bits(previous) {
        match SEARCH_FLOOR.compare_exchange_weak(
            previous,
            value.to_bits(),
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => break,
            Err(current) => previous = current,
        }
    }
}
thread_local! { static LOCAL_CANCEL: RefCell<Option<Arc<AtomicU64>>> = const { RefCell::new(None) }; }

pub(crate) fn with_cancellation<T>(generation: Arc<AtomicU64>, run: impl FnOnce() -> T) -> T {
    struct Restore(Option<Arc<AtomicU64>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            LOCAL_CANCEL.with(|local| {
                local.replace(self.0.take());
            });
        }
    }
    let previous = LOCAL_CANCEL.with(|local| local.replace(Some(generation)));
    let _restore = Restore(previous);
    run()
}

/// The cancellation handle of the current call, for work that continues on other threads.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn local_cancellation() -> Option<Arc<AtomicU64>> {
    LOCAL_CANCEL.with(|local| local.borrow().clone())
}
pub fn cancellation_generation() -> u64 {
    GENERATION
        .load(Ordering::Relaxed)
        .wrapping_add(LOCAL_CANCEL.with(|local| {
            local
                .borrow()
                .as_ref()
                .map(|g| g.load(Ordering::Relaxed))
                .unwrap_or(0)
        }))
}
pub fn cancel() {
    GENERATION.fetch_add(1, Ordering::Relaxed);
}

pub fn luck_curve_json(text: &str) -> Result<String, String> {
    let input: luck::Input = serde_json::from_str(text).map_err(|e| e.to_string())?;
    serde_json::to_string(&luck::run(&input)?).map_err(|e| e.to_string())
}

/// # Safety
/// `input` must point to a live NUL-terminated UTF-8 JSON string.
#[no_mangle]
pub unsafe extern "C" fn haneoka_luck_json(input: *const c_char) -> *mut c_char {
    let result = std::panic::catch_unwind(|| {
        let text = unsafe { CStr::from_ptr(input) }
            .to_str()
            .map_err(|e| e.to_string())?;
        luck_curve_json(text)
    });
    let text = match result {
        Ok(Ok(text)) => text,
        Ok(Err(error)) => serde_json::json!({ "error": error }).to_string(),
        Err(_) => "{\"error\":\"native luck panic\"}".into(),
    };
    CString::new(text).expect("JSON contains no NUL").into_raw()
}

/// # Safety
/// `value` must be null or an unreleased result allocated by this library.
#[no_mangle]
pub unsafe extern "C" fn haneoka_string_free(value: *mut c_char) {
    if !value.is_null() {
        drop(unsafe { CString::from_raw(value) });
    }
}

#[no_mangle]
pub extern "C" fn haneoka_cancel() {
    cancel();
}

/// # Safety
/// `input` must point to a live NUL-terminated UTF-8 JSON string.
#[no_mangle]
pub unsafe extern "C" fn haneoka_context_json(input: *const c_char) -> *mut c_char {
    let result = std::panic::catch_unwind(|| {
        let text = unsafe { CStr::from_ptr(input) }
            .to_str()
            .map_err(|e| e.to_string())?;
        let input: serde_json::Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
        session::execute(&input).map(|v| v.to_string())
    });
    let text = match result {
        Ok(Ok(text)) => text,
        Ok(Err(error)) => serde_json::json!({"error":error}).to_string(),
        Err(_) => "{\"error\":\"engine context panic\"}".into(),
    };
    CString::new(text).expect("JSON contains no NUL").into_raw()
}

// Explicit allocation exports also allow JS to use the same ABI through WASM
// memory without a JS engine or platform-specific libraries in the core.
#[no_mangle]
pub extern "C" fn haneoka_buffer_alloc(length: usize) -> *mut u8 {
    let bytes = vec![0u8; length].into_boxed_slice();
    Box::into_raw(bytes) as *mut u8
}

/// # Safety
/// Pass the original pointer and length returned by `haneoka_buffer_alloc` once.
#[no_mangle]
pub unsafe extern "C" fn haneoka_buffer_free(value: *mut u8, length: usize) {
    if !value.is_null() {
        drop(unsafe { Box::from_raw(std::ptr::slice_from_raw_parts_mut(value, length)) });
    }
}

/// # Safety
/// `input` must point to a live NUL-terminated UTF-8 JSON string.
#[no_mangle]
pub unsafe extern "C" fn haneoka_live_json(input: *const c_char) -> *mut c_char {
    let result = std::panic::catch_unwind(|| {
        let text = unsafe { CStr::from_ptr(input) }
            .to_str()
            .map_err(|e| e.to_string())?;
        let input: live::Input = serde_json::from_str(text).map_err(|e| e.to_string())?;
        serde_json::to_string(&live::score(&input)?).map_err(|e| e.to_string())
    });
    let text = match result {
        Ok(Ok(text)) => text,
        Ok(Err(error)) => serde_json::json!({"error":error}).to_string(),
        Err(_) => "{\"error\":\"native live panic\"}".into(),
    };
    CString::new(text).expect("JSON contains no NUL").into_raw()
}

/// # Safety
/// `input` must point to a live NUL-terminated UTF-8 JSON string.
#[no_mangle]
pub unsafe extern "C" fn haneoka_evaluate_json(input: *const c_char) -> *mut c_char {
    let result = std::panic::catch_unwind(|| {
        let text = unsafe { CStr::from_ptr(input) }
            .to_str()
            .map_err(|e| e.to_string())?;
        let input: serde_json::Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
        let mut engine = api::Engine::compile(&input["data"])?;
        let song_id = master::int(&input["song"]["songId"]);
        let difficulty = master::int(&input["song"]["difficulty"]);
        engine.insert_chart(
            song_id,
            difficulty,
            serde_json::from_value(input["chart"].clone()).map_err(|e| e.to_string())?,
        )?;
        engine
            .evaluate(&input["request"], &input["team"], song_id, difficulty)
            .map(|value| value.to_string())
    });
    let text = match result {
        Ok(Ok(text)) => text,
        Ok(Err(error)) => serde_json::json!({"error":error}).to_string(),
        Err(_) => "{\"error\":\"native evaluation panic\"}".into(),
    };
    CString::new(text).expect("JSON contains no NUL").into_raw()
}

/// # Safety
/// `input` must point to a live NUL-terminated UTF-8 JSON string.
#[no_mangle]
pub unsafe extern "C" fn haneoka_search_power_json(input: *const c_char) -> *mut c_char {
    let result = std::panic::catch_unwind(|| {
        let started = clock::milliseconds();
        let text = unsafe { CStr::from_ptr(input) }
            .to_str()
            .map_err(|e| e.to_string())?;
        let input: serde_json::Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
        let engine = api::Engine::compile(&input["data"])?;
        engine
            .search_power(&input["request"], started, &clock::milliseconds)
            .map(|value| value.to_string())
    });
    let text = match result {
        Ok(Ok(text)) => text,
        Ok(Err(error)) => serde_json::json!({"error":error}).to_string(),
        Err(_) => "{\"error\":\"native search panic\"}".into(),
    };
    CString::new(text).expect("JSON contains no NUL").into_raw()
}

/// # Safety
/// `input` must point to a live NUL-terminated UTF-8 JSON string.
#[no_mangle]
pub unsafe extern "C" fn haneoka_search_score_json(input: *const c_char) -> *mut c_char {
    unsafe { haneoka_run_json(input) }
}

/// # Safety
/// `input` must point to a live NUL-terminated UTF-8 chart JSON string.
#[no_mangle]
pub unsafe extern "C" fn haneoka_chart_json(input: *const c_char) -> *mut c_char {
    let result = std::panic::catch_unwind(|| {
        let text = unsafe { CStr::from_ptr(input) }
            .to_str()
            .map_err(|e| e.to_string())?;
        let input: serde_json::Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
        serde_json::to_string(&runtime_chart::convert(&input)?).map_err(|e| e.to_string())
    });
    let text = match result {
        Ok(Ok(text)) => text,
        Ok(Err(error)) => serde_json::json!({"error":error}).to_string(),
        Err(_) => "{\"error\":\"native chart panic\"}".into(),
    };
    CString::new(text).expect("JSON contains no NUL").into_raw()
}

/// # Safety
/// `input` must point to a live NUL-terminated UTF-8 JSON string.
#[no_mangle]
pub unsafe extern "C" fn haneoka_full_orders_json(input: *const c_char) -> *mut c_char {
    let result = std::panic::catch_unwind(|| {
        let text = unsafe { CStr::from_ptr(input) }
            .to_str()
            .map_err(|e| e.to_string())?;
        let input: serde_json::Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
        let engine = api::Engine::compile(&input["data"])?;
        let source = runtime_chart::convert(&input["chart"])?;
        engine
            .full_orders(&input["request"], &input["team"], &input["song"], &source)
            .map(|v| v.to_string())
    });
    let text = match result {
        Ok(Ok(text)) => text,
        Ok(Err(error)) => serde_json::json!({"error":error}).to_string(),
        Err(_) => "{\"error\":\"native full simulation panic\"}".into(),
    };
    CString::new(text).expect("JSON contains no NUL").into_raw()
}

/// # Safety
/// `input` must point to a live NUL-terminated UTF-8 JSON string.
#[no_mangle]
pub unsafe extern "C" fn haneoka_evaluate_gekiso_json(input: *const c_char) -> *mut c_char {
    let result = std::panic::catch_unwind(|| {
        let text = unsafe { CStr::from_ptr(input) }
            .to_str()
            .map_err(|e| e.to_string())?;
        let input: serde_json::Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
        let engine = api::Engine::compile(&input["data"])?;
        let source = runtime_chart::convert(&input["chart"])?;
        engine
            .evaluate_gekiso(&input["request"], &input["team"], &input["song"], &source)
            .map(|v| v.to_string())
    });
    let text = match result {
        Ok(Ok(text)) => text,
        Ok(Err(error)) => serde_json::json!({"error":error}).to_string(),
        Err(_) => "{\"error\":\"native Gekiso evaluation panic\"}".into(),
    };
    CString::new(text).expect("JSON contains no NUL").into_raw()
}

/// # Safety
/// `input` must point to a live NUL-terminated UTF-8 JSON string.
#[no_mangle]
pub unsafe extern "C" fn haneoka_search_gekiso_json(input: *const c_char) -> *mut c_char {
    let result = std::panic::catch_unwind(|| {
        let started = clock::milliseconds();
        let text = unsafe { CStr::from_ptr(input) }
            .to_str()
            .map_err(|e| e.to_string())?;
        let input: serde_json::Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
        let engine = api::Engine::compile(&input["data"])?;
        let source = runtime_chart::convert(&input["chart"])?;
        engine
            .search_gekiso(&input["request"], &source, started, &clock::milliseconds)
            .map(|v| v.to_string())
    });
    let text = match result {
        Ok(Ok(text)) => text,
        Ok(Err(error)) => serde_json::json!({"error":error}).to_string(),
        Err(_) => "{\"error\":\"native Gekiso search panic\"}".into(),
    };
    CString::new(text).expect("JSON contains no NUL").into_raw()
}

/// # Safety
/// JSON input and callback must remain live until this synchronous call returns.
/// The callback runs on the calling thread and must not reenter the context.
#[no_mangle]
pub unsafe extern "C" fn haneoka_context_stream_json(
    input: *const c_char,
    callback: Option<progress::Callback>,
) -> *mut c_char {
    progress::with_callback(callback, || unsafe { haneoka_context_json(input) })
}

/// # Safety
/// `input` must point to a live NUL-terminated UTF-8 JSON string containing
/// a release document, request, and the charts required by that request.
#[no_mangle]
pub unsafe extern "C" fn haneoka_run_json(input: *const c_char) -> *mut c_char {
    let result = std::panic::catch_unwind(|| {
        let started = clock::milliseconds();
        let text = unsafe { CStr::from_ptr(input) }
            .to_str()
            .map_err(|e| e.to_string())?;
        let input: serde_json::Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
        let mut engine = api::Engine::compile(&input["data"])?;
        engine
            .run(&input, started, &clock::milliseconds)
            .map(|v| v.to_string())
    });
    let text = match result {
        Ok(Ok(text)) => text,
        Ok(Err(error)) => serde_json::json!({"error":error}).to_string(),
        Err(_) => "{\"error\":\"native engine panic\"}".into(),
    };
    CString::new(text).expect("JSON contains no NUL").into_raw()
}

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "env")]
extern "C" {
    /// Receives each progress notification (a NUL-terminated JSON string, valid during the call).
    fn haneoka_progress(value: *const c_char);
}
#[cfg(target_arch = "wasm32")]
extern "C" fn forward_progress(value: *const c_char) {
    unsafe { haneoka_progress(value) }
}

/// WebAssembly hosts cannot pass a function pointer: progress goes to the `env.haneoka_progress` import.
///
/// # Safety
/// `input` must point to a live NUL-terminated UTF-8 JSON string.
#[cfg(target_arch = "wasm32")]
#[no_mangle]
pub unsafe extern "C" fn haneoka_context_stream_wasm(input: *const c_char) -> *mut c_char {
    unsafe { haneoka_context_stream_json(input, Some(forward_progress)) }
}
