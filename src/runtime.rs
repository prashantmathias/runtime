//! The JavaScriptCore-backed runtime: native ops, HTTP/WebSocket servers and
//! clients, module loading, the async event loop, and script/REPL execution.
//!
//! Each async op returns a JS promise (created with `JSObjectMakeDeferredPromise`),
//! runs the actual I/O on tokio, and resolves/rejects the promise on the JS
//! thread once the work completes. Incoming HTTP requests and WebSocket events
//! are delivered over the same event channel to keep all JSC access on the
//! single host thread.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::ptr;
use std::sync::{Arc, Mutex};

use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use http_body_util::{BodyExt, Full};
use hyper::body::{Bytes, Incoming};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::upgrade::OnUpgrade;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use sha1::{Digest, Sha1};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::sync::oneshot;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, Role, WebSocketConfig};
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::Message as WsMessage;

use crate::jsc::*;
use crate::transpile::resolve_and_load_module;

const BOOTSTRAP: &str = include_str!("bootstrap.js");
const WS_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// Result of an async op, produced off the JS thread.
enum OpOutcome {
    Undefined,
    String(String),
    FetchResponse {
        status: u16,
        status_text: String,
        headers_json: String,
        body: Vec<u8>,
    },
    Error(String),
}

/// Events delivered from background Tokio tasks to the main JS thread.
enum RuntimeEvent {
    OpComplete(u64, OpOutcome),
    HttpRequest {
        server_id: u64,
        req_id: u64,
        method: String,
        url: String,
        headers_json: String,
        body: Vec<u8>,
        can_upgrade: bool,
    },
    WsEvent {
        ws_id: u64,
        kind: WsEventKind,
    },
}

enum WsEventKind {
    Open { protocol: String },
    MessageText(String),
    MessageBinary(Vec<u8>),
    Close { code: u16, reason: String },
    Error(String),
}

enum WsCommand {
    Text(String),
    Binary(Vec<u8>),
    Close(Option<(u16, String)>),
}

enum HttpResponseAction {
    Respond {
        status: u16,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    },
    Upgrade {
        ws_id: u64,
        headers: Vec<(String, String)>,
        ws_cmd_rx: UnboundedReceiver<WsCommand>,
    },
}

struct PendingHttpRequest {
    tx: oneshot::Sender<HttpResponseAction>,
    can_upgrade: bool,
    upgraded_ws_id: Option<(u64, UnboundedReceiver<WsCommand>, Vec<(String, String)>)>,
}

struct PendingPromise {
    resolve: JSObjectRef,
    reject: JSObjectRef,
}

struct OpState {
    next_id: u64,
    pending: HashMap<u64, PendingPromise>,
    servers: HashMap<u64, oneshot::Sender<()>>,
    http_requests: HashMap<u64, PendingHttpRequest>,
    websockets: HashMap<u64, UnboundedSender<WsCommand>>,
    on_http_request: Option<JSObjectRef>,
    on_ws_event: Option<JSObjectRef>,
    tx: UnboundedSender<RuntimeEvent>,
    unhandled_rejections: Vec<String>,
}

impl OpState {
    fn alloc_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn has_active_work(&self) -> bool {
        !self.pending.is_empty()
            || !self.servers.is_empty()
            || !self.http_requests.is_empty()
            || !self.websockets.is_empty()
    }
}

thread_local! {
    static STATE: RefCell<Option<OpState>> = const { RefCell::new(None) };
}

pub struct Runtime {
    ctx: JSGlobalContextRef,
    rx: UnboundedReceiver<RuntimeEvent>,
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
                next_id: 1,
                pending: HashMap::new(),
                servers: HashMap::new(),
                http_requests: HashMap::new(),
                websockets: HashMap::new(),
                on_http_request: None,
                on_ws_event: None,
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
            set_function(ctx, ops, "op_load_module", Some(op_load_module));
            set_function(ctx, ops, "op_utf8_encode", Some(op_utf8_encode));
            set_function(ctx, ops, "op_utf8_decode", Some(op_utf8_decode));
            set_function(ctx, ops, "op_random_uuid", Some(op_random_uuid));
            set_function(ctx, ops, "op_set_event_handlers", Some(op_set_event_handlers));
            set_function(ctx, ops, "op_http_serve", Some(op_http_serve));
            set_function(ctx, ops, "op_http_stop", Some(op_http_stop));
            set_function(ctx, ops, "op_http_respond", Some(op_http_respond));
            set_function(ctx, ops, "op_ws_upgrade", Some(op_ws_upgrade));
            set_function(ctx, ops, "op_ws_connect", Some(op_ws_connect));
            set_function(ctx, ops, "op_ws_send_text", Some(op_ws_send_text));
            set_function(ctx, ops, "op_ws_send_binary", Some(op_ws_send_binary));
            set_function(ctx, ops, "op_ws_close", Some(op_ws_close));
            set_function(ctx, ops, "op_fetch", Some(op_fetch));

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
        unsafe {
            let global = JSContextGetGlobalObject(ctx);
            set_property(ctx, global, "__entryFilename", make_string(ctx, source_url));
        }
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

