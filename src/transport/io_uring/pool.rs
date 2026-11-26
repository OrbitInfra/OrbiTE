//! File staging slots and the outstanding byte charge for pageable fallback.
use std::ptr::NonNull;
use std::sync::{Arc, Mutex};

use super::{IoUringConfig, task::Bounce};
use crate::{Error, Result};

/// File staging pool snapshot; outstanding loans remain charged until native drain.
#[derive(Clone, Copy, Debug, Default)]
#[non_exhaustive]
pub struct IoUringStats {
    /// Rounded bytes per configured slot; zero when the reusable pool is disabled.
    pub slot_bytes: usize,
    /// Configured reusable slot capacity, including slots not yet allocated.
    pub slots: usize,
    /// Currently borrowed slots, including loans retained by unquiesced I/O.
    pub active_slots: usize,
    /// Whether the initialized staging slab uses CUDA-pinned host memory.
    /// This value survives close and does not prove that the slab is still live.
    pub pinned: bool,
    /// Cumulative successful reusable-slot acquisitions.
    pub slot_acquisitions: u64,
    /// Cumulative enabled-pool attempts without an available compatible slot.
    pub slot_misses: u64,
    /// Outstanding bytes charged for per-operation pageable fallback buffers.
    pub pageable_bytes: usize,
    /// Pageable bytes retained for the next scalar direct file READ.
    pub idle_read_bytes: usize,
}

pub(super) enum Access {
    Host,
    #[cfg(feature = "cuda")]
    Device(Option<u32>),
}
pub(super) enum Slab {
    Host(Bounce),
    #[cfg(feature = "cuda")]
    Pinned(crate::cuda::PinnedAllocation),
}
impl Slab {
    pub(super) fn host(size: usize) -> Result<Self> {
        #[cfg(feature = "cuda")]
        match crate::cuda::PinnedAllocation::file_pool(size) {
            Ok(pinned) => return Ok(Self::Pinned(pinned)),
            Err(Error::Unavailable(_)) => (),
            Err(error) => return Err(error),
        }
        Ok(Self::Host(Bounce::new(size, 4096)?))
    }
    fn address(&self) -> NonNull<u8> {
        match self {
            Self::Host(b) => b.ptr,
            #[cfg(feature = "cuda")]
            Self::Pinned(p) => p.address,
        }
    }
    fn accepts(&self, access: Access) -> bool {
        match access {
            Access::Host => true,
            #[cfg(feature = "cuda")]
            Access::Device(node) => {
                matches!(self, Self::Pinned(buffer) if node.is_none() || node==buffer.numa_node)
            }
        }
    }
    fn pinned(&self) -> bool {
        !matches!(self, Self::Host(_))
    }
    fn close(self) -> Result<()> {
        match self {
            Self::Host(_) => Ok(()),
            #[cfg(feature = "cuda")]
            Self::Pinned(p) => p.close(),
        }
    }
}

#[derive(Default)]
struct State {
    slab: Option<Slab>,
    read_prefix: Option<Bounce>,
    free: Vec<usize>,
    stats: IoUringStats,
    disabled: bool,
    building: bool,
    closed: bool,
    fault: Option<Error>,
}

pub(super) struct Pool {
    state: Mutex<State>,
    fallback_limit: usize,
    pub(super) enabled: bool,
}

impl Pool {
    pub(super) fn new(config: IoUringConfig, fallback_limit: usize) -> Result<Arc<Self>> {
        let size = config.slot_size()?;
        Ok(Arc::new(Self {
            state: Mutex::new(State {
                stats: IoUringStats {
                    slot_bytes: size,
                    slots: if size == 0 { 0 } else { config.staging_slots },
                    ..Default::default()
                },
                disabled: size == 0,
                ..Default::default()
            }),
            fallback_limit,
            enabled: size != 0,
        }))
    }
    pub(super) fn stats(&self) -> IoUringStats {
        self.state.lock().unwrap_or_else(|p| p.into_inner()).stats
    }

