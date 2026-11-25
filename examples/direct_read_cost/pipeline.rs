use super::*;
use std::fs::File;

pub(super) fn bias(mode: &str) -> usize {
    if mode.contains("off512") {
        512
    } else if mode.contains("off2k") {
        2048
    } else {
        0
    }
}

#[target_feature(enable = "avx2")]
unsafe fn grouped_nt(src: *const u8, dst: *mut u8, len: usize) {
    assert_eq!((src as usize | dst as usize | len) % 32, 0);
    assert_eq!(len % 256, 0);
    for at in (0..len).step_by(256) {
        let source = src.wrapping_add(at);
        let target = dst.wrapping_add(at);
        let prefetch = src.wrapping_add((at + 2048).min(len - 64));
        // SAFETY: 256 aligned confirmed source bytes and 256 disjoint destination
        // bytes remain live. The caller fences all streaming stores before return.
        unsafe {
            std::arch::asm!(
                "prefetcht0 [{prefetch}]",
                "vmovdqa ymm0, [{source}]", "vmovdqa ymm1, [{source}+32]",
                "vmovdqa ymm2, [{source}+64]", "vmovdqa ymm3, [{source}+96]",
                "vmovdqa ymm4, [{source}+128]", "vmovdqa ymm5, [{source}+160]",
                "vmovdqa ymm6, [{source}+192]", "vmovdqa ymm7, [{source}+224]",
                "vmovntdq [{target}], ymm0", "vmovntdq [{target}+32], ymm1",
                "vmovntdq [{target}+64], ymm2", "vmovntdq [{target}+96], ymm3",
                "vmovntdq [{target}+128], ymm4", "vmovntdq [{target}+160], ymm5",
                "vmovntdq [{target}+192], ymm6", "vmovntdq [{target}+224], ymm7",
                source=in(reg) source, target=in(reg) target, prefetch=in(reg) prefetch,
                out("ymm0") _, out("ymm1") _, out("ymm2") _, out("ymm3") _,
                out("ymm4") _, out("ymm5") _, out("ymm6") _, out("ymm7") _,
                options(nostack, preserves_flags)
            )
        }
    }
}

#[derive(Default)]
pub(super) struct Receipt {
    pub(super) prefix: usize,
    // A later request did not yet have a visible CQE when a copy started.
    pub(super) overlap_opportunities: usize,
    // A separate native CQE actually became visible during a copy.
    pub(super) arrivals_during_copy: usize,
    pub(super) retired: usize,
    pub(super) successes_after_gap: usize,
    pub(super) copies: [u64; 3],
}

pub(super) struct Control<'a> {
    pub(super) truncate_after_first: Option<(&'a File, u64)>,
    pub(super) bad_index: Option<usize>,
}

impl Window {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn rolling_read(
        &mut self,
        fd: i32,
        mode: &str,
        total: usize,
        chunk: usize,
        dst: *mut u8,
        control: Option<&Control<'_>>,
        detailed: bool,
    ) -> Receipt {
        let slots = self.iovs.len();
        let chunks = total / chunk;
        let mut result = Receipt::default();
        let mut posted = 0;
        let mut next = 0;
        let mut stopped = false;
        let mut queued = 0;
        let burst = mode.contains("burst");
        let grouped = mode.ends_with("-gnt");
        let mut cqes = vec![None; slots];
        let bad_index = control.and_then(|c| c.bad_index);
        let mut truncate = control.and_then(|c| c.truncate_after_first);
        let queue = |ring: &mut IoUring, index: usize| {
            let slot = index % slots;
            let file = types::Fd(if Some(index) == bad_index { -1 } else { fd });
            let offset = (4096 + index * chunk) as u64;
            let entry = if mode.starts_with("fixed") {
                opcode::ReadFixed::new(
                    file,
                    self.iovs[slot].iov_base.cast(),
                    chunk as u32,
                    slot as u16,
                )
                .offset(offset)
                .build()
            } else {
                opcode::Readv::new(file, &self.iovs[slot], 1)
                    .offset(offset)
                    .build()
            }
            .user_data(index as u64);
            // SAFETY: uniquely occupied slot and immutable iovec survive until its CQE.
            if unsafe { ring.submission().push(&entry) }.is_err() {
                std::process::abort()
            }
        };
        while posted < slots.min(chunks) {
            queue(&mut self.ring, posted);
            posted += 1;
        }
        let submit = |r: &mut IoUring, count| {
            // The diagnostic exits its process on uncertain submission. It never
            // unwinds across storage that might still be referenced by the kernel.
            if r.submit().ok() != Some(count) {
                std::process::abort()
            }
        };
        submit(&mut self.ring, posted);
        while next < posted {
            let slot = next % slots;
            while cqes[slot].is_none() {
                if queued > 0 && self.ring.completion().is_empty() {
                    submit(&mut self.ring, queued);
                    queued = 0;
                }
                if self.ring.completion().is_empty() && self.ring.submit_and_wait(1).is_err() {
                    std::process::abort()
                }
                for c in self.ring.completion() {
                    let id = c.user_data() as usize;
                    if id < next || id >= posted || cqes[id % slots].replace(c.result()).is_some() {
                        std::process::abort()
                    }
                }
            }
            let received = cqes[slot].take().unwrap().max(0) as usize;
            if received > chunk {
                std::process::abort()
            }
            result.retired += 1;
            if stopped {
                result.successes_after_gap += usize::from(received == chunk);
            } else {
                if received > 0 {
                    let measure = detailed.then(|| {
                        let before_cq = self.ring.completion().len();
                        let known = cqes.iter().filter(|s| s.is_some()).count();
                        result.overlap_opportunities +=
                            usize::from(posted - next - 1 - queued > known + before_cq);
                        (Time::now(), before_cq)
                    });
                    let src = self.iovs[slot].iov_base.cast::<u8>();
                    let target = dst.wrapping_add(next * chunk);
                    // SAFETY: this slot returned its terminal CQE. The caller owns the
                    // whole output and we only publish the contiguous confirmed prefix.
                    unsafe {
                        if grouped && received == chunk {
                            grouped_nt(src, target, received);
                        } else if mode.ends_with("-nt") && received == chunk {
                            stream_copy(src, target, received);
                        } else {
                            std::ptr::copy_nonoverlapping(src, target, received)
                        }
                    }
                    if let Some((t, before_cq)) = measure {
                        add(&mut result.copies, Time::now().since(&t));
                        result.arrivals_during_copy += self.ring.completion().len() - before_cq;
                    }
                    result.prefix += received;
                }
                stopped = received < chunk;
            }
            next += 1;
            if next == 1 {
                if let Some((file, len)) = truncate.take() {
                    file.set_len(len).unwrap()
                }
            }
            if !stopped && posted < chunks {
                queue(&mut self.ring, posted);
                posted += 1;
                queued += 1;
                if !burst || queued >= (slots / 2).max(1) {
                    submit(&mut self.ring, queued);
                    queued = 0;
                }
            }
        }
        if grouped {
            // SAFETY: x86-64 provides SSE2. Publish all streaming stores before
            // returning the completed prefix to the caller for inspection.
            unsafe { std::arch::x86_64::_mm_sfence() }
        }
        assert_eq!((result.retired, queued), (posted, 0));
        result
    }
}
