//! Native RC QP setup and payload or completion-message work requests.

use super::device::endpoint_address;
use super::*;
use orbit_rdma::verbs;

impl From<provider::Endpoint> for Descriptor {
    fn from(x: provider::Endpoint) -> Self {
        Self {
            qpn: x.qpn,
            psn: x.psn,
            lid: x.lid,
            mtu: x.mtu,
            link_layer: x.link_layer,
            gid: x.gid,
            ..Default::default()
        }
    }
}
impl From<Descriptor> for provider::Endpoint {
    fn from(x: Descriptor) -> Self {
        Self {
            qpn: x.qpn,
            psn: x.psn,
            lid: x.lid,
            mtu: x.mtu,
            link_layer: x.link_layer,
            gid: x.gid,
        }
    }
}
pub(super) unsafe extern "C" fn open(
    handles: *mut ConnectionHandles,
    memory: *mut MemoryHandles,
    device: *const DeviceHandles,
    port: u8,
    gid_index: u8,
    expected: *const Descriptor,
    buffer: *mut c_void,
    length: usize,
    permission: u32,
    psn: u32,
    max_send_wr: u32,
    max_recv_wr: u32,
    local: *mut Descriptor,
) -> c_int {
    // SAFETY: the context owns a frozen route; query before constructing children.
    unsafe {
        let h = &mut *handles;
        let memory = &mut *memory;
        let descriptor = match endpoint_address(
            provider::query_descriptor(device, port, gid_index).map(Descriptor::from),
            &*expected,
        ) {
            Ok(address) => address,
            Err(code) => return code,
        };
        let device = &*device;
        if h.cq.is_null() || h.receive_cq.is_null() {
            return libc::EINVAL;
        }
        let code = prepare_grant(handles, memory, device, buffer, length, permission);
        if code != 0 {
            return code;
        }
        let pd = if h.private_pd.is_null() {
            device.pd
        } else {
            h.private_pd
        };
        let qpn = match provider::open_rc(h, pd, port, permission, max_send_wr, max_recv_wr) {
            Ok(qpn) => qpn,
            Err(code) => return code,
        };
        *local = Descriptor {
            address: buffer as u64,
            rkey: if permission != 0 {
                memory.window_key
            } else {
                provider::remote_key(&memory.native)
            },
            qpn,
            psn,
            ..descriptor
        };
        0
    }
}

pub(super) unsafe extern "C" fn connect(
    handles: *mut ConnectionHandles,
    local: *const Descriptor,
    remote: *const Descriptor,
    port: u8,
    gid_index: u8,
    service_level: u8,
    traffic_class: u8,
    hop_limit: u8,
) -> c_int {
    // SAFETY: the grant retains this INIT QP; the handshake validated both endpoint descriptors.
    unsafe {
        provider::connect(
            handles,
            &(*local).into(),
            &(*remote).into(),
            port,
            gid_index,
            service_level,
            traffic_class,
            hop_limit,
        )
    }
}

pub(super) unsafe extern "C" fn post(
    h: *mut ConnectionHandles,
    memory: *const MemoryHandles,
    requests: *const WorkRequest,
    count: usize,
    local_address: u64,
    local_length: usize,
    address: u64,
    rkey: u32,
    write: c_int,
    result: *mut PostResult,
) -> c_int {
    // SAFETY: the grant holds the QP, MR and full request slice; result is writable.
    let (handles, memory, result) = unsafe { (&*h, &*memory, &mut *result) };
    *result = PostResult::default();
    let Some(qp) = std::ptr::NonNull::new(handles.qp) else {
        return libc::EINVAL;
    };
    if !(1..=64).contains(&count) || local_length == 0 {
        return libc::EINVAL;
    }
    // SAFETY: the grant holds this real MR through submission and physical drain.
    let lkey = match unsafe { provider::local_key(&memory.native, local_address, local_length) } {
        Ok(key) => key,
        Err(error) => return error,
    };
    let mut chain = Vec::new();
    if chain.try_reserve_exact(count).is_err() {
        return libc::ENOMEM;
    }
    // SAFETY: the caller provided count initialized WorkRequests for this post.
    for request in unsafe { std::slice::from_raw_parts(requests, count) } {
        if request.length == 0
            || (request.offset as usize)
                .checked_add(request.length as usize)
                .is_none_or(|end| end > local_length)
        {
            return libc::EINVAL;
        }
        let Some(remote) = address.checked_add(u64::from(request.offset)) else {
            return libc::EINVAL;
        };
        let opcode = if write != 0 {
            verbs::Opcode::Write {
                address: remote,
                rkey,
            }
        } else {
            verbs::Opcode::Read {
                address: remote,
                rkey,
            }
        };
        // The registered_key range check covered every local address in this batch.
        chain.push(
            verbs::WorkRequest::new(
                request.id,
                opcode,
                local_address + u64::from(request.offset),
                request.length,
                lkey,
            )
            .signaled(request.signaled),
        );
    }
    // SAFETY: OrbiTE's grant and CQ lane retain all possibly posted bytes and the QP until drain.
    result.record(unsafe { verbs::post(qp, &mut chain) })
}

fn post_message(
    handles: &ConnectionHandles,
    memory: &MemoryHandles,
    id: u64,
    address: u64,
    length: usize,
    opcode: verbs::Opcode,
    result: &mut PostResult,
) -> c_int {
    *result = PostResult::default();
    let Some(qp) = std::ptr::NonNull::new(handles.qp) else {
        return libc::EINVAL;
    };
    if id == 0 || length != crate::transport::rdma::COMPLETION_MESSAGE_SIZE {
        return libc::EINVAL;
    }
    // SAFETY: the control slot owns its MR through the posted work and retirement.
    let lkey = match unsafe { provider::local_key(&memory.native, address, length) } {
        Ok(key) => key,
        Err(error) => return error,
    };
    let mut wr = verbs::WorkRequest::new(id, opcode, address, length as u32, lkey)
        .fenced(opcode != verbs::Opcode::Receive);
    // SAFETY: the completion slot retains QP, MR and buffer through its own CQ receipt or drain.
    result.record(unsafe { verbs::post(qp, std::slice::from_mut(&mut wr)) })
}

pub(super) unsafe extern "C" fn post_receive(
    handles: *mut ConnectionHandles,
    memory: *const MemoryHandles,
    id: u64,
    address: u64,
    length: usize,
    result: *mut PostResult,
) -> c_int {
    // SAFETY: the receive owner supplies live handles and a writable result during post.
    unsafe {
        post_message(
            &*handles,
            &*memory,
            id,
            address,
            length,
            verbs::Opcode::Receive,
            &mut *result,
        )
    }
}

pub(super) unsafe extern "C" fn send_completion(
    handles: *mut ConnectionHandles,
    memory: *const MemoryHandles,
    id: u64,
    address: u64,
    length: usize,
    rkey: u32,
    invalidate: bool,
    result: *mut PostResult,
) -> c_int {
    let opcode = if invalidate {
        verbs::Opcode::SendWithInvalidate { rkey }
    } else {
        verbs::Opcode::Send
    };
    // SAFETY: the send owner supplies live handles and a writable result during post.
    unsafe {
        post_message(
            &*handles,
            &*memory,
            id,
            address,
            length,
            opcode,
            &mut *result,
        )
    }
}
