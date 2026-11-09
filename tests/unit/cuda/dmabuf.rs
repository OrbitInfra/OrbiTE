//! CPU driver/verbs controls. No GPU registration is exercised on this host.
use super::*;
use crate::engine::tests::{rdma_fixture_with_runtime, source_runtime};
use crate::test_support::EVENTUAL_PROGRESS;
use crate::transport::rdma::{GpuDirectRegistration as Policy, tests as verbs};
use std::collections::BTreeMap;
use std::ffi::{c_int, c_void};
use std::os::fd::IntoRawFd;
use std::sync::{OnceLock, Weak};

#[derive(Default)]
pub(super) struct State {
    pub supported: i32,
    export_error: bool,
    partial_export_error: bool,
    extent_trim: usize,
    unaligned_base: bool,
    register_error: bool,
    close_error: bool,
    version: i32,
    exports: Vec<(u64, usize, u64, i32)>,
    calls: Vec<(usize, u64, usize, u64, i32, i32)>,
    closed: Vec<i32>,
}
fn fds() -> &'static Mutex<BTreeMap<i32, Weak<shim::Device>>> {
    static FDS: OnceLock<Mutex<BTreeMap<i32, Weak<shim::Device>>>> = OnceLock::new();
    FDS.get_or_init(Default::default)
}
unsafe extern "C" fn range(base: *mut u64, size: *mut usize, addr: u64) -> driver::Status {
    let owner = shim::device_at(addr);
    // SAFETY: the shim owns the boxed storage and never relocates it.
    let bytes = unsafe { &*owner.bytes.get() };
    let first = (bytes.as_ptr() as u64 + 4095) & !4095;
    let length = ((bytes.as_ptr() as u64 + bytes.len() as u64 - first) & !4095) as usize;
    // SAFETY: CUDA supplies writable native extent outputs.
    unsafe {
        *base = first + u64::from(owner.dmabuf.lock().unwrap().unaligned_base);
        *size = length - owner.dmabuf.lock().unwrap().extent_trim;
    }
    SUCCESS
}
unsafe extern "C" fn version(out: *mut c_int) -> driver::Status {
    // SAFETY: CUDA supplies a writable integer.
    unsafe {
        *out = shim::current().dmabuf.lock().unwrap().version;
    }
    SUCCESS
}
unsafe extern "C" fn export(
    out: *mut c_void,
    base: u64,
    length: usize,
    kind: sys::CUmemRangeHandleType,
    flags: u64,
) -> driver::Status {
    assert_eq!(
        kind,
        sys::CUmemRangeHandleType::CU_MEM_RANGE_HANDLE_TYPE_DMA_BUF_FD
    );
    assert_eq!(base % 4096, 0);
    assert_eq!(length % 4096, 0);
    let owner = shim::device_at(base);
    let mut state = owner.dmabuf.lock().unwrap();
    if state.export_error {
        return driver::Status::CUDA_ERROR_UNKNOWN;
    }
    let fd = std::fs::File::open("/dev/null").unwrap().into_raw_fd();
    fds().lock().unwrap().insert(fd, Arc::downgrade(&owner));
    state.exports.push((base, length, flags, fd));
    // SAFETY: CUDA supplies one writable fd output.
    unsafe {
        *out.cast::<i32>() = fd;
    }
    if state.partial_export_error {
        driver::Status::CUDA_ERROR_UNKNOWN
    } else {
        SUCCESS
    }
}
unsafe extern "C" fn close(fd: c_int) -> c_int {
    let owner = fds()
        .lock()
        .unwrap()
        .remove(&fd)
        .unwrap()
        .upgrade()
        .unwrap();
    let mut state = owner.dmabuf.lock().unwrap();
    state.closed.push(fd);
    // SAFETY: the fixture exported this fd and asserts exactly one close above.
    assert_eq!(unsafe { libc::close(fd) }, 0);
    if state.close_error { -1 } else { 0 }
}
pub(crate) unsafe extern "C" fn register_dmabuf(
    pd: *mut c_void,
    offset: u64,
    length: usize,
    iova: u64,
    fd: c_int,
    access: c_int,
) -> *mut c_void {
    let owner = fds().lock().unwrap().get(&fd).unwrap().upgrade().unwrap();
    let mut state = owner.dmabuf.lock().unwrap();
    let &(base, extent, _, _) = state.exports.iter().find(|e| e.3 == fd).unwrap();
    assert_eq!(offset, iova - base);
    assert!(offset + length as u64 <= extent as u64);
    state
        .calls
        .push((pd as usize, offset, length, iova, fd, access));
    if state.register_error {
        std::ptr::null_mut()
    } else {
        std::ptr::dangling_mut::<u8>().cast()
    }
}
fn backing() -> (Arc<shim::Device>, Arc<CudaMemory>) {
    backing_with_export(true)
}
fn backing_with_export(available: bool) -> (Arc<shim::Device>, Arc<CudaMemory>) {
    let owner = allocation(&vec![0x5a; 4096 * 3]);
    {
        let mut s = owner.dmabuf.lock().unwrap();
        s.supported = 1;
        s.version = 13000;
    }
    let mut d = Arc::try_unwrap(driver()).ok().unwrap();
    d.dmabuf_export = available.then_some(export);
    d.address_range = range;
    d.version = version;
    d.close_fd = close;
    let d = Arc::new(d);
    // SAFETY: the boxed allocation remains owned and fixed in the shim Device.
    let address = (unsafe { &*owner.bytes.get() }.as_ptr() as u64 + 4095) & !4095;
    let memory = Arc::new(CudaMemory::register(address, 8192, owner.clone(), d).unwrap());
    (owner, memory)
}
fn engine(mode: &str, policy: Policy, verb: bool, peermem: bool, reuse: bool) -> TransferEngine {
    rdma_fixture_with_runtime(verbs::dmabuf_runtime(mode, policy, verb, peermem, reuse))
}

