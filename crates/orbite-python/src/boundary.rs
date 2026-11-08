use orbite::{Error, NotificationEffect, NotificationFailure};
use std::ffi::c_char;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::NonNull;
use std::sync::{Arc, Mutex, MutexGuard};

#[derive(Clone, Debug)]
pub(crate) enum Code {
    Native(Arc<Error>),
    Invalid,
    Full,
    Address,
    Unknown,
    Internal,
    Unsupported,
}
pub(crate) type Result<T> = std::result::Result<T, Code>;
pub(crate) const INVALID: Code = Code::Invalid;
pub(crate) const FULL: Code = Code::Full;
pub(crate) const ADDRESS: Code = Code::Address;
pub(crate) const UNKNOWN: Code = Code::Unknown;
pub(crate) const INTERNAL: Code = Code::Internal;
pub(crate) const UNSUPPORTED: Code = Code::Unsupported;
impl Code {
    pub fn error(&self) -> &Error {
        match self {
            Self::Native(e) => e,
            Self::Invalid => &Error::InvalidRange,
            Self::Full => &Error::ResourceExhausted,
            Self::Address => &Error::RegionNotFound,
            Self::Unknown => &Error::Unquiesced {
                operation: "Python owner cleanup",
                code: -1,
            },
            Self::Internal => &Error::Internal("Python boundary"),
            Self::Unsupported => &Error::Unavailable("requested facility is not available"),
        }
    }
    pub fn unquiesced(&self) -> bool {
        matches!(self.error(), Error::Unquiesced { .. })
    }
}
impl From<Error> for Code {
    fn from(e: Error) -> Self {
        Self::Native(Arc::new(e))
    }
}
impl From<NotificationFailure> for Code {
    fn from(f: NotificationFailure) -> Self {
        if f.effect == NotificationEffect::Unknown {
            UNKNOWN
        } else {
            f.error.into()
        }
    }
}
pub(crate) fn guard<T>(fallback: T, f: impl FnOnce() -> T) -> T {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(out) => out,
        Err(payload) => {
            // A foreign logger may panic with a payload whose destructor panics too.
            if let Err(second) = catch_unwind(AssertUnwindSafe(|| drop(payload))) {
                std::mem::forget(second);
            }
            fallback
        }
    }
}
pub(crate) fn lock<T>(m: &Mutex<T>) -> Result<MutexGuard<'_, T>> {
    m.lock().map_err(|_| INTERNAL)
}

// Validate representable bounds before checking the retained exporter extent.
pub(crate) fn checked<T>(p: *mut T, n: usize) -> Result<NonNull<T>> {
    let bytes = n
        .checked_mul(size_of::<T>())
        .filter(|n| *n <= isize::MAX as usize)
        .ok_or(INVALID)?;
    if !p.is_aligned() || (p as usize).checked_add(bytes).is_none() {
        return Err(INVALID);
    }
    NonNull::new(p).ok_or(INVALID)
}
pub(crate) fn fixed<const N: usize>(s: &str) -> [c_char; N] {
    let mut out = [0; N];
    for (dst, src) in out[..N - 1].iter_mut().zip(s.bytes()) {
        *dst = src as c_char;
    }
    out
}
