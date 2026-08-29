//! M2 guest: Luaur embedded in a wasm32-wasip1 cdylib, driven by kanbei-vm.
//!
//! Dispatcher ABI (per the S1 finding: prefer one dispatcher over many
//! same-signature imports):
//! - `kb_host(op, x)` — scalar dispatcher (e.g. `kb_host_double`).
//! - `kb_host_buf(op, ptr, len)` — payload-carrying sibling for JSON: the
//!   guest copies the request into `KB_SCRATCH`, the host reads it from guest
//!   memory and writes the result back at the same `ptr`, returning the result
//!   length (`>= 0`) or a negative host error code (`-1` generic, `-2` stale
//!   generation).
//!
//! The guest is deterministic: no time, no randomness, no environment.
//!
//! The exported `extern "C"` surface IS the ABI: raw pointers cross the wasm
//! boundary by design, so the deref-safety lint does not apply there.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::slice;
use std::sync::Mutex;

/// Scratch buffer the host writes inputs into before calling an export, and the
/// shared channel for `kb_host_buf` payloads. Layout:
/// - offset 0: `kb_host_buf` request/result
/// - host-initiated flows pass explicit `(ptr, len)` / `(ptr, len, out, cap)`
///   args; the host places inputs at the scratch base.
pub static mut KB_SCRATCH: [u8; SCRATCH_SIZE] = [0u8; SCRATCH_SIZE];

/// Scratch capacity in bytes.
pub const SCRATCH_SIZE: usize = 1 << 20;

/// Return-code table. Every export returns `>= 0` on success (a length or 0)
/// or one of these negative codes:
/// - `RC_BAD_UTF8`: input buffer is not valid UTF-8
/// - `RC_COMPILE`: Luau source failed to compile
/// - `RC_BUF_SMALL`: output buffer (`cap`) smaller than the result
/// - `RC_FN_CREATE`: failed to create a host-function binding
/// - `RC_GLOBAL_SET`: failed to install a global
/// - `RC_EXEC`: source failed to load/execute
/// - `RC_NO_HOT`: source does not define a global `kb_hot`
/// - `RC_NO_INIT`: hot VM not initialized (call `kb_init` first)
/// - `RC_HOT_CALL`: `kb_hot` call failed (incl. multi-value results)
/// - `RC_BAD_JSON`: args are not a single valid JSON value
/// - `RC_JSON_OUT`: result is not JSON-serializable (function/userdata/thread,
///   non-UTF-8 string, non-string/non-int table key)
/// - `RC_HOST`: host dispatcher returned an error code
/// - `RC_BOUNDS`: a buffer argument exceeds the scratch
pub const RC_OK: i32 = 0;
pub const RC_BAD_UTF8: i32 = -1;
pub const RC_COMPILE: i32 = -2;
pub const RC_BUF_SMALL: i32 = -3;
pub const RC_FN_CREATE: i32 = -4;
pub const RC_GLOBAL_SET: i32 = -5;
pub const RC_EXEC: i32 = -6;
pub const RC_NO_HOT: i32 = -7;
pub const RC_NO_INIT: i32 = -8;
pub const RC_HOT_CALL: i32 = -9;
pub const RC_BAD_JSON: i32 = -10;
pub const RC_JSON_OUT: i32 = -11;
pub const RC_HOST: i32 = -12;
pub const RC_BOUNDS: i32 = -13;

// Host dispatchers (see module docs).
unsafe extern "C" {
    fn kb_host(op: i32, x: i32) -> i32;
    fn kb_host_buf(op: i32, ptr: i32, len: i32) -> i32;
}

/// Persistent hot-path VM: holds one compiled generation so host→guest calls
/// don't recompile. Empty until `kb_init`.
static HOT: Mutex<Option<(luaur::rt::Lua, luaur::rt::Function)>> = Mutex::new(None);

/// Base of the scratch buffer, for host-side writes.
#[unsafe(no_mangle)]
pub extern "C" fn kb_scratch() -> *mut u8 {
    std::ptr::addr_of_mut!(KB_SCRATCH).cast::<u8>()
}

fn src<'a>(ptr: *const u8, len: usize) -> Option<&'a str> {
    if len == 0 {
        return Some("");
    }
    let bytes = unsafe { slice::from_raw_parts(ptr, len) };
    std::str::from_utf8(bytes).ok()
}