#[test]
fn dmabuf_route_selection_and_old_rdma_core_are_explicit_and_fail_closed() {
    for (policy, attribute, verb, peer, route) in [
        (Policy::Auto, 1, true, true, 1),
        (Policy::Auto, 0, true, true, 2),
        (Policy::Auto, 1, false, true, 2),
        (Policy::Auto, 0, true, false, 0),
        (Policy::Dmabuf, 0, true, true, 0),
        (Policy::Dmabuf, 1, false, true, 0),
        (Policy::Peermem, 1, true, false, 0),
        (Policy::Peermem, 1, true, true, 2),
    ] {
        let (owner, memory) = backing();
        owner.dmabuf.lock().unwrap().supported = attribute;
        let e = engine("direct:copy", policy, verb, peer, false);
        let result = e.register_cuda_memory_batch(&[(memory, 31, 55)], Permission::RemoteReadWrite);
        if route == 0 {
            assert!(
                matches!(result, Err(Error::Unavailable(_))),
                "{policy:?}: {result:?}"
            );
        } else {
            let view = result.unwrap().remove(0);
            let peer = e
                .open_segment(e.endpoint(), e.source_epoch(), EVENTUAL_PROGRESS)
                .unwrap();
            let evidence = peer.regions()[0].gpu_direct_routes();
            assert_eq!(
                (evidence.dmabuf, evidence.peermem),
                (route == 1, route == 2)
            );
            let s = e.stats().unwrap().rdma.unwrap();
            assert_eq!(
                (s.dmabuf_registrations, s.peermem_registrations),
                (u64::from(route == 1), u64::from(route == 2))
            );
            e.unregister_memory(&view).unwrap();
        }
        e.close(EVENTUAL_PROGRESS).unwrap();
        let s = owner.dmabuf.lock().unwrap();
        assert_eq!(s.exports.len(), usize::from(route == 1));
        assert_eq!(s.closed.len(), s.exports.len());
        if route == 1 {
            assert_eq!(
                (
                    s.calls[0].1,
                    s.calls[0].2,
                    s.calls[0].3,
                    s.calls[0].4,
                    s.calls[0].5
                ),
                (31, 55, s.exports[0].0 + 31, s.exports[0].3, 1)
            );
        }
    }
}

#[test]
fn dmabuf_export_or_registration_error_never_calls_the_pointer_registration() {
    for failure in ["export", "partial_export", "register"] {
        let (owner, memory) = backing();
        {
            let mut s = owner.dmabuf.lock().unwrap();
            s.export_error = failure == "export";
            s.partial_export_error = failure == "partial_export";
            s.register_error = failure == "register";
        }
        let e = engine("direct:copy", Policy::Auto, true, true, false);
        let result = e.register_cuda_memory_batch(&[(memory, 8, 24)], Permission::RemoteReadWrite);
        assert!(matches!(result, Err(Error::Backend { .. })), "{result:?}");
        let s = e.stats().unwrap().rdma.unwrap();
        assert_eq!(
            (
                s.dmabuf_registrations,
                s.peermem_registrations,
                s.persistent_mrs
            ),
            (0, 0, 0)
        );
        e.close(EVENTUAL_PROGRESS).unwrap();
        let s = owner.dmabuf.lock().unwrap();
        assert_eq!(s.calls.len(), usize::from(failure == "register"));
        assert_eq!(s.closed.len(), s.exports.len());
    }
}

