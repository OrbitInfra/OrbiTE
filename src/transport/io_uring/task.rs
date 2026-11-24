use std::alloc::{Layout, alloc, dealloc};
use std::fs::File;
use std::os::fd::AsRawFd;
use std::ptr::NonNull;
use std::sync::Arc;

use io_uring::{opcode, squeue, types};

use super::FileRequest;
use crate::memory::DmaLease;
use crate::{Error, Result, TransferOp};

pub(super) struct Bounce {
    pub(super) ptr: NonNull<u8>,
    layout: Layout,
}

#[allow(unsafe_code)]
// SAFETY: Bounce uniquely owns stable aligned bytes. Moving it never moves those
// bytes. Pool leases or the task’s CQE protect access for the entire native use.
unsafe impl Send for Bounce {}
impl Bounce {
    #[allow(unsafe_code)]
    pub(super) fn new(length: usize, alignment: usize) -> Result<Self> {
        let layout = Layout::from_size_align(length, alignment).map_err(|_| Error::InvalidRange)?;
        // SAFETY: file admission validated nonzero length, and Layout checked alignment.
        let ptr = NonNull::new(unsafe { alloc(layout) }).ok_or(Error::ResourceExhausted)?;
        Ok(Self { ptr, layout })
    }
}

impl Drop for Bounce {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        // SAFETY: same allocation and layout, dropped only before submission or after drain.
        unsafe {
            dealloc(self.ptr.as_ptr(), self.layout);
        }
    }
}

enum Scratch {
    Slot(super::pool::Slot),
    Pageable {
        buffer: Bounce,
        _charge: super::pool::Charge,
    },
}
impl Scratch {
    fn acquire(
        transport: &super::IoUringTransport,
        length: usize,
        alignment: usize,
        direct_read: bool,
    ) -> Result<Self> {
        if let Some(slot) = transport.pool.acquire(
            length,
            alignment,
            super::pool::Access::Host,
            super::pool::Slab::host,
        )? {
            return Ok(Self::Slot(slot));
        }
        let charge = transport.pool.charge(length)?;
        // Faulting every base page during GUP dominates a large, fresh direct
        // READ buffer. Only advise complete 2 MiB units of private scratch.
        // The kernel can always satisfy this hint with ordinary pages.
        const HUGE: usize = 2 << 20;
        let advise = direct_read && length >= HUGE;
        let buffer = if advise && alignment < HUGE {
            match (transport.allocate)(length, HUGE) {
                Err(Error::ResourceExhausted) => (transport.allocate)(length, alignment)?,
                result => result?,
            }
        } else {
            (transport.allocate)(length, alignment)?
        };
        if advise && buffer.ptr.as_ptr() as usize % HUGE == 0 {
            // SAFETY: the pointer is huge-page aligned and exclusively owned.
            // The hinted extent only includes complete units inside the allocation.
            #[allow(unsafe_code)]
            unsafe {
                libc::madvise(
                    buffer.ptr.as_ptr().cast(),
                    length / HUGE * HUGE,
                    libc::MADV_HUGEPAGE,
                );
            }
        }
        Ok(Self::Pageable {
            buffer,
            _charge: charge,
        })
    }

    fn address(&self) -> NonNull<u8> {
        match self {
            Self::Slot(slot) => slot.ptr,
            Self::Pageable { buffer, .. } => buffer.ptr,
        }
    }
}

enum HostMemory {
    One(DmaLease),
    Vector {
        _loans: Vec<DmaLease>,
        iovecs: Box<[libc::iovec]>,
        _padding: Option<Scratch>,
    },
}

pub(super) struct Task {
    pub(super) file: Arc<File>,
    memory: Option<HostMemory>,
    length: usize,
    bounce: Option<Scratch>,
    offset: u64,
    pub(super) result: Option<i32>,
    pub(super) late: bool,
}

