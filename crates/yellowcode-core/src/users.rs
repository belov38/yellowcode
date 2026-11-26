use mdconf::{Configuration, ConfigurationExt, LoadOptions, LoadProfile, MdKind};
use onecd::{BlobVal, Database, Locking, OpenOptions, Value};
use std::{collections::BTreeMap, io::Write, ops::ControlFlow, path::Path};
use unicode_normalization::UnicodeNormalization;
pub use v8users::card::{Authentication, Card, Role, Separation};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub rows: usize,
    pub total_data: usize,
    pub collections: usize,
    pub json: usize,
}

pub const WRITE_LIMITS: Limits = Limits {
    rows: 10_000,
    total_data: 64 * 1_048_576,
    collections: 100_000,
    json: 32 * 1_048_576,
};

pub const VIEWER_LIMITS: Limits = Limits {
    rows: 100_000,
    total_data: 256 * 1_048_576,
    collections: 10_000_000,
    json: 256 * 1_048_576,
};

const MAX_USERS: usize = WRITE_LIMITS.rows;
const MAX_TOTAL_DATA: usize = WRITE_LIMITS.total_data;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitKind {
    Rows,
    RowData,
    TotalData,
    Collections,
    Json,
}

impl LimitKind {
    pub fn of(self, limits: &Limits) -> usize {
        match self {
            LimitKind::Rows => limits.rows,
            LimitKind::RowData => v8users::MAX_DATA_BYTES,
            LimitKind::TotalData => limits.total_data,
            LimitKind::Collections => limits.collections,
            LimitKind::Json => limits.json,
        }
    }

    pub fn describe(self, limit: usize) -> String {
        match self {
            LimitKind::Rows => format!("V8USERS has more than {limit} rows (limit {limit})"),
            LimitKind::RowData => {
                format!("DATA of a V8USERS row is larger than {limit} bytes (limit {limit})")
            }
            LimitKind::TotalData => {
                format!("DATA of all V8USERS rows is larger than {limit} bytes (limit {limit})")
            }
            LimitKind::Collections => format!(
                "users have more than {limit} roles and data separation values in total (limit {limit})"
            ),
            LimitKind::Json => {
                format!("the JSON of all users is larger than {limit} bytes (limit {limit})")
            }
        }
    }

    pub fn write_text(self) -> &'static str {
        match self {
            LimitKind::Rows => "V8USERS has more than 10000 rows (limit 10000)",
            LimitKind::RowData => {
                "DATA of a V8USERS row is larger than 1048576 bytes (limit 1048576)"
            }
            LimitKind::TotalData => {
                "DATA of all V8USERS rows is larger than 67108864 bytes (limit 67108864)"
            }
            LimitKind::Collections => {
                "users have more than 100000 roles and data separation values in total (limit 100000)"
            }
            LimitKind::Json => {
                "the JSON of all users is larger than 33554432 bytes (limit 33554432)"
            }
        }
    }
}

struct OutputBudget {
    remaining: usize,
    items: usize,
    max_items: usize,
}

impl OutputBudget {
    fn new() -> Self {
        Self::within(&WRITE_LIMITS)
    }

    fn within(limits: &Limits) -> Self {
        Self {
            remaining: limits.json - 2,
            items: 0,
            max_items: limits.collections,
        }
    }

    fn admit(&mut self, card: &Card) -> Result<(), LimitKind> {
        self.items = self
            .items
            .checked_add(card.roles.len())
            .and_then(|n| n.checked_add(card.data_separation.len()))
            .filter(|n| *n <= self.max_items)
            .ok_or(LimitKind::Collections)?;
        serde_json::to_writer(&mut *self, card).map_err(|_| LimitKind::Json)?;
        self.write_all(b",").map_err(|_| LimitKind::Json)
    }
}

impl Write for OutputBudget {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.remaining = self
            .remaining
            .checked_sub(bytes.len())
            .ok_or_else(|| std::io::Error::other("users output limit"))?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("no such infobase user")]
    NotFound,
    #[error("several users have this name; select the UUID")]
    Ambiguous,
    #[error(
        "infobase user data exceeds the reading limit: {}{}",
        row.map(|r| format!("V8USERS row {r}: ")).unwrap_or_default(),
        what.describe(*limit)
    )]
    Limit {
        what: LimitKind,
        limit: usize,
        row: Option<usize>,
    },
    #[error("V8USERS DATA row {row}: {source}")]
    Data { row: usize, source: v8users::Error },
    #[error("V8USERS DATA column is missing or malformed")]
    Column,
    #[error(transparent)]
    Storage(#[from] onecd::Error),
    #[error("reading users configuration: {0}")]
    Configuration(String),
}

