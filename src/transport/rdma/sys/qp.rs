//! Native RC QP setup and payload or completion-message work requests.

use super::device::{endpoint_address, query_descriptor};
use super::*;
use orbit_rdma::verbs;

// The protocol grants only READ/WRITE authority, including on atomic-capable hardware.
pub(super) fn grant_access(permission: u32) -> u32 {
    let mut access = 0;
    if permission & 1 != 0 {
        access |= ffi::ibv_access_flags::IBV_ACCESS_REMOTE_READ.0;
    }
    if permission & 2 != 0 {
        access |= ffi::ibv_access_flags::IBV_ACCESS_REMOTE_WRITE.0;
    }
    access
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
        let descriptor =
            match endpoint_address(query_descriptor(device, port, gid_index), &*expected) {
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
        let mut init: ffi::ibv_qp_init_attr = std::mem::zeroed();
        init.send_cq = h.cq.cast();
        init.recv_cq = h.receive_cq.cast();
        init.qp_type = ffi::ibv_qp_type::IBV_QPT_RC;
        init.sq_sig_all = 0;
        init.cap.max_send_wr = max_send_wr;
        init.cap.max_recv_wr = max_recv_wr;
        init.cap.max_send_sge = 1;
        init.cap.max_recv_sge = 1;
        let pd = if h.private_pd.is_null() {
            device.pd
        } else {
            h.private_pd
        };
        h.qp = ffi::ibv_create_qp(pd.cast(), &mut init).cast();
        if h.qp.is_null() {
            return failure();
        }
        if init.cap.max_send_wr < max_send_wr
            || init.cap.max_send_sge < 1
            || init.cap.max_recv_wr < max_recv_wr
            || init.cap.max_recv_sge < 1
        {
            return libc::EOPNOTSUPP;
        }
        let mut qp_attr: ffi::ibv_qp_attr = std::mem::zeroed();
        qp_attr.qp_state = ffi::ibv_qp_state::IBV_QPS_INIT;
        qp_attr.port_num = port;
        qp_attr.qp_access_flags = grant_access(permission);
        let mask = ffi::ibv_qp_attr_mask::IBV_QP_STATE
            | ffi::ibv_qp_attr_mask::IBV_QP_PKEY_INDEX
            | ffi::ibv_qp_attr_mask::IBV_QP_PORT
            | ffi::ibv_qp_attr_mask::IBV_QP_ACCESS_FLAGS;
        let error = ffi::ibv_modify_qp(h.qp.cast(), &mut qp_attr, mask.0 as c_int);
        if error != 0 {
            return error;
        }
        *local = Descriptor {
            address: buffer as u64,
            rkey: if permission != 0 {
                memory.window_key
            } else {
                (*memory.mr.cast::<ffi::ibv_mr>()).rkey
            },
            qpn: (*h.qp.cast::<ffi::ibv_qp>()).qp_num,
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
    // SAFETY: RdmaGrant owns an initialized QP and validates both descriptors
    // before this call. Each mask matches the initialized fields for its state.
    unsafe {
        let h = &mut *handles;
        let local = &*local;
        let remote = &*remote;
        let mut attr: ffi::ibv_qp_attr = std::mem::zeroed();
        attr.qp_state = ffi::ibv_qp_state::IBV_QPS_RTR;
        attr.path_mtu = u32::from(local.mtu.min(remote.mtu));
        attr.dest_qp_num = remote.qpn;
        attr.rq_psn = remote.psn;
        attr.max_dest_rd_atomic = h.read_depth.responder;
        attr.min_rnr_timer = 12;
        attr.ah_attr.sl = service_level;
        attr.ah_attr.dlid = remote.lid;
        attr.ah_attr.port_num = port;
        if local.link_layer == 2 || remote.gid != [0; 16] {
            attr.ah_attr.is_global = 1;
            attr.ah_attr.grh.dgid.raw = remote.gid;
            attr.ah_attr.grh.sgid_index = gid_index;
            attr.ah_attr.grh.hop_limit = hop_limit;
            attr.ah_attr.grh.traffic_class = traffic_class;
        }
        let mask = ffi::ibv_qp_attr_mask::IBV_QP_STATE
            | ffi::ibv_qp_attr_mask::IBV_QP_AV
            | ffi::ibv_qp_attr_mask::IBV_QP_PATH_MTU
            | ffi::ibv_qp_attr_mask::IBV_QP_DEST_QPN
            | ffi::ibv_qp_attr_mask::IBV_QP_RQ_PSN
            | ffi::ibv_qp_attr_mask::IBV_QP_MAX_DEST_RD_ATOMIC
            | ffi::ibv_qp_attr_mask::IBV_QP_MIN_RNR_TIMER;
        let error = ffi::ibv_modify_qp(h.qp.cast(), &mut attr, mask.0 as c_int);
        if error != 0 {
            return error;
        }
        let mut attr: ffi::ibv_qp_attr = std::mem::zeroed();
        attr.qp_state = ffi::ibv_qp_state::IBV_QPS_RTS;
        attr.timeout = 14;
        attr.retry_cnt = 7;
        attr.rnr_retry = 7;
        attr.sq_psn = local.psn;
        attr.max_rd_atomic = h.read_depth.initiator;
        let mask = ffi::ibv_qp_attr_mask::IBV_QP_STATE
            | ffi::ibv_qp_attr_mask::IBV_QP_TIMEOUT
            | ffi::ibv_qp_attr_mask::IBV_QP_RETRY_CNT
            | ffi::ibv_qp_attr_mask::IBV_QP_RNR_RETRY
            | ffi::ibv_qp_attr_mask::IBV_QP_SQ_PSN
            | ffi::ibv_qp_attr_mask::IBV_QP_MAX_QP_RD_ATOMIC;
        ffi::ibv_modify_qp(h.qp.cast(), &mut attr, mask.0 as c_int)
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
    let lkey = match registered_key(memory, local_address, local_length) {
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

fn registered_key(
    memory: &MemoryHandles,
    address: u64,
    length: usize,
) -> std::result::Result<u32, c_int> {
    let mr = std::ptr::NonNull::new(memory.mr.cast::<ffi::ibv_mr>()).ok_or(libc::EINVAL)?;
    // SAFETY: the calling grant or completion owner retains the registered MR.
    let mr = unsafe { mr.as_ref() };
    let offset = address.checked_sub(mr.addr as u64).ok_or(libc::EINVAL)?;
    if address.checked_add(length as u64).is_none()
        || offset
            .checked_add(length as u64)
            .is_none_or(|end| end > mr.length as u64)
    {
        return Err(libc::EINVAL);
    }
    Ok(mr.lkey)
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
    let lkey = match registered_key(memory, address, length) {
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
