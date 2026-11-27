//! Adapt engine-owned registration backing to the provider for each native call.
use super::*;

impl MemoryHandles {
    fn source(&self) -> provider::RegistrationSource {
        #[cfg(feature = "cuda")]
        {
            if let Some(buf) = &self.dmabuf {
                return match self.dmabuf_verb {
                    Some(register) => provider::RegistrationSource::DmaBuf {
                        base: buf.base,
                        size: buf.length,
                        fd: buf.fd,
                        register,
                    },
                    None => provider::RegistrationSource::Unavailable,
                };
            }
            if self.gpu_route == 1 {
                return provider::RegistrationSource::Unavailable;
            }
        }
        provider::RegistrationSource::Host
    }
}

#[cfg(all(test, feature = "cuda"))]
pub(in crate::transport::rdma) unsafe fn register_on_pd(
    memory: &MemoryHandles,
    pd: *mut c_void,
    buffer: *mut c_void,
    length: usize,
    access: c_int,
) -> *mut c_void {
    // SAFETY: the caller retains the PD, buffer and optional CUDA export throughout registration.
    unsafe { provider::register_on_pd(memory.source(), pd, buffer, length, access) }
}
pub(super) unsafe extern "C" fn register_memory(
    memory: *mut MemoryHandles,
    device: *const DeviceHandles,
    buffer: *mut c_void,
    length: usize,
    window_bind: bool,
) -> c_int {
    // SAFETY: the engine owns the memory, export and PD, including every partial result on error.
    unsafe {
        let m = &mut *memory;
        let source = m.source();
        provider::register_memory(&mut m.native, device, buffer, length, window_bind, source)
    }
}
pub(super) unsafe extern "C" fn prepare_grant(
    h: *mut ConnectionHandles,
    memory: *mut MemoryHandles,
    device: *const DeviceHandles,
    buffer: *mut c_void,
    length: usize,
    permission: u32,
) -> c_int {
    // SAFETY: the grant holds these exclusive slots and the actual subrange until teardown.
    unsafe {
        let m = &mut *memory;
        let source = m.source();
        provider::prepare_grant(h, &mut m.native, device, buffer, length, permission, source)
    }
}
pub(super) unsafe extern "C" fn allocate_window(
    memory: *mut MemoryHandles,
    device: *const DeviceHandles,
) -> c_int {
    // SAFETY: the engine owns this empty window slot and its retained device.
    unsafe { provider::allocate_window(&mut (*memory).native, device) }
}
pub(super) unsafe extern "C" fn dealloc_window(memory: *mut MemoryHandles) -> c_int {
    // SAFETY: the engine has drained all access and retains the slot and PD across failed destruction.
    unsafe { provider::dealloc_window(&mut (*memory).native) }
}
pub(super) unsafe extern "C" fn window(
    h: *mut ConnectionHandles,
    memory: *mut MemoryHandles,
    id: u64,
    address: u64,
    length: usize,
    permission: u32,
    bind: bool,
    result: *mut PostResult,
) -> c_int {
    // SAFETY: the grant owns every native handle and the loan through the reported CQ receipt or drain.
    unsafe {
        provider::window(
            h,
            &mut (*memory).native,
            id,
            address,
            length,
            permission,
            bind,
            result,
        )
    }
}
pub(super) unsafe extern "C" fn deregister(memory: *mut MemoryHandles) -> c_int {
    // SAFETY: the retirement owner issues this call once and keeps backing and FD after failure.
    unsafe { provider::deregister(&mut (*memory).native) }
}
#[cfg(feature = "cuda")]
pub(super) unsafe extern "C" fn create_gpu_key(
    h: *const DeviceHandles,
    memory: *mut MemoryHandles,
    qpn: u32,
    addr: u64,
    length: u32,
    permission: u32,
    rkey: u32,
    probe: bool,
) -> c_int {
    // SAFETY: the engine retains this parent MR and each partially created DevX object for cleanup.
    unsafe {
        provider::mlx5::create(
            h,
            &mut (*memory).native,
            qpn,
            addr,
            length,
            permission,
            rkey,
            probe,
        )
    }
}
