# Troubleshooting

Start with the [capability matrix](capabilities.md) and the path you selected.
Build availability, device discovery, request completion and physical drain
are separate checks.

## The crates.io package does not expose the documented API

The published `0.1.0-alpha.1` is the original package reservation. Build the
current repository source for the APIs in this documentation. The
[Quick Start](getting-started.md) uses a source example.

## RDMA bindings fail to compile

Reproduce with Ubuntu 24.04, GCC/G++ and Clang/libclang 18, matching CI.
Confirm that `LIBCLANG_PATH` resolves the selected libclang and that an inherited
`CLANG_PATH` does not select a conflicting installation. The build needs a C
compiler, pkg-config and libibverbs headers. See the [build guide](build.md).

## libibverbs not found

`Unavailable("libibverbs.so.1: …")` means the configured RDMA install could not
load the host library or a required export. Install the host's rdma-core runtime
and its matching RNIC provider, and check the dynamic loader search path. Do not
copy a wheel's vendored provider onto the host. RDMA is loaded only on explicit
configuration/discovery; importing a combined wheel and using TCP or io_uring
needs no verbs runtime. CUDA and explicit GDS failures similarly name
`libcuda.so.1` and `libcufile.so.0`.

## No RDMA device or compatible path is selected

Check provider visibility and read/write access to the matching uverbs
character node. Confirm the device's port is active and the selected GID is
valid for the intended link layer, GID type and network interface.

An explicit device/port/GID pin is strict. `RdmaConfig::new` selects port 1 and
GID 0 explicitly; use `Default` and optional selectors for discovery. A nonempty
whitelist takes precedence over the blacklist and still intersects a device
pin. `device_mask` uses topology NIC IDs, including inert slots after failed
construction. Host Direct registration itself initializes contexts; inspect
`TransferEngine::topology()` after registration or first RDMA use.

Matching GID families or NIC names does not prove that two RDMA ports can
connect. The current implementation does not replay a failed transfer through
another rail.

## Host registration returns a native error or ResourceExhausted

Host Direct registration prepares MRs on all usable contexts, including paused
ports, before publishing the range. A successful registration does not mean a
route is ready: inspect selected topology metadata and wait for port recovery.
Check the original device error and count physical copies: one MR
per context for local-only ranges, two per context for remotely accessible
ranges. Both limits charge the full view for each copy. Published registrations
cannot be evicted to fit a new range; retire unused registrations or adjust the
aggregate count/byte budgets. A dispatch mask does not reduce publication fan-out.
Registration errors do not automatically switch to staging.

## A peer is rejected after restart or protocol change

Use the source owner's current authoritative epoch. Reopening the same TCP
address with an old epoch cannot adopt a replacement source. Both peers must
follow the [wire and consumer boundary](tent-parity.md#wire-and-consumer-boundaries).
A protocol mismatch is rejected before admission.

## A request times out or returns an uncertain WRITE

Inspect completed-prefix counts, failure information, remote effect and local
drain status. An uncertain WRITE may have changed remote bytes. Logical
cancellation and timeout do not establish physical DMA completion or rollback.
Keep application ownership consistent with the [transfer contract](cpu-tcp.md)
and [RDMA drain requirements](rdma-cuda.md).

## Native cleanup returns Unquiesced

The actual native owners and memory remain retained with their charges. New
admission is fenced; dropping a handle cannot establish a successful native
destroy or safely reset the PD's key history. Preserve the source/artifact,
device identity, error and cleanup evidence for diagnosis.

## CUDA allocation works but direct GPU RDMA does not

Check `rdma.gpu_direct_registration` and the [registration-route prerequisites](rdma-cuda.md#gpu-direct-registration-routes).
Dma-buf needs CUDA device support, its export symbol and the optional
`ibv_reg_dmabuf_mr` symbol. Peermem needs one of these version paths:
`/sys/kernel/mm/memory_peers/nv_mem/version`,
`/sys/kernel/mm/memory_peers/nv_mem_nc/version` or
`/sys/module/nvidia_peermem/version`. A module directory alone is insufficient;
both MLNX_OFED `nv_peer_mem` and NVIDIA `nvidia_peermem` are recognized.
`auto` selects an available route before registration; an export/native error
never falls back within that registration. Use the debug route diagnostic,
Describe evidence and RDMA route counters. Explicit Direct errors do not
silently select staging; choose the staged memory policy when needed. A
successful CUDA compute probe does not qualify GPUDirect or its mapping route.
