//! Python entry points use the existing retained owners, not a second coordinator.
use crate::{batch, boundary::*, engine, memory::Opt, records, segments::Target, support};
use orbite::{EngineConfig, IntentType};
use pyo3::{create_exception, exceptions::PyException, prelude::*};
mod export;
use std::{
    ffi::c_void,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

create_exception!(orbite, Error, PyException);
macro_rules! exception { ($($name:ident),* $(,)?) => {$(create_exception!(orbite, $name, Error);)*}; }
exception!(
    InvalidConfigError,
    ConfigError,
    InvalidQosConfigError,
    InvalidSelectorConfigError,
    TransportPolicyError,
    BatchBusyError,
    BatchClosedError,
    BatchOpenError,
    RequesterClosedError,
    PeerClosedError,
    InvalidTaskError,
    CancelledError,
    UnquiescedError,
    UnavailableError,
    BackendError,
    InvalidRangeError,
    PermissionDeniedError,
    RegionNotFoundError,
    ForeignRegistrationError,
    FileSegmentClosedError,
    ForeignQosPolicyError,
    RegistrationConflictError,
    SourceClosedError,
    EpochMismatchError,
    ResourceExhaustedError,
    ProtocolError,
    TimeoutError,
    DeadlineInfeasibleError,
    InternalError,
    IoError
);
fn error(code: Code, operation: &str) -> PyErr {
    native_error(code.error(), operation)
}
fn native_error(error: &orbite::Error, operation: &str) -> PyErr {
    let message = format!("{operation}: {error}");
    match error {
        orbite::Error::InvalidConfig(..) => InvalidConfigError::new_err(message),
        orbite::Error::Config { .. } => ConfigError::new_err(message),
        orbite::Error::InvalidQosConfig(..) => InvalidQosConfigError::new_err(message),
        orbite::Error::InvalidSelectorConfig(..) => InvalidSelectorConfigError::new_err(message),
        orbite::Error::TransportPolicy { .. } => TransportPolicyError::new_err(message),
        orbite::Error::BatchBusy => BatchBusyError::new_err(message),
        orbite::Error::BatchClosed => BatchClosedError::new_err(message),
        orbite::Error::BatchOpen => BatchOpenError::new_err(message),
        orbite::Error::RequesterClosed => RequesterClosedError::new_err(message),
        orbite::Error::PeerClosed => PeerClosedError::new_err(message),
        orbite::Error::InvalidTask => InvalidTaskError::new_err(message),
        orbite::Error::Cancelled => CancelledError::new_err(message),
        orbite::Error::Unquiesced { .. } => UnquiescedError::new_err(message),
        orbite::Error::Unavailable(..) => UnavailableError::new_err(message),
        orbite::Error::Backend { .. } => BackendError::new_err(message),
        orbite::Error::InvalidRange => InvalidRangeError::new_err(message),
        orbite::Error::PermissionDenied => PermissionDeniedError::new_err(message),
        orbite::Error::RegionNotFound => RegionNotFoundError::new_err(message),
        orbite::Error::ForeignRegistration => ForeignRegistrationError::new_err(message),
        orbite::Error::FileSegmentClosed => FileSegmentClosedError::new_err(message),
        orbite::Error::ForeignQosPolicy => ForeignQosPolicyError::new_err(message),
        orbite::Error::RegistrationConflict => RegistrationConflictError::new_err(message),
        orbite::Error::SourceClosed => SourceClosedError::new_err(message),
        orbite::Error::EpochMismatch => EpochMismatchError::new_err(message),
        orbite::Error::ResourceExhausted => ResourceExhaustedError::new_err(message),
        orbite::Error::Protocol(..) => ProtocolError::new_err(message),
        orbite::Error::Timeout => TimeoutError::new_err(message),
        orbite::Error::DeadlineInfeasible => DeadlineInfeasibleError::new_err(message),
        orbite::Error::Internal(..) => InternalError::new_err(message),
        orbite::Error::Io(..) => IoError::new_err(message),
        _ => InternalError::new_err(message),
    }
}
fn run<T: Send>(py: Python<'_>, name: &str, f: impl FnOnce() -> Result<T> + Send) -> PyResult<T> {
    py.detach(|| guard(Err(INTERNAL), f))
        .map_err(|e| error(e, name))
}
fn options(v: &Bound<'_, PyAny>) -> PyResult<Opt> {
    Opt::fields(
        &v.getattr("location")?.extract::<String>()?,
        v.getattr("permission")?.extract()?,
        v.getattr("transport")?.extract()?,
        v.getattr("shm_path")?.extract()?,
        v.getattr("shm_offset")?.extract()?,
        v.getattr("internal")?.extract()?,
    )
    .map_err(|e| error(e, "memory options"))
}
fn bounded(v: &Bound<'_, PyAny>, maximum: usize) -> PyResult<usize> {
    let len = v.len()?;
    if len > maximum {
        return Err(error(FULL, "sequence capacity"));
    }
    Ok(len)
}
fn notification(
    v: Option<&Bound<'_, PyAny>>,
    maximum: usize,
) -> PyResult<Option<orbite::Notification>> {
    v.map(|v| {
        let name: String = v.getattr("name")?.extract()?;
        let message: String = v.getattr("message")?.extract()?;
        if name
            .len()
            .checked_add(message.len())
            .is_none_or(|n| n > maximum)
        {
            return Err(error(FULL, "notification capacity"));
        }
        Ok(orbite::Notification::new(name, message))
    })
    .transpose()
}
#[pyclass(name = "_Engine", module = "orbite._native")]
struct PythonEngine {
    handle: usize,
    closed: AtomicBool,
    close_result: Mutex<Option<Result<()>>>,
    regions: usize,
    requests: usize,
    notification_bytes: usize,
}
impl PythonEngine {
    fn with<T: Send>(
        &self,
        py: Python<'_>,
        name: &str,
        f: impl FnOnce(&engine::Engine) -> Result<T> + Send,
    ) -> PyResult<T> {
        run(py, name, || {
            if self.closed.load(Ordering::Acquire) {
                return Err(INVALID);
            }
            engine::with(self.handle, f)
        })
    }
}
impl Drop for PythonEngine {
    fn drop(&mut self) {
        if !self.closed.swap(true, Ordering::AcqRel) {
            // At interpreter shutdown do not enter a finalizing interpreter or
            // release exporters underneath native work. The bounded engine slot is
            // the quarantine owner if Python can no longer safely detach.
            let _ = Python::try_attach(|py| {
                py.detach(|| {
                    guard((), || {
                        let _ = engine::destroy(self.handle);
                    });
                })
            });
        }
    }
}

#[pymethods]
impl PythonEngine {
    #[new]
    #[pyo3(signature = (config=None))]
    fn new(py: Python<'_>, config: Option<PyRef<'_, PythonConfig>>) -> PyResult<Self> {
        let cfg = match config {
            Some(c) => c.config.clone(),
            None => run(py, "configuration", || Ok(EngineConfig::from_env()?))?,
        };
        let (regions, requests, notification_bytes) = (
            cfg.max_registered_regions,
            cfg.max_batch_requests,
            cfg.max_notification_bytes,
        );
        let handle = run(py, "construct engine", || {
            let h = engine::create_using(|| Ok(cfg))?;
            if let Err(err) = engine::with(h, |_| Ok(())) {
                let _ = engine::destroy(h);
                return Err(err);
            }
            Ok(h)
        })?;
        Ok(Self {
            handle,
            closed: AtomicBool::new(false),
            close_result: Mutex::new(None),
            regions,
            requests,
            notification_bytes,
        })
    }
    fn registration_limit(&self) -> usize {
        self.regions
    }
    fn available(&self, py: Python<'_>) -> bool {
        self.with(py, "available", |_| Ok(())).is_ok()
    }
    fn endpoint(&self, py: Python<'_>) -> PyResult<String> {
        self.with(py, "endpoint", |e| Ok(e.core.endpoint().to_string()))
    }
    fn source_epoch(&self, py: Python<'_>) -> PyResult<String> {
        self.with(py, "source_epoch", |e| {
            Ok(e.core.source_epoch().to_string())
        })
    }
    fn stats(&self, py: Python<'_>) -> PyResult<String> {
        self.with(py, "stats", |e| { let s = e.core.stats()?; Ok(serde_json::json!({"deferred_batches":s.deferred_batches,"quarantined_batches":s.quarantined_batches,"unresolved_policy_devices":s.unresolved_policy_devices,"allocated_batches":s.allocated_batches,"allocated_requests":s.allocated_requests,"requester_workers":s.requester_workers,"retained_regions":s.retained_regions,"retained_bytes":s.retained_bytes,"active_source_operations":s.active_source_operations,"peer_segments":s.peer_segments,"live_peer_segments":s.live_peer_segments,"retired_peer_segments":s.retired_peer_segments,"unquiesced_peer_segments":s.unquiesced_peer_segments,"reclaimed_peer_segments":s.reclaimed_peer_segments,"active_peer_operations":s.active_peer_operations,"inflight_batches":s.inflight_batches,"inflight_bytes":s.inflight_bytes,"queued_notifications":s.queued_notifications,"retained_notifications":s.retained_notifications,"retained_notification_bytes":s.retained_notification_bytes}).to_string()) })
    }
    fn local_topology(&self, py: Python<'_>) -> PyResult<String> {
        self.with(py, "local_topology", support::local_topology)
    }
    fn open_segment(&self, py: Python<'_>, endpoint: &str, epoch: &str) -> PyResult<u64> {
        self.with(py, "open_segment", |e| e.open(endpoint, epoch))
    }
    fn open_file_segment(&self, py: Python<'_>, path: &str, permission: i32) -> PyResult<u64> {
        self.with(py, "open_file_segment", |e| e.open_file(path, permission))
    }
    fn close_segment(&self, py: Python<'_>, id: u64) -> PyResult<()> {
        self.with(py, "close_segment", |e| e.close_segment(id))
    }
    #[pyo3(signature = (id=None))]
    #[allow(clippy::type_complexity)]
    fn segment_info(
        &self,
        py: Python<'_>,
        id: Option<u64>,
    ) -> PyResult<(bool, Vec<(u64, u64, String)>)> {
        self.with(py, "segment_info", |e| {
            let (file, rows) = e.info(id)?;
            let rows = rows
                .into_iter()
                .map(|b| {
                    let bytes: Vec<_> = b
                        .location
                        .iter()
                        .take_while(|&&n| n != 0)
                        .map(|&n| n.to_ne_bytes()[0])
                        .collect();
                    Ok((
                        b.base,
                        b.length,
                        String::from_utf8(bytes).map_err(|_| INTERNAL)?,
                    ))
                })
                .collect::<Result<_>>()?;
            Ok((file, rows))
        })
    }
    fn allocate(&self, py: Python<'_>, n: usize, opts: &Bound<'_, PyAny>) -> PyResult<usize> {
        let opt = options(opts)?;
        self.with(py, "allocate_local_memory", |e| {
            Ok(lock(&e.memory)?.allocate(&e.core, n, opt)? as usize)
        })
    }
    fn free(&self, py: Python<'_>, addr: usize) -> PyResult<()> {
        self.with(py, "free_local_memory", |e| {
            lock(&e.memory)?.free(&e.core, addr as *mut c_void)
        })
    }
    fn abandon_memory(&self, py: Python<'_>, addr: usize) -> PyResult<()> {
        self.with(py, "MemoryGuard exit", |e| {
            lock(&e.memory)?.defer_free(&e.core, addr)
        })
    }
    #[pyo3(signature = (addrs, sizes, opts, owners=None))]
    fn register(
        &self,
        py: Python<'_>,
        addrs: &Bound<'_, PyAny>,
        sizes: &Bound<'_, PyAny>,
        opts: &Bound<'_, PyAny>,
        owners: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<()> {
        let count = bounded(addrs, self.regions)?;
        if bounded(sizes, self.regions)? != count
            || owners.is_some_and(|v| v.len().ok() != Some(count))
        {
            return Err(error(INVALID, "registration list sizes"));
        }
        let opt = options(opts)?;
        let mut addresses = Vec::with_capacity(count);
        let mut lengths = Vec::with_capacity(count);
        let mut retained = Vec::with_capacity(count);
        for i in 0..count {
            let address: usize = addrs.get_item(i)?.extract()?;
            let length: usize = sizes.get_item(i)?.extract()?;
            let owner = if let Some(owners) = owners {
                let owner = owners.get_item(i)?;
                if owner.is_none() {
                    None
                } else {
                    let (base, size, owner) = export::get(&owner)?;
                    if address < base
                        || address
                            .checked_add(length)
                            .is_none_or(|end| end > base.saturating_add(size))
                    {
                        return Err(error(INVALID, "range outside exporter"));
                    }
                    Some(owner)
                }
            } else {
                None
            };
            addresses.push(address);
            lengths.push(length);
            retained.push(owner);
        }
        self.with(py, "register_local_memory", |e| {
            let addresses: Vec<_> = addresses.into_iter().map(|n| n as *mut c_void).collect();
            lock(&e.memory)?.register_owned(&e.core, &addresses, &lengths, opt, &retained)
        })
    }
    fn register_buffer(
        &self,
        py: Python<'_>,
        value: &Bound<'_, PyAny>,
        opts: &Bound<'_, PyAny>,
    ) -> PyResult<(usize, usize)> {
        let (address, length, owner) = export::get(value)?;
        let opt = options(opts)?;
        self.with(py, "register_buffer", |e| {
            lock(&e.memory)?.register_owned(
                &e.core,
                &[address as *mut c_void],
                &[length],
                opt,
                &[Some(owner)],
            )?;
            Ok((address, length))
        })
    }
    fn unregister(
        &self,
        py: Python<'_>,
        addrs: &Bound<'_, PyAny>,
        sizes: &Bound<'_, PyAny>,
    ) -> PyResult<()> {
        let count = bounded(addrs, self.regions)?;
        let size_count = bounded(sizes, self.regions)?;
        if size_count != 0 && size_count != count {
            return Err(error(INVALID, "unregister list sizes"));
        }
        let addresses: Vec<usize> = addrs.extract()?;
        let sizes: Vec<usize> = sizes.extract()?;
        self.with(py, "unregister_local_memory", |e| {
            let addresses: Vec<_> = addresses.into_iter().map(|n| n as *mut c_void).collect();
            let mut memory = lock(&e.memory)?;
            memory.unregister(
                &e.core,
                &addresses,
                (!sizes.is_empty()).then_some(sizes.as_slice()),
            )?;
            memory.reap_deferred(&e.core);
            Ok(())
        })
    }
    fn allocate_batch(&self, py: Python<'_>, n: usize) -> PyResult<u64> {
        self.with(py, "allocate_batch", |e| e.alloc_batch(n))
    }
    fn free_batch(&self, py: Python<'_>, id: u64) -> PyResult<()> {
        self.with(py, "free_batch", |e| e.free_batch(id))
    }
    fn abandon_batch(&self, py: Python<'_>, id: u64) -> PyResult<()> {
        self.with(py, "BatchGuard release", |e| {
            // Batch::drop fences logical admission and moves running ownership to
            // its existing deferred retirement owner. It is not a drain receipt.
            let b = lock(&e.batches)?.remove(&id).ok_or(INVALID)?;
            drop(b);
            Ok(())
        })
    }
    #[pyo3(signature = (id, operation, target, rows, note=None))]
    fn submit(
        &self,
        py: Python<'_>,
        id: u64,
        operation: i32,
        target: u64,
        rows: &Bound<'_, PyAny>,
        note: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<()> {
        let n = bounded(rows, self.requests)?;
        let note = notification(note, self.notification_bytes)?;
        // Extract while attached; no Python object is touched by the worker.
        let mut decoded = Vec::with_capacity(n);
        for i in 0..n {
            let r = rows.get_item(i)?;
            let policy: Option<String> = r.getattr("policy_name")?.extract()?;
            if policy.as_ref().is_some_and(|p| p.len() > 256) {
                return Err(error(INVALID, "policy name"));
            }
            decoded.push(Row {
                opcode: operation,
                source: r.getattr("local_address")?.extract()?,
                target,
                offset: r.getattr("remote_address")?.extract()?,
                length: r.getattr("length")?.extract()?,
                priority: r.getattr("priority")?.extract()?,
                hint: r.getattr("transport")?.extract()?,
                policy,
                deadline: r.getattr("deadline_ns")?.extract()?,
                intent: r.getattr("intent")?.extract()?,
            });
        }
        self.with(py, "submit", |e| {
            let clock = decoded
                .iter()
                .any(|r| r.deadline != 0)
                .then(support::clock_pair)
                .transpose()?;
            let rows: Vec<_> = decoded
                .iter()
                .map(|r| {
                    (
                        records::Request {
                            opcode: r.opcode,
                            source: r.source as *mut c_void,
                            target_id: r.target,
                            target_offset: r.offset,
                            length: r.length,
                            priority: r.priority,
                            transport_hint: r.hint,
                        },
                        r,
                    )
                })
                .collect();
            e.submit_rows(id, &rows, note, |(raw, r)| {
                let intent = match r.intent {
                    0 => IntentType::Unspec,
                    1 => IntentType::ForegroundGet,
                    2 => IntentType::BackgroundPrefetch,
                    3 => IntentType::Migration,
                    4 => IntentType::Checkpoint,
                    5 => IntentType::WeightLoading,
                    6 => IntentType::StagingInternal,
                    _ => return Err(INVALID),
                };
                let policy = r
                    .policy
                    .as_ref()
                    .map(|p| {
                        e.defaults
                            .iter()
                            .find(|v| v.as_str() == p)
                            .cloned()
                            .map(Ok)
                            .unwrap_or_else(|| e.core.resolve_policy_name(p))
                    })
                    .transpose()?;
                let deadline = if r.deadline == 0 {
                    None
                } else {
                    let (now, ns) = clock.ok_or(INTERNAL)?;
                    Some(
                        if r.deadline >= ns {
                            now.checked_add(Duration::from_nanos(r.deadline - ns))
                        } else {
                            now.checked_sub(Duration::from_nanos(ns - r.deadline))
                        }
                        .ok_or(INVALID)?,
                    )
                };
                Ok((
                    raw,
                    batch::Fields {
                        policy,
                        deadline,
                        intent,
                    },
                ))
            })
        })
    }
    fn cancel(&self, py: Python<'_>, id: u64, task: usize) -> PyResult<()> {
        self.with(py, "cancel", |e| {
            lock(e.batch(id)?.as_ref())?.cancel(task)?;
            Ok(())
        })
    }
    fn send_notification(
        &self,
        py: Python<'_>,
        id: Option<u64>,
        note: &Bound<'_, PyAny>,
    ) -> PyResult<()> {
        let n = notification(Some(note), self.notification_bytes)?
            .ok_or_else(|| error(INVALID, "notification"))?;
        self.with(py, "send_notification", |e| {
            let Some(id) = id else {
                return Ok(e.core.send_local_notification(&n)?);
            };
            match e.target(id)? {
                Target::Peer(p) => Ok(e.core.send_notification(&p, &n, e.timeout())?),
                _ => Err(UNSUPPORTED),
            }
        })
    }
    fn receive_notifications(&self, py: Python<'_>) -> PyResult<Vec<(String, String)>> {
        self.with(py, "receive_notifications", |e| {
            Ok(e.core
                .receive_notifications()?
                .into_iter()
                .map(|n| (n.name, n.message))
                .collect())
        })
    }
    fn tasks(&self, py: Python<'_>, id: u64) -> PyResult<String> {
        self.with(py, "tasks", |e| {
            task_json(&lock(e.batch(id)?.as_ref())?.observe_tasks(None)?)
        })
    }
    fn progress_batch(&self, py: Python<'_>, id: u64) -> PyResult<String> {
        self.with(py, "progress_batch", |e| {
            let batch = e.batch(id)?;
            let batch = lock(&batch)?;
            e.core.progress_batch(batch.id()?)?;
            task_json(&batch.observe_tasks(None)?)
        })
    }
    fn wait(&self, py: Python<'_>, id: u64, timeout: f64) -> PyResult<String> {
        let duration =
            Duration::try_from_secs_f64(timeout).map_err(|_| error(INVALID, "wait timeout"))?;
        self.with(py, "wait", |e| {
            let deadline = Instant::now().checked_add(duration).ok_or(INVALID)?;
            let b = e.batch(id)?;
            let mut polls = 0u64;
            loop {
                {
                    // Never hold the per-batch lock across a wait: another Python
                    // thread must be able to cancel, observe or retire this batch.
                    let b = lock(&b)?;
                    e.core.progress_batch(b.id()?)?;
                    let tasks = b.observe_tasks(None)?;
                    if tasks.iter().any(|t| {
                        t.failure
                            .as_ref()
                            .is_some_and(|f| matches!(f.error, orbite::Error::Unquiesced { .. }))
                    }) {
                        return Err(UNKNOWN);
                    }
                    if tasks.iter().all(|t| t.state.is_terminal()) && b.locally_drained()? {
                        return task_json(&tasks);
                    }
                }
                if Instant::now() >= deadline {
                    return Err(orbite::Error::Timeout.into());
                }
                orbite::wait_before_next_poll(polls);
                polls = polls.saturating_add(1);
            }
        })
    }
    fn close(&self, py: Python<'_>) -> PyResult<()> {
        run(py, "close", || {
            let mut finished = lock(&self.close_result)?;
            if let Some(result) = finished.clone() {
                return result;
            }
            self.closed.store(true, Ordering::Release);
            let result = guard(Err(INTERNAL), || engine::destroy(self.handle));
            *finished = Some(result.clone());
            result
        })
    }
}
struct Row {
    opcode: i32,
    source: usize,
    target: u64,
    offset: u64,
    length: u64,
    priority: i32,
    hint: i32,
    policy: Option<String>,
    deadline: u64,
    intent: i32,
}

#[pyclass(name = "_Config", module = "orbite._native")]
struct PythonConfig {
    config: EngineConfig,
}
#[pymethods]
impl PythonConfig {
    #[staticmethod]
    fn from_json(py: Python<'_>, json: &str) -> PyResult<Self> {
        run(py, "configuration", || {
            Ok(Self {
                config: EngineConfig::from_json(json)?,
            })
        })
    }
    #[staticmethod]
    fn from_file(py: Python<'_>, path: &str) -> PyResult<Self> {
        run(py, "configuration", || {
            Ok(Self {
                config: EngineConfig::from_file(path)?,
            })
        })
    }
    #[staticmethod]
    fn from_env(py: Python<'_>) -> PyResult<Self> {
        run(py, "configuration", || {
            Ok(Self {
                config: EngineConfig::from_env()?,
            })
        })
    }
}
fn task_json(tasks: &[orbite::TaskStatus]) -> Result<String> {
    let rows: Vec<_> = tasks.iter().map(|t| serde_json::json!({
        "state": format!("{:?}", t.state).to_ascii_lowercase(),
        "transferred_bytes": t.transferred_bytes, "physical_owner": t.physical_owner,
        "transport": format!("{:?}", t.transport), "failover_count": t.failover_count,
        "cancel_requested": t.cancel_requested, "locally_drained": t.locally_drained,
        "failure": t.failure.as_ref().map(|f| serde_json::json!({"message": f.error.to_string(),
            "remote_effect": format!("{:?}", f.remote_effect).to_ascii_lowercase()})),
    })).collect();
    serde_json::to_string(&rows).map_err(|_| INTERNAL)
}
#[pymodule]
fn _native(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PythonEngine>()?;
    m.add_class::<PythonConfig>()?;
    macro_rules! add { ($($e:ident),*) => { $(m.add(stringify!($e), m.py().get_type::<$e>())?;)* }; }
    add!(
        Error,
        InvalidConfigError,
        ConfigError,
        InvalidQosConfigError,
        InvalidSelectorConfigError,
        TransportPolicyError,
        BatchBusyError,
        BatchClosedError,
        BatchOpenError,
        RequesterClosedError,
        PeerClosedError,
        InvalidTaskError,
        CancelledError,
        UnquiescedError,
        UnavailableError,
        BackendError,
        InvalidRangeError,
        PermissionDeniedError,
        RegionNotFoundError,
        ForeignRegistrationError,
        FileSegmentClosedError,
        ForeignQosPolicyError,
        RegistrationConflictError,
        SourceClosedError,
        EpochMismatchError,
        ResourceExhaustedError,
        ProtocolError,
        TimeoutError,
        DeadlineInfeasibleError,
        InternalError,
        IoError
    );
    Ok(())
}
#[cfg(test)]
#[path = "../tests/unit/python.rs"]
mod tests;
