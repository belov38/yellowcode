pub const POOL_BYTES: usize = 1024 * 1024;
const HEADER: usize = 32;
const OLD: u8 = 1;
const FINITE: u8 = 2;
const UNLIMITED: u8 = 4;

#[derive(Clone, Copy)]
pub struct Key<'a> {
    source: &'a [u8],
    provenance: &'a [u8],
    deployment: bool,
}
impl<'a> Key<'a> {
    pub fn kernel(source: &'a [u8], membership: &'a [u8]) -> Self {
        Self {
            source,
            provenance: membership,
            deployment: false,
        }
    }
    pub fn deployment(source: &'a [u8]) -> Self {
        Self {
            source,
            provenance: &[],
            deployment: true,
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Observation {
    Finite(u64),
    Unlimited,
}
pub struct ConfirmedSweep<'a> {
    before: &'a [u8],
    after: &'a [u8],
    complete: bool,
}
impl<'a> ConfirmedSweep<'a> {
    pub fn new(before: &'a [u8], after: &'a [u8], complete: bool) -> Self {
        Self {
            before,
            after,
            complete,
        }
    }
    fn confirmed(&self) -> bool {
        self.complete && !self.before.is_empty() && self.before == self.after
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Reduced {
    pub external_ceiling: Option<u64>,
    pub physical_ceiling: Option<u64>,
    pub partial: bool,
}
impl Reduced {
    pub fn effective_envelope(self) -> Option<u64> {
        self.external_ceiling.map(|external| {
            self.physical_ceiling
                .map_or(external, |ram| external.min(ram))
        })
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidPools;

pub struct History<'a> {
    active: &'a mut [u8],
    scratch: &'a mut [u8],
    used: usize,
    unidentified: Option<u64>,
    physical: Option<u64>,
    partial: bool,
}
impl<'a> History<'a> {
    pub fn new(active: &'a mut [u8], scratch: &'a mut [u8]) -> Result<Self, InvalidPools> {
        if active.len() != POOL_BYTES || scratch.len() != POOL_BYTES {
            return Err(InvalidPools);
        }
        Ok(Self {
            active,
            scratch,
            used: 0,
            unidentified: None,
            physical: None,
            partial: false,
        })
    }
    pub fn begin(&mut self) -> Tick<'_, 'a> {
        self.scratch[..self.used].copy_from_slice(&self.active[..self.used]);
        let used = self.used;
        Tick {
            history: self,
            used,
            physical: None,
            partial: false,
            done: false,
        }
    }
    pub fn reduced(&self) -> Reduced {
        let mut ceiling = self.unidentified;
        let mut at = 0;
        while at < self.used {
            let record = Record::read(self.active, at);
            ceiling = minimum(ceiling, Some(record.old));
            at += record.total;
        }
        Reduced {
            external_ceiling: ceiling,
            physical_ceiling: self.physical,
            partial: self.partial,
        }
    }
}

pub struct Tick<'h, 'a> {
    history: &'h mut History<'a>,
    used: usize,
    physical: Option<u64>,
    partial: bool,
    done: bool,
}
impl Tick<'_, '_> {
    pub fn observe(&mut self, key: Key<'_>, observation: Observation) {
        let mut at = 0;
        while at < self.used {
            let mut record = Record::read(self.history.scratch, at);
            if record.matches(self.history.scratch, at, key) {
                match observation {
                    Observation::Finite(value) => {
                        record.tick = if record.flags & FINITE != 0 {
                            record.tick.min(value)
                        } else {
                            value
                        };
                        record.flags |= FINITE;
                    }
                    Observation::Unlimited => record.flags |= UNLIMITED,
                }
                record.write(self.history.scratch, at);
                return;
            }
            at += record.total;
        }
        let Observation::Finite(value) = observation else {
            return;
        };
        let size = HEADER
            .checked_add(key.source.len())
            .and_then(|n| n.checked_add(key.provenance.len()));
        let Some(size) = size.filter(|size| *size <= POOL_BYTES - self.used) else {
            self.history.unidentified = minimum(self.history.unidentified, Some(value));
            self.partial = true;
            return;
        };
        let record = Record {
            total: size,
            source: key.source.len(),
            provenance: key.provenance.len(),
            deployment: key.deployment,
            flags: FINITE,
            old: 0,
            tick: value,
        };
        record.write(self.history.scratch, self.used);
        let source = self.used + HEADER;
        let provenance = source + key.source.len();
        self.history.scratch[source..provenance].copy_from_slice(key.source);
        self.history.scratch[provenance..self.used + size].copy_from_slice(key.provenance);
        self.used += size;
    }
    pub fn physical(&mut self, bytes: u64) {
        self.physical = minimum(self.physical, Some(bytes));
    }
    pub fn mark_partial(&mut self) {
        self.partial = true;
    }
    pub fn finish_partial(mut self) -> Reduced {
        self.commit(None);
        self.history.reduced()
    }
    pub fn finish_complete(mut self, sweep: ConfirmedSweep<'_>) -> Reduced {
        let complete = !self.partial && sweep.confirmed() && self.can_complete(&sweep);
        self.commit(if complete { Some(&sweep) } else { None });
        self.history.reduced()
    }
    fn can_complete(&self, sweep: &ConfirmedSweep<'_>) -> bool {
        let mut external = self.history.unidentified;
        let mut at = 0;
        while at < self.used {
            let record = Record::read(self.history.scratch, at);
            if record.flags & FINITE != 0 {
                external = minimum(external, Some(record.tick));
            } else if record.flags & UNLIMITED == 0
                && !record.retired(self.history.scratch, at, sweep)
            {
                return false;
            }
            at += record.total;
        }
        external.is_none() || self.history.physical.is_none() || self.physical.is_some()
    }
    fn commit(&mut self, sweep: Option<&ConfirmedSweep<'_>>) {
        let mut read = 0;
        let mut write = 0;
        while read < self.used {
            let mut record = Record::read(self.history.scratch, read);
            let tick = (record.flags & FINITE != 0).then_some(record.tick);
            let value = if sweep.is_some() {
                tick
            } else {
                minimum((record.flags & OLD != 0).then_some(record.old), tick)
            };
            if let Some(value) = value {
                self.history
                    .scratch
                    .copy_within(read..read + record.total, write);
                record.old = value;
                record.tick = 0;
                record.flags = OLD;
                record.write(self.history.scratch, write);
                write += record.total;
            }
            read += record.total;
        }
        self.history.physical = if sweep.is_some() {
            self.physical.or(self.history.physical)
        } else {
            minimum(self.history.physical, self.physical)
        };
        self.history.partial = sweep.is_none() || self.history.unidentified.is_some();
        self.history.used = write;
        std::mem::swap(&mut self.history.active, &mut self.history.scratch);
        self.done = true;
    }
}
impl Drop for Tick<'_, '_> {
    fn drop(&mut self) {
        if !self.done {
            self.commit(None);
        }
    }
}
fn minimum(left: Option<u64>, right: Option<u64>) -> Option<u64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (left, right) => left.or(right),
    }
}

#[derive(Clone, Copy)]
struct Record {
    total: usize,
    source: usize,
    provenance: usize,
    deployment: bool,
    flags: u8,
    old: u64,
    tick: u64,
}
impl Record {
    fn read(bytes: &[u8], at: usize) -> Self {
        Self {
            total: read32(bytes, at) as usize,
            source: read32(bytes, at + 4) as usize,
            provenance: read32(bytes, at + 8) as usize,
            deployment: bytes[at + 12] != 0,
            flags: bytes[at + 13],
            old: read64(bytes, at + 16),
            tick: read64(bytes, at + 24),
        }
    }
    fn write(self, bytes: &mut [u8], at: usize) {
        bytes[at..at + 4].copy_from_slice(&(self.total as u32).to_le_bytes());
        bytes[at + 4..at + 8].copy_from_slice(&(self.source as u32).to_le_bytes());
        bytes[at + 8..at + 12].copy_from_slice(&(self.provenance as u32).to_le_bytes());
        bytes[at + 12] = u8::from(self.deployment);
        bytes[at + 13] = self.flags;
        bytes[at + 14..at + 16].fill(0);
        bytes[at + 16..at + 24].copy_from_slice(&self.old.to_le_bytes());
        bytes[at + 24..at + 32].copy_from_slice(&self.tick.to_le_bytes());
    }
    fn matches(self, bytes: &[u8], at: usize, key: Key<'_>) -> bool {
        let source = at + HEADER;
        let provenance = source + self.source;
        self.deployment == key.deployment
            && bytes[source..provenance] == *key.source
            && bytes[provenance..at + self.total] == *key.provenance
    }
    fn retired(self, bytes: &[u8], at: usize, sweep: &ConfirmedSweep<'_>) -> bool {
        !self.deployment
            && self.provenance != 0
            && bytes[at + HEADER + self.source..at + self.total] != *sweep.after
    }
}
fn read32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}
fn read64(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes([
        bytes[at],
        bytes[at + 1],
        bytes[at + 2],
        bytes[at + 3],
        bytes[at + 4],
        bytes[at + 5],
        bytes[at + 6],
        bytes[at + 7],
    ])
}
