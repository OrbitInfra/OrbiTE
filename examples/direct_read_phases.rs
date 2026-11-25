//! Linux-only mechanism probe for a single 64 MiB ext4 direct READ.
use io_uring::{IoUring, opcode, types};
use serde_json::json;
use std::alloc::{Layout, alloc, dealloc};
use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileExt, OpenOptionsExt};
use std::ptr::NonNull;
use std::time::Instant;

const SIZE: usize = 64 << 20;
const PAGE: usize = 4096;
const HUGE: usize = 2 << 20;
const WARMUP: usize = 3;

struct Buffer {
    ptr: NonNull<u8>,
    layout: Layout,
}
impl Buffer {
    fn new(len: usize, alignment: usize) -> Self {
        let layout = Layout::from_size_align(len, alignment).unwrap();
        // SAFETY: nonempty valid layout, matched by Drop.
        let ptr = NonNull::new(unsafe { alloc(layout) }).expect("aligned private bytes");
        Self { ptr, layout }
    }
    fn advise(&self) -> io::Result<()> {
        // SAFETY: scratch is exclusively owned, page-aligned, with a whole number of pages.
        if unsafe { libc::madvise(self.ptr.as_ptr().cast(), SIZE, libc::MADV_HUGEPAGE) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
    fn touch(&self) {
        for off in (0..SIZE).step_by(PAGE) {
            // SAFETY: every byte written belongs to this exclusively owned live allocation.
            unsafe { self.ptr.as_ptr().add(off).write_volatile(0x3b) };
        }
    }
}
impl Drop for Buffer {
    fn drop(&mut self) {
        // SAFETY: all submitted work drained before the matching release.
        unsafe { dealloc(self.ptr.as_ptr(), self.layout) }
    }
}

struct Stamp(Instant, u64, i64);
impl Stamp {
    fn now() -> Self {
        let mut cpu = std::mem::MaybeUninit::uninit();
        let mut usage = std::mem::MaybeUninit::uninit();
        // SAFETY: the OS initializes both out structures on success.
        unsafe {
            assert_eq!(
                libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, cpu.as_mut_ptr()),
                0
            );
            assert_eq!(libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()), 0);
            let ts = cpu.assume_init();
            Self(
                Instant::now(),
                ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64,
                usage.assume_init().ru_minflt as i64,
            )
        }
    }
    fn since(&self, start: &Self) -> [u64; 3] {
        [
            self.0.duration_since(start.0).as_nanos() as u64,
            self.1 - start.1,
            (self.2 - start.2) as u64,
        ]
    }
}

fn resident(address: *mut u8) -> usize {
    let mut pages = vec![0u8; SIZE / PAGE];
    // SAFETY: this page-aligned range and the residency vector stay live for the query.
    assert_eq!(
        unsafe { libc::mincore(address.cast(), SIZE, pages.as_mut_ptr()) },
        0
    );
    pages.iter().filter(|byte| **byte & 1 != 0).count()
}

fn payload_read(ring: &mut IoUring, file: &File, address: *mut u8) -> io::Result<()> {
    let iov = libc::iovec {
        iov_base: address.cast(),
        iov_len: SIZE,
    };
    let entry = opcode::Readv::new(types::Fd(file.as_raw_fd()), &iov, 1)
        .offset(PAGE as u64)
        .build()
        .user_data(91);
    // SAFETY: stand-alone single issuer, with fd, data and iovec live through its CQE.
    unsafe { ring.submission().push(&entry).expect("one slot reserved") };
    assert_eq!(ring.submit_and_wait(1)?, 1);
    let cqe = ring.completion().next().expect("full read receipt");
    assert_eq!((cqe.user_data(), cqe.result()), (91, SIZE as i32));
    Ok(())
}

fn disk_bytes() -> u64 {
    std::fs::read_to_string("/proc/self/io")
        .unwrap()
        .lines()
        .find_map(|l| l.strip_prefix("read_bytes: "))
        .unwrap()
        .parse()
        .unwrap()
}

