/* This RTLD_LOCAL DSO has one private dependency, libother.so. */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <infiniband/verbs.h>

extern struct ibv_context *fixture_devx_context(void);

int fixture_scope_marker(void) { return 17; }

int fixture_has_scope(int next) {
    if (next) {
        struct ibv_context *(*dependency)(void) = dlsym(RTLD_NEXT, "fixture_devx_context");
        return dependency && dependency() == fixture_devx_context();
    }
    int (*local)(void) = dlsym(RTLD_DEFAULT, "fixture_scope_marker");
    return local && local() == 17;
}
