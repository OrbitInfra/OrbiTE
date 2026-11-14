//! Stream-ordered local copies between registered CUDA and pinned host ranges.
use super::{Context, driver};
use crate::{Error, IntentType, Priority, RegisteredMemory, Result};
use cudarc::driver::sys;
use driver::check;
use std::ffi::c_void;
use std::num::NonZeroU32;
use std::ptr::NonNull;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

mod dma;
mod flight;
mod kernel;
mod plan;
mod policy;

use flight::{Inner, Pool};

/// The exact CUDA host registration shared by one or more retained allocator slots.
pub struct PinnedHostRegistration {
    base: u64,
    length: usize,
    _owner: Arc<dyn Send + Sync>,
}
impl PinnedHostRegistration {
    /// Retain the identity and owner of an existing CUDA host registration.
    ///
    /// # Errors
    /// Returns [`Error::InvalidRange`] for an empty, oversized or overflowing range.
    /// This constructor does not query CUDA to verify the caller's registration.
    ///
    /// # Safety
    /// `base..base+length` must be one live CUDA host registration. The owner must
    /// keep it registered and stationary. All its suballocations must share this
    /// identity and coordinate external accesses with their OrbiTE range loans.
    pub unsafe fn from_raw_parts(
        base: NonNull<u8>,
        length: usize,
        owner: Arc<dyn Send + Sync>,
    ) -> Result<Arc<Self>> {
        let base = base.as_ptr() as usize as u64;
        if length == 0 || length > isize::MAX as usize || base.checked_add(length as u64).is_none()
        {
            return Err(Error::InvalidRange);
        }
        Ok(Arc::new(Self {
            base,
            length,
            _owner: owner,
        }))
    }
    pub(crate) fn base(&self) -> u64 {
        self.base
    }
    pub(crate) fn offset(&self, address: u64, length: usize) -> Result<usize> {
        let off = address
            .checked_sub(self.base)
            .and_then(|v| usize::try_from(v).ok())
            .ok_or(Error::InvalidRange)?;
        crate::memory::checked_range(off, length, self.length)?;
        Ok(off)
    }
}

/// Payload direction between a descriptor's pinned host and device views.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum CopyDirection {
    /// Copy pinned host bytes into the device view.
    HostToDevice,
    /// Copy device bytes into the pinned host view.
    DeviceToHost,
}
impl CopyDirection {
    fn ends(self, device: u64, host: u64) -> (u64, u64) {
        match self {
            Self::HostToDevice => (device, host),
            Self::DeviceToHost => (host, device),
        }
    }
}
/// Local copy selection; the caller's stream order is preserved by every backend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum LocalCopyBackend {
    /// Select DMA or the mapped-host kernel from validated geometry and policy.
    Auto,
    /// Require CUDA copy operations, including supported strided or batched copies.
    Dma,
    /// Require the mapped-host kernel; unavailable prerequisites fail explicitly.
    Kernel,
}

/// Per-stream copy budgets and backend policy, validated at [`LocalCopyEngine::bind`].
/// Defaults allow 4096 descriptors, four flights and 256 MiB of charged bytes.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct LocalCopyConfig {
    /// Selection mode; defaults to [`LocalCopyBackend::Auto`].
    pub backend: LocalCopyBackend,
    /// Minimum number of DMA operations after coalescing; positive, defaults to 128.
    pub kernel_threshold: usize,
    /// Higher Auto threshold for background device-to-host copies, in compiled
    /// DMA operations. `usize::MAX` keeps these copies on DMA pending calibration.
    pub kernel_threshold_background: usize,
    /// Maximum blocks in one kernel launch, not an SM reservation. `None` keeps
    /// the existing device-derived budget. Checked against the device at bind.
    pub kernel_cta_limit: Option<NonZeroU32>,
    /// Bounds input descriptors and expanded rows; 1..=65536, defaults to 4096.
    pub max_descriptors: usize,
    /// Simultaneously retained flight slots; 1..=64, defaults to four.
    pub max_inflight: usize,
    /// Total payload bytes plus reserved device and pinned kernel descriptor slots.
    pub max_bytes: usize,
}
impl Default for LocalCopyConfig {
    fn default() -> Self {
        Self {
            backend: LocalCopyBackend::Auto,
            kernel_threshold: 128,
            kernel_threshold_background: usize::MAX,
            kernel_cta_limit: None,
            max_descriptors: 4096,
            max_inflight: 4,
            max_bytes: 256 << 20,
        }
    }
}

