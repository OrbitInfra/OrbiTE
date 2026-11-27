//! Transfer private anonymous pages after a terminal direct READ receipt.
//! Linux UAPI: include/uapi/linux/userfaultfd.h (UFFDIO_API, REGISTER, MOVE).
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::ptr::NonNull;

use crate::{Error, Result};

const UFFDIO: u32 = 0xaa;
const UFFD_FEATURE_MOVE: u64 = 1 << 16;
const UFFDIO_MOVE: u32 = 5;
const UFFD_USER_MODE_ONLY: libc::c_int = 1;
const REGISTER_MISSING: u64 = 1;
const MOVE_DONTWAKE: u64 = 1;

#[repr(C)]
struct Api {
    api: u64,
    features: u64,
    ioctls: u64,
}

#[derive(Clone, Copy)]
#[repr(C)]
struct Range {
    start: u64,
    len: u64,
}

#[repr(C)]
struct Registration {
    range: Range,
    mode: u64,
    ioctls: u64,
}

#[repr(C)]
struct Move {
    dst: u64,
    src: u64,
    len: u64,
    mode: u64,
    moved: i64,
}

fn uncertain(code: i32) -> Error {
    Error::Unquiesced {
        operation: "io_uring READ page publication",
        code: -code,
    }
}

#[allow(unsafe_code)]
fn open_for_move() -> Option<OwnedFd> {
    // SAFETY: userfaultfd takes only these flags; a nonnegative result is a new owned fd.
    let raw = unsafe {
        libc::syscall(
            libc::SYS_userfaultfd,
            libc::O_CLOEXEC | libc::O_NONBLOCK | UFFD_USER_MODE_ONLY,
        )
    };
    if raw < 0 {
        return None;
    }
    // SAFETY: this is the newly created file descriptor, with no other Rust owner.
    let fd = unsafe { OwnedFd::from_raw_fd(raw as libc::c_int) };
    let mut api = Api {
        api: UFFDIO.into(),
        features: UFFD_FEATURE_MOVE,
        ioctls: 0,
    };
    // SAFETY: this ioctl takes exactly one writable uffdio_api record.
    let rc = unsafe { libc::ioctl(fd.as_raw_fd(), libc::_IOWR::<Api>(UFFDIO, 0x3f), &mut api) };
    (rc == 0 && api.features & UFFD_FEATURE_MOVE != 0).then_some(fd)
}

