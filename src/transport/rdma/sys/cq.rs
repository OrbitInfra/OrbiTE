//! Native completion queues, polling and completion-channel events.

use super::*;

pub(super) unsafe extern "C" fn create_cq(
    handles: *mut CqHandles,
    device: *const DeviceHandles,
    capacity: u32,
    channel: *mut c_void,
) -> c_int {
    // SAFETY: the progress owner retains this CQ and its actual RdmaContext/PD.
    unsafe {
        (*handles).cq = ffi::ibv_create_cq(
            (*device).context.cast(),
            capacity as c_int,
            std::ptr::null_mut(),
            channel.cast(),
            0,
        )
        .cast();
        if (*handles).cq.is_null() {
            failure()
        } else if (*(*handles).cq.cast::<ffi::ibv_cq>()).cqe < capacity as c_int {
            libc::EOPNOTSUPP
        } else {
            0
        }
    }
}

pub(super) unsafe extern "C" fn poll(h: *mut ConnectionHandles, result: *mut Completion) -> c_int {
    // SAFETY: the progress lane has exclusive dispatch for this live CQ and retains its context.
    let Some(cq) = std::ptr::NonNull::new(unsafe { (*h).cq }) else {
        return -libc::EINVAL;
    };
    // SAFETY: the queue and provider outlive this serialized poll.
    let completion = match unsafe { orbit_rdma::verbs::poll(cq) } {
        Ok(Some(wc)) => wc,
        Ok(None) => return 0,
        Err(err) => return -provider_errno(err),
    };
    let value = match completion {
        orbit_rdma::verbs::WorkCompletion::Success {
            id,
            qpn,
            opcode,
            bytes,
            flags,
            invalidated,
        } => Completion {
            id,
            qpn,
            opcode,
            length: bytes,
            flags,
            invalidated_rkey: invalidated.unwrap_or(0),
            ..Completion::default()
        },
        orbit_rdma::verbs::WorkCompletion::Error {
            id,
            qpn,
            status,
            vendor,
        } => Completion {
            id,
            qpn,
            status,
            vendor_error: vendor,
            ..Completion::default()
        },
    };
    // SAFETY: the progress lane provides one writable completion result.
    unsafe {
        *result = value;
    }
    1
}

pub(super) unsafe extern "C" fn create_completion_channel(
    handles: *mut ChannelHandles,
    device: *const DeviceHandles,
) -> c_int {
    // SAFETY: The caller owns writable channel handles and a live device context throughout
    // creation.
    unsafe {
        (*handles).channel = ffi::ibv_create_comp_channel((*device).context.cast()).cast();
        if (*handles).channel.is_null() {
            return failure();
        }
        let fd = (*(*handles).channel.cast::<ffi::ibv_comp_channel>()).fd;
        (*handles).fd = fd;
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags == -1 || libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) == -1 {
            return failure();
        }
        let flags = libc::fcntl(fd, libc::F_GETFD);
        if flags == -1 || libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) == -1 {
            return failure();
        }
        0
    }
}

pub(super) unsafe extern "C" fn get_completion_event(
    channel: *mut c_void,
    cq: *mut *mut c_void,
) -> c_int {
    // SAFETY: The monitor retains the channel and provides a writable CQ output for this event.
    unsafe {
        let mut native_cq = std::ptr::null_mut();
        let mut context = std::ptr::null_mut();
        if ffi::ibv_get_cq_event(channel.cast(), &mut native_cq, &mut context) != 0 {
            return failure();
        }
        *cq = native_cq.cast();
        0
    }
}

pub(super) unsafe extern "C" fn ack_completion_event(cq: *mut c_void) {
    // SAFETY: The monitor obtained one unacknowledged event for this CQ from the open channel.
    unsafe { ffi::ibv_ack_cq_events(cq.cast(), 1) };
}

pub(super) unsafe extern "C" fn arm_completion(cq: *mut c_void) -> c_int {
    // SAFETY: The lane owns the live CQ while its provider notification callback is invoked.
    unsafe {
        let cq = cq.cast::<ffi::ibv_cq>();
        let Some(arm) = (*(*cq).context).ops.req_notify_cq else {
            return libc::EOPNOTSUPP;
        };
        arm(cq, 0)
    }
}
