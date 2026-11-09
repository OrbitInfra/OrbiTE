# Local CUDA copies

Use `orbite::cuda::local_copy` to move retained layer ranges between a GPU
and pinned host memory. Enable the Linux `cuda` feature. One worker binds to
one caller-owned CUDA stream. Submit several descriptors in either direction
and keep the completion until the bytes can be reused. The caller chooses the
blocks, layer layout and cache tier. OrbiTE owns the copies and their receipts.

## Bind and submit

In this example `gpu` and `host` are `RegisteredMemory` views, and `stream` is
the consumer's `Arc<cudarc::driver::CudaStream>`. Bind once for the lifetime
of the worker. For a default stream, make its context current before binding.
A per-thread default stream also requires submission on the originating thread.

```rust
use orbite::cuda::local_copy::{
    CopyDirection, LocalCopyConfig, LocalCopyDesc, LocalCopyEngine, LocalCopyOptions,
};
use orbite::{IntentType, Priority};

// SAFETY: stream owns this noncapturing stream in gpu's context. The caller
// coordinates its other GPU work and retains each external allocation.
let worker = unsafe {
    LocalCopyEngine::bind(&gpu, stream.cu_stream().cast(), stream.clone(),
                          LocalCopyConfig::default())?
};
let layers = [
    LocalCopyDesc::new(&gpu, 128, &host, 64, 32),
    LocalCopyDesc::new(&gpu, 512, &host, 128, 16).strided(4, 64, 32),
];
let mut options = LocalCopyOptions::default();
options.intent = IntentType::ForegroundGet;
options.priority = Priority::High;
let done = worker.submit(CopyDirection::HostToDevice, &layers, options)?;
// Further work may be queued on the same stream immediately.
done.wait(std::time::Duration::from_secs(5))?;
```

Use `CopyDirection::DeviceToHost` to copy back. Offsets are relative to their
registered views. Each explicit row must fit both views. `strided` takes the
row count, device pitch and host pitch in bytes. Empty batches still get a
stream marker. Allocation-level producer events become `cuStreamWaitEvent`
dependencies. To pass a caller-recorded event, construct
`unsafe { ProducerEvent::from_raw(event, owner)? }` and set
`options.ready = Some(ready)`.
The owner keeps the event generation and producer inputs alive. Stream capture
is rejected. Submission restores the exact previous context, including an
empty binding.

## Register an arena

Register one retained `CudaMemory` per GPU allocation. Use
`cuda::allocate_pinned_host` for engine-owned host memory. For a consumer-owned
CUDA host arena, create one `PinnedHostRegistration` per physical pin, then
wrap each disjoint logical slot using its own allocator lease. A pin owner must
keep the physical registration live. A slot owner must prevent its allocator
from recycling that slot. Share the same `Arc<HostMemory>` for overlapping
views. Both constructors are unsafe because only the allocator can promise
these lifetimes and exclude external writes.

```rust
use orbite::{HostMemory, Permission};
use orbite::cuda::local_copy::PinnedHostRegistration;
use std::sync::Arc;

// SAFETY: these addresses belong to the stated physical pin and live slot.
let pin = unsafe { PinnedHostRegistration::from_raw_parts(arena_base, arena_len, arena_owner)? };
let slot = Arc::new(unsafe {
    HostMemory::from_cuda_registered_parts(slot_base, slot_len, slot_owner, pin.clone())?
});
let host = engine.register_host_memory_batch(&[(slot, 0, slot_len)], Permission::LocalOnly)?
    .remove(0);
```

## DMA and kernel selection

DMA preserves descriptor order. Contiguous source and destination ranges can
become a single 1D copy within one device allocation and physical host pin.
Equal-width rows with constant pitches within the device pitch limit can
become one `cuMemcpy2DAsync`. Gaps stay untouched. For independent 1D copies
on a nonlegacy stream, the driver can use the CUDA 13 batch entry or the
CUDA 12.8 entry with its failure-index argument. Missing optional entries
leave the directional asynchronous calls available. Overlapping destinations
use ordered DMA. Forced kernel submission rejects such a batch.

`LocalCopyConfig::default()` selects `Auto`. Bind compiles the typed direction
and intent policy once; submit performs no configuration parsing. Thresholds
count DMA operations **after** contiguous/strided coalescing, not input rows.

