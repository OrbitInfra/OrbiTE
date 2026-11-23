//! Private CI diagnostic measuring the guest's direct-read mechanisms.
use io_uring::{IoUring, opcode, types};
use serde_json::{Value, json};
use std::alloc::{Layout, alloc, dealloc};
use std::fs::OpenOptions;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::{FileExt, OpenOptionsExt};
use std::ptr::NonNull;
use std::time::Instant;

struct Bytes {
    data: NonNull<u8>,
    layout: Layout,
}
impl Bytes {
    fn new(size: usize, alignment: usize) -> Self {
        let layout = Layout::from_size_align(size, alignment).unwrap();
        // SAFETY: allocations are nonzero and are owned until the matching deallocation.
        let data = NonNull::new(unsafe { alloc(layout) }).expect("aligned allocation");
        Self { data, layout }
    }
    fn ptr(&self, offset: usize) -> *mut u8 {
        assert!(offset < self.layout.size());
        self.data.as_ptr().wrapping_add(offset)
    }
}
impl Drop for Bytes {
    fn drop(&mut self) {
        // SAFETY: all native requests finished before dropping their allocations.
        unsafe { dealloc(self.data.as_ptr(), self.layout) };
    }
}

struct Time(Instant, u64, i64);
impl Time {
    fn now() -> Self {
        let mut cpu = std::mem::MaybeUninit::uninit();
        let mut usage = std::mem::MaybeUninit::uninit();
        // SAFETY: both Linux calls initialize the supplied output on success.
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
fn add(to: &mut [u64; 3], v: [u64; 3]) {
    for (a, b) in to.iter_mut().zip(v) {
        *a += b;
    }
}

struct Window {
    // Close the ring (including fixed registrations) before dropping the allocations.
    ring: IoUring,
    scratch: Option<Bytes>,
    iovs: Vec<libc::iovec>,
    pipe: Option<(OwnedFd, OwnedFd, usize)>,
}
impl Window {
    fn new(mode: &str, slots: usize, chunk: usize, dst: *mut u8) -> io::Result<Self> {
        let ring = IoUring::new(slots as u32)?;
        let raw = mode == "raw" || mode == "pipe";
        let huge = mode == "huge";
        let scratch = (!raw).then(|| Bytes::new(chunk * slots, if huge { 2 << 20 } else { 4096 }));
        if huge {
            let b = scratch.as_ref().unwrap();
            // SAFETY: the pointer and length cover complete aligned pages in this allocation.
            if unsafe { libc::madvise(b.ptr(0).cast(), chunk * slots, libc::MADV_HUGEPAGE) } < 0 {
                return Err(io::Error::last_os_error());
            }
        }
        let iovs: Vec<_> = (0..slots)
            .map(|i| libc::iovec {
                iov_base: scratch.as_ref().map_or(dst, |b| b.ptr(i * chunk)).cast(),
                iov_len: chunk,
            })
            .collect();
        if mode.starts_with("fixed") {
            // SAFETY: every slot outlives the ring's registered buffer table.
            unsafe {
                ring.submitter().register_buffers(&iovs)?;
            }
        }
        let pipe = if mode == "pipe" {
            let mut pair = [-1; 2];
            // SAFETY: pipe2 initializes two distinct fresh descriptors on success.
            if unsafe { libc::pipe2(pair.as_mut_ptr(), libc::O_CLOEXEC) } < 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: ownership of both fresh fds transfers to these wrappers.
            let (r, w) = unsafe { (OwnedFd::from_raw_fd(pair[0]), OwnedFd::from_raw_fd(pair[1])) };
            // SAFETY: capacity commands operate on this live pipe descriptor.
            let capacity = unsafe {
                libc::fcntl(w.as_raw_fd(), libc::F_SETPIPE_SZ, chunk as i32);
                libc::fcntl(w.as_raw_fd(), libc::F_GETPIPE_SZ)
            };
            assert!(capacity >= 4096);
            Some((r, w, capacity as usize))
        } else {
            None
        };
        Ok(Self {
            ring,
            scratch,
            iovs,
            pipe,
        })
    }
    fn read(
        &mut self,
        fd: i32,
        mode: &str,
        total: usize,
        chunk: usize,
        dst: *mut u8,
    ) -> io::Result<([u64; 3], [u64; 3])> {
        let slots = self.iovs.len();
        let mut io_cost = [0; 3];
        let mut copy_cost = [0; 3];
        if let Some((rd, wr, capacity)) = &self.pipe {
            let mut base = 0;
            while base < total {
                let count = (total - base).min(*capacity);
                let mut offset = (4096 + base) as libc::loff_t;
                let start = Time::now();
                // SAFETY: the fd pair and mutable offset remain live for this synchronous transfer.
                let n = unsafe {
                    libc::splice(
                        fd,
                        &mut offset,
                        wr.as_raw_fd(),
                        std::ptr::null_mut(),
                        count,
                        0,
                    )
                };
                if n < 0 {
                    return Err(io::Error::last_os_error());
                }
                assert!(n > 0);
                let at = Time::now();
                add(&mut io_cost, at.since(&start));
                let mut copied = 0;
                while copied < n as usize {
                    // SAFETY: only the received byte count is read into the bounded destination.
                    let read = unsafe {
                        libc::read(
                            rd.as_raw_fd(),
                            dst.add(base + copied).cast(),
                            n as usize - copied,
                        )
                    };
                    if read < 0 {
                        return Err(io::Error::last_os_error());
                    }
                    assert!(read > 0);
                    copied += read as usize;
                }
                base += copied;
                add(&mut copy_cost, Time::now().since(&at));
            }
            return Ok((io_cost, copy_cost));
        }
        for base in (0..total).step_by(slots * chunk) {
            let n = ((total - base) / chunk).min(slots);
            let prep = Time::now();
            for i in 0..n {
                let offset = (4096 + base + i * chunk) as u64;
                let sqe = if mode.starts_with("fixed") {
                    opcode::ReadFixed::new(
                        types::Fd(fd),
                        self.iovs[i].iov_base.cast(),
                        chunk as u32,
                        i as u16,
                    )
                    .offset(offset)
                    .build()
                } else {
                    opcode::Readv::new(types::Fd(fd), &self.iovs[i], 1)
                        .offset(offset)
                        .build()
                }
                .user_data(i as u64);
                // SAFETY: this group retains every fd, iovec and destination through its full CQ set.
                unsafe {
                    self.ring
                        .submission()
                        .push(&sqe)
                        .expect("reserved queue capacity");
                }
            }
            let submitted = self.ring.submit_and_wait(n)?;
            assert_eq!(submitted, n);
            let mut seen = 0u64;
            for c in self.ring.completion() {
                assert!(c.user_data() < n as u64);
                assert_eq!(seen & (1 << c.user_data()), 0);
                seen |= 1 << c.user_data();
                assert_eq!(
                    c.result(),
                    chunk as i32,
                    "short device data on initialized file"
                );
            }
            assert_eq!(seen, (1 << n) - 1);
            let at = Time::now();
            add(&mut io_cost, at.since(&prep));
            if let Some(b) = &self.scratch {
                // SAFETY: full CQEs initialized the contiguous slots. The guarded destination is disjoint.
                unsafe { std::ptr::copy_nonoverlapping(b.ptr(0), dst.add(base), n * chunk) };
                add(&mut copy_cost, Time::now().since(&at));
            }
        }
        Ok((io_cost, copy_cost))
    }
}
fn main() -> io::Result<()> {
    let a: Vec<String> = std::env::args().collect();
    let (mode, path) = (&*a[1], &a[2]);
    let total = a[3].parse::<usize>().unwrap();
    let chunk = a[4].parse::<usize>().unwrap().min(total);
    let slots = a[5].parse::<usize>().unwrap().min(total / chunk);
    let count = a[6].parse::<usize>().unwrap();
    assert!(total % chunk == 0 && chunk % 4096 == 0 && slots > 0 && slots < 64);
    let file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open(path)?;
    // SAFETY: the valid fd and nonnegative extent describe the private test file.
    assert_eq!(
        unsafe { libc::posix_fallocate(file.as_raw_fd(), 0, (total + 8192) as _) },
        0
    );
    let direct = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECT)
        .open(path)?;
    // SAFETY: F_GETFL reads status for the live descriptor and takes no variadic data.
    let flags = unsafe { libc::fcntl(direct.as_raw_fd(), libc::F_GETFL) };
    assert!(flags >= 0 && flags & libc::O_DIRECT != 0);
    let dest = Bytes::new(total + 8192, 4096);
    let target = dest.ptr(4096);
    let mut image = vec![0xa5u8; total];
    let mut rows: Vec<Value> = Vec::new();
    let mut reusable = None;
    let read_bytes = || -> io::Result<u64> {
        let io = std::fs::read_to_string("/proc/self/io")?;
        Ok(io
            .lines()
            .find_map(|l| l.strip_prefix("read_bytes: "))
            .unwrap()
            .parse()
            .unwrap())
    };
    let initial_bytes = read_bytes()?;
    if mode.contains("keep") {
        reusable = Some(Window::new(mode, slots, chunk, target)?)
    }
    for i in 0..3 + count {
        let v = (i % 251 + 1) as u8;
        image.fill(v);
        // Exercise the same warmed reset and fdatasync boundary as the public benchmark.
        file.write_all_at(&image, 4096)?;
        file.sync_data()?;
        // SAFETY: the full allocation is exclusively available before submission.
        unsafe { dest.data.as_ptr().write_bytes(0xa5, total + 8192) };
        let before = Time::now();
        let mut window = match reusable.take() {
            Some(x) => x,
            None => Window::new(mode, slots, chunk, target)?,
        };
        let prepared = Time::now();
        let (io_cost, copy_cost) = window.read(direct.as_raw_fd(), mode, total, chunk, target)?;
        let copied = Time::now();
        if mode.contains("keep") {
            reusable = Some(window)
        } else {
            drop(window)
        }
        let finished = Time::now();
        // SAFETY: every byte in dest was initialized and all direct I/O has completed.
        let bytes = unsafe { std::slice::from_raw_parts(dest.ptr(0), total + 8192) };
        assert!(bytes[..4096].iter().all(|&x| x == 0xa5));
        assert_eq!(&bytes[4096..4096 + total], image.as_slice());
        assert!(bytes[4096 + total..].iter().all(|&x| x == 0xa5));
        if i >= 3 {
            rows.push(json!({"setup":prepared.since(&before),"read":io_cost,"copy":copy_cost,"retire":finished.since(&copied),"total":finished.since(&before)}));
        }
    }
    let stat = std::fs::read_to_string("/proc/self/status")?;
    let rss = stat.lines().find(|s| s.starts_with("VmHWM:")).unwrap();
    let real_bytes = read_bytes()? - initial_bytes;
    println!(
        "{}",
        json!({"mode":mode,"size":total,"chunk":chunk,"slots":slots,"trials":rows,"hwm":rss,"read_bytes":real_bytes,"verified":true})
    );
    drop(direct);
    drop(file);
    std::fs::remove_file(path)?;
    Ok(())
}
