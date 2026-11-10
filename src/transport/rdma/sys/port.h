#ifndef ORBITE_RDMA_PORT_H
#define ORBITE_RDMA_PORT_H

#include <stdint.h>
#include <infiniband/verbs.h>

/* Only these values cross the shim boundary; provider structs stay in C. */
struct orbite_port_attr {
    uint32_t state;
    uint32_t active_mtu;
    int32_t gid_tbl_len;
    uint32_t active_speed;
    uint16_t lid;
    uint8_t link_layer;
    uint8_t active_width;
};

int orbite_query_port(struct ibv_context *context, uint8_t port,
                      struct orbite_port_attr *result,
                      int (*legacy)(struct ibv_context *, uint8_t,
                                    struct _compat_ibv_port_attr *));

#endif
