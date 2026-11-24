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

struct Region {
    ptr: *mut u8,
    len: usize,
}
impl Region {
    fn new(len: usize) -> io::Result<Self> {
        // SAFETY: an independent anonymous mapping, without fixed addresses.
        let addr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        Self::owned(addr, len)
    }
    fn snapshot(ptr: *mut u8, len: usize) -> io::Result<Self> {
        // SAFETY: a complete-page subrange of our own exclusive test mapping. The kernel
        // selects the backup address. The original address remains mapped for direct I/O.
        let res = unsafe {
            libc::mremap(
                ptr.cast(),
                len,
                len,
                libc::MREMAP_MAYMOVE | libc::MREMAP_DONTUNMAP,
                std::ptr::null_mut::<libc::c_void>(),
            )
        };
        Self::owned(res, len)
    }
    fn owned(addr: *mut libc::c_void, len: usize) -> io::Result<Self> {
        if addr == libc::MAP_FAILED {
            Err(io::Error::last_os_error())
        } else {
            Ok(Self {
                ptr: addr.cast(),
                len,
            })
        }
    }
}
impl Drop for Region {
    fn drop(&mut self) {
        // SAFETY: this exact mapping remains owned with no outstanding operation.
        assert_eq!(unsafe { libc::munmap(self.ptr.cast(), self.len) }, 0);
    }
}