impl Error {
    fn limit(what: LimitKind, limits: &Limits, row: Option<usize>) -> Self {
        Error::Limit {
            what,
            limit: what.of(limits),
            row,
        }
    }

    pub fn limit_text(&self) -> Option<&'static str> {
        match self {
            Error::Limit { what, limit, .. } if *limit == what.of(&WRITE_LIMITS) => {
                Some(what.write_text())
            }
            _ => None,
        }
    }
}

enum NameKey {
    Legacy(Vec<u8>),
    Paid(memory_budget::BudgetedVec<u8>),
}

impl NameKey {
    fn bytes(&self) -> &[u8] {
        match self {
            NameKey::Legacy(key) => key,
            NameKey::Paid(key) => key.as_slice(),
        }
    }
}

fn name_key(name: &str, request: Option<&onecd::KeyRequest>) -> Result<NameKey, Error> {
    use onecd::collate::{Collate, IcuCollate, Strength};
    let name = name.trim_end_matches(' ');
    let Some(request) = request else {
        let units = name
            .nfc()
            .collect::<String>()
            .encode_utf16()
            .collect::<Vec<_>>();
        return Ok(NameKey::Legacy(IcuCollate.sort_key(
            "root",
            Strength::Secondary,
            &units,
        )?));
    };
    let text = paid_nfc_utf16(name, |bytes| {
        request.try_reserve(u64::try_from(bytes).unwrap_or(u64::MAX))
    })?;
    Ok(NameKey::Paid(request.sort_key(
        "root",
        Strength::Secondary,
        &text.units,
    )?))
}

struct PaidUnits<R> {
    units: Vec<u16>,
    _paid: R,
}

fn paid_nfc_utf16<R>(
    text: &str,
    mut admit: impl FnMut(usize) -> onecd::Result<R>,
) -> onecd::Result<PaidUnits<R>> {
    use unicode_normalization::char::{canonical_combining_class, compose, decompose_canonical};
    let (mut len, mut run, mut last, mut unsorted, mut longest) =
        (0usize, 0usize, 0u8, false, 0usize);
    for c in text.chars() {
        decompose_canonical(c, |d| {
            len += 1;
            let class = canonical_combining_class(d);
            if class == 0 {
                (run, last, unsorted) = (0, 0, false);
            } else {
                run += 1;
                unsorted |= class < last;
                last = class;
                if unsorted {
                    longest = longest.max(run);
                }
            }
        });
    }
    let size = std::mem::size_of::<char>();
    let decomposition = admit(len.saturating_mul(size))?;
    let mut chars = Vec::with_capacity(len);
    for c in text.chars() {
        decompose_canonical(c, |d| chars.push(d));
    }
    let ordering = admit(longest.saturating_mul(size))?;
    let mut scratch: Vec<char> = Vec::with_capacity(longest);
    let mut start = 0;
    while start < chars.len() {
        let Some(&c) = chars.get(start) else { break };
        if canonical_combining_class(c) == 0 {
            start += 1;
            continue;
        }
        let end = chars
            .get(start..)
            .and_then(|rest| rest.iter().position(|&c| canonical_combining_class(c) == 0))
            .map_or(chars.len(), |n| start + n);
        if let Some(seg) = chars.get_mut(start..end)
            && seg.windows(2).any(|w| {
                matches!(w, [a, b] if canonical_combining_class(*b) < canonical_combining_class(*a))
            })
        {
            scratch.clear();
            for class in 1..=u8::MAX {
                scratch.extend(seg.iter().filter(|&&c| canonical_combining_class(c) == class));
            }
            seg.copy_from_slice(&scratch);
        }
        start = end;
    }
    drop(scratch);
    let mut starter: Option<usize> = None;
    let mut blocking: Option<u8> = None;
    let mut written = 0;
    for read in 0..chars.len() {
        let Some(&c) = chars.get(read) else { break };
        let class = canonical_combining_class(c);
        if let Some(s) = starter
            && blocking.is_none_or(|b| b < class)
            && let Some(composed) = chars.get(s).and_then(|&k| compose(k, c))
        {
            if let Some(k) = chars.get_mut(s) {
                *k = composed;
            }
            continue;
        }
        if class == 0 {
            starter = Some(written);
            blocking = None;
        } else if starter.is_some() {
            blocking = Some(class);
        }
        if let Some(slot) = chars.get_mut(written) {
            *slot = c;
        }
        written += 1;
    }
    chars.truncate(written);
    let units = chars.iter().map(|c| c.len_utf16()).sum::<usize>();
    let output = admit(units.saturating_mul(std::mem::size_of::<u16>()))?;
    let mut out = Vec::with_capacity(units);
    let mut buf = [0u16; 2];
    for c in &chars {
        out.extend_from_slice(c.encode_utf16(&mut buf));
    }
    drop(chars);
    drop((decomposition, ordering));
    Ok(PaidUnits {
        units: out,
        _paid: output,
    })
}

