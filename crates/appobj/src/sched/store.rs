use super::schedule::Schedule;
use crate::Application;
use ibschema::fields::FieldValue;
use ibschema::names::{field as f, purpose};
use ibschema::storage::{TableId, TableInfo};
use ibschema::store::{ReadTxn, RowHandle};
use ibschema::values::{RefId, Uuid16};

#[derive(Clone, Debug)]
pub(crate) struct JobRow {
    pub id: Uuid16,
    pub md: Uuid16,
    pub description: String,
    pub key: String,
    pub predefined: bool,
    pub use_: bool,
    pub user_name: String,
    pub restart_count: i64,
    pub restart_period: i64,
    pub parameters_empty: bool,
    pub schedule: Result<Schedule, String>,
    pub version: i64,
    pub at: Stored,
}

#[derive(Clone, Debug)]
pub(crate) struct Stored {
    pub table: TableId,
    pub handle: RowHandle,
    pub fields: ibschema::Row,
}

pub(crate) const EMPTY_PARAMETERS: [u8; 2] = [0x41, 0x20];

pub(crate) fn job_tables(app: &Application) -> Vec<(TableId, &TableInfo)> {
    let map = app.store().storage();
    map.tables()
        .iter()
        .enumerate()
        .filter(|(_, ti)| {
            ti.logical
                .strip_prefix(purpose::SCHEDULED_JOBS)
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        })
        .filter_map(|(i, ti)| {
            u32::try_from(i)
                .ok()
                .map(|i| (ibschema::storage::TableId(i), ti))
        })
        .collect()
}

pub(crate) fn column(ti: &TableInfo, name: &str) -> Result<usize, String> {
    ti.field_by_name(name)
        .map(|f| usize::from(f.0))
        .ok_or_else(|| format!("нет поля {name} таблицы {}", ti.logical))
}

fn bytes(v: Option<&FieldValue>) -> Option<&[u8]> {
    match v? {
        FieldValue::Binary(b) | FieldValue::Storage(b) => Some(b),
        _ => None,
    }
}

fn text(v: Option<&FieldValue>) -> Result<String, String> {
    match v {
        Some(FieldValue::String(s)) => Ok(s.clone()),
        _ => Err("строковое поле".into()),
    }
}

fn flag(v: Option<&FieldValue>) -> Result<bool, String> {
    match v {
        Some(FieldValue::Bool(b)) => Ok(*b),
        _ => Err("булево поле".into()),
    }
}

pub(crate) fn int(v: Option<&FieldValue>) -> Result<i64, String> {
    match v {
        Some(FieldValue::Number(n)) => n.to_string().parse::<i64>().map_err(|_| "число".into()),
        _ => Err("числовое поле".into()),
    }
}

fn uuid_of(v: Option<&FieldValue>) -> Result<Uuid16, String> {
    match v {
        Some(FieldValue::Uuid(u)) => Ok(*u),
        Some(FieldValue::Binary(b)) => <[u8; 16]>::try_from(b.as_slice())
            .map(|b| RefId(b).to_uuid())
            .map_err(|_| "идентификатор не 16 байт".to_string()),
        _ => Err("идентификатор".into()),
    }
}

pub(crate) fn schedule_of(blob: &[u8]) -> Result<Schedule, String> {
    let c = v8cont::Container::open(blob).map_err(|e| format!("контейнер расписания: {e}"))?;
    let entry = c
        .find("$Stream$")
        .ok_or("контейнер расписания без $Stream$")?;
    let data = c
        .read_data(entry)
        .map_err(|e| format!("контейнер расписания: {e}"))?;
    let text = std::str::from_utf8(&data).map_err(|_| "расписание не UTF-8".to_string())?;
    Schedule::from_internal(text.trim_end_matches('\0'))
}

pub(crate) fn read_all(app: &Application, txn: &dyn ReadTxn) -> Result<Vec<JobRow>, String> {
    let tables = job_tables(app);
    let mut out = Vec::new();
    for (t, ti) in tables {
        let c = |name: &str| column(ti, name);
        let (id, desc, key, md) = (
            c(f::ID)?,
            c(f::DESCRIPTION)?,
            c(f::JOB_KEY)?,
            c(f::METADATA_ID)?,
        );
        let (pre, params, user) = (c(f::PREDEFINED)?, c(f::PARAMETERS)?, c(f::USER_NAME)?);
        let (cond, use_, count) = (
            c(f::ACTIVATION_CONDITION)?,
            c(f::USE)?,
            c(f::RESTART_COUNT)?,
        );
        let (period, version) = (c(f::RESTART_PERIOD)?, c(f::VERSION)?);
        let rows = txn
            .scan(t)
            .map_err(|e| format!("чтение {}: {e}", ti.logical))?;
        for r in rows {
            let (handle, row) = r.map_err(|e| format!("чтение {}: {e}", ti.logical))?;
            let schedule = bytes(row.get(cond))
                .ok_or_else(|| "расписание: нет данных".to_string())
                .and_then(schedule_of);
            out.push(JobRow {
                id: uuid_of(row.get(id))?,
                md: uuid_of(row.get(md))?,
                description: text(row.get(desc))?,
                key: text(row.get(key))?,
                predefined: flag(row.get(pre))?,
                use_: flag(row.get(use_))?,
                user_name: text(row.get(user))?,
                restart_count: int(row.get(count))?,
                restart_period: int(row.get(period))?,
                parameters_empty: bytes(row.get(params)) == Some(&EMPTY_PARAMETERS[..]),
                schedule,
                version: int(row.get(version))?,
                at: Stored {
                    table: t,
                    handle,
                    fields: row,
                },
            });
        }
    }
    Ok(out)
}
