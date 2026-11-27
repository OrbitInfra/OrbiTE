//! Public two-process control for two grants on one long-lived, direction-specific QP.
use super::*;
use orbite::{EngineConfig, Permission, transport::rdma::RdmaConfig};
use std::net::IpAddr;

const WARM: usize = 4;
const A: usize = 20;
const B: usize = 40;
const N: usize = 4;

pub(super) fn run(args: &[String]) -> Result<()> {
    let [role, address, local_ip, device, port, gid] = args else {
        return Err("windows listen|connect CONTROL_ADDR LOCAL_IP DEVICE PORT GID_INDEX".into());
    };
    let source = match role.as_str() {
        "listen" => true,
        "connect" => false,
        _ => return Err("expected listen or connect".into()),
    };
    let bind = SocketAddr::new(local_ip.parse::<IpAddr>()?, 0);
    let mut rdma = RdmaConfig::default();
    rdma.rdma_whitelist = vec![device.clone()];
    rdma.port = Some(port.parse()?);
    rdma.gid_index = Some(gid.parse()?);
    rdma.num_lanes = 1;
    let mut config = EngineConfig::default();
    config.bind_addr = bind;
    config.max_failover_attempts = 0;
    config.handler_threads = 4;
    config.handler_timeout = TIMEOUT;
    config.rdma = Some(rdma);
    let engine = TransferEngine::new(config)?;
    let memory = engine.register_memory(
        vec![0; 64].into_boxed_slice(),
        if source {
            Permission::RemoteReadWrite
        } else {
            Permission::LocalOnly
        },
    )?;
    let addr = address.parse()?;
    let mut stream = control(source, addr, Some(addr))?;
    let gate = source
        .then(|| gate::GrantGate::new(bind, engine.endpoint()))
        .transpose()?;
    let endpoint = gate.as_ref().map_or(engine.endpoint(), |g| g.address());
    let peer_addr = exchange(&mut stream, &endpoint.to_string())?.parse()?;
    let epoch = exchange(&mut stream, &engine.source_epoch().to_string())?.parse()?;
    let peer = if source {
        None
    } else {
        Some(engine.open_segment(peer_addr, epoch, TIMEOUT)?)
    };

    for op in [TransferOp::Read, TransferOp::Write] {
        let pattern = |source: bool| -> Vec<u8> {
            (0..64).map(|n| n + if source { 128 } else { 16 }).collect()
        };
        let mut expected = pattern(source);
        memory.write(0, &expected)?;
        barrier(&mut stream, "initialized")?;
        let transfer = |offset: usize| -> Result<()> {
            let peer = peer.as_ref().ok_or("missing source")?;
            let region = peer.regions().first().ok_or("missing remote region")?;
            let request = TransferRequest::new(
                Transport::Rdma,
                memory.base_address() + offset as u64,
                region.base_address() + offset as u64,
                N,
            );
            let report = engine.submit_and_wait(op, peer, &[request], TIMEOUT)?;
            if let Some(failure) = report.failure {
                return Err(failure.error.into());
            }
            if report.transferred_bytes != N {
                return Err("short RDMA transfer".into());
            }
            Ok(())
        };
        if !source {
            transfer(WARM)?;
        }
        barrier(&mut stream, "warm")?;
        let baseline = engine.stats()?.rdma.ok_or("missing RDMA counters")?;
        if let Some(gate) = &gate {
            gate.hold_one()?;
            barrier(&mut stream, "armed")?;
            let until = Instant::now() + TIMEOUT;
            while gate.held()? == 0 {
                if Instant::now() >= until {
                    return Err("B did not bind".into());
                }
                thread::sleep(Duration::from_millis(1));
            }
            barrier(&mut stream, "B bound")?;
            barrier(&mut stream, "A retired")?;
            let live = engine.stats()?.rdma.ok_or("missing RDMA counters")?;
            if live.opened_connections != baseline.opened_connections
                || live.active_windows != 1
                || live.completed_window_binds != baseline.completed_window_binds + 2
                || live.completed_window_invalidations
                    != baseline.completed_window_invalidations + 1
                || live.direct_bytes != N
                || memory.read(B, &mut [0; N]).is_ok()
            {
                return Err(format!("B lost its independent live window: {live:?}").into());
            }
            let mut a = [0; N];
            memory.read(A, &mut a)?;
            memory.write(A, &a)?;
            gate.resume()?;
        } else {
            barrier(&mut stream, "armed")?;
            thread::scope(|scope| -> Result<()> {
                let delayed = scope.spawn(|| transfer(B));
                barrier(&mut stream, "B bound")?;
                transfer(A)?;
                barrier(&mut stream, "A retired")?;
                delayed.join().map_err(|_| "B panicked")?
            })?;
        }
        barrier(&mut stream, "B retired")?;
        if source == (op == TransferOp::Write) {
            let other = pattern(!source);
            for i in [WARM, A, B] {
                expected[i..i + N].copy_from_slice(&other[i..i + N]);
            }
        }
        let mut actual = [0; 64];
        memory.read(0, &mut actual)?;
        let finished = engine.stats()?.rdma.ok_or("missing RDMA counters")?;
        if actual.as_slice() != expected
            || finished.opened_connections != baseline.opened_connections
            || finished.closed_connections != baseline.closed_connections
            || finished.completed_reusable_grants != baseline.completed_reusable_grants + 2
            || finished.active_windows != 0
            || finished.direct_bytes != 0
        {
            return Err(
                format!("shared window {op:?} byte or retirement mismatch: {finished:?}").into(),
            );
        }
        println!(
            "{role} {op:?}: A retired before B transferred on the same QP; all bytes and guards verified"
        );
        barrier(&mut stream, "verified")?;
    }
    if let Some(gate) = &gate {
        gate.arm()?;
        barrier(&mut stream, "interrupt armed")?;
        let until = Instant::now() + TIMEOUT;
        while gate.held()? != 1 {
            if Instant::now() >= until {
                return Err("interruption did not reach bound window".into());
            }
            thread::sleep(Duration::from_millis(1));
        }
        if memory.write(B, &[0; N]).is_ok() {
            return Err("interrupted window lost its loan".into());
        }
        gate.abort()?;
    } else if let Some(peer) = &peer {
        barrier(&mut stream, "interrupt armed")?;
        let r = TransferRequest::new(
            Transport::Rdma,
            memory.base_address() + B as u64,
            peer.regions()[0].base_address() + B as u64,
            N,
        );
        let result = engine.submit_and_wait(TransferOp::Write, peer, &[r], TIMEOUT)?;
        if result.is_complete() {
            return Err("disconnected grant reported success".into());
        }
    }
    barrier(&mut stream, "interrupted")?;
    if let Some(gate) = gate {
        gate.close()?;
    }
    engine.close(TIMEOUT)?;
    let stats = engine.stats()?.rdma.ok_or("missing RDMA counters")?;
    if stats.opened_connections != stats.closed_connections
        || stats.active_grants != 0
        || stats.retained_grants != 0
        || stats.persistent_mrs != 0
    {
        return Err("shared-window connection did not drain".into());
    }
    memory.read(B, &mut [0; N])?;
    println!("{role} closed the interrupted QP and recovered the bound range");
    Ok(())
}