    /// Drives pending async ops, HTTP servers, and WebSockets until none remain.
    pub async fn run_event_loop(&mut self) -> Result<(), JsError> {
        while has_active_work() {
            let Some(event) = self.rx.recv().await else { break };
            let ctx = self.ctx as JSContextRef;
            match event {
                RuntimeEvent::OpComplete(id, outcome) => {
                    let Some(promise) = STATE.with(|s| s.borrow_mut().as_mut().unwrap().pending.remove(&id)) else {
                        continue;
                    };
                    unsafe {
                        let (func, arg) = match outcome {
                            OpOutcome::Undefined => (promise.resolve, JSValueMakeUndefined(ctx)),
                            OpOutcome::String(s) => (promise.resolve, make_string(ctx, &s)),
                            OpOutcome::FetchResponse {
                                status,
                                status_text,
                                headers_json,
                                body,
                            } => {
                                let obj = JSObjectMake(ctx, ptr::null_mut(), ptr::null_mut());
                                set_property(ctx, obj, "status", JSValueMakeNumber(ctx, status as f64));
                                set_property(ctx, obj, "statusText", make_string(ctx, &status_text));
                                set_property(ctx, obj, "headersJson", make_string(ctx, &headers_json));
                                let body_arr = make_uint8_array(ctx, &body);
                                set_property(ctx, obj, "body", body_arr);
                                (promise.resolve, obj as JSValueRef)
                            }
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
                }
                RuntimeEvent::HttpRequest {
                    server_id,
                    req_id,
                    method,
                    url,
                    headers_json,
                    body,
                    can_upgrade,
                } => {
                    let handler = STATE.with(|s| s.borrow().as_ref().and_then(|st| st.on_http_request));
                    if let Some(handler) = handler {
                        unsafe {
                            let body_val = make_uint8_array(ctx, &body) as JSValueRef;
                            let args: [JSValueRef; 7] = [
                                JSValueMakeNumber(ctx, server_id as f64),
                                JSValueMakeNumber(ctx, req_id as f64),
                                make_string(ctx, &method),
                                make_string(ctx, &url),
                                make_string(ctx, &headers_json),
                                body_val,
                                JSValueMakeBoolean(ctx, can_upgrade),
                            ];
                            let mut exc: JSValueRef = ptr::null();
                            JSObjectCallAsFunction(ctx, handler, ptr::null_mut(), args.len(), args.as_ptr(), &mut exc);
                            if !exc.is_null() {
                                return Err(JsError(format_exception(ctx, exc)));
                            }
                        }
                    }
                }
                RuntimeEvent::WsEvent { ws_id, kind } => {
                    if matches!(kind, WsEventKind::Close { .. }) {
                        STATE.with(|s| {
                            if let Some(st) = s.borrow_mut().as_mut() {
                                st.websockets.remove(&ws_id);
                            }
                        });
                    }
                    let handler = STATE.with(|s| s.borrow().as_ref().and_then(|st| st.on_ws_event));
                    if let Some(handler) = handler {
                        unsafe {
                            let id_val = JSValueMakeNumber(ctx, ws_id as f64);
                            let mut exc: JSValueRef = ptr::null();
                            match kind {
                                WsEventKind::Open { protocol } => {
                                    let args = [id_val, make_string(ctx, "open"), make_string(ctx, &protocol)];
                                    JSObjectCallAsFunction(ctx, handler, ptr::null_mut(), args.len(), args.as_ptr(), &mut exc);
                                }
                                WsEventKind::MessageText(text) => {
                                    let args = [id_val, make_string(ctx, "message"), make_string(ctx, &text)];
                                    JSObjectCallAsFunction(ctx, handler, ptr::null_mut(), args.len(), args.as_ptr(), &mut exc);
                                }
                                WsEventKind::MessageBinary(bytes) => {
                                    let arr = make_uint8_array(ctx, &bytes) as JSValueRef;
                                    let args = [id_val, make_string(ctx, "message"), arr];
                                    JSObjectCallAsFunction(ctx, handler, ptr::null_mut(), args.len(), args.as_ptr(), &mut exc);
                                }
                                WsEventKind::Close { code, reason } => {
                                    let args = [
                                        id_val,
                                        make_string(ctx, "close"),
                                        JSValueMakeNumber(ctx, code as f64),
                                        make_string(ctx, &reason),
                                    ];
                                    JSObjectCallAsFunction(ctx, handler, ptr::null_mut(), args.len(), args.as_ptr(), &mut exc);
                                }
                                WsEventKind::Error(err) => {
                                    let args = [id_val, make_string(ctx, "error"), make_string(ctx, &err)];
                                    JSObjectCallAsFunction(ctx, handler, ptr::null_mut(), args.len(), args.as_ptr(), &mut exc);
                                }
                            }
                            if !exc.is_null() {
                                return Err(JsError(format_exception(ctx, exc)));
                            }
                        }
                    }
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
                    unsafe {
                        JSValueUnprotect(ctx, p.resolve);
                        JSValueUnprotect(ctx, p.reject);
                    }
                }
                if let Some(cb) = state.on_http_request {
                    unsafe { JSValueUnprotect(ctx, cb) };
                }
                if let Some(cb) = state.on_ws_event {
                    unsafe { JSValueUnprotect(ctx, cb) };
                }
            }
        });
        // The global context is intentionally NOT released. Bun's JSC fork
        // does not support tearing down the VM (Bun never does it).
    }
}

fn has_active_work() -> bool {
    STATE.with(|s| s.borrow().as_ref().is_some_and(|st| st.has_active_work()))
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

unsafe fn arg_u64(ctx: JSContextRef, args: *const JSValueRef, argc: usize, i: usize) -> u64 {
    unsafe {
        arg(args, argc, i)
            .map(|v| JSValueToNumber(ctx, v, ptr::null_mut()))
            .filter(|n| n.is_finite() && *n >= 0.0)
            .unwrap_or(0.0) as u64
    }
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
            let id = st.alloc_id();
            st.pending.insert(id, PendingPromise { resolve, reject });
            (id, st.tx.clone())
        });

