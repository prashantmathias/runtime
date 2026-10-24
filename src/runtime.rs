//! The JavaScriptCore-backed runtime: native ops, the async event loop and
//! script/REPL execution.
//!
//! Each async op returns a JS promise (created with `JSObjectMakeDeferredPromise`),
//! runs the actual I/O on tokio, and resolves/rejects the promise on the JS
//! thread once the work completes. Only `Send` data crosses the tokio boundary;
//! JS handles stay in a thread-local table.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::ptr;

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::jsc::*;

const BOOTSTRAP: &str = include_str!("bootstrap.js");

/// Result of an async op, produced off the JS thread.
enum OpOutcome {
    Undefined,
    String(String),
    Error(String),
}

struct PendingPromise {
    resolve: JSObjectRef,
    reject: JSObjectRef,
}

struct OpState {
    next_id: u64,
    pending: HashMap<u64, PendingPromise>,
    tx: UnboundedSender<(u64, OpOutcome)>,
    unhandled_rejections: Vec<String>,
}

thread_local! {
    static STATE: RefCell<Option<OpState>> = const { RefCell::new(None) };
}

pub struct Runtime {
    ctx: JSGlobalContextRef,
    rx: UnboundedReceiver<(u64, OpOutcome)>,
}

#[derive(Debug)]
pub struct JsError(pub String);

impl std::fmt::Display for JsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for JsError {}

impl Runtime {
    pub fn new() -> Result<Self, JsError> {
        let (tx, rx) = unbounded_channel();
        STATE.with(|s| {
            *s.borrow_mut() = Some(OpState {
                next_id: 0,
                pending: HashMap::new(),
                tx,
                unhandled_rejections: Vec::new(),
            })
        });

        // SAFETY: creating a fresh global context with the default global class.
        let ctx = unsafe { JSGlobalContextCreate(ptr::null_mut()) };
        let runtime = Runtime { ctx, rx };
        runtime.install_bootstrap()?;
        Ok(runtime)
    }

    fn install_bootstrap(&self) -> Result<(), JsError> {
        let ctx = self.ctx as JSContextRef;
        unsafe {
            let ops = JSObjectMake(ctx, ptr::null_mut(), ptr::null_mut());
            set_function(ctx, ops, "op_custom_print", Some(op_custom_print));
            set_function(ctx, ops, "op_sleep", Some(op_sleep));
            set_function(ctx, ops, "op_fs_read_file_sync", Some(op_fs_read_file_sync));
            set_function(ctx, ops, "op_fs_write_file_sync", Some(op_fs_write_file_sync));
            set_function(ctx, ops, "op_fs_read_file_async", Some(op_fs_read_file_async));
            set_function(ctx, ops, "op_fs_write_file_async", Some(op_fs_write_file_async));

            // Track unhandled promise rejections so they fail the process.
            let name = JsString::new("onUnhandledRejection");
            let cb = JSObjectMakeFunctionWithCallback(ctx, name.as_raw(), Some(on_unhandled_rejection));
            let mut exc: JSValueRef = ptr::null();
            JSGlobalContextSetUnhandledRejectionCallback(self.ctx, cb, &mut exc);
            if !exc.is_null() {
                return Err(JsError(format_exception(ctx, exc)));
            }

            // bootstrap.js evaluates to a function that receives the ops object.
            let init = self.eval_raw(BOOTSTRAP, "runtime:bootstrap.js")?;
            let init = JSValueToObject(ctx, init, ptr::null_mut());
            if init.is_null() || !JSObjectIsFunction(ctx, init) {
                return Err(JsError("bootstrap.js must evaluate to a function".into()));
            }
            let arg: JSValueRef = ops;
            let mut exc: JSValueRef = ptr::null();
            JSObjectCallAsFunction(ctx, init, ptr::null_mut(), 1, &arg, &mut exc);
            if !exc.is_null() {
                return Err(JsError(format_exception(ctx, exc)));
            }
        }
        Ok(())
    }

    fn eval_raw(&self, code: &str, source_url: &str) -> Result<JSValueRef, JsError> {
        let ctx = self.ctx as JSContextRef;
        let script = JsString::new(code);
        let url = JsString::new(source_url);
        let mut exc: JSValueRef = ptr::null();
        // SAFETY: ctx, script and url are valid for the duration of the call.
        let value = unsafe { JSEvaluateScript(ctx, script.as_raw(), ptr::null_mut(), url.as_raw(), 1, &mut exc) };
        if !exc.is_null() {
            return Err(JsError(unsafe { format_exception(ctx, exc) }));
        }
        self.check_unhandled_rejections()?;
        Ok(value)
    }

