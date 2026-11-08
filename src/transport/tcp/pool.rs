//! Exclusive, epoch-scoped stream leases, following HP-TCP's clean/dirty lanes.
//! The executing caller/batch worker owns the socket; no forwarding worker.
use crate::engine::remaining;
use crate::runtime::control_plane::ControlChannel;
use crate::runtime::segment_manager::PeerKey;
use crate::{Error, Result};
use std::collections::HashMap;
use std::io;
use std::net::{Shutdown, TcpStream};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;

type ShutdownFn = fn(&TcpStream) -> io::Result<()>;
fn shutdown(stream: &TcpStream) -> io::Result<()> {
    stream.shutdown(Shutdown::Both)
}

struct State {
    channel: Option<ControlChannel<'static>>,
    busy: bool,
    failure: Option<Error>,
    touched: Instant,
}
struct Lane {
    state: Mutex<State>,
    changed: Condvar,
    shutdown: ShutdownFn,
}
impl Lane {
    fn retire(&self, state: &mut State, channel: ControlChannel<'static>) -> Result<()> {
        match (self.shutdown)(&channel.stream) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotConnected => Ok(()),
            Err(e) => {
                let error = Error::Unquiesced {
                    operation: "TCP socket retirement",
                    code: e.raw_os_error().unwrap_or(-1),
                };
                state.channel = Some(channel);
                state.failure = Some(error.for_report());
                Err(error)
            }
        }
    }
    fn close(&self) -> Result<()> {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(error) = &state.failure {
            return Err(error.for_report());
        }
        if state.busy {
            return Err(Error::Unquiesced {
                operation: "TCP lane still borrowed",
                code: libc::EBUSY,
            });
        }
        if let Some(channel) = state.channel.take() {
            self.retire(&mut state, channel)?;
        }
        Ok(())
    }
}
impl Drop for Lane {
    fn drop(&mut self) {
        let state = self.state.get_mut().unwrap_or_else(|p| p.into_inner());
        if state.failure.is_some() {
            // Teardown was attempted once. Even engine destruction cannot retry
            // it or release the uncertain descriptor.
            if let Some(channel) = state.channel.take() {
                std::mem::forget(channel);
            }
        }
    }
}

pub(super) struct Pool {
    lanes: Mutex<HashMap<PeerKey, Arc<Lane>>>,
    capacity: usize,
    shutdown: ShutdownFn,
}
impl Pool {
    pub(super) fn new(capacity: usize) -> Self {
        Self {
            lanes: Mutex::default(),
            capacity,
            shutdown,
        }
    }

    pub(super) fn checkout(&self, peer: PeerKey, deadline: Instant) -> Result<Lease> {
        let lane = {
            let mut lanes = self.lanes.lock().unwrap_or_else(|p| p.into_inner());
            if !lanes.contains_key(&peer) {
                if lanes.len() == self.capacity {
                    let oldest = lanes
                        .iter()
                        .filter(|(_, lane)| Arc::strong_count(lane) == 1)
                        .filter_map(|(key, lane)| {
                            let s = lane.state.lock().unwrap_or_else(|p| p.into_inner());
                            (!s.busy && s.failure.is_none()).then_some((*key, s.touched))
                        })
                        .min_by_key(|(_, touched)| *touched)
                        .map(|(key, _)| key)
                        .ok_or(Error::ResourceExhausted)?;
                    if let Some(lane) = lanes.get(&oldest) {
                        lane.close()?;
                    }
                    lanes.remove(&oldest);
                }
                lanes.try_reserve(1).map_err(|_| Error::ResourceExhausted)?;
                lanes.insert(
                    peer,
                    Arc::new(Lane {
                        state: Mutex::new(State {
                            channel: None,
                            busy: false,
                            failure: None,
                            touched: Instant::now(),
                        }),
                        changed: Condvar::new(),
                        shutdown: self.shutdown,
                    }),
                );
            }
            lanes
                .get(&peer)
                .cloned()
                .ok_or(Error::Internal("missing TCP lane"))?
        };
        let mut state = lane.state.lock().unwrap_or_else(|p| p.into_inner());
        while state.busy {
            state = lane
                .changed
                .wait_timeout(state, remaining(deadline)?)
                .unwrap_or_else(|p| p.into_inner())
                .0;
        }
        if let Some(error) = &state.failure {
            return Err(error.for_report());
        }
        remaining(deadline)?;
        state.busy = true;
        let channel = state.channel.take();
        drop(state);
        let mut lease = Lease {
            lane,
            channel,
            released: false,
        };
        // EOF on an idle, completed stream is safe to replace before publishing
        // this request. No retry is performed after any part of this request.
        let ready = (|| {
            if let Some(channel) = &mut lease.channel {
                channel.stream.set_nonblocking(true)?;
                channel.nonblocking = true;
                match channel.stream.peek(&mut [0]) {
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                    Ok(0) => lease.retire_idle()?,
                    Err(e) => return Err(e.into()),
                    Ok(_) => return Err(Error::Protocol("unexpected bytes on idle TCP lane")),
                }
            }
            if lease.channel.is_none() {
                lease.channel = Some(ControlChannel::connect(peer.endpoint, deadline)?);
            }
            lease.channel()?.deadline = deadline;
            Ok(())
        })();
        if let Err(error) = ready {
            // Cleanup uncertainty supersedes a setup error; Drop cannot report
            // that receipt to the physical owner.
            lease.finish(false)?;
            return Err(error);
        }
        Ok(lease)
    }

