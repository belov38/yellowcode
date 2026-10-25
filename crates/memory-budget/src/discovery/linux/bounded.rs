use super::bounded_io::{Error, KernelFile, NativeBoundedReader, ProcFile};
use crate::policy::history::{ConfirmedSweep, Key, Observation, Reduced, Tick};
use std::path::Path;

pub trait Reader {
    fn proc_file(&self, file: ProcFile, output: &mut [u8]) -> Result<usize, Error>;
    fn cgroup_file(
        &self,
        mount: &Path,
        relative: &Path,
        file: KernelFile,
        output: &mut [u8],
        component: &mut [u8],
    ) -> Result<usize, Error>;
    fn v1_unlimited_sentinel(&self) -> Option<u64> {
        None
    }
}
impl Reader for NativeBoundedReader {
    fn proc_file(&self, file: ProcFile, output: &mut [u8]) -> Result<usize, Error> {
        self.read_proc(file, output)
    }
    fn cgroup_file(
        &self,
        mount: &Path,
        relative: &Path,
        file: KernelFile,
        output: &mut [u8],
        component: &mut [u8],
    ) -> Result<usize, Error> {
        self.read_cgroup(mount, relative, file, output, component)
    }
    fn v1_unlimited_sentinel(&self) -> Option<u64> {
        super::KernelReader::v1_unlimited_sentinel(&super::NativeReader)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidScratch;
pub struct Scratch<'a> {
    membership: &'a mut [u8],
    mounts: &'a mut [u8],
    root: &'a mut [u8],
    mount: &'a mut [u8],
    relative: &'a mut [u8],
    text: &'a mut [u8],
    key: &'a mut [u8],
    provenance: &'a mut [u8],
    component: &'a mut [u8],
}
impl<'a> Scratch<'a> {
    #[allow(
        clippy::too_many_arguments,
        reason = "Disjoint caller-owned buffer loans"
    )]
    pub fn new(
        membership: &'a mut [u8],
        mounts: &'a mut [u8],
        root: &'a mut [u8],
        mount: &'a mut [u8],
        relative: &'a mut [u8],
        text: &'a mut [u8],
        key: &'a mut [u8],
        provenance: &'a mut [u8],
        component: &'a mut [u8],
    ) -> Result<Self, InvalidScratch> {
        let prov = 17usize
            .checked_add(membership.len())
            .ok_or(InvalidScratch)?;
        let source = 18usize
            .checked_add(mount.len())
            .and_then(|n| n.checked_add(relative.len()))
            .ok_or(InvalidScratch)?;
        if key.len() < source.max(prov) || provenance.len() < prov {
            return Err(InvalidScratch);
        }
        Ok(Self {
            membership,
            mounts,
            root,
            mount,
            relative,
            text,
            key,
            provenance,
            component,
        })
    }
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Summary {
    pub warnings: u32,
    pub mappings: u64,
    pub reads: u64,
    pub retries: u8,
    pub soft_minimum: Option<u64>,
    pub physical_observed: Option<u64>,
    pub cgroup_v1: bool,
    pub cgroup_v2: bool,
}
pub const MALFORMED: u32 = 1;
pub const CAPACITY: u32 = 2;
pub const DENIED: u32 = 4;
pub const MISSING: u32 = 8;
pub const IO: u32 = 16;
pub const HIDDEN: u32 = 32;
pub const AMBIGUOUS: u32 = 64;
pub const MIGRATED: u32 = 128;
pub const HIERARCHY: u32 = 256;

struct State {
    summary: Summary,
    partial: bool,
}
impl State {
    fn bad(&mut self, bit: u32) {
        self.summary.warnings |= bit;
        self.partial = true;
    }
    fn error(&mut self, error: Error) {
        self.bad(match error {
            Error::Capacity => CAPACITY,
            Error::UnsafePath => MALFORMED,
            Error::Io(Some(libc::ENOENT)) => MISSING,
            Error::Io(Some(libc::EACCES | libc::EPERM)) => DENIED,
            Error::Io(_) => IO,
        });
    }
    fn read(&mut self) {
        self.summary.reads = self.summary.reads.saturating_add(1);
    }
}

