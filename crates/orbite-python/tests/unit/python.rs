use super::*;
#[test]
fn panic_barrier_and_uncertain_cleanup_have_distinct_typed_errors() {
    Python::initialize();
    Python::attach(|py| {
        let err = run::<()>(py, "panic control", || panic!("boundary control")).unwrap_err();
        assert!(err.is_instance_of::<InternalError>(py));
        let err = run::<()>(py, "uncertain", || Err(UNKNOWN)).unwrap_err();
        assert!(err.is_instance_of::<UnquiescedError>(py));
        assert!(err.is_instance_of::<Error>(py));
    });
}
#[test]
fn rust_errors_survive_the_binding_without_status_code_collapse() {
    Python::initialize();
    Python::attach(|py| {
        assert!(error(INVALID, "range").is_instance_of::<InvalidRangeError>(py));
        assert!(error(ADDRESS, "owner").is_instance_of::<RegionNotFoundError>(py));
        assert!(error(FULL, "budget").is_instance_of::<ResourceExhaustedError>(py));
        assert!(error(UNSUPPORTED, "feature").is_instance_of::<UnavailableError>(py));
        assert!(
            error(orbite::Error::EpochMismatch.into(), "epoch")
                .is_instance_of::<EpochMismatchError>(py)
        );
        assert!(error(orbite::Error::Timeout.into(), "wait").is_instance_of::<TimeoutError>(py));
        assert!(
            error(orbite::Error::PermissionDenied.into(), "write")
                .is_instance_of::<PermissionDeniedError>(py)
        );
        assert!(
            error(
                orbite::Error::Backend {
                    operation: "cuMemcpy",
                    code: 1
                }
                .into(),
                "copy"
            )
            .is_instance_of::<BackendError>(py)
        );
    });
}
