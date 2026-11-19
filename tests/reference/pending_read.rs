use super::{TIMEOUT, finish_batch, hex};
use orbite::{
    Batch, BatchState, EngineConfig, HostMemory, Permission, RemoteSegment, TransferEngine,
    TransferOp, TransferRequest, Transport,
};
use std::error::Error;
use std::io;
use std::sync::Arc;

type Result<T> = std::result::Result<T, Box<dyn Error>>;

fn command(expected: &str) -> Result<()> {
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    if line.trim_end() != expected {
        return Err(format!("expected controller command {expected}").into());
    }
    Ok(())
}

pub(super) fn source(payload: &str) -> Result<()> {
    let engine = TransferEngine::new(EngineConfig::default())?;
    let memory = Arc::new(HostMemory::new(payload.as_bytes().into())?);
    let reg =
        engine.register_host_memory(memory.clone(), 0, memory.len(), Permission::RemoteRead)?;
    println!("READY {} {}", engine.endpoint(), engine.source_epoch());
    command("close")?;
    engine.unregister_memory(&reg)?;
    engine.close(TIMEOUT)?;
    Ok(())
}

fn check_pending(batch: &Batch) -> Result<()> {
    let status = batch.status()?;
    let tasks = batch.observe_tasks(None)?;
    if status.state != BatchState::Pending
        || status.locally_drained
        || !matches!(tasks.as_slice(), [task] if !task.state.is_terminal()
            && !task.locally_drained && task.transferred_bytes == 0)
    {
        return Err(format!("first READ no longer pending: {status:?}, {tasks:?}").into());
    }
    Ok(())
}

pub(super) fn reads(slow_peer: &str, fast_peer: &str) -> Result<()> {
    let engine = Arc::new(TransferEngine::new(EngineConfig::default())?);
    let open = |peer: &str| -> Result<RemoteSegment> {
        let (addr, epoch) = peer.split_once(' ').ok_or("missing source epoch")?;
        Ok(engine.open_segment(addr.parse()?, epoch.parse()?, TIMEOUT)?)
    };
    let slow = open(slow_peer)?;
    let fast = open(fast_peer)?;
    let destination = || -> Result<_> {
        let memory = Arc::new(HostMemory::new(Box::new([0xee; 8]))?);
        let reg =
            engine.register_host_memory(memory.clone(), 0, memory.len(), Permission::LocalOnly)?;
        Ok((memory, reg))
    };
    let submit = |peer: &RemoteSegment, dst: &HostMemory, len: usize| -> Result<Batch> {
        let [region] = peer.regions() else {
            return Err("expected one source region".into());
        };
        let request = TransferRequest::new(
            Transport::Tcp,
            dst.base_address() + 1,
            region.base_address(),
            len,
        );
        Ok(engine.submit_async(TransferOp::Read, peer, &[request], None, TIMEOUT)?)
    };
    let (slow_bytes, slow_reg) = destination()?;
    let (fast_bytes, fast_reg) = destination()?;
    println!("OPEN");
    command("stopped")?;
    let first = submit(&slow, &slow_bytes, 5)?;
    check_pending(&first)?;
    let owner = Arc::downgrade(&slow_bytes);
    engine.unregister_memory(&slow_reg)?;
    drop((slow_reg, slow_bytes));

    let second = submit(&fast, &fast_bytes, 3)?;
    finish_batch(second, "second", false)?;
    let mut bytes = [0; 8];
    fast_bytes.read(0, &mut bytes)?;
    println!("OBS\tBYTES\tsecond\t{}", hex(&bytes));
    engine.unregister_memory(&fast_reg)?;
    drop(fast_reg);
    check_pending(&first)?;
    let stats = engine.stats()?;
    if (
        stats.allocated_batches,
        stats.inflight_batches,
        stats.inflight_bytes,
        stats.retained_regions,
        stats.retained_bytes,
    ) != (1, 1, 5, 1, 8)
    {
        return Err(format!("pending READ lost its charges: {stats:?}").into());
    }
    let retained = owner.upgrade().ok_or("pending READ lost its destination")?;
    println!("PENDING first");
    command("resumed")?;
    finish_batch(first, "first", false)?;
    retained.read(0, &mut bytes)?;
    println!("OBS\tBYTES\tfirst\t{}", hex(&bytes));
    drop(retained);
    let stats = engine.stats()?;
    if owner.upgrade().is_some()
        || (
            stats.allocated_batches,
            stats.inflight_batches,
            stats.inflight_bytes,
            stats.retained_regions,
            stats.retained_bytes,
        ) != (0, 0, 0, 0, 0)
    {
        return Err(format!("drained READ retained its owners: {stats:?}").into());
    }
    engine.close(TIMEOUT)?;
    Ok(())
}
