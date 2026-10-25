use crate::error::{Error, Result};
use crate::format::{FileHeader, Format};
use crate::layout::{Captured, Layout, spec_of, write_exact};
use crate::read::{ChainReader, Container, EntryInfo, detect, from_io};
use crate::source::ReadAt;
use crate::time::V8Time;
use crate::write::{DataSource, EntrySpec, StorageVer, WriteOptions, write_container};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub const MANIFEST: &str = ".v8cont.json";
pub const RESIDUE: &str = ".v8cont.residue";
pub const PLACEHOLDER: &str = ".v8cont.placeholder";

const MANIFEST_VERSION: u32 = 1;
const ENTRY_TMP: &str = ".v8cont.entry";
const MAX_DEPTH: usize = 3;
const MAX_FILE_NAME: usize = 200;
const BUF: usize = 1 << 16;
const OUT_BUF: usize = 1 << 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnpackOptions {
    pub inflate: bool,
    pub recursive: bool,
    pub inflate_limit: u64,
}

impl Default for UnpackOptions {
    fn default() -> Self {
        UnpackOptions {
            inflate: false,
            recursive: false,
            inflate_limit: zraw::DEFAULT_LIMIT,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UnpackReport {
    pub entries: usize,
    pub files: usize,
    pub nested: usize,
    pub kept_raw: usize,
    pub layout: bool,
    pub residue_bytes: u64,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PackOptions {
    pub format: Option<Format>,
    pub now: bool,
    pub exact: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PackReport {
    pub entries: usize,
    pub exact: bool,
    pub warnings: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Manifest {
    v8cont: u32,
    format: Format,
    header: FileHeader,
    placeholder: Option<String>,
    residue: Option<String>,
    entries: Vec<ManifestEntry>,
    layout: Option<Layout>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ManifestEntry {
    name: String,
    file: String,
    ctime: V8Time,
    mtime: V8Time,
    attrs: u32,
    data: DataForm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum DataForm {
    None,
    Raw,
    Inflated,
    Container { deflated: bool },
}

fn io_at(path: &Path, e: io::Error) -> Error {
    Error::Io(io::Error::new(e.kind(), format!("{}: {e}", path.display())))
}

fn manifest_err(path: &Path, what: impl Into<String>) -> Error {
    Error::Manifest {
        path: path.to_path_buf(),
        what: what.into(),
    }
}

fn deflate_io(entry: &str, e: io::Error) -> Error {
    if !e.get_ref().is_some_and(|i| i.is::<zraw::Error>()) {
        return from_io(e);
    }
    match e.into_inner().map(|b| b.downcast::<zraw::Error>()) {
        Some(Ok(z)) => Error::Deflate {
            entry: entry.to_string(),
            source: *z,
        },
        _ => Error::Io(io::Error::other("deflate stream error")),
    }
}

fn zlib_err(entry: &str) -> impl FnOnce(zraw::Error) -> Error + '_ {
    move |source| Error::Deflate {
        entry: entry.to_string(),
        source,
    }
}

fn note(rel: &str, msg: String) -> String {
    if rel.is_empty() {
        msg
    } else {
        format!("{rel}: {msg}")
    }
}

fn sub_rel(rel: &str, file: &str) -> String {
    if rel.is_empty() {
        file.to_string()
    } else {
        format!("{rel}/{file}")
    }
}

fn is_device_name(s: &str) -> bool {
    let stem = s.split('.').next().unwrap_or(s).trim_end_matches(' ');
    match stem.to_ascii_uppercase().as_bytes() {
        b"CON" | b"PRN" | b"AUX" | b"NUL" => true,
        [b'C', b'O', b'M', d] | [b'L', b'P', b'T', d] => (b'1'..=b'9').contains(d),
        _ => false,
    }
}

#[derive(Default)]
pub(crate) struct FileNames {
    taken: HashSet<String>,
    next: HashMap<String, u64>,
}

impl FileNames {
    pub(crate) fn get(&mut self, name: &str) -> String {
        let s = safe_base_name(name);
        if self.taken.insert(s.to_lowercase()) {
            return s;
        }
        let key = s.to_lowercase();
        let mut n = self.next.get(&key).copied().unwrap_or(1);
        loop {
            let candidate = format!("{s}~{n}");
            n = n.saturating_add(1);
            if self.taken.insert(candidate.to_lowercase()) {
                self.next.insert(key, n);
                return candidate;
            }
        }
    }
}

fn safe_base_name(name: &str) -> String {
    let mut s: String = name
        .chars()
        .map(|c| {
            if c.is_control() || "<>:\"/\\|?*".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    if s.len() > MAX_FILE_NAME {
        let mut end = MAX_FILE_NAME;
        while !s.is_char_boundary(end) {
            end = end.saturating_sub(1);
        }
        s.truncate(end);
    }
    if s.is_empty()
        || s == "."
        || s == ".."
        || s.to_ascii_lowercase().starts_with(".v8cont")
        || is_device_name(&s)
    {
        s.insert(0, '_');
    }
    if s.ends_with('.') || s.ends_with(' ') {
        s.push('_');
    }
    s
}

fn create_new(path: &Path) -> Result<File> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| io_at(path, e))
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    create_new(path)?
        .write_all(bytes)
        .map_err(|e| io_at(path, e))
}

fn make_empty_dir(dir: &Path) -> Result<()> {
    match fs::read_dir(dir) {
        Ok(mut it) => match it.next() {
            None => Ok(()),
            Some(_) => Err(io_at(
                dir,
                io::Error::new(io::ErrorKind::AlreadyExists, "directory is not empty"),
            )),
        },
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            fs::create_dir_all(dir).map_err(|e| io_at(dir, e))
        }
        Err(e) => Err(io_at(dir, e)),
    }
}

fn read_full(r: &mut dyn Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while let Some(rest) = buf.get_mut(n..).filter(|b| !b.is_empty()) {
        match r.read(rest) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

pub fn unpack_file(src: &Path, dir: &Path, opts: &UnpackOptions) -> Result<UnpackReport> {
    let f = File::open(src).map_err(|e| io_at(src, e))?;
    let c = Container::open(f)?;
    unpack(&c, dir, opts)
}

pub fn unpack<R: ReadAt>(
    c: &Container<R>,
    dir: &Path,
    opts: &UnpackOptions,
) -> Result<UnpackReport> {
    let mut rep = UnpackReport::default();
    unpack_at(c, replayable_layout(c), dir, opts, 0, "", &mut rep)?;
    rep.entries = c.entries().len();
    Ok(rep)
}

fn replayable_layout<R: ReadAt>(c: &Container<R>) -> Result<Captured> {
    let cap = c.capture_layout()?;
    let specs: Vec<EntrySpec> = c.entries().iter().map(spec_of).collect();
    cap.layout.check(&specs)?;
    Ok(cap)
}

fn unpack_at<R: ReadAt>(
    c: &Container<R>,
    captured: Result<Captured>,
    dir: &Path,
    opts: &UnpackOptions,
    depth: usize,
    rel: &str,
    rep: &mut UnpackReport,
) -> Result<()> {
    make_empty_dir(dir)?;
    let (layout, residue, placeholder) = match captured {
        Ok(cap) => (Some(cap.layout), cap.residue, cap.placeholder),
        Err(e) => {
            let msg = format!("layout not recorded ({e}): pack will use the canonical writer");
            rep.warnings.push(note(rel, msg));
            (None, Vec::new(), None)
        }
    };
    if depth == 0 {
        rep.layout = layout.is_some();
    }
    let mut names = FileNames::default();
    let mut entries = Vec::with_capacity(c.entries().len());
    for e in c.entries() {
        let file = names.get(&e.name);
        let data = match e.data_addr {
            None => DataForm::None,
            Some(_) => unpack_entry(c, e, dir, &file, opts, depth, rel, rep)?,
        };
        entries.push(ManifestEntry {
            name: e.name.clone(),
            file,
            ctime: e.ctime,
            mtime: e.mtime,
            attrs: e.attrs,
            data,
        });
    }
    let residue_file = if residue.is_empty() {
        None
    } else {
        write_new(&dir.join(RESIDUE), &residue)?;
        rep.residue_bytes = rep.residue_bytes.saturating_add(residue.len() as u64);
        Some(RESIDUE.to_string())
    };
    let placeholder_file = match placeholder {
        Some(p) => {
            write_new(&dir.join(PLACEHOLDER), &p)?;
            Some(PLACEHOLDER.to_string())
        }
        None => None,
    };
    let m = Manifest {
        v8cont: MANIFEST_VERSION,
        format: c.format(),
        header: *c.header(),
        placeholder: placeholder_file,
        residue: residue_file,
        entries,
        layout,
    };
    let path = dir.join(MANIFEST);
    let json = serde_json::to_vec_pretty(&m).map_err(|e| manifest_err(&path, e.to_string()))?;
    write_new(&path, &json)
}

#[allow(clippy::too_many_arguments)]
fn unpack_entry<R: ReadAt>(
    c: &Container<R>,
    e: &EntryInfo,
    dir: &Path,
    file: &str,
    opts: &UnpackOptions,
    depth: usize,
    rel: &str,
    rep: &mut UnpackReport,
) -> Result<DataForm> {
    let path = dir.join(file);
    let mut written = None;
    if opts.inflate {
        let kept = match inflate_entry(c, e, &path, opts.inflate_limit)? {
            Inflated::Same(head) => {
                written = Some(head);
                None
            }
            Inflated::NotDeflate => None,
            Inflated::Differs => {
                Some("deflate at level 6 does not give the stored bytes back".to_string())
            }
            Inflated::TooLarge => Some(format!(
                "inflates to more than {} bytes",
                opts.inflate_limit
            )),
        };
        if let Some(why) = kept {
            rep.kept_raw += 1;
            let msg = format!("entry {}: {why}; kept as stored", e.name);
            rep.warnings.push(note(rel, msg));
        }
        if written.is_none() {
            fs::remove_file(&path).map_err(|err| io_at(&path, err))?;
        }
    }
    let inflated = written.is_some();
    let head = match written {
        Some(head) => head,
        None => copy_stored(c, e, &path)?,
    };
    if opts.recursive
        && depth < MAX_DEPTH
        && detect(&head).map_err(|err| io_at(&path, err))?.is_some()
    {
        let tmp = dir.join(ENTRY_TMP);
        fs::rename(&path, &tmp).map_err(|err| io_at(&tmp, err))?;
        if let Some((nc, cap)) = nested_container(&tmp, e, rel, rep)? {
            unpack_at(
                &nc,
                Ok(cap),
                &path,
                opts,
                depth + 1,
                &sub_rel(rel, file),
                rep,
            )?;
            drop(nc);
            fs::remove_file(&tmp).map_err(|err| io_at(&tmp, err))?;
            rep.nested += 1;
            return Ok(DataForm::Container { deflated: inflated });
        }
        fs::rename(&tmp, &path).map_err(|err| io_at(&path, err))?;
    }
    rep.files += 1;
    Ok(if inflated {
        DataForm::Inflated
    } else {
        DataForm::Raw
    })
}

fn nested_container(
    path: &Path,
    e: &EntryInfo,
    rel: &str,
    rep: &mut UnpackReport,
) -> Result<Option<(Container<File>, Captured)>> {
    let f = File::open(path).map_err(|err| io_at(path, err))?;
    let Ok(nc) = Container::open(f) else {
        return Ok(None);
    };
    match replayable_layout(&nc) {
        Ok(cap) => Ok(Some((nc, cap))),
        Err(err) => {
            let msg = format!(
                "entry {}: nested container without an exact layout ({err}); kept as a file",
                e.name
            );
            rep.warnings.push(note(rel, msg));
            Ok(None)
        }
    }
}

const HEAD: usize =
    (Format::F16.base() + Format::F16.file_header_size() + Format::F16.page_header_size()) as usize;

struct Head {
    bytes: Vec<u8>,
    size: u64,
}

impl ReadAt for Head {
    fn size(&self) -> io::Result<u64> {
        Ok(self.size)
    }
    fn read_exact_at(&self, buf: &mut [u8], offset: u64) -> io::Result<()> {
        self.bytes.read_exact_at(buf, offset)
    }
}

struct EntryFile<'p> {
    w: BufWriter<File>,
    head: Head,
    path: &'p Path,
}

impl<'p> EntryFile<'p> {
    fn create(path: &'p Path) -> Result<Self> {
        Ok(EntryFile {
            w: BufWriter::with_capacity(BUF, create_new(path)?),
            head: Head {
                bytes: Vec::new(),
                size: 0,
            },
            path,
        })
    }

    fn put(&mut self, b: &[u8]) -> Result<()> {
        let k = HEAD.saturating_sub(self.head.bytes.len()).min(b.len());
        self.head
            .bytes
            .extend_from_slice(b.get(..k).unwrap_or_default());
        self.head.size = self.head.size.saturating_add(b.len() as u64);
        self.w.write_all(b).map_err(|err| io_at(self.path, err))
    }

    fn finish(mut self) -> Result<Head> {
        self.w.flush().map_err(|err| io_at(self.path, err))?;
        Ok(self.head)
    }
}

fn copy_stored<R: ReadAt>(c: &Container<R>, e: &EntryInfo, path: &Path) -> Result<Head> {
    let mut out = EntryFile::create(path)?;
    let mut r = c.data_reader(e);
    let mut buf = vec![0u8; BUF];
    loop {
        let n = match r.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(from_io(err)),
        };
        out.put(buf.get(..n).unwrap_or_default())?;
    }
    out.finish()
}

enum Inflated {
    Same(Head),
    NotDeflate,
    Differs,
    TooLarge,
}

fn inflate_entry<R: ReadAt>(
    c: &Container<R>,
    e: &EntryInfo,
    path: &Path,
    limit: u64,
) -> Result<Inflated> {
    let mut out = Some(EntryFile::create(path)?);
    let mut v = Verify {
        r: c.data_reader(e),
        buf: vec![0u8; BUF],
        differs: false,
    };
    let mut d = Some(zraw::DeflateWriter::new(&mut v, zraw::LEVEL_1C).map_err(zlib_err(&e.name))?);
    let mut r = zraw::InflateReader::new(c.data_reader(e)).map_err(zlib_err(&e.name))?;
    let mut buf = vec![0u8; BUF];
    let mut total = 0u64;
    loop {
        let n = match r.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return inflate_failure(&e.name, err),
        };
        total = total.saturating_add(n as u64);
        if total > limit {
            return Ok(Inflated::TooLarge);
        }
        let chunk = buf.get(..n).unwrap_or_default();
        if let Some(dw) = d.as_mut()
            && let Err(err) = dw.write_all(chunk)
        {
            if !is_differs(&err) {
                return Err(deflate_io(&e.name, err));
            }
            d = None;
            out = None;
        }
        if let Some(f) = out.as_mut() {
            f.put(chunk)?;
        }
    }
    let (Some(dw), Some(f)) = (d, out) else {
        return Ok(Inflated::Differs);
    };
    match dw.finish() {
        Ok(v) => {
            if v.at_end().map_err(from_io)? {
                Ok(Inflated::Same(f.finish()?))
            } else {
                Ok(Inflated::Differs)
            }
        }
        Err(err) if is_differs(&err) => Ok(Inflated::Differs),
        Err(err) => Err(deflate_io(&e.name, err)),
    }
}

fn inflate_failure(entry: &str, err: io::Error) -> Result<Inflated> {
    match err.get_ref().and_then(|i| i.downcast_ref::<zraw::Error>()) {
        Some(zraw::Error::Zlib(_)) => Err(deflate_io(entry, err)),
        Some(_) => Ok(Inflated::NotDeflate),
        None => Err(from_io(err)),
    }
}

#[derive(Debug)]
struct Differs;

impl std::fmt::Display for Differs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("deflate output differs from the stored bytes")
    }
}

impl std::error::Error for Differs {}

fn is_differs(e: &io::Error) -> bool {
    e.get_ref().is_some_and(|i| i.is::<Differs>())
}

struct Verify<'a, R: ReadAt> {
    r: ChainReader<'a, R>,
    buf: Vec<u8>,
    differs: bool,
}

impl<R: ReadAt> Verify<'_, R> {
    fn at_end(&mut self) -> io::Result<bool> {
        let mut one = [0u8; 1];
        Ok(read_full(&mut self.r, &mut one)? == 0)
    }
}

impl<R: ReadAt> Write for Verify<'_, R> {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        if !self.differs {
            let k = b.len().min(self.buf.len());
            let (Some(want), Some(got)) = (b.get(..k), self.buf.get_mut(..k)) else {
                return Err(io::Error::other("verify buffer"));
            };
            let n = read_full(&mut self.r, got)?;
            if n == k && got == want {
                return Ok(k);
            }
            self.differs = true;
        }
        Err(io::Error::other(Differs))
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub fn pack(dir: &Path, out: &Path, opts: &PackOptions) -> Result<PackReport> {
    let parent = match out.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let mut spool = Spool::File {
        dir: parent.to_path_buf(),
        file: None,
    };
    let p = prepare(dir, opts, 0, "", &mut spool)?;
    spool.finish()?;
    let (tmp, file) = TempPath::create(parent, "pack")?;
    let mut w = BufWriter::with_capacity(OUT_BUF, file);
    emit(&p, &mut w, &spool)?;
    w.flush().map_err(|e| io_at(tmp.path(), e))?;
    drop(w);
    drop(spool);
    tmp.persist(out)?;
    Ok(p.report)
}