#[test]
fn dmabuf_private_grants_and_reusable_gpu_parents_use_one_registration_boundary() {
    for reuse in [false, true] {
        let (owner, backing) = backing();
        let source = engine("gpu_key_copy", Policy::Dmabuf, true, false, reuse);
        let requester = rdma_fixture_with_runtime(verbs::runtime(if reuse {
            "direct:copy_check_gpu_keys"
        } else {
            "direct:copy"
        }));
        let remote = source
            .register_cuda_memory_batch(&[(backing.clone(), 17, 24)], Permission::RemoteReadWrite)
            .unwrap()
            .remove(0);
        let local = requester
            .register_memory(vec![0xcc; 12].into_boxed_slice(), Permission::LocalOnly)
            .unwrap();
        let peer = requester
            .open_segment(source.endpoint(), source.source_epoch(), EVENTUAL_PROGRESS)
            .unwrap();
        for op in [TransferOp::Read, TransferOp::Write] {
            let q = TransferRequest::new(
                Transport::Rdma,
                local.base_address() + 2,
                remote.base_address() + 4,
                8,
            );
            if op == TransferOp::Write {
                local.write(2, &[0x7e; 8]).unwrap();
            }
            let r = requester
                .submit_and_wait(op, &peer, &[q], EVENTUAL_PROGRESS)
                .unwrap();
            assert!(r.is_complete() && r.locally_drained, "{r:?}");
            let mut bytes = [0; 12];
            local.read(0, &mut bytes).unwrap();
            assert_eq!(bytes[..2], [0xcc; 2]);
            assert_eq!(bytes[10..], [0xcc; 2]);
            assert_eq!(
                bytes[2..10],
                [if op == TransferOp::Read { 0x5a } else { 0x7e }; 8]
            );
        }
        let mut bytes = [0; 48];
        backing.read(0, &mut bytes).unwrap();
        let mut expected = [0x5a; 48];
        expected[21..29].fill(0x7e);
        assert_eq!(bytes, expected);
        let s = source.stats().unwrap().rdma.unwrap();
        assert_eq!(s.dmabuf_registrations, if reuse { 1 } else { 3 });
        assert_eq!(s.opened_grant_pds, if reuse { 0 } else { 2 });
        source.unregister_memory(&remote).unwrap();
        requester.close(EVENTUAL_PROGRESS).unwrap();
        source.close(EVENTUAL_PROGRESS).unwrap();
        let s = owner.dmabuf.lock().unwrap();
        assert_eq!(s.exports.len(), if reuse { 1 } else { 3 });
        assert_eq!(s.closed.len(), s.exports.len());
        assert!(s.calls.iter().all(|c| c.4 >= 0));
        if !reuse {
            assert_eq!(s.calls[1].5, 5);
            assert_eq!(s.calls[2].5, 3);
        }
    }
}

#[test]
fn dmabuf_failed_deregister_or_fd_close_keeps_backing_context_and_charges_without_retry() {
    for close_fd in [false, true] {
        let (owner, memory) = backing();
        let weak = Arc::downgrade(&owner);
        owner.dmabuf.lock().unwrap().close_error = close_fd;
        let e = engine(
            if close_fd { "direct:copy" } else { "mr_close" },
            Policy::Dmabuf,
            true,
            false,
            false,
        );
        let m = e
            .register_cuda_memory_batch(&[(memory.clone(), 0, 32)], Permission::RemoteReadWrite)
            .unwrap()
            .remove(0);
        assert!(owner.dmabuf.lock().unwrap().closed.is_empty());
        assert!(matches!(
            e.unregister_memory(&m),
            Err(Error::Unquiesced { .. })
        ));
        let log = verbs::events(source_runtime(&e));
        let n = log
            .lock()
            .unwrap()
            .iter()
            .filter(|&&e| e == "deregister_local")
            .count();
        assert!(e.close(EVENTUAL_PROGRESS).is_err());
        assert_eq!(
            log.lock()
                .unwrap()
                .iter()
                .filter(|&&e| e == "deregister_local")
                .count(),
            n
        );
        assert_eq!(e.stats().unwrap().rdma.unwrap().retained_persistent_mrs, 1);
        assert_eq!(
            owner.dmabuf.lock().unwrap().closed.len(),
            usize::from(close_fd)
        );
        drop(m);
        drop(memory);
        drop(owner);
        drop(e);
        assert!(weak.upgrade().is_some());
    }
}