/// Install the Luau-facing host functions: `kb_host_double(x)` (scalar, op 0)
/// and `kb_host_call(op, payload_json) -> result_json` (payload, op as given).
fn install_host_fns(lua: &luaur::rt::Lua) -> i32 {
    let double = match lua.create_function(|_, x: i32| Ok(unsafe { kb_host(0, x) })) {
        Ok(f) => f,
        Err(_) => return RC_FN_CREATE,
    };
    let host_call = match lua.create_function(|lua, (op, payload): (i32, String)| {
        host_call_impl(lua, op, payload)
    }) {
        Ok(f) => f,
        Err(_) => return RC_FN_CREATE,
    };
    if lua.globals().set("kb_host_double", double).is_err() {
        return RC_GLOBAL_SET;
    }
    if lua.globals().set("kb_host_call", host_call).is_err() {
        return RC_GLOBAL_SET;
    }
    RC_OK
}

/// `kb_host_call` body: copy the JSON payload into the scratch, dispatch via
/// `kb_host_buf`, copy the result back into a Lua string.
fn host_call_impl(
    lua: &luaur::rt::Lua,
    op: i32,
    payload: String,
) -> luaur::rt::Result<luaur::rt::LuaString> {
    let payload = payload.into_bytes();
    if payload.len() > SCRATCH_SIZE {
        return Err(luaur::rt::Error::RuntimeError(
            "kb_host_call: payload exceeds the scratch buffer".into(),
        ));
    }
    unsafe {
        let dst = std::ptr::addr_of_mut!(KB_SCRATCH).cast::<u8>();
        std::ptr::copy_nonoverlapping(payload.as_ptr(), dst, payload.len());
        let ret = kb_host_buf(op, std::ptr::addr_of!(KB_SCRATCH) as i32, payload.len() as i32);
        if ret < 0 {
            // Defensive: kanbei-vm traps on host errors instead, so this only
            // fires against a non-trapping host.
            let why = if ret == -2 { "stale generation" } else { "host error" };
            return Err(luaur::rt::Error::RuntimeError(
                format!("kb_host_call: {why} (code {ret})"),
            ));
        }
        // Cap defensively: the host contract bounds the result to the scratch.
        let n = (ret as usize).min(SCRATCH_SIZE);
        let out = slice::from_raw_parts(std::ptr::addr_of!(KB_SCRATCH).cast::<u8>(), n);
        Ok(lua.create_string(out))
    }
}

/// Load `src`, keep the VM, install host functions, and store the global
/// `kb_hot` function for `kb_hot_call` / `kb_hot_call_str`.
#[unsafe(no_mangle)]
pub extern "C" fn kb_init(ptr: *const u8, len: usize) -> i32 {
    let Some(s) = src(ptr, len) else { return RC_BAD_UTF8 };
    let lua = luaur::rt::Lua::new();
    let rc = install_host_fns(&lua);
    if rc != RC_OK {
        return rc;
    }
    if lua.load(s).exec().is_err() {
        return RC_EXEC;
    }
    let f = match lua.globals().get::<luaur::rt::Function>("kb_hot") {
        Ok(f) => f,
        Err(_) => return RC_NO_HOT,
    };
    let mut guard = HOT.lock().unwrap();
    *guard = Some((lua, f));
    RC_OK
}

/// Call the cached `kb_hot(x)` function. Returns its result, or a negative
/// code.
#[unsafe(no_mangle)]
pub extern "C" fn kb_hot_call(x: i32) -> i32 {
    let guard = HOT.lock().unwrap();
    let Some((_, f)) = guard.as_ref() else { return RC_NO_INIT };
    match f.call::<i32>(x) {
        Ok(v) => v,
        Err(_) => RC_HOT_CALL,
    }
}

/// Call the cached `kb_hot` with one JSON value (args at `ptr`), write the
/// JSON-serialized result to `out` (cap bytes). Returns the result length or a
/// negative code.
///
/// Chosen marshalling shape: direct `(ptr, len, out, cap)` export mirroring
/// `kb_compile_out`, rather than marshalling through the scratch — the scratch
/// stays exclusively the `kb_host_buf` payload channel.
#[unsafe(no_mangle)]
pub extern "C" fn kb_hot_call_str(ptr: *const u8, len: usize, out: *mut u8, cap: usize) -> i32 {
    let Some(s) = src(ptr, len) else { return RC_BAD_UTF8 };
    let guard = HOT.lock().unwrap();
    let Some((lua, f)) = guard.as_ref() else { return RC_NO_INIT };
    let args = match json::parse(lua, s.as_bytes()) {
        Ok(v) => v,
        Err(()) => return RC_BAD_JSON,
    };
    let result = match f.call::<luaur::rt::Value>(args) {
        Ok(v) => v,
        Err(_) => return RC_HOT_CALL,
    };
    let mut buf = Vec::new();
    if json::serialize(lua, &result, &mut buf).is_err() {
        return RC_JSON_OUT;
    }
    if buf.len() > cap {
        return RC_BUF_SMALL;
    }
    unsafe { std::ptr::copy_nonoverlapping(buf.as_ptr(), out, buf.len()) };
    buf.len() as i32
}

