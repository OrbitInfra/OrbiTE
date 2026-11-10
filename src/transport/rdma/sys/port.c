#include "port.h"

#include <errno.h>

int orbite_query_port(struct ibv_context *context, uint8_t port,
                      struct orbite_port_attr *result,
                      int (*legacy)(struct ibv_context *, uint8_t,
                                    struct _compat_ibv_port_attr *)) {
    struct ibv_port_attr attr = {0};
    /* Same extended-provider/legacy split as verbs.h's ___ibv_query_port.
       The legacy symbol is supplied by the retained runtime table. */
    struct verbs_context *vctx = verbs_get_ctx_op(context, query_port);
    int code = vctx ? vctx->query_port(context, port, &attr, sizeof(attr))
                    : (legacy ? legacy(context, port, (struct _compat_ibv_port_attr *)&attr)
                              : ENOSYS);
    if (code != 0)
        return code;

    uint32_t speed = attr.active_speed;
#ifdef ORBITE_HAVE_ACTIVE_SPEED_EX
    if (attr.active_speed_ex != 0)
        speed = attr.active_speed_ex;
#endif
    *result = (struct orbite_port_attr){
        .state = attr.state,
        .active_mtu = attr.active_mtu,
        .gid_tbl_len = attr.gid_tbl_len,
        .active_speed = speed,
        .lid = attr.lid,
        .link_layer = attr.link_layer,
        .active_width = attr.active_width,
    };
    return 0;
}

#ifdef ORBITE_GPU_KEYS
#include <errno.h>
#if __has_include(<infiniband/mlx5dv.h>)
#include <infiniband/mlx5dv.h>
int orbite_mlx5_pdn(int (*init)(struct mlx5dv_obj *, uint64_t), void *pd, uint32_t *pdn) {
    struct mlx5dv_pd result = {0};
    struct mlx5dv_obj object = {.pd = {.in = pd, .out = &result}};
    int code = init(&object, MLX5DV_OBJ_PD);
    if (!code) *pdn = result.pdn;
    return code;
}
#else
int orbite_mlx5_pdn(int (*init)(void *, uint64_t), void *pd, uint32_t *pdn) {
    (void)init; (void)pd; (void)pdn;
    return EOPNOTSUPP;
}
#endif
#endif
