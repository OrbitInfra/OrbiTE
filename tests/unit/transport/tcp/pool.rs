use super::*;
use crate::{SourceEpoch, test_support::EVENTUAL_PROGRESS as WAIT};
use std::{
    io::Read,
    net::TcpListener,
    sync::atomic::{AtomicUsize, Ordering},
};

fn key(listener: &TcpListener) -> PeerKey {
    PeerKey {
        endpoint: listener.local_addr().unwrap(),
        epoch: SourceEpoch::generate().unwrap(),
    }
}
fn eof(stream: &mut TcpStream) {
    stream.set_read_timeout(Some(WAIT)).unwrap();
    assert_eq!(stream.read(&mut [0]).unwrap(), 0);
}

#[test]
fn clean_lanes_reuse_only_their_epoch_and_cannot_evict_a_borrower() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let pool = Pool::new(2);
    let a = key(&listener);
    let b = key(&listener);
    let mut first = pool.checkout(a, Instant::now() + WAIT).unwrap();
    let (mut remote_a, _) = listener.accept().unwrap();
    let address_a = first.channel().unwrap().stream.local_addr().unwrap();
    first.finish(true).unwrap();
    let mut reused = pool.checkout(a, Instant::now() + WAIT).unwrap();
    assert_eq!(
        reused.channel().unwrap().stream.local_addr().unwrap(),
        address_a
    );
    let mut second = pool.checkout(b, Instant::now() + WAIT).unwrap();
    let (mut remote_b, _) = listener.accept().unwrap();
    assert_ne!(
        second.channel().unwrap().stream.local_addr().unwrap(),
        address_a
    );
    assert!(matches!(
        pool.checkout(key(&listener), Instant::now() + WAIT),
        Err(Error::ResourceExhausted)
    ));
    assert!(matches!(pool.close_peer(a), Err(Error::Unquiesced { .. })));
    assert_eq!(pool.lanes.lock().unwrap().len(), 2);
    reused.finish(true).unwrap();
    pool.close_peer(a).unwrap();
    eof(&mut remote_a);
    // Retiring another epoch did not shut down the borrowed socket.
    second.channel().unwrap().write_all(&[0x6d]).unwrap();
    let mut byte = [0];
    remote_b.read_exact(&mut byte).unwrap();
    assert_eq!(byte, [0x6d]);
    second.finish(true).unwrap();
    pool.close_peer(b).unwrap();
    eof(&mut remote_b);
    assert!(pool.lanes.lock().unwrap().is_empty());
}

#[test]
fn failed_or_abandoned_leases_close_instead_of_reentering_the_cache() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let pool = Pool::new(1);
    let peer = key(&listener);
    for abandoned in [false, true] {
        let mut lease = pool.checkout(peer, Instant::now() + WAIT).unwrap();
        let (mut remote, _) = listener.accept().unwrap();
        // The real channel observes a deadline only after admission. It is not
        // a success receipt, even though this request has posted no payload.
        lease.channel().unwrap().deadline = Instant::now();
        assert!(matches!(
            lease.channel().unwrap().blocking_read(&mut [0]),
            Err(Error::Timeout)
        ));
        if abandoned {
            drop(lease);
        } else {
            lease.finish(false).unwrap();
        }
        eof(&mut remote);
        let lanes = pool.lanes.lock().unwrap();
        let state = lanes[&peer].state.lock().unwrap();
        assert!(!state.busy && state.channel.is_none());
    }
}

#[test]
fn idle_lru_reclaims_only_completed_streams_under_churn() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let pool = Pool::new(1);
    let a = key(&listener);
    let first = pool.checkout(a, Instant::now() + WAIT).unwrap();
    let (mut remote, _) = listener.accept().unwrap();
    first.finish(true).unwrap();
    for _ in 0..10 {
        let p = key(&listener);
        let next = pool.checkout(p, Instant::now() + WAIT).unwrap();
        eof(&mut remote);
        remote = listener.accept().unwrap().0;
        next.finish(true).unwrap();
        assert_eq!(pool.lanes.lock().unwrap().len(), 1);
    }
    pool.close().unwrap();
    eof(&mut remote);
    assert!(pool.lanes.lock().unwrap().is_empty());
}

#[test]
fn completed_peer_eof_reconnects_before_any_new_frame() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let pool = Pool::new(1);
    let peer = key(&listener);
    let first = pool.checkout(peer, Instant::now() + WAIT).unwrap();
    let (mut remote, _) = listener.accept().unwrap();
    remote.set_read_timeout(Some(WAIT)).unwrap();
    remote.shutdown(Shutdown::Write).unwrap();
    // Observe the FIN before returning the completed lease: no scheduling
    // delay is used to decide whether a half-closed idle socket is reusable.
    first
        .channel
        .as_ref()
        .unwrap()
        .stream
        .set_read_timeout(Some(WAIT))
        .unwrap();
    assert_eq!(
        first
            .channel
            .as_ref()
            .unwrap()
            .stream
            .peek(&mut [0])
            .unwrap(),
        0
    );
    first.finish(true).unwrap();
    let next = pool.checkout(peer, Instant::now() + WAIT).unwrap();
    let (mut replacement, _) = listener.accept().unwrap();
    assert_eq!(remote.read(&mut [0]).unwrap(), 0);
    next.finish(true).unwrap();
    pool.close_peer(peer).unwrap();
    eof(&mut replacement);
}

#[test]
fn failed_shutdown_stays_charged_and_is_never_retried() {
    static CALLS: AtomicUsize = AtomicUsize::new(0);
    fn fail(_: &TcpStream) -> io::Result<()> {
        CALLS.fetch_add(1, Ordering::SeqCst);
        Err(io::Error::from_raw_os_error(libc::EIO))
    }
    CALLS.store(0, Ordering::SeqCst);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut pool = Pool::new(1);
    pool.shutdown = fail;
    let peer = key(&listener);
    let lease = pool.checkout(peer, Instant::now() + WAIT).unwrap();
    let (remote, _) = listener.accept().unwrap();
    assert!(matches!(lease.finish(false), Err(Error::Unquiesced { .. })));
    assert!(matches!(
        pool.checkout(peer, Instant::now() + WAIT),
        Err(Error::Unquiesced { .. })
    ));
    assert!(matches!(
        pool.checkout(key(&listener), Instant::now() + WAIT),
        Err(Error::ResourceExhausted)
    ));
    assert!(matches!(
        pool.close_peer(peer),
        Err(Error::Unquiesced { .. })
    ));
    assert!(matches!(pool.close(), Err(Error::Unquiesced { .. })));
    assert_eq!(pool.lanes.lock().unwrap().len(), 1);
    drop(pool);
    assert_eq!(CALLS.load(Ordering::SeqCst), 1);
    remote.set_nonblocking(true).unwrap();
    assert!(matches!(remote.peek(&mut [0]), Err(e) if e.kind() == io::ErrorKind::WouldBlock));
}
