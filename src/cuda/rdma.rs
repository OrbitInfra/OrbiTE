//! CUDA registration consistency, range ownership and GPUDirect write visibility.

use std::ops::Range;

use super::*;
use crate::transport::rdma::{GpuDirectRegistration, GpuRegistrationVerbs, MemoryHandles};

/// A CUDA-exported descriptor, retained by the actual MR owner. Linux close
/// consumes the descriptor even on an error; never retry it.
pub(crate) struct DmaBuf {
    pub(crate) fd: i32,
    pub(crate) base: u64,
    pub(crate) length: usize,
    context: Arc<Context>,
    closed: bool,
    failure: i32,
}
impl DmaBuf {
    pub(crate) fn close(&mut self) -> std::result::Result<(), (&'static str, i32)> {
        if !self.closed {
            self.closed = true;
            // SAFETY: this export owns the fd exclusively and closes it exactly once.
            if unsafe { (self.context.driver.close_fd)(self.fd) } != 0 {
                self.failure = std::io::Error::last_os_error()
                    .raw_os_error()
                    .filter(|code| *code != 0)
                    .unwrap_or(libc::EIO);
            }
        }
        if self.failure == 0 {
            Ok(())
        } else {
            Err(("close CUDA dma-buf", self.failure))
        }
    }
}
impl Drop for DmaBuf {
    fn drop(&mut self) {
        if let Err((operation, code)) = self.close() {
            log::error!("{operation} failed ({code}); retaining CUDA context");
            std::mem::forget(self.context.clone());
        }
    }
}

pub(super) enum Ordering {
    Device,
    Flush,
}

impl DeviceAllocation {
    fn prepare_rdma(&mut self) -> Result<()> {
        if self.rdma.is_some() {
            return Ok(());
        }
        let context = &self.context;
        let ordering = context.current(|| {
            let mut ordered = 0;
            // SAFETY: The retained device belongs to the current context and ordered is a
            // writable C int.
            check("cuDeviceGetAttribute RDMA ordering", unsafe {
                (context.driver.device_attribute)(&mut ordered,
                    sys::CUdevice_attribute::CU_DEVICE_ATTRIBUTE_GPU_DIRECT_RDMA_WRITES_ORDERING,
                    context.device)
            })?;
            let ordering = if ordered >= sys::CUGPUDirectRDMAWritesOrdering::CU_GPU_DIRECT_RDMA_WRITES_ORDERING_OWNER as i32 {
                Ordering::Device
            } else {
                let mut options = 0;
                // SAFETY: The same live device is queried into a separate writable integer.
                check("cuDeviceGetAttribute RDMA flush", unsafe {
                    (context.driver.device_attribute)(&mut options,
                        sys::CUdevice_attribute::CU_DEVICE_ATTRIBUTE_GPU_DIRECT_RDMA_FLUSH_WRITES_OPTIONS,
                        context.device)
                })?;
                if options & sys::CUflushGPUDirectRDMAWritesOptions::CU_FLUSH_GPU_DIRECT_RDMA_WRITES_OPTION_HOST as i32 == 0 {
                    return Err(Error::Unavailable("CUDA device cannot establish GPUDirect write visibility"));
                }
                Ordering::Flush
            };
            let enabled = 1u32;
            // SAFETY: The allocation owns this device address and SYNC_MEMOPS reads one
            // unsigned integer.
            check("cuPointerSetAttribute SYNC_MEMOPS", unsafe {
                (context.driver.set_pointer_attribute)((&enabled as *const u32).cast(),
                    PointerAttribute::CU_POINTER_ATTRIBUTE_SYNC_MEMOPS, self.address)
            })?;
            Ok(ordering)
        })?;
        self.rdma = Some(ordering);
        Ok(())
    }
}