/// Compile and copy the serialized bytecode into `out` (cap bytes). Returns
/// the bytecode length, or a negative code.
#[unsafe(no_mangle)]
pub extern "C" fn kb_compile_out(ptr: *const u8, len: usize, out: *mut u8, cap: usize) -> i32 {
    let Some(s) = src(ptr, len) else { return RC_BAD_UTF8 };
    match luaur::compile(s) {
        Ok(bc) => {
            if bc.len() > cap {
                return RC_BUF_SMALL;
            }
            unsafe { std::ptr::copy_nonoverlapping(bc.as_ptr(), out, bc.len()) };
            bc.len() as i32
        }
        Err(_) => RC_COMPILE,
    }
}

/// Compile + run Luau source with host functions available. Returns 0 on
/// success or a negative code.
#[unsafe(no_mangle)]
pub extern "C" fn kb_run(ptr: *const u8, len: usize) -> i32 {
    let Some(s) = src(ptr, len) else { return RC_BAD_UTF8 };
    let lua = luaur::rt::Lua::new();
    let rc = install_host_fns(&lua);
    if rc != RC_OK {
        return rc;
    }
    match lua.load(s).exec() {
        Ok(()) => RC_OK,
        Err(_) => RC_EXEC,
    }
}

/// Minimal deterministic JSON (RFC 8259 subset) for the marshalling layer:
/// `null`, booleans, numbers, strings, arrays, objects. Hand-rolled so the
/// guest stays dependency-free and byte-exact (e.g. `10` serializes as `"10"`,
/// not `"10.0"`).
mod json {
    use luaur::rt::{Lua, Value};

    pub fn parse(lua: &Lua, b: &[u8]) -> Result<Value, ()> {
        let mut p = Parser { b, i: 0 };
        let v = p.value(lua)?;
        p.ws();
        if p.i != p.b.len() {
            return Err(());
        }
        Ok(v)
    }

