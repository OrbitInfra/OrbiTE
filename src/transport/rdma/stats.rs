/// Capabilities and effective READ depths of the most recently connected local RC QP.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct QpReadDepths {
    /// Whether the local HCA advertises remote atomic support.
    pub supports_atomics: bool,
    /// Local HCA maximum initiated READ depth.
    pub initiator_limit: u8,
    /// Local HCA maximum accepted READ depth.
    pub responder_limit: u8,
    /// Effective initiated READ depth after configuration and provider clamps.
    pub initiator: u8,
    /// Effective accepted READ depth after configuration and provider clamps.
    pub responder: u8,
}

/// A retained endpoint is charged and blocks further RDMA admission. It can
/// still own pinned pages or RNIC resources after native teardown failed.
#[derive(Clone, Copy, Debug, Default)]
#[non_exhaustive]
pub struct RdmaStats {
    /// Native CUDA MRs successfully created through each route (including private
    /// source PDs). Cumulative evidence; a counter is not a retirement receipt.
    pub dmabuf_registrations: u64,
    /// Cumulative successful CUDA MRs using NVIDIA/MLNX_OFED peermem.
    pub peermem_registrations: u64,
    /// Most recently connected QP depth snapshot, or None before a connection.
    pub last_connected_qp: Option<QpReadDepths>,
    /// Pair-level failures, trial admissions and confirmed recoveries.
    pub rail_errors: u64,
    /// Cumulative half-open trial admissions.
    pub rail_probes: u64,
    /// Cumulative probes that confirmed rail recovery.
    pub rail_recoveries: u64,
    /// Rail pairs excluded from new work by cooldown.
    pub open_rails: usize,
    /// Rail pairs permitting only bounded recovery probes.
    pub half_open_rails: usize,
    /// Isolated PeerDirect source PDs, owned through QP/MR/RQ cleanup.
    pub active_grant_pds: usize,
    /// Cumulative isolated GPU source PD creations.
    pub opened_grant_pds: u64,
    /// Cumulative confirmed isolated GPU source PD retirements.
    pub closed_grant_pds: u64,
    /// Shared context/PD owners, including partially constructed retained ones.
    pub active_devices: usize,
    /// Context/PD owners retained after failed cleanup.
    pub retained_devices: usize,
    /// Successful context/PD constructions during this runtime lifetime.
    pub opened_devices: u64,
    /// Hardware-verified, exact-range device keys created over persistent GPU MRs.
    pub opened_gpu_grant_keys: u64,
    /// GPU keys invalidated on the matching QP by a confirmed completion SEND.
    pub invalidated_gpu_grant_keys: u64,
    /// Live memory-window owners, including retained failures.
    pub active_windows: usize,
    /// Cumulative memory-window allocations.
    pub opened_windows: u64,
    /// Never-bound MWs parked in their PD to prevent an exhausted index reappearing.
    pub exhausted_windows: usize,
    /// Index rotations; all previously burned full keys remain unavailable.
    pub window_key_rollovers: u64,
    /// Historical native MW indices charged for the lifetime of their PD.
    pub window_key_indices: usize,
    /// Full rkeys burned before an endpoint can submit BIND, including failures.
    pub burned_window_keys: u64,
    /// Cumulative failures to reserve a fresh revocable key.
    pub window_key_exhaustions: u64,
    /// Memory windows whose retirement failed or remains uncertain.
    pub retained_windows: usize,
    /// Validated bind completion receipts.
    pub completed_window_binds: u64,
    /// Validated invalidation completion receipts.
    pub completed_window_invalidations: u64,
    /// Source completion credits: parent reservations and unreaped message MRs.
    pub receive_credits: usize,
    /// High-water mark of source completion credits.
    pub peak_receive_credits: usize,
    /// Requests rejected before posting because completion credits were exhausted.
    pub receive_admission_rejections: u64,
    /// Posted or reserved receives still charged to CQ capacity.
    pub receive_work_requests: usize,
    /// Receive debt retained after uncertain retirement.
    pub retained_receive_work_requests: usize,
    /// High-water mark of charged receive work requests.
    pub peak_receive_work_requests: usize,
    /// Cumulative native receive-post calls.
    pub receive_post_calls: u64,
    /// Validated receive completion receipts.
    pub completed_receives: u64,
    /// Validated completion-message SEND receipts.
    pub completed_sends: u64,
    /// Live completion-message MRs, including retained ones.
    pub active_message_mrs: usize,
    /// Completion-message MRs kept after uncertain cleanup.
    pub retained_message_mrs: usize,
    /// Completion-message backing bytes still charged.
    pub message_bytes: usize,
    /// Subset of message bytes kept by retained native owners.
    pub retained_message_bytes: usize,
    /// Live native completion channels.
    pub active_completion_channels: usize,
    /// Cumulative completion-channel creations.
    pub opened_completion_channels: u64,
    /// Channels retained after uncertain cleanup.
    pub retained_completion_channels: usize,
    /// Live completion-channel registrations in the transport monitor.
    pub completion_event_sources: usize,
    /// One transport-wide thread consumes all device and CQ-channel events.
    pub monitor_workers: usize,
    /// Cumulative monitor-thread starts.
    pub opened_monitor_workers: u64,
    /// Completed periodic context/endpoint scans, independent of new requests.
    pub monitor_ticks: u64,
    /// Cumulative native port/GID route queries.
    pub route_queries: u64,
    /// Confirmed route recoveries after a port change.
    pub route_recoveries: u64,
    /// Successful native gets, each acknowledged before notifying progress.
    pub completion_events: u64,
    /// Cumulative completion-queue notification arms.
    pub cq_notification_arms: u64,
    /// Cumulative completion-event waits.
    pub cq_event_waits: u64,
    /// Native completion queues still owned.
    pub active_cqs: usize,
    /// Completion queues retained after failed retirement.
    pub retained_cqs: usize,
    /// Cumulative completion-queue creations.
    pub opened_cqs: u64,
    /// Live transport-level data threads, including idle workers.
    pub progress_workers: usize,
    /// Successful data-thread starts; context recovery does not start threads.
    pub opened_progress_workers: u64,
    /// CQ lanes still assigned to data workers, summed across all contexts.
    pub progress_lanes: usize,
    /// Live device async-event registrations in the monitor.
    pub async_event_sources: usize,
    /// Native async events acknowledged after handling.
    pub acknowledged_async_events: u64,
    /// Cumulative shared-progress faults, including permanently retained owners.
    pub progress_failures: u64,
    /// Faulted progress owners retired after complete native drain and worker detachment.
    pub progress_recoveries: u64,
    /// Highest pending data-QP count observed by any single CQ lane progress pass.
    pub peak_pending_qps: usize,
    /// Physical endpoint owners, including idle and retained entries.
    pub active_endpoints: usize,
    /// Retired-from-use endpoints eligible for safe reuse.
    pub idle_endpoints: usize,
    /// Cumulative physical endpoint constructions.
    pub opened_connections: u64,
    /// Endpoint pool indices on which a native RC connection reached RTS.
    pub connected_pool_indices: u64,
    /// Cumulative confirmed endpoint retirements.
    pub closed_connections: u64,
    /// Cumulative leases of existing endpoint owners.
    pub reused_connections: u64,
    /// Cumulative idle endpoint evictions.
    pub evicted_connections: u64,
    /// Grants completed with a revocation receipt permitting endpoint reuse.
    pub completed_reusable_grants: u64,
    /// Per-request memory owners, including outputs after physical QP close.
    pub active_grants: usize,
    /// Whole-parent grant reservations, held through every child cleanup.
    pub grant_credits: usize,
    /// Per-request grants kept after unproven quiescence.
    pub retained_grants: usize,
    /// Staging backing bytes charged to live or retained grants.
    pub staging_bytes: usize,
    /// Direct host bytes charged through endpoint release, including retention.
    pub direct_bytes: usize,
    /// Successfully constructed direct host endpoints during this runtime.
    pub opened_direct_endpoints: u64,
    /// Endpoints quarantined after uncertain native teardown.
    pub retained_endpoints: usize,
    /// Payload backing bytes attached to retained grants.
    pub retained_bytes: usize,
    /// All cached physical MRs, including creation, closing and retained failures.
    pub persistent_mrs: usize,
    /// Full-view bytes charged across every cached physical MR copy.
    pub persistent_mr_bytes: usize,
    /// Cached MRs with failed native retirement.
    pub retained_persistent_mrs: usize,
    /// Cumulative physical cached-MR registrations.
    pub opened_persistent_mrs: u64,
    /// Cumulative leases of existing cached MRs.
    pub reused_persistent_mrs: u64,
    /// Cumulative idle cached-MR evictions.
    pub evicted_persistent_mrs: u64,
    /// MW-bind parent subset of persistent_mrs; never remote-enabled MRs.
    pub persistent_source_mrs: usize,
    /// Cumulative source parent MR constructions.
    pub opened_source_mrs: u64,
    /// Cumulative leases of source parent MRs.
    pub reused_source_mrs: u64,
    /// Source parents retained by failed deregistration. Failed MW cleanup keeps
    /// its parent borrow charged in persistent_source_mrs without a dereg attempt.
    pub retained_source_mrs: usize,
    /// Successful physical READ owners.
    pub completed_reads: u64,
    /// Physical SQ completions observed for payload WRs, including error receipts.
    pub payload_send_cqes: u64,
    /// Successful physical WRITE owners.
    pub completed_writes: u64,
    /// Payload bytes in successful READ owners.
    pub completed_read_bytes: u64,
    /// Payload bytes in successful WRITE owners.
    pub completed_write_bytes: u64,
    /// Reserved or possibly posted WR slots pending CQ validation or teardown.
    pub work_requests: usize,
    /// SQ slots retained after uncertain retirement.
    pub retained_work_requests: usize,
    /// High-water mark of charged SQ slots.
    pub peak_work_requests: usize,
    /// Cumulative accepted native payload work requests.
    pub posted_work_requests: u64,
    /// Cumulative native send-post calls.
    pub post_calls: u64,
    /// Post calls with a known accepted prefix and a rejected suffix.
    pub partial_posts: u64,
    /// Post calls whose accepted prefix cannot be established safely.
    pub uncertain_posts: u64,
    /// Cumulative error completions seen by shared progress.
    pub completion_errors: u64,
}