    /// Evaluates a script, returning its completion value as a string
    /// (`None` when the value is `undefined`).
    pub fn execute_script(&self, source_url: &str, code: &str) -> Result<Option<String>, JsError> {
        let value = self.eval_raw(code, source_url)?;
        let ctx = self.ctx as JSContextRef;
        // SAFETY: value was just returned by JSC for this context.
        unsafe {
            if value.is_null() || JSValueIsUndefined(ctx, value) {
                Ok(None)
            } else {
                Ok(Some(value_to_string(ctx, value)))
            }
        }
    }

    /// Drives pending async ops until none remain.
    pub async fn run_event_loop(&mut self) -> Result<(), JsError> {
        while pending_count() > 0 {
            let Some((id, outcome)) = self.rx.recv().await else { break };
            let Some(promise) = STATE.with(|s| s.borrow_mut().as_mut().unwrap().pending.remove(&id)) else {
                continue;
            };
            let ctx = self.ctx as JSContextRef;
            // SAFETY: resolve/reject were protected when the op was created.
            unsafe {
                let (func, arg) = match outcome {
                    OpOutcome::Undefined => (promise.resolve, JSValueMakeUndefined(ctx)),
                    OpOutcome::String(s) => (promise.resolve, make_string(ctx, &s)),
                    OpOutcome::Error(e) => (promise.reject, make_error(ctx, &e)),
                };
                let mut exc: JSValueRef = ptr::null();
                JSObjectCallAsFunction(ctx, func, ptr::null_mut(), 1, &arg, &mut exc);
                JSValueUnprotect(ctx, promise.resolve);
                JSValueUnprotect(ctx, promise.reject);
                if !exc.is_null() {
                    return Err(JsError(format_exception(ctx, exc)));
                }
            }
            self.check_unhandled_rejections()?;
        }
        Ok(())
    }

    fn check_unhandled_rejections(&self) -> Result<(), JsError> {
        let rejections = STATE.with(|s| std::mem::take(&mut s.borrow_mut().as_mut().unwrap().unhandled_rejections));
        match rejections.into_iter().next() {
            Some(reason) => Err(JsError(format!("Uncaught (in promise) {reason}"))),
            None => Ok(()),
        }
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        let ctx = self.ctx as JSContextRef;
        STATE.with(|s| {
            if let Some(state) = s.borrow_mut().take() {
                for (_, p) in state.pending {
                    // SAFETY: values were protected on creation.
                    unsafe {
                        JSValueUnprotect(ctx, p.resolve);
                        JSValueUnprotect(ctx, p.reject);
                    }
                }
            }
        });
        // The global context is intentionally NOT released. Bun's JSC fork
        // does not support tearing down the VM (Bun never does it) and
        // `JSGlobalContextRelease` fail-fasts with STATUS_STACK_BUFFER_OVERRUN
        // (0xC0000409). The runtime lives for the whole process, so the OS
        // reclaims the memory on exit.
    }
}

fn pending_count() -> usize {
    STATE.with(|s| s.borrow().as_ref().map_or(0, |st| st.pending.len()))
}

// ---------------------------------------------------------------------------
// Op helpers
// ---------------------------------------------------------------------------

unsafe fn arg(args: *const JSValueRef, argc: usize, i: usize) -> Option<JSValueRef> {
    if i < argc { Some(unsafe { *args.add(i) }) } else { None }
}

unsafe fn arg_string(ctx: JSContextRef, args: *const JSValueRef, argc: usize, i: usize) -> String {
    unsafe { arg(args, argc, i).map(|v| value_to_string(ctx, v)).unwrap_or_default() }
}

unsafe fn throw(ctx: JSContextRef, exception: *mut JSValueRef, message: &str) -> JSValueRef {
    unsafe {
        if !exception.is_null() {
            *exception = make_error(ctx, message);
        }
        JSValueMakeUndefined(ctx)
    }
}

/// Creates a deferred promise and spawns `work` on tokio; the event loop
/// settles the promise when `work` completes.
unsafe fn spawn_async_op<F>(ctx: JSContextRef, exception: *mut JSValueRef, work: F) -> JSValueRef
where
    F: Future<Output = OpOutcome> + Send + 'static,
{
    unsafe {
        let mut resolve: JSObjectRef = ptr::null_mut();
        let mut reject: JSObjectRef = ptr::null_mut();
        let promise = JSObjectMakeDeferredPromise(ctx, &mut resolve, &mut reject, exception);
        if promise.is_null() {
            return JSValueMakeUndefined(ctx);
        }
        JSValueProtect(ctx, resolve);
        JSValueProtect(ctx, reject);

        let (id, tx) = STATE.with(|s| {
            let mut s = s.borrow_mut();
            let st = s.as_mut().expect("runtime state not initialised");
            let id = st.next_id;
            st.next_id += 1;
            st.pending.insert(id, PendingPromise { resolve, reject });
            (id, st.tx.clone())
        });

        tokio::spawn(async move {
            let outcome = work.await;
            let _ = tx.send((id, outcome));
        });

        promise
    }
}