pub fn pack_to_vec(dir: &Path, opts: &PackOptions) -> Result<(Vec<u8>, PackReport)> {
    pack_vec_at(dir, opts, 0, "")
}

fn pack_vec_at(
    dir: &Path,
    opts: &PackOptions,
    depth: usize,
    rel: &str,
) -> Result<(Vec<u8>, PackReport)> {
    let mut spool = Spool::Mem(Vec::new());
    let p = prepare(dir, opts, depth, rel, &mut spool)?;
    let mut out = Vec::new();
    emit(&p, &mut out, &spool)?;
    Ok((out, p.report))
}

enum Src {
    None,
    File(PathBuf, u64),
    Spool(u64, u64),
}

enum Mode {
    Exact {
        layout: Layout,
        residue: Vec<u8>,
        placeholder: Option<Vec<u8>>,
    },
    Canonical(WriteOptions),
}

struct Prepared {
    specs: Vec<EntrySpec>,
    srcs: Vec<Src>,
    mode: Mode,
    report: PackReport,
}

fn prepare(
    dir: &Path,
    opts: &PackOptions,
    depth: usize,
    rel: &str,
    spool: &mut Spool,
) -> Result<Prepared> {
    let mpath = dir.join(MANIFEST);
    match fs::read(&mpath) {
        Ok(bytes) => prepare_manifest(dir, &mpath, &bytes, opts, depth, rel, spool),
        Err(e) if e.kind() == io::ErrorKind::NotFound => prepare_plain(dir, opts, depth),
        Err(e) => Err(io_at(&mpath, e)),
    }
}