/// Per-submission scheduling metadata and an optional producer generation.
/// Priority is advisory on a caller-owned stream: submissions are never reordered.
#[derive(Clone, Default)]
#[non_exhaustive]
pub struct LocalCopyOptions {
    /// Workload intent used by Auto; defaults to [`IntentType::Unspec`].
    pub intent: IntentType,
    /// Advisory class for policy and counters; does not reorder the caller stream.
    pub priority: Priority,
    /// Optional already-recorded producer event retained through this copy's receipt.
    pub ready: Option<ProducerEvent>,
}

/// Why Auto used DMA instead of a mapped-memory kernel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum LocalCopyFallback {
    /// Coalesced work is below the applicable kernel threshold.
    Threshold,
    /// Work is below the higher device-to-host threshold for this background intent.
    Intent,
    /// Overlapping descriptor ranges prevent parallel kernel copying.
    Overlap,
    /// The kernel image or required driver capabilities are unavailable.
    ImageUnavailable,
    /// A host range cannot supply the required device mapping.
    MappingUnavailable,
    /// Reserving kernel descriptor storage would exhaust the configured byte budget.
    DescriptorBudget,
}

/// Cumulative decisions for one direction, intent and priority. Failed preflight
/// is not a submission. Counts include enqueues whose event is still pending.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct LocalCopyClassStats {
    /// Host/device payload direction of this counter class.
    pub direction: CopyDirection,
    /// Intent supplied by the caller for this class.
    pub intent: IntentType,
    /// Advisory priority supplied by the caller for this class.
    pub priority: Priority,
    /// Submissions that selected DMA; not a count of completed copies.
    pub dma_submissions: u64,
    /// Native batch-DMA enqueue calls that returned success.
    /// Like kernel launches, these are not physical completion receipts.
    pub dma_batch_calls: u64,
    /// Submissions that selected the mapped-host kernel; not completion receipts.
    pub kernel_submissions: u64,
    /// Sum of blocks from successful native kernel launches (not completed work).
    pub kernel_ctas: u64,
    /// Blocks launched by the latest submission; zero if none launched successfully.
    pub last_kernel_ctas: u32,
    /// Backend chosen for the latest admitted submission in this class.
    pub last_backend: LocalCopyBackend,
    /// Reason the latest Auto decision used DMA, if applicable.
    pub last_fallback: Option<LocalCopyFallback>,
}

/// Retained device and pinned-host views plus byte geometry for a local copy.
/// Construction clones the registration handles; submission validates bounds,
/// access conflicts, context, overlap and the backing's host-registration identity.
#[derive(Clone, Debug)]
pub struct LocalCopyDesc {
    device: RegisteredMemory,
    device_offset: usize,
    host: RegisteredMemory,
    host_offset: usize,
    width: usize,
    rows: usize,
    device_pitch: usize,
    host_pitch: usize,
}
impl LocalCopyDesc {
    /// Describe one `width`-byte row at the two byte offsets, without submitting it.
    /// Geometry is checked by [`LocalCopyEngine::submit`], not this constructor.
    pub fn new(
        device: &RegisteredMemory,
        device_offset: usize,
        host: &RegisteredMemory,
        host_offset: usize,
        width: usize,
    ) -> Self {
        Self {
            device: device.clone(),
            device_offset,
            host: host.clone(),
            host_offset,
            width,
            rows: 1,
            device_pitch: width,
            host_pitch: width,
        }
    }
    /// Repeat this row with independent byte pitches in the same two views.
    #[must_use]
    pub fn strided(mut self, rows: usize, device_pitch: usize, host_pitch: usize) -> Self {
        self.rows = rows;
        self.device_pitch = device_pitch;
        self.host_pitch = host_pitch;
        self
    }
}