fn main() -> io::Result<()> {
    let args: Vec<_> = std::env::args().collect();
    let (mode, path, count) = (
        args[1].as_str(),
        &args[2],
        args[3].parse::<usize>().unwrap(),
    );
    assert!(matches!(
        mode,
        "raw" | "cold" | "cold-touch" | "hot" | "hot-touch" | "base" | "base-touch"
    ));
    let raw = mode == "raw";
    let hot = mode.starts_with("hot");
    let base = mode.starts_with("base");
    let explicit = mode.ends_with("-touch");
    let alignment = if base { PAGE } else { HUGE };
    // SAFETY: sysconf reads a kernel property without pointer arguments.
    assert_eq!(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }, PAGE as i64);
    let file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .read(true)
        .open(path)?;
    // SAFETY: fresh private file, nonnegative extent, no pointer.
    assert_eq!(
        unsafe { libc::posix_fallocate(file.as_raw_fd(), 0, (SIZE + 2 * PAGE) as _) },
        0
    );
    let direct = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECT)
        .open(path)?;
    // SAFETY: read-only descriptor flag query.
    assert!(unsafe { libc::fcntl(direct.as_raw_fd(), libc::F_GETFL) } & libc::O_DIRECT != 0);
    let mut image = vec![0u8; SIZE];
    let dest = Buffer::new(SIZE + 2 * PAGE, PAGE);
    let target = dest.ptr.as_ptr().wrapping_add(PAGE);
    let mut retained = if hot {
        let b = Buffer::new(SIZE, alignment);
        b.advise()?;
        b.touch();
        Some(b)
    } else {
        None
    };
    let mut trials = Vec::with_capacity(count);
    let initial = disk_bytes();
    let mut residency = None;
    let begin_stamps = Stamp::now();
    for _ in 0..4096 {
        std::hint::black_box(Stamp::now());
    }
    let clock_pair = Stamp::now().since(&begin_stamps);
    for i in 0..count + WARMUP {
        for (n, page) in image.chunks_exact_mut(PAGE).enumerate() {
            page.fill(((n + 11 * i) % 251) as u8);
            page[..8].copy_from_slice(&((n + i * SIZE / PAGE) as u64).to_le_bytes());
        }
        file.write_all_at(&image, PAGE as u64)?;
        file.sync_data()?;
        // SAFETY: destination allocation is exclusive and no SQE is outstanding.
        unsafe { dest.ptr.as_ptr().write_bytes(0xa5, dest.layout.size()) };
        let t0 = Stamp::now();
        let b = if raw {
            None
        } else if hot {
            retained.take()
        } else {
            Some(Buffer::new(SIZE, alignment))
        };
        let t1 = Stamp::now();
        if !raw && !hot && !base {
            b.as_ref().unwrap().advise()?;
        }
        let addr = b.as_ref().map_or(target, |b| b.ptr.as_ptr());
        let t2 = Stamp::now();
        // Only the first throwaway warmup probes residency. Measured rows do no mincore calls.
        let before_res = (i == 0).then(|| resident(addr));
        if explicit {
            b.as_ref().unwrap().touch();
        }
        let t3 = Stamp::now();
        let after_touch = (i == 0).then(|| resident(addr));
        let mut ring = IoUring::new(1)?;
        let t4 = Stamp::now();
        payload_read(&mut ring, &direct, addr)?;
        let t5 = Stamp::now();
        if !raw {
            // SAFETY: full CQE proves the scratch bytes initialized. Both ranges are disjoint.
            unsafe { std::ptr::copy_nonoverlapping(addr, target, SIZE) };
        }
        let t6 = Stamp::now();
        drop(ring);
        let t7 = Stamp::now();
        if i == 0 {
            residency = Some([before_res.unwrap(), after_touch.unwrap(), resident(addr)]);
        }
        if hot {
            retained = b
        } else {
            drop(b);
        }
        let t8 = Stamp::now();
        // SAFETY: complete destination initialized, exclusive, all kernel work completed.
        let output = unsafe { std::slice::from_raw_parts(dest.ptr.as_ptr(), dest.layout.size()) };
        assert!(
            output[..PAGE]
                .iter()
                .chain(&output[PAGE + SIZE..])
                .all(|&b| b == 0xa5)
        );
        assert_eq!(&output[PAGE..SIZE + PAGE], &*image);
        if i >= WARMUP {
            trials.push(json!({"alloc":t1.since(&t0),"advice":t2.since(&t1),"touch":t3.since(&t2),
                "ring_setup":t4.since(&t3),"read":t5.since(&t4),"copy":t6.since(&t5),"ring_drop":t7.since(&t6),
                "free":t8.since(&t7),"total":t8.since(&t0)}));
        }
    }
    let read = disk_bytes() - initial;
    assert!(read >= (count + WARMUP) as u64 * SIZE as u64);
    let stat = std::fs::read_to_string("/proc/self/status")?;
    println!(
        "{}",
        json!({"mode":mode,"verified":true,"bytes":SIZE,"samples":trials,"read_bytes":read,
       "resident_pages":[residency.unwrap(),SIZE/PAGE],"stamps4096":clock_pair,
       "rss":stat.lines().find(|s| s.starts_with("VmHWM:"))})
    );
    Ok(())
}
