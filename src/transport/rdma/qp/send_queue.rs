//! Physical QP send depth and the exact WRs still owned by the provider.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use super::super::quota::DeviceInfo;

use super::super::{RdmaStats, sys};
use crate::{Error, Result};
use orbit_rdma::provider::SendQueueLedger;

pub(in crate::transport::rdma) struct SendQueue {
    state: Mutex<SendQueueLedger>,
    pub(in crate::transport::rdma) bandwidth: Option<Arc<DeviceInfo>>,
    pub(in crate::transport::rdma) usage: Arc<Mutex<RdmaStats>>,
}

impl SendQueue {
    pub(in crate::transport::rdma) fn new(
        limit: usize,
        usage: Arc<Mutex<RdmaStats>>,
        bandwidth: Option<Arc<DeviceInfo>>,
    ) -> Result<Arc<Self>> {
        if !(1..=64).contains(&limit) {
            return Err(Error::InvalidConfig("RDMA QP send depth must be in 1..=64"));
        }
        let ledger = SendQueueLedger::new(limit).map_err(|_| Error::ResourceExhausted)?;
        Ok(Arc::new(Self {
            bandwidth,
            state: Mutex::new(ledger),
            usage,
        }))
    }

    pub(in crate::transport::rdma) fn reserve(
        &self,
        requests: &mut [sys::WorkRequest],
        capacity: usize,
        signal_interval: usize,
    ) -> Result<usize> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::Internal("RDMA SQ lock poisoned"))?;
        let mut usage = self
            .usage
            .lock()
            .map_err(|_| Error::Internal("RDMA budget lock poisoned"))?;
        let count = state
            .reserve(
                requests,
                capacity.saturating_sub(usage.work_requests),
                signal_interval,
            )
            .map_err(|e| match e {
                orbit_rdma::Error::Closed => Error::Unavailable("RDMA QP send queue is fenced"),
                orbit_rdma::Error::Exhausted(_) => Error::ResourceExhausted,
                _ => Error::Protocol("RDMA invalid SQ reservation"),
            })?;
        usage.work_requests += count;
        usage.peak_work_requests = usage.peak_work_requests.max(usage.work_requests);
        Ok(count)
    }

    pub(in crate::transport::rdma) fn posted(
        &self,
        requests: &[sys::WorkRequest],
        receipt: orbit_rdma::verbs::PostReceipt,
        now: Instant,
    ) -> Result<(usize, bool, Option<u64>)> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::Internal("RDMA SQ lock poisoned"))?;
        let code = receipt.code;
        let mut usage = self
            .usage
            .lock()
            .map_err(|_| Error::Internal("RDMA budget lock poisoned"))?;
        usage.post_calls += 1;
        let (owed, uncertain) = state
            .posted(requests, receipt)
            .map_err(|_| Error::Protocol("RDMA SQ post receipt has no reservation"))?;
        usage.work_requests -= requests.len() - owed;
        if uncertain {
            usage.uncertain_posts += 1;
        } else {
            usage.posted_work_requests += owed as u64;
            usage.partial_posts += u64::from(code != 0 && owed != 0);
        }
        drop(usage);
        let bytes = requests[..owed]
            .iter()
            .map(|request| u64::from(request.length))
            .sum();
        let generation = self.bandwidth.as_ref().and_then(|info| {
            let failed = code != 0 || uncertain;
            if bytes == 0 && !failed {
                return None;
            }
            let generation = info.posted(bytes, failed, now);
            (bytes != 0).then_some(generation)
        });
        Ok((owed, uncertain, generation))
    }

    pub(in crate::transport::rdma) fn complete(
        &self,
        prefix: &[sys::WorkRequest],
        successful: usize,
        now: Instant,
    ) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::Internal("RDMA SQ lock poisoned"))?;
        state
            .complete(prefix, successful)
            .map_err(|_| Error::Protocol("RDMA completion has no posted SQ owner"))?;
        let failed = successful < prefix.len();
        let mut usage = self.usage.lock().unwrap_or_else(|p| p.into_inner());
        usage.work_requests -= prefix.len();
        drop(usage);
        if let Some(info) = &self.bandwidth {
            let (ok, errors) = prefix.split_at(successful);
            let bytes =
                |entries: &[sys::WorkRequest]| entries.iter().map(|r| u64::from(r.length)).sum();
            if !ok.is_empty() {
                info.ended(bytes(ok), true, now);
            }
            if failed {
                let bytes = bytes(errors);
                if bytes == 0 {
                    info.invalidate();
                }
                info.ended(bytes, false, now);
            }
        }
        Ok(())
    }

    pub(in crate::transport::rdma) fn outstanding(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .outstanding()
    }

    pub(in crate::transport::rdma) fn fence(&self) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if state.fence() {
            if let Some(info) = &self.bandwidth {
                info.invalidate();
            }
        }
    }

    // Called only after every associated QP and both CQs were destroyed.
    // Dropping a grant or observing a timeout never returns these credits.
    pub(in crate::transport::rdma) fn quiesce(&self) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let mut usage = self.usage.lock().unwrap_or_else(|p| p.into_inner());
        let (count, bytes) = state.quiesce();
        usage.work_requests -= count;
        drop(usage);
        if let Some(info) = &self.bandwidth {
            info.ended(bytes, false, Instant::now());
        }
    }

    pub(in crate::transport::rdma) fn drained(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .drained()
    }
}