/// A caller-recorded CUDA event generation. Submission waits on the GPU and
/// keeps the supplied owner until the copy's own completion event retires.
#[derive(Clone)]
pub struct ProducerEvent {
    raw: usize,
    _owner: Arc<dyn Send + Sync>,
}
impl ProducerEvent {
    /// Retain one already-recorded producer generation without synchronizing it.
    ///
    /// # Errors
    /// Returns [`Error::InvalidConfig`] for a null event handle.
    ///
    /// # Safety
    /// The event must be live, already recorded, and represent the intended
    /// producer generation. The owner retains the event and producer inputs.
    /// Do not re-record or destroy it while a submission may still bind its wait.
    pub unsafe fn from_raw(event: *mut c_void, owner: Arc<dyn Send + Sync>) -> Result<Self> {
        if event.is_null() {
            return Err(Error::InvalidConfig("null CUDA producer event"));
        }
        Ok(Self {
            raw: event as usize,
            _owner: owner,
        })
    }
}

/// Bounded copy owner bound to one retained caller stream and CUDA context.
/// Completion events retain loans and scratch independently of handle lifetime.
/// Use [`Self::close`] to observe drain and cleanup errors before dropping it.
pub struct LocalCopyEngine {
    inner: Arc<Inner>,
}

/// Copy-owner charges and cumulative selection counters; no synchronization implied.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct LocalCopyStats {
    /// Occupied flight slots, including quarantined submissions.
    pub active: usize,
    /// Occupied slots whose cleanup/completion is uncertain and retained permanently.
    pub retained: usize,
    /// Payload bytes charged to occupied flight slots, including retained slots.
    pub payload_bytes: usize,
    /// Reserved kernel descriptor bytes in device and pinned host storage.
    pub descriptor_bytes: usize,
    /// Only classes with a submitted decision, bounded by direction × intent × priority.
    /// These counters survive retirement and close; inspecting them makes no CUDA calls.
    pub classes: Vec<LocalCopyClassStats>,
}

