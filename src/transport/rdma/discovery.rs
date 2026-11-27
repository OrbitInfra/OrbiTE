use std::net::Ipv6Addr;
use std::path::Path;

use super::{Descriptor, RdmaConfig, sys};
use crate::platform::cpu_probe;
use crate::runtime::topology::Topology;
use crate::{Error, Result};

pub(super) use orbit_rdma::Link as GidType;
use orbit_rdma::provider::inventory;

/// Selected port address and observed link state; publication waits for native drain.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct RdmaAddress {
    pub device: String,
    pub port: u8,
    pub active: bool,
    /// Effective rate when reported, otherwise encoded rate; None means unknown.
    pub link_speed_mbps: Option<u64>,
    pub effective_speed_mbps: Option<u64>,
    pub effective_speed_query_failures: u64,
    pub effective_speed_query_failing: bool,
    pub gid_index: u8,
    pub gid_type: GidType,
    pub net_device: Option<String>,
    pub net_device_index: u32,
    pub descriptor: Descriptor,
}

impl RdmaAddress {
    pub(super) fn same_route(&self, other: &Self) -> bool {
        self.device == other.device
            && self.port == other.port
            && self.active == other.active
            && self.gid_index == other.gid_index
            && self.gid_type == other.gid_type
            && self.net_device == other.net_device
            && self.net_device_index == other.net_device_index
            && self.descriptor == other.descriptor
    }

    pub(super) fn publish(&self, topology: &mut Topology) -> Result<()> {
        let nic = topology
            .nic_list
            .iter_mut()
            .find(|nic| nic.name == self.device)
            .ok_or(Error::Internal(
                "selected RDMA device is missing from topology",
            ))?;
        let attrs = &mut nic.device_attrs;
        for (key, rate) in [
            ("rdma.link_speed_mbps", self.link_speed_mbps),
            ("rdma.effective_speed_mbps", self.effective_speed_mbps),
        ] {
            if let Some(rate) = rate {
                attrs.insert(key.into(), rate.to_string());
            } else {
                attrs.remove(key);
            }
        }
        for (key, value) in [
            ("rdma.selected", "true".into()),
            (
                "rdma.effective_speed_query_failures",
                self.effective_speed_query_failures.to_string(),
            ),
            (
                "rdma.effective_speed_query_failing",
                self.effective_speed_query_failing.to_string(),
            ),
            ("rdma.port", self.port.to_string()),
            ("rdma.gid_index", self.gid_index.to_string()),
            ("rdma.gid", Ipv6Addr::from(self.descriptor.gid).to_string()),
            ("rdma.gid_type", self.gid_type.name().into()),
            ("rdma.net_device_index", self.net_device_index.to_string()),
            ("rdma.lid", self.descriptor.lid.to_string()),
            (
                "rdma.mtu_bytes",
                (128u32 << self.descriptor.mtu).to_string(),
            ),
            ("rdma.link_layer", self.descriptor.link_layer.to_string()),
        ] {
            attrs.insert(key.into(), value);
        }
        if let Some(net_device) = &self.net_device {
            attrs.insert("rdma.net_device".into(), net_device.clone());
        }
        Ok(())
    }
}

pub(super) fn permitted(config: &RdmaConfig, device: &str) -> bool {
    config.device.as_deref().is_none_or(|pin| pin == device)
        && if config.rdma_whitelist.is_empty() {
            !config.rdma_blacklist.iter().any(|name| name == device)
        } else {
            config.rdma_whitelist.iter().any(|name| name == device)
        }
}

pub(super) fn discover(config: &RdmaConfig) -> Result<(Topology, Vec<RdmaAddress>)> {
    let devices = sys::devices()?;
    let (topology, routes) = discover_at(config, Path::new("/sys"), Path::new("/dev"), &devices)?;
    #[cfg(feature = "cuda")]
    let topology = {
        let mut topology = topology;
        crate::cuda::discover_topology(&mut topology, Path::new("/sys"))?;
        topology
    };
    Ok((topology, routes))
}

fn discover_at(
    config: &RdmaConfig,
    sys: &Path,
    dev: &Path,
    devices: &[(String, String)],
) -> Result<(Topology, Vec<RdmaAddress>)> {
    let mut names: Vec<_> = devices
        .iter()
        // Filtering precedes every access check and every native open.
        .filter(|(name, _)| permitted(config, name))
        .filter(|(_, node)| inventory::accessible(dev, node))
        .map(|(name, _)| name.clone())
        .collect();
    names.sort();
    names.dedup();
    let mut topology = cpu_probe::discover(sys, &names);
    let mut candidates = Vec::new();
    for name in &names {
        for port in inventory::ports(sys, name) {
            if config.port.is_some_and(|pin| pin != port) {
                continue;
            }
            candidates.extend(port_addresses(sys, name, port, config.gid_index));
        }
    }
    // A NIC is inventoried only when at least one route satisfies the selectors.
    topology
        .nic_list
        .retain(|nic| candidates.iter().any(|route| route.device == nic.name));
    topology.rank();
    candidates.sort_by_key(|route| {
        let nic = topology
            .nic_list
            .iter()
            .find(|nic| nic.name == route.device);
        let local = config.preferred_numa_node.is_some()
            && nic.is_some_and(|nic| nic.numa_node == config.preferred_numa_node);
        (
            !route.active,
            !local,
            gid_priority(sys, route),
            route.device.clone(),
            route.port,
            route.gid_index,
        )
    });
    if candidates.is_empty() {
        return Err(Error::Unavailable(
            "no accessible RDMA route matches selectors",
        ));
    }
    Ok((topology, candidates))
}

fn gid_priority(sys: &Path, route: &RdmaAddress) -> (bool, bool, bool, bool) {
    let overlay = route
        .net_device
        .as_ref()
        .is_some_and(|name| inventory::virtual_netdev(sys, name));
    let preferred = matches!(route.gid_type, GidType::InfiniBand | GidType::RoceV2);
    (
        !preferred,
        route.net_device.is_none(),
        overlay,
        Ipv6Addr::from(route.descriptor.gid).is_unicast_link_local(),
    )
}

pub(super) fn port_addresses(
    sys: &Path,
    device: &str,
    port: u8,
    gid_index: Option<u8>,
) -> Vec<RdmaAddress> {
    let mut candidates: Vec<_> = inventory::port_addresses(sys, device, port, gid_index)
        .into_iter()
        .map(|address| RdmaAddress {
            device: device.to_owned(),
            port,
            active: address.active,
            link_speed_mbps: None,
            effective_speed_mbps: None,
            effective_speed_query_failures: 0,
            effective_speed_query_failing: false,
            gid_index: address.gid_index,
            gid_type: address.link,
            net_device: address.net_device,
            net_device_index: address.net_device_index,
            descriptor: Descriptor {
                gid: address.gid,
                link_layer: address.link_layer,
                ..Descriptor::default()
            },
        })
        .collect();
    candidates.sort_by_key(|route| (gid_priority(sys, route), route.gid_index));
    candidates
}

#[cfg(test)]
#[path = "../../../tests/unit/transport/rdma/discovery.rs"]
mod tests;
