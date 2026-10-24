// src/bootstrap.js
// Evaluated by the runtime as a function expression and invoked with the
// native ops object (JavaScriptCore host functions defined in runtime.rs).
(function (ops) {
const {
    op_custom_print,
    op_sleep,
    op_fs_read_file_sync,
    op_fs_write_file_sync,
    op_fs_read_file_async,
    op_fs_write_file_async,
    op_load_module,
    op_utf8_encode,
    op_utf8_decode,
    op_random_uuid,
    op_set_event_handlers,
    op_http_serve,
    op_http_stop,
    op_http_respond,
    op_ws_upgrade,
    op_ws_connect,
    op_ws_send_text,
    op_ws_send_binary,
    op_ws_close,
    op_fetch
} = ops;

// ---------------------------------------------------------------------------
// 1. Console, Timers & Filesystem
// ---------------------------------------------------------------------------

function formatConsoleArgs(args) {
    return args.map(arg => {
        if (arg instanceof Error) {
            return arg.stack || `${arg.name}: ${arg.message}`;
        }
        if (typeof arg === 'object' && arg !== null) {
            try {
                return JSON.stringify(arg);
            } catch (_) {
                return String(arg);
            }
        }
        return String(arg);
    }).join(' ');
}

globalThis.console = {
    log(...args) { op_custom_print(formatConsoleArgs(args), false); },
    info(...args) { op_custom_print(formatConsoleArgs(args), false); },
    debug(...args) { op_custom_print(formatConsoleArgs(args), false); },
    warn(...args) { op_custom_print(formatConsoleArgs(args), true); },
    error(...args) { op_custom_print(formatConsoleArgs(args), true); }
};

let nextTimerId = 1;
const activeTimers = new Set();

globalThis.setTimeout = (callback, delay, ...args) => {
    const id = nextTimerId++;
    activeTimers.add(id);
    op_sleep(delay || 0).then(() => {
        if (activeTimers.delete(id)) {
            callback(...args);
        }
    });
    return id;
};

globalThis.clearTimeout = (id) => {
    activeTimers.delete(id);
};

globalThis.setInterval = (callback, delay, ...args) => {
    const id = nextTimerId++;
    activeTimers.add(id);
    const tick = () => {
        op_sleep(delay || 0).then(() => {
            if (activeTimers.has(id)) {
                callback(...args);
                tick();
            }
        });
    };
    tick();
    return id;
};

globalThis.clearInterval = (id) => {
    activeTimers.delete(id);
};

if (typeof globalThis.queueMicrotask !== 'function') {
    globalThis.queueMicrotask = (cb) => Promise.resolve().then(cb);
}

globalThis.fs = {
    readFileSync(path) { return op_fs_read_file_sync(path); },
    writeFileSync(path, contents) { op_fs_write_file_sync(path, contents); },
    readFile(path) { return op_fs_read_file_async(path); },
    writeFile(path, contents) { return op_fs_write_file_async(path, contents); }
};

// ---------------------------------------------------------------------------
// 2. CommonJS + ESM Module Loader
// ---------------------------------------------------------------------------

const moduleCache = new Map();

function dirnameOf(filePath) {
    const normalized = String(filePath).replace(/\\/g, '/');
    const idx = normalized.lastIndexOf('/');
    return idx >= 0 ? filePath.slice(0, idx) : '.';
}

globalThis.__toESM = (mod) => {
    if (mod && mod.__esModule) return mod;
    const out = Object.create(null);
    Object.defineProperty(out, '__esModule', { value: true });
    if (mod != null && (typeof mod === 'object' || typeof mod === 'function')) {
        for (const key of Object.keys(mod)) {
            Object.defineProperty(out, key, {
                enumerable: true,
                configurable: true,
                get: () => mod[key]
            });
        }
    }
    out.default = mod && mod.__esModule ? mod.default : mod;
    return out;
};

globalThis.__exportStar = (from, to) => {
    if (!from || (typeof from !== 'object' && typeof from !== 'function')) return to;
    for (const key of Object.keys(from)) {
        if (key !== 'default' && key !== '__esModule' && !Object.prototype.hasOwnProperty.call(to, key)) {
            Object.defineProperty(to, key, {
                enumerable: true,
                configurable: true,
                get: () => from[key]
            });
        }
    }
    return to;
};

const builtinModules = {
    'fs': () => ({
        ...globalThis.fs,
        promises: builtinModules['fs/promises']()
    }),
    'fs/promises': () => ({
        readFile: (path) => op_fs_read_file_async(path),
        writeFile: (path, contents) => op_fs_write_file_async(path, contents),
        stat: async () => ({ isDirectory: () => false, isFile: () => true }),
        mkdir: async () => {}
    }),
    'path': () => ({
        sep: '/',
        join: (...parts) => parts.filter(Boolean).join('/').replace(/\/+/g, '/'),
        resolve: (...parts) => parts.filter(Boolean).join('/').replace(/\/+/g, '/'),
        dirname: (p) => dirnameOf(p),
        basename: (p) => String(p).replace(/\\/g, '/').split('/').pop() || '',
        extname: (p) => {
            const b = String(p).replace(/\\/g, '/').split('/').pop() || '';
            const i = b.lastIndexOf('.');
            return i > 0 ? b.slice(i) : '';
        }
    }),
    'crypto': () => globalThis.crypto,
    'url': () => ({ URL: globalThis.URL, URLSearchParams: globalThis.URLSearchParams }),
    'events': () => ({ EventEmitter: class {} }),
    'util': () => ({ TextEncoder: globalThis.TextEncoder, TextDecoder: globalThis.TextDecoder })
};

function createRequire(referrer) {
    function req(specifier) {
        const specStr = String(specifier);
        const bareBuiltin = specStr.startsWith('node:') ? specStr.slice(5) : specStr;
        if (Object.prototype.hasOwnProperty.call(builtinModules, bareBuiltin)) {
            return builtinModules[bareBuiltin]();
        }
        const ref = referrer || globalThis.__entryFilename || 'repl';
        const loaded = op_load_module(specStr, String(ref));
        if (moduleCache.has(loaded.id)) {
            return moduleCache.get(loaded.id).exports;
        }
        const mod = { id: loaded.id, filename: loaded.id, loaded: false, exports: {} };
        moduleCache.set(loaded.id, mod);
        if (loaded.isJson) {
            mod.exports = JSON.parse(loaded.code);
            mod.loaded = true;
            return mod.exports;
        }
        const subRequire = createRequire(loaded.id);
        const dirname = dirnameOf(loaded.id);
        const wrapper = new Function(
            'exports',
            'require',
            'module',
            '__filename',
            '__dirname',
            `${loaded.code}\n//# sourceURL=${loaded.id}`
        );
        wrapper(mod.exports, subRequire, mod, loaded.id, dirname);
        mod.loaded = true;
        return mod.exports;
    }
    req.cache = moduleCache;
    return req;
}

globalThis.exports = {};
globalThis.module = { exports: globalThis.exports };
globalThis.require = createRequire(null);

// ---------------------------------------------------------------------------
// 3. TextEncoder, TextDecoder & Crypto
// ---------------------------------------------------------------------------

class TextEncoder {
    get encoding() { return 'utf-8'; }
    encode(input = '') {
        return op_utf8_encode(String(input));
    }
    encodeInto(source, destination) {
        const encoded = this.encode(source);
        const written = Math.min(encoded.byteLength, destination.byteLength);
        destination.set(encoded.subarray(0, written));
        return { read: source.length, written };
    }
}

class TextDecoder {
    constructor(label = 'utf-8', options = {}) {
        this.encoding = String(label).toLowerCase();
        this.fatal = Boolean(options.fatal);
        this.ignoreBOM = Boolean(options.ignoreBOM);
    }
    decode(input) {
        if (!input) return '';
        let u8;
        if (input instanceof Uint8Array) {
            u8 = input;
        } else if (ArrayBuffer.isView(input)) {
            u8 = new Uint8Array(input.buffer, input.byteOffset, input.byteLength);
        } else if (input instanceof ArrayBuffer) {
            u8 = new Uint8Array(input);
        } else {
            return '';
        }
        return op_utf8_decode(u8);
    }
}

globalThis.TextEncoder = TextEncoder;
globalThis.TextDecoder = TextDecoder;

globalThis.crypto = {
    randomUUID() {
        return op_random_uuid();
    },
    getRandomValues(typedArray) {
        const u8 = new Uint8Array(typedArray.buffer, typedArray.byteOffset, typedArray.byteLength);
        for (let i = 0; i < u8.length; i++) {
            u8[i] = (Math.random() * 256) | 0;
        }
        return typedArray;
    }
};

// ---------------------------------------------------------------------------
// 4. Events & AbortController
// ---------------------------------------------------------------------------

class Event {
    constructor(type, eventInitDict = {}) {
        this.type = String(type);
        this.bubbles = Boolean(eventInitDict.bubbles);
        this.cancelable = Boolean(eventInitDict.cancelable);
        this.composed = Boolean(eventInitDict.composed);
        this.defaultPrevented = false;
        this.target = null;
        this.currentTarget = null;
        this.timeStamp = Date.now();
    }
    preventDefault() {
        if (this.cancelable) this.defaultPrevented = true;
    }
    stopPropagation() {}
    stopImmediatePropagation() {}
}

class CustomEvent extends Event {
    constructor(type, eventInitDict = {}) {
        super(type, eventInitDict);
        this.detail = eventInitDict.detail ?? null;
    }
}

class MessageEvent extends Event {
    constructor(type, eventInitDict = {}) {
        super(type, eventInitDict);
        this.data = eventInitDict.data ?? null;
        this.origin = eventInitDict.origin ?? '';
        this.lastEventId = eventInitDict.lastEventId ?? '';
        this.source = eventInitDict.source ?? null;
        this.ports = eventInitDict.ports ?? [];
    }
}

class CloseEvent extends Event {
    constructor(type, eventInitDict = {}) {
        super(type, eventInitDict);
        this.wasClean = Boolean(eventInitDict.wasClean ?? true);
        this.code = eventInitDict.code ?? 1000;
        this.reason = eventInitDict.reason ?? '';
    }
}

class ErrorEvent extends Event {
    constructor(type, eventInitDict = {}) {
        super(type, eventInitDict);
        this.message = eventInitDict.message ?? '';
        this.error = eventInitDict.error ?? null;
    }
}

class EventTarget {
    constructor() {
        this._listeners = new Map();
    }
    addEventListener(type, listener) {
        if (typeof listener !== 'function') return;
        let list = this._listeners.get(type);
        if (!list) {
            list = new Set();
            this._listeners.set(type, list);
        }
        list.add(listener);
    }
    removeEventListener(type, listener) {
        const list = this._listeners.get(type);
        if (list) list.delete(listener);
    }
    dispatchEvent(event) {
        event.target = this;
        event.currentTarget = this;
        const prop = `on${event.type}`;
        if (typeof this[prop] === 'function') {
            this[prop].call(this, event);
        }
        const list = this._listeners.get(event.type);
        if (list) {
            for (const fn of [...list]) {
                fn.call(this, event);
            }
        }
        return !event.defaultPrevented;
    }
}

class AbortSignal extends EventTarget {
    constructor() {
        super();
        this.aborted = false;
        this.reason = undefined;
        this.onabort = null;
    }
    throwIfAborted() {
        if (this.aborted) throw this.reason;
    }
    static abort(reason) {
        const sig = new AbortSignal();
        sig.aborted = true;
        sig.reason = reason !== undefined ? reason : new Error('This operation was aborted');
        return sig;
    }
}

class AbortController {
    constructor() {
        this.signal = new AbortSignal();
    }
    abort(reason) {
        if (this.signal.aborted) return;
        this.signal.aborted = true;
        this.signal.reason = reason !== undefined ? reason : new Error('This operation was aborted');
        this.signal.dispatchEvent(new Event('abort'));
    }
}

globalThis.Event = Event;
globalThis.CustomEvent = CustomEvent;
globalThis.MessageEvent = MessageEvent;
globalThis.CloseEvent = CloseEvent;
globalThis.ErrorEvent = ErrorEvent;
globalThis.EventTarget = EventTarget;
globalThis.AbortSignal = AbortSignal;
globalThis.AbortController = AbortController;

// ---------------------------------------------------------------------------
// 5. URL & URLSearchParams
// ---------------------------------------------------------------------------

class URLSearchParams {
    constructor(init = '') {
        this._entries = [];
        if (typeof init === 'string') {
            const q = init.startsWith('?') ? init.slice(1) : init;
            if (q) {
                for (const part of q.split('&')) {
                    if (!part) continue;
                    const eq = part.indexOf('=');
                    if (eq === -1) {
                        this.append(decodeURIComponent(part.replace(/\+/g, ' ')), '');
                    } else {
                        const k = decodeURIComponent(part.slice(0, eq).replace(/\+/g, ' '));
                        const v = decodeURIComponent(part.slice(eq + 1).replace(/\+/g, ' '));
                        this.append(k, v);
                    }
                }
            }
        } else if (Array.isArray(init)) {
            for (const [k, v] of init) this.append(k, v);
        } else if (init instanceof URLSearchParams) {
            for (const [k, v] of init._entries) this.append(k, v);
        } else if (init && typeof init === 'object') {
            for (const k of Object.keys(init)) this.append(k, init[k]);
        }
    }
    append(name, value) {
        this._entries.push([String(name), String(value)]);
    }
    delete(name) {
        const k = String(name);
        this._entries = this._entries.filter(([ek]) => ek !== k);
    }
    get(name) {
        const k = String(name);
        const found = this._entries.find(([ek]) => ek === k);
        return found ? found[1] : null;
    }
    getAll(name) {
        const k = String(name);
        return this._entries.filter(([ek]) => ek === k).map(([, v]) => v);
    }
    has(name) {
        const k = String(name);
        return this._entries.some(([ek]) => ek === k);
    }
    set(name, value) {
        const k = String(name);
        const v = String(value);
        let replaced = false;
        this._entries = this._entries.filter(([ek], idx) => {
            if (ek === k) {
                if (!replaced) {
                    this._entries[idx] = [k, v];
                    replaced = true;
                    return true;
                }
                return false;
            }
            return true;
        });
        if (!replaced) this._entries.push([k, v]);
    }
    forEach(cb, thisArg) {
        for (const [k, v] of this._entries) cb.call(thisArg, v, k, this);
    }
    *entries() {
        for (const pair of this._entries) yield pair;
    }
    *keys() {
        for (const [k] of this._entries) yield k;
    }
    *values() {
        for (const [, v] of this._entries) yield v;
    }
    [Symbol.iterator]() {
        return this.entries();
    }
    toString() {
        return this._entries
            .map(([k, v]) => `${encodeURIComponent(k)}=${encodeURIComponent(v)}`)
            .join('&');
    }
}

class URL {
    constructor(url, base) {
        let input = String(url);
        if (base !== undefined) {
            const b = base instanceof URL ? base.href : String(base);
            if (!/^[a-zA-Z][a-zA-Z0-9+.-]*:\/\//.test(input)) {
                const baseParsed = new URL(b);
                if (input.startsWith('/')) {
                    input = `${baseParsed.origin}${input}`;
                } else {
                    const dir = baseParsed.pathname.replace(/\/[^/]*$/, '/');
                    input = `${baseParsed.origin}${dir}${input}`;
                }
            }
        }
        const match = input.match(/^([a-zA-Z][a-zA-Z0-9+.-]*:)\/\/([^/?#]*)(\/[^?#]*)?(\?[^#]*)?(#.*)?$/);
        if (!match) {
            throw new TypeError(`Invalid URL: ${input}`);
        }
        this.protocol = match[1].toLowerCase();
        const authority = match[2];
        const atIdx = authority.lastIndexOf('@');
        let hostPart = authority;
        if (atIdx !== -1) {
            const userinfo = authority.slice(0, atIdx);
            const colon = userinfo.indexOf(':');
            this.username = colon === -1 ? userinfo : userinfo.slice(0, colon);
            this.password = colon === -1 ? '' : userinfo.slice(colon + 1);
            hostPart = authority.slice(atIdx + 1);
        } else {
            this.username = '';
            this.password = '';
        }
        const portMatch = hostPart.match(/^(.*):(\d+)$/);
        if (portMatch) {
            this.hostname = portMatch[1];
            this.port = portMatch[2];
        } else {
            this.hostname = hostPart;
            this.port = '';
        }
        if (
            (this.protocol === 'http:' && this.port === '80') ||
            (this.protocol === 'https:' && this.port === '443') ||
            (this.protocol === 'ws:' && this.port === '80') ||
            (this.protocol === 'wss:' && this.port === '443')
        ) {
            this.port = '';
        }
        this.host = this.port ? `${this.hostname}:${this.port}` : this.hostname;
        this.origin = `${this.protocol}//${this.host}`;
        this.pathname = match[3] || '/';
        this._searchParams = new URLSearchParams(match[4] || '');
        this.hash = match[5] || '';
    }
    get searchParams() {
        return this._searchParams;
    }
    get search() {
        const s = this._searchParams.toString();
        return s ? `?${s}` : '';
    }
    set search(val) {
        this._searchParams = new URLSearchParams(val);
    }
    get href() {
        return `${this.origin}${this.pathname}${this.search}${this.hash}`;
    }
    toString() {
        return this.href;
    }
    toJSON() {
        return this.href;
    }
}

globalThis.URLSearchParams = URLSearchParams;
globalThis.URL = URL;

// ---------------------------------------------------------------------------
// 6. Headers, Blob, FormData, Request, Response & fetch
// ---------------------------------------------------------------------------

class Headers {
    constructor(init) {
        this._map = new Map();
        this._cookies = [];
        if (init instanceof Headers) {
            for (const [k, v] of init._map.entries()) this._map.set(k, v);
            this._cookies = [...init._cookies];
        } else if (Array.isArray(init)) {
            for (const [k, v] of init) this.append(k, v);
        } else if (init && typeof init === 'object') {
            for (const k of Object.keys(init)) {
                const v = init[k];
                if (v !== undefined) this.set(k, v);
            }
        }
    }
    append(name, value) {
        const k = String(name).toLowerCase();
        const v = String(value);
        if (k === 'set-cookie') {
            this._cookies.push(v);
            return;
        }
        const existing = this._map.get(k);
        this._map.set(k, existing !== undefined ? `${existing}, ${v}` : v);
    }
    set(name, value) {
        const k = String(name).toLowerCase();
        const v = String(value);
        if (k === 'set-cookie') {
            this._cookies = [v];
            return;
        }
        this._map.set(k, v);
    }
    get(name) {
        const k = String(name).toLowerCase();
        if (k === 'set-cookie') {
            return this._cookies.length ? this._cookies.join(', ') : null;
        }
        return this._map.has(k) ? this._map.get(k) : null;
    }
    has(name) {
        const k = String(name).toLowerCase();
        if (k === 'set-cookie') return this._cookies.length > 0;
        return this._map.has(k);
    }
    delete(name) {
        const k = String(name).toLowerCase();
        if (k === 'set-cookie') {
            this._cookies = [];
            return;
        }
        this._map.delete(k);
    }
    getSetCookie() {
        return [...this._cookies];
    }
    forEach(callback, thisArg) {
        for (const [k, v] of this.entries()) {
            callback.call(thisArg, v, k, this);
        }
    }
    *entries() {
        for (const [k, v] of this._map.entries()) yield [k, v];
        for (const c of this._cookies) yield ['set-cookie', c];
    }
    *keys() {
        for (const [k] of this.entries()) yield k;
    }
    *values() {
        for (const [, v] of this.entries()) yield v;
    }
    [Symbol.iterator]() {
        return this.entries();
    }
}

const sharedEncoder = new TextEncoder();
const sharedDecoder = new TextDecoder();

function normalizeBodyToBytes(body) {
    if (body === null || body === undefined) {
        return { bytes: new Uint8Array(0), contentType: null };
    }
    if (typeof body === 'string') {
        return { bytes: sharedEncoder.encode(body), contentType: 'text/plain;charset=UTF-8' };
    }
    if (body instanceof Uint8Array) {
        return { bytes: body, contentType: null };
    }
    if (ArrayBuffer.isView(body)) {
        return { bytes: new Uint8Array(body.buffer, body.byteOffset, body.byteLength), contentType: null };
    }
    if (body instanceof ArrayBuffer) {
        return { bytes: new Uint8Array(body), contentType: null };
    }
    if (body instanceof URLSearchParams) {
        return {
            bytes: sharedEncoder.encode(body.toString()),
            contentType: 'application/x-www-form-urlencoded;charset=UTF-8'
        };
    }
    if (typeof Blob !== 'undefined' && body instanceof Blob) {
        return { bytes: body._bytes, contentType: body.type || null };
    }
    return { bytes: sharedEncoder.encode(String(body)), contentType: 'text/plain;charset=UTF-8' };
}

class Blob {
    constructor(blobParts = [], options = {}) {
        const chunks = [];
        let total = 0;
        for (const part of blobParts) {
            const { bytes } = normalizeBodyToBytes(part);
            chunks.push(bytes);
            total += bytes.byteLength;
        }
        const merged = new Uint8Array(total);
        let offset = 0;
        for (const c of chunks) {
            merged.set(c, offset);
            offset += c.byteLength;
        }
        this._bytes = merged;
        this.size = merged.byteLength;
        this.type = options.type ? String(options.type).toLowerCase() : '';
    }
    async text() {
        return sharedDecoder.decode(this._bytes);
    }
    async arrayBuffer() {
        return this._bytes.buffer.slice(this._bytes.byteOffset, this._bytes.byteOffset + this._bytes.byteLength);
    }
    async bytes() {
        return new Uint8Array(this._bytes);
    }
    slice(start = 0, end = this.size, contentType = '') {
        const sub = this._bytes.subarray(start, end);
        return new Blob([sub], { type: contentType });
    }
}

class FormData {
    constructor() {
        this._entries = [];
    }
    append(name, value) {
        this._entries.push([String(name), value]);
    }
    set(name, value) {
        const k = String(name);
        this._entries = this._entries.filter(([ek]) => ek !== k);
        this._entries.push([k, value]);
    }
    get(name) {
        const k = String(name);
        const found = this._entries.find(([ek]) => ek === k);
        return found ? found[1] : null;
    }
    getAll(name) {
        const k = String(name);
        return this._entries.filter(([ek]) => ek === k).map(([, v]) => v);
    }
    has(name) {
        const k = String(name);
        return this._entries.some(([ek]) => ek === k);
    }
    delete(name) {
        const k = String(name);
        this._entries = this._entries.filter(([ek]) => ek !== k);
    }
    forEach(cb, thisArg) {
        for (const [k, v] of this._entries) cb.call(thisArg, v, k, this);
    }
    *entries() {
        for (const pair of this._entries) yield pair;
    }
    *keys() {
        for (const [k] of this._entries) yield k;
    }
    *values() {
        for (const [, v] of this._entries) yield v;
    }
    [Symbol.iterator]() {
        return this.entries();
    }
}

class BodyMixin {
    constructor(bodyInit) {
        this.bodyUsed = false;
        if (bodyInit === null || bodyInit === undefined) {
            this._bodyBytes = null;
            this.body = null;
            this._defaultContentType = null;
        } else {
            const { bytes, contentType } = normalizeBodyToBytes(bodyInit);
            this._bodyBytes = bytes;
            this.body = bytes;
            this._defaultContentType = contentType;
        }
    }
    _consumeBytes() {
        if (this.bodyUsed) {
            return Promise.reject(new TypeError('Body has already been consumed.'));
        }
        this.bodyUsed = true;
        return Promise.resolve(this._bodyBytes || new Uint8Array(0));
    }
    async arrayBuffer() {
        const bytes = await this._consumeBytes();
        return bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength);
    }
    async bytes() {
        const bytes = await this._consumeBytes();
        return new Uint8Array(bytes);
    }
    async text() {
        const bytes = await this._consumeBytes();
        return sharedDecoder.decode(bytes);
    }
    async json() {
        const text = await this.text();
        return JSON.parse(text);
    }
    async blob() {
        const bytes = await this._consumeBytes();
        const type = this.headers?.get('content-type') || '';
        return new Blob([bytes], { type });
    }
    async formData() {
        const text = await this.text();
        const fd = new FormData();
        const params = new URLSearchParams(text);
        for (const [k, v] of params.entries()) fd.append(k, v);
        return fd;
    }
}

class Request extends BodyMixin {
    constructor(input, init = {}) {
        let url;
        let method = 'GET';
        let headersInit = undefined;
        let bodyInit = undefined;
        let signal = undefined;

        if (input instanceof Request) {
            url = input.url;
            method = input.method;
            headersInit = new Headers(input.headers);
            bodyInit = input._bodyBytes ? new Uint8Array(input._bodyBytes) : null;
            signal = input.signal;
        } else {
            url = String(input);
        }

        if (init.method !== undefined) method = String(init.method).toUpperCase();
        if (init.headers !== undefined) headersInit = init.headers;
        if (init.body !== undefined) bodyInit = init.body;
        if (init.signal !== undefined) signal = init.signal;

        super(bodyInit);
        this.url = url;
        this.method = method;
        this.headers = new Headers(headersInit);
        if (this._defaultContentType && !this.headers.has('content-type')) {
            this.headers.set('content-type', this._defaultContentType);
        }
        this.signal = signal || new AbortSignal();
        this.cache = init.cache || 'default';
        this.credentials = init.credentials || 'same-origin';
        this.integrity = init.integrity || '';
        this.keepalive = Boolean(init.keepalive);
        this.mode = init.mode || 'cors';
        this.redirect = init.redirect || 'follow';
        this.referrer = init.referrer || 'about:client';
        this.referrerPolicy = init.referrerPolicy || '';
        this._reqId = init._reqId || 0;
        this._serverId = init._serverId || 0;
    }
    clone() {
        if (this.bodyUsed) {
            throw new TypeError('Cannot clone a Request whose body has already been used.');
        }
        return new Request(this);
    }
}

class Response extends BodyMixin {
    constructor(body = null, init = {}) {
        super(body);
        const status = init.status !== undefined ? Number(init.status) : 200;
        this.status = status;
        this.statusText = init.statusText !== undefined ? String(init.statusText) : '';
        this.ok = status >= 200 && status < 300;
        this.headers = new Headers(init.headers);
        if (this._defaultContentType && !this.headers.has('content-type')) {
            this.headers.set('content-type', this._defaultContentType);
        }
        this.redirected = false;
        this.type = 'default';
        this.url = '';
    }
    clone() {
        if (this.bodyUsed) {
            throw new TypeError('Cannot clone a Response whose body has already been used.');
        }
        return new Response(this._bodyBytes ? new Uint8Array(this._bodyBytes) : null, {
            status: this.status,
            statusText: this.statusText,
            headers: new Headers(this.headers)
        });
    }
    static json(data, init = {}) {
        const headers = new Headers(init.headers);
        if (!headers.has('content-type')) {
            headers.set('content-type', 'application/json');
        }
        return new Response(JSON.stringify(data), {
            status: init.status ?? 200,
            statusText: init.statusText ?? '',
            headers
        });
    }
    static redirect(url, status = 302) {
        return new Response(null, {
            status,
            headers: { location: String(url) }
        });
    }
    static error() {
        const res = new Response(null, { status: 0, statusText: '' });
        res.type = 'error';
        res.ok = false;
        return res;
    }
}

globalThis.Headers = Headers;
globalThis.Blob = Blob;
globalThis.FormData = FormData;
globalThis.Request = Request;
globalThis.Response = Response;

globalThis.fetch = async (input, init = {}) => {
    const req = input instanceof Request ? (init && Object.keys(init).length ? new Request(input, init) : input) : new Request(input, init);
    const headersArr = [...req.headers.entries()];
    const bodyBytes = req._bodyBytes || new Uint8Array(0);
    const raw = await op_fetch(req.url, req.method, JSON.stringify(headersArr), bodyBytes);
    const respHeaders = JSON.parse(raw.headersJson);
    return new Response(raw.body, {
        status: raw.status,
        statusText: raw.statusText,
        headers: respHeaders
    });
};

// ---------------------------------------------------------------------------
// 7. WebSockets (Client `WebSocket`, `Bun.serve` & `Deno.serve` WebSockets)
// ---------------------------------------------------------------------------

const activeServers = new Map();
const wsConnections = new Map();

class ServerWebSocket {
    constructor(wsId, data, protocol = '') {
        this._wsId = wsId;
        this.data = data;
        this.readyState = 1; // OPEN
        this.remoteAddress = '127.0.0.1';
        this.protocol = protocol;
        this.binaryType = 'arraybuffer';
    }
    send(data) {
        if (this.readyState !== 1) return 0;
        if (typeof data === 'string') {
            op_ws_send_text(this._wsId, data);
            return data.length;
        }
        const { bytes } = normalizeBodyToBytes(data);
        op_ws_send_binary(this._wsId, bytes);
        return bytes.byteLength;
    }
    close(code = 1000, reason = '') {
        if (this.readyState >= 2) return;
        this.readyState = 2; // CLOSING
        op_ws_close(this._wsId, Number(code) || 1000, String(reason || ''));
    }
    ping() {}
    pong() {}
}

class WebSocket extends EventTarget {
    static CONNECTING = 0;
    static OPEN = 1;
    static CLOSING = 2;
    static CLOSED = 3;

    constructor(url, protocols = []) {
        super();
        this.url = String(url);
        this.protocol = Array.isArray(protocols) ? (protocols[0] || '') : String(protocols || '');
        this.readyState = WebSocket.CONNECTING;
        this.extensions = '';
        this.bufferedAmount = 0;
        this.binaryType = 'blob';
        this.onopen = null;
        this.onmessage = null;
        this.onclose = null;
        this.onerror = null;

        if (protocols === '__internal_server_socket__') {
            this._wsId = 0;
        } else {
            const wsId = op_ws_connect(this.url);
            this._wsId = wsId;
            wsConnections.set(wsId, { kind: 'client', socket: this });
        }
    }
    send(data) {
        if (this.readyState !== WebSocket.OPEN) {
            throw new Error('WebSocket is not open');
        }
        if (typeof data === 'string') {
            op_ws_send_text(this._wsId, data);
        } else {
            const { bytes } = normalizeBodyToBytes(data);
            op_ws_send_binary(this._wsId, bytes);
        }
    }
    close(code = 1000, reason = '') {
        if (this.readyState === WebSocket.CLOSING || this.readyState === WebSocket.CLOSED) return;
        this.readyState = WebSocket.CLOSING;
        op_ws_close(this._wsId, Number(code) || 1000, String(reason || ''));
    }
}

WebSocket.prototype.CONNECTING = 0;
WebSocket.prototype.OPEN = 1;
WebSocket.prototype.CLOSING = 2;
WebSocket.prototype.CLOSED = 3;

globalThis.WebSocket = WebSocket;

async function handleIncomingHttpRequest(serverId, reqId, method, url, headersJson, bodyBytes, canUpgrade) {
    const server = activeServers.get(serverId);
    if (!server) {
        op_http_respond(reqId, 503, '[]', new Uint8Array(0));
        return;
    }

    const headersList = JSON.parse(headersJson);
    const hasBody = method !== 'GET' && method !== 'HEAD' && bodyBytes && bodyBytes.byteLength > 0;
    const req = new Request(url, {
        method,
        headers: headersList,
        body: hasBody ? bodyBytes : null,
        _reqId: reqId,
        _serverId: serverId
    });
    req._canUpgrade = canUpgrade;

    try {
        let res = await server._fetch(req, server);
        if (!(res instanceof Response)) {
            res = new Response(res != null ? String(res) : null, { status: 200 });
        }
        const respHeaders = [...res.headers.entries()];
        const respBody = res._bodyBytes || new Uint8Array(0);
        op_http_respond(reqId, res.status || 200, JSON.stringify(respHeaders), respBody);
    } catch (err) {
        if (typeof server._error === 'function') {
            try {
                const errRes = await server._error(err);
                if (errRes instanceof Response) {
                    const respHeaders = [...errRes.headers.entries()];
                    const respBody = errRes._bodyBytes || new Uint8Array(0);
                    op_http_respond(reqId, errRes.status || 500, JSON.stringify(respHeaders), respBody);
                    return;
                }
            } catch (_) {}
        }
        const msg = err instanceof Error ? (err.stack || err.message) : String(err);
        op_http_respond(reqId, 500, JSON.stringify([['content-type', 'text/plain;charset=UTF-8']]), sharedEncoder.encode(msg));
    }
}

function handleWsEvent(wsId, eventType, arg1, arg2) {
    const entry = wsConnections.get(wsId);
    if (!entry) return;

    if (entry.kind === 'bun') {
        const { ws, handlers } = entry;
        if (eventType === 'open') {
            ws.readyState = 1;
            if (arg1) ws.protocol = arg1;
            if (handlers && typeof handlers.open === 'function') {
                handlers.open(ws);
            }
        } else if (eventType === 'message') {
            if (handlers && typeof handlers.message === 'function') {
                handlers.message(ws, arg1);
            }
        } else if (eventType === 'close') {
            ws.readyState = 3;
            wsConnections.delete(wsId);
            if (handlers && typeof handlers.close === 'function') {
                handlers.close(ws, arg1 ?? 1000, arg2 ?? '');
            }
        } else if (eventType === 'error') {
            if (handlers && typeof handlers.error === 'function') {
                handlers.error(ws, new Error(String(arg1)));
            }
        }
    } else if (entry.kind === 'client') {
        const { socket } = entry;
        if (eventType === 'open') {
            socket.readyState = WebSocket.OPEN;
            if (arg1) socket.protocol = arg1;
            socket.dispatchEvent(new Event('open'));
        } else if (eventType === 'message') {
            let data = arg1;
            if (data instanceof Uint8Array && socket.binaryType === 'arraybuffer') {
                data = data.buffer.slice(data.byteOffset, data.byteOffset + data.byteLength);
            }
            socket.dispatchEvent(new MessageEvent('message', { data }));
        } else if (eventType === 'close') {
            socket.readyState = WebSocket.CLOSED;
            wsConnections.delete(wsId);
            socket.dispatchEvent(new CloseEvent('close', { code: arg1 ?? 1000, reason: arg2 ?? '', wasClean: true }));
        } else if (eventType === 'error') {
            socket.dispatchEvent(new ErrorEvent('error', { message: String(arg1), error: new Error(String(arg1)) }));
        }
    }
}

op_set_event_handlers(handleIncomingHttpRequest, handleWsEvent);

function createServer(options = {}) {
    const port = options.port !== undefined ? Number(options.port) : 3000;
    const hostname = options.hostname || '127.0.0.1';
    const fetchHandler = options.fetch;
    if (typeof fetchHandler !== 'function') {
        throw new TypeError('Bun.serve requires a fetch(request, server) function');
    }

    const info = op_http_serve(port, hostname);
    const server = {
        port: info.port,
        hostname: info.hostname,
        development: Boolean(options.development),
        get url() {
            return new URL(`http://${this.hostname}:${this.port}/`);
        },
        _serverId: info.serverId,
        _fetch: fetchHandler,
        _websocket: options.websocket || null,
        _error: options.error || null,
        stop() {
            activeServers.delete(this._serverId);
            op_http_stop(this._serverId);
        },
        upgrade(req, upgradeOpts = {}) {
            const reqId = req && req._reqId;
            if (!reqId) return false;
            const extraHeaders = upgradeOpts.headers ? [...new Headers(upgradeOpts.headers).entries()] : [];
            const wsId = op_ws_upgrade(reqId, JSON.stringify(extraHeaders));
            if (!wsId) return false;
            const serverWs = new ServerWebSocket(wsId, upgradeOpts.data);
            wsConnections.set(wsId, {
                kind: 'bun',
                ws: serverWs,
                handlers: this._websocket
            });
            return true;
        }
    };

    activeServers.set(info.serverId, server);
    return server;
}

globalThis.Bun = {
    version: '1.2.0',
    serve: createServer,
    write: (path, data) => op_fs_write_file_async(String(path), typeof data === 'string' ? data : sharedDecoder.decode(normalizeBodyToBytes(data).bytes)),
    file: (path) => ({
        text: () => op_fs_read_file_async(String(path)),
        exists: async () => {
            try {
                await op_fs_read_file_async(String(path));
                return true;
            } catch (_) {
                return false;
            }
        }
    })
};

globalThis.Deno = {
    serve(optionsOrHandler, maybeHandler) {
        if (typeof optionsOrHandler === 'function') {
            return createServer({ fetch: optionsOrHandler });
        }
        return createServer({
            ...optionsOrHandler,
            fetch: maybeHandler || optionsOrHandler.handler || optionsOrHandler.fetch
        });
    },
    upgradeWebSocket(req, options = {}) {
        const reqId = req && req._reqId;
        if (!reqId) {
            throw new TypeError('Failed to upgrade WebSocket: invalid request');
        }
        const extraHeaders = options.headers ? [...new Headers(options.headers).entries()] : [];
        if (options.protocol) {
            extraHeaders.push(['sec-websocket-protocol', String(options.protocol)]);
        }
        const wsId = op_ws_upgrade(reqId, JSON.stringify(extraHeaders));
        if (!wsId) {
            throw new TypeError('Failed to upgrade WebSocket: request is not upgradable');
        }
        const socket = new WebSocket(req.url, '__internal_server_socket__');
        socket._wsId = wsId;
        socket.binaryType = 'arraybuffer';
        wsConnections.set(wsId, { kind: 'client', socket });
        return {
            response: new Response(null, { status: 101 }),
            socket
        };
    }
};
})
