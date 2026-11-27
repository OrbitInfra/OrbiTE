fn main() {
    println!("cargo:rustc-check-cfg=cfg(docsrs)");
    // Shared host access tracking and registered DMA leases have distinct users.
    for (name, enabled) in [
        (
            "native_dma",
            cfg!(any(
                feature = "cuda",
                feature = "rdma",
                feature = "io-uring"
            )),
        ),
        (
            "native_dma_lease",
            cfg!(any(feature = "rdma", feature = "io-uring", feature = "gds")),
        ),
    ] {
        println!("cargo:rustc-check-cfg=cfg({name})");
        if enabled {
            println!("cargo:rustc-cfg={name}");
        }
    }
    build_identity();
    println!("cargo:rustc-check-cfg=cfg(unsupported_native_target)");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("linux")
        && cfg!(any(
            feature = "cuda",
            feature = "rdma",
            feature = "io-uring",
            feature = "gds"
        ))
    {
        println!("cargo:rustc-cfg=unsupported_native_target");
    }
}

// Build identity is diagnostic metadata only. Neither the wire nor feature selection uses it.
fn build_identity() {
    use std::{env, fs, path::Path, process::Command};
    let git = |args: &[&str]| {
        let output = Command::new("git").args(args).output().ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
    };
    let commit = if Path::new(".cargo_vcs_info.json").is_file() {
        println!("cargo:rerun-if-changed=.cargo_vcs_info.json");
        fs::read_to_string(".cargo_vcs_info.json")
            .ok()
            .and_then(|v| {
                let (_, value) = v.split_once("\"sha1\"")?;
                Some(value.split('"').nth(1)?.to_owned())
            })
    } else if Path::new(".git").exists() {
        for name in [
            Some("HEAD".into()),
            git(&["symbolic-ref", "-q", "HEAD"]),
            Some("packed-refs".into()),
        ]
        .into_iter()
        .flatten()
        {
            if let Some(path) = git(&["rev-parse", "--git-path", &name]) {
                if Path::new(&path).exists() {
                    println!("cargo:rerun-if-changed={path}");
                }
            }
        }
        git(&["rev-parse", "--verify", "HEAD"])
    } else {
        None
    };
    let commit = commit.filter(|s| s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit()));
    println!(
        "cargo:rustc-env=ORBITE_BUILD_COMMIT={}",
        commit.as_deref().unwrap_or("unknown")
    );
    println!(
        "cargo:rustc-env=ORBITE_BUILD_PROFILE={}",
        env::var("PROFILE").unwrap_or_default()
    );
    let compiler = env::var_os("RUSTC")
        .and_then(|c| Command::new(c).arg("-V").output().ok())
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    println!(
        "cargo:rustc-env=ORBITE_BUILD_COMPILER={}",
        compiler.as_deref().unwrap_or("unknown")
    );
}