fn member(dir: &Path, mpath: &Path, what: &str, name: &str) -> Result<PathBuf> {
    let mut comps = Path::new(name).components();
    let plain = matches!(
        (comps.next(), comps.next()),
        (Some(Component::Normal(_)), None)
    ) && !name.contains(['/', '\\']);
    if !plain {
        return Err(manifest_err(
            mpath,
            format!("{what}: {name:?} is not a plain file name"),
        ));
    }
    Ok(dir.join(name))
}

fn file_len(path: &Path, mpath: &Path, entry: &str) -> Result<u64> {
    let md = fs::metadata(path).map_err(|e| io_at(path, e))?;
    if !md.is_file() {
        return Err(manifest_err(
            mpath,
            format!("entry {entry}: {} is not a file", path.display()),
        ));
    }
    Ok(md.len())
}

fn prepare_manifest(
    dir: &Path,
    mpath: &Path,
    bytes: &[u8],
    opts: &PackOptions,
    depth: usize,
    rel: &str,
    spool: &mut Spool,
) -> Result<Prepared> {
    let mut m: Manifest =
        serde_json::from_slice(bytes).map_err(|e| manifest_err(mpath, e.to_string()))?;
    if m.v8cont != MANIFEST_VERSION {
        return Err(manifest_err(
            mpath,
            format!("version {}, expected {MANIFEST_VERSION}", m.v8cont),
        ));
    }
    let mut report = PackReport {
        entries: m.entries.len(),
        exact: false,
        warnings: Vec::new(),
    };
    let nested_opts = PackOptions {
        format: None,
        ..*opts
    };
    let mut nested_exact = true;
    let mut specs = Vec::with_capacity(m.entries.len());
    let mut srcs = Vec::with_capacity(m.entries.len());
    for me in &m.entries {
        let what = format!("entry {}", me.name);
        let src = match me.data {
            DataForm::None => Src::None,
            DataForm::Raw => {
                let path = member(dir, mpath, &what, &me.file)?;
                let len = file_len(&path, mpath, &me.name)?;
                Src::File(path, len)
            }
            DataForm::Inflated => {
                let path = member(dir, mpath, &what, &me.file)?;
                file_len(&path, mpath, &me.name)?;
                let (off, len) = spool.append(|w| deflate_file(&me.name, &path, w))?;
                Src::Spool(off, len)
            }
            DataForm::Container { deflated } => {
                let path = member(dir, mpath, &what, &me.file)?;
                if depth >= MAX_DEPTH {
                    return Err(manifest_err(
                        mpath,
                        format!("{what}: nested containers deeper than {MAX_DEPTH} levels"),
                    ));
                }
                if !path.is_dir() {
                    return Err(manifest_err(
                        mpath,
                        format!("{what}: {} is not a directory", path.display()),
                    ));
                }
                let (img, r) =
                    pack_vec_at(&path, &nested_opts, depth + 1, &sub_rel(rel, &me.file))?;
                nested_exact &= r.exact;
                report.warnings.extend(r.warnings);
                let (off, len) = spool.append(|w| {
                    if !deflated {
                        return Ok(w.write_all(&img)?);
                    }
                    let mut d =
                        zraw::DeflateWriter::new(w, zraw::LEVEL_1C).map_err(zlib_err(&me.name))?;
                    d.write_all(&img).map_err(|e| deflate_io(&me.name, e))?;
                    d.finish().map_err(|e| deflate_io(&me.name, e))?;
                    Ok(())
                })?;
                Src::Spool(off, len)
            }
        };
        let data_len = match src {
            Src::None => None,
            Src::File(_, n) | Src::Spool(_, n) => Some(n),
        };
        specs.push(EntrySpec {
            name: me.name.clone(),
            ctime: me.ctime,
            mtime: me.mtime,
            attrs: me.attrs,
            data_len,
        });
        srcs.push(src);
    }

    let format = match opts.format {
        Some(f) if depth == 0 => f,
        _ => m.format,
    };
    let layout = m.layout.take().map(|mut l| {
        l.header = m.header;
        l
    });
    let replay = match &layout {
        _ if format != m.format => Err(format!(
            "format {format:?} requested, the manifest has {:?}",
            m.format
        )),
        None => Err("no recorded layout".to_string()),
        Some(l) if l.format != m.format => Err(format!(
            "the recorded layout is {:?}, the manifest {:?}",
            l.format, m.format
        )),
        Some(l) => l.check(&specs).map_err(|e| match e {
            Error::LayoutMismatch(s) => format!("the recorded layout does not fit: {s}"),
            e => e.to_string(),
        }),
    };
    let mode = match (replay, layout) {
        (Ok(()), Some(layout)) => {
            let residue = match &m.residue {
                Some(f) => {
                    let path = member(dir, mpath, "residue", f)?;
                    fs::read(&path).map_err(|e| io_at(&path, e))?
                }
                None => Vec::new(),
            };
            let need = layout
                .patches
                .iter()
                .try_fold(0u64, |a, p| a.checked_add(p.len));
            if need != Some(residue.len() as u64) {
                return Err(manifest_err(
                    mpath,
                    format!(
                        "residue has {} bytes, the layout patches need {need:?}",
                        residue.len()
                    ),
                ));
            }
            let placeholder = match &m.placeholder {
                Some(f) => {
                    let path = member(dir, mpath, "placeholder", f)?;
                    let p = fs::read(&path).map_err(|e| io_at(&path, e))?;
                    if p.len() as u64 != layout.format.base() {
                        return Err(manifest_err(
                            mpath,
                            format!(
                                "placeholder has {} bytes, {:?} needs {}",
                                p.len(),
                                layout.format,
                                layout.format.base()
                            ),
                        ));
                    }
                    Some(p)
                }
                None => None,
            };
            Mode::Exact {
                layout,
                residue,
                placeholder,
            }
        }
        (why, layout) => {
            let why = why.err().unwrap_or_default();
            if opts.exact {
                return Err(Error::LayoutMismatch(note(rel, why)));
            }
            report
                .warnings
                .push(note(rel, format!("{why}; canonical writer")));
            let before = layout.as_ref().map_or(m.entries.len(), |l| l.entries.len());
            let storage_ver = if before == m.entries.len() {
                StorageVer::Fixed(m.header.storage_ver)
            } else if depth == 0 {
                StorageVer::EntryCount
            } else {
                WriteOptions::nested().storage_ver
            };
            Mode::Canonical(WriteOptions {
                format,
                toc_page_min: if depth == 0 {
                    format.default_toc_page()
                } else {
                    0
                },
                storage_ver,
            })
        }
    };
    report.exact = matches!(mode, Mode::Exact { .. }) && nested_exact;
    Ok(Prepared {
        specs,
        srcs,
        mode,
        report,
    })
}