pub fn valid_id(id: &str) -> bool {
    brace::Guid::parse(id).is_some()
}

pub fn find<'a>(cards: &'a [Card], identity: &str) -> Result<&'a Card, Error> {
    if let Some(id) = brace::Guid::parse(identity) {
        return cards
            .iter()
            .find(|c| c.id == id.to_string())
            .ok_or(Error::NotFound);
    }
    let request = onecd::KeyRequest::when_bound(None);
    let key = name_key(identity, request.as_ref())?;
    let mut found = None;
    for card in cards {
        if name_key(&card.name, request.as_ref())?.bytes() == key.bytes() {
            if found.is_some() {
                return Err(Error::Ambiguous);
            }
            found = Some(card);
        }
    }
    found.ok_or(Error::NotFound)
}

fn open(base: &Path) -> Result<Database, Error> {
    let mut options = OpenOptions::read_only();
    options.locking = Locking::None;
    Ok(Database::open(
        &crate::viewer::source::resolve(base),
        options,
    )?)
}

fn role_names(txn: &onecd::ReadTxn) -> Result<BTreeMap<String, String>, Error> {
    let src = infobase::config::ConfigTxnSource::read(txn, "CONFIG")
        .map_err(|e| Error::Configuration(e.to_string()))?;
    if src.is_empty() {
        return Ok(BTreeMap::new());
    }
    let cfg = Configuration::load(
        &src,
        &LoadOptions {
            keep_source: false,
            profile: LoadProfile::Query,
            ..Default::default()
        },
    )
    .map_err(|e| Error::Configuration(e.to_string()))?;
    Ok(role_map(&cfg)
        .into_iter()
        .map(|(id, name)| (id, name.to_owned()))
        .collect())
}

fn role_map(cfg: &Configuration) -> BTreeMap<String, &str> {
    cfg.objects
        .iter()
        .filter(|o| o.kind == MdKind::Role)
        .map(|o| (o.id.to_string(), o.name.as_str()))
        .collect()
}

pub fn list(base: &Path) -> Result<Vec<Card>, Error> {
    let db = open(base)?;
    let txn = db.read();
    let mut cards = read_cards_within(&txn, &VIEWER_LIMITS)?;
    if cards.iter().any(|c| !c.roles.is_empty()) {
        let src = infobase::config::ConfigTxnSource::read(&txn, "CONFIG")
            .map_err(|e| Error::Configuration(e.to_string()))?;
        if !src.is_empty() {
            let cfg = Configuration::load(
                &src,
                &LoadOptions {
                    keep_source: false,
                    profile: LoadProfile::Query,
                    ..Default::default()
                },
            )
            .map_err(|e| Error::Configuration(e.to_string()))?;
            resolve_roles_within(&mut cards, &cfg, &VIEWER_LIMITS)?;
        }
    }
    Ok(cards)
}

pub fn list_with_configuration(base: &Path, cfg: &Configuration) -> Result<Vec<Card>, Error> {
    let db = open(base)?;
    let mut cards = read_cards_within(&db.read(), &VIEWER_LIMITS)?;
    resolve_roles_within(&mut cards, cfg, &VIEWER_LIMITS)?;
    Ok(cards)
}

pub fn resolve_roles(cards: &mut [Card], cfg: &Configuration) -> Result<(), Error> {
    resolve_roles_within(cards, cfg, &WRITE_LIMITS)
}

fn resolve_roles_within(
    cards: &mut [Card],
    cfg: &Configuration,
    limits: &Limits,
) -> Result<(), Error> {
    let roles = role_map(cfg);
    let refused = |what| Error::limit(what, limits, None);
    let mut names = OutputBudget::within(limits);
    for role in cards.iter().flat_map(|card| &card.roles) {
        if let Some(name) = roles.get(&role.id) {
            serde_json::to_writer(&mut names, name).map_err(|_| refused(LimitKind::Json))?;
        }
    }
    for card in cards.iter_mut() {
        for role in &mut card.roles {
            role.name = roles.get(&role.id).map(|n| (*n).to_owned());
        }
    }
    let mut output = OutputBudget::within(limits);
    for card in cards.iter() {
        output.admit(card).map_err(refused)?;
    }
    Ok(())
}

fn read_cards(txn: &onecd::ReadTxn) -> Result<Vec<Card>, Error> {
    read_cards_within(txn, &WRITE_LIMITS)
}

