pub mod bounded;
pub mod bounded_io;

use super::{Discovery, ExternalBound, Visibility};
use std::{
    ffi::CString,
    fs::File,
    io::{self, Read},
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    os::unix::ffi::OsStrExt,
    path::{Component, Path, PathBuf},
};
#[derive(Clone, Debug)]
struct Mapping {
    mount: PathBuf,
    relative: PathBuf,
    root: PathBuf,
    v2: bool,
}
fn valid_path(s: &str) -> Result<PathBuf, String> {
    if !s.starts_with('/')
        || s.as_bytes().contains(&0)
        || s.split('/').any(|p| p == ".." || p == ".")
    {
        return Err("Unsafe cgroup path".into());
    }
    Ok(PathBuf::from(s))
}
fn decode(s: &str) -> Result<String, String> {
    let mut result = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            if i + 3 >= bytes.len()
                || !bytes[i + 1..i + 4]
                    .iter()
                    .all(|b| (b'0'..=b'7').contains(b))
            {
                return Err("Malformed mount escape".into());
            }
            let value = u16::from(bytes[i + 1] - b'0') * 64
                + u16::from(bytes[i + 2] - b'0') * 8
                + u16::from(bytes[i + 3] - b'0');
            let value = u8::try_from(value).map_err(|_| "Invalid mount escape")?;
            result.push(value);
            i += 4;
        } else {
            result.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(result).map_err(|_| "Non-UTF8 mount path".into())
}
fn mappings(membership: &str, mountinfo: &str) -> Result<Vec<Mapping>, String> {
    let mut members = Vec::new();
    for line in membership.lines() {
        let mut f = line.splitn(3, ':');
        let _id = f.next().ok_or("Malformed cgroup membership")?;
        let controllers = f.next().ok_or("Malformed cgroup membership")?;
        let path = f.next().ok_or("Malformed cgroup membership")?;
        if controllers.is_empty() || controllers.split(',').any(|v| v == "memory") {
            members.push((valid_path(path)?, controllers.is_empty()));
        }
    }
    let mut result = Vec::new();
    for line in mountinfo.lines() {
        let Some((left, right)) = line.split_once(" - ") else {
            return Err("Malformed mountinfo".into());
        };
        let fields = left.split_ascii_whitespace().collect::<Vec<_>>();
        let fs = right.split_ascii_whitespace().collect::<Vec<_>>();
        if fields.len() < 6 || fs.len() < 3 {
            return Err("Malformed mountinfo fields".into());
        }
        let v2 = fs[0] == "cgroup2";
        if !v2 && !(fs[0] == "cgroup" && fs[2].split(',').any(|v| v == "memory")) {
            continue;
        }
        let root = valid_path(&decode(fields[3])?)?;
        let mount = valid_path(&decode(fields[4])?)?;
        for (p, is_v2) in &members {
            if *is_v2 != v2 {
                continue;
            }
            if let Ok(relative) = p.strip_prefix(&root) {
                result.push(Mapping {
                    mount: mount.clone(),
                    relative: relative.to_path_buf(),
                    root: root.clone(),
                    v2,
                });
            }
        }
    }
    Ok(result)
}
fn parse_limit(text: &str, v2: bool) -> Result<Option<u64>, String> {
    let text = text.trim_matches(|c: char| c.is_ascii_whitespace());
    if v2 && text == "max" {
        return Ok(None);
    }
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return Err("Malformed kernel memory limit".into());
    }
    let n = text.parse::<u64>().map_err(|_| "Kernel limit overflow")?;
    Ok(Some(n))
}
fn kernel_limit(reader: &impl KernelReader, text: &str, v2: bool) -> Result<Option<u64>, String> {
    let value = parse_limit(text, v2)?;
    if !v2 && value.is_some() && value == reader.v1_unlimited_sentinel() {
        Ok(None)
    } else {
        Ok(value)
    }
}
pub trait KernelReader {
    fn v1_unlimited_sentinel(&self) -> Option<u64> {
        None
    }
    fn proc_file(&self, name: &str) -> io::Result<String>;
    fn cgroup_file(&self, mount: &Path, relative: &Path, name: &str) -> io::Result<String>;
}
pub struct NativeReader;
fn read_bounded(file: File) -> io::Result<String> {
    let mut data = String::new();
    file.take(1024 * 1024 + 1).read_to_string(&mut data)?;
    if data.len() > 1024 * 1024 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Kernel file too large",
        ));
    }
    Ok(data)
}
fn openat(parent: i32, name: &std::ffi::OsStr, directory: bool) -> io::Result<OwnedFd> {
    let name = CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL path"))?;
    let flags = libc::O_RDONLY
        | libc::O_CLOEXEC
        | libc::O_NOFOLLOW
        | if directory { libc::O_DIRECTORY } else { 0 };
    let fd = unsafe { libc::openat(parent, name.as_ptr(), flags) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}
impl KernelReader for NativeReader {
    fn v1_unlimited_sentinel(&self) -> Option<u64> {
        let mut name = std::mem::MaybeUninit::<libc::utsname>::uninit();
        if unsafe { libc::uname(name.as_mut_ptr()) } != 0 {
            return None;
        }
        let name = unsafe { name.assume_init() };
        let machine = unsafe { std::ffi::CStr::from_ptr(name.machine.as_ptr()) }.to_bytes();
        if ![
            b"x86_64".as_slice(),
            b"aarch64".as_slice(),
            b"ppc64".as_slice(),
            b"ppc64le".as_slice(),
            b"s390x".as_slice(),
            b"riscv64".as_slice(),
        ]
        .contains(&machine)
        {
            return None;
        }
        let page = u64::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).ok()?;
        if page == 0 || !page.is_power_of_two() {
            return None;
        }
        Some((i64::MAX as u64) / page * page)
    }
    fn proc_file(&self, name: &str) -> io::Result<String> {
        read_bounded(File::open(Path::new("/proc").join(name))?)
    }
    fn cgroup_file(&self, mount: &Path, relative: &Path, name: &str) -> io::Result<String> {
        let mut anchor = openat(libc::AT_FDCWD, std::ffi::OsStr::new("/"), true)?;
        for c in mount.components().chain(relative.components()) {
            match c {
                Component::Normal(n) => anchor = openat(anchor.as_raw_fd(), n, true)?,
                Component::RootDir => {}
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "Unsafe path component",
                    ));
                }
            }
        }
        let file = openat(anchor.as_raw_fd(), std::ffi::OsStr::new(name), false)?;
        read_bounded(File::from(file))
    }
}
fn partial(d: &mut Discovery, message: impl Into<String>) {
    d.visibility = Visibility::Partial;
    d.warnings.push(message.into())
}
fn add_limit(
    reader: &impl KernelReader,
    m: &Mapping,
    relative: &Path,
    name: &str,
    root: bool,
    d: &mut Discovery,
    soft: bool,
) {
    match reader.cgroup_file(&m.mount, relative, name) {
        Ok(s) => match kernel_limit(reader, &s, m.v2) {
            Ok(n) => {
                let source = format!("{}:{}", m.mount.join(relative).display(), name);
                if soft {
                    if let Some(n) = n {
                        d.soft_limit = Some(d.soft_limit.map_or(n, |old| old.min(n)));
                    }
                } else {
                    if let Some(bytes) = n {
                        d.bounds.push(ExternalBound { source, bytes });
                    } else {
                        d.confirmed_unlimited.push(source);
                    }
                }
            }
            Err(e) => partial(d, format!("{name}: {e}")),
        },
        Err(e) if root && e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => partial(
            d,
            format!(
                "{} {}: {:?}",
                m.mount.join(relative).display(),
                name,
                e.kind()
            ),
        ),
    }
}
fn snapshot(reader: &impl KernelReader, membership: &str) -> Discovery {
    let mut d = Discovery {
        visibility: Visibility::Visible,
        membership: membership.into(),
        ..Discovery::default()
    };
    let mounts = match reader.proc_file("self/mountinfo") {
        Ok(v) => v,
        Err(e) => {
            partial(&mut d, format!("mountinfo: {:?}", e.kind()));
            return d;
        }
    };
    let mappings = match mappings(membership, &mounts) {
        Ok(v) => v,
        Err(e) => {
            partial(&mut d, e);
            return d;
        }
    };
    if mappings.is_empty() {
        partial(&mut d, "No proven memory controller mount mapping");
    }
    let has_v1 = mappings.iter().any(|m| !m.v2);
    for m in mappings.iter().filter(|m| !has_v1 || !m.v2) {
        if m.root != Path::new("/") {
            partial(&mut d, "Mount exposes only a subtree; ancestors are hidden");
        }
        let mut relative = m.relative.clone();
        loop {
            let root = relative.as_os_str().is_empty();
            if m.v2 {
                add_limit(reader, m, &relative, "memory.max", root, &mut d, false);
                add_limit(reader, m, &relative, "memory.high", root, &mut d, true);
            } else {
                let hierarchy = reader.cgroup_file(&m.mount, &relative, "memory.use_hierarchy");
                if relative != m.relative && !matches!(&hierarchy,Ok(v) if v.trim()=="1") {
                    partial(
                        &mut d,
                        "v1 ancestor enforcement unconfirmed; bound is conservative policy ceiling",
                    );
                }
                add_limit(
                    reader,
                    m,
                    &relative,
                    "memory.limit_in_bytes",
                    root,
                    &mut d,
                    false,
                );
                match reader.cgroup_file(&m.mount, &relative, "memory.memsw.limit_in_bytes") {
                    Ok(s) => match kernel_limit(reader, &s, false) {
                        Ok(Some(bytes)) => d.bounds.push(ExternalBound {
                            source: format!(
                                "{}:memory.memsw.limit_in_bytes",
                                m.mount.join(&relative).display()
                            ),
                            bytes,
                        }),
                        Ok(None) => d.confirmed_unlimited.push(format!(
                            "{}:memory.memsw.limit_in_bytes",
                            m.mount.join(&relative).display()
                        )),
                        Err(e) => partial(&mut d, e),
                    },
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(e) => partial(
                        &mut d,
                        format!("memory.memsw.limit_in_bytes: {:?}", e.kind()),
                    ),
                }
                match reader.cgroup_file(&m.mount, &relative, "memory.stat") {
                    Ok(s) => {
                        for line in s.lines() {
                            let mut f = line.split_ascii_whitespace();
                            if f.next() == Some("hierarchical_memory_limit") {
                                match f.next().and_then(|n| kernel_limit(reader, n, false).ok()) {
                                    Some(Some(bytes)) => d.bounds.push(ExternalBound {
                                        source: format!(
                                            "{}:hierarchical_memory_limit",
                                            m.mount.join(&relative).display()
                                        ),
                                        bytes,
                                    }),
                                    Some(None) => d.confirmed_unlimited.push(format!(
                                        "{}:hierarchical_memory_limit",
                                        m.mount.join(&relative).display()
                                    )),
                                    None => partial(&mut d, "Malformed hierarchical_memory_limit"),
                                }
                            }
                        }
                    }
                    Err(e) if root && e.kind() == io::ErrorKind::NotFound => {}
                    Err(e) => partial(&mut d, format!("memory.stat: {:?}", e.kind())),
                }
            }
            if root {
                break;
            }
            if !relative.pop() {
                break;
            }
        }
    }
    if let Ok(meminfo) = reader.proc_file("meminfo") {
        for line in meminfo.lines() {
            let f = line.split_ascii_whitespace().collect::<Vec<_>>();
            if f.first() == Some(&"MemTotal:") && f.get(2) == Some(&"kB") {
                d.physical_memory = f
                    .get(1)
                    .and_then(|s| s.parse::<u64>().ok())
                    .and_then(|n| n.checked_mul(1024));
            }
        }
    }
    d
}
pub fn discover_with(reader: &impl KernelReader) -> Discovery {
    let mut last = Discovery::default();
    for attempt in 0..2 {
        let membership = match reader.proc_file("self/cgroup") {
            Ok(v) => v,
            Err(e) => {
                partial(&mut last, format!("membership: {:?}", e.kind()));
                return last;
            }
        };
        let mut d = snapshot(reader, &membership);
        match reader.proc_file("self/cgroup") {
            Ok(after) if after == membership => {
                d.membership_confirmed = true;
                return d;
            }
            _ => {
                partial(
                    &mut d,
                    "Cgroup membership changed or could not be confirmed during discovery",
                );
                last = d;
                if attempt == 1 {
                    return last;
                }
            }
        }
    }
    last
}
pub fn discover() -> Discovery {
    discover_with(&NativeReader)
}
