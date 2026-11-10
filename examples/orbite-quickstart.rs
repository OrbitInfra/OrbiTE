//! Installed, byte-checked TCP (two processes) and Linux io_uring quickstart.
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::time::Duration;

use orbite::{EngineConfig, Permission, TransferEngine, TransferOp, TransferRequest, Transport};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const WAIT: Duration = Duration::from_secs(30);

fn check(ok: bool, message: &str) -> Result<()> {
    if ok { Ok(()) } else { Err(message.into()) }
}

fn source() -> Result<()> {
    let engine = TransferEngine::new(EngineConfig::default())?;
    let memory = engine.register_memory(
        b"LLsourceRR".to_vec().into_boxed_slice(),
        Permission::RemoteReadWrite,
    )?;
    println!(
        "{}",
        serde_json::json!([
            engine.endpoint().to_string(),
            engine.source_epoch().to_string(),
            memory.base_address()
        ])
    );
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    check(line.trim() == "done", "requester did not complete")?;
    let mut bytes = [0; 10];
    memory.read(0, &mut bytes)?;
    check(
        &bytes == b"LLtargetRR",
        "TCP WRITE or source guard mismatch",
    )?;
    engine.unregister_memory(&memory)?;
    engine.close(WAIT)?;
    Ok(())
}

fn run() -> Result<()> {
    let mut child = Command::new(std::env::current_exe()?)
        .arg("--source")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    let result = (|| -> Result<()> {
        let mut metadata = String::new();
        BufReader::new(child.stdout.take().ok_or("missing child output")?)
            .read_line(&mut metadata)?;
        let (endpoint, epoch, address): (String, String, u64) = serde_json::from_str(&metadata)?;
        let engine = TransferEngine::new(EngineConfig::default())?;
        let memory = engine.register_memory(
            b"xx------yy".to_vec().into_boxed_slice(),
            Permission::LocalOnly,
        )?;
        let peer = engine.open_segment(endpoint.parse()?, epoch.parse()?, WAIT)?;
        let request =
            TransferRequest::new(Transport::Tcp, memory.base_address() + 2, address + 2, 6);
        let report = engine.submit_and_wait(
            TransferOp::Read,
            &peer,
            std::slice::from_ref(&request),
            WAIT,
        )?;
        check(
            report.is_complete() && report.transferred_bytes == 6,
            "incomplete TCP READ",
        )?;
        let mut bytes = [0; 10];
        memory.read(0, &mut bytes)?;
        check(
            &bytes == b"xxsourceyy",
            "TCP READ or destination guard mismatch",
        )?;
        memory.write(2, b"target")?;
        check(
            engine
                .submit_and_wait(TransferOp::Write, &peer, &[request], WAIT)?
                .is_complete(),
            "incomplete TCP WRITE",
        )?;
        child
            .stdin
            .take()
            .ok_or("missing child input")?
            .write_all(b"done\n")?;
        check(child.wait()?.success(), "source verification failed")?;
        engine.unregister_memory(&memory)?;
        engine.close(WAIT)?;
        println!("TCP: two-process READ/WRITE and guards verified");
        file_roundtrip()
    })();
    // A failed requester must not strand its demonstration child.
    if result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    result
}

#[cfg(all(target_os = "linux", feature = "io-uring"))]
fn file_roundtrip() -> Result<()> {
    let engine = TransferEngine::new(EngineConfig::default())?;
    let path = std::env::temp_dir().join(format!("orbite-quickstart-{}", engine.source_epoch()));
    let result = (|| -> Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)?;
        file.write_all(b"LL------RR")?;
        let segment = engine.open_file_segment(
            &format!("file://{}", path.display()),
            Permission::RemoteReadWrite,
        )?;
        let memory = engine.register_memory(
            b"xxstoragyy".to_vec().into_boxed_slice(),
            Permission::LocalOnly,
        )?;
        let request = TransferRequest::new(Transport::IoUring, memory.base_address() + 2, 2, 6);
        check(
            engine
                .submit_and_wait(
                    TransferOp::Write,
                    &segment,
                    std::slice::from_ref(&request),
                    WAIT,
                )?
                .is_complete(),
            "incomplete file WRITE",
        )?;
        check(
            std::fs::read(&path)? == b"LLstoragRR",
            "file WRITE or guard mismatch",
        )?;
        memory.write(2, b"------")?;
        check(
            engine
                .submit_and_wait(TransferOp::Read, &segment, &[request], WAIT)?
                .is_complete(),
            "incomplete file READ",
        )?;
        let mut bytes = [0; 10];
        memory.read(0, &mut bytes)?;
        check(&bytes == b"xxstoragyy", "file READ or guard mismatch")?;
        engine.close_file_segment(&segment, WAIT)?;
        engine.unregister_memory(&memory)?;
        engine.close(WAIT)?;
        println!("io_uring: DRAM/file READ/WRITE and guards verified");
        Ok(())
    })();
    let cleanup = std::fs::remove_file(&path);
    result?;
    cleanup?;
    Ok(())
}

#[cfg(not(all(target_os = "linux", feature = "io-uring")))]
fn file_roundtrip() -> Result<()> {
    Err("the file quickstart requires Linux and --features io-uring".into())
}

fn main() -> Result<()> {
    if std::env::args().nth(1).as_deref() == Some("--source") {
        source()
    } else {
        run()
    }
}