#[test]
fn dmabuf_unregister_and_close_timeout_keep_fds_until_the_real_invalidation() {
    for reuse in [false, true] {
        let (owner, backing) = backing();
        let source = engine("gpu_key_copy", Policy::Dmabuf, true, false, reuse);
        let requester = rdma_fixture_with_runtime(verbs::runtime(if reuse {
            "direct:pause_copy_check_gpu_keys"
        } else {
            "direct:pause_copy"
        }));
        let pause = verbs::pause(source_runtime(&requester));
        let remote = source
            .register_cuda_memory_batch(&[(backing.clone(), 17, 24)], Permission::RemoteWrite)
            .unwrap()
            .remove(0);
        let local = requester
            .register_memory(vec![0x82; 8].into_boxed_slice(), Permission::LocalOnly)
            .unwrap();
        let peer = requester
            .open_segment(source.endpoint(), source.source_epoch(), EVENTUAL_PROGRESS)
            .unwrap();
        let request = TransferRequest::new(
            Transport::Rdma,
            local.base_address(),
            remote.base_address() + 5,
            8,
        );
        std::thread::scope(|scope| {
            let transfer = scope.spawn(|| {
                requester
                    .submit_and_wait(TransferOp::Write, &peer, &[request], EVENTUAL_PROGRESS)
                    .unwrap()
            });
            let (mut gate, _) = pause
                .1
                .wait_timeout_while(pause.0.lock().unwrap(), EVENTUAL_PROGRESS, |s| !s.0)
                .unwrap();
            assert!(gate.0);
            source.unregister_memory(&remote).unwrap();
            assert!(matches!(
                source.close_source(Duration::ZERO),
                Err(Error::Timeout)
            ));
            // A private grant owns a separate MR; its unused cached parent can retire.
            assert_eq!(
                owner.dmabuf.lock().unwrap().closed.len(),
                usize::from(!reuse)
            );
            assert_eq!(source.stats().unwrap().rdma.unwrap().active_grants, 1);
            assert!(backing.read(22, &mut [0; 8]).is_err());
            gate.1 = true;
            pause.1.notify_all();
            drop(gate);
            let report = transfer.join().unwrap();
            assert!(report.is_complete() && report.locally_drained, "{report:?}");
        });
        requester.close(EVENTUAL_PROGRESS).unwrap();
        source.close(EVENTUAL_PROGRESS).unwrap();
        let mut bytes = [0; 48];
        backing.read(0, &mut bytes).unwrap();
        let mut expected = [0x5a; 48];
        expected[22..30].fill(0x82);
        assert_eq!(bytes, expected);
        let state = owner.dmabuf.lock().unwrap();
        assert_eq!(state.closed.len(), state.exports.len());
        assert_eq!(state.exports.len(), if reuse { 1 } else { 2 });
    }
}

#[test]
fn dmabuf_pcie_flag_and_optional_cuda_symbol_follow_the_driver_contract() {
    for (available, version, flag) in [(true, 12040, 0), (true, 12080, 1), (false, 13000, 0)] {
        let (owner, memory) = backing_with_export(available);
        owner.dmabuf.lock().unwrap().version = version;
        let engine = engine("direct:copy", Policy::Auto, true, true, false);
        let m = engine
            .register_cuda_memory_batch(&[(memory, 0, 32)], Permission::RemoteRead)
            .unwrap()
            .remove(0);
        assert_eq!(
            engine.stats().unwrap().rdma.unwrap().peermem_registrations,
            u64::from(!available)
        );
        engine.unregister_memory(&m).unwrap();
        engine.close(EVENTUAL_PROGRESS).unwrap();
        let s = owner.dmabuf.lock().unwrap();
        assert_eq!(s.exports.len(), usize::from(available));
        if available {
            assert_eq!(s.exports[0].2, flag);
        }
    }
}