pub fn discover(
    reader: &impl Reader,
    scratch: Scratch<'_>,
    mut tick: Tick<'_, '_>,
) -> (Reduced, Summary) {
    let mut state = State {
        summary: Summary::default(),
        partial: false,
    };
    state.read();
    let mut member_len = match reader.proc_file(ProcFile::Membership, scratch.membership) {
        Ok(n) if n <= scratch.membership.len() => n,
        Ok(_) => {
            state.error(Error::Capacity);
            return (tick.finish_partial(), state.summary);
        }
        Err(e) => {
            state.error(e);
            return (tick.finish_partial(), state.summary);
        }
    };
    for attempt in 0..2 {
        let raw = &scratch.membership[..member_len];
        let v1 = raw.split(|b| *b == b'\n').any(|line| {
            line.split(|b| *b == b':')
                .nth(1)
                .is_some_and(|c| token(c, b"memory"))
        });
        let mut first: Option<Member<'_>> = None;
        for line in raw.split(|b| *b == b'\n').filter(|line| !line.is_empty()) {
            match member(line) {
                Ok(Some(m)) if m.v1 == v1 => {
                    if let Some(old) = first {
                        if old.id != m.id || !same_path(old.path, m.path) {
                            state.bad(AMBIGUOUS);
                        }
                    } else {
                        first = Some(m);
                    }
                }
                Ok(_) => {}
                Err(()) => state.bad(MALFORMED),
            }
        }
        if first.is_none() {
            state.bad(MISSING);
        }
        state.read();
        match reader.proc_file(ProcFile::Mountinfo, scratch.mounts) {
            Ok(n) if n <= scratch.mounts.len() => {
                for line in raw.split(|b| *b == b'\n').filter(|line| !line.is_empty()) {
                    let m = match member(line) {
                        Ok(Some(m)) if m.v1 == v1 => m,
                        _ => continue,
                    };
                    let prov_len = provenance(m, scratch.provenance);
                    let path = &scratch.provenance[17..prov_len];
                    let mut found = false;
                    for line in scratch.mounts[..n]
                        .split(|b| *b == b'\n')
                        .filter(|l| !l.is_empty())
                    {
                        let mapping = match mount_fields(line, v1) {
                            Ok(Some(fields)) => fields,
                            Ok(None) => continue,
                            Err(()) => {
                                state.bad(MALFORMED);
                                continue;
                            }
                        };
                        let root_len = match decode(mapping.0, scratch.root) {
                            Ok(n) => n,
                            Err(e) => {
                                state.error(e);
                                continue;
                            }
                        };
                        let mount_len = match decode(mapping.1, scratch.mount) {
                            Ok(n) => n,
                            Err(e) => {
                                state.error(e);
                                continue;
                            }
                        };
                        let root = &scratch.root[..root_len];
                        let Some(relative) = strip_root(path, root) else {
                            continue;
                        };
                        if relative.len() > scratch.relative.len() {
                            state.bad(CAPACITY);
                            continue;
                        }
                        scratch.relative[..relative.len()].copy_from_slice(relative);
                        found = true;
                        state.summary.mappings = state.summary.mappings.saturating_add(1);
                        if root != b"/" {
                            state.bad(HIDDEN);
                        }
                        let Ok(mount) = std::str::from_utf8(&scratch.mount[..mount_len]) else {
                            state.bad(MALFORMED);
                            continue;
                        };
                        let mut len = relative.len();
                        let leaf_len = len;
                        loop {
                            let Ok(rel) = std::str::from_utf8(&scratch.relative[..len]) else {
                                state.bad(MALFORMED);
                                break;
                            };
                            let root = len == 0;
                            let mut io = Level {
                                reader,
                                mount: Path::new(mount),
                                relative: Path::new(rel),
                                text: scratch.text,
                                component: scratch.component,
                                key: scratch.key,
                                provenance: &scratch.provenance[..prov_len],
                                tick: &mut tick,
                                state: &mut state,
                                root,
                                v1,
                                hard_observed: false,
                            };
                            if v1 {
                                io.hard(KernelFile::MemoryLimit, 1, false);
                                io.hard(KernelFile::MemoryMemsw, 2, true);
                                io.stat();
                                io.hierarchy(len != leaf_len);
                            } else {
                                io.hard(KernelFile::MemoryMax, 0, false);
                                io.soft();
                            }
                            if root {
                                break;
                            }
                            len = scratch.relative[..len]
                                .iter()
                                .rposition(|b| *b == b'/')
                                .unwrap_or(0);
                        }
                    }
                    if !found {
                        state.bad(MISSING);
                    }
                }
            }
            Ok(_) => state.error(Error::Capacity),
            Err(e) => state.error(e),
        }
        state.read();
        match reader.proc_file(ProcFile::Meminfo, scratch.text) {
            Ok(n) if n <= scratch.text.len() => match physical(&scratch.text[..n]) {
                Some(bytes) => {
                    tick.physical(bytes);
                    state.summary.physical_observed =
                        minimum(state.summary.physical_observed, Some(bytes));
                }
                None => state.bad(MALFORMED),
            },
            Ok(_) => state.error(Error::Capacity),
            Err(e) => state.error(e),
        }
        let before_len = first
            .map(|m| provenance(m, scratch.provenance))
            .unwrap_or(0);
        state.read();
        let after_len = match reader.proc_file(ProcFile::Membership, scratch.mounts) {
            Ok(n) if n <= scratch.mounts.len() => n,
            Ok(_) => {
                state.error(Error::Capacity);
                return (tick.finish_partial(), state.summary);
            }
            Err(e) => {
                state.error(e);
                return (tick.finish_partial(), state.summary);
            }
        };
        if after_len > scratch.membership.len() {
            state.error(Error::Capacity);
            return (tick.finish_partial(), state.summary);
        }
        if scratch.mounts[..after_len] != *raw {
            state.summary.warnings |= MIGRATED;
            if attempt == 0 {
                scratch.membership[..after_len].copy_from_slice(&scratch.mounts[..after_len]);
                member_len = after_len;
                state.summary.retries = 1;
                continue;
            }
            state.bad(MIGRATED);
        }
        let after = &scratch.mounts[..after_len];
        let after_v1 = after.split(|b| *b == b'\n').any(|line| {
            line.split(|b| *b == b':')
                .nth(1)
                .is_some_and(|c| token(c, b"memory"))
        });
        let mut after_member = None;
        for line in after.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
            match member(line) {
                Ok(Some(m)) if m.v1 == after_v1 => {
                    if let Some(old) = after_member {
                        let old: Member<'_> = old;
                        if old.id != m.id || !same_path(old.path, m.path) {
                            state.bad(AMBIGUOUS);
                        }
                    } else {
                        after_member = Some(m);
                    }
                }
                Ok(_) => {}
                Err(()) => state.bad(MALFORMED),
            }
        }
        let after_prov = after_member
            .map(|m| provenance(m, scratch.key))
            .unwrap_or(0);
        if state.partial {
            tick.mark_partial();
        }
        let reduced = tick.finish_complete(ConfirmedSweep::new(
            &scratch.provenance[..before_len],
            &scratch.key[..after_prov],
            !state.partial,
        ));
        return (reduced, state.summary);
    }
    (tick.finish_partial(), state.summary)
}