impl LocalCopyEngine {
    /// Bind one bounded copy owner to a caller stream. Kernel scratch is reserved here,
    /// outside submission. The device registration supplies the exact execution context.
    ///
    /// # Errors
    /// Returns configuration, context/capture, driver or scratch-budget errors.
    /// Explicit kernel mode fails if its prerequisites are unavailable; Auto may
    /// select DMA. Failed native cleanup returns [`Error::Unquiesced`].
    ///
    /// # Safety
    /// The stream must be valid in the device registration's context, and `owner`
    /// must keep it and its context alive. Coordinate stream capture, destruction,
    /// producer work and access by external code with OrbiTE. A per-thread default
    /// stream must be bound and submitted on its originating thread.
    pub unsafe fn bind(
        device: &RegisteredMemory,
        stream: *mut c_void,
        owner: Arc<dyn Send + Sync>,
        config: LocalCopyConfig,
    ) -> Result<Self> {
        if !(1..=65536).contains(&config.max_descriptors)
            || !(1..=64).contains(&config.max_inflight)
            || config.kernel_threshold == 0
            || config.max_bytes == 0
        {
            return Err(Error::InvalidConfig(
                "invalid local copy descriptor, slot, threshold or byte limit",
            ));
        }
        if config.kernel_threshold_background <= config.kernel_threshold {
            return Err(Error::InvalidConfig(
                "kernel_threshold_background must exceed kernel_threshold",
            ));
        }
        let (gpu, _) = device.cuda_range(0, device.len())?;
        let ctx = gpu
            .allocation
            .lock()
            .map_err(|_| Error::Internal("CUDA allocation lock"))?
            .context
            .clone();
        // Special handles are relative to the caller's current context.
        let mut actual = std::ptr::null_mut();
        // SAFETY: the caller retains a valid stream; resolve it before pushing a context.
        check("cuStreamGetCtx local copy", unsafe {
            (ctx.driver.stream_context)(stream.cast(), &mut actual)
        })?;
        if actual != ctx.raw.as_ptr() {
            return Err(Error::Unavailable(
                "caller stream belongs to another CUDA context",
            ));
        }
        let pitch = ctx.current(|| {
            check_capture(&ctx, stream.cast())?;
            if let Some(limit) = config.kernel_cta_limit {
                let maximum = attribute(
                    &ctx,
                    sys::CUdevice_attribute::CU_DEVICE_ATTRIBUTE_MAX_GRID_DIM_X,
                )?;
                if limit.get() as usize > maximum.min(65535) {
                    return Err(Error::InvalidConfig(
                        "local copy CTA limit exceeds launch grid limit",
                    ));
                }
            }
            if ctx.driver.local.copy_2d.is_some() && ctx.driver.local.attribute.is_some() {
                attribute(&ctx, sys::CUdevice_attribute::CU_DEVICE_ATTRIBUTE_MAX_PITCH)
            } else {
                Ok(0)
            }
        })?;
        let reserve = config.max_descriptors * config.max_inflight * 48;
        let want_kernel = config.backend == LocalCopyBackend::Kernel
            || (config.backend == LocalCopyBackend::Auto
                && config.kernel_threshold <= config.max_descriptors);
        let (kernel, kernel_unavailable) = if !want_kernel {
            (None, LocalCopyFallback::Threshold)
        } else if reserve >= config.max_bytes {
            if config.backend == LocalCopyBackend::Kernel {
                return Err(Error::ResourceExhausted);
            }
            (None, LocalCopyFallback::DescriptorBudget)
        } else {
            match kernel::Kernel::install(&ctx, &config) {
                Ok(k) => (Some(k), LocalCopyFallback::ImageUnavailable),
                Err(Error::Unavailable(reason)) if config.backend == LocalCopyBackend::Auto => {
                    log::debug!("local CUDA DMA selected: {reason}");
                    (None, LocalCopyFallback::ImageUnavailable)
                }
                Err(e) => return Err(e),
            }
        };
        let policy = policy::Policy::new(config, kernel_unavailable);
        let pool = Pool::new(ctx, stream as usize, owner, config, pitch, kernel, policy);
        Ok(Self {
            inner: Arc::new(Inner(Mutex::new(pool))),
        })
    }

    /// Enqueue in caller stream order. A partial enqueue error is reported by
    /// the completion after its marker. A missing marker returns Unquiesced.
    /// The stream is not synchronized; the returned completion owns the receipt.
    ///
    /// # Errors
    /// Returns admission/geometry/access errors before enqueue, or native errors.
    /// [`Error::Unquiesced`] retains all potentially accessed owners and charges.
    pub fn submit(
        &self,
        direction: CopyDirection,
        descs: &[LocalCopyDesc],
        options: LocalCopyOptions,
    ) -> Result<LocalCopyCompletion> {
        let mut pool = self
            .inner
            .0
            .lock()
            .map_err(|_| Error::Internal("local copy pool lock"))?;
        let (index, signal, backend) = pool.submit(direction, descs, options)?;
        Ok(LocalCopyCompletion {
            inner: self.inner.clone(),
            index,
            signal,
            backend,
        })
    }
    /// Poll abandoned submissions and return current ownership charges.
    ///
    /// # Errors
    /// Returns a quarantined-flight, cleanup or poisoned-lock error. Ordinary
    /// copy failures after successful retirement are reported by their individual
    /// completions, not aggregated here. Uncertain flights remain retained.
    pub fn progress(&self) -> Result<LocalCopyStats> {
        let mut pool = self
            .inner
            .0
            .lock()
            .map_err(|_| Error::Internal("local copy pool lock"))?;
        pool.progress()?;
        Ok(pool.stats())
    }
    /// Inspect live and quarantined charges without advancing a CUDA event.
    ///
    /// # Errors
    /// Returns [`Error::Internal`] if the pool lock is poisoned.
    pub fn stats(&self) -> Result<LocalCopyStats> {
        let pool = self
            .inner
            .0
            .lock()
            .map_err(|_| Error::Internal("local copy pool lock"))?;
        Ok(pool.stats())
    }
    /// Stop admission and wait up to the timeout for event receipts. A timeout
    /// retains all active loans. Teardown can be called again after a timeout.
    ///
    /// # Errors
    /// Returns [`Error::Timeout`] while receipts are pending, or native cleanup
    /// errors. [`Error::Unquiesced`] is sticky and never permits native destroy retry.
    pub fn close(&self, timeout: Duration) -> Result<()> {
        self.inner.close(timeout)
    }
}