#[test]
fn dmabuf_policy_is_parsed_once_with_strict_values() {
    for (name, expected) in [
        ("auto", Policy::Auto),
        ("dmabuf", Policy::Dmabuf),
        ("peermem", Policy::Peermem),
    ] {
        let config = EngineConfig::from_json(&format!(
            r#"{{"rdma":{{"gpu_direct_registration":"{name}"}}}}"#
        ))
        .unwrap();
        assert_eq!(config.rdma.unwrap().gpu_direct_registration, expected);
    }
    for value in ["null", "1", "true", r#""host""#] {
        assert!(
            EngineConfig::from_json(&format!(
                r#"{{"rdma":{{"gpu_direct_registration":{value}}}}}"#
            ))
            .is_err()
        );
    }
    assert_eq!(
        crate::transport::rdma::RdmaConfig::default().gpu_direct_registration,
        Policy::Auto
    );
}

#[test]
fn dmabuf_rounds_the_export_but_never_widens_the_registered_view() {
    let (owner, memory) = backing();
    // The native legacy allocation reports a non-page-multiple logical extent.
    owner.dmabuf.lock().unwrap().extent_trim = 1;
    // This view is shorter than the originally retained handle; a foreign native
    // extent change must be rejected before exporting or publishing an MR.
    let e = engine("direct:copy", Policy::Dmabuf, true, false, false);
    assert!(matches!(
        e.register_cuda_memory_batch(&[(memory.clone(), 0, 8)], Permission::LocalOnly),
        Err(Error::InvalidRange)
    ));
    assert!(owner.dmabuf.lock().unwrap().exports.is_empty());
    drop(memory);
    let mut d = Arc::try_unwrap(driver()).ok().unwrap();
    d.dmabuf_export = Some(export);
    d.address_range = range;
    d.version = version;
    d.close_fd = close;
    // SAFETY: retained boxed shim storage; the entire shorter native extent is owned.
    let address = (unsafe { &*owner.bytes.get() }.as_ptr() as u64 + 4095) & !4095;
    let memory = Arc::new(CudaMemory::register(address, 8191, owner.clone(), Arc::new(d)).unwrap());
    let m = e
        .register_cuda_memory_batch(&[(memory, 31, 55)], Permission::LocalOnly)
        .unwrap()
        .remove(0);
    e.unregister_memory(&m).unwrap();
    e.close(EVENTUAL_PROGRESS).unwrap();
    let s = owner.dmabuf.lock().unwrap();
    assert_eq!(s.exports[0].1, 8192);
    assert_eq!((s.calls[0].1, s.calls[0].2), (31, 55));
    assert_eq!(s.closed.len(), 1);
}

#[cfg(feature = "metrics")]
#[test]
fn dmabuf_metrics_observe_successful_native_registrations_only() {
    let (owner, memory) = backing();
    let mut config = EngineConfig::default();
    config.metrics.enabled = true;
    config.metrics.http_port = 0;
    let e = crate::engine::tests::rdma_fixture_with_config(
        verbs::dmabuf_runtime("direct:copy", Policy::Auto, true, true, false),
        config,
    );
    let m = e
        .register_cuda_memory_batch(&[(memory, 0, 8)], Permission::LocalOnly)
        .unwrap()
        .remove(0);
    let j = e.metrics().unwrap().json();
    assert_eq!(j["orbite_rdma_dmabuf_registrations_total"], 1);
    assert_eq!(j["orbite_rdma_peermem_registrations_total"], 0);
    e.unregister_memory(&m).unwrap();
    assert_eq!(
        e.metrics().unwrap().json()["orbite_rdma_dmabuf_registrations_total"],
        1
    );
    e.close(EVENTUAL_PROGRESS).unwrap();
    assert_eq!(owner.dmabuf.lock().unwrap().closed.len(), 1);
}

#[test]
fn dmabuf_alignment_fails_closed_without_pointer_fallback() {
    let (owner, memory) = backing();
    owner.dmabuf.lock().unwrap().unaligned_base = true;
    let e = engine("direct:copy", Policy::Auto, true, true, false);
    assert!(matches!(
        e.register_cuda_memory_batch(&[(memory, 31, 55)], Permission::LocalOnly),
        Err(Error::Unavailable(_))
    ));
    assert!(owner.dmabuf.lock().unwrap().exports.is_empty());
    assert_eq!(e.stats().unwrap().rdma.unwrap().peermem_registrations, 0);
    e.close(EVENTUAL_PROGRESS).unwrap();
}
