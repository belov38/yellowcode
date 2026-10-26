use crate::{Error, Infobase, OpenMode, Result, colval, folder, raw::FolderFile};
use ibschema::columns::{ColRole, ColType, Column, Origin, Part};
use ibschema::store::{StoreError, WriterTicket};
use ibschema::values::ColumnValue;
use std::time::Duration;

pub struct ServiceTxn<'ib> {
    txn: onecd::WriteTxn<'ib>,
    ticket: WriterTicket,
}

impl Infobase {
    pub fn service_write(&self, wait: Duration) -> Result<ServiceTxn<'_>> {
        if self.mode() != OpenMode::ReadWrite {
            return Err(Error::Unsupported(
                "service writes require ReadWrite mode".into(),
            ));
        }
        if let Some(why) = self.write_refusal().filter(|why| *why != "extensions") {
            return Err(Error::Unsupported(why.into()));
        }
        let ticket = self.gate().acquire(wait).map_err(|e| match e {
            StoreError::WriterBusy => Error::WriterBusy,
            other => Error::Store(other),
        })?;
        let txn = self.raw().db().write()?;
        Ok(ServiceTxn { txn, ticket })
    }
}

impl ServiceTxn<'_> {
    pub fn user_columns(&self) -> Result<Vec<Column>> {
        let t = self.txn.read_table("V8USERS")?;
        t.descr()
            .fields
            .iter()
            .enumerate()
            .map(|(i, f)| {
                let ty = colval::col_type_of(f).ok_or_else(|| {
                    Error::Unsupported(format!("V8USERS.{}: unsupported column type", f.name))
                })?;
                let role = match ty {
                    ColType::B(_) | ColType::VB(_) | ColType::I => ColRole::Binary,
                    ColType::L => ColRole::Bool,
                    ColType::N(..) => ColRole::Number,
                    ColType::NC(_) | ColType::NVC(_) | ColType::NT => ColRole::Str,
                    ColType::DT => ColRole::DateTime,
                    ColType::RV => ColRole::RowVersion,
                };
                Ok(Column {
                    name: f.name.clone(),
                    ty,
                    role,
                    nullable: f.nullable,
                    origin: Origin::Field(i),
                    part: Part::Single,
                })
            })
            .collect()
    }

    pub fn users(&self) -> Result<Vec<(onecd::RowId, Vec<ColumnValue>)>> {
        let cols = self.user_columns()?;
        let t = self.txn.read_table("V8USERS")?;
        t.scan()
            .map(|entry| {
                let (id, row) = entry?;
                if row.len() != cols.len() {
                    return Err(Error::Inconsistent(
                        "V8USERS: row/descriptor arity differs".into(),
                    ));
                }
                let values = row
                    .iter()
                    .zip(&cols)
                    .map(|(v, c)| colval::from_onecd(v, c.ty, &mut |r| t.blob_bytes(r)))
                    .collect::<Result<_>>()?;
                Ok((id, values))
            })
            .collect()
    }

    fn encode_user(&self, row: &[ColumnValue]) -> Result<onecd::Row> {
        let cols = self.user_columns()?;
        if cols.len() != row.len() {
            return Err(Error::Inconsistent(format!(
                "V8USERS: expected {} columns, got {}",
                cols.len(),
                row.len()
            )));
        }
        cols.iter()
            .zip(row)
            .map(|(c, v)| {
                c.check(v)
                    .map_err(|why| Error::Inconsistent(format!("V8USERS.{}: {why}", c.name)))?;
                colval::to_onecd(v, c.ty)
            })
            .collect()
    }

    fn change<T>(
        &mut self,
        f: impl FnOnce(&mut onecd::TableWrite<'_>) -> onecd::Result<T>,
    ) -> Result<T> {
        let result = self.txn.table("V8USERS").and_then(|mut t| f(&mut t));
        if matches!(result, Err(onecd::Error::TxnTooLarge)) {
            self.txn.abort();
        }
        result.map_err(Error::from)
    }

    pub fn insert_user(&mut self, row: &[ColumnValue]) -> Result<onecd::RowId> {
        let row = self.encode_user(row)?;
        self.change(|t| t.insert(&row))
    }

    pub fn update_user(&mut self, id: onecd::RowId, row: &[ColumnValue]) -> Result<()> {
        let row = self.encode_user(row)?;
        self.change(|t| t.update(id, &row))
    }

    pub fn delete_user(&mut self, id: onecd::RowId) -> Result<()> {
        self.change(|t| t.delete(id))
    }

    pub fn param(&self, name: &str) -> Result<Option<Vec<u8>>> {
        Ok(folder::read_file(&self.txn, "PARAMS", name)?.map(|f| f.data))
    }

    pub fn put_param(&mut self, name: &str, stored: &[u8], at: onecd::DateTime) -> Result<()> {
        folder::write_file(
            &mut self.txn,
            "PARAMS",
            &FolderFile {
                name: name.into(),
                creation: at,
                modified: at,
                attributes: onecd::Numeric::from_i64(0),
                data_size: onecd::Numeric::from_i64(0),
                data: stored.to_vec(),
            },
        )
    }

    pub fn delete_param(&mut self, name: &str) -> Result<bool> {
        folder::delete_file(&mut self.txn, "PARAMS", name)
    }

    pub fn commit(self) -> Result<()> {
        let result = self.txn.commit();
        drop(self.ticket);
        result.map_err(Error::from)
    }
}