/// Receipt for one local copy and its retained registrations, event and producer.
/// Dropping this handle leaves pending work with the copy owner; it is not a cancel
/// or release receipt. Only successful observation authorizes normal byte reuse.
#[must_use = "observe the completion before reusing payload bytes"]
pub struct LocalCopyCompletion {
    inner: Arc<Inner>,
    index: usize,
    signal: Arc<flight::Signal>,
    backend: LocalCopyBackend,
}
impl LocalCopyCompletion {
    /// Backend selected for this submission; observing it does not poll the event.
    pub fn backend(&self) -> LocalCopyBackend {
        self.backend
    }
    /// Query only the event recorded immediately after this submission.
    /// Returns `false` while pending and `true` after successful physical retirement.
    ///
    /// # Errors
    /// Returns a submission/completion error once observed; uncertain native state
    /// returns [`Error::Unquiesced`] and retains owners. A failed result is not a
    /// general permission to replay a partially submitted copy.
    pub fn poll(&self) -> Result<bool> {
        let mut pool = self
            .inner
            .0
            .lock()
            .map_err(|_| Error::Internal("local copy pool lock"))?;
        if self.signal.status()?.is_none() {
            pool.poll(self.index);
        }
        self.signal.status().map(|s| s.is_some())
    }
    /// Poll this receipt until completion or the bounded timeout, without synchronizing
    /// unrelated work on the caller stream.
    ///
    /// # Errors
    /// Returns [`Error::Timeout`] without releasing pending owners, a deadline
    /// overflow error, or the errors from [`Self::poll`].
    pub fn wait(&self, timeout: Duration) -> Result<()> {
        wait_for(timeout, || self.poll())
    }
}

fn wait_for(timeout: Duration, mut step: impl FnMut() -> Result<bool>) -> Result<()> {
    let end = Instant::now()
        .checked_add(timeout)
        .ok_or(Error::InvalidConfig("local copy deadline overflow"))?;
    loop {
        if step()? {
            return Ok(());
        }
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(Error::Timeout);
        }
        std::thread::sleep(left.min(Duration::from_micros(200)));
    }
}
fn attribute(ctx: &Context, key: sys::CUdevice_attribute) -> Result<usize> {
    let f = ctx
        .driver
        .local
        .attribute
        .ok_or(Error::Unavailable("cuDeviceGetAttribute local copy"))?;
    let mut value = 0;
    // SAFETY: the device is owned by the bound context; output is writable.
    check("cuDeviceGetAttribute local copy", unsafe {
        f(&mut value, key, ctx.device)
    })?;
    usize::try_from(value)
        .ok()
        .filter(|n| *n > 0)
        .ok_or(Error::Unavailable("invalid local CUDA device limit"))
}
fn check_capture(ctx: &Context, stream: sys::CUstream) -> Result<()> {
    let mut capture = sys::CUstreamCaptureStatus::CU_STREAM_CAPTURE_STATUS_NONE;
    // SAFETY: the caller retains the verified stream, and capture is writable.
    check("cuStreamIsCapturing local copy", unsafe {
        (ctx.driver.stream_capture)(stream, &mut capture)
    })?;
    if capture != sys::CUstreamCaptureStatus::CU_STREAM_CAPTURE_STATUS_NONE {
        return Err(Error::Unavailable(
            "local CUDA copy cannot join stream capture",
        ));
    }
    Ok(())
}
fn uncertain(e: Error) -> Error {
    match e {
        Error::Backend { operation, code } | Error::Unquiesced { operation, code } => {
            Error::Unquiesced { operation, code }
        }
        _ => Error::Unquiesced {
            operation: "local CUDA completion",
            code: -1,
        },
    }
}
