use crate::Application;
use bsl_vm::HostError;
use std::collections::HashSet;
use std::io::{self, BufRead, BufReader, Read};

const MAX_DEPTH: usize = 16;

const MAX_INLINE: usize = 8 << 20;

pub(crate) fn storage_refusal(app: &Application, bytes: &[u8]) -> Option<HostError> {
    let mut known = Known::default();
    match check_form(app, &mut known, &mut &bytes[..], 0) {
        Ok(()) => None,
        Err(Stop::Refused(e)) => Some(e),
        Err(Stop::Unreadable) => None,
    }
}

enum Stop {
    Refused(HostError),
    Unreadable,
}

impl From<io::Error> for Stop {
    fn from(_: io::Error) -> Stop {
        Stop::Unreadable
    }
}

#[derive(Default)]
struct Known {
    refs: Option<HashSet<[u8; 16]>>,
}

impl Known {
    fn is_ref(&mut self, app: &Application, g: &[u8; 16]) -> bool {
        self.refs
            .get_or_insert_with(|| {
                app.model()
                    .objects
                    .iter()
                    .flat_map(|o| o.generated.iter())
                    .filter(|g| g.category == "Ref")
                    .map(|g| *g.type_id.as_bytes())
                    .collect()
            })
            .contains(g)
    }
}

fn refused(type_name: &str, member: &str) -> Stop {
    Stop::Refused(HostError::not_implemented(type_name, member))
}

fn check_form(
    app: &Application,
    known: &mut Known,
    r: &mut dyn Read,
    depth: usize,
) -> Result<(), Stop> {
    if depth > MAX_DEPTH {
        return Err(refused(
            "ХранилищеЗначения",
            "Получить: вложенность хранилищ",
        ));
    }
    let mut kind = [0u8; 2];
    match r.read_exact(&mut kind) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
        Err(e) => return Err(e.into()),
    }
    match kind {
        [1, 1] => scan(app, known, &mut BufReader::new(r), depth),
        [2, 1] => {
            let mut signature = [0u8; 16];
            r.read_exact(&mut signature)?;
            let inflate = zraw::InflateReader::new(r).map_err(|_| Stop::Unreadable)?;
            scan(app, known, &mut BufReader::new(inflate), depth)
        }
        _ => Ok(()),
    }
}

enum State {
    Prefix(usize),
    Guid(Vec<u8>),
    StorageOpen,
    StorageAtom(Vec<u8>),
}

const PREFIX: &[u8] = b"{\"#\",";

fn scan(
    app: &Application,
    known: &mut Known,
    r: &mut dyn BufRead,
    depth: usize,
) -> Result<(), Stop> {
    let mut len = [0u8; 8];
    r.read_exact(&mut len)?;
    let len = u64::from_le_bytes(len);
    let storage_type = platform_catalog::catalog().find_type("ХранилищеЗначения");
    let mut streams: Vec<u32> = Vec::new();
    let mut state = State::Prefix(0);
    let mut text = (&mut *r).take(len);
    let mut buf = [0u8; 8192];
    loop {
        let n = text.read(&mut buf)?;
        if n == 0 {
            break;
        }
        for &b in buf.get(..n).unwrap_or_default() {
            state = match state {
                State::Prefix(k) if PREFIX.get(k) == Some(&b) => {
                    if k + 1 == PREFIX.len() {
                        State::Guid(Vec::with_capacity(37))
                    } else {
                        State::Prefix(k + 1)
                    }
                }
                State::Prefix(_) => State::Prefix(usize::from(b == b'{')),
                State::Guid(mut g) if g.len() < 36 => {
                    g.push(b);
                    State::Guid(g)
                }
                State::Guid(g) => {
                    if b != b',' {
                        State::Prefix(usize::from(b == b'{'))
                    } else {
                        let guid = guid_of(&g).ok_or_else(|| {
                            refused("ХранилищеЗначения", "Получить: тип значения не определён")
                        })?;
                        match platform_catalog::catalog().find_type_by_guid(&guid) {
                            Some(idx) if Some(idx) == storage_type => State::StorageOpen,
                            Some(idx) if bsl_stdlib::serial::codec::codec_of(idx).is_some() => {
                                State::Prefix(0)
                            }
                            Some(idx) => {
                                return Err(refused(
                                    &platform_catalog::catalog().type_info(idx).name_ru,
                                    "значение в ХранилищеЗначения",
                                ));
                            }
                            None if known.is_ref(app, &guid) => State::Prefix(0),
                            None => {
                                return Err(refused(
                                    "ХранилищеЗначения",
                                    &format!(
                                        "Получить: значение типа {}",
                                        String::from_utf8_lossy(&g)
                                    ),
                                ));
                            }
                        }
                    }
                }
                State::StorageOpen if b == b'{' => State::StorageAtom(Vec::new()),
                State::StorageOpen if b.is_ascii_whitespace() => State::StorageOpen,
                State::StorageOpen => return Err(Stop::Unreadable),
                State::StorageAtom(a) if b == b'}' => {
                    nested(app, known, &a, &mut streams, depth)?;
                    State::Prefix(0)
                }
                State::StorageAtom(mut a) => {
                    if !b.is_ascii_whitespace() {
                        if a.len() >= MAX_INLINE {
                            return Err(refused(
                                "ХранилищеЗначения",
                                "Получить: вложенная форма в тексте больше 8 МиБ",
                            ));
                        }
                        a.push(b);
                    }
                    State::StorageAtom(a)
                }
            };
        }
    }
    if text.limit() != 0 {
        return Err(Stop::Unreadable);
    }
    if streams.is_empty() {
        return Ok(());
    }
    loop {
        let mut head = [0u8; 12];
        match r.read_exact(&mut head) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(e) => return Err(e.into()),
        }
        let (num, size) = head.split_at(4);
        let num = u32::from_le_bytes(num.try_into().map_err(|_| Stop::Unreadable)?);
        let size = u64::from_le_bytes(size.try_into().map_err(|_| Stop::Unreadable)?);
        let mut part = (&mut *r).take(size);
        if streams.contains(&num) {
            check_form(app, known, &mut part, depth + 1)?;
        }
        io::copy(&mut part, &mut io::sink())?;
    }
}

fn nested(
    app: &Application,
    known: &mut Known,
    atom: &[u8],
    streams: &mut Vec<u32>,
    depth: usize,
) -> Result<(), Stop> {
    let atom = std::str::from_utf8(atom).map_err(|_| Stop::Unreadable)?;
    if atom.starts_with("#base64:") {
        let form = brace::Node::atom(atom).base64().ok_or(Stop::Unreadable)?;
        return check_form(app, known, &mut &form[..], depth + 1);
    }
    let n = atom.parse::<u32>().map_err(|_| Stop::Unreadable)?;
    streams.push(n);
    Ok(())
}

fn guid_of(text: &[u8]) -> Option<[u8; 16]> {
    if text.len() != 36 {
        return None;
    }
    let mut out = [0u8; 16];
    let mut n = 0usize;
    for (i, c) in text.iter().enumerate() {
        if matches!(i, 8 | 13 | 18 | 23) {
            if *c != b'-' {
                return None;
            }
            continue;
        }
        let d = u8::try_from(char::from(*c).to_digit(16)?).ok()?;
        let slot = out.get_mut(n / 2)?;
        *slot |= if n.is_multiple_of(2) { d << 4 } else { d };
        n += 1;
    }
    Some(out)
}