fn io_outcome<T>(res: std::io::Result<T>, ok: impl FnOnce(T) -> OpOutcome) -> OpOutcome {
    match res {
        Ok(v) => ok(v),
        Err(e) => OpOutcome::Error(e.to_string()),
    }
}

// ---------------------------------------------------------------------------
// Ops (exposed to bootstrap.js)
// ---------------------------------------------------------------------------

unsafe extern "C" fn op_custom_print(
    ctx: JSContextRef,
    _f: JSObjectRef,
    _this: JSObjectRef,
    argc: usize,
    args: *const JSValueRef,
    _exception: *mut JSValueRef,
) -> JSValueRef {
    unsafe {
        let msg = arg_string(ctx, args, argc, 0);
        let is_err = arg(args, argc, 1).is_some_and(|v| JSValueToBoolean(ctx, v));
        if is_err {
            eprintln!("{msg}");
        } else {
            println!("{msg}");
        }
        JSValueMakeUndefined(ctx)
    }
}

unsafe extern "C" fn op_sleep(
    ctx: JSContextRef,
    _f: JSObjectRef,
    _this: JSObjectRef,
    argc: usize,
    args: *const JSValueRef,
    exception: *mut JSValueRef,
) -> JSValueRef {
    unsafe {
        let delay = arg(args, argc, 0)
            .map(|v| JSValueToNumber(ctx, v, ptr::null_mut()))
            .filter(|n| n.is_finite() && *n > 0.0)
            .unwrap_or(0.0) as u64;
        spawn_async_op(ctx, exception, async move {
            tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
            OpOutcome::Undefined
        })
    }
}

unsafe extern "C" fn op_fs_read_file_sync(
    ctx: JSContextRef,
    _f: JSObjectRef,
    _this: JSObjectRef,
    argc: usize,
    args: *const JSValueRef,
    exception: *mut JSValueRef,
) -> JSValueRef {
    unsafe {
        let path = arg_string(ctx, args, argc, 0);
        match fs::read_to_string(&path) {
            Ok(contents) => make_string(ctx, &contents),
            Err(e) => throw(ctx, exception, &e.to_string()),
        }
    }
}

unsafe extern "C" fn op_fs_write_file_sync(
    ctx: JSContextRef,
    _f: JSObjectRef,
    _this: JSObjectRef,
    argc: usize,
    args: *const JSValueRef,
    exception: *mut JSValueRef,
) -> JSValueRef {
    unsafe {
        let path = arg_string(ctx, args, argc, 0);
        let contents = arg_string(ctx, args, argc, 1);
        match fs::write(&path, contents) {
            Ok(()) => JSValueMakeUndefined(ctx),
            Err(e) => throw(ctx, exception, &e.to_string()),
        }
    }
}

unsafe extern "C" fn op_fs_read_file_async(
    ctx: JSContextRef,
    _f: JSObjectRef,
    _this: JSObjectRef,
    argc: usize,
    args: *const JSValueRef,
    exception: *mut JSValueRef,
) -> JSValueRef {
    unsafe {
        let path = arg_string(ctx, args, argc, 0);
        spawn_async_op(ctx, exception, async move {
            io_outcome(tokio::fs::read_to_string(path).await, OpOutcome::String)
        })
    }
}

unsafe extern "C" fn op_fs_write_file_async(
    ctx: JSContextRef,
    _f: JSObjectRef,
    _this: JSObjectRef,
    argc: usize,
    args: *const JSValueRef,
    exception: *mut JSValueRef,
) -> JSValueRef {
    unsafe {
        let path = arg_string(ctx, args, argc, 0);
        let contents = arg_string(ctx, args, argc, 1);
        spawn_async_op(ctx, exception, async move {
            io_outcome(tokio::fs::write(path, contents).await, |()| OpOutcome::Undefined)
        })
    }
}

unsafe extern "C" fn on_unhandled_rejection(
    ctx: JSContextRef,
    _f: JSObjectRef,
    _this: JSObjectRef,
    argc: usize,
    args: *const JSValueRef,
    _exception: *mut JSValueRef,
) -> JSValueRef {
    unsafe {
        // JSC invokes the callback as (promise, reason).
        let reason = arg(args, argc, 1)
            .map(|v| format_exception(ctx, v))
            .unwrap_or_else(|| "undefined".to_string());
        STATE.with(|s| {
            if let Some(st) = s.borrow_mut().as_mut() {
                st.unhandled_rejections.push(reason);
            }
        });
        JSValueMakeUndefined(ctx)
    }
}
