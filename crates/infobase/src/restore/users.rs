use super::*;
use memory_budget::{Category, MemoryBudget, Reservation};
use std::io::{Seek, SeekFrom, Write};
use v8users::{
    BslDate, Uuid16,
    user::{SERVICE_ROW_ID, UserData, UserRow},
};

mod configuration;
pub(super) use configuration::facts;

const KIB64: u64 = 64 << 10;
const MAX_NAMES: usize = 4096;

fn reserve(budget: Option<&MemoryBudget>, bytes: u64) -> Result<Option<Reservation>> {
    budget
        .map(|b| {
            b.try_reserve(Category::Metadata, bytes)
                .map_err(|m| Error::Memory(memory_budget::BudgetError::Limit(m)))
        })
        .transpose()
}
fn scaled(bytes: u64, factor: u64, extra: u64) -> Result<u64> {
    bytes
        .checked_mul(factor)
        .and_then(|n| n.checked_add(extra))
        .ok_or_else(|| bad("users workspace size overflow"))
}

#[derive(Default)]
pub(super) struct Facts {
    pub separated: bool,
    pub administration: Option<Vec<Uuid16>>,
    pub names: Vec<String>,
    _lease: Option<Reservation>,
}
impl Facts {
    fn scratch_bytes(&self, data: usize) -> Result<u64> {
        let names = self
            .names
            .iter()
            .try_fold(0u64, |n, s| n.checked_add(s.len() as u64))
            .ok_or_else(|| bad("users configuration size overflow"))?;
        let context = scaled(names, 8, scaled(self.names.len() as u64, 256, MIB)?)?;
        scaled(data as u64, 128, context)
    }
}

pub(super) struct Prepared {
    spool: Option<File>,
    pub rebuild: bool,
    pub dropped: u64,
    count: u64,
}

fn put_frame(file: &mut File, head: &TableHead, row: &[ColumnValue]) -> Result<()> {
    let start = io(file.stream_position())?;
    io(file.write_all(&[0; 8]))?;
    let mut writer = brace::binary::Writer::new(&mut *file);
    dtfile::rows::encode_row(&mut writer, &head.columns, row, KIB64 as usize)?;
    io(writer.finish())?;
    let end = io(file.stream_position())?;
    let length = end
        .checked_sub(start)
        .and_then(|n| n.checked_sub(8))
        .ok_or_else(|| bad("users frame size"))?;
    io(file.seek(SeekFrom::Start(start)))?;
    io(file.write_all(&length.to_le_bytes()))?;
    io(file.seek(SeekFrom::Start(end)))?;
    Ok(())
}

fn encode_admission(row: &[ColumnValue], capacity: usize) -> Result<u64> {
    let mut retained = scaled(
        capacity as u64,
        std::mem::size_of::<ColumnValue>() as u64,
        0,
    )?;
    let mut largest = 0u64;
    for value in row {
        let bytes = match value {
            ColumnValue::Bytes(bytes) => bytes.capacity() as u64,
            ColumnValue::Str(text) => {
                largest = largest.max(text.len() as u64);
                text.capacity() as u64
            }
            ColumnValue::Number(number) => {
                largest = largest.max(number.digits().len() as u64 + 64);
                scaled(number.digits().len() as u64, 2, 64)?
            }
            _ => 0,
        };
        retained = retained
            .checked_add(bytes)
            .ok_or_else(|| bad("users retained row size overflow"))?;
    }
    scaled(
        largest,
        16,
        retained
            .checked_add(MIB)
            .ok_or_else(|| bad("users encoder size overflow"))?,
    )
}

pub(super) fn prepare<R: Read>(
    reader: &mut DtReader<R>,
    head: &TableHead,
    run: &Run<'_, '_>,
) -> Result<Prepared> {
    let mut out = Prepared {
        spool: None,
        rebuild: false,
        dropped: 0,
        count: 0,
    };
    loop {
        match reader.next_event()? {
            Some(Event::TableEnd) => break,
            Some(Event::Row(row)) => {
                check_row(head, &row)?;
                if run.opts.without_users {
                    out.dropped += 1;
                    continue;
                }
                out.rebuild |= head.source != Source::UsersSpr || !users_as_is(head, &row);
                if out.spool.is_none() {
                    let parent = run
                        .database
                        .parent()
                        .ok_or_else(|| bad("users staging parent"))?;
                    out.spool = Some(io(tempfile::tempfile_in(parent))?);
                }
                let _lease = reserve(
                    run.opts.memory_budget.as_ref(),
                    encode_admission(&row, row.capacity())?,
                )?;
                let frame = Frame { row, _lease };
                put_frame(
                    out.spool.as_mut().ok_or_else(|| bad("users spool"))?,
                    head,
                    &frame.row,
                )?;
                out.count = out
                    .count
                    .checked_add(1)
                    .ok_or_else(|| bad("users count overflow"))?;
            }
            _ => return Err(bad("missing system table end")),
        }
    }
    Ok(out)
}