        tokio::spawn(async move {
            let outcome = work.await;
            let _ = tx.send(RuntimeEvent::OpComplete(id, outcome));
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
// Core Ops (console, timers, fs, modules, utf8, uuid)
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
        let delay = arg_u64(ctx, args, argc, 0);
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

unsafe extern "C" fn op_load_module(
    ctx: JSContextRef,
    _f: JSObjectRef,
    _this: JSObjectRef,
    argc: usize,
    args: *const JSValueRef,
    exception: *mut JSValueRef,
) -> JSValueRef {
    unsafe {
        let specifier = arg_string(ctx, args, argc, 0);
        let referrer = arg_string(ctx, args, argc, 1);
        match resolve_and_load_module(&specifier, &referrer) {
            Ok((id, code, is_json)) => {
                let obj = JSObjectMake(ctx, ptr::null_mut(), ptr::null_mut());
                set_property(ctx, obj, "id", make_string(ctx, &id));
                set_property(ctx, obj, "code", make_string(ctx, &code));
                set_property(ctx, obj, "isJson", JSValueMakeBoolean(ctx, is_json));
                obj as JSValueRef
            }
            Err(err) => throw(ctx, exception, &err),
        }
    }
}

unsafe extern "C" fn op_utf8_encode(
    ctx: JSContextRef,
    _f: JSObjectRef,
    _this: JSObjectRef,
    argc: usize,
    args: *const JSValueRef,
    _exception: *mut JSValueRef,
) -> JSValueRef {
    unsafe {
        let s = arg_string(ctx, args, argc, 0);
        make_uint8_array(ctx, s.as_bytes()) as JSValueRef
    }
}

unsafe extern "C" fn op_utf8_decode(
    ctx: JSContextRef,
    _f: JSObjectRef,
    _this: JSObjectRef,
    argc: usize,
    args: *const JSValueRef,
    _exception: *mut JSValueRef,
) -> JSValueRef {
    unsafe {
        let bytes = arg(args, argc, 0)
            .and_then(|v| read_uint8_array(ctx, v))
            .unwrap_or_default();
        let s = String::from_utf8_lossy(&bytes);
        make_string(ctx, &s)
    }
}

unsafe extern "C" fn op_random_uuid(
    ctx: JSContextRef,
    _f: JSObjectRef,
    _this: JSObjectRef,
    _argc: usize,
    _args: *const JSValueRef,
    _exception: *mut JSValueRef,
) -> JSValueRef {
    use std::time::{SystemTime, UNIX_EPOCH};
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut hasher = Sha1::new();
    hasher.update(nanos.to_le_bytes());
    hasher.update(seq.to_le_bytes());
    hasher.update(std::process::id().to_le_bytes());
    let digest = hasher.finalize();
    let mut b = [0u8; 16];
    b.copy_from_slice(&digest[..16]);
    b[6] = (b[6] & 0x0f) | 0x40; // version 4
    b[8] = (b[8] & 0x3f) | 0x80; // variant 1
    let uuid = format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]
    );
    unsafe { make_string(ctx, &uuid) }
}

// ---------------------------------------------------------------------------
// HTTP Server & WebSocket Ops
// ---------------------------------------------------------------------------

unsafe extern "C" fn op_set_event_handlers(
    ctx: JSContextRef,
    _f: JSObjectRef,
    _this: JSObjectRef,
    argc: usize,
    args: *const JSValueRef,
    _exception: *mut JSValueRef,
) -> JSValueRef {
    unsafe {
        let http_cb = arg(args, argc, 0).map(|v| JSValueToObject(ctx, v, ptr::null_mut()));
        let ws_cb = arg(args, argc, 1).map(|v| JSValueToObject(ctx, v, ptr::null_mut()));
        STATE.with(|s| {
            let mut s = s.borrow_mut();
            let st = s.as_mut().expect("runtime state not initialised");
            if let Some(old) = st.on_http_request.take() {
                JSValueUnprotect(ctx, old);
            }
            if let Some(old) = st.on_ws_event.take() {
                JSValueUnprotect(ctx, old);
            }
            if let Some(cb) = http_cb.filter(|p| !p.is_null()) {
                JSValueProtect(ctx, cb);
                st.on_http_request = Some(cb);
            }
            if let Some(cb) = ws_cb.filter(|p| !p.is_null()) {
                JSValueProtect(ctx, cb);
                st.on_ws_event = Some(cb);
            }
        });
        JSValueMakeUndefined(ctx)
    }
}

/// Shared state accessible from Hyper connection tasks to register incoming
/// HTTP requests before forwarding them to the JS thread.
type SharedPendingRequests = Arc<Mutex<HashMap<u64, (oneshot::Sender<HttpResponseAction>, bool)>>>;

unsafe extern "C" fn op_http_serve(
    ctx: JSContextRef,
    _f: JSObjectRef,
    _this: JSObjectRef,
    argc: usize,
    args: *const JSValueRef,
    exception: *mut JSValueRef,
) -> JSValueRef {
    unsafe {
        let port = arg_u64(ctx, args, argc, 0) as u16;
        let hostname = {
            let h = arg_string(ctx, args, argc, 1);
            if h.is_empty() || h == "undefined" {
                "127.0.0.1".to_string()
            } else {
                h
            }
        };

        let std_listener = match std::net::TcpListener::bind((hostname.as_str(), port)) {
            Ok(l) => l,
            Err(e) => return throw(ctx, exception, &format!("Failed to bind {hostname}:{port}: {e}")),
        };
        if let Err(e) = std_listener.set_nonblocking(true) {
            return throw(ctx, exception, &format!("Failed to set nonblocking: {e}"));
        }
        let actual_port = std_listener.local_addr().map(|a| a.port()).unwrap_or(port);
        let listener = match tokio::net::TcpListener::from_std(std_listener) {
            Ok(l) => l,
            Err(e) => return throw(ctx, exception, &format!("Failed to create tokio listener: {e}")),
        };

        let (stop_tx, mut stop_rx) = oneshot::channel::<()>();
        let (server_id, event_tx) = STATE.with(|s| {
            let mut s = s.borrow_mut();
            let st = s.as_mut().expect("runtime state not initialised");
            let id = st.alloc_id();
            st.servers.insert(id, stop_tx);
            (id, st.tx.clone())
        });

        let host_for_url = hostname.clone();
        let req_counter = Arc::new( std::sync::atomic::AtomicU64::new(server_id * 1_000_000 + 1));
        let shared_reqs: SharedPendingRequests = Arc::new(Mutex::new(HashMap::new()));
        let shared_reqs_clone = shared_reqs.clone();

        // Store shared_reqs handle in thread-local bridge map via a background forwarder.
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut stop_rx => {
                        break;
                    }
                    accept_res = listener.accept() => {
                        let Ok((stream, _peer_addr)) = accept_res else { continue };
                        let event_tx = event_tx.clone();
                        let host_default = format!("{host_for_url}:{actual_port}");
                        let req_counter = req_counter.clone();
                        let shared_reqs = shared_reqs_clone.clone();

                        tokio::spawn(async move {
                            let io = TokioIo::new(stream);
                            let service = service_fn(move |mut req: Request<Incoming>| {
                                let event_tx = event_tx.clone();
                                let host_default = host_default.clone();
                                let req_counter = req_counter.clone();
                                let shared_reqs = shared_reqs.clone();

                                async move {
                                    handle_hyper_request(
                                        &mut req,
                                        server_id,
                                        &host_default,
                                        &req_counter,
                                        &shared_reqs,
                                        &event_tx,
                                    )
                                    .await
                                }
                            });

                            let _ = http1::Builder::new()
                                .serve_connection(io, service)
                                .with_upgrades()
                                .await;
                        });
                    }
                }
            }
        });

        // Register shared_reqs in thread-local registry so op_http_respond / op_ws_upgrade can claim requests.
        HTTP_SHARED_REQS.with(|m| {
            m.borrow_mut().insert(server_id, shared_reqs);
        });

        let obj = JSObjectMake(ctx, ptr::null_mut(), ptr::null_mut());
        set_property(ctx, obj, "serverId", JSValueMakeNumber(ctx, server_id as f64));
        set_property(ctx, obj, "port", JSValueMakeNumber(ctx, actual_port as f64));
        set_property(ctx, obj, "hostname", make_string(ctx, &hostname));
        obj as JSValueRef
    }
}

