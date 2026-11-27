//! Native device discovery, PD creation, route queries and asynchronous events.

use super::*;

pub(in crate::transport::rdma) fn devices() -> Result<Vec<(String, String)>> {
    provider::devices().map_err(provider_error)
}

pub(super) fn validate_gid_entry(entry: &provider::GidEntry, address: &RdmaAddress) -> Result<()> {
    let (kind, link_layer) = match address.gid_type {
        discovery::GidType::InfiniBand => (ffi::IBV_GID_TYPE_IB, 1),
        discovery::GidType::RoceV1 => (ffi::IBV_GID_TYPE_ROCE_V1, 2),
        discovery::GidType::RoceV2 => (ffi::IBV_GID_TYPE_ROCE_V2, 2),
    };
    if entry.gid_index != u32::from(address.gid_index)
        || entry.port_num != u32::from(address.port)
        || entry.gid != address.descriptor.gid
        || entry.gid_type != kind
        || address.descriptor.link_layer != link_layer
        || entry.ndev_ifindex != address.net_device_index
    {
        return Err(Error::Backend {
            operation: "RDMA route query changed",
            code: libc::ESTALE,
        });
    }
    Ok(())
}

unsafe fn read_address(
    device: *const DeviceHandles,
    expected: &RdmaAddress,
) -> Result<(RdmaAddress, provider::GidEntry)> {
    #[cfg(test)]
    if let Some(result) = tests::take_address_read(device) {
        return result;
    }

    // SAFETY: The caller keeps the device and its selected port alive through both queries.
    let observed = unsafe { provider::read_port(device, expected.port, expected.gid_index) };
    let (descriptor, active, link_speed_mbps) = observed.map_err(|code| Error::Backend {
        operation: "ibverbs route query",
        code,
    })?;
    let descriptor: Descriptor = descriptor.into();
    // SAFETY: the context owner retains this open device through its native GID query.
    let entry = unsafe { provider::gid_entry(&*device, expected.port, expected.gid_index) }
        .map_err(|code| Error::Backend {
            operation: "ibv_query_gid_ex",
            code,
        })?;
    let (gid_type, net_device, net_device_index) = provider::inventory::gid_metadata(
        std::path::Path::new("/sys"),
        &expected.device,
        expected.port,
        expected.gid_index,
        descriptor.link_layer,
    )
    .map_err(provider_error)?;
    let observed = RdmaAddress {
        active,
        link_speed_mbps,
        descriptor,
        gid_type,
        net_device,
        net_device_index,
        ..expected.clone()
    };
    Ok((observed, entry))
}

pub(super) unsafe fn query_address(
    device: *const DeviceHandles,
    expected: &RdmaAddress,
    gid_index: Option<u8>,
) -> Result<RdmaAddress> {
    if let Some(gid_index) = gid_index {
        let pinned = RdmaAddress {
            gid_index,
            ..expected.clone()
        };
        // SAFETY: The caller's device handle stays live while querying the pinned GID index.
        return unsafe { query_selected_address(device, &pinned) };
    }
    let candidates = discovery::port_addresses(
        std::path::Path::new("/sys"),
        &expected.device,
        expected.port,
        None,
    );
    let mut error = Error::Unavailable("no valid GID on the selected RDMA port");
    for candidate in candidates {
        // SAFETY: Each candidate is queried synchronously against the same caller-held device.
        match unsafe { query_selected_address(device, &candidate) } {
            Ok(address) => return Ok(address),
            Err(fault) => error = fault,
        }
    }
    Err(error)
}

unsafe fn query_selected_address(
    device: *const DeviceHandles,
    expected: &RdmaAddress,
) -> Result<RdmaAddress> {
    // SAFETY: The caller retains the open device throughout the selected address query.
    let (observed, entry) = unsafe { read_address(device, expected) }?;
    validate_gid_entry(&entry, &observed)?;
    if entry.ndev_ifindex != 0
        && !provider::inventory::netdev_matches(entry.ndev_ifindex, observed.net_device.as_deref())
            .map_err(provider_error)?
    {
        return Err(Error::Backend {
            operation: "RDMA network device changed",
            code: libc::ESTALE,
        });
    }
    Ok(observed)
}

pub(super) fn endpoint_address(
    observed: std::result::Result<Descriptor, c_int>,
    expected: &Descriptor,
) -> std::result::Result<Descriptor, c_int> {
    match observed {
        Ok(address) if address == *expected => Ok(address),
        _ => Err(libc::ESTALE),
    }
}
