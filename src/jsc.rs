//! Minimal hand-written bindings to the JavaScriptCore C API, plus a few safe
//! helpers. Linked statically against Bun's JSC build (see `build.rs`).

#![allow(non_camel_case_types, non_upper_case_globals, dead_code)]

use std::ffi::c_void;
use std::ptr;

#[repr(C)]
pub struct OpaqueJSContextGroup(c_void);
#[repr(C)]
pub struct OpaqueJSContext(c_void);
#[repr(C)]
pub struct OpaqueJSString(c_void);
#[repr(C)]
pub struct OpaqueJSClass(c_void);
#[repr(C)]
pub struct OpaqueJSValue(c_void);

pub type JSContextGroupRef = *const OpaqueJSContextGroup;
pub type JSContextRef = *const OpaqueJSContext;
pub type JSGlobalContextRef = *mut OpaqueJSContext;
pub type JSStringRef = *mut OpaqueJSString;
pub type JSClassRef = *mut OpaqueJSClass;
pub type JSValueRef = *const OpaqueJSValue;
pub type JSObjectRef = *mut OpaqueJSValue;
pub type JSChar = u16;
pub type JSPropertyAttributes = u32;
pub type JSTypedArrayType = u32;

pub const kJSPropertyAttributeNone: JSPropertyAttributes = 0;
pub const kJSPropertyAttributeReadOnly: JSPropertyAttributes = 1 << 1;
pub const kJSPropertyAttributeDontEnum: JSPropertyAttributes = 1 << 2;
pub const kJSPropertyAttributeDontDelete: JSPropertyAttributes = 1 << 3;

pub const kJSTypedArrayTypeUint8Array: JSTypedArrayType = 3;
pub const kJSTypedArrayTypeNone: JSTypedArrayType = 10;

pub type JSObjectCallAsFunctionCallback = Option<
    unsafe extern "C" fn(
        ctx: JSContextRef,
        function: JSObjectRef,
        this_object: JSObjectRef,
        argument_count: usize,
        arguments: *const JSValueRef,
        exception: *mut JSValueRef,
    ) -> JSValueRef,
>;

