use super::{TIMEOUT, finish_batch, hex};
use orbite::{
    Batch, BatchState, EngineConfig, HostMemory, Permission, RemoteEffect, RemoteSegment,
    TaskState, TransferEngine, TransferOp, TransferRequest, Transport,
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

pub(super) const BULK_BYTES: usize = 64 << 20;

fn read_engine(length: usize, port: u16) -> Result<TransferEngine> {
    let mut config = EngineConfig::default();
    config.bind_addr.set_port(port);
    config.max_transfer_bytes = config.max_transfer_bytes.max(length);
    Ok(TransferEngine::new(config)?)
}

pub(super) fn source(payload: Box<[u8]>, port: u16) -> Result<()> {
    let engine = read_engine(payload.len(), port)?;
    let memory = Arc::new(HostMemory::new(payload)?);
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
        || status.completed_tasks != 0
        || status.transferred_bytes != 0
        || !matches!(tasks.as_slice(), [task] if !task.state.is_terminal()
            && !task.locally_drained && task.transferred_bytes == 0)
    {
        return Err(format!("READ no longer pending: {status:?}, {tasks:?}").into());
    }
    Ok(())
}

fn open(engine: &TransferEngine, peer: &str) -> Result<RemoteSegment> {
    let (addr, epoch) = peer.split_once(' ').ok_or("missing source epoch")?;
    Ok(engine.open_segment(addr.parse()?, epoch.parse()?, TIMEOUT)?)
}

pub(super) fn restart(peer: &str) -> Result<()> {
    let engine = Arc::new(TransferEngine::new(EngineConfig::default())?);
    let old = open(&engine, peer)?;
    let guard = [0xee; 5];
    let dst = engine.register_memory(Box::new(guard), Permission::LocalOnly)?;
    let request = |segment: &RemoteSegment| -> Result<_> {
        let [region] = segment.regions() else {
            return Err("expected one restart source region".into());
        };
        if region.len() != guard.len() - 2 {
            return Err("unexpected restart source length".into());
        }
        Ok(TransferRequest::new(
            Transport::Tcp,
            dst.base_address() + 1,
            region.base_address(),
            region.len(),
        ))
    };
    let read = |segment: &RemoteSegment, label: &str| -> Result<()> {
        let batch = engine.submit_async(
            TransferOp::Read,
            segment,
            &[request(segment)?],
            None,
            TIMEOUT,
        )?;
        finish_batch(batch, label, false)?;
        let mut bytes = [0; 5];
        dst.read(0, &mut bytes)?;
        println!("OBS\tBYTES\t{label}\t{}", hex(&bytes));
        Ok(())
    };
    read(&old, "old")?;
    dst.write(0, &guard)?;
    let mut replacement = String::new();
    io::stdin().read_line(&mut replacement)?;
    let fresh = open(&engine, replacement.trim_end())?;
    if fresh.endpoint() != old.endpoint() || fresh.source_epoch() == old.source_epoch() {
        return Err("expected a new source epoch at the original endpoint".into());
    }
    if !matches!(
        engine.open_segment(old.endpoint(), old.source_epoch(), TIMEOUT),
        Err(orbite::Error::EpochMismatch)
    ) {
        return Err("old Describe did not return EpochMismatch".into());
    }
    let old_request = [request(&old)?];
    let report = engine.submit_and_wait(TransferOp::Read, &old, &old_request, TIMEOUT)?;
    let mut bytes = [0; 5];
    dst.read(0, &mut bytes)?;
    let fenced = report.failure.as_ref().is_some_and(|failure| {
        matches!(failure.error, orbite::Error::EpochMismatch)
            && failure.remote_effect == RemoteEffect::None
    });
    if !fenced
        || !report.locally_drained
        || (report.completed_requests, report.transferred_bytes) != (0, 0)
        || bytes != guard
    {
        return Err(format!("stale READ returned {report:?} with destination {bytes:?}").into());
    }
    eprintln!("old Describe and READ: EpochMismatch, zero bytes, drained, destination intact");
    engine.close_segment(old.endpoint(), old.source_epoch(), TIMEOUT)?;
    if !matches!(
        engine.submit_and_wait(TransferOp::Read, &old, &old_request, TIMEOUT),
        Err(orbite::Error::PeerClosed)
    ) {
        return Err("closed descriptor still admitted a READ".into());
    }
    println!("RETIRED old");
    read(&fresh, "new")?;
    engine.close_segment(fresh.endpoint(), fresh.source_epoch(), TIMEOUT)?;
    engine.unregister_memory(&dst)?;
    engine.close(TIMEOUT)?;
    Ok(())
}

pub(super) fn interrupted(peer: &str, outcome: &str) -> Result<()> {
    let (state, task_state, bytes, fill) = match outcome {
        "cut" => (BatchState::Failed, TaskState::Failed, 0, 0xee),
        "resume" => (
            BatchState::Completed,
            TaskState::Completed,
            BULK_BYTES,
            0x6d,
        ),
        _ => return Err("unknown READ outcome".into()),
    };
    let engine = Arc::new(read_engine(BULK_BYTES, 0)?);
    let peer = open(&engine, peer)?;
    let [region] = peer.regions() else {
        return Err("expected one READ source".into());
    };
    let dst = Arc::new(HostMemory::new(vec![0xee; BULK_BYTES + 2].into())?);
    let owner = Arc::downgrade(&dst);
    let reg = engine.register_host_memory(dst.clone(), 0, dst.len(), Permission::LocalOnly)?;
    let request = |length| {
        TransferRequest::new(
            Transport::Tcp,
            dst.base_address() + 1,
            region.base_address(),
            length,
        )
    };
    let warm = engine.submit_async(TransferOp::Read, &peer, &[request(1)], None, TIMEOUT)?;
    finish_batch(warm, "warmup", false)?;
    let mut first = [0; 1];
    dst.read(1, &mut first)?;
    if first != [0x6d] {
        return Err("warm READ returned the wrong byte".into());
    }
    dst.write(1, &[0xee])?;
    let read = request(BULK_BYTES);
    println!("OPEN");
    command("submit")?;
    let mut batch = engine.submit_async(
        TransferOp::Read,
        &peer,
        &[read],
        None,
        std::time::Duration::from_secs(20),
    )?;
    engine.unregister_memory(&reg)?;
    drop((reg, dst));
    check_pending(&batch)?;
    println!("PENDING read");
    command("prefix")?;
    check_pending(&batch)?;
    let stats = engine.stats()?;
    if (
        stats.inflight_bytes,
        stats.retained_regions,
        stats.retained_bytes,
    ) != (BULK_BYTES, 1, BULK_BYTES + 2)
        || !matches!(batch.free(), Err(orbite::Error::BatchBusy))
    {
        return Err(format!("prefix lost pending READ ownership: {stats:?}").into());
    }
    let dst = owner.upgrade().ok_or("pending READ lost its backing")?;
    println!("PENDING prefix");
    command(outcome)?;
    let result = batch.wait(TIMEOUT)?;
    let tasks = batch.observe_tasks(None)?;
    let [task] = tasks.as_slice() else {
        return Err("expected one terminal READ".into());
    };
    if result.state != state
        || !result.locally_drained
        || result.completed_tasks != usize::from(bytes != 0)
        || result.transferred_bytes != bytes
        || task.state != task_state
        || !task.locally_drained
        || task.transferred_bytes != bytes
        || batch.status()? != result
    {
        return Err(format!("unexpected {outcome} result: {result:?}, {task:?}").into());
    }
    if state == BatchState::Failed
        && !task.failure.as_ref().is_some_and(|failure| {
            failure.remote_effect == RemoteEffect::None
                && matches!(&failure.error, orbite::Error::Io(e)
            if matches!(e.kind(), io::ErrorKind::UnexpectedEof | io::ErrorKind::ConnectionReset))
        })
    {
        return Err(format!("expected a broken READ reply: {task:?}").into());
    }
    let mut image = vec![0; BULK_BYTES + 2];
    dst.read(0, &mut image)?;
    if image[0] != 0xee
        || image[BULK_BYTES + 1] != 0xee
        || !image[1..=BULK_BYTES].iter().all(|&b| b == fill)
    {
        return Err(format!("wrong destination bytes after {outcome}").into());
    }
    batch.free()?;
    drop(dst);
    let stats = engine.stats()?;
    if owner.upgrade().is_some()
        || (
            stats.allocated_batches,
            stats.inflight_bytes,
            stats.retained_regions,
            stats.retained_bytes,
        ) != (0, 0, 0, 0)
    {
        return Err(format!("drained READ kept its backing or charges: {stats:?}").into());
    }
    engine.close(TIMEOUT)?;
    let status = if result.state == BatchState::Completed {
        "completed"
    } else {
        "failed"
    };
    println!(
        "OBS\tRESULT\t{outcome}\t{status}\t{}",
        result.transferred_bytes
    );
    println!("OBS\tBYTES\t{outcome}\t{bytes}\tguards-intact");
    Ok(())
}

pub(super) fn reads(slow_peer: &str, fast_peer: &str) -> Result<()> {
    let engine = Arc::new(TransferEngine::new(EngineConfig::default())?);
    let slow = open(&engine, slow_peer)?;
    let fast = open(&engine, fast_peer)?;
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
    let stats = engine.stats()?;
    if (stats.allocated_batches, stats.allocated_requests) != (1, 1) {
        return Err(format!(
            "expected only the first batch after freeing the later one: {stats:?}"
        )
        .into());
    }
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