impl Task {
    #[allow(unsafe_code)]
    pub(super) fn new(
        operation: TransferOp,
        request: &FileRequest,
        transport: &super::IoUringTransport,
    ) -> Result<(Self, libc::iovec)> {
        if request.parts.vectored() {
            let (loans, spans) = request.parts.borrow_ranges(|m, o, n| m.dma_lease(o, n))?;
            let padding_bytes = request
                .parts
                .iter()
                .filter(|part| part.local.is_none())
                .map(|part| part.length)
                .sum::<usize>();
            let padding = if padding_bytes == 0 {
                None
            } else {
                let scratch = Scratch::acquire(transport, padding_bytes, 4096, false)?;
                if operation == TransferOp::Write {
                    // SAFETY: this exclusive scratch lease covers all padding bytes.
                    // A reused slot may contain bytes from an earlier READ.
                    unsafe {
                        scratch.address().as_ptr().write_bytes(0, padding_bytes);
                    }
                }
                Some(scratch)
            };
            let mut scratch_offset = 0;
            let mut iovecs = Vec::with_capacity(spans.len());
            for (part, span) in request.parts.iter().zip(spans) {
                let address = if let Some(span) = span {
                    loans[span.loan]
                        .address()
                        .as_ptr()
                        .wrapping_add(span.offset)
                } else {
                    let scratch = padding
                        .as_ref()
                        .ok_or(Error::Internal("missing file padding scratch"))?;
                    let address = scratch.address().as_ptr().wrapping_add(scratch_offset);
                    scratch_offset += part.length;
                    address
                };
                iovecs.push(libc::iovec {
                    iov_base: address.cast(),
                    iov_len: part.length,
                });
            }
            let (file, alignment) = request.lease.file_for_iovecs(request.offset, &iovecs)?;
            let (bounce, padding) = if request.lease.is_direct_read(operation, &file) {
                // Padding only advances the position in the private READ buffer.
                for (iov, part) in iovecs.iter_mut().zip(request.parts.iter()) {
                    if part.local.is_none() {
                        iov.iov_base = std::ptr::null_mut();
                    }
                }
                drop(padding);
                (
                    Some(Scratch::acquire(
                        transport,
                        request.length,
                        alignment,
                        true,
                    )?),
                    None,
                )
            } else {
                (None, padding)
            };
            let span = libc::iovec {
                iov_base: bounce
                    .as_ref()
                    .map_or(std::ptr::null_mut(), |b| b.address().as_ptr().cast()),
                iov_len: request.length,
            };
            return Ok((
                Self {
                    file,
                    memory: Some(HostMemory::Vector {
                        _loans: loans,
                        iovecs: iovecs.into_boxed_slice(),
                        _padding: padding,
                    }),
                    length: request.length,
                    bounce,
                    offset: request.offset,
                    result: None,
                    late: false,
                },
                span,
            ));
        }
        let part = &request.parts[0];
        let local = part.registered()?;
        let memory = local.dma_lease(part.local_offset, part.length)?;
        // CUDA-owned host memory is directly accessible after dma_lease consumes
        // its producer event. Use buffered I/O for an unaligned view, without a copy.
        #[cfg(feature = "cuda")]
        let direct_host = if local.is_cuda_pinned_host() {
            Some(request.lease.file_for_buffer(
                request.offset,
                request.length,
                memory.address().as_ptr() as usize,
            )?)
        } else {
            None
        };
        #[cfg(not(feature = "cuda"))]
        let direct_host = None;
        let host_pinned = direct_host.is_some();
        let (file, alignment) = match direct_host {
            Some(descriptor) => descriptor,
            None => request.lease.io_file(request.offset, request.length)?,
        };
        let staging_alignment = if !host_pinned && transport.pool.enabled {
            alignment.max(4096)
        } else {
            alignment
        };
        // A direct READ racing truncate can zero the rest of the last block
        // even when its CQE reports a shorter prefix. Keep that write private.
        let protect = request.lease.is_direct_read(operation, &file);
        let bounce = if protect || memory.address().as_ptr() as usize % staging_alignment != 0 {
            let bounce = Scratch::acquire(transport, request.length, staging_alignment, protect)?;
            if operation == TransferOp::Write {
                // SAFETY: the exclusive loan covers all initialized WRITE bytes.
                // The fresh bounce is disjoint and has exactly that capacity.
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        memory.address().as_ptr(),
                        bounce.address().as_ptr(),
                        request.length,
                    );
                }
            }
            Some(bounce)
        } else {
            None
        };
        let iov = libc::iovec {
            iov_base: bounce
                .as_ref()
                .map_or(memory.address(), Scratch::address)
                .as_ptr()
                .cast(),
            iov_len: memory.len(),
        };
        Ok((
            Self {
                file,
                memory: Some(HostMemory::One(memory)),
                length: request.length,
                bounce,
                offset: request.offset,
                result: None,
                late: false,
            },
            iov,
        ))
    }

    pub(super) fn bytes(&self) -> usize {
        self.result.unwrap_or(0).max(0) as usize
    }

    pub(super) fn observed_error(&self, operation: TransferOp) -> Option<Error> {
        cqe_error(operation, self.result?, self.length, self.late)
    }

    pub(super) fn entry(
        &self,
        operation: TransferOp,
        iov: &libc::iovec,
        index: usize,
    ) -> squeue::Entry {
        let iovecs = match (&self.memory, &self.bounce) {
            (Some(HostMemory::Vector { iovecs, .. }), None) => &**iovecs,
            _ => std::slice::from_ref(iov),
        };
        entry(&self.file, self.offset, operation, iovecs, index)
    }
    #[allow(unsafe_code)]
    pub(super) fn complete(&mut self, operation: TransferOp, result: i32, late: bool) -> bool {
        let Some(memory) = &self.memory else {
            return false;
        };
        if result > 0 && result as usize > self.length {
            return false;
        }
        self.result = Some(result);
        self.late = late;
        if operation == TransferOp::Read && result > 0 {
            if let Some(bounce) = &self.bounce {
                let source = bounce.address().as_ptr();
                let length = result as usize;
                match memory {
                    HostMemory::One(memory) => {
                        // SAFETY: this CQE initialized length disjoint scratch bytes.
                        // The exclusive destination loan covers the checked prefix.
                        unsafe {
                            std::ptr::copy_nonoverlapping(source, memory.address().as_ptr(), length)
                        };
                    }
                    HostMemory::Vector { iovecs, .. } => {
                        let mut copied = 0;
                        for iov in iovecs {
                            let n = iov.iov_len.min(length - copied);
                            if n == 0 {
                                break;
                            }
                            if !iov.iov_base.is_null() {
                                // SAFETY: the CQE covers this scratch prefix and the retained
                                // loan covers this destination. Padding has a null address.
                                unsafe {
                                    std::ptr::copy_nonoverlapping(
                                        source.add(copied),
                                        iov.iov_base.cast(),
                                        n,
                                    )
                                };
                            }
                            copied += n;
                        }
                    }
                }
            }
        }
        // Publish only after releasing this CQE's exclusive range. Another task
        // in the same ring can still be using its own disjoint range.
        self.bounce.take();
        self.memory.take();
        true
    }
}

pub(super) fn entry(
    file: &File,
    offset: u64,
    operation: TransferOp,
    iov: &[libc::iovec],
    index: usize,
) -> squeue::Entry {
    let fd = types::Fd(file.as_raw_fd());
    let entry = match operation {
        TransferOp::Read => opcode::Readv::new(fd, iov.as_ptr(), iov.len() as u32)
            .offset(offset)
            .build(),
        TransferOp::Write => opcode::Writev::new(fd, iov.as_ptr(), iov.len() as u32)
            .offset(offset)
            .build(),
    };
    entry.user_data(index as u64)
}

pub(super) fn cqe_error(
    operation: TransferOp,
    result: i32,
    length: usize,
    late: bool,
) -> Option<Error> {
    match result {
        code if code < 0 => Some(Error::Backend {
            operation: "io_uring CQE",
            code,
        }),
        bytes if bytes as usize != length => Some(Error::Io(std::io::Error::new(
            if operation == TransferOp::Read {
                std::io::ErrorKind::UnexpectedEof
            } else {
                std::io::ErrorKind::WriteZero
            },
            "short io_uring completion",
        ))),
        _ if late => Some(Error::Timeout),
        _ => None,
    }
}
