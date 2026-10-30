use super::*;

type EngineTxn<'a> = Option<onecd::WriteTxn<'a>>;
self_cell::self_cell!(
    struct TxnCell {
        owner: Arc<onecd::Database>,
        #[covariant]
        dependent: EngineTxn,
    }
);

type Write1cd = Read1cd<TxnCell>;

fn ended() -> StoreError {
    StoreError::Unsupported("transaction ended".into())
}

impl TableSource for TxnCell {
    fn set_read_budget(&mut self, _: memory_budget::MemoryBudget) -> StoreResult<()> {
        Err(StoreError::Unsupported(
            "admitted reads require a read snapshot".into(),
        ))
    }

    fn table(&self, name: &str) -> StoreResult<onecd::TableRead<'_>> {
        self.borrow_dependent()
            .as_ref()
            .ok_or_else(ended)?
            .read_table(name)
            .map_err(engine_err)
    }
}

impl Store1cd {
    pub(super) fn begin_write(&self, wait: Duration) -> StoreResult<Box<dyn WriteTxn>> {
        if let Some(reason) = &self.refusal {
            return Err(StoreError::Unsupported(reason.clone()));
        }
        let ticket = self.gate.acquire(wait)?;
        let cell = TxnCell::try_new(Arc::clone(&self.db), |db| db.write().map(Some))
            .map_err(|e| write_err(&self.map, &self.phys, e))?;
        Ok(Box::new(self.transaction(cell, Some(ticket))))
    }
}

impl PhysMap {
    fn table_by_phys(&self, name: &str) -> Option<TableId> {
        self.tables
            .iter()
            .position(|t| same_name(&t.name, name))
            .and_then(|n| u32::try_from(n).ok())
            .map(TableId)
    }

    fn index_by_phys(&self, t: TableId, name: &str) -> Option<IndexId> {
        self.tables
            .get(usize::try_from(t.0).ok()?)?
            .write_indexes
            .iter()
            .find(|(n, _)| same_name(n, name))
            .map(|(_, i)| *i)
    }

    fn field_by_column<'a>(
        &self,
        map: &'a StorageMap,
        t: TableId,
        column: &str,
    ) -> Option<&'a FieldInfo> {
        let ti = map.try_table(t)?;
        let col = ti.columns.iter().position(|c| same_name(&c.name, column))?;
        ti.fields.iter().find(|f| f.columns.contains(&col))
    }
}

fn write_err(map: &StorageMap, phys: &PhysMap, e: onecd::Error) -> StoreError {
    let logical_field = |table: &str, column: &str| {
        let t = phys.table_by_phys(table)?;
        let ti = map.try_table(t)?;
        let fi = phys.field_by_column(map, t, column)?;
        Some((ti.logical.clone(), fi.name.clone()))
    };
    if let onecd::Error::Constraint(c) = &e {
        use onecd::Constraint as C;
        match c {
            C::UniqueViolation { table, index } => {
                let logical = phys.table_by_phys(table).and_then(|t| {
                    let ti = map.try_table(t)?;
                    let i = phys.index_by_phys(t, index)?;
                    let ix = match i {
                        IndexId::Id => ti.key_index.as_ref(),
                        IndexId::Named(k) => ti.indexes.get(usize::from(k)),
                    }?;
                    Some(StoreError::UniqueViolation {
                        table: t,
                        index: i,
                        table_name: ti.logical.clone(),
                        index_name: ix.name.clone(),
                    })
                });
                if let Some(error) = logical {
                    return error;
                }
            }
            C::NullNotAllowed { table, field } => {
                if let Some((table, field)) = logical_field(table, field) {
                    return StoreError::NullNotAllowed { table, field };
                }
            }
            C::ValueDoesNotFit { table, field, why } => {
                if let Some((table, field)) = logical_field(table, field) {
                    return StoreError::ValueDoesNotFit {
                        table,
                        field,
                        why: why.clone(),
                    };
                }
            }
            C::TypeMismatch { table, field } => {
                if let Some((table, field)) = logical_field(table, field) {
                    return StoreError::ValueDoesNotFit {
                        table,
                        field,
                        why: "wrong value type".into(),
                    };
                }
            }
            C::WrongArity {
                table,
                expected,
                got,
            } => {
                if let Some(ti) = phys.table_by_phys(table).and_then(|t| map.try_table(t)) {
                    return StoreError::ValueDoesNotFit {
                        table: ti.logical.clone(),
                        field: String::new(),
                        why: format!("expected {expected} columns, got {got}"),
                    };
                }
            }
            _ => {}
        }
    }
    match e {
        onecd::Error::Unsupported(reason) => StoreError::Unsupported(reason.to_string()),
        other => engine_err(other),
    }
}