    pub(super) fn acquire(
        self: &Arc<Self>,
        length: usize,
        alignment: usize,
        access: Access,
        allocate: impl FnOnce(usize) -> Result<Slab>,
    ) -> Result<Option<Slot>> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::Internal("file staging lock poisoned"))?;
        if let Some(e) = &state.fault {
            return Err(e.for_report());
        }
        if state.closed {
            return Err(Error::RequesterClosed);
        }
        if state.building {
            state.stats.slot_misses = state.stats.slot_misses.saturating_add(1);
            return Ok(None);
        }
        if state.disabled {
            return Ok(None);
        }
        if length <= state.stats.slot_bytes {
            if state.slab.is_none() {
                let slots = state.stats.slots;
                state
                    .free
                    .try_reserve_exact(slots)
                    .map_err(|_| Error::ResourceExhausted)?;
                let bytes = state.stats.slot_bytes * slots;
                state.building = true;
                drop(state);
                let slab = allocate(bytes);
                state = self.state.lock().unwrap_or_else(|p| p.into_inner());
                state.building = false;
                match slab {
                    Ok(slab) => {
                        state.stats.pinned = slab.pinned();
                        state.slab = Some(slab);
                        state.free.extend((0..slots).rev());
                    }
                    Err(e @ Error::Unquiesced { .. }) => {
                        state.fault = Some(e.for_report());
                        return Err(e);
                    }
                    Err(e) => {
                        log::warn!(
                            "io_uring staging pool unavailable ({e}), using charged fallback"
                        );
                        state.disabled = true;
                    }
                }
            }
            if let (Some(&index), Some(slab)) = (state.free.last(), state.slab.as_ref()) {
                let address = slab
                    .address()
                    .as_ptr()
                    .wrapping_add(index * state.stats.slot_bytes);
                if slab.accepts(access) && address as usize % alignment == 0 {
                    let ptr =
                        NonNull::new(address).ok_or(Error::Internal("null file staging slot"))?;
                    state.free.pop();
                    state.stats.active_slots += 1;
                    state.stats.slot_acquisitions = state.stats.slot_acquisitions.saturating_add(1);
                    return Ok(Some(Slot {
                        pool: self.clone(),
                        index,
                        ptr,
                        #[cfg(feature = "cuda")]
                        length,
                    }));
                }
            }
        }
        state.stats.slot_misses = state.stats.slot_misses.saturating_add(1);
        Ok(None)
    }
    pub(super) fn charge(self: &Arc<Self>, length: usize) -> Result<Charge> {
        self.charge_read(length, None)
    }

    pub(super) fn charge_read(
        self: &Arc<Self>,
        length: usize,
        alignment: Option<usize>,
    ) -> Result<Charge> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::Internal("file staging lock poisoned"))?;
        if state.closed {
            return Err(Error::RequesterClosed);
        }
        state.stats.pageable_bytes = state
            .stats
            .pageable_bytes
            .checked_add(length)
            .filter(|&n| n <= self.fallback_limit)
            .ok_or(Error::ResourceExhausted)?;
        let take = alignment.is_some_and(|alignment| {
            state
                .read_prefix
                .as_ref()
                .is_some_and(|p| p.ptr.as_ptr() as usize % alignment == 0)
        });
        let prefix = if take {
            state.stats.idle_read_bytes = 0;
            state.read_prefix.take()
        } else {
            if state.stats.idle_read_bytes > self.fallback_limit - state.stats.pageable_bytes {
                state.read_prefix.take();
                state.stats.idle_read_bytes = 0;
            }
            None
        };
        Ok(Charge {
            pool: self.clone(),
            length,
            prefix,
        })
    }
    pub(super) fn close(&self) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::Internal("file staging lock poisoned"))?;
        if let Some(e) = &state.fault {
            return Err(e.for_report());
        }
        state.closed = true;
        state.read_prefix.take();
        state.stats.idle_read_bytes = 0;
        if state.building || state.stats.active_slots != 0 || state.stats.pageable_bytes != 0 {
            return Err(Error::Unquiesced {
                operation: "file staging loans",
                code: libc::EBUSY,
            });
        }
        if let Some(slab) = state.slab.take() {
            if let Err(e) = slab.close() {
                state.fault = Some(e.for_report());
                return Err(e);
            }
        }
        Ok(())
    }
}

pub(crate) struct Slot {
    pool: Arc<Pool>,
    index: usize,
    pub(crate) ptr: NonNull<u8>,
    #[cfg(feature = "cuda")]
    pub(crate) length: usize,
}
impl Drop for Slot {
    fn drop(&mut self) {
        let mut state = self.pool.state.lock().unwrap_or_else(|p| p.into_inner());
        state.free.push(self.index);
        state.stats.active_slots -= 1;
    }
}
pub(super) struct Charge {
    pool: Arc<Pool>,
    length: usize,
    pub(super) prefix: Option<Bounce>,
}
impl Drop for Charge {
    fn drop(&mut self) {
        let mut state = self.pool.state.lock().unwrap_or_else(|p| p.into_inner());
        state.stats.pageable_bytes -= self.length;
        if !state.closed
            && state.read_prefix.is_none()
            && self.prefix.is_some()
            && super::task::READ_PREFIX_BYTES
                <= self.pool.fallback_limit - state.stats.pageable_bytes
        {
            state.read_prefix = self.prefix.take();
            state.stats.idle_read_bytes = super::task::READ_PREFIX_BYTES;
        }
    }
}
