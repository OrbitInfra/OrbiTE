//! Exercise the packaged host RDMA API with a private source PD on Linux.
use orbite::transport::rdma::RdmaConfig;
use orbite::{EngineConfig, Permission, TransferEngine, TransferOp, TransferRequest, Transport};
use std::{error::Error, time::Duration};

fn engine(device: &str, port: u8, gid: u8, reuse: bool) -> orbite::Result<TransferEngine> {
    let mut rdma = RdmaConfig::default();
    rdma.rdma_whitelist = vec![device.into()];
    rdma.port = Some(port);
    rdma.gid_index = Some(gid);
    rdma.host_source_reuse = reuse;
    rdma.num_lanes = 1;
    let mut config = EngineConfig::default();
    config.rdma = Some(rdma);
    TransferEngine::new(config)
}

fn main() -> Result<(), Box<dyn Error>> {
    let wait = Duration::from_secs(8);
    let args: Vec<_> = std::env::args().skip(1).collect();
    let [device, port, gid] = args.as_slice() else {
        return Err("usage: rdma_roundtrip DEVICE PORT GID_INDEX".into());
    };
    let port = port.parse()?;
    let gid = gid.parse()?;
    let source = engine(device, port, gid, false)?;
    let requester = engine(device, port, gid, true)?;
    let mut source_bytes = [0xdd; 32];
    source_bytes[7..15].copy_from_slice(b"OrbitRXE");
    let mut local_bytes = [0x99; 32];
    let exported = source.register_memory(
        source_bytes.to_vec().into_boxed_slice(),
        Permission::RemoteReadWrite,
    )?;
    let local = requester.register_memory(
        local_bytes.to_vec().into_boxed_slice(),
        Permission::LocalOnly,
    )?;
    let peer = requester.open_segment(source.endpoint(), source.source_epoch(), wait)?;
    let region = peer
        .regions()
        .iter()
        .find(|r| r.id() == exported.id())
        .ok_or("missing remote region")?;
    let request = || {
        TransferRequest::new(
            Transport::Rdma,
            local.base_address() + 3,
            region.base_address() + 7,
            8,
        )
    };
    for op in [TransferOp::Read, TransferOp::Write] {
        if op == TransferOp::Write {
            local.write(3, b"RDMAwr!!")?;
            local_bytes[3..11].copy_from_slice(b"RDMAwr!!");
            source_bytes[7..15].copy_from_slice(b"RDMAwr!!");
        } else {
            local_bytes[3..11].copy_from_slice(b"OrbitRXE");
        }
        let report = requester.submit_and_wait(op, &peer, &[request()], wait)?;
        if let Some(failure) = report.failure {
            return Err(failure.error.into());
        }
        assert_eq!(report.transferred_bytes, 8);
        let mut bytes = [0; 32];
        local.read(0, &mut bytes)?;
        assert_eq!(bytes, local_bytes);
        exported.read(0, &mut bytes)?;
        assert_eq!(bytes, source_bytes);
    }
    requester.close(wait)?;
    source.close(wait)?;
    println!("Verified RDMA READ and WRITE, both 32-byte buffers and engine shutdown on {device}.");
    Ok(())
}