impl Write1cd {
    fn writable(&self, t: TableId) -> StoreResult<(&TableInfo, &PhysTable)> {
        let (ti, p) = self.table(t)?;
        if !p.writable {
            return Err(StoreError::Unsupported(format!(
                "{}: schema does not cover every physical column",
                ti.logical
            )));
        }
        Ok((ti, p))
    }

    fn encode(&self, t: TableId, row: &Row) -> StoreResult<onecd::Row> {
        let (ti, p) = self.writable(t)?;
        if row.len() != ti.fields.len() {
            return Err(StoreError::ValueDoesNotFit {
                table: ti.logical.clone(),
                field: String::new(),
                why: format!("expected {} fields, got {}", ti.fields.len(), row.len()),
            });
        }
        let mut out = vec![onecd::Value::Null; p.cols.len()];
        for (k, (fi, value)) in ti.fields.iter().zip(row).enumerate() {
            if fields::is_version(fi) {
                continue;
            }
            let f = FieldId(
                u16::try_from(k).map_err(|_| StoreError::Unsupported("too many fields".into()))?,
            );
            let cells = fields::encode(ti, f, value).map_err(StoreError::from_schema)?;
            for (col, cell) in fi.columns.clone().zip(cells) {
                let invalid_map = || {
                    StoreError::Unsupported(format!(
                        "{}: invalid physical column mapping",
                        ti.logical
                    ))
                };
                let column = ti.columns.get(col).ok_or_else(invalid_map)?;
                let pos = p.cols.get(col).ok_or_else(invalid_map)?;
                let dest = out.get_mut(*pos).ok_or_else(invalid_map)?;
                *dest = colval::to_onecd(&cell, column.ty).map_err(|e| {
                    StoreError::ValueDoesNotFit {
                        table: ti.logical.clone(),
                        field: fi.name.clone(),
                        why: e.to_string(),
                    }
                })?;
            }
        }
        Ok(out)
    }

    fn change<R>(
        &mut self,
        f: impl FnOnce(&mut onecd::WriteTxn<'_>) -> onecd::Result<R>,
    ) -> StoreResult<R> {
        let (map, phys) = (&self.map, &self.phys);
        self.txn.with_dependent_mut(|_, txn| {
            let txn = txn.as_mut().ok_or_else(ended)?;
            let result = f(txn);
            if matches!(result, Err(onecd::Error::TxnTooLarge)) {
                txn.abort();
            }
            result.map_err(|e| write_err(map, phys, e))
        })
    }
}

impl WriteTxn for Write1cd {
    fn insert(&mut self, t: TableId, row: &Row) -> StoreResult<RowHandle> {
        let row = self.encode(t, row)?;
        let name = self.writable(t)?.1.name.clone();
        self.change(|txn| txn.table(&name)?.insert(&row))
            .map(handle)
    }

    fn update(&mut self, t: TableId, h: RowHandle, row: &Row) -> StoreResult<()> {
        let row = self.encode(t, row)?;
        let name = self.writable(t)?.1.name.clone();
        let id = u32::try_from(h.0).map_err(|_| StoreError::NotFound(format!("row {}", h.0)))?;
        self.change(|txn| txn.table(&name)?.update(id, &row))
    }

    fn delete(&mut self, t: TableId, h: RowHandle) -> StoreResult<()> {
        let name = self.writable(t)?.1.name.clone();
        let id = u32::try_from(h.0).map_err(|_| StoreError::NotFound(format!("row {}", h.0)))?;
        self.change(|txn| txn.table(&name)?.delete(id))
    }

    fn commit(mut self: Box<Self>) -> StoreResult<()> {
        self.txn.with_dependent_mut(|_, txn| {
            txn.take()
                .ok_or_else(ended)?
                .commit()
                .map_err(|e| write_err(&self.map, &self.phys, e))
        })
    }
}