fn prepare_plain(dir: &Path, opts: &PackOptions, depth: usize) -> Result<Prepared> {
    let mut files = Vec::new();
    for de in fs::read_dir(dir).map_err(|e| io_at(dir, e))? {
        let de = de.map_err(|e| io_at(dir, e))?;
        let path = de.path();
        let Ok(name) = de.file_name().into_string() else {
            return Err(manifest_err(&path, "file name is not valid Unicode"));
        };
        if name.starts_with('.') {
            continue;
        }
        let md = fs::metadata(&path).map_err(|e| io_at(&path, e))?;
        if md.is_dir() {
            return Err(manifest_err(
                &path,
                format!("a subdirectory, but {} has no {MANIFEST}", dir.display()),
            ));
        }
        if md.is_file() {
            files.push((name, path, md.len()));
        }
    }
    if opts.exact {
        return Err(Error::LayoutMismatch(format!(
            "{}: no {MANIFEST}, no layout to replay",
            dir.display()
        )));
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let t = if opts.now {
        V8Time::now()
    } else {
        V8Time::PACK_DEFAULT
    };
    let specs = files
        .iter()
        .map(|(name, _, len)| EntrySpec {
            name: name.clone(),
            ctime: t,
            mtime: t,
            attrs: 0,
            data_len: Some(*len),
        })
        .collect();
    let report = PackReport {
        entries: files.len(),
        exact: false,
        warnings: Vec::new(),
    };
    let srcs = files
        .into_iter()
        .map(|(_, path, len)| Src::File(path, len))
        .collect();
    let opts = if depth == 0 {
        WriteOptions::top_level(opts.format.unwrap_or(Format::F16))
    } else {
        WriteOptions::nested()
    };
    Ok(Prepared {
        specs,
        srcs,
        mode: Mode::Canonical(opts),
        report,
    })
}

fn deflate_file(entry: &str, path: &Path, w: &mut dyn Write) -> Result<()> {
    let mut f = File::open(path).map_err(|e| io_at(path, e))?;
    let mut d = zraw::DeflateWriter::new(w, zraw::LEVEL_1C).map_err(zlib_err(entry))?;
    let mut buf = vec![0u8; BUF];
    loop {
        let n = match f.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(io_at(path, e)),
        };
        d.write_all(buf.get(..n).unwrap_or_default())
            .map_err(|e| deflate_io(entry, e))?;
    }
    d.finish().map_err(|e| deflate_io(entry, e))?;
    Ok(())
}

