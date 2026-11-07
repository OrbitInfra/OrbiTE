use std::collections::BTreeMap;

#[cfg(feature = "rdma")]
mod config;

/// TENT's three NIC priority ranks, ordered from preferred to fallback.
pub const DEVICE_PRIORITY_RANKS: usize = 3;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum NicType {
    Rdma,
}

/// TENT Topology::TopologyMemoryKind: a topology location kind, not a registration backing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum TopologyMemoryKind {
    Host,
    Cuda,
}

/// A provider-visible NIC. Unknown PCI or NUMA placement stays unknown.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct NicEntry {
    pub name: String,
    pub pci_bus_id: Option<String>,
    pub kind: NicType,
    pub numa_node: Option<u32>,
    /// Namespaced transport metadata; only rdma.selected marks a validated path.
    pub device_attrs: BTreeMap<String, String>,
}

/// A memory location and its ranked indices into Topology::nic_list.
/// Only listed NICs are eligible; masks and physical locality further restrict them.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct MemEntry {
    pub name: String,
    pub pci_bus_id: Option<String>,
    pub kind: TopologyMemoryKind,
    pub numa_node: Option<u32>,
    pub device_list: [Vec<usize>; DEVICE_PRIORITY_RANKS],
}

/// An immutable inventory snapshot, not a claim about allocation placement.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub struct Topology {
    pub nic_list: Vec<NicEntry>,
    pub mem_list: Vec<MemEntry>,
}

#[cfg(feature = "rdma")]
impl Topology {
    pub(crate) fn rank(&mut self) {
        for memory in self
            .mem_list
            .iter_mut()
            .filter(|entry| entry.kind == TopologyMemoryKind::Host)
        {
            memory.device_list = Default::default();
            for (index, nic) in self.nic_list.iter().enumerate() {
                let rank = if memory.numa_node.is_some() && memory.numa_node == nic.numa_node {
                    0
                } else {
                    2
                };
                memory.device_list[rank].push(index);
            }
        }
    }
}

impl NicEntry {
    /// Describe a named NIC with unknown placement and no transport attributes.
    pub fn new(name: impl Into<String>, kind: NicType) -> Self {
        Self {
            name: name.into(),
            pci_bus_id: None,
            kind,
            numa_node: None,
            device_attrs: BTreeMap::new(),
        }
    }
}

impl MemEntry {
    /// Describe a memory location with no eligible NICs until device_list is set.
    /// Each list contains indices into the containing topology's nic_list.
    pub fn new(name: impl Into<String>, kind: TopologyMemoryKind) -> Self {
        Self {
            name: name.into(),
            pci_bus_id: None,
            kind,
            numa_node: None,
            device_list: Default::default(),
        }
    }
}