/// Source and destination must be disjoint, exclusive private anonymous pages. If
/// a move does not start, return zero. A successful partial move consumes only
/// its source prefix. The fd never leaves this operation.
#[allow(unsafe_code)]
unsafe fn move_prefix(
    src: NonNull<u8>,
    dst: NonNull<u8>,
    length: usize,
    discard_destination: bool,
) -> Result<usize> {
    // SAFETY: sysconf takes one constant and has no pointer arguments.
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if page <= 0 {
        return Ok(0);
    }
    let page = page as usize;
    let whole = length - length % page;
    if whole == 0 || src.as_ptr() as usize % page != 0 || dst.as_ptr() as usize % page != 0 {
        return Ok(0);
    }
    let Some(fd) = open_for_move() else {
        return Ok(0);
    };
    let mut registration = Registration {
        range: Range {
            start: dst.as_ptr() as usize as u64,
            len: whole as u64,
        },
        mode: REGISTER_MISSING,
        ioctls: 0,
    };
    // SAFETY: the writable registration record describes only the caller's exclusive range.
    let reg_rc = unsafe {
        libc::ioctl(
            fd.as_raw_fd(),
            libc::_IOWR::<Registration>(UFFDIO, 0),
            &mut registration,
        )
    };
    let mut progress = Ok(0);
    if reg_rc == 0 && registration.ioctls & (1u64 << UFFDIO_MOVE) != 0 {
        if !discard_destination
            // SAFETY: every whole destination page belongs to this confirmed prefix.
            // Its complete authoritative new contents are still in src.
            || unsafe { libc::madvise(dst.as_ptr().cast(), whole, libc::MADV_DONTNEED) } == 0
        {
            let mut request = Move {
                dst: registration.range.start,
                src: src.as_ptr() as usize as u64,
                len: whole as u64,
                mode: MOVE_DONTWAKE,
                moved: i64::MIN,
            };
            // SAFETY: both ranges remain mapped and exclusively owned; the writable
            // stack record remains live for the kernel's final signed move receipt.
            let rc = unsafe {
                libc::ioctl(
                    fd.as_raw_fd(),
                    libc::_IOWR::<Move>(UFFDIO, UFFDIO_MOVE),
                    &mut request,
                )
            };
            let errno = io::Error::last_os_error()
                .raw_os_error()
                .unwrap_or(libc::EIO);
            // The kernel returns EAGAIN with the moved prefix on partial success.
            // EFAULT with no output could mean the final receipt failed to copy out.
            progress = match usize::try_from(request.moved) {
                Ok(n) if n <= whole && n % page == 0 && (rc < 0 || n == whole) => Ok(n),
                Err(_) if rc < 0 && (request.moved != i64::MIN || errno != libc::EFAULT) => Ok(0),
                _ => Err(uncertain(libc::EPROTO)),
            };
        }
    } else if reg_rc < 0 {
        let errno = io::Error::last_os_error()
            .raw_os_error()
            .unwrap_or(libc::EIO);
        if errno == libc::EBUSY {
            // A foreign fault handler owns this range. Neither unregister it nor
            // fault through it during fallback publication.
            return Err(uncertain(errno));
        }
        if matches!(errno, libc::EPERM | libc::ENOSYS | libc::ENOTTY) {
            // These errors reject registration before attaching any part of the range.
            return Ok(0);
        }
        // VMA splitting can fail partway through registration. Clear this range
        // before allowing a copy even when REGISTER did not return success.
    }
    // SAFETY: the exclusive range cannot contain another userfaultfd registration.
    // Explicitly detach even if a concurrent fork inherited a copy of this fd.
    let detached = unsafe {
        libc::ioctl(
            fd.as_raw_fd(),
            libc::_IOR::<Range>(UFFDIO, 1),
            &registration.range,
        )
    };
    if detached != 0 {
        return Err(uncertain(
            io::Error::last_os_error()
                .raw_os_error()
                .unwrap_or(libc::EIO),
        ));
    }
    drop(fd);
    progress
}

/// Commit exactly the positive CQE prefix. Returns the count published by page movement.
///
/// # Safety
/// Both ranges must remain exclusively accessible to this owner, disjoint, valid
/// for `confirmed` bytes, and private anonymous memory with no retained native pins
/// or userfaultfd registrations. The CQE proves src initialized through `confirmed`.
/// Quarantine both owners on an uncertain result.
#[allow(unsafe_code)]
pub(super) unsafe fn publish(
    src: NonNull<u8>,
    dst: NonNull<u8>,
    confirmed: usize,
) -> Result<usize> {
    // SAFETY: the caller retained both exclusive ranges and holds the positive receipt.
    let moved = unsafe { move_prefix(src, dst, confirmed, true)? };
    if moved < confirmed {
        // SAFETY: only the unmoved suffix is read. Its destination was synchronously
        // detached from fault tracking before copying, and the ranges do not overlap.
        unsafe {
            std::ptr::copy_nonoverlapping(
                src.as_ptr().add(moved),
                dst.as_ptr().add(moved),
                confirmed - moved,
            );
        }
    }
    Ok(moved)
}

/// Reuse old destination pages after a full CQE. Missing or occupied spare pages
/// need no initialization. The confirmed new data lives in separate staging.
///
/// # Safety
/// Both ranges satisfy the same exclusive mapping and pin requirements as
/// `publish`. The source bytes are already superseded by the full CQE. The
/// entire source must subsequently be published from authoritative staging.
#[allow(unsafe_code)]
pub(super) unsafe fn recycle(
    src: NonNull<u8>,
    spare: NonNull<u8>,
    disposable: usize,
) -> Result<usize> {
    // SAFETY: these ranges belong exclusively to the caller and old bytes are disposable.
    unsafe { move_prefix(src, spare, disposable, false) }
}