fn read_cards_within(txn: &onecd::ReadTxn, limits: &Limits) -> Result<Vec<Card>, Error> {
    let mut cards = Vec::new();
    let mut total = 0usize;
    let mut output = OutputBudget::within(limits);
    let refused = |what| Error::limit(what, limits, None);
    scan_cards(
        txn,
        Some(limits),
        |len| {
            total = total
                .checked_add(len)
                .filter(|n| *n <= limits.total_data)
                .ok_or_else(|| refused(LimitKind::TotalData))?;
            Ok(())
        },
        |card| {
            output.admit(&card).map_err(refused)?;
            cards.push(card);
            Ok(ControlFlow::Continue(()))
        },
    )?;
    Ok(cards)
}

fn scan_cards(
    txn: &onecd::ReadTxn,
    limits: Option<&Limits>,
    mut admit: impl FnMut(usize) -> Result<(), Error>,
    mut each: impl FnMut(Card) -> Result<ControlFlow<()>, Error>,
) -> Result<(), Error> {
    let table = txn.table("V8USERS")?;
    let column = table
        .descr()
        .field("DATA")
        .map(|(i, _)| i)
        .ok_or(Error::Column)?;
    for (row_index, row) in table.scan().enumerate() {
        if let Some(limits) = limits
            && row_index >= limits.rows
        {
            return Err(Error::limit(LimitKind::Rows, limits, None));
        }
        let (_, row) = row?;
        let value = row.get(column).ok_or(Error::Column)?;
        let len = match value {
            Value::Image(BlobVal::Ref(r)) => r.len as usize,
            Value::Image(BlobVal::Data(b)) | Value::Binary(b) => b.len(),
            _ => return Err(Error::Column),
        };
        admit(len)?;
        if len > v8users::MAX_DATA_BYTES {
            return Err(Error::limit(
                LimitKind::RowData,
                &WRITE_LIMITS,
                Some(row_index),
            ));
        }
        let blob = match value {
            Value::Image(BlobVal::Ref(r)) => table.blob_bytes(*r)?,
            Value::Image(BlobVal::Data(b)) | Value::Binary(b) => b.clone(),
            _ => return Err(Error::Column),
        };
        let card = Card::from_blob(&blob).map_err(|source| Error::Data {
            row: row_index,
            source,
        })?;
        drop(blob);
        if !card.is_service() && each(card)?.is_break() {
            break;
        }
    }
    Ok(())
}

pub fn for_each(
    base: &Path,
    named: bool,
    mut each: impl FnMut(Card) -> Result<ControlFlow<()>, Error>,
) -> Result<(), Error> {
    let db = open(base)?;
    let txn = db.read();
    let mut names: Option<BTreeMap<String, String>> = None;
    scan_cards(
        &txn,
        None,
        |_| Ok(()),
        |mut card| {
            if named && !card.roles.is_empty() {
                let names = match &mut names {
                    Some(names) => names,
                    none => none.insert(role_names(&txn)?),
                };
                name_roles(&mut card, names);
            }
            each(card)
        },
    )
}

fn name_roles(card: &mut Card, names: &BTreeMap<String, String>) {
    for role in &mut card.roles {
        role.name = names.get(&role.id).cloned();
    }
}

pub fn find_in(base: &Path, identity: &str, named: bool) -> Result<Card, Error> {
    let db = open(base)?;
    let txn = db.read();
    let id = brace::Guid::parse(identity).map(|id| id.to_string());
    let request = id
        .is_none()
        .then(|| onecd::KeyRequest::when_bound(None))
        .flatten();
    let key = match id {
        Some(_) => None,
        None => Some(name_key(identity, request.as_ref())?),
    };
    let mut found = None;
    scan_cards(
        &txn,
        None,
        |_| Ok(()),
        |card| {
            let hit = match (&id, &key) {
                (Some(id), _) => card.id == *id,
                (None, Some(key)) => name_key(&card.name, request.as_ref())?.bytes() == key.bytes(),
                (None, None) => false,
            };
            if !hit {
                return Ok(ControlFlow::Continue(()));
            }
            if found.is_some() {
                return Err(Error::Ambiguous);
            }
            found = Some(card);
            Ok(if id.is_some() {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            })
        },
    )?;
    let mut card = found.ok_or(Error::NotFound)?;
    if named && !card.roles.is_empty() {
        name_roles(&mut card, &role_names(&txn)?);
    }
    Ok(card)
}