unsafe extern "C" {
    // Contexts
    pub fn JSGlobalContextCreate(global_object_class: JSClassRef) -> JSGlobalContextRef;
    pub fn JSGlobalContextRelease(ctx: JSGlobalContextRef);
    pub fn JSContextGetGlobalObject(ctx: JSContextRef) -> JSObjectRef;
    pub fn JSGlobalContextSetUnhandledRejectionCallback(
        ctx: JSGlobalContextRef,
        function: JSObjectRef,
        exception: *mut JSValueRef,
    );

    // Script evaluation
    pub fn JSEvaluateScript(
        ctx: JSContextRef,
        script: JSStringRef,
        this_object: JSObjectRef,
        source_url: JSStringRef,
        starting_line_number: i32,
        exception: *mut JSValueRef,
    ) -> JSValueRef;
    pub fn JSGarbageCollect(ctx: JSContextRef);

    // Strings
    pub fn JSStringCreateWithCharacters(chars: *const JSChar, num_chars: usize) -> JSStringRef;
    pub fn JSStringRelease(string: JSStringRef);
    pub fn JSStringGetLength(string: JSStringRef) -> usize;
    pub fn JSStringGetCharactersPtr(string: JSStringRef) -> *const JSChar;

    // Values
    pub fn JSValueMakeUndefined(ctx: JSContextRef) -> JSValueRef;
    pub fn JSValueMakeBoolean(ctx: JSContextRef, boolean: bool) -> JSValueRef;
    pub fn JSValueMakeNumber(ctx: JSContextRef, number: f64) -> JSValueRef;
    pub fn JSValueMakeString(ctx: JSContextRef, string: JSStringRef) -> JSValueRef;
    pub fn JSValueIsUndefined(ctx: JSContextRef, value: JSValueRef) -> bool;
    pub fn JSValueIsObject(ctx: JSContextRef, value: JSValueRef) -> bool;
    pub fn JSValueToBoolean(ctx: JSContextRef, value: JSValueRef) -> bool;
    pub fn JSValueToNumber(ctx: JSContextRef, value: JSValueRef, exception: *mut JSValueRef) -> f64;
    pub fn JSValueToStringCopy(ctx: JSContextRef, value: JSValueRef, exception: *mut JSValueRef) -> JSStringRef;
    pub fn JSValueToObject(ctx: JSContextRef, value: JSValueRef, exception: *mut JSValueRef) -> JSObjectRef;
    pub fn JSValueProtect(ctx: JSContextRef, value: JSValueRef);
    pub fn JSValueUnprotect(ctx: JSContextRef, value: JSValueRef);

    // Objects
    pub fn JSObjectMake(ctx: JSContextRef, js_class: JSClassRef, data: *mut c_void) -> JSObjectRef;
    pub fn JSObjectMakeFunctionWithCallback(
        ctx: JSContextRef,
        name: JSStringRef,
        call_as_function: JSObjectCallAsFunctionCallback,
    ) -> JSObjectRef;
    pub fn JSObjectMakeError(
        ctx: JSContextRef,
        argument_count: usize,
        arguments: *const JSValueRef,
        exception: *mut JSValueRef,
    ) -> JSObjectRef;
    pub fn JSObjectMakeDeferredPromise(
        ctx: JSContextRef,
        resolve: *mut JSObjectRef,
        reject: *mut JSObjectRef,
        exception: *mut JSValueRef,
    ) -> JSObjectRef;
    pub fn JSObjectGetProperty(
        ctx: JSContextRef,
        object: JSObjectRef,
        property_name: JSStringRef,
        exception: *mut JSValueRef,
    ) -> JSValueRef;
    pub fn JSObjectSetProperty(
        ctx: JSContextRef,
        object: JSObjectRef,
        property_name: JSStringRef,
        value: JSValueRef,
        attributes: JSPropertyAttributes,
        exception: *mut JSValueRef,
    );
    pub fn JSObjectIsFunction(ctx: JSContextRef, object: JSObjectRef) -> bool;
    pub fn JSObjectCallAsFunction(
        ctx: JSContextRef,
        object: JSObjectRef,
        this_object: JSObjectRef,
        argument_count: usize,
        arguments: *const JSValueRef,
        exception: *mut JSValueRef,
    ) -> JSValueRef;

    // Typed arrays
    pub fn JSValueGetTypedArrayType(
        ctx: JSContextRef,
        value: JSValueRef,
        exception: *mut JSValueRef,
    ) -> JSTypedArrayType;
    pub fn JSObjectMakeTypedArray(
        ctx: JSContextRef,
        array_type: JSTypedArrayType,
        length: usize,
        exception: *mut JSValueRef,
    ) -> JSObjectRef;
    pub fn JSObjectGetTypedArrayBytesPtr(
        ctx: JSContextRef,
        object: JSObjectRef,
        exception: *mut JSValueRef,
    ) -> *mut u8;
    pub fn JSObjectGetTypedArrayByteLength(
        ctx: JSContextRef,
        object: JSObjectRef,
        exception: *mut JSValueRef,
    ) -> usize;
}

/// Owned `JSStringRef`, released on drop.
pub struct JsString(JSStringRef);

impl JsString {
    pub fn new(s: &str) -> Self {
        let utf16: Vec<u16> = s.encode_utf16().collect();
        // SAFETY: pointer/length pair describes a valid UTF-16 buffer; JSC copies it.
        JsString(unsafe { JSStringCreateWithCharacters(utf16.as_ptr(), utf16.len()) })
    }

    pub fn as_raw(&self) -> JSStringRef {
        self.0
    }

    /// Takes ownership of a `JSStringRef` returned by a `*Copy`/`*Create` API.
    ///
    /// # Safety
    /// `raw` must be a valid, owned (+1) `JSStringRef`.
    pub unsafe fn from_owned(raw: JSStringRef) -> Self {
        JsString(raw)
    }

    pub fn to_rust(&self) -> String {
        // SAFETY: self.0 is a valid JSStringRef for the lifetime of self.
        unsafe {
            let len = JSStringGetLength(self.0);
            let chars = JSStringGetCharactersPtr(self.0);
            if len == 0 || chars.is_null() {
                return String::new();
            }
            String::from_utf16_lossy(std::slice::from_raw_parts(chars, len))
        }
    }
}

impl Drop for JsString {
    fn drop(&mut self) {
        // SAFETY: we own exactly one reference.
        unsafe { JSStringRelease(self.0) }
    }
}