#[derive(Clone, Copy)]
struct Member<'a> {
    id: u64,
    v1: bool,
    path: &'a [u8],
}
fn token(bytes: &[u8], name: &[u8]) -> bool {
    bytes.split(|b| *b == b',').any(|v| v == name)
}
fn member(line: &[u8]) -> Result<Option<Member<'_>>, ()> {
    let mut fields = line.splitn(3, |b| *b == b':');
    let id = number(fields.next().ok_or(())?).ok_or(())?;
    let controllers = fields.next().ok_or(())?;
    let path = fields.next().ok_or(())?;
    let v1 = token(controllers, b"memory");
    if !v1 && !controllers.is_empty() {
        return Ok(None);
    }
    if (v1 && id == 0) || (!v1 && id != 0) || !valid_path(path) {
        return Err(());
    }
    Ok(Some(Member { id, v1, path }))
}
fn valid_path(bytes: &[u8]) -> bool {
    bytes.starts_with(b"/")
        && !bytes.contains(&0)
        && std::str::from_utf8(bytes).is_ok()
        && !bytes.split(|b| *b == b'/').any(|p| p == b"." || p == b"..")
}
fn components(bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    bytes.split(|b| *b == b'/').filter(|p| !p.is_empty())
}
fn same_path(a: &[u8], b: &[u8]) -> bool {
    components(a).eq(components(b))
}
fn canonical(bytes: &[u8], output: &mut [u8]) -> usize {
    let mut n = 0;
    for part in components(bytes) {
        output[n] = b'/';
        n += 1;
        output[n..n + part.len()].copy_from_slice(part);
        n += part.len();
    }
    if n == 0 {
        output[0] = b'/';
        n = 1;
    }
    n
}
fn provenance(m: Member<'_>, output: &mut [u8]) -> usize {
    output[0] = if m.v1 { 1 } else { 2 };
    output[1..9].copy_from_slice(&m.id.to_le_bytes());
    let n = canonical(m.path, &mut output[17..]);
    output[9..17].copy_from_slice(&(n as u64).to_le_bytes());
    17 + n
}
fn strip_root<'a>(path: &'a [u8], root: &[u8]) -> Option<&'a [u8]> {
    if root == b"/" {
        return Some(&path[1..]);
    }
    if path == root {
        return Some(&[]);
    }
    path.strip_prefix(root).and_then(|p| p.strip_prefix(b"/"))
}
type MountFields<'a> = (&'a [u8], &'a [u8]);
fn mount_fields(line: &[u8], v1: bool) -> Result<Option<MountFields<'_>>, ()> {
    let mut f = line
        .split(|b| b.is_ascii_whitespace())
        .filter(|p| !p.is_empty());
    number(f.next().ok_or(())?).ok_or(())?;
    number(f.next().ok_or(())?).ok_or(())?;
    let device = f.next().ok_or(())?;
    let mut numbers = device.splitn(2, |b| *b == b':');
    let major = numbers.next().ok_or(())?;
    let minor = numbers.next().ok_or(())?;
    number(major).ok_or(())?;
    number(minor).ok_or(())?;
    let root = f.next().ok_or(())?;
    let mount = f.next().ok_or(())?;
    f.next().ok_or(())?;
    loop {
        if f.next().ok_or(())? == b"-" {
            break;
        }
    }
    let fs = f.next().ok_or(())?;
    f.next().ok_or(())?;
    let options = f.next().ok_or(())?;
    if f.next().is_some() {
        return Err(());
    }
    Ok((if v1 {
        fs == b"cgroup" && token(options, b"memory")
    } else {
        fs == b"cgroup2"
    })
    .then_some((root, mount)))
}
fn decode(input: &[u8], output: &mut [u8]) -> Result<usize, Error> {
    let (mut at, mut used) = (0, 0);
    while at < input.len() {
        let byte = if input[at] == b'\\' {
            let digits = input.get(at + 1..at + 4).ok_or(Error::UnsafePath)?;
            let mut value: u16 = 0;
            for d in digits {
                if !(b'0'..=b'7').contains(d) {
                    return Err(Error::UnsafePath);
                }
                value = value * 8 + u16::from(d - b'0');
            }
            at += 4;
            u8::try_from(value).map_err(|_| Error::UnsafePath)?
        } else {
            let b = input[at];
            at += 1;
            b
        };
        *output.get_mut(used).ok_or(Error::Capacity)? = byte;
        used += 1;
    }
    if !valid_path(&output[..used]) {
        return Err(Error::UnsafePath);
    }
    let (mut read, mut write) = (0, 0);
    while read < used {
        let byte = output[read];
        read += 1;
        if byte == b'/' && write > 0 && output[write - 1] == b'/' {
            continue;
        }
        output[write] = byte;
        write += 1;
    }
    if write > 1 && output[write - 1] == b'/' {
        write -= 1;
    }
    Ok(write)
}
fn number(bytes: &[u8]) -> Option<u64> {
    if bytes.is_empty() {
        return None;
    }
    bytes.iter().try_fold(0u64, |n, d| {
        if !d.is_ascii_digit() {
            return None;
        }
        n.checked_mul(10)?.checked_add(u64::from(d - b'0'))
    })
}
fn trim(bytes: &[u8]) -> &[u8] {
    bytes.trim_ascii()
}
fn limit(bytes: &[u8], v1: bool, sentinel: Option<u64>) -> Option<Observation> {
    let bytes = trim(bytes);
    if !v1 && bytes == b"max" {
        return Some(Observation::Unlimited);
    }
    let value = number(bytes)?;
    Some(if v1 && Some(value) == sentinel {
        Observation::Unlimited
    } else {
        Observation::Finite(value)
    })
}
fn minimum(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}
fn physical(bytes: &[u8]) -> Option<u64> {
    let mut found = None;
    for line in bytes.split(|b| *b == b'\n') {
        let mut f = line
            .split(|b| b.is_ascii_whitespace())
            .filter(|v| !v.is_empty());
        if f.next() == Some(b"MemTotal:".as_slice()) {
            let value = number(f.next()?)?.checked_mul(1024)?;
            if f.next() != Some(b"kB".as_slice()) || f.next().is_some() || found.is_some() {
                return None;
            }
            found = Some(value);
        }
    }
    found
}

