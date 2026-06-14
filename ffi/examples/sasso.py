"""Pure-stdlib ctypes binding for the sasso C ABI (see ../include/sasso.h).

Loads libsasso_ffi from the cargo target dir and exposes a small `compile()`
helper. Run this file directly to exercise compile, compressed output, error
reporting, load paths, and a host importer callback end-to-end.
"""

from __future__ import annotations

import ctypes
import os
import sys
from ctypes import (
    CFUNCTYPE,
    POINTER,
    Structure,
    c_char_p,
    c_int32,
    c_size_t,
    c_uint8,
    c_uint32,
    c_void_p,
)

STYLE_EXPANDED, STYLE_COMPRESSED = 0, 1
SYNTAX_SCSS, SYNTAX_SASS, SYNTAX_CSS = 0, 1, 2

# The resolve callback returns a raw pointer (c_void_p, not c_char_p) so ctypes
# does not try to own/convert the returned buffer — we manage its lifetime
# ourselves and release it from the free callback.
RESOLVE_FN = CFUNCTYPE(c_void_p, c_void_p, c_char_p)
FREE_FN = CFUNCTYPE(None, c_void_p, c_void_p)


class SassoImporter(Structure):
    _fields_ = [
        ("user_data", c_void_p),
        ("resolve", RESOLVE_FN),
        ("free", FREE_FN),
    ]


class SassoOptions(Structure):
    _fields_ = [
        ("style", c_uint8),
        ("syntax", c_uint8),
        ("unicode", c_uint8),
        ("source_map", c_uint8),
        ("url", c_char_p),
        ("load_paths", POINTER(c_char_p)),
        ("importer", SassoImporter),
    ]


class SassoResult(Structure):
    _fields_ = [
        ("ok", c_int32),
        ("css", c_char_p),
        ("source_map", c_char_p),
        ("error_message", c_char_p),
        ("error_line", c_uint32),
        ("error_col", c_uint32),
    ]


def _load_library() -> ctypes.CDLL:
    here = os.path.dirname(os.path.abspath(__file__))
    target = os.path.join(here, "..", "target")
    names = {
        "darwin": "libsasso_ffi.dylib",
        "win32": "sasso_ffi.dll",
    }
    libname = names.get(sys.platform, "libsasso_ffi.so")
    for profile in ("release", "debug"):
        path = os.path.join(target, profile, libname)
        if os.path.exists(path):
            return ctypes.CDLL(path)
    raise FileNotFoundError(
        f"{libname} not found under {target}; build it with "
        "`cargo build --release` in the ffi/ directory first."
    )


_lib = _load_library()
_lib.sasso_version.restype = c_char_p
_lib.sasso_compile.restype = POINTER(SassoResult)
_lib.sasso_compile.argtypes = [c_char_p, POINTER(SassoOptions)]
_lib.sasso_result_free.argtypes = [POINTER(SassoResult)]


class SassError(Exception):
    def __init__(self, message: str, line: int, col: int):
        super().__init__(message)
        self.line = line
        self.col = col


def version() -> str:
    return _lib.sasso_version().decode()


def compile(
    source: str,
    *,
    compressed: bool = False,
    syntax: int = SYNTAX_SCSS,
    url: str | None = None,
    load_paths: list[str] | None = None,
    importer=None,
    source_map: bool = False,
):
    """Compile `source` to CSS. Returns the CSS string, or (css, map) when
    `source_map=True`. Raises SassError on failure. `importer`, if given, is a
    callable taking the requested URL (str) and returning SCSS source (str) or
    None."""
    opts = SassoOptions()
    opts.style = STYLE_COMPRESSED if compressed else STYLE_EXPANDED
    opts.syntax = syntax
    opts.unicode = 0
    opts.source_map = 1 if source_map else 0
    opts.url = url.encode() if url else None

    # Keep Python-side refs alive for the duration of the call.
    keepalive = []
    if load_paths:
        arr = (c_char_p * (len(load_paths) + 1))()
        for i, p in enumerate(load_paths):
            arr[i] = p.encode()
        arr[len(load_paths)] = None
        opts.load_paths = arr
        keepalive.append(arr)

    if importer is not None:
        # Each resolve allocates a NUL-terminated buffer we own; pin it by its
        # address so it survives until sasso calls free with that same address.
        cache: dict[int, ctypes.Array] = {}

        def _resolve(_user_data, path):
            src = importer(path.decode())
            if src is None:
                return None
            buf = ctypes.create_string_buffer(src.encode())  # adds the NUL
            addr = ctypes.cast(buf, c_void_p).value
            cache[addr] = buf  # pin until freed
            return addr

        def _free(_user_data, ptr):
            cache.pop(ptr, None)

        rcb, fcb = RESOLVE_FN(_resolve), FREE_FN(_free)
        opts.importer = SassoImporter(None, rcb, fcb)
        keepalive += [rcb, fcb, cache]

    res = _lib.sasso_compile(source.encode(), ctypes.byref(opts))
    if not res:
        raise SassError("sasso_compile returned NULL (null source?)", 0, 0)
    try:
        r = res.contents
        if not r.ok:
            msg = r.error_message.decode() if r.error_message else "unknown error"
            raise SassError(msg, r.error_line, r.error_col)
        css = r.css.decode() if r.css else ""
        if source_map:
            smap = r.source_map.decode() if r.source_map else None
            return css, smap
        return css
    finally:
        _lib.sasso_result_free(res)
        del keepalive  # release pins after the result (and its copies) are done


def _main() -> int:
    print("sasso version:", version())

    css = compile(".a { color: red; .b { color: blue; } }")
    print("\n-- expanded --\n" + css)
    assert ".a .b" in css

    mini = compile(".a { color: red; }", compressed=True)
    print("-- compressed --\n" + mini)
    assert mini == ".a{color:red}"

    try:
        compile(".a { color: ; }")
    except SassError as e:
        print(f"-- error (expected) -- {e} @ {e.line}:{e.col}")

    # Host importer: resolve @import from an in-memory map.
    files = {"colors": "$brand: #0d6efd;"}
    out = compile(
        '@import "colors"; .x { color: $brand; }',
        importer=lambda url: files.get(url),
    )
    print("-- importer --\n" + out)
    assert "#0d6efd" in out

    # Source map.
    css, smap = compile(".a { color: red; }", source_map=True)
    print("-- source map --\n" + (smap or "")[:80] + " ...")
    assert smap and '"version":3' in smap

    print("\nall checks passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(_main())