    struct Parser<'a> {
        b: &'a [u8],
        i: usize,
    }

    impl Parser<'_> {
        fn ws(&mut self) {
            while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\t' | b'\n' | b'\r') {
                self.i += 1;
            }
        }

        fn eat(&mut self, c: u8) -> Result<(), ()> {
            if self.b.get(self.i) == Some(&c) {
                self.i += 1;
                Ok(())
            } else {
                Err(())
            }
        }

        fn lit(&mut self, s: &[u8]) -> Result<(), ()> {
            if self.b.get(self.i..self.i + s.len()) == Some(s) {
                self.i += s.len();
                Ok(())
            } else {
                Err(())
            }
        }

        fn value(&mut self, lua: &Lua) -> Result<Value, ()> {
            self.ws();
            match self.b.get(self.i) {
                Some(b'n') => {
                    self.lit(b"null")?;
                    Ok(Value::Nil)
                }
                Some(b't') => {
                    self.lit(b"true")?;
                    Ok(Value::Boolean(true))
                }
                Some(b'f') => {
                    self.lit(b"false")?;
                    Ok(Value::Boolean(false))
                }
                Some(b'"') => self.string().map(|s| Value::String(lua.create_string(s))),
                Some(b'[') => self.array(lua),
                Some(b'{') => self.object(lua),
                Some(b'-' | b'0'..=b'9') => self.number(),
                _ => Err(()),
            }
        }

        fn array(&mut self, lua: &Lua) -> Result<Value, ()> {
            self.eat(b'[')?;
            let t = lua.create_table();
            self.ws();
            if self.b.get(self.i) == Some(&b']') {
                self.i += 1;
                return Ok(Value::Table(t));
            }
            let mut n = 1i64;
            loop {
                let v = self.value(lua)?;
                t.set(n, v).map_err(|_| ())?;
                n += 1;
                self.ws();
                match self.b.get(self.i) {
                    Some(b',') => self.i += 1,
                    Some(b']') => {
                        self.i += 1;
                        return Ok(Value::Table(t));
                    }
                    _ => return Err(()),
                }
            }
        }

        fn object(&mut self, lua: &Lua) -> Result<Value, ()> {
            self.eat(b'{')?;
            let t = lua.create_table();
            self.ws();
            if self.b.get(self.i) == Some(&b'}') {
                self.i += 1;
                return Ok(Value::Table(t));
            }
            loop {
                self.ws();
                let key = String::from_utf8(self.string()?).map_err(|_| ())?;
                self.ws();
                self.eat(b':')?;
                let v = self.value(lua)?;
                t.set(key, v).map_err(|_| ())?;
                self.ws();
                match self.b.get(self.i) {
                    Some(b',') => self.i += 1,
                    Some(b'}') => {
                        self.i += 1;
                        return Ok(Value::Table(t));
                    }
                    _ => return Err(()),
                }
            }
        }

        fn string(&mut self) -> Result<Vec<u8>, ()> {
            self.eat(b'"')?;
            let mut out = Vec::new();
            loop {
                let Some(&c) = self.b.get(self.i) else { return Err(()) };
                self.i += 1;
                match c {
                    b'"' => return Ok(out),
                    b'\\' => {
                        let Some(&e) = self.b.get(self.i) else { return Err(()) };
                        self.i += 1;
                        match e {
                            b'"' => out.push(b'"'),
                            b'\\' => out.push(b'\\'),
                            b'/' => out.push(b'/'),
                            b'b' => out.push(0x08),
                            b'f' => out.push(0x0C),
                            b'n' => out.push(b'\n'),
                            b'r' => out.push(b'\r'),
                            b't' => out.push(b'\t'),
                            b'u' => {
                                let cp = self.hex4()?;
                                let cp = if (0xD800..=0xDBFF).contains(&cp) {
                                    if self.b.get(self.i..self.i + 2) == Some(b"\\u") {
                                        self.i += 2;
                                        let lo = self.hex4()?;
                                        if (0xDC00..=0xDFFF).contains(&lo) {
                                            0x10000 + ((cp - 0xD800) << 10) + (lo - 0xDC00)
                                        } else {
                                            0xFFFD
                                        }
                                    } else {
                                        0xFFFD
                                    }
                                } else if (0xDC00..=0xDFFF).contains(&cp) {
                                    0xFFFD
                                } else {
                                    cp
                                };
                                push_utf8(&mut out, cp);
                            }
                            _ => return Err(()),
                        }
                    }
                    0x00..=0x1F => return Err(()), // unescaped control character
                    c => out.push(c),
                }
            }
        }

        fn hex4(&mut self) -> Result<u32, ()> {
            let mut v = 0u32;
            for _ in 0..4 {
                let Some(&c) = self.b.get(self.i) else { return Err(()) };
                self.i += 1;
                v = v * 16
                    + match c {
                        b'0'..=b'9' => (c - b'0') as u32,
                        b'a'..=b'f' => (c - b'a' + 10) as u32,
                        b'A'..=b'F' => (c - b'A' + 10) as u32,
                        _ => return Err(()),
                    };
            }
            Ok(v)
        }

        fn number(&mut self) -> Result<Value, ()> {
            let start = self.i;
            if self.b.get(self.i) == Some(&b'-') {
                self.i += 1;
            }
            match self.b.get(self.i) {
                Some(b'0') => self.i += 1,
                Some(b'1'..=b'9') => {
                    while matches!(self.b.get(self.i), Some(b'0'..=b'9')) {
                        self.i += 1;
                    }
                }
                _ => return Err(()),
            }
            let mut is_int = true;
            if self.b.get(self.i) == Some(&b'.') {
                is_int = false;
                self.i += 1;
                if !matches!(self.b.get(self.i), Some(b'0'..=b'9')) {
                    return Err(());
                }
                while matches!(self.b.get(self.i), Some(b'0'..=b'9')) {
                    self.i += 1;
                }
            }
            if matches!(self.b.get(self.i), Some(b'e' | b'E')) {
                is_int = false;
                self.i += 1;
                if matches!(self.b.get(self.i), Some(b'+' | b'-')) {
                    self.i += 1;
                }
                if !matches!(self.b.get(self.i), Some(b'0'..=b'9')) {
                    return Err(());
                }
                while matches!(self.b.get(self.i), Some(b'0'..=b'9')) {
                    self.i += 1;
                }
            }
            let tok = std::str::from_utf8(&self.b[start..self.i]).map_err(|_| ())?;
            if is_int
                && let Ok(i) = tok.parse::<i64>()
                // Exact f64 round-trip, so Lua (f64) keeps the value.
                && i as f64 as i64 == i
            {
                return Ok(Value::Integer(i));
            }
            tok.parse::<f64>().map(Value::Number).map_err(|_| ())
        }
    }

    fn push_utf8(out: &mut Vec<u8>, cp: u32) {
        if cp < 0x80 {
            out.push(cp as u8);
        } else if cp < 0x800 {
            out.push(0xC0 | (cp >> 6) as u8);
            out.push(0x80 | (cp & 0x3F) as u8);
        } else if cp < 0x10000 {
            out.push(0xE0 | (cp >> 12) as u8);
            out.push(0x80 | ((cp >> 6) & 0x3F) as u8);
            out.push(0x80 | (cp & 0x3F) as u8);
        } else {
            out.push(0xF0 | (cp >> 18) as u8);
            out.push(0x80 | ((cp >> 12) & 0x3F) as u8);
            out.push(0x80 | ((cp >> 6) & 0x3F) as u8);
            out.push(0x80 | (cp & 0x3F) as u8);
        }
    }

    /// Serialize a Lua value as JSON. Tables with contiguous integer keys
    /// `1..=n` become arrays; everything else becomes an object with keys
    /// sorted bytewise (canonical shape); an empty table is `{}`. Non-finite
    /// numbers become `null`; non-JSON values (functions, userdata, threads,
    /// non-UTF-8 strings, non-string/non-int keys) are an error.
    #[allow(clippy::only_used_in_recursion)] // `lua` threads the VM handle down for nested tables