fn emit(p: &Prepared, out: &mut dyn Write, spool: &Spool) -> Result<()> {
    let mut src = Sources {
        srcs: &p.srcs,
        spool,
        buf: vec![0u8; BUF],
    };
    match &p.mode {
        Mode::Exact {
            layout,
            residue,
            placeholder,
        } => write_exact(
            out,
            layout,
            residue,
            placeholder.as_deref(),
            &p.specs,
            &mut src,
        ),
        Mode::Canonical(o) => write_container(out, &p.specs, &mut src, o).map(|_| ()),
    }
}

struct Sources<'a> {
    srcs: &'a [Src],
    spool: &'a Spool,
    buf: Vec<u8>,
}

impl DataSource for Sources<'_> {
    fn write_data(&mut self, index: usize, out: &mut dyn Write) -> Result<()> {
        match self.srcs.get(index) {
            Some(Src::File(path, len)) => {
                let mut f = File::open(path).map_err(|e| io_at(path, e))?;
                let mut left = *len;
                while left > 0 {
                    let k = usize::try_from(left).map_or(self.buf.len(), |l| l.min(self.buf.len()));
                    let chunk = self.buf.get_mut(..k).unwrap_or_default();
                    let n = read_full(&mut f, chunk).map_err(|e| io_at(path, e))?;
                    if n == 0 {
                        return Err(manifest_err(path, "the file got shorter while packing"));
                    }
                    out.write_all(chunk.get(..n).unwrap_or_default())?;
                    left = left.saturating_sub(n as u64);
                }
                Ok(())
            }
            Some(Src::Spool(off, len)) => self.spool.copy(*off, *len, out, &mut self.buf),
            Some(Src::None) | None => Ok(()),
        }
    }
}

