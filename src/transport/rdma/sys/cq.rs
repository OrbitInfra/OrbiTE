use super::*;

pub(super) unsafe extern "C" fn poll(h: *mut ConnectionHandles, result: *mut Completion) -> c_int {
    // SAFETY: the progress lane has exclusive dispatch for this live CQ and retains its context.
    let Some(cq) = std::ptr::NonNull::new(unsafe { (*h).cq }) else {
        return -libc::EINVAL;
    };
    // SAFETY: the queue and provider outlive this serialized poll.
    let completion = match unsafe { orbit_rdma::verbs::poll(cq) } {
        Ok(Some(wc)) => wc,
        Ok(None) => return 0,
        Err(err) => return -provider::provider_errno(err),
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
