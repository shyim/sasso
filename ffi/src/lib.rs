//! Native C-ABI wrapper around [`sasso::compile`] for embedding the SCSS
//! compiler in other languages.
//!
//! The ABI is deliberately small and ownership-simple:
//!
//! * [`sasso_compile`] takes the source plus a [`sasso_options`] struct and
//!   returns a heap-allocated [`sasso_result`] (or null on allocation
//!   failure). Every string the result points at is owned by sasso.
//! * [`sasso_result_free`] frees the result *and* all of its strings in one
//!   call, so the host never frees individual fields.
//! * Strings crossing the boundary are NUL-terminated UTF-8 (`char*`), which
//!   every common FFI host (ctypes, cffi, Ruby FFI, cgo, JNA, PHP FFI) marshals
//!   natively. SCSS/CSS never contains an interior NUL in practice.
//!
//! See `include/sasso.h` for the C declarations and `examples/` for a working
//! host binding.

// FFI entry points dereference raw pointers by contract (the caller upholds the
// invariants documented per function). Keeping the signatures pointer-typed
// (not `unsafe fn`) is intentional so the C header stays plain.
#![allow(clippy::not_unsafe_ptr_arg_deref)]
#![allow(clippy::missing_safety_doc)]

use std::ffi::{c_char, c_void, CStr, CString};
use std::path::PathBuf;
use std::ptr;

use sasso::{compile, compile_with_source_map, FsImporter, Importer, Options, OutputStyle, Syntax};

// Install sasso's scoped bump arena as the global allocator, exactly like the
// wasm wrapper: every allocation inside a `compile()` scope is a pointer bump
// from one pre-grown region, reset (not freed) when the compile ends. The FFI
// result strings below are deep-cloned out of the arena by `compile()` before
// the reset, then handed to `CString`, which routes to the system allocator
// (outside any scope), so they outlive the reset and free correctly.
#[global_allocator]
static GLOBAL: sasso::ScopedAlloc = sasso::ScopedAlloc;

/// Output style: matches [`OutputStyle`]. `0` = expanded, `1` = compressed.
pub const SASSO_STYLE_EXPANDED: u8 = 0;
pub const SASSO_STYLE_COMPRESSED: u8 = 1;

/// Input syntax: matches [`Syntax`]. `0` = scss, `1` = indented (`.sass`),
/// `2` = plain css.
pub const SASSO_SYNTAX_SCSS: u8 = 0;
pub const SASSO_SYNTAX_SASS: u8 = 1;
pub const SASSO_SYNTAX_CSS: u8 = 2;

/// A host-supplied importer: a resolve callback plus an optional free callback.
///
/// `resolve(user_data, path)` is called for each `@import`/`@use`/`@forward`
/// URL. It must return either a NUL-terminated UTF-8 buffer holding the
/// resolved source, or null if the URL cannot be found. If `free` is non-null,
/// sasso calls `free(user_data, returned_ptr)` after copying the source, so the
/// host can release whatever the resolve callback allocated; if `free` is null,
/// sasso assumes the returned buffer is owned by the host and does not free it
/// (e.g. a pointer into a long-lived string table).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct sasso_importer {
    /// Opaque pointer passed back to the callbacks unchanged.
    pub user_data: *mut c_void,
    /// `resolve(user_data, path) -> source | NULL`.
    pub resolve: Option<extern "C" fn(*mut c_void, *const c_char) -> *const c_char>,
    /// Optional `free(user_data, ptr)` for what `resolve` returned.
    pub free: Option<extern "C" fn(*mut c_void, *const c_char)>,
}

/// Compile options. Zero-initializing this struct yields the defaults:
/// expanded SCSS, no source map, no URL, no load paths, no importer.
#[repr(C)]
pub struct sasso_options {
    /// One of `SASSO_STYLE_*`. Unknown values fall back to expanded.
    pub style: u8,
    /// One of `SASSO_SYNTAX_*`. Unknown values fall back to scss.
    pub syntax: u8,
    /// Non-zero to allow non-ASCII characters in output/messages.
    pub unicode: u8,
    /// Non-zero to also produce a Source Map v3 (in `result.source_map`).
    pub source_map: u8,
    /// Input URL/filename (used for messages and the source map's `file`), or
    /// null for the default (`"stdin"`).
    pub url: *const c_char,
    /// A null-terminated array of load-path directories (the CLI `-I` flag),
    /// searched for `@import`/`@use`. Null or an empty array means none.
    pub load_paths: *const *const c_char,
    /// An optional host importer, tried before `load_paths`. Set `resolve` to
    /// null to disable (the whole struct may be zeroed).
    pub importer: sasso_importer,
}

