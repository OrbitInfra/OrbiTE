use std::net::TcpListener;

// Reserve an explicit test port outside Linux's automatic allocation range.
// A TCP connection alone does not stop another SO_REUSEADDR listener binding
// port zero from taking the port between two explicit binds. The UDP socket
// excludes other fixture processes during that handoff.
#[cfg(target_os = "linux")]
pub(crate) fn guarded_port() -> (TcpListener, std::net::UdpSocket) {
    assert_isolated();
    use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let sysctl = std::fs::read_to_string("/proc/sys/net/ipv4/ip_local_port_range").unwrap();
    let range: Vec<u16> = sysctl
        .split_whitespace()
        .map(|s| s.parse().unwrap())
        .collect();
    let ephemeral = range[0]..=range[1];
    // Prefer the upper unprivileged range, and fall back to the lower one
    // on hosts whose ephemeral range reaches the top of the port space.
    let base = NEXT.fetch_add(1, Relaxed);
    for i in 0..(65536 - 1024) {
        let port = (1024 + (base.wrapping_add(i).wrapping_add(63000) % (65536 - 1024))) as u16;
        if ephemeral.contains(&port) {
            continue;
        }
        let addr = (std::net::Ipv4Addr::LOCALHOST, port);
        if let Ok(guard) = std::net::UdpSocket::bind(addr) {
            if let Ok(listener) = TcpListener::bind(addr) {
                return (listener, guard);
            }
        }
    }
    panic!("no free loopback fixture port outside {ephemeral:?}");
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn guarded_port() -> (TcpListener, [std::net::TcpStream; 2]) {
    assert_isolated();
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let client = std::net::TcpStream::connect_timeout(
        &listener.local_addr().unwrap(),
        crate::test_support::EVENTUAL_PROGRESS,
    )
    .unwrap();
    let (accepted, _) = listener.accept().unwrap();
    (listener, [client, accepted])
}

const CHILD: &str = "ORBITE_TEST_PORT_OWNER";

fn assert_isolated() {
    assert_eq!(
        std::env::var(CHILD).ok().as_deref(),
        std::thread::current().name(),
        "run fixed-port lifecycles through test_ports::isolated before opening sockets"
    );
}

// A UDP reservation excludes other fixtures, but cannot exclude a concurrent
// fork inheriting this process's TCP listener before CLOEXEC takes effect.
// Fixed-port lifecycles run after exec in their own process, so sibling tests
// cannot inherit their sockets. This does not serialize any tests or workers.
pub(crate) fn isolated() -> Option<std::process::Output> {
    let current = std::thread::current();
    let name = current.name().expect("named test thread");
    if std::env::var(CHILD).ok().as_deref() == Some(name) {
        return None;
    }
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", name, "--nocapture"])
        .env(CHILD, name)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{name}: {stdout}\n{stderr}");
    assert!(
        stdout.contains("test result: ok. 1 passed; 0 failed;"),
        "{stdout}"
    );
    Some(output)
}

#[test]
#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
fn a_fork_inherits_the_guarded_tcp_listener_until_the_child_exits() {
    if let Some(output) = isolated() {
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
        return;
    }
    use std::{io::Write, os::fd::AsRawFd, os::unix::net::UnixStream};
    let (listener, _guard) = guarded_port();
    let endpoint = listener.local_addr().unwrap();
    let fd = listener.as_raw_fd();
    // SAFETY: query flags on the live listener without modifying or closing it.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    assert!(flags >= 0);
    assert_ne!(flags & libc::FD_CLOEXEC, 0);
    let inode = std::fs::read_link(format!("/proc/self/fd/{fd}")).unwrap();
    let (mut release, wait) = UnixStream::pair().unwrap();
    let wait_fd = wait.as_raw_fd();
    let release_fd = release.as_raw_fd();
    let timeout_ms = crate::test_support::EVENTUAL_PROGRESS
        .as_millis()
        .try_into()
        .unwrap();
    // SAFETY: the child performs only async-signal-safe read/_exit, with stack
    // storage. No allocator, Rust destructor, driver or inherited lock is used.
    let pid = unsafe { libc::fork() };
    assert!(pid >= 0);
    if pid == 0 {
        // SAFETY: close the unused inherited endpoint, without Rust destructors.
        unsafe { libc::close(release_fd) };
        let mut ready = libc::pollfd {
            fd: wait_fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: poll one valid stack descriptor with a bounded wait.
        if unsafe { libc::poll(&mut ready, 1, timeout_ms) } != 1 {
            // SAFETY: terminate only this child, without inherited destructors.
            unsafe { libc::_exit(1) };
        }
        let mut byte = 0u8;
        // SAFETY: wait_fd is inherited and byte is valid writable stack storage.
        unsafe { libc::read(wait_fd, std::ptr::from_mut(&mut byte).cast(), 1) };
        // SAFETY: exit this child without executing any inherited destructors.
        unsafe { libc::_exit(0) };
    }
    drop(wait);
    drop(listener);
    // This is the original listener, held by a child, not a TIME_WAIT socket,
    // a wrapped reservation, or a second HP-TCP rail.
    assert_eq!(
        std::fs::read_link(format!("/proc/{pid}/fd/{fd}")).unwrap(),
        inode
    );
    let mut config = crate::EngineConfig {
        tcp: false,
        ..Default::default()
    };
    config.hp_tcp.enable = true;
    config.hp_tcp.bind_address = "127.0.0.1".into();
    config.hp_tcp.port = endpoint.port();
    let result = crate::TransferEngine::new(config.clone());
    release.write_all(&[1]).unwrap();
    let mut status = 0;
    // SAFETY: wait only for our child and provide valid writable status storage.
    assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
    assert!(libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0);
    assert!(
        matches!(&result, Err(crate::Error::Io(e)) if e.kind() == std::io::ErrorKind::AddrInUse),
        "unexpected constructor error: {:?}",
        result.as_ref().err()
    );
    eprintln!(
        "inherited listener {inode:?}, child {pid}, endpoint {endpoint}: initial bind rejected; child exit releases it"
    );
    crate::TransferEngine::new(config)
        .unwrap()
        .close(crate::test_support::EVENTUAL_PROGRESS)
        .unwrap();
}