thread_local! {
    static HTTP_SHARED_REQS: RefCell<HashMap<u64, SharedPendingRequests>> = RefCell::new(HashMap::new());
}

fn take_shared_req(req_id: u64) -> Option<(oneshot::Sender<HttpResponseAction>, bool)> {
    HTTP_SHARED_REQS.with(|m| {
        for shared in m.borrow().values() {
            if let Ok(mut guard) = shared.lock()
                && let Some(entry) = guard.remove(&req_id)
            {
                return Some(entry);
            }
        }
        None
    })
}

async fn handle_hyper_request(
    req: &mut Request<Incoming>,
    server_id: u64,
    host_default: &str,
    req_counter: &std::sync::atomic::AtomicU64,
    shared_reqs: &SharedPendingRequests,
    event_tx: &UnboundedSender<RuntimeEvent>,
) -> Result<Response<Full<Bytes>>, std::convert::Infallible> {
    let method = req.method().as_str().to_string();
    let host = req
        .headers()
        .get(hyper::header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or(host_default)
        .to_string();
    let path_and_query = req
        .uri()
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or("/");
    let url = format!("http://{host}{path_and_query}");

    let is_ws_upgrade = req
        .headers()
        .get(hyper::header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("websocket"));
    let ws_key = req
        .headers()
        .get("sec-websocket-key")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let can_upgrade = is_ws_upgrade && ws_key.is_some();
    let on_upgrade: Option<OnUpgrade> = if can_upgrade {
        Some(hyper::upgrade::on(&mut *req))
    } else {
        None
    };

    let headers_vec: Vec<(String, String)> = req
        .headers()
        .iter()
        .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or("").to_string()))
        .collect();
    let headers_json = serde_json::to_string(&headers_vec).unwrap_or_else(|_| "[]".into());

    let body_bytes = match req.body_mut().collect().await {
        Ok(collected) => collected.to_bytes().to_vec(),
        Err(_) => Vec::new(),
    };

    let req_id = req_counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let (resp_tx, resp_rx) = oneshot::channel::<HttpResponseAction>();
    if let Ok(mut guard) = shared_reqs.lock() {
        guard.insert(req_id, (resp_tx, can_upgrade));
    }

    if event_tx
        .send(RuntimeEvent::HttpRequest {
            server_id,
            req_id,
            method,
            url,
            headers_json,
            body: body_bytes,
            can_upgrade,
        })
        .is_err()
    {
        return Ok(Response::builder()
            .status(StatusCode::SERVICE_UNAVAILABLE)
            .body(Full::new(Bytes::new()))
            .unwrap());
    }

    match resp_rx.await {
        Ok(HttpResponseAction::Respond { status, headers, body }) => {
            let mut builder = Response::builder().status(StatusCode::from_u16(status).unwrap_or(StatusCode::OK));
            for (k, v) in headers {
                builder = builder.header(k, v);
            }
            Ok(builder.body(Full::new(Bytes::from(body))).unwrap())
        }
        Ok(HttpResponseAction::Upgrade {
            ws_id,
            headers,
            ws_cmd_rx,
        }) => {
            let key = ws_key.unwrap_or_default();
            let accept_val = compute_ws_accept(&key);
            let mut builder = Response::builder()
                .status(StatusCode::SWITCHING_PROTOCOLS)
                .header(hyper::header::UPGRADE, "websocket")
                .header(hyper::header::CONNECTION, "Upgrade")
                .header("Sec-WebSocket-Accept", accept_val);
            let mut negotiated_protocol = String::new();
            for (k, v) in headers {
                if k.eq_ignore_ascii_case("sec-websocket-protocol") {
                    negotiated_protocol = v.clone();
                }
                builder = builder.header(k, v);
            }

            if let Some(on_upgrade) = on_upgrade {
                let event_tx = event_tx.clone();
                tokio::spawn(async move {
                    match on_upgrade.await {
                        Ok(upgraded) => {
                            let io = TokioIo::new(upgraded);
                            let ws_stream = WebSocketStream::from_raw_socket(
                                io,
                                Role::Server,
                                Some(WebSocketConfig::default()),
                            )
                            .await;
                            run_ws_pump(ws_id, negotiated_protocol, ws_stream, ws_cmd_rx, event_tx).await;
                        }
                        Err(e) => {
                            let _ = event_tx.send(RuntimeEvent::WsEvent {
                                ws_id,
                                kind: WsEventKind::Error(e.to_string()),
                            });
                            let _ = event_tx.send(RuntimeEvent::WsEvent {
                                ws_id,
                                kind: WsEventKind::Close {
                                    code: 1006,
                                    reason: e.to_string(),
                                },
                            });
                        }
                    }
                });
            }

            Ok(builder.body(Full::new(Bytes::new())).unwrap())
        }
        Err(_) => Ok(Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .body(Full::new(Bytes::from_static(b"Request handler dropped")))
            .unwrap()),
    }
}

