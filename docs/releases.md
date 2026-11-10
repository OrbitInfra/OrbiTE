# Release notes

## 0.1.0-beta.1 — preparation

This is the first beta publication candidate, not a publication record.
The maintainer publishes and approves the tag separately. The last published
Rust milestone is `0.1.0-alpha.1` from `7cc48490`; **beta.1** is `0.1.0b1` on
PyPI. No Python release predates it. OrbiTE is a transfer engine for LLM
inference on NVIDIA; [scope and non-goals](architecture.md#scope-and-non-goals)
separate byte movement from the consumer's cache and discovery policy.

### Changes since alpha.1

- Rust is primary; Python is a PEP 8 binding to the same retained owners.
  The [Rust inventory](rust-api.md#current-public-surface) and
  [Python inventory](python.md#surface-and-defaults) describe today's surface,
  not a freeze. Public enums and field records are non-exhaustive except
  `TransferOp`. `NVLinkStats` became `NvlinkStats`, `MemType` became
  `TopologyMemoryKind`, and `MemoryBacking` is the retained backing type.
  Hidden binding helpers require the Python crate's exact core-version pin.
- TENT wire, API, configuration and naming compatibility are removed.
  `TentSettings`, `from_tent_*`, `MC_TENT_CONF`, legacy environment mappings,
  TENT-shaped Python names and transport-ID conversions are gone. Use
  `EngineConfig::from_json/from_file/from_env`; only `ORBITE_CONF` selects an
  environment document. Unknown keys and wrong types fail. Metrics use
  `orbite_` names. Frozen TENT v0.3.14 remains the design reference, CPU oracle
  and performance baseline, not a runtime dependency.
- C ABI, C++/classic-TE adapters, device plugin ABI and diagnostics C exports
  are removed. The CLI is `orbite diagnostics version [--json]`, replacing
  `tent`. Python ships no `tent` or `mooncake.engine` alias.
- Discovery is P2P-only. Consumers publish endpoint and authoritative source
  epoch. Rust `close_segment` fences a peer, drains it and reclaims identities
  after their last reference; uncertain cleanup stays retained and charged.
  Python segment close remains reversible handle invalidation.
- FABRIC/libfabric is excluded: NVIDIA platforms only; AWS EFA is a non-goal.
  Its feature, transport, configuration and metrics are removed; remaining
  transport discriminants are unchanged. CUDA fabric handles and MNNVL/EGM
  are unaffected. MPComm is deferred: no consumer or available provider.
- File vectors accept retained host/CUDA ranges and padding: zero-fill on
  WRITE, discard on READ, with completed-prefix semantics. Runtime GDS disable
  and lazy probing preserve feature isolation and strict explicit selection.
- `rdma.gpu_direct_registration = auto | dmabuf | peermem` adds a checked,
  fail-closed CUDA dma-buf route with optional verbs symbol lookup. Explicit
  Direct registration never silently becomes staging. Export flags remain
  zero; Grace topology-specific PCIe forcing is not implemented. The opt-in
  GPU source reuse route remains unqualified.
- Local-copy `submit(direction, descriptors, LocalCopyOptions)` replaces the
  optional event argument; options carry intent, priority and readiness.
  `kernel_cta_limit` bounds the grid; `kernel_threshold_background` defaults to
  `usize::MAX`, keeping background DeviceToHost Auto on DMA until calibration.
  `None` retains the original CTA budget; normal Auto uses 128 compiled DMA
  operations. `LocalCopyStats` adds per-class diagnostics and is no longer
  `Copy`. Forced backends, overlap ordering and event-retained owners remain.
  These APIs are Rust-only; installed calibration is unrun.
- TCP reuses bounded, epoch-scoped streams. Small eligible host-file work can
  complete on the caller with a deadline-bounded wait of at most 200 µs.
  Unfinished rings transfer to the existing drain owner without resubmission.
  Worker exit/start failures retain submitted owners as sticky `Unquiesced`;
  exceptional GDS group paths settle common admission and metrics accounting.

### Qualification and limits

| Profile on this source | Evidence / limit |
| --- | --- |
| TCP DRAM, configuration | Qualified on Linux CI: [named jobs and controls](qualification.md#linux-ci-release-profiles); loopback/multiple processes, not physical two-host qualification |
| HP-TCP, SHM | Qualified on Linux CI: [loopback rails](qualification.md#hp-tcp-rail-verification) and [same-host DRAM](qualification.md#shared-memory-and-cxl-verification); no physical multi-rail or CXL claim |
| io_uring DRAM/file | Qualified on Linux CI: [real ext4/tmpfs](qualification.md#local-file-segments); unavailable kernels fail explicitly |
| Python CPU/TCP and quickstart | [Installed-wheel CI](qualification.md#python-wheels), CPython 3.10–3.14, Linux x86_64/aarch64; exporter lifetime and GIL controls remain. Including native features in a wheel does not qualify hardware |
| GPU, RDMA, GDS, NVLink, MNNVL/EGM, CXL and physical rails | Implemented; installed profiles unqualified on this artifact. Older H20/RoCE artifacts qualify only their recorded revisions. [Verification queue](qualification.md#verification-queue) |

Missing public documentation remains a pre-0.1.0 gate: close the findings and
turn the audit into `-D missing-docs`. The NIXL plugin is not implemented; its
[upstream and hardware prerequisites](nixl-backend.md) remain separate.

### Known performance limits

These are byte-checked CPU-runner measurements, not general speed guarantees.
[Benchmarks](benchmarks.md) pins revisions, hosts, sample counts and every row;
raw results remain CI artifacts. p99 uses 100 samples per trial. Timed WRITEs
exclude durability barriers. There is no GPU, RNIC or physical NVMe claim.

| Workload | Recorded result / remaining limit |
| --- | --- |
| Scattered 64 × 4 KiB TCP | READ p50 8.59→4.23 ms; WRITE 9.33→6.30 ms, still 3.7% above TENT's 6.07 ms on the paired runner. Single 4 KiB READ CPU falls 370→190 µs; TENT 256 µs. One exclusive lane per peer has no concurrent-caller performance claim |
| Small tmpfs WRITE (#193) | 4 KiB p50 117.9→57.3 µs on stable and 192.8→76.1 µs on 1.85; after/TENT 1.02×/0.96×. A missed caller window still needs worker drain |
| Bounded-wait trade-off (#193) | Stable tmpfs 4 KiB READ p50 +9.3%; ext4 64 KiB WRITE p99 331→544 µs (+64.4%), separate trial ranges. A miss can delay handoff; its contribution versus VM/device variation is not isolated |
| Clean-harness ext4 tail (#186) | Rust 1.85 gapped WRITE p99 16.26 ms versus TENT 3.59 ms, overlapping three-trial ranges. Bulk-verifier and offered-load counterfactuals reproduce the tail in unchanged TENT. This stays a P11 limit; physical NVMe measurement is in the hardware batch, with no more virtual-disk tuning planned |
| Other TCP regression | Rust 1.85 64 MiB READ p50/p99 +2.8%/+10.7% while CPU falls 4.2%; wake latency versus host variation is not isolated |

Earlier S26 ext4 values are **old-harness** evidence: truncate/rewrite reset
writeback overlapped 31,999 of 32,000 measured windows. Both harnesses now
preallocate once, reset in place and `fdatasync` before timing; zero fixed
windows contained reset writeback. The intermediate clean-reset comparison
still used different verification reads. Keep those historical tables under
[reset isolation](benchmarks.md#reset-only-paired-comparison) and the subsequent
[block-layer comparison](benchmarks.md#direct-io-issue-paths-and-verification-load);
they are not current release performance claims.

### Compatibility boundary

Breaking Rust/Python API changes remain allowed before 0.1.0, including between
betas. The **0.1.x API and wire freeze begins at 0.1.0**: same-0.1.x peers will
interoperate, with breaking API or wire changes requiring a minor bump under
0.x semver. Until then, both peers must run the same build. The native
control-plane `VERSION` remains **1** and cannot detect a build mismatch;
there is no pre-0.1.0 compatibility promise. See the
[wire rule](tent-parity.md#wire-and-consumer-boundaries). Timeout, cancellation
and logical terminal status never substitute for physical retirement, and an
uncertain WRITE is never replayed.

## Maintainer publication procedure

This section configures a future publication. This PR creates no tag, package
publication or GitHub Release. Source versions stay `0.1.0-beta.1` and
`0.1.0b1`. Only the maintainer approves publication.

### One-time setup

Use [PyPI Trusted Publishing](https://docs.pypi.org/trusted-publishers/creating-a-project-through-oidc/)
and [crates.io Trusted Publishing](https://crates.io/docs/trusted-publishing),
so neither job stores a long-lived registry token. The crate already exists
under owner `feichai0017`; the Python distribution is a pending publisher until
its first successful upload (a pending publisher does not reserve the name).

1. In repository **Settings → Environments**, create `pypi` and `crates-io`.
   Each must require a maintainer reviewer (for example `feichai0017`), prohibit
   administrator bypass, and allow deployment **tags matching `v*` only**, no
   branches. If the sole maintainer also starts the run, leave “prevent
   self-review” off so that their separate approval is possible; enabling it
   requires a second authorized reviewer. Do not run a publishing tag before
   these protection rules exist. GitHub must offer required reviewers for the
   repository's visibility/plan.
2. In the maintainer's PyPI account, **Publishing → Add a new pending publisher**:
   project `orbite`, owner `OrbitInfra`, repository `OrbiTE`, workflow filename
   `release.yml`, environment `pypi`. Confirm the name remains available.
   After first upload, manage the publisher under the project's Publishing tab.
3. As the existing crates.io owner, add a GitHub trusted publisher to `orbite`:
   owner `OrbitInfra`, repository `OrbiTE`, workflow `release.yml`, environment
   `crates-io`. No publisher is configured for the unpublished Python Rust crate.
4. Allow the pinned official publishing actions. Only the two protected jobs
   receive `id-token: write`. Only the final GitHub Release job receives
   `contents: write`. Build/test jobs have read-only repository access.

The published wheels are the **combined RDMA/CUDA/io_uring/GDS/metrics**
profile (including the hidden `rdma-adaptive` reference core), one for each
CPython 3.10–3.14 × Linux x86_64/aarch64. Native libraries load at runtime;
CPU quickstarts need no GPU. The sdist uses the same profile and its rebuild
requires native build headers. CPU + io_uring and MSRV wheels remain review
artifacts; profiles share filenames and must never be uploaded together.

### Review and dry run

A pull request changing release machinery runs `release.yml` as a dry run.
After merge, the maintainer can also run:

```sh
gh workflow run release.yml --repo OrbitInfra/OrbiTE --ref main -f dry_run=true
gh run list --repo OrbitInfra/OrbiTE --workflow release.yml
# Choose the displayed run ID; then watch and download its tested artifacts.
gh run watch RUN_ID --repo OrbitInfra/OrbiTE --exit-status
gh run download RUN_ID --repo OrbitInfra/OrbiTE --name release-artifacts --dir release-artifacts
(cd release-artifacts && sha256sum -c SHA256SUMS)
```

The workflow checks versions, exact Python core pin and tag, reruns both
Rust toolchains/all CI features, strict docs and docs.rs without verbs headers,
the frozen oracle/480-row comparisons, all wheels and the sdist rebuild.
It installs and runs the packaged quickstarts. Package allowlists reject logs,
traces (except the frozen CPU fixture), generated HTML and hardware run output.
The reviewed set contains ten wheels, one sdist, one `.crate`, `SHA256SUMS` and
these release notes. Dry runs cannot obtain registry credentials or publish.

### Approved publication and partial failure

Only after separate approval, from the accepted clean commit:

```sh
git switch main
git pull --ff-only
# Verify HEAD is the approved release-preparation commit before creating the tag.
git tag -a v0.1.0-beta.1 -m 'OrbiTE 0.1.0-beta.1'
git push origin refs/tags/v0.1.0-beta.1
```

The tag starts a fresh gate/build run. Mismatched tags fail, including during
dry runs. Approve each environment only after reviewing the gates and artifact
hashes. The publishing jobs run once, verify registry SHA-256 receipts, and
only then create a prerelease on GitHub for the **existing** tag with notes and
checksums. No branch/manual run can publish unless its ref is that matching tag.
For an existing tag, an explicitly approved manual run uses
`gh workflow run release.yml --ref v0.1.0-beta.1 -f dry_run=false`; do not use it
to blindly repeat a partly successful publication.

If one registry fails, the workflow summary preserves the other's result and
creates no GitHub Release. A failed upload may already have published some
files. Compare PyPI's version JSON filename/digest list and crates.io's version
checksum with the retained `SHA256SUMS` before recovery. Do not move the tag,
rebuild a different artifact under the same version, enable `skip-existing`,
or rerun successful publishing jobs. A maintainer may rerun only a failed job
once they establish that it uploaded nothing. For partial PyPI uploads, approve
a recovery uploading only the missing files from the original checked set;
for a verification/network failure after upload, verify receipts read-only.
`python3 scripts/verify-registry.py pypi release-artifacts` and the
`crates-io` variant perform those checks without uploading. Create the GitHub
Release only after both complete receipts are verified. No automatic recovery
or deletion of published versions is part of this workflow.