impl CudaMemory {
    pub(crate) fn with_rdma_registration<T>(
        &self,
        handles: &mut MemoryHandles,
        policy: GpuDirectRegistration,
        verbs: &GpuRegistrationVerbs,
        operation: impl FnOnce(&mut MemoryHandles) -> Result<T>,
    ) -> Result<T> {
        let mut allocation = self
            .allocation
            .lock()
            .map_err(|_| Error::Internal("CUDA memory lock poisoned"))?;
        if allocation.uncertain {
            return Err(Error::Unquiesced {
                operation: "CUDA prior DMA",
                code: -1,
            });
        }
        allocation.prepare_rdma()?;
        let context = &allocation.context;
        context.current(|| {
            // A cached parent MR already has a route and its own native lifetime.
            if handles.mr.is_null() && handles.gpu_route == 0 {
                let verb = (verbs.dmabuf)();
                let mut supported = 0;
                if policy != GpuDirectRegistration::Peermem && verb.is_some()
                    && context.driver.dmabuf_export.is_some() {
                    // SAFETY: live retained CUDA device and a writable attribute output.
                    let rc = unsafe { (context.driver.device_attribute)(
                        &mut supported, sys::CUdevice_attribute::CU_DEVICE_ATTRIBUTE_DMA_BUF_SUPPORTED,
                        context.device,
                    ) };
                    match rc {
                        SUCCESS => (),
                        sys::CUresult::CUDA_ERROR_INVALID_VALUE | sys::CUresult::CUDA_ERROR_NOT_SUPPORTED => supported = 0,
                        rc => check("cuDeviceGetAttribute DMA_BUF_SUPPORTED", rc)?,
                    }
                }
                if supported != 0 && policy != GpuDirectRegistration::Peermem {
                    let export = context.driver.dmabuf_export
                        .ok_or(Error::Unavailable("CUDA dma-buf export symbol"))?;
                    // SAFETY: sysconf has no pointer arguments and its result is checked.
                    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
                    let page = usize::try_from(page).ok().filter(|p| p.is_power_of_two())
                        .ok_or(Error::Unavailable("host page size for CUDA dma-buf"))?;
                    let (base, size) = if let Some(mapping) = &allocation.fabric {
                        (mapping.address, mapping.size)
                    } else {
                        let mut base = 0;
                        let mut size = 0;
                        // SAFETY: this allocation is retained; query its actual native extent.
                        check("cuMemGetAddressRange dma-buf", unsafe {
                            (context.driver.address_range)(&mut base, &mut size, allocation.address)
                        })?;
                        (base, size)
                    };
                    if base == 0 || size == 0 || base % page as u64 != 0  {
                        return Err(Error::Unavailable("CUDA dma-buf allocation extent is not host-page aligned"));
                    }
                    let offset = usize::try_from(self.base_address().checked_sub(base).ok_or(Error::InvalidRange)?)
                        .map_err(|_| Error::InvalidRange)?;
                    checked_range(offset, self.len(), size)?;
                    // Legacy allocations are exported in whole host pages, as in NCCL.
                    // VMM mappings already cover whole allocation-granularity pages.
                    let size = size.checked_add(page - 1).ok_or(Error::InvalidRange)? & !(page - 1);
                    base.checked_add(size as u64).ok_or(Error::InvalidRange)?;
                    let mut version = 0;
                    // SAFETY: writable driver version output; no runtime state is changed.
                    check("cuDriverGetVersion dma-buf", unsafe { (context.driver.version)(&mut version) })?;
                    // OrbiTE selects PCIe RNIC paths, not a topology-selected C2C route.
                    // CUDA before 12.8 accepts only zero; newer drivers can force BAR1.
                    let flags = if version >= 12080 { sys::CUmemRangeFlags_enum::CU_MEM_RANGE_FLAG_DMA_BUF_MAPPING_TYPE_PCIE as u64 } else { 0 };
                    let mut fd = -1;
                    // SAFETY: the containing native allocation is fully mapped, retained and
                    // host-page aligned. The output is an owned Linux dma-buf descriptor.
                    let rc = unsafe { export((&mut fd as *mut i32).cast(), base, size,
                        sys::CUmemRangeHandleType::CU_MEM_RANGE_HANDLE_TYPE_DMA_BUF_FD, flags) };
                    if fd >= 0 {
                        handles.dmabuf = Some(DmaBuf { fd, base, length: size, context: context.clone(), closed: false, failure: 0 });
                    }
                    check("cuMemGetHandleForAddressRange dma-buf", rc)?;
                    if handles.dmabuf.is_none() {
                        return Err(Error::Backend { operation: "CUDA dma-buf returned no fd", code: libc::EBADF });
                    }
                    handles.dmabuf_verb = verb;
                    handles.gpu_route = 1;
                } else if policy != GpuDirectRegistration::Dmabuf && (verbs.peermem)() {
                    handles.gpu_route = 2;
                } else {
                    return Err(Error::Unavailable("GPU Direct registration route unavailable: require CUDA dma-buf and ibv_reg_dmabuf_mr, or nvidia_peermem"));
                }
            }
            operation(handles)
        })
    }

