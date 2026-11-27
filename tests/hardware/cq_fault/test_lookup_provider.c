/* CPU fixture loaded by handle, without a link-time verbs dependency. */
#include <infiniband/verbs.h>
#include <errno.h>

static int poll_cq(struct ibv_cq *cq, int capacity, struct ibv_wc *wc) {
    (void)cq; (void)capacity; (void)wc;
    return -EAGAIN;
}

static int post_send(struct ibv_qp *qp, struct ibv_send_wr *wr, struct ibv_send_wr **bad) {
    (void)qp; (void)wr; (void)bad;
    return ENOSPC;
}

static struct ibv_context context = {.ops = {.poll_cq = poll_cq, .post_send = post_send}};
static struct ibv_context devx = {.ops = {.poll_cq = poll_cq, .post_send = post_send}};

struct ibv_context *fixture_devx_context(void) { return &devx; }

struct ibv_context *ibv_open_device(struct ibv_device *device) {
    (void)device;
    return &context;
}

struct ibv_pd *ibv_alloc_pd(struct ibv_context *ctx) {
    static struct ibv_pd pd;
    pd.context = ctx;
    return &pd;
}

int ibv_close_device(struct ibv_context *ctx) {
    return ctx->ops.poll_cq == poll_cq && ctx->ops.post_send == post_send ? 0 : EINVAL;
}

int ibv_destroy_qp(struct ibv_qp *qp) { return qp->qp_num == 7 ? 0 : EINVAL; }
int ibv_dereg_mr(struct ibv_mr *mr) { return mr->context == &context ? EBUSY : EINVAL; }
int ibv_query_qp(struct ibv_qp *qp, struct ibv_qp_attr *attr, int mask,
                 struct ibv_qp_init_attr *init) {
    (void)qp; (void)attr; (void)mask; (void)init;
    return ENOSYS;
}