pub fn write_all(base: &Path, json: bool, named: bool, out: &mut dyn Write) -> crate::Result<()> {
    use serde::ser::{SerializeSeq, Serializer};
    if json {
        let mut failed = None;
        {
            let mut ser = serde_json::Serializer::pretty(&mut *out);
            let mut seq = ser.serialize_seq(None)?;
            for_each(base, named, |card| {
                Ok(match seq.serialize_element(&card) {
                    Ok(()) => ControlFlow::Continue(()),
                    Err(e) => {
                        failed = Some(e);
                        ControlFlow::Break(())
                    }
                })
            })?;
            if let Some(e) = failed {
                return Err(e.into());
            }
            seq.end()?;
        }
        writeln!(out)?;
    } else {
        writeln!(out, "Name\tFull name\tRoles\tUUID")?;
        let mut failed = None;
        for_each(base, named, |card| {
            Ok(match write_row(out, &card) {
                Ok(()) => ControlFlow::Continue(()),
                Err(e) => {
                    failed = Some(e);
                    ControlFlow::Break(())
                }
            })
        })?;
        if let Some(e) = failed {
            return Err(e.into());
        }
    }
    Ok(())
}

pub fn write_json(out: &mut dyn Write, value: &impl serde::Serialize) -> crate::Result<()> {
    serde_json::to_writer_pretty(&mut *out, value)?;
    writeln!(out)?;
    Ok(())
}

fn cell(s: &str) -> String {
    s.chars()
        .flat_map(|c| {
            if c.is_control() {
                c.escape_default().collect::<Vec<_>>()
            } else {
                vec![c]
            }
        })
        .collect()
}

pub fn write_list(out: &mut dyn Write, cards: &[Card]) -> crate::Result<()> {
    writeln!(out, "Name\tFull name\tRoles\tUUID")?;
    for c in cards {
        write_row(out, c)?;
    }
    Ok(())
}

fn write_row(out: &mut dyn Write, c: &Card) -> std::io::Result<()> {
    let roles = c
        .roles
        .iter()
        .map(|r| cell(r.name.as_deref().unwrap_or(&r.id)))
        .collect::<Vec<_>>()
        .join(", ");
    writeln!(
        out,
        "{}\t{}\t{}\t{}",
        cell(&c.name),
        cell(&c.full_name),
        roles,
        c.id
    )
}

pub fn write_card(out: &mut dyn Write, card: &Card) -> crate::Result<()> {
    let value = serde_json::to_value(card)?;
    if let Some(fields) = value.as_object() {
        for (name, value) in fields {
            let text = value
                .as_str()
                .map(cell)
                .unwrap_or_else(|| value.to_string());
            writeln!(out, "{name}: {text}")?;
        }
    }
    Ok(())
}

mod write;
pub use write::*;

#[cfg(all(feature = "testing", unix))]
mod du2u;
mod metadata;
mod mutation;
mod qualification;
#[cfg(all(feature = "testing", unix))]
#[doc(hidden)]
pub use du2u::qualify_sealed_unix_clear;
#[cfg(all(feature = "testing", unix))]
#[doc(hidden)]
pub fn du2u_qualification_binding() -> serde_json::Value {
    serde_json::json!({"head":option_env!("DU2U_QUALIFICATION_HEAD"),
        "source_sha256":option_env!("DU2U_QUALIFICATION_SOURCE_SHA256"),
        "source_binding":qualification::source_binding()})
}
mod snapshot;
#[cfg(unix)]
mod source;

#[cfg(feature = "testing")]
fn qualification_pin(path: &Path, maximum: u64) -> Result<(Vec<u8>, String), UserWriteError> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    for parent in path.ancestors() {
        let metadata =
            std::fs::symlink_metadata(parent).map_err(|_| UserWriteError::UnsupportedSource)?;
        if metadata.file_type().is_symlink() {
            return Err(UserWriteError::UnsupportedSource);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if metadata.file_attributes() & 0x400 != 0 {
                return Err(UserWriteError::UnsupportedSource);
            }
        }
    }
    let mut file = std::fs::File::open(path).map_err(|_| UserWriteError::UnsupportedSource)?;
    let metadata = file
        .metadata()
        .map_err(|_| UserWriteError::UnsupportedSource)?;
    if !metadata.is_file() || metadata.len() > maximum {
        return Err(UserWriteError::UnsupportedSource);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.nlink() != 1 {
            return Err(UserWriteError::UnsupportedSource);
        }
    }
    #[cfg(windows)]
    {
        if !qualification_single_link(&file)? {
            return Err(UserWriteError::UnsupportedSource);
        }
    }
    let mut digest = Sha256::new();
    let mut retained = Vec::new();
    let mut count = 0u64;
    let mut buffer = [0u8; 65536];
    loop {
        let n = file
            .read(&mut buffer)
            .map_err(|_| UserWriteError::UnsupportedSource)?;
        if n == 0 {
            break;
        }
        count = count
            .checked_add(n as u64)
            .filter(|n| *n <= maximum)
            .ok_or(UserWriteError::UnsupportedSource)?;
        let block = buffer.get(..n).ok_or(UserWriteError::UnsupportedSource)?;
        digest.update(block);
        if maximum <= 1_048_576 {
            retained.extend_from_slice(block);
        }
    }
    if count != metadata.len() {
        return Err(UserWriteError::UnsupportedSource);
    }
    Ok((retained, format!("{:x}", digest.finalize())))
}