struct Level<'a, 'h, 'p, R> {
    reader: &'a R,
    mount: &'a Path,
    relative: &'a Path,
    text: &'a mut [u8],
    component: &'a mut [u8],
    key: &'a mut [u8],
    provenance: &'a [u8],
    tick: &'a mut Tick<'h, 'p>,
    state: &'a mut State,
    root: bool,
    v1: bool,
    hard_observed: bool,
}
impl<R: Reader> Level<'_, '_, '_, R> {
    fn read(&mut self, file: KernelFile, optional: bool) -> Option<usize> {
        self.state.read();
        match self
            .reader
            .cgroup_file(self.mount, self.relative, file, self.text, self.component)
        {
            Ok(n) if n <= self.text.len() => Some(n),
            Ok(_) => {
                self.state.error(Error::Capacity);
                None
            }
            Err(Error::Io(Some(libc::ENOENT))) if self.root || optional => None,
            Err(e) => {
                self.state.error(e);
                None
            }
        }
    }
    fn observe(&mut self, tag: u8, value: Observation) {
        use std::os::unix::ffi::OsStrExt;
        let mount = self.mount.as_os_str().as_bytes();
        let relative = self.relative.as_os_str().as_bytes();
        self.hard_observed = true;
        if self.v1 {
            self.state.summary.cgroup_v1 = true;
        } else {
            self.state.summary.cgroup_v2 = true;
        }
        self.key[0] = 1;
        self.key[1] = tag;
        self.key[2..10].copy_from_slice(&(mount.len() as u64).to_le_bytes());
        let split = 10 + mount.len();
        self.key[10..split].copy_from_slice(mount);
        self.key[split..split + 8].copy_from_slice(&(relative.len() as u64).to_le_bytes());
        let end = split + 8 + relative.len();
        self.key[split + 8..end].copy_from_slice(relative);
        self.tick
            .observe(Key::kernel(&self.key[..end], self.provenance), value);
    }
    fn hard(&mut self, file: KernelFile, tag: u8, optional: bool) {
        let Some(n) = self.read(file, optional) else {
            return;
        };
        match limit(
            &self.text[..n],
            self.v1,
            self.reader.v1_unlimited_sentinel(),
        ) {
            Some(value) => self.observe(tag, value),
            None => self.state.bad(MALFORMED),
        }
    }
    fn soft(&mut self) {
        let Some(n) = self.read(KernelFile::MemoryHigh, false) else {
            return;
        };
        match limit(&self.text[..n], false, None) {
            Some(Observation::Finite(v)) => {
                self.state.summary.soft_minimum = minimum(self.state.summary.soft_minimum, Some(v))
            }
            Some(Observation::Unlimited) => {}
            None => self.state.bad(MALFORMED),
        }
    }
    fn hierarchy(&mut self, ancestor: bool) {
        let Some(n) = self.read(KernelFile::MemoryHierarchy, false) else {
            if ancestor && self.hard_observed {
                self.state.bad(HIERARCHY);
            }
            return;
        };
        match number(trim(&self.text[..n])) {
            Some(1) => {}
            Some(0) if !ancestor => {}
            Some(0) => self.state.bad(HIERARCHY),
            _ => self.state.bad(MALFORMED),
        }
    }
    fn stat(&mut self) {
        let Some(n) = self.read(KernelFile::MemoryStat, false) else {
            return;
        };
        let mut at = 0;
        while at < n {
            let len = self.text[at..n]
                .iter()
                .position(|b| *b == b'\n')
                .unwrap_or(n - at);
            let line = &self.text[at..at + len];
            let mut f = line
                .split(|b| b.is_ascii_whitespace())
                .filter(|v| !v.is_empty());
            if f.next() == Some(b"hierarchical_memory_limit".as_slice()) {
                let value = f
                    .next()
                    .and_then(|v| limit(v, true, self.reader.v1_unlimited_sentinel()));
                if f.next().is_some() {
                    self.state.bad(MALFORMED);
                } else if let Some(value) = value {
                    self.observe(3, value);
                } else {
                    self.state.bad(MALFORMED);
                }
            }
            at += len + usize::from(at + len < n);
        }
    }
}
