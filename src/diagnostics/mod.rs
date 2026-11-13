//! Offline build diagnostics. The version collector reads compile-time facts only.
#![deny(missing_docs)]

use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fmt::Write};

/// Package version embedded at build time.
pub const BUILD_VERSION: &str = env!("CARGO_PKG_VERSION");
/// Build revision, or the build script's unknown marker when unavailable.
pub const BUILD_COMMIT: &str = env!("ORBITE_BUILD_COMMIT");
/// Schema identifier for serialized diagnostic snapshots.
pub const SCHEMA_VERSION: u32 = 1;

/// Result of one offline diagnostic.
/// This enum is non-exhaustive; consumers must handle unknown variants.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum Outcome {
    /// The check succeeded.
    Pass,
    /// The check completed with a warning.
    Warn,
    /// The check failed.
    Fail,
    /// The check did not run.
    Skip,
}

/// One diagnostic result and suggested remedial actions.
#[derive(Debug, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Check {
    /// Machine-readable check identifier.
    pub id: String,
    /// Result severity; not a transfer completion receipt.
    pub outcome: Outcome,
    /// Human-readable result.
    pub summary: String,
    /// Suggested actions, in display order.
    pub remediation: Vec<String>,
}
/// Compile-time build identity; querying it never loads a provider.
#[derive(Debug, Serialize, Deserialize)]
#[non_exhaustive]
pub struct BuildInfo {
    /// Package version.
    pub version: String,
    /// Build revision or unknown marker.
    pub commit: String,
    /// Cargo profile recorded by the build script.
    pub build_type: String,
    /// Compiler identity recorded at build time.
    pub compiler: String,
    /// Compiled transports and features; true does not establish runtime availability.
    pub features: BTreeMap<String, bool>,
}
/// Offline diagnostic report; it owns metadata, not an engine or native resources.
#[derive(Debug, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Snapshot {
    /// Schema identifier for consumers of the JSON form.
    pub schema_version: u32,
    /// Diagnostic command that produced this snapshot.
    pub command: String,
    /// Compile-time identity.
    pub build: BuildInfo,
    /// Individual results in display order.
    pub checks: Vec<Check>,
}

/// Collect version and compiled features without starting an engine or using the network.
///
/// # Examples
/// ```
/// let snapshot = orbite::diagnostics::version_snapshot();
/// assert_eq!(snapshot.build.version, orbite::diagnostics::BUILD_VERSION);
/// ```
pub fn version_snapshot() -> Snapshot {
    Snapshot {
        schema_version: SCHEMA_VERSION,
        command: "version".into(),
        build: BuildInfo {
            version: BUILD_VERSION.into(),
            commit: BUILD_COMMIT.into(),
            compiler: env!("ORBITE_BUILD_COMPILER").into(),
            build_type: env!("ORBITE_BUILD_PROFILE").into(),
            features: [
                ("tcp", true),
                ("hp_tcp", true),
                ("shm", cfg!(target_os = "linux")),
                ("nvlink", cfg!(feature = "cuda")),
                ("mnnvl", cfg!(feature = "cuda")),
                ("cuda", cfg!(feature = "cuda")),
                ("rdma", cfg!(feature = "rdma")),
                ("io-uring", cfg!(feature = "io-uring")),
                ("gds", cfg!(feature = "gds")),
                ("metrics", cfg!(feature = "metrics")),
                ("rdma-adaptive", cfg!(feature = "rdma-adaptive")),
            ]
            .map(|(k, v)| (k.to_owned(), v))
            .into(),
        },
        checks: vec![Check {
            id: "build.info.available".into(),
            outcome: Outcome::Pass,
            summary: "compile-time build information is available".into(),
            remediation: Vec::new(),
        }],
    }
}
impl Snapshot {
    /// Serialize the snapshot as pretty-printed JSON.
    ///
    /// # Errors
    /// Returns a `serde_json::Error` if serialization fails; no engine operation is performed.
    pub fn render_json(&self) -> serde_json::Result<String> {
        serde_json::to_string_pretty(self)
    }
    /// Render build identity and check results for a human reader.
    pub fn render_text(&self) -> String {
        let mut out = format!(
            "OrbiTE diagnostics schema: {}\nCommand: {}\nVersion: {}\nCommit: {}\nBuild type: {}\nCompiler: {}\nFeatures:",
            self.schema_version,
            self.command,
            self.build.version,
            self.build.commit,
            self.build.build_type,
            self.build.compiler
        );
        for (name, enabled) in &self.build.features {
            let _ = write!(
                out,
                " {name}={}",
                if *enabled { "enabled" } else { "disabled" }
            );
        }
        out.push('\n');
        if !self.checks.is_empty() {
            out.push_str("\nChecks:\n");
        }
        for c in &self.checks {
            let outcome = match c.outcome {
                Outcome::Pass => "pass",
                Outcome::Warn => "warn",
                Outcome::Fail => "fail",
                Outcome::Skip => "skip",
            };
            let _ = writeln!(out, "  [{outcome}] {}: {}", c.id, c.summary);
            for r in &c.remediation {
                let _ = writeln!(out, "    remediation: {r}");
            }
        }
        out
    }
    /// Return the CLI severity code: success 0, failed checks 1, unavailable probes 3,
    /// internal failures 4. Warnings count as 1 only when `warnings_are_errors` is true.
    pub fn exit_code(&self, warnings_are_errors: bool) -> u8 {
        self.checks
            .iter()
            .map(|c| match (c.outcome, c.id.as_str()) {
                (Outcome::Fail, "tool.internal") => 4,
                (Outcome::Fail, "engine.process" | "tool.probe_missing") => 3,
                (Outcome::Fail, _) => 1,
                (Outcome::Warn, _) if warnings_are_errors => 1,
                _ => 0,
            })
            .max()
            .unwrap_or(0)
    }
}
#[cfg(test)]
#[path = "../../tests/unit/diagnostics/mod.rs"]
mod tests;
