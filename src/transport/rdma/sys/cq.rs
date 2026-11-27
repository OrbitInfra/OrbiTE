use super::*;
use orbit_rdma::verbs::{CompletionBatch, WorkCompletion};

pub(in crate::transport::rdma) const POLL_BATCH: usize = CompletionBatch::CAPACITY;

pub(super) unsafe extern "C" fn poll(
    h: *mut ConnectionHandles,
    result: *mut Completion,
    limit: usize,
) -> c_int {
    if limit == 0 || limit > POLL_BATCH || result.is_null() || h.is_null() {
        return -libc::EINVAL;
    }
    // SAFETY: the progress lane retains the CQ and exclusively owns its dispatch.
    let Some(cq) = std::ptr::NonNull::new(unsafe { (*h).cq }) else {
        return -libc::EINVAL;
    };
    let mut batch = CompletionBatch::default();
    // SAFETY: the lane retains the CQ and its native provider through this poll.
    match unsafe { orbit_rdma::verbs::poll_batch(cq, &mut batch, limit) } {
        Ok(count) => {
            // SAFETY: the caller supplies limit writable slots and poll_batch bounds the count.
            let output = unsafe { std::slice::from_raw_parts_mut(result, count) };
            for (slot, work) in output.iter_mut().zip(batch.iter()) {
                *slot = decode(work);
            }
            count as c_int
        }
        Err(err) => -provider::provider_errno(err),
    }
}

fn decode(completion: WorkCompletion) -> Completion {
    match completion {
        WorkCompletion::Success {
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
        WorkCompletion::Error {
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
    }
}
