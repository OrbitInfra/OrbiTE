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
        return Err(format!("READ no longer pending: {status:?}, {tasks:?}").into());
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
    let submit =
        |peer: &RemoteSegment, dst: &HostMemory, offset: u64, len: usize| -> Result<Batch> {
            let [region] = peer.regions() else {
                return Err("expected one source region".into());
            };
            let request = TransferRequest::new(
                Transport::Tcp,
                dst.base_address() + 1,
                region.base_address() + offset,
                len,
            );
            Ok(engine.submit_async(TransferOp::Read, peer, &[request], None, TIMEOUT)?)
        };
    let check_charges = |expected| -> Result<()> {
        let stats = engine.stats()?;
        let actual = (
            stats.allocated_batches,
            stats.inflight_batches,
            stats.inflight_bytes,
            stats.retained_regions,
            stats.retained_bytes,
        );
        if actual != expected {
            return Err(format!("expected READ charges {expected:?}, observed {actual:?}").into());
        }
        Ok(())
    };
    let (slow_bytes, slow_reg) = destination()?;
    let (fast_bytes, fast_reg) = destination()?;
    println!("OPEN");
    command("stopped")?;
    let first = submit(&slow, &slow_bytes, 0, 5)?;
    check_pending(&first)?;
    let owner = Arc::downgrade(&slow_bytes);
    engine.unregister_memory(&slow_reg)?;
    drop((slow_reg, slow_bytes));

    let second = submit(&fast, &fast_bytes, 0, 3)?;
    finish_batch(second, "second", false)?;
    let mut bytes = [0; 8];
    fast_bytes.read(0, &mut bytes)?;
    println!("OBS\tBYTES\tsecond\t{}", hex(&bytes));
    engine.unregister_memory(&fast_reg)?;
    drop(fast_reg);
    check_pending(&first)?;
    check_charges((1, 1, 5, 1, 8))?;
    let retained = owner.upgrade().ok_or("pending READ lost its destination")?;
    println!("PENDING first");

    let (later_bytes, later_reg) = destination()?;
    let later = submit(&slow, &later_bytes, 3, 2)?;
    check_pending(&first)?;
    check_pending(&later)?;
    check_charges((2, 2, 7, 2, 16))?;
    println!("PENDING first later");
    command("resumed")?;
    finish_batch(later, "later", false)?;
    later_bytes.read(0, &mut bytes)?;
    println!("OBS\tBYTES\tlater\t{}", hex(&bytes));
    engine.unregister_memory(&later_reg)?;
    drop(later_reg);
    finish_batch(first, "first", false)?;
    retained.read(0, &mut bytes)?;
    println!("OBS\tBYTES\tfirst\t{}", hex(&bytes));
    drop(retained);
    if owner.upgrade().is_some() {
        return Err("drained READ retained its destination".into());
    }
    check_charges((0, 0, 0, 0, 0))?;
    engine.close(TIMEOUT)?;
    Ok(())
}
