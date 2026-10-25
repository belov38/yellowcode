use mdconf::ConfigurationExt;
use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::ops::Bound;

use brace::binary::{Atom, BinNode};
use dtfile::container::{DtHeader, DtVersion};
use dtfile::{DtWriter, Source, TableHead};
use ibschema::columns::ColType;
use ibschema::logical::LogicalSchema;
use ibschema::values::ColumnValue;
use onecd::{BlobVal, DateTime, Numeric, ReadTxn, RowId, TableRead, Value};

use crate::colval::{col_type_of, from_onecd, value_kind};
use crate::config::ConfigTxnSource;
use crate::raw::{dbnames_text_in, dbschema_text_in};
use crate::{Error, Infobase, Result};
use mdconf::{EntryMeta, EntrySource, LoadOptions};

pub const MAX_VALUE_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_ROW_BYTES: usize = 128 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsersLayout {
    Users8,
    UsersSpr11,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoUsersProfile {
    Observed8511529Controlled,
}

#[derive(Clone, Copy)]
enum UsersSelection {
    Explicit(UsersLayout),
    Auto(AutoUsersProfile),
}

fn separation(object: &mdconf::MdObject, name: &str) -> Result<bool> {
    let context = format!("CONFIG/CommonAttribute/{name}");
    let mut props = object.props.iter().filter(|p| p.name == name);
    let prop = props
        .next()
        .ok_or_else(|| limit(&context, "missing property"))?;
    if props.next().is_some() {
        return Err(limit(&context, "duplicate property"));
    }
    if prop.codec != mdconf::Codec::Enum {
        return Err(limit(&context, "expected enum property"));
    }
    match &prop.value {
        mdconf::Value::Text(value) if value == "Separate" => Ok(true),
        mdconf::Value::Text(value) if value == "DontUse" => Ok(false),
        _ => Err(limit(&context, "unsupported separation value")),
    }
}

const AUTO_PROJECTION_XDTO_UNKNOWNS: &[&str] = &[
    "unknown:CatalogProperties.AdditionalIndexes",
    "unknown:CatalogProperties.ExtendedConfigurationObject",
    "unknown:CatalogProperties.Help",
    "unknown:CatalogProperties.ManagerModule",
    "unknown:CatalogProperties.ObjectBelonging",
    "unknown:CatalogProperties.ObjectModule",
    "unknown:CatalogProperties.Predefined",
    "unknown:CommonAttributeProperties.ExtendedConfigurationObject",
    "unknown:CommonAttributeProperties.ObjectBelonging",
    "unknown:HTTPServiceProperties.ExtendedConfigurationObject",
    "unknown:HTTPServiceProperties.Module",
    "unknown:HTTPServiceProperties.ObjectBelonging",
    "unknown:LanguageProperties.ExtendedConfigurationObject",
    "unknown:LanguageProperties.ObjectBelonging",
    "unknown:RoleProperties.ExtendedConfigurationObject",
    "unknown:RoleProperties.ObjectBelonging",
    "unknown:RoleProperties.Rights",
    "unknown:SessionParameterProperties.ExtendedConfigurationObject",
    "unknown:SessionParameterProperties.ObjectBelonging",
];

fn auto_users_layout(
    configuration: Option<&mdconf::Configuration>,
    profile: AutoUsersProfile,
) -> Result<UsersLayout> {
    let configuration = configuration
        .ok_or_else(|| limit("CONFIG", "auto users profile requires configuration"))?;
    match profile {
        AutoUsersProfile::Observed8511529Controlled => {
            if configuration.compatibility_mode().0 != 80501 {
                return Err(limit(
                    "CONFIG/CompatibilityMode",
                    "auto users profile requires 80501; producer build is caller-asserted",
                ));
            }
            if configuration
                .diagnostics
                .misses
                .keys()
                .any(|key| !AUTO_PROJECTION_XDTO_UNKNOWNS.contains(&key.as_str()))
                || !configuration.diagnostics.unsupported.is_empty()
            {
                return Err(limit(
                    "CONFIG",
                    "auto users profile has unsupported metadata diagnostics",
                ));
            }
            let mut attributes = configuration
                .objects
                .iter()
                .filter(|o| o.kind == mdconf::MdKind::CommonAttribute);
            let attribute = attributes.next().ok_or_else(|| {
                limit(
                    "CONFIG/CommonAttribute",
                    "auto users profile requires exactly one top-level common attribute",
                )
            })?;
            if attributes.next().is_some() {
                return Err(limit(
                    "CONFIG/CommonAttribute",
                    "auto users profile requires exactly one top-level common attribute",
                ));
            }
            if !separation(attribute, "DataSeparation")? {
                return Err(limit(
                    "CONFIG/CommonAttribute/DataSeparation",
                    "auto users profile requires Separate",
                ));
            }
            let users = separation(attribute, "UsersSeparation")?;
            let authentication = separation(attribute, "AuthenticationSeparation")?;
            Ok(if users || authentication {
                UsersLayout::UsersSpr11
            } else {
                UsersLayout::Users8
            })
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DumpReport {
    pub files: u64,
    pub users: u64,
    pub tables: usize,
    pub rows: u64,
}

fn bad(context: &str, reason: &str) -> Error {
    Error::Inconsistent(format!("{context}: {reason}"))
}
fn limit(context: &str, reason: &str) -> Error {
    Error::Unsupported(format!("{context}: {reason}"))
}
fn absent(error: &onecd::Error) -> bool {
    matches!(error, onecd::Error::NotFound(onecd::NotFound::Table(_)))
}

#[derive(Clone, Copy)]
struct Limits {
    value: usize,
    row: usize,
}
const LIMITS: Limits = Limits {
    value: MAX_VALUE_BYTES,
    row: MAX_ROW_BYTES,
};

struct Budget {
    live: usize,
    limits: Limits,
}
impl Budget {
    fn new(limits: Limits) -> Self {
        Self { live: 0, limits }
    }
    fn add(&mut self, bytes: usize, context: &str) -> Result<()> {
        let total = self
            .live
            .checked_add(bytes)
            .ok_or_else(|| limit(context, "row payload length overflow"))?;
        if total > self.limits.row {
            return Err(limit(context, "row payload exceeds 128 MiB product limit"));
        }
        self.live = total;
        Ok(())
    }
    fn value(&self, bytes: usize, context: &str) -> Result<()> {
        if bytes > self.limits.value {
            return Err(limit(context, "value exceeds 64 MiB product limit"));
        }
        Ok(())
    }
    fn release(&mut self, bytes: usize) {
        self.live = self.live.saturating_sub(bytes);
    }
}

fn bytes_len(value: &Value) -> usize {
    match value {
        Value::Binary(b) | Value::Image(BlobVal::Data(b)) | Value::NText(BlobVal::Data(b)) => {
            b.len()
        }
        Value::Char(s) | Value::VarChar(s) => s.len(),
        _ => 0,
    }
}

fn append(out: &mut Vec<u8>, bytes: &[u8], budget: &mut Budget, context: &str) -> Result<()> {
    let length = out
        .len()
        .checked_add(bytes.len())
        .ok_or_else(|| limit(context, "value length overflow"))?;
    budget.value(length, context)?;
    budget.add(bytes.len(), context)?;
    if length > out.capacity() {
        let capacity = out
            .capacity()
            .saturating_mul(2)
            .max(length)
            .min(budget.limits.value);
        out.try_reserve_exact(capacity.saturating_sub(out.len()))
            .map_err(|e| limit(context, &format!("allocation: {e}")))?;
    }
    out.extend_from_slice(bytes);
    Ok(())
}

fn materialize(
    t: &TableRead<'_>,
    slot: RowId,
    field: &str,
    value: &BlobVal,
    budget: &mut Budget,
    context: &str,
) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    append_blob(t, slot, field, value, &mut out, budget, context)?;
    Ok(out)
}

fn append_blob(
    t: &TableRead<'_>,
    slot: RowId,
    field: &str,
    value: &BlobVal,
    out: &mut Vec<u8>,
    budget: &mut Budget,
    context: &str,
) -> Result<()> {
    let initial = out.len();
    let declared = match value {
        BlobVal::Data(b) => b.len(),
        BlobVal::Ref(r) => r.len as usize,
    };
    match value {
        BlobVal::Data(bytes) => {
            check_allocation(initial, declared, budget, context)?;
            append(out, bytes, budget, context)?;
        }
        BlobVal::Ref(_) => {
            let mut reader = guarded_open(initial, declared, budget, context, || {
                Ok(t.blob(slot, field)?)
            })?;
            append_reader(&mut reader, out, declared, budget, context)?;
        }
    }
    if out.len().checked_sub(initial) != Some(declared) {
        return Err(bad(context, "blob length differs from declared length"));
    }
    Ok(())
}

fn check_allocation(initial: usize, declared: usize, budget: &Budget, context: &str) -> Result<()> {
    budget.value(
        initial
            .checked_add(declared)
            .ok_or_else(|| limit(context, "joined length overflow"))?,
        context,
    )?;
    let future = budget
        .live
        .checked_add(declared)
        .ok_or_else(|| limit(context, "row payload length overflow"))?;
    if future > budget.limits.row {
        return Err(limit(context, "row payload exceeds 128 MiB product limit"));
    }
    Ok(())
}

fn guarded_open<R>(
    initial: usize,
    declared: usize,
    budget: &Budget,
    context: &str,
    open: impl FnOnce() -> Result<R>,
) -> Result<R> {
    check_allocation(initial, declared, budget, context)?;
    open()
}

fn append_reader(
    reader: &mut impl Read,
    out: &mut Vec<u8>,
    declared: usize,
    budget: &mut Budget,
    context: &str,
) -> Result<()> {
    let initial = out.len();
    let mut buffer = [0; 8192];
    loop {
        let remaining = budget
            .limits
            .value
            .saturating_sub(out.len())
            .min(budget.limits.row.saturating_sub(budget.live));
        let take = remaining.saturating_add(1).min(buffer.len());
        let n = reader
            .read(
                buffer
                    .get_mut(..take)
                    .ok_or_else(|| bad(context, "invalid read bound"))?,
            )
            .map_err(|e| bad(context, &format!("blob read: {e}")))?;
        if n == 0 {
            break;
        }
        append(
            out,
            buffer
                .get(..n)
                .ok_or_else(|| bad(context, "invalid reader length"))?,
            budget,
            context,
        )?;
    }
    if out.len().checked_sub(initial) != Some(declared) {
        return Err(bad(context, "blob length differs from declared length"));
    }
    Ok(())
}

struct TextBuffer {
    text: String,
    reserves: usize,
}
impl TextBuffer {
    fn new() -> Self {
        Self {
            text: String::new(),
            reserves: 0,
        }
    }
    fn push(&mut self, ch: char, budget: &mut Budget, context: &str) -> Result<()> {
        let add = ch.len_utf8();
        let length = self
            .text
            .len()
            .checked_add(add)
            .ok_or_else(|| limit(context, "UTF-8 length overflow"))?;
        budget.value(length, context)?;
        budget.add(add, context)?;
        if length > self.text.capacity() {
            let capacity = self
                .text
                .capacity()
                .saturating_mul(2)
                .max(length)
                .min(budget.limits.value);
            self.text
                .try_reserve_exact(capacity.saturating_sub(self.text.len()))
                .map_err(|e| limit(context, &format!("UTF-8 allocation: {e}")))?;
            self.reserves += 1;
        }
        self.text.push(ch);
        Ok(())
    }
    fn decode(&mut self, bytes: &[u8], budget: &mut Budget, context: &str) -> Result<()> {
        if !bytes.len().is_multiple_of(2) {
            return Err(limit(context, "odd UTF-16 byte length"));
        }
        let units = bytes.as_chunks::<2>().0.iter().map(|b| {
            let mut pair = [0; 2];
            pair.copy_from_slice(b);
            u16::from_le_bytes(pair)
        });
        for ch in char::decode_utf16(units) {
            self.push(
                ch.map_err(|_| limit(context, "malformed UTF-16"))?,
                budget,
                context,
            )?;
        }
        Ok(())
    }
}

fn columns(t: &TableRead<'_>, head: &TableHead) -> Result<Vec<usize>> {
    head.columns
        .iter()
        .map(|column| {
            let (i, field) = t
                .descr()
                .field(&column.name)
                .ok_or_else(|| bad(&head.phys_name, &format!("no column {}", column.name)))?;
            if col_type_of(field) != Some(column.ty) {
                return Err(bad(
                    &head.phys_name,
                    &format!("wrong type for {}", column.name),
                ));
            }
            Ok(i)
        })
        .collect()
}

fn convert_row(
    t: &TableRead<'_>,
    slot: RowId,
    row: &[Value],
    head: &TableHead,
    mapping: &[usize],
    limits: Limits,
) -> Result<Vec<ColumnValue>> {
    let mut budget = Budget::new(limits);
    for (i, value) in row.iter().enumerate() {
        let name = t
            .descr()
            .fields
            .get(i)
            .map_or("unknown", |f| f.name.as_str());
        let context = format!("{}/{}", head.phys_name, name);
        let size = bytes_len(value);
        budget.value(size, &context)?;
        budget.add(size, &context)?;
    }
    let mut values = Vec::with_capacity(head.columns.len());
    for (column, &i) in head.columns.iter().zip(mapping) {
        let context = format!("{}/{}", head.phys_name, column.name);
        let value = row.get(i).ok_or_else(|| bad(&context, "short row"))?;
        let converted = match (value, column.ty) {
            (Value::Image(blob), ColType::I) => ColumnValue::Bytes(materialize(
                t,
                slot,
                &column.name,
                blob,
                &mut budget,
                &context,
            )?),
            (Value::NText(blob), ColType::NT) => {
                let bytes = materialize(t, slot, &column.name, blob, &mut budget, &context)?;
                let mut output = TextBuffer::new();
                output.decode(&bytes, &mut budget, &context)?;
                budget.release(bytes.len());
                ColumnValue::Str(output.text)
            }
            (Value::Binary(bytes), ColType::B(_) | ColType::VB(_)) => {
                let mut output = Vec::new();
                append(&mut output, bytes, &mut budget, &context)?;
                ColumnValue::Bytes(output)
            }
            (Value::Char(text), ColType::NC(_)) | (Value::VarChar(text), ColType::NVC(_)) => {
                budget.value(text.len(), &context)?;
                budget.add(text.len(), &context)?;
                let mut output = String::new();
                output
                    .try_reserve_exact(text.len())
                    .map_err(|e| limit(&context, &format!("allocation: {e}")))?;
                output.push_str(text);
                ColumnValue::Str(output)
            }
            _ => {
                let size = bytes_len(value);
                budget.value(size, &context)?;
                budget.add(size, &context)?;
                from_onecd(value, column.ty, &mut |_| {
                    Err(onecd::Error::Io(std::io::Error::other(
                        "unexpected unbounded blob request",
                    )))
                })?
            }
        };
        values.push(converted);
    }
    Ok(values)
}

struct BoundedConfig<'a>(ConfigTxnSource<'a>);
impl EntrySource for BoundedConfig<'_> {
    fn list(&self) -> mdconf::Result<Vec<EntryMeta>> {
        self.0.list()
    }
    fn stored(&self, name: &str) -> mdconf::Result<Option<Vec<u8>>> {
        self.0.stored_bounded(name, MAX_VALUE_BYTES)
    }
    fn stored_bounded(&self, name: &str, cap: usize) -> mdconf::Result<Option<Vec<u8>>> {
        self.0.stored_bounded(name, cap.min(MAX_VALUE_BYTES))
    }
}

fn configuration_in(txn: &ReadTxn) -> Result<Option<mdconf::Configuration>> {
    let source = match ConfigTxnSource::read(txn, "CONFIG") {
        Ok(source) => source,
        Err(Error::Engine(e)) if absent(&e) => return Ok(None),
        Err(e) => return Err(e),
    };
    if source.is_empty() {
        return Ok(None);
    }
    mdconf::Configuration::load(
        &BoundedConfig(source),
        &LoadOptions {
            keep_source: false,
            ..Default::default()
        },
    )
    .map(Some)
    .map_err(|source| Error::ConfigTable {
        table: "CONFIG".into(),
        source,
    })
}

struct SnapshotMetadata {
    schema: LogicalSchema,
    compatibility: u32,
    users: TableHead,
}
impl SnapshotMetadata {
    fn read(txn: &ReadTxn, selection: UsersSelection) -> Result<Self> {
        let schema = LogicalSchema::from_text(&dbschema_text_in(txn)?)?;
        let configuration = configuration_in(txn)?;
        let users = match selection {
            UsersSelection::Explicit(users) => users,
            UsersSelection::Auto(profile) => auto_users_layout(configuration.as_ref(), profile)?,
        };
        let compatibility = configuration
            .as_ref()
            .map_or(80501, |c| c.compatibility_mode().0);
        if compatibility <= 80214 {
            return Err(limit(
                "CONFIG/CompatibilityMode",
                "F2 export is unsupported",
            ));
        }
        if txn
            .table_names()
            .iter()
            .any(|n| n.eq_ignore_ascii_case("PARAMS"))
        {
            let params = ConfigTxnSource::read(txn, "PARAMS")?;
            if params
                .list()?
                .iter()
                .any(|entry| entry.name.eq_ignore_ascii_case("DBNames"))
            {
                ibschema::dbnames::DbNames::parse(&dbnames_text_in(txn)?)?;
            }
        }
        let source = match users {
            UsersLayout::Users8 => Source::Users,
            UsersLayout::UsersSpr11 => Source::UsersSpr,
        };
        let users =
            TableHead::system(&source).ok_or_else(|| bad("V8USERS", "no selected layout"))?;
        match txn.table(&users.phys_name) {
            Ok(table) => {
                columns(&table, &users)?;
            }
            Err(e) if absent(&e) => {}
            Err(e) => return Err(e.into()),
        }
        Ok(Self {
            schema,
            compatibility,
            users,
        })
    }
}

fn write_table(
    txn: &ReadTxn,
    writer: &mut DtWriter<&mut dyn Write>,
    head: &TableHead,
    optional: bool,
) -> Result<u64> {
    writer.begin_table(head)?;
    let table = match txn.table(&head.phys_name) {
        Ok(table) => Some(table),
        Err(e) if optional && absent(&e) => None,
        Err(e) if absent(&e) => return Err(bad(&head.phys_name, "DataBase table is missing")),
        Err(e) => return Err(e.into()),
    };
    let mut count = 0;
    if let Some(table) = table {
        let mapping = columns(&table, head)?;
        for item in table.scan() {
            let (slot, row) = item?;
            let values = convert_row(&table, slot, &row, head, &mapping, LIMITS)?;
            writer.row(&values)?;
            count += 1;
        }
    }
    writer.end_table()?;
    Ok(count)
}

struct FolderFile {
    name: String,
    created: DateTime,
    modified: DateTime,
    attributes: Numeric,
    size: Numeric,
    length: usize,
    parts: Vec<(u64, RowId)>,
}
fn number(value: &Numeric, context: &str) -> Result<u64> {
    if value.scale() != 0 || (value.is_negative() && value.digits().iter().any(|&d| d != 0)) {
        return Err(bad(context, "invalid nonnegative integer"));
    }
    value.digits().iter().try_fold(0u64, |n, &d| {
        n.checked_mul(10)
            .and_then(|n| n.checked_add(u64::from(d)))
            .ok_or_else(|| bad(context, "integer overflow"))
    })
}
fn folder_files(table: &TableRead<'_>) -> Result<Vec<FolderFile>> {
    let descr = table.descr();
    let context = &descr.name;
    let field = |name: &str| {
        descr
            .field(name)
            .map(|(i, _)| i)
            .ok_or_else(|| bad(context, &format!("no column {name}")))
    };
    let name = field("FILENAME")?;
    let created = field("CREATION")?;
    let modified = field("MODIFIED")?;
    let attributes = field("ATTRIBUTES")?;
    let size = field("DATASIZE")?;
    field("BINARYDATA")?;
    let part = descr.field("PARTNO").map(|(i, _)| i);
    let index = descr
        .indexes
        .iter()
        .find(|ix| {
            ix.fields
                .first()
                .is_some_and(|f| f.name.eq_ignore_ascii_case("FILENAME"))
        })
        .ok_or_else(|| bad(context, "no index beginning with FILENAME"))?;
    let index = table.index(&index.name)?;
    let mut files: Vec<FolderFile> = Vec::new();
    let mut by_name = HashMap::new();
    let mut seen = HashSet::new();
    for item in index.entries(
        Bound::Unbounded,
        Bound::Unbounded,
        onecd::Direction::Forward,
    )? {
        let entry = item?;
        if !seen.insert(entry.row) {
            return Err(bad(context, "repeated indexed row"));
        }
        let row = table
            .get(entry.row)?
            .ok_or_else(|| bad(context, "indexed row vanished"))?;
        let cell = |i| row.get(i).ok_or_else(|| bad(context, "short folder row"));
        let name = match cell(name)? {
            Value::Char(s) | Value::VarChar(s) => s.clone(),
            v => return Err(bad(context, &format!("FILENAME is {}", value_kind(v)))),
        };
        let date = |i| match cell(i)? {
            Value::DateTime(d) => Ok(*d),
            _ => Err(bad(context, "invalid folder date")),
        };
        let numeric = |i| match cell(i)? {
            Value::Numeric(n) => Ok(n.clone()),
            _ => Err(bad(context, "invalid folder number")),
        };
        let attributes = numeric(attributes)?;
        let size = numeric(size)?;
        let length = usize::try_from(number(&size, context)?)
            .map_err(|_| limit(context, "DATASIZE overflow"))?;
        if length > MAX_VALUE_BYTES {
            return Err(limit(context, "BINARYDATA exceeds 64 MiB product limit"));
        }
        let created = date(created)?;
        let modified = date(modified)?;
        let partno = match part {
            Some(i) => number(&numeric(i)?, context)?,
            None => 0,
        };
        let i = *by_name.entry(name.clone()).or_insert_with(|| {
            files.push(FolderFile {
                name,
                created,
                modified,
                attributes: attributes.clone(),
                size: size.clone(),
                length,
                parts: Vec::new(),
            });
            files.len() - 1
        });
        let file = files
            .get_mut(i)
            .ok_or_else(|| bad(context, "invalid folder index"))?;
        if (file.created, file.modified, &file.attributes, &file.size)
            != (created, modified, &attributes, &size)
        {
            return Err(bad(context, "inconsistent multipart metadata"));
        }
        file.parts.push((partno, entry.row));
    }
    for file in &mut files {
        file.parts.sort_by_key(|&(part, _)| part);
        for (expected, &(actual, _)) in file.parts.iter().enumerate() {
            if actual != expected as u64 {
                return Err(bad(context, "PARTNO must be contiguous from zero"));
            }
        }
    }
    for item in table.scan() {
        let (slot, _) = item?;
        if !seen.contains(&slot) {
            return Err(bad(context, "row missing from filename index"));
        }
    }
    Ok(files)
}

fn write_folder(txn: &ReadTxn, writer: &mut DtWriter<&mut dyn Write>, name: &str) -> Result<u64> {
    let head = TableHead::system(&Source::Folder(name.into()))
        .ok_or_else(|| bad(name, "no folder layout"))?;
    writer.begin_table(&head)?;
    let table = match txn.table(&head.phys_name) {
        Ok(table) => table,
        Err(e) if absent(&e) => {
            writer.end_table()?;
            return Ok(0);
        }
        Err(e) => return Err(e.into()),
    };
    let files = folder_files(&table)?;
    let (column, _) = table
        .descr()
        .field("BINARYDATA")
        .ok_or_else(|| bad(name, "no BINARYDATA"))?;
    let mut count = 0;
    for file in files {
        let context = format!("{}/BINARYDATA", head.phys_name);
        let mut budget = Budget::new(LIMITS);
        budget.value(file.name.len(), &context)?;
        budget.add(file.name.len(), &context)?;
        let mut joined = Vec::new();
        for (_, slot) in file.parts {
            let row = table.get(slot)?.ok_or_else(|| bad(name, "part vanished"))?;
            let value = row.get(column).ok_or_else(|| bad(name, "short row"))?;
            let blob = match value {
                Value::Image(b) => b,
                _ => return Err(bad(&context, "folder data is not an image")),
            };
            let inline = bytes_len(value);
            budget.value(inline, &context)?;
            budget.add(inline, &context)?;
            append_blob(
                &table,
                slot,
                "BINARYDATA",
                blob,
                &mut joined,
                &mut budget,
                &context,
            )?;
            budget.release(inline);
        }
        if joined.len() != file.length {
            return Err(bad(&context, "actual length differs from DATASIZE"));
        }
        writer.row(&[
            ColumnValue::Str(file.name),
            ColumnValue::DateTime(file.created),
            ColumnValue::DateTime(file.modified),
            ColumnValue::Number(file.attributes),
            ColumnValue::Number(file.size),
            ColumnValue::Bytes(joined),
        ])?;
        count += 1;
    }
    writer.end_table()?;
    Ok(count)
}

pub fn write_dt(txn: &ReadTxn, users: UsersLayout, out: &mut dyn Write) -> Result<DumpReport> {
    write_dt_selected(txn, UsersSelection::Explicit(users), out)
}

pub fn write_dt_auto(
    txn: &ReadTxn,
    profile: AutoUsersProfile,
    out: &mut dyn Write,
) -> Result<DumpReport> {
    write_dt_selected(txn, UsersSelection::Auto(profile), out)
}

fn write_dt_selected(
    txn: &ReadTxn,
    selection: UsersSelection,
    out: &mut dyn Write,
) -> Result<DumpReport> {
    let metadata = SnapshotMetadata::read(txn, selection)?;
    let mut writer = DtWriter::new(
        out,
        DtHeader {
            version: DtVersion::F3,
            ev: false,
        },
    )?;
    writer.compatibility_mode(i64::from(metadata.compatibility))?;
    for name in ["IndexCompatibility", "UseMVCC"] {
        writer.custom_flag(name, &BinNode::Atom(Atom::int(1)))?;
    }
    let mut report = DumpReport::default();
    for name in [
        "Config",
        "ConfigSave",
        "Params",
        "Files",
        "DepotFiles",
        "ConfigCAS",
        "ConfigCASSave",
    ] {
        report.files += write_folder(txn, &mut writer, name)?;
    }
    report.users = write_table(txn, &mut writer, &metadata.users, true)?;
    for source in [Source::PasswordPolicies, Source::CompromisedPasswords] {
        let head = TableHead::system(&source).ok_or_else(|| bad("system table", "no layout"))?;
        write_table(txn, &mut writer, &head, true)?;
    }
    writer.begin_database(&metadata.schema)?;
    for head in TableHead::database(&metadata.schema)? {
        report.rows += write_table(txn, &mut writer, &head, false)?;
        report.tables += 1;
    }
    writer.end_database()?;
    writer.finish()?;
    Ok(report)
}

impl Infobase {
    pub fn dump_dt_auto(
        &self,
        profile: AutoUsersProfile,
        out: &mut dyn Write,
    ) -> Result<DumpReport> {
        let txn = self.raw().db().read();
        write_dt_auto(&txn, profile, out)
    }

    pub fn dump_dt(&self, users: UsersLayout, out: &mut dyn Write) -> Result<DumpReport> {
        let txn = self.raw().db().read();
        write_dt(&txn, users, out)
    }
}
