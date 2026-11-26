use super::job::{Job, TYPE, Values};
use super::store::{EMPTY_PARAMETERS, JobRow, column};
use super::{Read, read_rows, refusal};
use crate::Application;
use crate::session::call_ctx;
use crate::txn::CallTxn;
use bsl_vm::{BslDate, CallError, CallResult, Execution, HostError, Value};
use ibschema::fields::FieldValue as FV;
use ibschema::names::field as f;
use ibschema::storage::TableInfo;
use ibschema::values::{RefId, Uuid16};
use ibschema::{DateTime, Numeric};
use std::rc::Rc;
use std::sync::Arc;

const COMMIT: &str = "запись регламентного задания";

const PAGE: usize = 512;

fn refuse(what: &str) -> CallError {
    refusal(TYPE, what).into()
}

fn transaction(ex: &mut Execution, member: &str) -> CallResult<Rc<CallTxn>> {
    if !call_ctx(ex).is_some_and(|c| c.sess.user().no_users) {
        return Err(refuse(&format!("{member}: сеанс с пользователями")));
    }
    match CallTxn::of_call(ex) {
        Some(txn) if txn.innermost_explicit() => {
            txn.check().map_err(|e| e.call_error(ex))?;
            Ok(txn)
        }
        _ => Err(refuse(&format!("{member}: вне транзакции"))),
    }
}

fn rows(ex: &mut Execution, app: &Arc<Application>, member: &str) -> CallResult<Vec<JobRow>> {
    read_rows(ex, app).map_err(|e| match e {
        Read::Call(e) => e,
        Read::Refused(why) => refuse(&format!("{member}: {why}")),
    })
}

pub(crate) fn write(ex: &mut Execution, job: &Job) -> CallResult<()> {
    const MEMBER: &str = "Записать";
    if job.deleted.get() {
        return Err(refuse("Записать после Удалить: не наблюдалось"));
    }
    let txn = transaction(ex, MEMBER)?;
    let app = job.app.clone();
    let values = job.values()?;
    let now = now(ex)?;
    let all = rows(ex, &app, MEMBER)?;
    let (table, handle, row) = match job.stored() {
        Some(r) => {
            let cur = all
                .iter()
                .find(|c| c.id == r.id)
                .ok_or_else(|| refuse("Записать: строки задания нет в базе"))?;
            if cur.version != r.version {
                return Err(refuse(
                    "Записать: строка записана другим объектом после чтения",
                ));
            }
            let ti = app.store().storage().table(cur.at.table).clone();
            let row = fill(&ti, cur.at.fields.clone(), &values, Some(cur.version), now)
                .map_err(|why| refuse(&format!("Записать: {why}")))?;
            (cur.at.table, Some(cur.at.handle), row)
        }
        None => {
            let md = job.md();
            let form = all
                .iter()
                .find(|r| r.md == md && r.predefined)
                .ok_or_else(|| refuse("Записать: нет строки предопределенного задания объекта"))?;
            let ti = app.store().storage().table(form.at.table).clone();
            let mut fields = form.at.fields.clone();
            let id = job.id();
            set(&ti, &mut fields, f::ID, |old| uuid_like(old, id))
                .and_then(|()| set(&ti, &mut fields, f::PREDEFINED, |_| FV::Bool(false)))
                .and_then(|()| set(&ti, &mut fields, f::RESTART_ATTEMPT_NUMBER, |_| number(0)))
                .and_then(|()| set(&ti, &mut fields, f::STATE, |_| number(0)))
                .and_then(|()| {
                    set(&ti, &mut fields, f::START_TIME, |_| {
                        FV::Date(DateTime::EMPTY)
                    })
                })
                .and_then(|()| {
                    set(&ti, &mut fields, f::FINISH_TIME, |_| {
                        FV::Date(DateTime::EMPTY)
                    })
                })
                .map_err(|why| refuse(&format!("Записать: {why}")))?;
            let row = fill(&ti, fields, &values, None, now)
                .map_err(|why| refuse(&format!("Записать: {why}")))?;
            (form.at.table, None, row)
        }
    };
    let written = txn.with_write(&*app, |w| match handle {
        Some(h) => w.update(table, h, &row),
        None => w.insert(table, &row).map(|_| ()),
    });
    if let Err(e) = written {
        txn.poison();
        return Err(e.call_error(ex));
    }
    txn.refuse_commit(COMMIT.to_string());
    let id = job.id();
    let fresh = rows(ex, &app, MEMBER)?
        .into_iter()
        .find(|r| r.id == id)
        .ok_or_else(|| refuse("Записать: записанной строки нет"))?;
    *job.origin.borrow_mut() = super::job::Origin::Stored(Box::new(fresh));
    *job.edit.borrow_mut() = Default::default();
    Ok(())
}

pub(crate) fn delete(ex: &mut Execution, job: &Job) -> CallResult<()> {
    const MEMBER: &str = "Удалить";
    if job.deleted.get() {
        return Err(refuse("Удалить после Удалить: не наблюдалось"));
    }
    let Some(r) = job.stored() else {
        return Err(refuse("Удалить: новое задание не записано"));
    };
    let txn = transaction(ex, MEMBER)?;
    if r.predefined {
        return Err(predefined_deletion(&job.values()?.description));
    }
    let app = job.app.clone();
    let cur = rows(ex, &app, MEMBER)?
        .into_iter()
        .find(|c| c.id == r.id)
        .ok_or_else(|| refuse("Удалить: строки задания нет в базе"))?;
    if cur.version != r.version {
        return Err(refuse(
            "Удалить: строка записана другим объектом после чтения",
        ));
    }
    let removed = txn.with_write(&*app, |w| w.delete(cur.at.table, cur.at.handle));
    if let Err(e) = removed {
        txn.poison();
        return Err(e.call_error(ex));
    }
    txn.refuse_commit(COMMIT.to_string());
    job.deleted.set(true);
    Ok(())
}