fn compute_ws_accept(key: &str) -> String {
    let mut hasher = Sha1::new();
    hasher.update(key.trim().as_bytes());
    hasher.update(WS_GUID.as_bytes());
    base64::engine::general_purpose::STANDARD.encode(hasher.finalize())
}

async fn run_ws_pump<S>(
    ws_id: u64,
    protocol: String,
    mut ws_stream: WebSocketStream<S>,
    mut cmd_rx: UnboundedReceiver<WsCommand>,
    event_tx: UnboundedSender<RuntimeEvent>,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let _ = event_tx.send(RuntimeEvent::WsEvent {
        ws_id,
        kind: WsEventKind::Open { protocol },
    });

    let mut close_info: Option<(u16, String)> = None;
    let mut closing = false;

    loop {
        tokio::select! {
            cmd = cmd_rx.recv(), if !closing => {
                let Some(cmd) = cmd else { break };
                match cmd {
                    WsCommand::Text(t) => {
                        if ws_stream.send(WsMessage::Text(t.into())).await.is_err() {
                            break;
                        }
                    }
                    WsCommand::Binary(b) => {
                        if ws_stream.send(WsMessage::Binary(b.into())).await.is_err() {
                            break;
                        }
                    }
                    WsCommand::Close(frame) => {
                        closing = true;
                        let cf = frame.as_ref().map(|(code, reason)| CloseFrame {
                            code: CloseCode::from(*code),
                            reason: reason.clone().into(),
                        });
                        if let Some((code, reason)) = frame {
                            close_info = Some((code, reason));
                        }
                        let _ = ws_stream.close(cf).await;
                        let _ = ws_stream.flush().await;
                    }
                }
            }
            item = ws_stream.next() => {
                let Some(item) = item else { break };
                match item {
                    Ok(WsMessage::Text(t)) => {
                        let _ = event_tx.send(RuntimeEvent::WsEvent {
                            ws_id,
                            kind: WsEventKind::MessageText(t.to_string()),
                        });
                    }
                    Ok(WsMessage::Binary(b)) => {
                        let _ = event_tx.send(RuntimeEvent::WsEvent {
                            ws_id,
                            kind: WsEventKind::MessageBinary(b.to_vec()),
                        });
                    }
                    Ok(WsMessage::Close(frame)) => {
                        let (code, reason) = frame
                            .map(|f| (u16::from(f.code), f.reason.to_string()))
                            .unwrap_or((1000, String::new()));
                        close_info = Some((code, reason));
                        let _ = ws_stream.flush().await;
                        break;
                    }
                    Ok(WsMessage::Ping(_) | WsMessage::Pong(_) | WsMessage::Frame(_)) => {}
                    Err(e) => {
                        if !closing {
                            let _ = event_tx.send(RuntimeEvent::WsEvent {
                                ws_id,
                                kind: WsEventKind::Error(e.to_string()),
                            });
                            close_info = Some((1006, e.to_string()));
                        }
                        break;
                    }
                }
            }
        }
    }

    let (code, reason) = close_info.unwrap_or((1000, String::new()));
    let _ = event_tx.send(RuntimeEvent::WsEvent {
        ws_id,
        kind: WsEventKind::Close { code, reason },
    });
}