enum Spool {
    Mem(Vec<u8>),
    File {
        dir: PathBuf,
        file: Option<SpoolFile>,
    },
}

struct SpoolFile {
    w: BufWriter<File>,
    len: u64,
    tmp: TempPath,
}

struct Counter<'a> {
    w: &'a mut dyn Write,
    n: u64,
}

impl Write for Counter<'_> {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        let k = self.w.write(b)?;
        self.n = self.n.saturating_add(k as u64);
        Ok(k)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.w.flush()
    }
}

impl Spool {
    fn append(&mut self, f: impl FnOnce(&mut dyn Write) -> Result<()>) -> Result<(u64, u64)> {
        let sf = match self {
            Spool::Mem(v) => {
                let off = v.len() as u64;
                f(v)?;
                return Ok((off, (v.len() as u64).saturating_sub(off)));
            }
            Spool::File { dir, file } => match file {
                Some(sf) => sf,
                None => {
                    let (tmp, fh) = TempPath::create(dir, "spool")?;
                    file.insert(SpoolFile {
                        w: BufWriter::with_capacity(OUT_BUF, fh),
                        len: 0,
                        tmp,
                    })
                }
            },
        };
        let mut c = Counter { w: &mut sf.w, n: 0 };
        f(&mut c)?;
        let (off, n) = (sf.len, c.n);
        sf.len = off.saturating_add(n);
        Ok((off, n))
    }

