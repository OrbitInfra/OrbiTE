use super::*;
use std::fs::File;

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
    ) -> Receipt {
        let slots = self.iovs.len();
        let chunks = total / chunk;
        let mut result = Receipt::default();
        let mut posted = 0;
        let mut next = 0;
        let mut stopped = false;
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
                    let remaining = posted - next - 1;
                    let known = cqes.iter().filter(|s| s.is_some()).count();
                    let before_cq = self.ring.completion().len();
                    result.overlap_opportunities += usize::from(remaining > known + before_cq);
                    let t = Time::now();
                    let src = self.iovs[slot].iov_base.cast::<u8>();
                    let target = dst.wrapping_add(next * chunk);
                    // SAFETY: this slot returned its terminal CQE. The caller owns the
                    // whole output and we only publish the contiguous confirmed prefix.
                    unsafe {
                        if mode.ends_with("-nt") && received == chunk {
                            stream_copy(src, target, received);
                        } else {
                            std::ptr::copy_nonoverlapping(src, target, received)
                        }
                    }
                    add(&mut result.copies, Time::now().since(&t));
                    result.arrivals_during_copy += self.ring.completion().len() - before_cq;
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
                submit(&mut self.ring, 1);
            }
        }
        assert_eq!(result.retired, posted);
        result
    }
}