#[cfg(all(feature = "testing", windows))]
#[allow(unsafe_code)]
fn qualification_single_link(file: &std::fs::File) -> Result<bool, UserWriteError> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };
    let mut info = std::mem::MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::uninit();
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), info.as_mut_ptr()) } == 0 {
        return Err(UserWriteError::UnsupportedSource);
    }
    Ok(unsafe { info.assume_init() }.nNumberOfLinks == 1)
}

#[cfg(feature = "testing")]
#[doc(hidden)]
pub fn qualify_sealed_user_clear(
    root: &Path,
    request_sha256: &str,
    base_sha256: &str,
) -> Result<UserWriteOutcome, UserWriteError> {
    qualify_fixture_clear(root, request_sha256, base_sha256, false)
}

#[cfg(feature = "testing")]
#[doc(hidden)]
pub fn qualify_sealed_mirror_clear(
    root: &Path,
    request_sha256: &str,
    base_sha256: &str,
) -> Result<UserWriteOutcome, UserWriteError> {
    qualify_fixture_clear(root, request_sha256, base_sha256, true)
}

#[cfg(feature = "testing")]
fn qualify_fixture_clear(
    root: &Path,
    request_sha256: &str,
    base_sha256: &str,
    mirrored_admin: bool,
) -> Result<UserWriteOutcome, UserWriteError> {
    let refused = || UserWriteError::UnsupportedSource;
    let compiled_head = option_env!("DU2A_QUALIFICATION_HEAD").ok_or_else(refused)?;
    if !cfg!(windows)
        || compiled_head.len() != 40
        || !root.is_absolute()
        || root.parent() != Some(Path::new(r"C:\yc"))
        || !root
            .file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|s| s.starts_with("du2a-native-"))
        || request_sha256.len() != 64
        || base_sha256.len() != 64
    {
        return Err(refused());
    }
    let base = qualification_candidate_slot(
        root,
        compiled_head,
        request_sha256,
        base_sha256,
        mirrored_admin,
    )?;
    let snapshot = write_snapshot(&base)?;
    let mut names = snapshot
        .users
        .iter()
        .map(|c| c.name.as_str())
        .collect::<Vec<_>>();
    names.sort_unstable();
    let expected = if mirrored_admin {
        vec!["Du2aAdmin", "Du2aEdited", "Du2aUnrelated"]
    } else {
        vec!["Du2aAdmin", "Du2aCreated", "Du2aEdited", "Du2aUnrelated"]
    };
    if names != expected {
        return Err(refused());
    }
    let target = snapshot
        .users
        .iter()
        .find(|c| {
            c.name
                == if mirrored_admin {
                    "Du2aAdmin"
                } else {
                    "Du2aEdited"
                }
        })
        .ok_or_else(refused)?;
    if !target.password_set || !target.data_separation.is_empty() {
        return Err(refused());
    }
    let mutation = UserWriteRequest {
        revision: snapshot.revision,
        confirmed: true,
        allow_last_user: false,
        action: UserAction::ClearPassword {
            identity: target.id.clone(),
        },
    };
    mutation::apply_fixture_clear(&base, &mutation)
}