struct Exchange {
    _maps: [Region; 2],
    windows: [*mut u8; 2],
    active: usize,
}
impl Exchange {
    fn new(size: usize, dst: *mut u8, huge: bool) -> io::Result<Self> {
        let unit = 2 << 20;
        let maps = [
            Region::new(size + 2 * unit + 8192)?,
            Region::new(size + 2 * unit + 8192)?,
        ];
        let mut windows = [std::ptr::null_mut(); 2];
        for (i, map) in maps.iter().enumerate() {
            if huge {
                // SAFETY: advise only this diagnostic's independent anonymous VMAs.
                assert_eq!(
                    unsafe { libc::madvise(map.ptr.cast(), map.len, libc::MADV_HUGEPAGE) },
                    0
                );
            }
            windows[i] = ((map.ptr as usize + 4096).next_multiple_of(unit) + (dst as usize % unit))
                as *mut u8;
        }
        Ok(Self {
            _maps: maps,
            windows,
            active: 0,
        })
    }
    fn publish(&mut self, dst: *mut u8, len: usize) -> io::Result<()> {
        let next = self.active ^ 1;
        for (src, target) in [(dst, self.windows[next]), (self.windows[self.active], dst)] {
            // SAFETY: the three nonoverlapping extents are exclusive anonymous regions in
            // this process. No I/O is outstanding. Both original addresses stay reserved.
            let v = unsafe {
                libc::mremap(
                    src.cast(),
                    len,
                    len,
                    libc::MREMAP_MAYMOVE | libc::MREMAP_FIXED | libc::MREMAP_DONTUNMAP,
                    target.cast::<libc::c_void>(),
                )
            };
            if v == libc::MAP_FAILED {
                return Err(io::Error::last_os_error());
            }
            assert_eq!(v, target.cast());
        }
        self.active = next;
        Ok(())
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

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn stream_copy(src: *const u8, dst: *mut u8, len: usize) {
    use std::arch::x86_64::*;
    assert_eq!(src as usize % 32, 0);
    assert_eq!(dst as usize % 32, 0);
    assert_eq!(len % 256, 0);
    // SAFETY: the mode selects AVX2 at runtime. Both disjoint allocations cover len aligned bytes.
    unsafe {
        for offset in (0..len).step_by(256) {
            for i in 0..8 {
                let at = offset + i * 32;
                let v = _mm256_load_si256(src.add(at).cast());
                _mm256_stream_si256(dst.add(at).cast(), v);
            }
        }
        _mm_sfence();
    }
}

fn move_pages(dst: *mut u8, src: *mut u8, len: usize) -> io::Result<()> {
    // This private diagnostic owns anonymous pages on both sides. Only whole,
    // fully completed pages enter this code. Check forward progress explicitly.
    // SAFETY: USER_MODE_ONLY restricts faults to user mode. Each ioctl's local
    // array matches the documented Linux u64 ABI. Closing unregisters the range.
    unsafe {
        let fd = libc::syscall(
            libc::SYS_userfaultfd,
            libc::O_CLOEXEC | libc::O_NONBLOCK | 1,
        ) as i32;
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let fd = OwnedFd::from_raw_fd(fd);
        let mut api = [0xaau64, 1 << 16, 0];
        if libc::ioctl(fd.as_raw_fd(), 0xc018aa3fu64, api.as_mut_ptr()) < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut reg = [dst as u64, len as u64, 1, 0];
        if libc::ioctl(fd.as_raw_fd(), 0xc020aa00u64, reg.as_mut_ptr()) < 0 {
            return Err(io::Error::last_os_error());
        }
        assert_ne!(reg[3] & (1 << 5), 0);
        if libc::madvise(dst.cast(), len, libc::MADV_DONTNEED) < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut cmd = [dst as u64, src as u64, len as u64, 1, 0];
        let rc = libc::ioctl(fd.as_raw_fd(), 0xc028aa05u64, cmd.as_mut_ptr());
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
        assert_eq!(cmd[4], len as u64);
    }
    Ok(())
}

struct Window {
    // Close the ring (including fixed registrations) before dropping the allocations.
    ring: IoUring,
    scratch: Option<Bytes>,
    exchange: Option<Exchange>,
    iovs: Vec<libc::iovec>,
    pipe: Option<(OwnedFd, OwnedFd, usize)>,
}
impl Window {
    fn new(mode: &str, slots: usize, chunk: usize, dst: *mut u8) -> io::Result<Self> {
        let ring = IoUring::new(slots as u32)?;
        let exchange = if mode.starts_with("xchg") {
            Some(Exchange::new(chunk * slots, dst, mode.contains("thp"))?)
        } else {
            None
        };
        let raw = exchange.is_some()
            || mode.starts_with("raw")
            || mode.starts_with("snap")
            || mode == "pipe";
        let huge = mode.starts_with("huge") || mode.starts_with("move");
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
            exchange,
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
                if let Some(x) = &self.exchange {
                    self.iovs[i].iov_base = x.windows[x.active].wrapping_add(i * chunk).cast();
                } else if self.scratch.is_none() {
                    self.iovs[i].iov_base = dst.wrapping_add(base + i * chunk).cast();
                }
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
            if let Some(x) = &mut self.exchange {
                x.publish(dst.wrapping_add(base), n * chunk)?;
                add(&mut copy_cost, Time::now().since(&at));
            }
            if let Some(b) = &self.scratch {
                let target = dst.wrapping_add(base);
                if mode.starts_with("move") {
                    move_pages(target, b.ptr(0), n * chunk)?;
                } else if mode.ends_with("-nt") {
                    assert!(std::is_x86_feature_detected!("avx2"));
                    // SAFETY: these complete, aligned, disjoint ranges passed the full CQE check.
                    unsafe { stream_copy(b.ptr(0), target, n * chunk) };
                } else {
                    // SAFETY: full CQEs initialized the contiguous slots. The guarded destination is disjoint.
                    unsafe { std::ptr::copy_nonoverlapping(b.ptr(0), target, n * chunk) };
                }
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
    let huge = 2 << 20;
    let dest = Region::new(total + 2 * huge + 8192)?;
    let boundary = (dest.ptr as usize + 4096).next_multiple_of(huge);
    let target = (boundary + if mode.ends_with("aligned") { 0 } else { 4096 }) as *mut u8;
    let guard = target as usize - dest.ptr as usize;
    if mode.contains("thp") {
        // SAFETY: advise the whole exclusively owned mapping, including both guards.
        assert_eq!(
            unsafe { libc::madvise(dest.ptr.cast(), dest.len, libc::MADV_HUGEPAGE) },
            0
        );
    }
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
        for (tile, region) in image.chunks_exact_mut(4096).enumerate() {
            region.fill(v.wrapping_add((tile % 251) as u8));
            region[..8].copy_from_slice(&(tile as u64).to_ne_bytes());
        }
        // Exercise the same warmed reset and fdatasync boundary as the public benchmark.
        file.write_all_at(&image, 4096)?;
        file.sync_data()?;
        // SAFETY: the full allocation is exclusively available before submission.
        unsafe { dest.ptr.write_bytes(0xa5, dest.len) };
        let before = Time::now();
        let backup = if mode.starts_with("snap") {
            Some(Region::snapshot(target, total)?)
        } else {
            None
        };
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
        drop(backup);
        let finished = Time::now();
        // SAFETY: every byte in dest was initialized and all direct I/O has completed.
        let bytes = unsafe { std::slice::from_raw_parts(dest.ptr, dest.len) };
        assert!(bytes[..guard].iter().all(|&x| x == 0xa5));
        assert_eq!(&bytes[guard..guard + total], image.as_slice());
        assert!(bytes[guard + total..].iter().all(|&x| x == 0xa5));
        if i >= 3 {
            rows.push(json!({"setup":prepared.since(&before),"read":io_cost,"copy":copy_cost,"retire":finished.since(&copied),"total":finished.since(&before)}));
        }
    }
    let stat = std::fs::read_to_string("/proc/self/status")?;
    let rss = stat.lines().find(|s| s.starts_with("VmHWM:")).unwrap();
    let real_bytes = read_bytes()? - initial_bytes;
    if mode.starts_with("xchg") {
        let size = chunk * slots;
        let mut exchange = Exchange::new(size, target, mode.contains("thp"))?;
        for cut in [
            Some(0),
            Some(512),
            Some(4095),
            Some(size - 512),
            Some(size),
            None,
        ] {
            file.set_len((4096 + size) as u64)?;
            file.write_all_at(&image[..size], 4096)?;
            // SAFETY: verified private destination and guards, with no pending SQEs.
            unsafe { dest.ptr.write_bytes(0xa5, dest.len) };
            if let Some(n) = cut {
                file.set_len((4096 + n) as u64)?;
            }
            let mut ring = IoUring::new(4)?;
            let private = exchange.windows[exchange.active];
            let iov = libc::iovec {
                iov_base: private.cast(),
                iov_len: size,
            };
            let sq = opcode::Readv::new(
                types::Fd(if cut.is_some() {
                    direct.as_raw_fd()
                } else {
                    -1
                }),
                &iov,
                1,
            )
            .offset(4096)
            .build();
            // SAFETY: keep the private slot, iovec and file alive through the real CQE.
            unsafe { ring.submission().push(&sq).unwrap() };
            assert_eq!(ring.submit_and_wait(1)?, 1);
            let result = ring.completion().next().unwrap().result();
            assert_eq!(result, cut.map_or(-libc::EBADF, |n| n as i32));
            let valid = result.max(0) as usize;
            if valid == size {
                exchange.publish(target, size)?;
            } else if valid > 0 {
                // SAFETY: only covered CQE bytes go to the private, exclusively held destination.
                unsafe { std::ptr::copy_nonoverlapping(private, target, valid) };
            }
            // SAFETY: both mappings are initialized and all I/O has finished.
            let bytes = unsafe { std::slice::from_raw_parts(dest.ptr, dest.len) };
            assert_eq!(&bytes[guard..guard + valid], &image[..valid]);
            assert!(
                bytes[..guard]
                    .iter()
                    .chain(bytes[guard + valid..].iter())
                    .all(|b| *b == 0xa5)
            );
        }
    }
    if mode.starts_with("snap") {
        for cut in [0, 512, 4095, 4096, 4327, total - 512] {
            file.set_len((4096 + total) as u64)?;
            file.write_all_at(&image, 4096)?;
            // SAFETY: restore the guarded initialized bytes before each new admission.
            unsafe { dest.ptr.write_bytes(0xa5, dest.len) };
            let old = Region::snapshot(target, total)?;
            file.set_len((4096 + cut) as u64)?;
            let mut ring = IoUring::new(4)?;
            let iov = libc::iovec {
                iov_base: target.cast(),
                iov_len: total,
            };
            let sq = opcode::Readv::new(types::Fd(direct.as_raw_fd()), &iov, 1)
                .offset(4096)
                .build();
            // SAFETY: this exclusive fixture owns the iovec, fd and two mappings through the CQE.
            unsafe { ring.submission().push(&sq).unwrap() };
            assert_eq!(ring.submit_and_wait(1)?, 1);
            assert_eq!(ring.completion().next().unwrap().result(), cut as i32);
            // SAFETY: valid CQE, disjoint mappings. Only its uncompleted suffix is restored.
            unsafe {
                std::ptr::copy_nonoverlapping(old.ptr.add(cut), target.add(cut), total - cut)
            };
            // SAFETY: mapping is initialized and the kernel released the request.
            let bytes = unsafe { std::slice::from_raw_parts(dest.ptr, dest.len) };
            assert_eq!(&bytes[guard..guard + cut], &image[..cut]);
            assert!(
                bytes[..guard]
                    .iter()
                    .chain(bytes[guard + cut..].iter())
                    .all(|x| *x == 0xa5)
            );
        }
    }
    println!(
        "{}",
        json!({"mode":mode,"size":total,"chunk":chunk,"slots":slots,"trials":rows,"hwm":rss,"read_bytes":real_bytes,"verified":true})
    );
    drop(direct);
    drop(file);
    std::fs::remove_file(path)?;
    Ok(())
}
