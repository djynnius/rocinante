//! The C surface of `shim/rcn_llama.cpp`.

use std::ffi::{c_char, c_void};

#[repr(C)]
pub struct RcnModel {
    _private: [u8; 0],
}

#[repr(C)]
pub struct RcnSession {
    _private: [u8; 0],
}

#[repr(C)]
pub struct RcnTurnParams {
    pub messages_json: *const c_char,
    pub tools_json: *const c_char,
    pub enable_thinking: bool,
    pub temperature: f32,
    pub top_p: f32,
    pub top_k: i32,
    pub min_p: f32,
    pub max_tokens: i32,
    pub seed: u32,
}

pub type LogFn = extern "C" fn(level: i32, text: *const c_char);

/// kind 0 = answer text, 1 = reasoning, 2 = prompt progress. Return false
/// to cancel the turn.
pub type EmitFn =
    extern "C" fn(user: *mut c_void, kind: i32, text: *const c_char, len: usize) -> bool;

unsafe extern "C" {
    pub fn rcn_init(log: Option<LogFn>);
    pub fn rcn_default_threads() -> i32;
    pub fn rcn_gpu_offload_supported() -> bool;
    pub fn rcn_string_free(s: *mut c_char);

    pub fn rcn_model_load(
        path: *const c_char,
        n_gpu_layers: i32,
        err: *mut *mut c_char,
    ) -> *mut RcnModel;
    pub fn rcn_model_free(m: *mut RcnModel);
    pub fn rcn_model_n_ctx_train(m: *const RcnModel) -> i32;
    pub fn rcn_model_desc(m: *const RcnModel) -> *mut c_char;

    pub fn rcn_session_new(
        m: *mut RcnModel,
        n_ctx: u32,
        n_threads: i32,
        err: *mut *mut c_char,
    ) -> *mut RcnSession;
    pub fn rcn_session_free(s: *mut RcnSession);
    pub fn rcn_session_n_ctx(s: *const RcnSession) -> u32;
    pub fn rcn_session_chat(
        s: *mut RcnSession,
        params: *const RcnTurnParams,
        emit: EmitFn,
        user: *mut c_void,
    ) -> *mut c_char;
}

/// Take ownership of a shim-allocated string.
///
/// # Safety
/// `ptr` must be null or a string the shim returned and nobody freed yet.
pub unsafe fn take_string(ptr: *mut c_char) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    // SAFETY: the shim returns NUL-terminated malloc'd strings.
    let s = unsafe { std::ffi::CStr::from_ptr(ptr) }
        .to_string_lossy()
        .into_owned();
    // SAFETY: allocated by the shim with malloc, freed exactly once here.
    unsafe { rcn_string_free(ptr) };
    Some(s)
}
