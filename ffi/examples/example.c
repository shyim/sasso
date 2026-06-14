/*
 * Minimal C example / smoke test for the sasso C ABI.
 *
 * Build (after `cargo build --release` in ffi/):
 *   cc -I include examples/example.c target/release/libsasso_ffi.a \
 *      -o /tmp/sasso_example -lpthread -ldl -lm        # Linux
 *   cc -I include examples/example.c target/release/libsasso_ffi.a \
 *      -o /tmp/sasso_example -framework CoreFoundation # macOS
 *
 * Then run /tmp/sasso_example. Exit code 0 means all checks passed.
 */
#include <assert.h>
#include <stdio.h>
#include <string.h>

#include "sasso.h"

/* A host importer backed by a tiny in-memory file table. */
static const char *resolve(void *user_data, const char *path) {
    (void)user_data;
    if (strcmp(path, "colors") == 0) {
        return "$brand: #0d6efd;";
    }
    return NULL; /* not found */
}

int main(void) {
    printf("sasso version: %s\n", sasso_version());

    /* 1. Expanded compile with nesting. */
    sasso_result *r = sasso_compile(".a { color: red; .b { color: blue; } }", NULL);
    assert(r && r->ok);
    printf("-- expanded --\n%s\n", r->css);
    assert(strstr(r->css, ".a .b") != NULL);
    sasso_result_free(r);

    /* 2. Compressed. */
    sasso_options opts = {0};
    opts.style = SASSO_STYLE_COMPRESSED;
    r = sasso_compile(".a { color: red; }", &opts);
    assert(r && r->ok);
    printf("-- compressed --\n%s\n", r->css);
    assert(strcmp(r->css, ".a{color:red}") == 0);
    sasso_result_free(r);

    /* 3. Error with line/col. */
    r = sasso_compile(".a { color: ; }", NULL);
    assert(r && !r->ok && r->error_message);
    printf("-- error (expected) -- %s (%u:%u)\n",
           r->error_message, r->error_line, r->error_col);
    sasso_result_free(r);

    /* 4. Host importer. */
    sasso_options imp = {0};
    imp.importer.resolve = resolve;
    r = sasso_compile("@import \"colors\"; .x { color: $brand; }", &imp);
    assert(r && r->ok);
    printf("-- importer --\n%s\n", r->css);
    assert(strstr(r->css, "#0d6efd") != NULL);
    sasso_result_free(r);

    /* 5. Source map. */
    sasso_options sm = {0};
    sm.source_map = 1;
    r = sasso_compile(".a { color: red; }", &sm);
    assert(r && r->ok && r->source_map);
    assert(strstr(r->source_map, "\"version\":3") != NULL);
    sasso_result_free(r);

    printf("\nall checks passed\n");
    return 0;
}
