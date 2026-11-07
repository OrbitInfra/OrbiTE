use crate::{
    boundary::*,
    engine::{self, Engine},
    records::Buffer,
};
use orbite::{FileSegment, RemoteSegment, TransferTarget};
use std::collections::HashMap;

#[derive(Default)]
pub(crate) struct Segments {
    entries: HashMap<u64, Target>,
}
#[derive(Clone)]
pub(crate) enum Target {
    Peer(RemoteSegment),
    File(FileSegment),
}
impl Target {
    pub fn view(&self) -> TransferTarget<'_> {
        match self {
            Self::Peer(s) => s.into(),
            Self::File(s) => s.into(),
        }
    }
}
impl Engine {
    pub fn open(&self, endpoint: &str, epoch: &str) -> Result<u64> {
        let peer = self.core.open_segment(
            endpoint.parse().map_err(|_| INVALID)?,
            epoch.parse()?,
            self.timeout(),
        )?;
        self.insert_target(Target::Peer(peer))
    }
    pub fn open_file(&self, path: &str, permission: i32) -> Result<u64> {
        self.insert_target(Target::File(
            self.core
                .open_file_segment(path, crate::memory::permission(permission)?)?,
        ))
    }
    fn insert_target(&self, target: Target) -> Result<u64> {
        let mut table = lock(&self.segments)?;
        if table.entries.len() >= self.core.config().max_peer_segments {
            return Err(FULL);
        }
        let id = engine::id()? as u64;
        table.entries.insert(id, target);
        Ok(id)
    }
    pub fn close_segment(&self, id: u64) -> Result<()> {
        let mut table = lock(&self.segments)?;
        if let Target::File(f) = table.entries.get(&id).ok_or(INVALID)? {
            self.core.close_file_segment(f, self.timeout())?;
        }
        table.entries.remove(&id).ok_or(INVALID)?;
        Ok(())
    }
    pub fn target(&self, id: u64) -> Result<Target> {
        lock(&self.segments)?
            .entries
            .get(&id)
            .cloned()
            .ok_or(INVALID)
    }
    pub fn info(&self, id: Option<u64>) -> Result<(bool, Vec<Buffer>)> {
        let Some(id) = id else {
            return Ok((false, lock(&self.memory)?.info()?));
        };
        match self.target(id)? {
            Target::Peer(s) => Ok((
                false,
                s.regions()
                    .iter()
                    .map(|r| Buffer {
                        base: r.base_address(),
                        length: r.len() as u64,
                        location: fixed(r.location_name().as_str()),
                    })
                    .collect(),
            )),
            Target::File(f) => {
                let info = self.core.file_segment_info(&f)?;
                Ok((
                    true,
                    vec![Buffer {
                        base: info.offset,
                        length: info.length,
                        location: fixed("file"),
                    }],
                ))
            }
        }
    }
}