#[cfg(feature = "testing")]
fn qualification_candidate_slot(
    root: &Path,
    compiled_head: &str,
    request_sha256: &str,
    base_sha256: &str,
    mirrored_admin: bool,
) -> Result<std::path::PathBuf, UserWriteError> {
    use serde_json::Value as Json;
    let refused = || UserWriteError::UnsupportedSource;
    let (raw, pin) = qualification_pin(&root.join("request.json"), 1_048_576)?;
    if pin != request_sha256 {
        return Err(refused());
    }
    let request: Json = serde_json::from_slice(&raw).map_err(|_| refused())?;
    if request.get("owner").and_then(Json::as_str) != Some("DU2a users minimal")
        || request.get("head").and_then(Json::as_str) != Some(compiled_head)
        || request.get("root").and_then(Json::as_str).map(Path::new) != Some(root)
        || request
            .get("build")
            .and_then(|v| v.get("head"))
            .and_then(Json::as_str)
            != Some(compiled_head)
    {
        return Err(refused());
    }
    let (raw, pin) = qualification_pin(&root.join("inputs").join("input-pins.json"), 1_048_576)?;
    if request
        .get("private_input_commitment")
        .and_then(|v| v.get("sha256"))
        .and_then(Json::as_str)
        != Some(pin.as_str())
        || request
            .get("private_input_commitment")
            .and_then(|v| v.get("bytes"))
            .and_then(Json::as_u64)
            != Some(raw.len() as u64)
    {
        return Err(refused());
    }
    let pins: std::collections::BTreeMap<String, Json> =
        serde_json::from_slice(&raw).map_err(|_| refused())?;
    if pins.is_empty()
        || pins.len() > 256
        || !pins.contains_key("packet.json")
        || !pins.contains_key("fixture-private.json")
    {
        return Err(refused());
    }
    for (name, expected) in pins {
        let relative = Path::new(&name);
        if relative.is_absolute()
            || name.contains('\\')
            || relative
                .components()
                .any(|c| !matches!(c, std::path::Component::Normal(_)))
        {
            return Err(refused());
        }
        let (raw, hash) = qualification_pin(&root.join("inputs").join(relative), 1_048_576)?;
        if expected.get("bytes").and_then(Json::as_u64) != Some(raw.len() as u64)
            || expected.get("sha256").and_then(Json::as_str) != Some(hash.as_str())
        {
            return Err(refused());
        }
    }
    let (raw, _) = qualification_pin(&root.join("inputs").join("packet.json"), 1_048_576)?;
    let plan: Json = serde_json::from_slice(&raw).map_err(|_| refused())?;
    if plan.get("head") != request.get("head")
        || plan.get("root") != request.get("root")
        || plan.get("turn") != request.get("turn")
        || plan
            .get("candidate_clear_qualification")
            .and_then(Json::as_str)
            != Some("sealed-candidate-only-v1")
    {
        return Err(refused());
    }
    let cases = plan
        .get("cases")
        .and_then(Json::as_array)
        .ok_or_else(refused)?;
    let case_id = if mirrored_admin {
        "candidate-mirror-empty"
    } else {
        "candidate-permanent-empty"
    };
    let operation = if mirrored_admin {
        "candidate:clear-mirror"
    } else {
        "candidate:clear"
    };
    if cases
        .iter()
        .filter(|case| case.get("id").and_then(Json::as_str) == Some(case_id))
        .count()
        != 1
        || !cases.iter().any(|case| {
            case.get("id").and_then(Json::as_str) == Some(case_id)
                && case.get("lane").and_then(Json::as_str) == Some("candidate")
                && case.get("operation").and_then(Json::as_str) == Some(operation)
                && case.get("state").and_then(Json::as_str) == Some("READY")
        })
    {
        return Err(refused());
    }
    let base = root.join("candidate").join("1Cv8.1CD");
    let (_, pin) = qualification_pin(&base, 256 * 1_048_576)?;
    if pin != base_sha256 {
        return Err(refused());
    }
    Ok(base)
}

#[cfg(all(test, feature = "testing"))]
fn qualification_candidate(
    root: &Path,
    compiled_head: &str,
    request_sha256: &str,
    base_sha256: &str,
) -> Result<std::path::PathBuf, UserWriteError> {
    qualification_candidate_slot(root, compiled_head, request_sha256, base_sha256, false)
}

pub(crate) fn source_identity(
    base: &Path,
) -> Result<(std::path::PathBuf, (u64, u64)), UserWriteError> {
    let path = snapshot::path(base)?;
    let file = std::fs::File::open(&path).map_err(|_| UserWriteError::UnsupportedSource)?;
    Ok((path, snapshot::identity(&file)?))
}

pub(crate) fn file_identity(file: &std::fs::File) -> Result<(u64, u64), UserWriteError> {
    snapshot::identity(file)
}

#[cfg(all(test, feature = "testing"))]
mod qualification_tests {
    use super::*;