    pub(super) fn close_peer(&self, peer: PeerKey) -> Result<()> {
        let mut lanes = self.lanes.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(lane) = lanes.get(&peer) {
            if Arc::strong_count(lane) != 1 {
                return Err(Error::Unquiesced {
                    operation: "TCP lane still referenced",
                    code: libc::EBUSY,
                });
            }
            lane.close()?;
            lanes.remove(&peer);
        }
        Ok(())
    }
    pub(super) fn close(&self) -> Result<()> {
        let mut lanes = self.lanes.lock().unwrap_or_else(|p| p.into_inner());
        let mut failure = None;
        lanes.retain(|_, lane| {
            match if Arc::strong_count(lane) == 1 {
                lane.close()
            } else {
                Err(Error::Unquiesced {
                    operation: "TCP lane still referenced",
                    code: libc::EBUSY,
                })
            } {
                Ok(()) => false,
                Err(error) => {
                    failure.get_or_insert(error);
                    true
                }
            }
        });
        failure.map_or(Ok(()), Err)
    }
}
impl Drop for Pool {
    fn drop(&mut self) {
        if let Err(error) = self.close() {
            log::error!("TCP pool retained uncertain sockets: {error}");
            for (_, lane) in self
                .lanes
                .get_mut()
                .unwrap_or_else(|p| p.into_inner())
                .drain()
            {
                std::mem::forget(lane);
            }
        }
    }
}

pub(super) struct Lease {
    lane: Arc<Lane>,
    channel: Option<ControlChannel<'static>>,
    released: bool,
}
impl Lease {
    pub(super) fn channel(&mut self) -> Result<&mut ControlChannel<'static>> {
        self.channel
            .as_mut()
            .ok_or(Error::Internal("missing TCP stream"))
    }
    fn retire_idle(&mut self) -> Result<()> {
        if let Some(channel) = self.channel.take() {
            let mut state = self.lane.state.lock().unwrap_or_else(|p| p.into_inner());
            self.lane.retire(&mut state, channel)?;
        }
        Ok(())
    }
    fn release(&mut self, clean: bool) -> Result<()> {
        self.released = true;
        let mut state = self.lane.state.lock().unwrap_or_else(|p| p.into_inner());
        let result = if let Some(channel) = self.channel.take() {
            if clean {
                state.channel = Some(channel);
                Ok(())
            } else {
                self.lane.retire(&mut state, channel)
            }
        } else {
            Ok(())
        };
        state.busy = false;
        state.touched = Instant::now();
        self.lane.changed.notify_all();
        result
    }
    pub(super) fn finish(mut self, clean: bool) -> Result<()> {
        self.release(clean)
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        if !self.released {
            if let Err(error) = self.release(false) {
                log::error!("TCP lease retained uncertain socket: {error}");
            }
        }
    }
}

#[cfg(test)]
#[path = "../../../tests/unit/transport/tcp/pool.rs"]
mod tests;
