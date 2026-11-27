use std::fs;
use std::os::unix::fs::FileExt;

use super::*;
use crate::transport::io_uring::tests::{Fixture, kernel};
use crate::{EngineConfig, FileIoElement, TransferEngine, TransferRequest, Transport};

impl FileSegment {
    pub(crate) fn override_test_direct_offset_alignment(&self, alignment: u64) -> Result<()> {
        let entry = self.0.upgrade().ok_or(Error::FileSegmentClosed)?;
        let mut state = entry.lock()?;
        let direct = state.direct.as_mut().expect("warm direct descriptor");
        assert!(direct.file.is_some());
        direct.offset_alignment = alignment;
        Ok(())
    }
}

#[test]
fn padded_vectors_apply_device_memory_and_offset_alignment_independently() -> Result<()> {
    if !kernel() {
        return Ok(());
    }
    const BLOCK: usize = 4096;
    let image = |seed: u8| {
        (0..5 * BLOCK)
            .map(|i| seed + (i / BLOCK) as u8)
            .collect::<Vec<_>>()
    };
    let buffered = image(0x20);
    let direct = image(0x80);
    let f = Fixture::new(
        &std::env::temp_dir(),
        &buffered,
        TransferEngine::new(EngineConfig::default())?,
    )?;
    // Distinct bytes identify the fd submitted by the real READV/WRITEV.
    // Only the cached descriptor and its reported DIO geometry are injected.
    let marker_path = f.path.with_extension("geometry");
    let marker = Arc::new(
        OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&marker_path)?,
    );
    fs::remove_file(marker_path)?;
    marker.write_all_at(&direct, 0)?;
    let entry = f.segment.0.upgrade().ok_or(Error::FileSegmentClosed)?;
    let geometry = |offset_alignment| -> Result<()> {
        entry.lock()?.direct = Some(DirectFile {
            file: Some(Arc::clone(&marker)),
            memory_alignment: 2 * BLOCK,
            offset_alignment,
        });
        Ok(())
    };
    geometry(BLOCK as u64)?;
    let base = f.memory.base_address();
    let address = base.next_multiple_of((2 * BLOCK) as u64);
    let start = (address - base) as usize;
    let middle = start + 2 * BLOCK;
    let request = TransferRequest::file_vectored(
        Transport::IoUring,
        0,
        &[
            (address, BLOCK).into(),
            FileIoElement::Padding(BLOCK),
            FileIoElement::Padding(BLOCK),
            (address + (2 * BLOCK) as u64, BLOCK).into(),
        ],
    )?;
    let requests = std::slice::from_ref(&request);
    assert!(f.run(TransferOp::Read, requests)?.is_complete());
    let mut expected = vec![0x9a; f.memory.len()];
    expected[start..start + BLOCK].copy_from_slice(&direct[..BLOCK]);
    expected[middle..middle + BLOCK].copy_from_slice(&direct[3 * BLOCK..4 * BLOCK]);
    let mut got = vec![0; expected.len()];
    f.memory.read(0, &mut got)?;
    assert!(
        got == expected,
        "direct READ must publish only its tagged range bytes"
    );

    // The packed padding addresses differ by 4 KiB. At least one fails the
    // 8 KiB memory alignment, so the writer must use the buffered descriptor.
    assert!(f.run(TransferOp::Write, requests)?.is_complete());
    let mut disk = buffered.clone();
    disk[..BLOCK].copy_from_slice(&direct[..BLOCK]);
    disk[BLOCK..3 * BLOCK].fill(0);
    disk[3 * BLOCK..4 * BLOCK].copy_from_slice(&direct[3 * BLOCK..4 * BLOCK]);
    assert!(
        fs::read(&f.path)? == disk,
        "WRITE must use the buffered file and zero both gaps"
    );
    let mut marker_bytes = vec![0; direct.len()];
    marker.read_exact_at(&mut marker_bytes, 0)?;
    assert!(
        marker_bytes == direct,
        "WRITE touched the direct descriptor"
    );

    fs::write(&f.path, &buffered)?;
    geometry((2 * BLOCK) as u64)?;
    // The complete vector is 16 KiB, but its 4 KiB elements violate the
    // reported length alignment. The first destination must see buffered bytes.
    assert!(f.run(TransferOp::Read, requests)?.is_complete());
    let mut first = vec![0; BLOCK];
    f.memory.read(start, &mut first)?;
    assert!(
        first == buffered[..BLOCK],
        "misaligned element length selected direct I/O"
    );

    // Every element now meets the length alignment, but the file offset doesn't.
    let request = TransferRequest::file_vectored(
        Transport::IoUring,
        BLOCK as u64,
        &[
            (address, 2 * BLOCK).into(),
            FileIoElement::Padding(2 * BLOCK),
        ],
    )?;
    assert!(f.run(TransferOp::Read, &[request])?.is_complete());
    f.memory.read(start, &mut first)?;
    assert!(
        first == buffered[BLOCK..2 * BLOCK],
        "misaligned file offset selected direct I/O"
    );
    Ok(())
}