    fn finish(&mut self) -> Result<()> {
        if let Spool::File { file: Some(sf), .. } = self {
            sf.w.flush().map_err(|e| io_at(sf.tmp.path(), e))?;
        }
        Ok(())
    }

    fn copy(&self, off: u64, len: u64, out: &mut dyn Write, buf: &mut [u8]) -> Result<()> {
        let bad = || Error::LayoutMismatch(format!("spool range {off}+{len} is out of bounds"));
        match self {
            Spool::Mem(v) => {
                let a = usize::try_from(off).map_err(|_| bad())?;
                let z = usize::try_from(off.saturating_add(len)).map_err(|_| bad())?;
                out.write_all(v.get(a..z).ok_or_else(bad)?)?;
            }
            Spool::File { file: Some(sf), .. } => {
                let mut done = 0u64;
                while done < len {
                    let k = usize::try_from(len - done).map_or(buf.len(), |l| l.min(buf.len()));
                    let chunk = buf.get_mut(..k).unwrap_or_default();
                    sf.w.get_ref()
                        .read_exact_at(chunk, off.saturating_add(done))
                        .map_err(|e| io_at(sf.tmp.path(), e))?;
                    out.write_all(chunk)?;
                    done = done.saturating_add(k as u64);
                }
            }
            Spool::File { file: None, .. } if len == 0 => {}
            Spool::File { file: None, .. } => return Err(bad()),
        }
        Ok(())
    }
}

static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

struct TempPath(Option<PathBuf>);

impl TempPath {
    fn create(dir: &Path, what: &str) -> Result<(TempPath, File)> {
        let pid = std::process::id();
        for _ in 0..64 {
            let n = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
            let p = dir.join(format!(".v8cont-{what}.{pid}.{n}.tmp"));
            match OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(&p)
            {
                Ok(f) => return Ok((TempPath(Some(p)), f)),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(io_at(&p, e)),
            }
        }
        Err(io_at(
            dir,
            io::Error::new(io::ErrorKind::AlreadyExists, "no free temporary file name"),
        ))
    }

    fn path(&self) -> &Path {
        self.0.as_deref().unwrap_or(Path::new(""))
    }

    fn persist(mut self, to: &Path) -> Result<()> {
        if let Some(p) = self.0.take()
            && let Err(e) = fs::rename(&p, to)
        {
            let _ = fs::remove_file(&p);
            return Err(io_at(to, e));
        }
        Ok(())
    }
}

impl Drop for TempPath {
    fn drop(&mut self) {
        if let Some(p) = self.0.take() {
            let _ = fs::remove_file(p);
        }
    }
}
