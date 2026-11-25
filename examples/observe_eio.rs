use orbite::{EngineConfig, Permission, TransferEngine, TransferOp, TransferRequest, Transport};
use std::time::Duration;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mode = std::env::args().nth(1).unwrap();
    let cut = std::env::args()
        .nth(2)
        .map(|s| s.parse::<usize>().unwrap())
        .unwrap_or((32 << 20) + 512);
    let n = 64 << 20;
    let mut cfg = EngineConfig::default();
    cfg.max_transfer_bytes = n;
    cfg.max_inflight_bytes = n;
    cfg.max_failover_attempts = 0;
    let e = std::sync::Arc::new(TransferEngine::new(cfg)?);
    let mut original = vec![0u8; n + 3 * 4096];
    for (i, b) in original.iter_mut().enumerate() {
        *b = (i % 251) as u8;
    }
    let r = e.register_memory(original.clone().into_boxed_slice(), Permission::LocalOnly)?;
    let base = r.base_address().next_multiple_of(4096) + 4096;
    let offset = (base - r.base_address()) as usize;
    let root = std::env::var("OBS_ROOT").unwrap_or_else(|_| "/tmp/ext4".into());
    let f = e.open_file_segment(&format!("file://{root}/payload"), Permission::RemoteRead)?;
    let req = TransferRequest::new(Transport::IoUring, base, 0, n);
    let wait = Duration::from_secs(10);
    let mut batch = e.submit_async(TransferOp::Read, &f, &[req], None, wait)?;
    let status = batch.wait(wait)?;
    let task = batch.task_status(0)?;
    assert_eq!(e.file_segment_info(&f)?.direct_io, Some(true));
    assert!(status.locally_drained && task.locally_drained);
    let mut got = vec![0; r.len()];
    r.read(0, &mut got)?;
    let err = task.failure.as_ref().map(|f| format!("{:?}", f.error));
    if mode == "full" {
        assert_eq!(task.transferred_bytes, n);
    } else if mode == "cut" {
        assert_eq!(task.transferred_bytes, cut);
        assert!(err.is_some());
    } else {
        assert_eq!(task.transferred_bytes, 0);
        assert!(err.as_ref().unwrap().contains("-5"));
    }
    for (i, b) in original[offset..offset + task.transferred_bytes]
        .iter_mut()
        .enumerate()
    {
        let j = i % 16384;
        let byte = ((j * 53 + (j / 256) * 11 + (j / 32) * 17 + 5) % 251) as u8;
        *b = if byte == 0xa5 { 0xb7 } else { byte };
    }
    assert!(original == got);
    batch.free()?;
    e.close_file_segment(&f, wait)?;
    e.close(wait)?;
    println!(
        "PROOF public OrbiTE {mode} completed={} transferred={} error={err:?} all={} nonuniform bytes checked",
        status.completed_tasks,
        task.transferred_bytes,
        got.len()
    );
    Ok(())
}
