//! Submit disjoint pieces of one large direct READ to the io_uring worker pool.
//! Every piece targets the same private, charged Task allocation. The logical
//! destination remains unchanged until the entire submitted group has drained.

use super::*;
use io_uring::{opcode, types};
use std::os::fd::AsRawFd;

pub(super) mod phases;
use phases::PhaseClock;

pub(super) fn execute(
    transport: &IoUringTransport,
    request: &FileRequest,
    deadline: Instant,
    progress: Option<(&FileAttempts<'_>, usize)>,
    report: &mut BatchReport,
    #[cfg(test)] after_first: impl FnOnce(usize),
) -> Result<bool> {
    if request.length != task::READ_PREFIX_BYTES
        || transport.pool.enabled
        || request.parts.vectored()
    {
        return Ok(false);
    }
    let mut phases = PhaseClock::start();
    let local = request.parts[0].registered()?;
    #[cfg(feature = "cuda")]
    if local.is_cuda_pinned_host() {
        return Ok(false);
    }
    let (file, alignment) = request.lease.io_file(request.offset, request.length)?;
    let address = local.base_address() + request.parts[0].local_offset as u64;
    if !request.lease.is_direct_read(TransferOp::Read, &file) || address % alignment as u64 != 0 {
        return Ok(false);
    }
    let (task, iov) = Task::new(TransferOp::Read, request, transport)?;
    // On a scalar fallback Task::entry may reference the caller-supplied iovec.
    // Its stable address follows the task into quarantine if a receipt is lost.
    let scalar_iov = Box::new(iov);
    // A single READV on the original private iovec also covers a fresh mapping
    // and any geometry for which the exact discard boundary is not legal.
    let mut lengths = [request.length, 0];
    let mut first = task.entry(TransferOp::Read, &scalar_iov, 0);
    let mut second = None;
    if let Some((scratch, prefix)) = task.retired_read_boundary() {
        let tail = request.length - prefix;
        let tail_address = scratch.wrapping_add(prefix);
        let views = [
            libc::iovec {
                iov_base: scratch.cast(),
                iov_len: prefix,
            },
            libc::iovec {
                iov_base: tail_address.cast(),
                iov_len: tail,
            },
        ];
        // This checks the actual private addresses, file offset and each length
        // with the retained descriptor. A valid first extent aligns the next offset.
        let (selected, _) = request.lease.file_for_iovecs(request.offset, &views)?;
        if request.lease.is_direct_read(TransferOp::Read, &selected) {
            let fd = types::Fd(task.file.as_raw_fd());
            let read = |address, length, offset, index| {
                opcode::Read::new(fd, address, length as u32)
                    .offset(offset)
                    .build()
                    .user_data(index)
            };
            if phases::split_enabled() {
                first = read(scratch, prefix, request.offset, 0).flags(squeue::Flags::ASYNC);
                // Ordinary direct I/O faults the discarded pages as needed.
                second = Some(read(tail_address, tail, request.offset + prefix as u64, 1));
                lengths = [prefix, tail];
            } else {
                first = read(scratch, request.length, request.offset, 0);
            }
        }
    }
    let count = 1 + usize::from(second.is_some());
    let mut ring = transport.create_ring(2)?;
    if ring.space() < count {
        return Err(Error::ResourceExhausted);
    }
    let until = deadline
        .checked_add(DESTRUCTION_TIMEOUT)
        .ok_or(Error::InvalidConfig(
            "file drain deadline overflows monotonic clock",
        ))?;
    remaining(deadline)?;
    ring.push(std::slice::from_ref(&first))?;
    let mut live = ManuallyDrop::new((ring, task, scalar_iov));
    let mut receipts = [None; 2];
    phases.mark(1);
    let mut drain_result = submit_ring(&mut *live.0, 1, deadline);
    if drain_result.consumed != 0 {
        if let Some((attempts, index)) = progress {
            attempts.posted(index);
        }
    }
    phases.mark(2);
    if let Some(second) =
        second.filter(|_| drain_result.status.is_ok() && drain_result.consumed == 1)
    {
        #[cfg(test)]
        after_first(lengths[0]);
        let prep = if Instant::now() >= deadline {
            Err(io::Error::from_raw_os_error(libc::ETIMEDOUT))
        } else {
            live.0.push(std::slice::from_ref(&second))
        };
        match prep {
            Ok(()) => {
                let tail = submit_ring(&mut *live.0, 1, deadline);
                drain_result.consumed += tail.consumed;
                drain_result.status = tail.status;
                drain_result.submit_error = tail.submit_error;
            }
            Err(e) => {
                // A failed second push or expired deadline leaves the first SQE
                // possibly active. Drain its receipt before releasing its owners.
                drain_result.submit_error = -e.raw_os_error().unwrap_or(libc::EIO);
            }
        }
    }
    if count == 2 {
        phases.mark(3);
    } else {
        phases.skip_second();
    }
    if drain_result.status.is_ok() {
        drain_result.status = drain(
            &mut *live.0,
            drain_result.consumed,
            until,
            |index, result| {
                result <= lengths[index] as i32 && receipts[index].replace(result).is_none()
            },
        );
    }
    phases.mark(4);
    let mut bytes = 0;
    let mut locally_drained = drain_result.status.is_ok();
    let error = if let Err(code) = drain_result.status {
        // No proof of quiescence. Keep the fd, scratch, loan and ring together.
        Some(unquiesced(code))
    } else if drain_result.consumed < count {
        // Every consumed SQE has drained, but the logical read did not complete.
        // Explicit retirement can fail. Keep the ring, fd, charge and caller's
        // exclusive destination loan together until that failure is reported.
        match live.1.finish_staging() {
            Ok(()) => {
                drop(ManuallyDrop::into_inner(live));
                Some(drain_result.missing_error(drain_result.consumed))
            }
            Err(error) => {
                locally_drained = false;
                Some(error)
            }
        }
    } else {
        let (_ring, mut task, iov) = ManuallyDrop::into_inner(live);
        // A negative CQE anywhere overrides even an earlier positive prefix.
        // Only a fully drained sequence of nonnegative receipts can publish.
        let mut prefix = 0;
        let mut result = 0;
        for (index, receipt) in receipts[..count].iter().enumerate() {
            let code = receipt.ok_or(Error::Internal("missing drained file piece"))?;
            if code < 0 {
                result = code;
                break;
            }
            if index == 0 || prefix == lengths[0] {
                prefix += code as usize;
            }
            result = prefix as i32;
        }
        if !task.complete_with_publish_mark(
            TransferOp::Read,
            result,
            Instant::now() >= deadline,
            || phases.mark(5),
        ) {
            // A partially completed page move may still own both mappings.
            // Keep the destination loan, scratch, descriptor and ring fenced.
            let _retained = ManuallyDrop::new((_ring, task, iov));
            return Err(unquiesced(-libc::EIO));
        }
        bytes = task.bytes();
        task.observed_error(TransferOp::Read)
    };
    phases.mark(6);
    if error.is_none() {
        phases.emit(count);
    }
    report.locally_drained = locally_drained;
    if let Some(error) = error {
        if let Some((attempts, index)) = progress {
            attempts.fail_file_task(
                index,
                bytes,
                error.for_report(),
                RemoteEffect::None,
                report.locally_drained,
            );
        }
        report.failure = Some(failure(0, error, RemoteEffect::None));
    } else {
        if let Some((attempts, index)) = progress {
            attempts.complete_task(index, bytes);
        }
        report.completed_requests = 1;
        report.transferred_bytes = bytes;
    }
    Ok(true)
}