unsafe extern "C" fn op_http_stop(
    ctx: JSContextRef,
    _f: JSObjectRef,
    _this: JSObjectRef,
    argc: usize,
    args: *const JSValueRef,
    _exception: *mut JSValueRef,
) -> JSValueRef {
    unsafe {
        let server_id = arg_u64(ctx, args, argc, 0);
        HTTP_SHARED_REQS.with(|m| {
            m.borrow_mut().remove(&server_id);
        });
        STATE.with(|s| {
            if let Some(st) = s.borrow_mut().as_mut()
                && let Some(stop_tx) = st.servers.remove(&server_id)
            {
                let _ = stop_tx.send(());
            }
        });
        JSValueMakeUndefined(ctx)
    }
}

fn ensure_http_req_in_state(st: &mut OpState, req_id: u64) {
    if !st.http_requests.contains_key(&req_id)
        && let Some((tx, can_upgrade)) = take_shared_req(req_id)
    {
        st.http_requests.insert(
            req_id,
            PendingHttpRequest {
                tx,
                can_upgrade,
                upgraded_ws_id: None,
            },
        );
    }
}

unsafe extern "C" fn op_ws_upgrade(
    ctx: JSContextRef,
    _f: JSObjectRef,
    _this: JSObjectRef,
    argc: usize,
    args: *const JSValueRef,
    _exception: *mut JSValueRef,
) -> JSValueRef {
    unsafe {
        let req_id = arg_u64(ctx, args, argc, 0);
        let headers_json = arg_string(ctx, args, argc, 1);
        let extra_headers: Vec<(String, String)> = serde_json::from_str(&headers_json).unwrap_or_default();

        let ws_id = STATE.with(|s| {
            let mut s = s.borrow_mut();
            let st = s.as_mut().expect("runtime state not initialised");
            ensure_http_req_in_state(st, req_id);
            let ws_id = st.alloc_id();
            let Some(req_entry) = st.http_requests.get_mut(&req_id) else {
                return 0;
            };
            if !req_entry.can_upgrade {
                return 0;
            }
            let (cmd_tx, cmd_rx) = unbounded_channel();
            req_entry.upgraded_ws_id = Some((ws_id, cmd_rx, extra_headers));
            st.websockets.insert(ws_id, cmd_tx);
            ws_id
        });

        JSValueMakeNumber(ctx, ws_id as f64)
    }
}