    #[cfg(test)]
    pub(crate) fn with_rdma_context<T>(&self, operation: impl FnOnce() -> Result<T>) -> Result<T> {
        let mut allocation = self
            .allocation
            .lock()
            .map_err(|_| Error::Internal("CUDA memory lock poisoned"))?;
        if allocation.uncertain {
            return Err(Error::Unquiesced {
                operation: "CUDA prior DMA",
                code: -1,
            });
        }
        allocation.prepare_rdma()?;
        allocation.context.current(operation)
    }

    pub(crate) fn acquire_dma(&self, range: Range<usize>) -> Result<()> {
        let mut allocation = self
            .allocation
            .lock()
            .map_err(|_| Error::Internal("CUDA memory lock poisoned"))?;
        allocation.prepare_access(&range)?;
        allocation.prepare_rdma()?;
        allocation
            .dma
            .try_reserve(1)
            .map_err(|_| Error::ResourceExhausted)?;
        allocation.dma.push(range);
        Ok(())
    }

    pub(crate) fn complete_dma(&self) -> Result<()> {
        let mut allocation = self.allocation.lock().map_err(|_| Error::Unquiesced {
            operation: "CUDA DMA lock",
            code: -1,
        })?;
        if allocation.uncertain {
            return Err(Error::Unquiesced {
                operation: "CUDA prior DMA",
                code: -1,
            });
        }
        let context = Arc::clone(&allocation.context);
        let result = context.current(|| match allocation.rdma {
            Some(Ordering::Device) => Ok(()),
            // SAFETY: The registered allocation still owns the current context, whose host
            // flush capability was checked at registration.
            Some(Ordering::Flush) => check("cuFlushGPUDirectRDMAWrites", unsafe {
                (context.driver.flush_rdma)(
                    sys::CUflushGPUDirectRDMAWritesTarget::CU_FLUSH_GPU_DIRECT_RDMA_WRITES_TARGET_CURRENT_CTX,
                    sys::CUflushGPUDirectRDMAWritesScope::CU_FLUSH_GPU_DIRECT_RDMA_WRITES_TO_OWNER)
            }),
            None => Err(Error::Internal("CUDA DMA lacks registration ordering")),
        });
        if let Err(error) = result {
            allocation.uncertain = true;
            let (operation, code) = match error {
                Error::Backend { operation, code } | Error::Unquiesced { operation, code } => {
                    (operation, code)
                }
                _ => ("CUDA GPUDirect visibility", -1),
            };
            return Err(Error::Unquiesced { operation, code });
        }
        Ok(())
    }

    pub(crate) fn release_dma(&self, range: &Range<usize>) {
        self.allocation
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .dma
            .retain(|loan| loan != range);
    }
}
