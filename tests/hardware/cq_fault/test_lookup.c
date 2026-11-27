/* Exercise the installed observer through the same explicit lookup as OrbitRDMA. */
#define _GNU_SOURCE
#include <assert.h>
#include <dlfcn.h>
#include <errno.h>
#include <infiniband/verbs.h>

struct snapshot {
    uint64_t version, armed, phase, posts, lost, errors, contexts, sequence, destroyed_qps;
};

int main(int argc, char **argv) {
    assert(argc == 3);
    void *verbs = dlopen("libibverbs.so.1", RTLD_NOW | RTLD_LOCAL);
    void *other = dlopen(argv[1], RTLD_NOW | RTLD_LOCAL);
    assert(verbs && other && verbs != other);
    void *caller = dlopen(argv[2], RTLD_NOW | RTLD_LOCAL);
    assert(caller);
    int (*has_scope)(int) = dlsym(caller, "fixture_has_scope");
    assert(has_scope && has_scope(0) && has_scope(1));
    int (*snapshot)(unsigned, unsigned, unsigned, unsigned, struct snapshot *) =
        dlsym(RTLD_DEFAULT, "orbite_cq_fault");
    struct ibv_context *(*open_device)(struct ibv_device *) = dlsym(verbs, "ibv_open_device");
    struct ibv_context *(*other_open)(struct ibv_device *) = dlsym(other, "ibv_open_device");
    int (*close_device)(struct ibv_context *) = dlsym(verbs, "ibv_close_device");
    int (*destroy_qp)(struct ibv_qp *) = dlsym(verbs, "ibv_destroy_qp");
    int (*dereg_mr)(struct ibv_mr *) = dlsym(verbs, "ibv_dereg_mr");
    struct ibv_context *(*devx_context)(void) = dlsym(verbs, "fixture_devx_context");
    struct ibv_pd *(*alloc_pd)(struct ibv_context *) = dlsym(verbs, "ibv_alloc_pd");
    assert(snapshot && open_device && other_open && close_device && destroy_qp && dereg_mr);
    assert(devx_context && alloc_pd);
    dlerror();
    assert(!dlsym(verbs, "ibv_reg_mr_iova2") && dlerror());

    struct snapshot state;
    assert(other_open(NULL));
    assert(snapshot(0, 0, 0, 0, &state) == 0 && state.contexts == 0);
    struct ibv_context *ctx = open_device(NULL);
    assert(ctx && snapshot(0, 0, 0, 0, &state) == 0 && state.contexts == 1);
    struct ibv_context *devx = devx_context();
    struct ibv_pd *pd = alloc_pd(devx);
    assert(pd && pd->context == devx);
    assert(snapshot(0, 0, 0, 0, &state) == 0 && state.contexts == 2);

    struct ibv_cq cq = {.context = ctx};
    struct ibv_qp qp = {.context = ctx, .send_cq = &cq, .qp_num = 7};
    struct ibv_mr mr = {.context = ctx};
    struct ibv_wc wc;
    assert(ctx->ops.poll_cq(&cq, 1, &wc) == -EAGAIN);
    assert(destroy_qp(&qp) == 0 && dereg_mr(&mr) == EBUSY);
    assert(snapshot(0, 0, 0, 0, &state) == 0 && state.destroyed_qps == 1);
    assert(close_device(devx) == 0 && close_device(ctx) == 0);
    assert(snapshot(0, 0, 0, 0, &state) == 0 && state.contexts == 0 && !state.errors);
    assert(dlclose(caller) == 0 && dlclose(other) == 0 && dlclose(verbs) == 0);
    return 0;
}