/// Converts any JS value to a Rust string via JS `String(value)` semantics.
///
/// # Safety
/// `ctx` and `value` must be valid.
pub unsafe fn value_to_string(ctx: JSContextRef, value: JSValueRef) -> String {
    unsafe {
        let mut exc: JSValueRef = ptr::null();
        let s = JSValueToStringCopy(ctx, value, &mut exc);
        if s.is_null() {
            return "<unprintable value>".to_string();
        }
        JsString::from_owned(s).to_rust()
    }
}

/// Formats an exception value, preferring its `stack` when available.
///
/// # Safety
/// `ctx` and `exception` must be valid.
pub unsafe fn format_exception(ctx: JSContextRef, exception: JSValueRef) -> String {
    unsafe {
        let message = value_to_string(ctx, exception);
        if JSValueIsObject(ctx, exception) {
            let obj = JSValueToObject(ctx, exception, ptr::null_mut());
            let stack_name = JsString::new("stack");
            let stack = JSObjectGetProperty(ctx, obj, stack_name.as_raw(), ptr::null_mut());
            if !stack.is_null() && !JSValueIsUndefined(ctx, stack) {
                let stack = value_to_string(ctx, stack);
                if !stack.is_empty() {
                    return format!("{message}\n{stack}");
                }
            }
        }
        message
    }
}

/// # Safety
/// `ctx` must be valid.
pub unsafe fn make_string(ctx: JSContextRef, s: &str) -> JSValueRef {
    let js = JsString::new(s);
    unsafe { JSValueMakeString(ctx, js.as_raw()) }
}

/// Creates a JS `Error` with the given message.
///
/// # Safety
/// `ctx` must be valid.
pub unsafe fn make_error(ctx: JSContextRef, message: &str) -> JSValueRef {
    unsafe {
        let msg = make_string(ctx, message);
        JSObjectMakeError(ctx, 1, &msg, ptr::null_mut())
    }
}

/// # Safety
/// `ctx` and `object` must be valid.
pub unsafe fn set_property(ctx: JSContextRef, object: JSObjectRef, name: &str, value: JSValueRef) {
    let name = JsString::new(name);
    unsafe {
        JSObjectSetProperty(ctx, object, name.as_raw(), value, kJSPropertyAttributeNone, ptr::null_mut())
    }
}

/// # Safety
/// `ctx` and `object` must be valid.
pub unsafe fn set_function(
    ctx: JSContextRef,
    object: JSObjectRef,
    name: &str,
    callback: JSObjectCallAsFunctionCallback,
) {
    unsafe {
        let js_name = JsString::new(name);
        let func = JSObjectMakeFunctionWithCallback(ctx, js_name.as_raw(), callback);
        set_property(ctx, object, name, func);
    }
}

/// Creates a JS `Uint8Array` copying `bytes`.
///
/// # Safety
/// `ctx` must be valid.
pub unsafe fn make_uint8_array(ctx: JSContextRef, bytes: &[u8]) -> JSObjectRef {
    unsafe {
        let arr = JSObjectMakeTypedArray(ctx, kJSTypedArrayTypeUint8Array, bytes.len(), ptr::null_mut());
        if !arr.is_null() && !bytes.is_empty() {
            let dst = JSObjectGetTypedArrayBytesPtr(ctx, arr, ptr::null_mut());
            if !dst.is_null() {
                ptr::copy_nonoverlapping(bytes.as_ptr(), dst, bytes.len());
            }
        }
        arr
    }
}

/// Copies the bytes of a JS TypedArray (e.g. `Uint8Array`) into a `Vec<u8>`.
///
/// # Safety
/// `ctx` and `value` must be valid.
pub unsafe fn read_uint8_array(ctx: JSContextRef, value: JSValueRef) -> Option<Vec<u8>> {
    unsafe {
        if value.is_null() || !JSValueIsObject(ctx, value) {
            return None;
        }
        let ty = JSValueGetTypedArrayType(ctx, value, ptr::null_mut());
        if ty == kJSTypedArrayTypeNone {
            return None;
        }
        let obj = JSValueToObject(ctx, value, ptr::null_mut());
        if obj.is_null() {
            return None;
        }
        let len = JSObjectGetTypedArrayByteLength(ctx, obj, ptr::null_mut());
        if len == 0 {
            return Some(Vec::new());
        }
        let ptr = JSObjectGetTypedArrayBytesPtr(ctx, obj, ptr::null_mut());
        if ptr.is_null() {
            return Some(Vec::new());
        }
        Some(std::slice::from_raw_parts(ptr, len).to_vec())
    }
}
