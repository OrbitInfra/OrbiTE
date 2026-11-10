use super::*;
use std::path::PathBuf;
use std::process::Command;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "orbite-verbs-loader-{}-{}",
            std::process::id(),
            crate::SourceEpoch::generate().unwrap()
        ));
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("native.c"), include_str!("native.c")).unwrap();
        Self(path)
    }
    fn build(&self, optional: bool, missing: Option<&str>) -> PathBuf {
        let path = self
            .0
            .join(format!("{}-{}.so", optional, missing.unwrap_or("complete")));
        let mut command = Command::new("cc");
        command.args(["-shared", "-fPIC", "-Wall", "-Wextra", "-Werror"]);
        if optional {
            command.arg("-DORBITE_OPTIONAL_VERBS");
        }
        // Remove one definition's exported name, without changing verbs.h or
        // its function-like macros. The complete fixture checks every signature.
        let source = include_str!("native.c");
        let source = missing.map_or_else(
            || source.to_owned(),
            |name| source.replace(&format!("{name}("), "orbite_missing_export("),
        );
        let input = path.with_extension("c");
        std::fs::write(&input, source).unwrap();
        let output = command.arg(input).arg("-o").arg(&path).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        path
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn missing_library_and_each_required_export_are_unavailable_before_a_native_call() {
    let fixture = Fixture::new();
    assert!(
        load(fixture.0.join("absent.so").as_os_str())
            .err()
            .unwrap()
            .starts_with("libibverbs.so.1:")
    );
    for &name in REQUIRED {
        let file = fixture.build(false, Some(name));
        assert_eq!(
            load(file.as_os_str()).err().unwrap(),
            format!("libibverbs.so.1: missing {name}")
        );
    }
}

#[test]
fn optional_exports_and_loaded_code_follow_the_retained_library() {
    let fixture = Fixture::new();
    for optional in [false, true] {
        let file = fixture.build(optional, None);
        let native = load(file.as_os_str()).unwrap();
        assert_eq!(native.port_speed.is_some(), optional);
        #[cfg(feature = "cuda")]
        assert_eq!(native.dmabuf.is_some(), optional);
        // Removing the pathname cannot unload functions owned by the table.
        std::fs::remove_file(&file).unwrap();
        let mut count = -1;
        // SAFETY: this exact fixture ABI accepts the writable count and returns no devices.
        assert!(unsafe { (native.ibv_get_device_list)(&mut count) }.is_null());
        assert_eq!(count, 0);
        assert_eq!(
            // SAFETY: the fixture accepts null and returns EIO without destroying an owner.
            unsafe { (native.ibv_close_device)(std::ptr::null_mut()) },
            libc::EIO
        );
        if let Some(speed) = native.port_speed {
            let mut value = 0;
            // SAFETY: the loaded optional fixture accepts null and initializes the output.
            assert_eq!(unsafe { speed(std::ptr::null_mut(), 1, &mut value) }, 0);
            assert_eq!(value, 2000);
        }
    }
}

#[test]
fn real_install_is_lazy_and_reports_missing_libraries_in_an_isolated_process() {
    const CHILD: &str = "ORBITE_TEST_NO_NATIVE_RUNTIME";
    if std::env::var_os(CHILD).is_some() {
        let engine = crate::TransferEngine::new(crate::EngineConfig::default()).unwrap();
        let maps = std::fs::read_to_string("/proc/self/maps").unwrap();
        assert!(!maps.contains("libibverbs.so"));
        drop(engine);
        let config = crate::EngineConfig {
            rdma: Some(super::super::RdmaConfig::default()),
            ..Default::default()
        };
        assert!(
            matches!(crate::TransferEngine::new(config), Err(Error::Unavailable(message)) if message.starts_with("libibverbs.so.1:"))
        );
        return;
    }
    // The clean-container gate sets this explicit requirement. Other hosts
    // exercise a missing library and every missing export with local fixtures.
    if std::env::var_os("ORBITE_REQUIRE_NO_NATIVE_LIBRARIES").is_some() {
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "transport::rdma::sys::native::tests::real_install_is_lazy_and_reports_missing_libraries_in_an_isolated_process", "--nocapture"])
            .env(CHILD, "1")
            .output().unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
    }
}
