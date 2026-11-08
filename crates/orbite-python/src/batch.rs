use super::{boundary::*, engine::Engine, memory::transport, records, segments::Target};
use orbite::{Notification, Priority, TransferOp, TransferRequest};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[derive(Default)]
pub(crate) struct Fields {
    pub intent: orbite::IntentType,
    pub policy: Option<orbite::PolicyName>,
    pub deadline: Option<std::time::Instant>,
}

impl Engine {
    pub fn alloc_batch(&self, n: usize) -> Result<u64> {
        let batch = self.core.allocate_batch(n)?;
        let id = super::engine::id()? as u64;
        lock(&self.batches)?.insert(id, Arc::new(Mutex::new(batch)));
        Ok(id)
    }
    pub fn free_batch(&self, id: u64) -> Result<()> {
        let batch = self.batch(id)?;
        lock(&batch)?.free()?;
        lock(&self.batches)?.remove(&id);
        Ok(())
    }

    pub fn submit_rows<'a, T>(
        &self,
        id: u64,
        raw: &'a [T],
        notify: Option<Notification>,
        decode: impl Fn(&'a T) -> Result<(&'a records::Request, Fields)>,
    ) -> Result<()> {
        let batch = self.batch(id)?;
        let mut batch = lock(&batch)?;
        if raw.len()
            > batch
                .capacity()?
                .saturating_sub(batch.status()?.submitted_tasks)
        {
            return Err(FULL);
        }
        let mut targets = HashMap::<u64, Target>::new();
        let mut requests = Vec::with_capacity(raw.len());
        let mut runs = Vec::new();
        for row in raw {
            let (r, fields) = decode(row)?;
            let op = match r.opcode {
                0 => TransferOp::Read,
                1 => TransferOp::Write,
                _ => return Err(INVALID),
            };
            let hint = transport(r.transport_hint)?;
            let priority = match r.priority {
                0 => Priority::High,
                1 => Priority::Medium,
                2 => Priority::Low,
                _ => return Err(INVALID),
            };
            let n = usize::try_from(r.length).map_err(|_| INVALID)?;
            let source = r.source as usize;
            if source == 0
                || n == 0
                || source.checked_add(n).is_none()
                || r.target_offset.checked_add(r.length).is_none()
            {
                return Err(INVALID);
            }
            if let std::collections::hash_map::Entry::Vacant(e) = targets.entry(r.target_id) {
                e.insert(self.target(r.target_id)?);
            }
            if runs
                .last()
                .is_none_or(|&(prior, target, _)| prior != op || target != r.target_id)
            {
                runs.push((op, r.target_id, requests.len()));
            }
            let mut request = TransferRequest::new(hint, source as u64, r.target_offset, n)
                .with_priority(priority);
            request.intent = fields.intent;
            request.policy_name = fields.policy;
            request.deadline = fields.deadline;
            requests.push(request);
        }
        let mut groups = Vec::with_capacity(runs.len());
        for (i, &(op, id, start)) in runs.iter().enumerate() {
            let end = runs.get(i + 1).map_or(requests.len(), |g| g.2);
            groups.push((
                op,
                targets.get(&id).ok_or(INTERNAL)?.view(),
                &requests[start..end],
            ));
        }
        batch.submit_groups(&groups, notify.as_ref(), self.timeout())?;
        Ok(())
    }
}
