//! Bun's JavaScriptCore build allocates through mimalloc (v3) but does not
//! ship it, since Bun links its own copy. We link upstream mimalloc v3 via
//! `libmimalloc-sys` and provide the one extension Bun's fork adds.

/// Bun-fork extension: hints that the current thread is idle so mimalloc can
/// return cached memory. Approximated with a non-forced collection.
#[unsafe(no_mangle)]
pub extern "C" fn mi_on_thread_idle() {
    // SAFETY: mi_collect is safe to call from any thread at any time.
    unsafe { libmimalloc_sys::mi_collect(false) }
}