unsafe extern "C" fn op_http_respond(
    ctx: JSContextRef,
    _f: JSObjectRef,
    _this: JSObjectRef,
    argc: usize,
    args: *const JSValueRef,
    _exception: *mut JSValueRef,
) -> JSValueRef {
    unsafe {
        let req_id = arg_u64(ctx, args, argc, 0);
        let status = arg_u64(ctx, args, argc, 1) as u16;
        let headers_json = arg_string(ctx, args, argc, 2);
        let headers: Vec<(String, String)> = serde_json::from_str(&headers_json).unwrap_or_default();
        let body = arg(args, argc, 3)
            .and_then(|v| read_uint8_array(ctx, v))
            .unwrap_or_default();

        STATE.with(|s| {
            let mut s = s.borrow_mut();
            let st = s.as_mut().expect("runtime state not initialised");
            ensure_http_req_in_state(st, req_id);
            if let Some(pending) = st.http_requests.remove(&req_id) {
                if let Some((ws_id, ws_cmd_rx, mut upgrade_headers)) = pending.upgraded_ws_id {
                    upgrade_headers.extend(headers);
                    let _ = pending.tx.send(HttpResponseAction::Upgrade {
                        ws_id,
                        headers: upgrade_headers,
                        ws_cmd_rx,
                    });
                } else {
                    let _ = pending.tx.send(HttpResponseAction::Respond {
                        status,
                        headers,
                        body,
                    });
                }
            }
        });

        JSValueMakeUndefined(ctx)
    }
}

unsafe extern "C" fn op_ws_connect(
    ctx: JSContextRef,
    _f: JSObjectRef,
    _this: JSObjectRef,
    argc: usize,
    args: *const JSValueRef,
    _exception: *mut JSValueRef,
) -> JSValueRef {
    unsafe {
        let url = arg_string(ctx, args, argc, 0);
        let (ws_id, cmd_rx, event_tx) = STATE.with(|s| {
            let mut s = s.borrow_mut();
            let st = s.as_mut().expect("runtime state not initialised");
            let ws_id = st.alloc_id();
            let (cmd_tx, cmd_rx) = unbounded_channel();
            st.websockets.insert(ws_id, cmd_tx);
            (ws_id, cmd_rx, st.tx.clone())
        });

        tokio::spawn(async move {
            match tokio_tungstenite::connect_async(&url).await {
                Ok((ws_stream, resp)) => {
                    let protocol = resp
                        .headers()
                        .get("sec-websocket-protocol")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("")
                        .to_string();
                    run_ws_pump(ws_id, protocol, ws_stream, cmd_rx, event_tx).await;
                }
                Err(e) => {
                    let _ = event_tx.send(RuntimeEvent::WsEvent {
                        ws_id,
                        kind: WsEventKind::Error(e.to_string()),
                    });
                    let _ = event_tx.send(RuntimeEvent::WsEvent {
                        ws_id,
                        kind: WsEventKind::Close {
                            code: 1006,
                            reason: e.to_string(),
                        },
                    });
                }
            }
        });

        JSValueMakeNumber(ctx, ws_id as f64)
    }
}

unsafe extern "C" fn op_ws_send_text(
    ctx: JSContextRef,
    _f: JSObjectRef,
    _this: JSObjectRef,
    argc: usize,
    args: *const JSValueRef,
    _exception: *mut JSValueRef,
) -> JSValueRef {
    unsafe {
        let ws_id = arg_u64(ctx, args, argc, 0);
        let text = arg_string(ctx, args, argc, 1);
        STATE.with(|s| {
            if let Some(st) = s.borrow().as_ref()
                && let Some(tx) = st.websockets.get(&ws_id)
            {
                let _ = tx.send(WsCommand::Text(text));
            }
        });
        JSValueMakeUndefined(ctx)
    }
}

