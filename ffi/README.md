# sasso-ffi

Native C-ABI bindings for the [sasso](https://github.com/momiji-rs/sasso) SCSS
compiler, for embedding it in other languages (Ruby, Python, Go, PHP, C/C++,
Node via N-API, …).

This is a thin `cdylib`/`staticlib` wrapper around `sasso::compile`. Like the
wasm wrapper it lives **outside** the core `sasso` workspace so the compiler
crate keeps `unsafe_code = "deny"` and zero dependencies while this FFI layer
uses the `unsafe` an `extern "C"` ABI needs. The C header
(`include/sasso.h`) is hand-written — no bindgen build dependency.

## Build

```sh
cd ffi
cargo build --release
# -> target/release/libsasso_ffi.{so,dylib}  (cdylib)
# -> target/release/libsasso_ffi.a           (staticlib)
```

## C API

See [`include/sasso.h`](include/sasso.h). The shape:

```c
sasso_options opts = {0};            /* zero = expanded SCSS, no map */
opts.style = SASSO_STYLE_COMPRESSED;
const char *paths[] = { "src/scss", NULL };
opts.load_paths = paths;             /* the CLI -I flag */

sasso_result *r = sasso_compile("@import 'base'; .a{color:red}", &opts);
if (r->ok) {
    puts(r->css);
} else {
    fprintf(stderr, "%s (%u:%u)\n", r->error_message, r->error_line, r->error_col);
}
sasso_result_free(r);                /* frees the struct AND every string */
```

### Ownership

`sasso_compile` returns a heap `sasso_result`; sasso owns it and every string
it points at. Free the whole graph with a single `sasso_result_free` — the host
never frees individual fields. Strings are NUL-terminated UTF-8.

### Custom importers

Set `opts.importer.resolve` to a `const char *(*)(void *user_data, const char
*path)` to resolve `@import`/`@use` from memory, a database, a VFS, etc. Return
the source or `NULL`. If you also set `importer.free`, sasso calls it with the
pointer you returned once it has copied the source. A host importer takes
precedence over `load_paths`.

## C example

[`examples/example.c`](examples/example.c) links the staticlib and exercises
the same surface:

```sh
cd ffi && cargo build --release
cc -I include examples/example.c target/release/libsasso_ffi.a \
   -o /tmp/sasso_example -framework CoreFoundation   # macOS
# Linux: ... -o /tmp/sasso_example -lpthread -ldl -lm
/tmp/sasso_example
```

## Python example

[`examples/sasso.py`](examples/sasso.py) is a pure-stdlib `ctypes` binding and
smoke test covering expanded/compressed output, error reporting, load paths, a
host importer callback, and source maps:

```sh
cd ffi && cargo build --release && python3 examples/sasso.py
```

## Thread-safety

The library is thread-compatible: independent threads may compile concurrently,
provided they do not share a `sasso_result`. (The bump-arena allocator is
per-compile-scope.)