| Submission | Auto kernel eligibility |
| --- | --- |
| HostToDevice, any intent | At least `kernel_threshold` compiled operations |
| DeviceToHost, `Checkpoint`, `Migration`, `BackgroundPrefetch` | At least `kernel_threshold_background` compiled operations |
| DeviceToHost, other intents (including `ForegroundGet`) | At least `kernel_threshold` compiled operations |

The normal threshold remains 128. The background threshold must be strictly
higher; its default `usize::MAX` keeps background write-back on DMA until a
consumer explicitly chooses a calibrated finite value. Default options are
`IntentType::Unspec`, `Priority::High`, with no explicit producer event, so
existing default selection is unchanged. Priority labels the submission for
diagnostics; it does not reorder or reprioritize the caller's CUDA stream.
Forced `Dma` and `Kernel` retain their existing behavior, including rejection
of overlapping destinations for a forced kernel. Auto always uses ordered DMA
for overlap, and can choose DMA before upload if the image, mapped alias or
descriptor budget is unavailable. A failed enqueue never triggers a replay.

The kernel resolves the actual device alias for each physical
host pin, uploads one descriptor array and launches a grid-strided copy with
an aligned vector path and byte tails. The crate ships embedded PTX generated
from the included CUDA source. The driver loads the module during bind.
Consumers need neither a CUDA compiler nor NVRTC.

## Budgets and retirement

The defaults allow 4,096 expanded rows per submission, four in-flight
submissions and 256 MiB per worker. The byte limit covers the sum of live
payload lengths plus the kernel's reserved descriptor storage. Each kernel
slot reserves 24 bytes per possible row on each side of the upload. Each
occupied slot owns one completion event and all of its source and destination
loans. Descriptor and slot limits also bound CPU planning memory. Exhaustion
returns `Error::ResourceExhausted`.

`kernel_cta_limit: Option<NonZeroU32>` caps the total number of blocks in each
kernel launch. The name describes the controlled quantity: it is not an SM
reservation or a guarantee of a particular occupancy. Bind rejects a cap above
the smaller of 65,535 and the device's
[`MAX_GRID_DIM_X`](https://docs.nvidia.com/cuda/cuda-driver-api/cuda_driver_api/group__CUDA__TYPES.html)
before loading a module or allocating descriptor slots. `None` preserves the
existing four-times-SM budget and per-descriptor clamp exactly. A finite cap
also limits that budget and the final grid, including when it is smaller than
the descriptor count. The unchanged grid-strided kernel still covers every
descriptor, vector and byte tail. For example, set
`config.kernel_cta_limit = std::num::NonZeroU32::new(4)` before bind.

`poll()` queries the completion. `wait(timeout)` bounds the CPU wait.
Dropping a handle leaves its slot owned by the worker. Subsequent submission
or `progress()` polls abandoned slots. `close(timeout)` stops admission and
drains, and can be called again after a timeout. A failed payload enqueue may
already have scheduled a subset. Its marker must complete before the handle
reports the enqueue error. An uncertain record, query, destroy or context
restoration returns `Error::Unquiesced` from the completion, progress and
subsequent submission on this stream. Other pending markers can still drain.
`stats()` inspects the charges without calling CUDA. Its `classes` snapshot
contains cumulative DMA/kernel decisions, successful batch-DMA calls and
launched CTAs for each observed direction × intent × priority (at most 42
classes). `last_backend`, `last_kernel_ctas` and `last_fallback` describe that
class's latest admitted submission. Auto fallback reasons distinguish the
threshold, background intent, overlap, missing image, missing mapping and
descriptor budget. Launch counters are not completion receipts. These fixed
counters survive retirement and close; recording them allocates no memory.
The worker retains its
loans, stream, scratch and charges. Final drop bounds its drain and retains
unconfirmed owners for process lifetime.

The installed comparison checks both directions against OrbitKV on the same
GPU. The calibration sweep also measures DMA, batch DMA and several CTA caps
with a fixed compute kernel on another nonblocking stream. Run it separately
on PCIe H100/H20 and Grace C2C GH200/GB200; CPU shims cannot calibrate the
trade-off. Candidate thresholds and caps are evidence for a later review,
not an automatic configuration change. The existing 128-operation threshold
and uncapped default stay unchanged until those results exist. The local copy
profile is **Implemented; installed profile unqualified**.
[Verification queue](qualification.md#caller-stream-local-cuda-copy)