unsafe extern "C" fn op_ws_send_binary(
    ctx: JSContextRef,
    _f: JSObjectRef,
    _this: JSObjectRef,
    argc: usize,
    args: *const JSValueRef,
    _exception: *mut JSValueRef,
) -> JSValueRef {
    unsafe {
        let ws_id = arg_u64(ctx, args, argc, 0);
        let bytes = arg(args, argc, 1)
            .and_then(|v| read_uint8_array(ctx, v))
            .unwrap_or_default();
        STATE.with(|s| {
            if let Some(st) = s.borrow().as_ref()
                && let Some(tx) = st.websockets.get(&ws_id)
            {
                let _ = tx.send(WsCommand::Binary(bytes));
            }
        });
        JSValueMakeUndefined(ctx)
    }
}

unsafe extern "C" fn op_ws_close(
    ctx: JSContextRef,
    _f: JSObjectRef,
    _this: JSObjectRef,
    argc: usize,
    args: *const JSValueRef,
    _exception: *mut JSValueRef,
) -> JSValueRef {
    unsafe {
        let ws_id = arg_u64(ctx, args, argc, 0);
        let code = arg_u64(ctx, args, argc, 1) as u16;
        let reason = arg_string(ctx, args, argc, 2);
        let close_arg = if code == 0 { None } else { Some((code, reason)) };
        STATE.with(|s| {
            if let Some(st) = s.borrow().as_ref()
                && let Some(tx) = st.websockets.get(&ws_id)
            {
                let _ = tx.send(WsCommand::Close(close_arg));
            }
        });
        JSValueMakeUndefined(ctx)
    }
}

// ---------------------------------------------------------------------------
// HTTP Client `fetch()` Op (Hyper over Tokio TCP)
// ---------------------------------------------------------------------------

unsafe extern "C" fn op_fetch(
    ctx: JSContextRef,
    _f: JSObjectRef,
    _this: JSObjectRef,
    argc: usize,
    args: *const JSValueRef,
    exception: *mut JSValueRef,
) -> JSValueRef {
    unsafe {
        let url_str = arg_string(ctx, args, argc, 0);
        let method_str = arg_string(ctx, args, argc, 1);
        let headers_json = arg_string(ctx, args, argc, 2);
        let body_bytes = arg(args, argc, 3)
            .and_then(|v| read_uint8_array(ctx, v))
            .unwrap_or_default();

        spawn_async_op(ctx, exception, async move {
            match perform_http_fetch(&url_str, &method_str, &headers_json, body_bytes).await {
                Ok((status, status_text, resp_headers_json, resp_body)) => OpOutcome::FetchResponse {
                    status,
                    status_text,
                    headers_json: resp_headers_json,
                    body: resp_body,
                },
                Err(e) => OpOutcome::Error(e),
            }
        })
    }
}

async fn perform_http_fetch(
    url_str: &str,
    method_str: &str,
    headers_json: &str,
    body_bytes: Vec<u8>,
) -> Result<(u16, String, String, Vec<u8>), String> {
    let parsed = url::Url::parse(url_str).map_err(|e| format!("Invalid URL '{url_str}': {e}"))?;
    let host = parsed.host_str().ok_or_else(|| "URL missing host".to_string())?;
    let port = parsed.port_or_known_default().unwrap_or(80);
    let addr = format!("{host}:{port}");

    let stream = tokio::net::TcpStream::connect(&addr)
        .await
        .map_err(|e| format!("Failed to connect to {addr}: {e}"))?;
    let io = TokioIo::new(stream);

    let (mut sender, conn) = hyper::client::conn::http1::handshake(io)
        .await
        .map_err(|e| format!("HTTP handshake failed: {e}"))?;
    tokio::spawn(async move {
        let _ = conn.await;
    });

    let mut path_and_query = parsed.path().to_string();
    if let Some(q) = parsed.query() {
        path_and_query.push('?');
        path_and_query.push_str(q);
    }

    let method = hyper::Method::from_bytes(method_str.as_bytes()).unwrap_or(hyper::Method::GET);
    let host_header = if let Some(p) = parsed.port() {
        format!("{host}:{p}")
    } else {
        host.to_string()
    };

    let mut builder = Request::builder()
        .method(method)
        .uri(path_and_query)
        .header(hyper::header::HOST, host_header);

    let req_headers: Vec<(String, String)> = serde_json::from_str(headers_json).unwrap_or_default();
    for (k, v) in req_headers {
        builder = builder.header(k, v);
    }

    let req = builder
        .body(Full::new(Bytes::from(body_bytes)))
        .map_err(|e| e.to_string())?;

    let resp = sender.send_request(req).await.map_err(|e| e.to_string())?;
    let status = resp.status().as_u16();
    let status_text = resp.status().canonical_reason().unwrap_or("").to_string();
    let resp_headers: Vec<(String, String)> = resp
        .headers()
        .iter()
        .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or("").to_string()))
        .collect();
    let resp_headers_json = serde_json::to_string(&resp_headers).unwrap_or_else(|_| "[]".into());
    let resp_body = resp
        .into_body()
        .collect()
        .await
        .map_err(|e| e.to_string())?
        .to_bytes()
        .to_vec();

    Ok((status, status_text, resp_headers_json, resp_body))
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