struct Frame {
    row: Vec<ColumnValue>,
    _lease: Option<Reservation>,
}
fn get_frame(file: &mut File, head: &TableHead, opts: &RestoreOptions<'_>) -> Result<Frame> {
    let mut length = [0; 8];
    io(file.read_exact(&mut length))?;
    let length = u64::from_le_bytes(length);
    if length > scaled(opts.limits.row_bytes, 8, scaled(ROW_NODES, 8, KIB64)?)? {
        return Err(bad("users frame limit"));
    }
    let nodes = length
        .checked_add(KIB64 - 1)
        .map(|n| n / KIB64)
        .and_then(|n| n.checked_add(scaled(head.columns.len() as u64, 4, 16).ok()?))
        .ok_or_else(|| bad("users replay node size overflow"))?;
    let node_bytes = scaled(nodes, std::mem::size_of::<BinNode>() as u64, 0)?;
    let workspace = scaled(length, 8, scaled(node_bytes, 4, MIB)?)?;
    let lease = reserve(opts.memory_budget.as_ref(), workspace)?;
    let mut reader = brace::binary::Reader::new(file.take(length));
    reader.begin_budget(ReadLimits {
        atom_bytes: length,
        retained_bytes: length,
        nodes,
    });
    let node = reader
        .read_value()
        .map_err(|_| bad("users spool encoding"))?
        .ok_or_else(|| bad("users spool row"))?;
    if reader
        .read_value()
        .map_err(|_| bad("users spool trailer"))?
        .is_some()
        || reader.into_inner().limit() != 0
    {
        return Err(bad("users spool frame length"));
    }
    let BinNode::List(list) = node else {
        return Err(bad("users spool list"));
    };
    let row = dtfile::rows::decode_row(&head.columns, &list, 0)?;
    Ok(Frame { row, _lease: lease })
}

fn projected(row: &UserRow, descr: &TableDescr) -> Result<Row> {
    use ibschema::values::{RefId, datetime_from_ticks};
    let changed = datetime_from_ticks(row.changed.ticks() as u64)?;
    named_row(
        descr,
        "V8USERS",
        &[
            ("ID", Value::Binary(RefId::from_uuid(row.id).0.to_vec())),
            ("NAME", Value::VarChar(row.name.clone())),
            ("DESCR", Value::VarChar(row.descr.clone())),
            (
                "OSNAME",
                row.osname.clone().map_or(Value::Null, Value::VarChar),
            ),
            ("CHANGED", Value::DateTime(changed)),
            (
                "ROLESID",
                Value::Numeric(Numeric::from_i64(i64::from(row.rolesid))),
            ),
            ("SHOW", Value::Bool(row.show)),
            ("DATA", Value::Image(BlobVal::Data(row.data.clone()))),
            ("EAUTH", Value::Bool(row.eauth)),
            ("ADMROLE", Value::Bool(row.admrole)),
            (
                "USSPRH",
                Value::Numeric(Numeric::from_i64(row.ussprh as i64)),
            ),
            ("EMAIL", Value::VarChar(row.email.clone())),
        ],
    )
}

fn short(text: &str) -> Result<String> {
    let mut units = 0;
    let mut end = text.len();
    for (index, c) in text.char_indices() {
        let next = units + c.len_utf16();
        if next > 128 {
            if units != 128 {
                return Err(bad("matching-key UTF-16 boundary unsupported"));
            }
            end = index;
            break;
        }
        units = next;
    }
    Ok(text
        .get(..end)
        .ok_or_else(|| bad("matching-key boundary"))?
        .to_owned())
}

fn insert_rebuilt(batches: &mut Batches<'_>, descr: &TableDescr, user: UserRow) -> Result<u64> {
    let row = projected(&user, descr)?;
    let estimate = user
        .data
        .len()
        .saturating_add((descr.indexes.len() + 2) * 4 * PAGE);
    batches
        .unit(estimate, "a rebuilt user")?
        .table("V8USERS")?
        .insert(&row)?;
    let text = v8users::data::decode(&user.data).map_err(|_| bad("invalid rebuilt user DATA"))?;
    let parsed = UserData::parse(&text.text).map_err(|_| bad("invalid rebuilt user DATA"))?;
    let mut count = 0u64;
    for pair in parsed
        .matching_keys()
        .map_err(|_| bad("invalid user matching keys"))?
    {
        let body = brace::to_text(&brace::Node::list(vec![
            brace::Node::guid(user.id),
            brace::Node::str(pair.0),
            brace::Node::str(pair.1),
        ]));
        let mut data = Vec::with_capacity(body.len() + 3);
        data.extend_from_slice(&[0xef, 0xbb, 0xbf]);
        data.extend_from_slice(body.as_bytes());
        let key = vec![
            Value::Binary(ibschema::values::RefId::from_uuid(user.id).0.to_vec()),
            Value::VarChar(short(pair.0)?),
            Value::VarChar(short(pair.1)?),
            Value::Image(BlobVal::Data(data)),
        ];
        batches
            .unit(
                body.len().saturating_add(4 * 4 * PAGE),
                "a user matching key",
            )?
            .table("V8USERSMATKEYS")?
            .insert(&key)?;
        count = count
            .checked_add(1)
            .ok_or_else(|| bad("matching-key count overflow"))?;
    }
    Ok(count)
}