/// The result of [`sasso_compile`]. All non-null `char*` fields are owned by
/// sasso; release the whole struct with [`sasso_result_free`].
#[repr(C)]
pub struct sasso_result {
    /// `1` on success, `0` on failure.
    pub ok: i32,
    /// The compiled CSS (NUL-terminated UTF-8) on success, else null.
    pub css: *mut c_char,
    /// The Source Map v3 JSON when `options.source_map` was set and the compile
    /// succeeded, else null.
    pub source_map: *mut c_char,
    /// The error message (NUL-terminated UTF-8) on failure, else null.
    pub error_message: *mut c_char,
    /// 1-based error line, or `0` when unknown / on success.
    pub error_line: u32,
    /// 1-based error column, or `0` when unknown / on success.
    pub error_col: u32,
}

/// The sasso version string (the core crate's, NUL-terminated, `'static`).
/// Never freed by the caller.
#[no_mangle]
pub extern "C" fn sasso_version() -> *const c_char {
    // A compile-time-built C string of the dependency's version.
    concat!(env!("CARGO_PKG_VERSION"), "\0").as_ptr() as *const c_char
}

/// Override the bump arena reservation size, in bytes, before the first
/// compile. `0` disables the arena (slower, lower peak memory). No-op once a
/// compile has run. Mirrors the wasm wrapper's `sasso_set_arena_bytes`.
#[no_mangle]
pub extern "C" fn sasso_set_arena_bytes(bytes: usize) {
    sasso::set_arena_bytes(bytes);
}

/// An [`Importer`] that bridges to the host's C callbacks.
struct HostImporter {
    user_data: *mut c_void,
    resolve: extern "C" fn(*mut c_void, *const c_char) -> *const c_char,
    free: Option<extern "C" fn(*mut c_void, *const c_char)>,
}

impl Importer for HostImporter {
    fn resolve(&self, path: &str) -> Option<String> {
        // The path is short-lived; build a temporary C string for the call.
        let c_path = CString::new(path).ok()?;
        let ret = (self.resolve)(self.user_data, c_path.as_ptr());
        if ret.is_null() {
            return None;
        }
        // SAFETY: the host contract says a non-null return is a NUL-terminated
        // buffer that stays valid until our `free` callback runs (or, if `free`
        // is null, until at least after this call). We copy before freeing.
        let src = unsafe { CStr::from_ptr(ret) }
            .to_str()
            .ok()
            .map(str::to_owned);
        if let Some(free) = self.free {
            free(self.user_data, ret);
        }
        src
    }
}

/// Read a null-terminated array of C strings into owned `PathBuf`s.
///
/// # Safety
/// `arr` is either null or points at a null-terminated array of valid
/// NUL-terminated C strings.
unsafe fn collect_load_paths(arr: *const *const c_char) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if arr.is_null() {
        return out;
    }
    let mut i = 0isize;
    loop {
        let entry = *arr.offset(i);
        if entry.is_null() {
            break;
        }
        if let Ok(s) = CStr::from_ptr(entry).to_str() {
            out.push(PathBuf::from(s));
        }
        i += 1;
    }
    out
}

/// Convert an owned `String` into a heap `char*` the host can later free via
/// [`sasso_result_free`]. Returns null if the string contains an interior NUL
/// (which compiled CSS / sasso messages never do).
fn into_c_string(s: String) -> *mut c_char {
    match CString::new(s) {
        Ok(c) => c.into_raw(),
        Err(_) => ptr::null_mut(),
    }
}