    #[test]
    fn qualification_refuses_a_foreign_base_without_writing() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().join("1Cv8.1CD");
        std::fs::write(&base, b"foreign source").unwrap();
        assert!(matches!(
            qualify_sealed_user_clear(temp.path(), &"a".repeat(64), &"b".repeat(64)),
            Err(UserWriteError::UnsupportedSource)
        ));
        assert_eq!(std::fs::read(&base).unwrap(), b"foreign source");
        assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 1);
    }

    #[test]
    fn qualification_pin_is_bounded_and_refuses_shared_files() {
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let file = root.join("pin.json");
        std::fs::write(&file, b"{}").unwrap();
        assert!(qualification_pin(&file, 1).is_err());
        let (raw, hash) = qualification_pin(&file, 2).unwrap();
        assert_eq!(raw, b"{}");
        assert_eq!(
            hash,
            "44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a"
        );
        std::fs::hard_link(&file, root.join("alias.json")).unwrap();
        assert!(qualification_pin(&file, 2).is_err());
    }

    #[test]
    fn qualification_requires_exact_sealed_candidate_inputs_and_database() {
        use serde_json::json;
        let temp = tempfile::tempdir().unwrap();
        let physical = std::fs::canonicalize(temp.path()).unwrap();
        let root = physical.as_path();
        let inputs = root.join("inputs");
        std::fs::create_dir(&inputs).unwrap();
        std::fs::create_dir(root.join("candidate")).unwrap();
        let base = root.join("candidate").join("1Cv8.1CD");
        std::fs::write(&base, b"synthetic candidate bytes").unwrap();
        let head = "a".repeat(40);
        let mut plan = json!({"head":head,"root":root,"turn":"du2a-native-test",
            "candidate_clear_qualification":"sealed-candidate-only-v1",
            "cases":[{"id":"candidate-permanent-empty","lane":"candidate",
                "operation":"candidate:clear","state":"READY"}]});
        let reseal = |plan: &serde_json::Value| {
            std::fs::write(
                inputs.join("packet.json"),
                serde_json::to_vec(plan).unwrap(),
            )
            .unwrap();
            std::fs::write(inputs.join("fixture-private.json"), b"{\"synthetic\":true}").unwrap();
            let mut pins = std::collections::BTreeMap::new();
            for name in ["packet.json", "fixture-private.json"] {
                let (raw, hash) = qualification_pin(&inputs.join(name), 1_048_576).unwrap();
                pins.insert(name, json!({"bytes":raw.len(),"sha256":hash}));
            }
            std::fs::write(
                inputs.join("input-pins.json"),
                serde_json::to_vec(&pins).unwrap(),
            )
            .unwrap();
            let (raw, hash) =
                qualification_pin(&inputs.join("input-pins.json"), 1_048_576).unwrap();
            let request = json!({"head":head,"build":{"head":head},"owner":"DU2a users minimal",
                "root":root,"turn":"du2a-native-test","private_input_commitment":{"bytes":raw.len(),"sha256":hash}});
            std::fs::write(
                root.join("request.json"),
                serde_json::to_vec(&request).unwrap(),
            )
            .unwrap();
            qualification_pin(&root.join("request.json"), 1_048_576)
                .unwrap()
                .1
        };
        let request = reseal(&plan);
        let database = qualification_pin(&base, 256 * 1_048_576).unwrap().1;
        assert_eq!(
            qualification_candidate(root, &head, &request, &database).unwrap(),
            base
        );
        assert!(qualification_candidate_slot(root, &head, &request, &database, true).is_err());
        let mut mirror_plan = plan.clone();
        mirror_plan["cases"][0]["id"] = json!("candidate-mirror-empty");
        mirror_plan["cases"][0]["operation"] = json!("candidate:clear-mirror");
        let mirror_request = reseal(&mirror_plan);
        assert_eq!(
            qualification_candidate_slot(root, &head, &mirror_request, &database, true).unwrap(),
            base
        );
        assert!(qualification_candidate(root, &head, &mirror_request, &database).is_err());
        let request = reseal(&plan);
        assert!(qualification_candidate(root, &"b".repeat(40), &request, &database).is_err());
        assert!(qualification_candidate(root, &head, &"b".repeat(64), &database).is_err());
        assert!(qualification_candidate(root, &head, &request, &"b".repeat(64)).is_err());
        std::fs::write(inputs.join("fixture-private.json"), b"changed input").unwrap();
        assert!(qualification_candidate(root, &head, &request, &database).is_err());
        let request = reseal(&plan);
        std::fs::write(&base, b"foreign database").unwrap();
        assert!(qualification_candidate(root, &head, &request, &database).is_err());
        plan["cases"][0]["lane"] = json!("native");
        let request = reseal(&plan);
        let foreign = qualification_pin(&base, 256 * 1_048_576).unwrap().1;
        assert!(qualification_candidate(root, &head, &request, &foreign).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn qualification_pin_refuses_symlink_ancestors() {
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let real = root.join("real");
        std::fs::create_dir(&real).unwrap();
        std::fs::write(real.join("pin.json"), b"{}").unwrap();
        let alias = root.join("alias");
        std::os::unix::fs::symlink(&real, &alias).unwrap();
        assert!(qualification_pin(&alias.join("pin.json"), 2).is_err());
    }
}
