//! Installed-only CUDA registration-route and retirement gate. No hardware auto-run.
use orbite::cuda::CudaMemory;
use orbite::transport::rdma::{GpuDirectRegistration, RdmaConfig, RdmaCudaMemory};
use orbite::{
    EngineConfig, Error, Permission, RegisteredMemory, RemoteSegment, SourceEpoch, TransferEngine,
    TransferOp, TransferRequest, Transport,
};
use std::{
    io::{BufRead, Write},
    sync::Arc,
    time::{Duration, Instant},
};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const WAIT: Duration = Duration::from_secs(60);
const PAYLOAD: usize = 32 << 20;
const GUARD: usize = 4096;
const LEN: usize = PAYLOAD + 2 * GUARD;

fn say(s: &str) -> Result<()> {
    println!("{s}");
    std::io::stdout().flush()?;
    Ok(())
}
fn line() -> Result<String> {
    let mut s = String::new();
    use std::io::Read;
    if std::io::stdin().lock().take(1025).read_line(&mut s)? == 0 || s.len() > 1024 {
        return Err("invalid coordinator line".into());
    }
    Ok(s.trim().into())
}
fn check(m: &RegisteredMemory, edge: u8, value: u8) -> Result<()> {
    let mut bytes = vec![0; LEN];
    m.read(0, &mut bytes)?;
    if bytes[..GUARD]
        .iter()
        .chain(bytes[GUARD + PAYLOAD..].iter())
        .any(|b| *b != edge)
        || bytes[GUARD..GUARD + PAYLOAD].iter().any(|b| *b != value)
    {
        return Err("payload or guard mismatch".into());
    }
    Ok(())
}
fn evidence(
    e: &TransferEngine,
    policy: GpuDirectRegistration,
    peer: Option<&RemoteSegment>,
) -> Result<()> {
    let s = e.stats()?.rdma.ok_or("missing RDMA statistics")?;
    let expected = if policy == GpuDirectRegistration::Dmabuf {
        (s.dmabuf_registrations, s.peermem_registrations)
    } else {
        (s.peermem_registrations, s.dmabuf_registrations)
    };
    if expected.0 == 0 || expected.1 != 0 {
        return Err("wrong or missing native registration route".into());
    }
    if let Some(p) = peer {
        let r = p.regions()[0].gpu_direct_routes();
        if (r.dmabuf, r.peermem)
            != (
                policy == GpuDirectRegistration::Dmabuf,
                policy == GpuDirectRegistration::Peermem,
            )
        {
            return Err("Describe route evidence mismatch".into());
        }
    }
    say(&format!(
        "ROUTE dmabuf={} peermem={}",
        s.dmabuf_registrations, s.peermem_registrations
    ))
}
fn exchange(e: &TransferEngine, locals: &[RegisteredMemory], p: &RemoteSegment) -> Result<()> {
    let mut expected = [0x5a; 2];
    if p.regions().len() != 2 {
        return Err("expected GPU and host regions".into());
    }
    for local in locals {
        for (i, remote) in p.regions().iter().enumerate() {
            let q = TransferRequest::new(
                Transport::Rdma,
                local.base_address() + GUARD as u64,
                remote.base_address() + GUARD as u64,
                PAYLOAD,
            );
            local.write(0, &vec![0xcd; LEN])?;
            let read = e.submit_and_wait(TransferOp::Read, p, std::slice::from_ref(&q), WAIT)?;
            if !read.is_complete() || !read.locally_drained {
                return Err(format!("READ {read:?}").into());
            }
            check(local, 0xcd, expected[i])?;
            local.write(GUARD, &vec![0x6b; PAYLOAD])?;
            let write = e.submit_and_wait(TransferOp::Write, p, std::slice::from_ref(&q), WAIT)?;
            if !write.is_complete() || !write.locally_drained {
                return Err(format!("WRITE {write:?}").into());
            }
            check(local, 0xcd, 0x6b)?;
            expected[i] = 0x6b;
        }
    }
    Ok(())
}
fn retire_running(
    e: &Arc<TransferEngine>,
    local: &RegisteredMemory,
    p: &RemoteSegment,
) -> Result<()> {
    local.write(0, &vec![0xcd; LEN])?;
    let q = TransferRequest::new(
        Transport::Rdma,
        local.base_address() + GUARD as u64,
        p.regions()[0].base_address() + GUARD as u64,
        PAYLOAD,
    );
    let prior = e
        .stats()?
        .rdma
        .ok_or("RDMA statistics")?
        .posted_work_requests;
    let mut batch = e.allocate_batch(1)?;
    batch.submit(TransferOp::Read, p, &[q], None, WAIT)?;
    let until = Instant::now() + WAIT;
    loop {
        let s = e.stats()?;
        if s.rdma.ok_or("RDMA statistics")?.posted_work_requests > prior
            && !batch.status()?.locally_drained
        {
            break;
        }
        if batch.status()?.locally_drained || Instant::now() >= until {
            return Err(
                "no observed in-flight native READ; this control is inconclusive, not a pass"
                    .into(),
            );
        }
        std::thread::yield_now();
    }
    // Retirement fences first and waits for the exact admitted owner. Never
    // treat the preceding observation, a timeout, or cancellation as its receipt.
    e.close_segment(p.endpoint(), p.source_epoch(), WAIT)?;
    let status = batch.wait(WAIT)?;
    if !status.locally_drained || status.completed_tasks != 1 {
        return Err(format!("retirement did not drain READ: {status:?}").into());
    }
    check(local, 0xcd, 0x6b)?;
    batch.free()?;
    if e.stats()?.inflight_bytes != 0 {
        return Err("retirement released without charge receipt".into());
    }
    Ok(())
}
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args == ["--protocol"] {
        return say("orbite-installed-gdr-dmabuf");
    }
    let [role, route, bind, nic, gid, peer @ ..] = &args[..] else {
        return Err("ROLE ROUTE BIND RNIC GID [ENDPOINT EPOCH]".into());
    };
    let policy = match route.as_str() {
        "dmabuf" => GpuDirectRegistration::Dmabuf,
        "peermem" => GpuDirectRegistration::Peermem,
        _ => return Err("explicit route required".into()),
    };
    let loaded = std::path::Path::new("/sys/module/nvidia_peermem").is_dir();
    if loaded != (policy == GpuDirectRegistration::Peermem) {
        return Err("kernel peermem state differs from requested profile".into());
    }
    let mut r = RdmaConfig::new(nic);
    r.gid_index = Some(gid.parse()?);
    r.cuda_memory = RdmaCudaMemory::Direct;
    r.gpu_direct_registration = policy;
    r.max_direct_bytes = 256 << 20;
    r.max_cached_mr_bytes = 256 << 20;
    let mut c = EngineConfig::default();
    c.rdma = Some(r);
    c.tcp = false;
    c.max_failover_attempts = 0;
    c.bind_addr = bind.parse()?;
    c.handler_timeout = WAIT;
    c.max_transfer_bytes = PAYLOAD;
    let e = Arc::new(TransferEngine::new(c)?);
    let permission = if role == "source" {
        Permission::RemoteReadWrite
    } else {
        Permission::LocalOnly
    };
    let gpu = Arc::new(CudaMemory::allocate(0, LEN)?);
    let gpu = e
        .register_cuda_memory_batch(&[(gpu, 0, LEN)], permission)?
        .remove(0);
    let host = e.register_memory(vec![0x5a; LEN].into_boxed_slice(), permission)?;
    let local = [gpu, host];
    for m in &local {
        m.write(0, &vec![0x5a; LEN])?;
    }
    if role == "source" {
        say(&format!("READY {} {}", e.endpoint(), e.source_epoch()))?;
        if line()? != "VERIFY" {
            return Err("expected VERIFY".into());
        }
        for m in &local {
            check(m, 0x5a, 0x6b)?;
        }
        evidence(&e, policy, None)?;
    } else if role == "requester" {
        let [endpoint, epoch] = peer else {
            return Err("authoritative endpoint and epoch required".into());
        };
        let old = e.open_segment(endpoint.parse()?, epoch.parse()?, WAIT)?;
        exchange(&e, &local, &old)?;
        evidence(&e, policy, Some(&old))?;
        retire_running(&e, &local[0], &old)?;
        say("RETIRED")?;
        let new = line()?;
        let words: Vec<_> = new.split_whitespace().collect();
        let ["NEW", epoch] = words.as_slice() else {
            return Err("expected NEW epoch".into());
        };
        let epoch: SourceEpoch = epoch.parse()?;
        if epoch == old.source_epoch() {
            return Err("source restart reused epoch".into());
        }
        let fresh = e.open_segment(old.endpoint(), epoch, WAIT)?;
        exchange(&e, &local, &fresh)?;
        evidence(&e, policy, Some(&fresh))?;
        if !matches!(
            e.open_segment(old.endpoint(), old.source_epoch(), WAIT),
            Err(Error::PeerClosed)
        ) {
            return Err("stale epoch reopened".into());
        }
        e.close_segment(fresh.endpoint(), fresh.source_epoch(), WAIT)?;
    } else {
        return Err("unknown role".into());
    }
    for m in &local {
        e.unregister_memory(m)?;
    }
    e.close(WAIT)?;
    let s = e.stats()?;
    let r = s.rdma.ok_or("RDMA statistics")?;
    if s.inflight_bytes != 0
        || r.active_grants != 0
        || r.persistent_mrs != 0
        || r.retained_endpoints != 0
    {
        return Err("native owners remain".into());
    }
    say(&format!("PASS {role}"))
}
