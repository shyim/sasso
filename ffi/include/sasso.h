/*
 * sasso.h — native C-ABI bindings for the sasso SCSS compiler.
 *
 * Link against libsasso_ffi (cdylib or staticlib). All strings crossing the
 * boundary are NUL-terminated UTF-8. The result of sasso_compile() and every
 * string it points at are owned by sasso; release them in one call with
 * sasso_result_free(). The library is thread-compatible: distinct threads may
 * compile concurrently as long as they do not share a sasso_result.
 *
 * SPDX-License-Identifier: MIT OR Apache-2.0
 */
#ifndef SASSO_H
#define SASSO_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Output style (sasso_options.style). */
#define SASSO_STYLE_EXPANDED   0
#define SASSO_STYLE_COMPRESSED 1

/* Input syntax (sasso_options.syntax). */
#define SASSO_SYNTAX_SCSS 0
#define SASSO_SYNTAX_SASS 1  /* the indented .sass syntax */
#define SASSO_SYNTAX_CSS  2

/*
 * A host-supplied importer. `resolve(user_data, path)` is called for each
 * @import/@use/@forward URL and must return a NUL-terminated UTF-8 source
 * buffer, or NULL if the URL cannot be found. If `free` is non-NULL, sasso
 * calls free(user_data, returned_ptr) after copying the source; if `free` is
 * NULL, sasso does not free the returned buffer (use this for pointers into a
 * long-lived table). Leave `resolve` NULL to disable (then `load_paths` is
 * used instead).
 */
typedef struct {
    void *user_data;
    const char *(*resolve)(void *user_data, const char *path);
    void (*free)(void *user_data, const char *ptr);
} sasso_importer;

/*
 * Compile options. A zero-initialized struct selects the defaults: expanded
 * SCSS, no source map, no URL, no load paths, no importer.
 */
typedef struct {
    uint8_t style;   /* one of SASSO_STYLE_*  */
    uint8_t syntax;  /* one of SASSO_SYNTAX_* */
    uint8_t unicode; /* non-zero: allow non-ASCII in output/messages */
    uint8_t source_map; /* non-zero: also emit a Source Map v3 */
    const char *url; /* input URL/filename, or NULL ("stdin") */
    const char *const *load_paths; /* NULL-terminated dir array, or NULL */
    sasso_importer importer;       /* tried before load_paths; zero to skip */
} sasso_options;

/*
 * The result of sasso_compile(). Every non-NULL char* is owned by sasso;
 * release the whole struct with sasso_result_free().
 */
typedef struct {
    int32_t ok;             /* 1 = success, 0 = failure */
    char *css;              /* compiled CSS on success, else NULL */
    char *source_map;       /* Source Map v3 JSON, or NULL */
    char *error_message;    /* error text on failure, else NULL */
    uint32_t error_line;    /* 1-based, 0 if unknown / on success */
    uint32_t error_col;     /* 1-based, 0 if unknown / on success */
} sasso_result;

/* The sasso version string ('static, never freed by the caller). */
const char *sasso_version(void);

/*
 * Override the bump-arena reservation size (bytes) before the first compile.
 * 0 disables the arena (slower, lower peak memory). No-op once a compile ran.
 */
void sasso_set_arena_bytes(size_t bytes);

/*
 * Compile `source` (NUL-terminated UTF-8) with `opts` (may be NULL for all
 * defaults). Returns a heap sasso_result, or NULL only when `source` is NULL.
 * On invalid UTF-8 input it returns a failure result. Free with
 * sasso_result_free().
 */
sasso_result *sasso_compile(const char *source, const sasso_options *opts);

/* Free a sasso_result and all of its strings. NULL is a no-op. */
void sasso_result_free(sasso_result *result);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* SASSO_H */