pub fn serialize(lua: &Lua, v: &Value, out: &mut Vec<u8>) -> Result<(), ()> {
        match v {
            Value::Nil => out.extend_from_slice(b"null"),
            Value::Boolean(b) => out.extend_from_slice(if *b { b"true" } else { b"false" }),
            Value::Integer(i) => out.extend_from_slice(i.to_string().as_bytes()),
            Value::Number(n) => {
                if !n.is_finite() {
                    out.extend_from_slice(b"null");
                } else if n.fract() == 0.0 && n.abs() < 1e15 {
                    out.extend_from_slice((*n as i64).to_string().as_bytes());
                } else {
                    out.extend_from_slice(format!("{n}").as_bytes());
                }
            }
            Value::String(s) => push_json_string(&s.to_str().map_err(|_| ())?, out),
            Value::Table(t) => {
                let mut seq: Vec<(i64, Value)> = Vec::new();
                let mut obj: Vec<(String, Value)> = Vec::new();
                for pair in t.pairs::<Value, Value>() {
                    let (k, v) = pair.map_err(|_| ())?;
                    match k {
                        Value::Integer(i) if i >= 1 => seq.push((i, v)),
                        Value::String(s) => obj.push((s.to_str().map_err(|_| ())?, v)),
                        _ => return Err(()),
                    }
                }
                let n = seq.len();
                let is_seq = obj.is_empty()
                    && n > 0
                    && seq.iter().map(|(i, _)| *i).max() == Some(n as i64);
                if is_seq {
                    seq.sort_by_key(|(i, _)| *i);
                    out.push(b'[');
                    for (idx, (_, v)) in seq.iter().enumerate() {
                        if idx > 0 {
                            out.push(b',');
                        }
                        serialize(lua, v, out)?;
                    }
                    out.push(b']');
                } else {
                    obj.sort_by(|(a, _), (b, _)| a.cmp(b));
                    out.push(b'{');
                    for (idx, (k, v)) in obj.iter().enumerate() {
                        if idx > 0 {
                            out.push(b',');
                        }
                        push_json_string(k, out);
                        out.push(b':');
                        serialize(lua, v, out)?;
                    }
                    out.push(b'}');
                }
            }
            _ => return Err(()),
        }
        Ok(())
    }

    fn push_json_string(s: &str, out: &mut Vec<u8>) {
        out.push(b'"');
        for &c in s.as_bytes() {
            match c {
                b'"' => out.extend_from_slice(b"\\\""),
                b'\\' => out.extend_from_slice(b"\\\\"),
                0x08 => out.extend_from_slice(b"\\b"),
                0x0C => out.extend_from_slice(b"\\f"),
                b'\n' => out.extend_from_slice(b"\\n"),
                b'\r' => out.extend_from_slice(b"\\r"),
                b'\t' => out.extend_from_slice(b"\\t"),
                0x00..=0x1F => out.extend_from_slice(format!("\\u{:04x}", c).as_bytes()),
                c => out.push(c),
            }
        }
        out.push(b'"');
    }
}