fn predefined_deletion(description: &str) -> CallError {
    if !description.is_empty() {
        return refuse("Удалить: предопределенное задание с наименованием");
    }
    HostError::text(
        "Нельзя удалить предопределенное регламентное задание ",
        "Cannot delete a predefined scheduled job ",
    )
    .into()
}

fn now(ex: &mut Execution) -> CallResult<BslDate> {
    match ex.call_global("ТекущаяДата", &mut [])? {
        Value::Date(d) => Ok(d),
        _ => Err(HostError::text("internal: ТекущаяДата", "internal: ТекущаяДата").into()),
    }
}

fn number(n: i64) -> FV {
    FV::Number(Numeric::from_i64(n))
}

fn uuid_like(old: &FV, id: Uuid16) -> FV {
    match old {
        FV::Binary(_) => FV::Binary(RefId::from_uuid(id).0.to_vec()),
        _ => FV::Uuid(id),
    }
}

fn bytes_like(old: &FV, b: Vec<u8>) -> FV {
    match old {
        FV::Binary(_) => FV::Binary(b),
        _ => FV::Storage(b),
    }
}

fn set(
    ti: &TableInfo,
    fields: &mut [FV],
    name: &str,
    value: impl FnOnce(&FV) -> FV,
) -> Result<(), String> {
    let at = column(ti, name)?;
    let slot = fields
        .get_mut(at)
        .ok_or_else(|| format!("нет поля {name} в строке"))?;
    *slot = value(slot);
    Ok(())
}

fn fill(
    ti: &TableInfo,
    mut fields: Vec<FV>,
    v: &Values,
    previous: Option<i64>,
    now: BslDate,
) -> Result<Vec<FV>, String> {
    let text = v
        .schedule
        .to_internal()
        .map_err(|why| format!("Расписание: {why}"))?;
    let container = activation(&text, now)?;
    let restart_count = v.restart_count;
    let restart_period = v.restart_period;
    set(ti, &mut fields, f::DESCRIPTION, |_| {
        FV::String(v.description.clone())
    })?;
    set(ti, &mut fields, f::JOB_KEY, |_| FV::String(v.key.clone()))?;
    set(ti, &mut fields, f::USER_NAME, |_| {
        FV::String(v.user_name.clone())
    })?;
    set(ti, &mut fields, f::USE, |_| FV::Bool(v.use_))?;
    set(ti, &mut fields, f::RESTART_COUNT, |_| number(restart_count))?;
    set(ti, &mut fields, f::RESTART_PERIOD, |_| {
        number(restart_period)
    })?;
    set(ti, &mut fields, f::PARAMETERS, |old| {
        bytes_like(old, EMPTY_PARAMETERS.to_vec())
    })?;
    set(ti, &mut fields, f::ACTIVATION_CONDITION, |old| {
        bytes_like(old, container)
    })?;
    set(ti, &mut fields, f::VERSION, |_| {
        number(next_version(previous))
    })?;
    Ok(fields)
}

fn next_version(previous: Option<i64>) -> i64 {
    previous.map_or(1, |v| v + 1)
}

pub(crate) fn activation(text: &str, now: BslDate) -> Result<Vec<u8>, String> {
    let p = now.parts();
    let time = v8cont::V8Time::from_civil(
        i32::try_from(p.year).map_err(|_| "время записи")?,
        p.month,
        p.day,
        p.hour,
        p.minute,
        p.second,
    )
    .ok_or("время записи")?;
    let mut data = vec![0xef, 0xbb, 0xbf];
    data.extend_from_slice(text.as_bytes());
    let block = |out: &mut Vec<u8>, len: usize, page: usize| {
        out.extend_from_slice(format!("\r\n{len:08x} {page:08x} 7fffffff \r\n").as_bytes());
    };
    let mut out = Vec::with_capacity(1200 + data.len());
    out.extend_from_slice(&[0xff, 0xff, 0xff, 0x7f, 0x00, 0x02, 0x00, 0x00]);
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    let toc_at = out.len();
    let header_at = toc_at + 31 + PAGE;
    let header_len = 8 + 8 + 4 + 16 + 4;
    let data_at = header_at + 31 + header_len;
    block(&mut out, 12, PAGE);
    let addr = |a: usize| u32::try_from(a).map(u32::to_le_bytes).map_err(|_| "адрес");
    out.extend_from_slice(&addr(header_at)?);
    out.extend_from_slice(&addr(data_at)?);
    out.extend_from_slice(&0x7fff_ffffu32.to_le_bytes());
    out.resize(header_at, 0);
    block(&mut out, header_len, header_len);
    out.extend_from_slice(&time.0.to_le_bytes());
    out.extend_from_slice(&time.0.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    for u in "$Stream$".encode_utf16() {
        out.extend_from_slice(&u.to_le_bytes());
    }
    out.extend_from_slice(&[0; 4]);
    let page = data.len().max(PAGE);
    block(&mut out, data.len(), page);
    let end = out.len() + page;
    out.extend_from_slice(&data);
    out.resize(end, 0);
    Ok(out)
}
