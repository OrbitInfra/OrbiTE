//! Per-PD, non-repeating device keys. A management-QPN probe uses the actual GPU
//! parent MR. The advertised width is pinned to the resulting path domains.
use super::*;

impl RdmaContext {
    pub(in crate::transport::rdma) fn probe_gpu_parent(
        &self,
        memory: &RegisteredMemory,
        handles: &mut sys::MemoryHandles,
    ) -> Result<Option<[u8; 16]>> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::Internal("RDMA GPU key probe lock"))?;
        let State::Open(d) = &mut *state else {
            return Err(Error::Unavailable("RDMA device is closed or retained"));
        };
        if d.handles.gpu_keys.count == 0 {
            return Ok(None);
        }
        let mut supported = true;
        for permission in [1, 2] {
            let result = d.create_gpu_key(
                self.verbs,
                handles,
                sys::GPU_PROBE_QPN,
                memory.base_address(),
                1,
                permission,
                true,
            );
            // If the firmware returned an object, even a failed query must release it.
            handles
                .destroy_gpu_key(self.verbs)
                .map_err(|(operation, code)| Error::Unquiesced { operation, code })?;
            if let Err(error) = result {
                log::debug!("GPU source grant-key probe unavailable: {error}");
                supported = false;
                break;
            }
        }
        Ok(supported.then_some(d.domain))
    }

    pub(in crate::transport::rdma) fn bind_gpu_grant(
        &self,
        grant: &mut super::super::GrantResources,
        qpn: u32,
    ) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::Internal("RDMA GPU grant-key lock"))?;
        let State::Open(d) = &mut *state else {
            return Err(Error::Unavailable("RDMA device is closed or retained"));
        };
        let addr = grant.buffer.address().as_ptr() as u64;
        let length = u32::try_from(grant.buffer.len()).map_err(|_| Error::InvalidRange)?;
        let key = d.create_gpu_key(
            self.verbs,
            &mut grant.handles,
            qpn,
            addr,
            length,
            grant.permission,
            false,
        )?;
        grant.rkey = key;
        grant.window_bound = true;
        grant.window_used = true;
        d.usage
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .opened_gpu_grant_keys += 1;
        Ok(())
    }
}

impl Resources {
    #[allow(clippy::too_many_arguments)]
    fn create_gpu_key(
        &mut self,
        verbs: &sys::Verbs,
        memory: &mut sys::MemoryHandles,
        qpn: u32,
        addr: u64,
        length: u32,
        permission: u32,
        probe: bool,
    ) -> Result<u32> {
        let ops = verbs.gpu_keys.ok_or(Error::Unavailable(
            "provider has no revocable GPU grant keys",
        ))?;
        let cursor = self
            .gpu_key_cursor
            .as_mut()
            .ok_or(Error::ResourceExhausted)?;
        for _ in 0..self.gpu_key_attempts {
            let before = self.keys.len();
            let candidate = cursor
                .claim(&mut self.keys, 1)
                .map_err(|_| Error::ResourceExhausted)?;
            self.usage
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .window_key_indices += self.keys.len() - before;
            let Some(key) = candidate else {
                continue;
            };
            // SAFETY: the caller retains the local-only parent MR, actual range
            // and PD. The index and tag have been burned before any firmware call.
            let code = unsafe {
                (ops.create)(
                    &self.handles,
                    memory,
                    qpn,
                    addr,
                    length,
                    permission,
                    key,
                    probe,
                )
            };
            if matches!(code, libc::EBUSY | libc::EEXIST) && memory.gpu_key.is_null() {
                continue;
            }
            if memory.gpu_key.is_null()
                && !matches!(
                    code,
                    libc::EINVAL
                        | libc::EOPNOTSUPP
                        | libc::EACCES
                        | libc::EPERM
                        | libc::ENOSYS
                        | libc::ENOMEM
                        | libc::ENOSPC
                )
            {
                let code = if code == 0 { libc::EIO } else { code };
                memory.gpu_key_failure = code;
                return Err(Error::Unquiesced {
                    operation: "mlx5 DevX GPU key creation",
                    code,
                });
            }
            check("mlx5 DevX GPU grant key", code)?;
            return Ok(key);
        }
        Err(Error::ResourceExhausted)
    }
}
