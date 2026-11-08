//! Each existing control worker owns its idle TCP sockets and readiness loop.
//! Idle sockets hold no memory lease or source admission. Active work keeps the
//! existing handler/queue bounds; cached sockets have a separate shared charge.
use super::*;
use std::collections::HashMap;
use std::sync::atomic::AtomicUsize;

pub(super) struct Slots {
    used: AtomicUsize,
    limit: usize,
}
struct Slot(Arc<Slots>);
impl Drop for Slot {
    fn drop(&mut self) {
        self.0.used.fetch_sub(1, Ordering::AcqRel);
    }
}
impl Slots {
    pub(super) fn new(limit: usize) -> Arc<Self> {
        Arc::new(Self {
            used: AtomicUsize::new(0),
            limit,
        })
    }
    fn reserve(self: &Arc<Self>) -> Option<Slot> {
        self.used
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < self.limit).then_some(n + 1)
            })
            .ok()
            .map(|_| Slot(self.clone()))
    }
}

struct Idle {
    stream: mio::net::TcpStream,
    _slot: Slot,
}

pub(super) struct Worker {
    poll: mio::Poll,
    pub(super) wake: Arc<mio::Waker>,
    idle: HashMap<mio::Token, Idle>,
    slots: Arc<Slots>,
    next: usize,
}
impl Worker {
    pub(super) fn new(slots: Arc<Slots>) -> Result<Self> {
        let poll = mio::Poll::new()?;
        let wake = Arc::new(mio::Waker::new(poll.registry(), mio::Token(0))?);
        Ok(Self {
            poll,
            wake,
            idle: HashMap::new(),
            slots,
            next: 0,
        })
    }

    fn cache(&mut self, stream: TcpStream) -> Result<()> {
        let Some(slot) = self.slots.reserve() else {
            // No memory owner remains here. Closing this stream cannot release
            // a caller's admitted request; its next frame gets a socket error.
            return Ok(());
        };
        self.idle
            .try_reserve(1)
            .map_err(|_| Error::ResourceExhausted)?;
        self.next = self.next.checked_add(1).ok_or(Error::ResourceExhausted)?;
        let token = mio::Token(self.next);
        stream.set_nonblocking(true)?;
        let mut stream = mio::net::TcpStream::from_std(stream);
        self.poll
            .registry()
            .register(&mut stream, token, mio::Interest::READABLE)?;
        self.idle.insert(
            token,
            Idle {
                stream,
                _slot: slot,
            },
        );
        Ok(())
    }

    pub(super) fn run(
        mut self,
        receiver: &Mutex<mpsc::Receiver<(TcpStream, Instant)>>,
        stopping: &AtomicBool,
        timeout: Duration,
        mut serve: impl FnMut(TcpStream, Instant) -> Option<TcpStream>,
    ) -> Result<()> {
        let mut events = mio::Events::with_capacity(64);
        while !stopping.load(Ordering::Acquire) {
            let queued = receiver
                .lock()
                .map_err(|_| Error::Internal("control handoff lock"))?
                .try_recv();
            let active = queued.is_ok();
            match queued {
                Ok((stream, until)) => {
                    if let Some(stream) = serve(stream, until) {
                        self.cache(stream)?;
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => return Ok(()),
                Err(mpsc::TryRecvError::Empty) => {}
            }
            match self.poll.poll(
                &mut events,
                Some(if active { Duration::ZERO } else { IO_POLL }),
            ) {
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                result => result?,
            }
            for event in events.iter() {
                if stopping.load(Ordering::Acquire) {
                    break;
                }
                let Some(mut idle) = self.idle.remove(&event.token()) else {
                    continue;
                };
                self.poll.registry().deregister(&mut idle.stream)?;
                let stream: TcpStream = idle.stream.into();
                drop(idle._slot);
                match stream.peek(&mut [0]) {
                    Ok(0) => continue,
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        self.cache(stream)?;
                        continue;
                    }
                    Err(_) => continue,
                    Ok(_) => {}
                }
                stream.set_nonblocking(false)?;
                if let Some(stream) = serve(stream, deadline(timeout)?) {
                    self.cache(stream)?;
                }
            }
        }
        Ok(())
    }
}