/// Compile `source` (a NUL-terminated UTF-8 C string) with `opts`.
///
/// Returns a heap-allocated [`sasso_result`], or null only if `source` is null
/// or not valid UTF-8 in a way that prevents even reporting an error (a null
/// `source` yields null; invalid UTF-8 yields a failure result). Free the
/// result with [`sasso_result_free`].
#[no_mangle]
pub extern "C" fn sasso_compile(
    source: *const c_char,
    opts: *const sasso_options,
) -> *mut sasso_result {
    if source.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: caller guarantees `source` is a valid NUL-terminated buffer.
    let source = match unsafe { CStr::from_ptr(source) }.to_str() {
        Ok(s) => s,
        Err(_) => return boxed_error("input is not valid UTF-8".to_string(), 0, 0),
    };

    // SAFETY: `opts` is either null or a valid `sasso_options`.
    let opts_ref: Option<&sasso_options> = unsafe { opts.as_ref() };

    let want_source_map = opts_ref.is_some_and(|o| o.source_map != 0);
    let style = match opts_ref.map(|o| o.style) {
        Some(SASSO_STYLE_COMPRESSED) => OutputStyle::Compressed,
        _ => OutputStyle::Expanded,
    };
    let syntax = match opts_ref.map(|o| o.syntax) {
        Some(SASSO_SYNTAX_SASS) => Syntax::Sass,
        Some(SASSO_SYNTAX_CSS) => Syntax::Css,
        _ => Syntax::Scss,
    };
    let unicode = opts_ref.is_some_and(|o| o.unicode != 0);

    // The URL string, kept alive for the borrow in `Options`.
    let url: Option<String> = opts_ref.and_then(|o| {
        if o.url.is_null() {
            None
        } else {
            // SAFETY: non-null `url` is a NUL-terminated C string per contract.
            unsafe { CStr::from_ptr(o.url) }.to_str().ok().map(str::to_owned)
        }
    });

    // Pick the importer: a host callback wins; otherwise an `FsImporter` over
    // the load paths (only when some were given).
    let host_importer: Option<HostImporter> = opts_ref.and_then(|o| {
        o.importer.resolve.map(|resolve| HostImporter {
            user_data: o.importer.user_data,
            resolve,
            free: o.importer.free,
        })
    });
    let fs_importer: Option<FsImporter> = if host_importer.is_none() {
        // SAFETY: `load_paths` is null or a null-terminated C-string array.
        let paths = unsafe {
            opts_ref.map(|o| collect_load_paths(o.load_paths)).unwrap_or_default()
        };
        if paths.is_empty() {
            None
        } else {
            Some(FsImporter::new(paths))
        }
    } else {
        None
    };

    let mut options = Options::new().with_style(style).with_syntax(syntax).with_unicode(unicode);
    if let Some(u) = &url {
        options = options.with_url(u);
    }
    if want_source_map {
        options = options.with_source_map_include_sources(true);
    }
    if let Some(imp) = &host_importer {
        options = options.with_importer(imp as &dyn Importer);
    } else if let Some(imp) = &fs_importer {
        options = options.with_importer(imp as &dyn Importer);
    }

    if want_source_map {
        match compile_with_source_map(source, &options) {
            Ok(res) => boxed_ok(res.css, Some(res.source_map.to_json())),
            Err(e) => boxed_error(e.message.clone(), e.line as u32, e.col as u32),
        }
    } else {
        match compile(source, &options) {
            Ok(css) => boxed_ok(css, None),
            Err(e) => boxed_error(e.message.clone(), e.line as u32, e.col as u32),
        }
    }
}

/// Allocate a success result.
fn boxed_ok(css: String, source_map: Option<String>) -> *mut sasso_result {
    Box::into_raw(Box::new(sasso_result {
        ok: 1,
        css: into_c_string(css),
        source_map: source_map.map(into_c_string).unwrap_or(ptr::null_mut()),
        error_message: ptr::null_mut(),
        error_line: 0,
        error_col: 0,
    }))
}

/// Allocate a failure result.
fn boxed_error(message: String, line: u32, col: u32) -> *mut sasso_result {
    Box::into_raw(Box::new(sasso_result {
        ok: 0,
        css: ptr::null_mut(),
        source_map: ptr::null_mut(),
        error_message: into_c_string(message),
        error_line: line,
        error_col: col,
    }))
}

/// Free a [`sasso_result`] and every string it owns. Null is a no-op.
/// Calling it twice on the same pointer is undefined behavior.
#[no_mangle]
pub extern "C" fn sasso_result_free(result: *mut sasso_result) {
    if result.is_null() {
        return;
    }
    // SAFETY: `result` came from `Box::into_raw` in this module; reclaim it.
    let boxed = unsafe { Box::from_raw(result) };
    free_c_string(boxed.css);
    free_c_string(boxed.source_map);
    free_c_string(boxed.error_message);
    // `boxed` drops here, freeing the struct itself.
}

/// Reclaim a `char*` produced by [`into_c_string`].
fn free_c_string(ptr: *mut c_char) {
    if !ptr.is_null() {
        // SAFETY: `ptr` was produced by `CString::into_raw` in this module.
        unsafe { drop(CString::from_raw(ptr)) };
    }
}
