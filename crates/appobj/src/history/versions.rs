use ibschema::DateTime;
use ibschema::Numeric;
use ibschema::fields::FieldValue;
use ibschema::names::purpose;
use ibschema::storage::{IndexId, TableId, TableInfo};
use ibschema::store::{Direction, ReadTxn, StoreResult};
use ibschema::values::{RefId, Uuid16};
use std::ops::Bound;

use super::{column, index_from};
use crate::Application;

#[derive(Clone, Debug)]
pub(crate) struct Version {
    pub data: [u8; 20],
    pub number: i64,
    pub change: i64,
    pub date: DateTime,
    pub user: Uuid16,
    pub user_name: String,
    pub user_full_name: String,
    pub comment: String,
    pub txn: [u8; 16],
    pub node_empty: bool,
}

pub(crate) struct HistTables<'a> {
    settings: TableId,
    s_index: IndexId,
    latest: TableId,
    l_index: IndexId,
    l_data: usize,
    l_hid: usize,
    versions: TableId,
    v_index: IndexId,
    v: &'a TableInfo,
    v_number: usize,
    v_date: usize,
    v_change: usize,
    v_user: usize,
    v_user_name: usize,
    v_user_full_name: usize,
    v_comment: usize,
    v_txn: usize,
    queue: TableId,
    after: TableId,
}

impl<'a> HistTables<'a> {
    pub(crate) fn of(app: &'a Application) -> Result<HistTables<'a>, String> {
        let map = app.store().storage();
        let table = |name: &str| {
            map.table_by_name(name)
                .and_then(|t| map.try_table(t).map(|ti| (t, ti)))
                .ok_or_else(|| format!("нет таблицы {name}"))
        };
        let (settings, s) = table(purpose::DATA_HISTORY_SETTINGS)?;
        let (latest, l) = table(purpose::DATA_HISTORY_LATEST_VERSIONS)?;
        let (versions, v) = table(purpose::DATA_HISTORY_VERSIONS)?;
        let (queue, q) = table(purpose::DATA_HISTORY_QUEUE0)?;
        let (after, a) = table(purpose::DATA_HISTORY_AFTER_WRITE_QUEUE)?;
        if [s, l, v, q, a].iter().any(|t| !t.separators.is_empty()) {
            return Err("разделённые таблицы истории".into());
        }
        Ok(HistTables {
            settings,
            s_index: index_from(s, column(s, "MetadataId")?)?,
            latest,
            l_index: index_from(l, column(l, "MetadataId")?)?,
            l_data: column(l, "DataId")?,
            l_hid: column(l, "HistoryDataId")?,
            versions,
            v_index: index_from(v, column(v, "HistoryDataId")?)?,
            v,
            v_number: column(v, "VersionNumber")?,
            v_date: column(v, "Date")?,
            v_change: column(v, "ChangeType")?,
            v_user: column(v, "UserId")?,
            v_user_name: column(v, "UserName")?,
            v_user_full_name: column(v, "UserFullName")?,
            v_comment: column(v, "Comment")?,
            v_txn: column(v, "Transaction")?,
            queue,
            after,
        })
    }

    pub(crate) fn has_settings(&self, r: &dyn ReadTxn, md: Uuid16) -> StoreResult<bool> {
        let key = [FieldValue::Uuid(md)];
        let mut rows = r.range(
            self.settings,
            self.s_index,
            Bound::Included(key.as_slice()),
            Bound::Included(key.as_slice()),
            Direction::Forward,
        )?;
        Ok(rows.next().transpose()?.is_some())
    }

    pub(crate) fn queues_empty(&self, r: &dyn ReadTxn) -> StoreResult<bool> {
        for t in [self.queue, self.after] {
            if r.scan(t)?.next().transpose()?.is_some() {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub(crate) fn versions(
        &self,
        r: &dyn ReadTxn,
        md: Uuid16,
        data: Option<[u8; 20]>,
        each: &mut dyn FnMut() -> Result<(), String>,
    ) -> StoreResult<Result<Vec<Version>, String>> {
        let mut key = vec![FieldValue::Uuid(md)];
        if let Some(d) = data {
            key.push(FieldValue::Binary(d.to_vec()));
        }
        let mut found: Vec<([u8; 20], FieldValue)> = Vec::new();
        for item in r.range(
            self.latest,
            self.l_index,
            Bound::Included(key.as_slice()),
            Bound::Included(key.as_slice()),
            Direction::Forward,
        )? {
            let (_, row) = item?;
            if let Err(e) = each() {
                return Ok(Err(e));
            }
            let Some(data) = bytes20(row.get(self.l_data)) else {
                return Ok(Err("ключ данных последней версии".into()));
            };
            let Some(hid) = row.get(self.l_hid).cloned() else {
                return Ok(Err("идентификатор истории".into()));
            };
            found.push((data, hid));
        }
        let mut out = Vec::new();
        for (data, hid) in found {
            let key = [hid];
            for item in r.range(
                self.versions,
                self.v_index,
                Bound::Included(key.as_slice()),
                Bound::Included(key.as_slice()),
                Direction::Forward,
            )? {
                let (_, row) = item?;
                if let Err(e) = each() {
                    return Ok(Err(e));
                }
                match self.version(&row, data) {
                    Ok(v) => out.push(v),
                    Err(e) => return Ok(Err(e)),
                }
            }
        }
        Ok(Ok(out))
    }

    fn version(&self, row: &[FieldValue], data: [u8; 20]) -> Result<Version, String> {
        let text = |at: usize, what: &str| match row.get(at) {
            Some(FieldValue::String(s)) => Ok(s.clone()),
            _ => Err(format!("поле {what} версии")),
        };
        let node = self
            .v
            .field_by_name("Node")
            .map(|f| usize::from(f.0))
            .ok_or("нет поля Node")?;
        Ok(Version {
            data,
            number: whole(row.get(self.v_number)).ok_or("номер версии")?,
            change: whole(row.get(self.v_change)).ok_or("вид изменения версии")?,
            date: match row.get(self.v_date) {
                Some(FieldValue::Date(d)) => *d,
                _ => return Err("дата версии".into()),
            },
            user: match row.get(self.v_user) {
                Some(FieldValue::Uuid(u)) => *u,
                Some(FieldValue::Binary(b)) => <[u8; 16]>::try_from(b.as_slice())
                    .map(|b| RefId(b).to_uuid())
                    .map_err(|_| "пользователь версии".to_string())?,
                _ => return Err("пользователь версии".into()),
            },
            user_name: text(self.v_user_name, "UserName")?,
            user_full_name: text(self.v_user_full_name, "UserFullName")?,
            comment: text(self.v_comment, "Comment")?,
            txn: match row.get(self.v_txn) {
                Some(FieldValue::Uuid(u)) => RefId::from_uuid(*u).0,
                Some(FieldValue::Binary(b)) => <[u8; 16]>::try_from(b.as_slice())
                    .map_err(|_| "транзакция версии".to_string())?,
                _ => return Err("транзакция версии".into()),
            },
            node_empty: matches!(row.get(node), Some(FieldValue::Undefined)),
        })
    }
}

fn whole(v: Option<&FieldValue>) -> Option<i64> {
    match v {
        Some(FieldValue::Number(n)) => number(n),
        _ => None,
    }
}

fn number(n: &Numeric) -> Option<i64> {
    n.to_string().parse::<i64>().ok()
}

fn bytes20(v: Option<&FieldValue>) -> Option<[u8; 20]> {
    match v {
        Some(FieldValue::Binary(b)) => <[u8; 20]>::try_from(b.as_slice()).ok(),
        _ => None,
    }
}
