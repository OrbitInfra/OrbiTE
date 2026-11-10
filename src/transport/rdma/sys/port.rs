//! Public libibverbs port-query ABI and optional effective-rate symbol.

use super::{DeviceHandles, c_int, ffi};
use crate::{Error, Result};

use super::ffi as shim;

type QueryPortSpeed = super::native::PortSpeed;

pub(super) unsafe fn query_port(
    context: *mut ffi::ibv_context,
    port: u8,
) -> std::result::Result<shim::orbite_port_attr, c_int> {
    let mut attr = shim::orbite_port_attr::default();
    // The C compiler uses the system's verbs inline and provider layout. Only
    // our generated, fixed-width result crosses back into Rust.
    let legacy = super::native::get().ok().map(|table| table.ibv_query_port);
    // SAFETY: The caller retains the verbs context and the C shim writes into its generated
    // output type.
    let code = unsafe { shim::orbite_query_port(context, port, &mut attr, legacy) };
    if code == 0 { Ok(attr) } else { Err(code) }
}

pub(super) fn link_speed_mbps(attr: &shim::orbite_port_attr) -> Option<u64> {
    let speed = attr.active_speed;
    let lane = match speed {
        1 => 2_500,
        2 => 5_000,
        4 | 8 => 10_000,
        16 => 14_000,
        32 => 25_000,
        64 => 50_000,
        128 => 100_000,
        256 => 200_000,
        _ => return None,
    };
    let lanes = match attr.active_width {
        1 => 1,
        2 => 4,
        4 => 8,
        8 => 12,
        16 => 2,
        _ => return None,
    };
    Some(lane * lanes)
}

pub(super) unsafe fn query_port_speed(
    device: *const DeviceHandles,
    port: u8,
) -> Result<Option<u64>> {
    match super::native::get()?.port_speed {
        // SAFETY: the caller owns the open context; the process-lifetime table owns the symbol.
        Some(query) => unsafe { effective_speed(query, (*device).context.cast(), port).map(Some) },
        None => Ok(None),
    }
}

unsafe fn effective_speed(
    query: QueryPortSpeed,
    context: *mut ffi::ibv_context,
    port: u8,
) -> Result<u64> {
    let mut speed = 0;
    // SAFETY: The retained context and port are passed to the loaded ABI with writable u64
    // output.
    let code = unsafe { query(context, u32::from(port), &mut speed) };
    if code != 0 {
        return Err(Error::Backend {
            operation: "ibv_query_port_speed",
            code,
        });
    }
    // The native result is in 100 Mb/s units, not Mb/s or bytes/s.
    speed.checked_mul(100).ok_or(Error::Backend {
        operation: "ibv_query_port_speed overflow",
        code: libc::EOVERFLOW,
    })
}

#[cfg(test)]
#[path = "../../../../tests/unit/transport/rdma/sys/port.rs"]
mod tests;