fn reconstruct(row: &[ColumnValue], head: &TableHead, facts: &Facts) -> Result<UserRow> {
    let at = |name: &str| {
        head.columns
            .iter()
            .position(|c| c.name == name)
            .and_then(|i| row.get(i))
            .ok_or_else(|| bad("missing user column"))
    };
    let ColumnValue::Bytes(data) = at("DATA")? else {
        return Err(bad("user DATA type"));
    };
    let ColumnValue::DateTime(changed) = at("CHANGED")? else {
        return Err(bad("user CHANGED type"));
    };
    let changed = BslDate::from_ticks(ibschema::values::datetime_to_ticks(changed) as i64)
        .ok_or_else(|| bad("user CHANGED range"))?;
    let names: Vec<_> = facts.names.iter().map(String::as_str).collect();
    UserRow::rebuild(
        data,
        changed,
        &v8users::rebuild::RebuildContext {
            administration_roles: facts.administration.as_deref(),
            user_separators: &names,
        },
        &v8users::new_key(),
    )
    .map_err(|_| bad("invalid user DATA or unsupported projection; --without-users restores the base without them"))
}

impl Prepared {
    pub(super) fn write(
        mut self,
        head: &TableHead,
        descr: &TableDescr,
        batches: &mut Batches<'_>,
        facts: &Facts,
        opts: &RestoreOptions<'_>,
    ) -> Result<(u64, u64)> {
        let Some(file) = self.spool.as_mut() else {
            return Ok((0, 0));
        };
        io(file.seek(SeekFrom::Start(0)))?;
        let plan = if self.rebuild {
            None
        } else {
            Some(Plan::with_absent(descr, head, &["EMAIL"])?)
        };
        let mut service = None;
        let mut keys = 0u64;
        for ordinal in 0..self.count {
            let frame = get_frame(file, head, opts)?;
            let row = &frame.row;
            if let Some(plan) = &plan {
                let estimate = plan.estimate(row);
                let Frame { row, _lease } = frame;
                let row = plan.row(row)?;
                batches
                    .unit(estimate, "a user")?
                    .table("V8USERS")?
                    .insert(&row)?;
                drop(row);
                drop(_lease);
            } else {
                let data = head
                    .columns
                    .iter()
                    .position(|c| c.name == "DATA")
                    .and_then(|i| row.get(i));
                let Some(ColumnValue::Bytes(data)) = data else {
                    return Err(bad("user DATA type"));
                };
                if data.len() > v8users::MAX_DATA_BYTES {
                    return Err(bad("user DATA size limit"));
                }
                let _scratch = reserve(
                    opts.memory_budget.as_ref(),
                    facts.scratch_bytes(data.len())?,
                )?;
                let rebuilt = reconstruct(row, head, facts)?;
                if rebuilt.id.to_string() == SERVICE_ROW_ID {
                    if service.replace(ordinal).is_some() {
                        return Err(bad("duplicate user service row"));
                    }
                } else {
                    keys = keys
                        .checked_add(insert_rebuilt(batches, descr, rebuilt)?)
                        .ok_or_else(|| bad("matching-key count overflow"))?;
                }
            }
        }
        if let Some(want) = service {
            io(file.seek(SeekFrom::Start(0)))?;
            for ordinal in 0..=want {
                let frame = get_frame(file, head, opts)?;
                let row = &frame.row;
                if ordinal == want {
                    let data = head
                        .columns
                        .iter()
                        .position(|c| c.name == "DATA")
                        .and_then(|i| row.get(i));
                    let Some(ColumnValue::Bytes(data)) = data else {
                        return Err(bad("user DATA type"));
                    };
                    let _scratch = reserve(
                        opts.memory_budget.as_ref(),
                        facts.scratch_bytes(data.len())?,
                    )?;
                    keys = keys
                        .checked_add(insert_rebuilt(
                            batches,
                            descr,
                            reconstruct(row, head, facts)?,
                        )?)
                        .ok_or_else(|| bad("matching-key count overflow"))?;
                }
            }
        }
        Ok((self.count, keys))
    }
}
