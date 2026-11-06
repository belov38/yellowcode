use super::{TempTables, folded_name_char};
use crate::model::{invalid_size, size_add, size_vec};
use memory_budget::{Evictor, Reservation};
use query_ir::bind::temp_provenance::{
    MaterializedOrigins, TempGeneration, TempOriginEvent, TempOriginProvider,
};
use query_ir::host::TxnId;
use query_ir::{ExecError, TempSchema};
use std::cell::Cell;
use std::ops::ControlFlow;
use std::sync::Arc;

pub(crate) struct OriginAdapter<'a> {
    tables: &'a TempTables,
}
impl<'a> OriginAdapter<'a> {
    pub(crate) fn new(tables: &'a TempTables) -> Self {
        Self { tables }
    }
    fn lookup(
        &self,
        name: &str,
        step: &mut dyn FnMut(usize) -> ControlFlow<()>,
    ) -> Option<&'a super::TempTable> {
        for table in self.tables.tables.values() {
            if step(1).is_break() {
                return None;
            }
            let mut a = table.schema.name.chars().map(folded_name_char);
            let mut b = name.chars().map(folded_name_char);
            loop {
                if step(1).is_break() {
                    return None;
                }
                match (a.next(), b.next()) {
                    (None, None) => return Some(table),
                    (Some(a), Some(b)) if a == b => {}
                    _ => break,
                }
            }
        }
        None
    }
}

unsafe impl TempOriginProvider for OriginAdapter<'_> {
    fn destination_header<'a>(
        &'a self,
        name: &str,
        step: &mut dyn FnMut(usize) -> ControlFlow<()>,
    ) -> Option<(&'a Arc<TempSchema>, &'a TempGeneration)> {
        let table = self.lookup(name, step)?;
        Some((&table.schema, table.origins.generation.as_ref()?))
    }
    fn visit_committed<'a>(
        &'a self,
        name: &str,
        visit: &mut dyn FnMut(TempOriginEvent<'a>) -> ControlFlow<()>,
    ) -> Option<(&'a Arc<TempSchema>, &'a TempGeneration, usize)> {
        let table = self.lookup(name, &mut |n| visit(TempOriginEvent::Step(n)))?;
        let generation = table.origins.generation.as_ref()?;
        let mut end = 0usize;
        let mut seen = false;
        for run in &table.origins.runs {
            if visit(TempOriginEvent::Step(1)).is_break() {
                return None;
            }
            let proof = run.proof.as_ref()?;
            if run.first != end || (run.count == 0 && seen) {
                return None;
            }
            end = run.first.checked_add(run.count)?;
            if end > table.rows.len() {
                return None;
            }
            if visit(TempOriginEvent::Run {
                rows: run.first..end,
                proof,
            })
            .is_break()
            {
                return None;
            }
            seen = true;
        }
        if !seen || end != table.rows.len() {
            return None;
        }
        Some((&table.schema, generation, end))
    }
}

pub(super) struct OriginRun {
    first: usize,
    count: usize,
    txn: Option<TxnId>,
    proof: Option<MaterializedOrigins>,
    bytes: u64,
}
#[derive(Default)]
pub(super) struct OriginLedger {
    pub(super) generation: Option<TempGeneration>,
    pub(super) runs: Vec<OriginRun>,
    pub(super) bytes: u64,
}
impl std::fmt::Debug for OriginLedger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OriginLedger")
            .field("anchored", &self.generation.is_some())
            .field("runs", &self.runs.len())
            .field(
                "transaction_runs",
                &self.runs.iter().filter(|r| r.txn.is_some()).count(),
            )
            .field("bytes", &self.bytes)
            .finish()
    }
}
impl OriginLedger {
    pub(super) fn for_put(
        proof: Option<MaterializedOrigins>,
        count: usize,
        txn: Option<TxnId>,
        stage: &TempWriteStage,
    ) -> Result<Self, ExecError> {
        let Some(proof) = proof else {
            return Ok(Self::default());
        };
        let payload = proof.retained_bytes()?;
        let bytes = size_add(
            size_vec(1, std::mem::size_of::<OriginRun>())?,
            size_add(payload, proof.generation().retained_bytes()?)?,
        )?;
        stage.grow(bytes)?;
        let generation = Some(proof.generation().clone());
        let runs = vec![OriginRun {
            first: 0,
            count,
            txn,
            proof: Some(proof),
            bytes: payload,
        }];
        Ok(Self {
            generation,
            runs,
            bytes,
        })
    }
    pub(super) fn prepare_append(
        &self,
        proof: Option<MaterializedOrigins>,
        first: usize,
        count: usize,
        txn: Option<TxnId>,
        stage: &TempWriteStage,
    ) -> Result<OriginAppend, ExecError> {
        if self.generation.is_none() || count == 0 {
            return Ok(OriginAppend {
                run: None,
                replacement: None,
                next_bytes: self.bytes,
            });
        }
        first.checked_add(count).ok_or_else(invalid_size)?;
        let bytes = proof
            .as_ref()
            .map_or(Ok(0), MaterializedOrigins::retained_bytes)?;
        stage.grow(bytes)?;
        let mut next_bytes = size_add(self.bytes, bytes)?;
        let replacement = if self.runs.len() == self.runs.capacity() {
            let capacity = self
                .runs
                .capacity()
                .checked_mul(2)
                .ok_or_else(invalid_size)?
                .max(self.runs.len().checked_add(1).ok_or_else(invalid_size)?)
                .max(4);
            let backing = size_vec(capacity, std::mem::size_of::<OriginRun>())?;
            stage.grow(backing)?;
            let old = size_vec(self.runs.capacity(), std::mem::size_of::<OriginRun>())?;
            next_bytes = size_add(
                next_bytes,
                backing.checked_sub(old).ok_or_else(invalid_size)?,
            )?;
            Some(Vec::with_capacity(capacity))
        } else {
            None
        };
        Ok(OriginAppend {
            run: Some(OriginRun {
                first,
                count,
                txn,
                proof,
                bytes,
            }),
            replacement,
            next_bytes,
        })
    }
    pub(super) fn rollback(&mut self, ledger: &crate::model::RowLedger) -> u64 {
        let mut freed = 0;
        self.runs.retain_mut(|run| {
            if run.count == 0 {
                return true;
            }
            let charges = run
                .first
                .checked_add(run.count)
                .and_then(|end| ledger.rows.get(run.first..end));
            if charges.is_some_and(|rows| rows.iter().all(|c| c.new_row == usize::MAX)) {
                freed += run.bytes;
                return false;
            }
            run.first = charges
                .and_then(|rows| rows.first())
                .map_or(usize::MAX, |c| c.new_row);
            true
        });
        self.bytes -= freed;
        freed
    }
}
pub(super) struct OriginAppend {
    run: Option<OriginRun>,
    replacement: Option<Vec<OriginRun>>,
    pub(super) next_bytes: u64,
}
impl OriginAppend {
    pub(super) fn commit(mut self, ledger: &mut OriginLedger) {
        if let Some(mut replacement) = self.replacement {
            replacement.append(&mut ledger.runs);
            drop(std::mem::replace(&mut ledger.runs, replacement));
        }
        if let Some(run) = self.run.take() {
            ledger.runs.push(run);
        }
        ledger.bytes = self.next_bytes;
    }
}

pub(super) struct TempWriteStage {
    lease: Option<Arc<Reservation>>,
    evictor: Option<Arc<dyn Evictor + Send + Sync>>,
    used: u64,
    limit: u64,
    taken: Cell<u64>,
}
impl TempWriteStage {
    pub(super) fn new(tables: &TempTables, env: &crate::ExecEnv<'_>) -> Self {
        Self {
            lease: tables.lease.clone(),
            evictor: tables.evictor.clone(),
            used: tables.bytes,
            limit: env.mem_limit,
            taken: Cell::new(0),
        }
    }
    pub(super) fn grow(&self, bytes: u64) -> Result<(), ExecError> {
        let next = size_add(self.taken.get(), bytes)?;
        match (&self.lease, &self.evictor) {
            (Some(lease), Some(evictor)) => lease
                .try_grow_or_evict(bytes, evictor.as_ref())
                .map_err(ExecError::Memory)?,
            (Some(lease), None) => lease.try_grow(bytes).map_err(ExecError::Memory)?,
            (None, _) => {
                if size_add(self.used, next)? > self.limit {
                    return Err(super::limit());
                }
            }
        }
        self.taken.set(next);
        Ok(())
    }
    pub(super) fn release(&self, bytes: u64) {
        self.taken.set(self.taken.get() - bytes);
        self.refund(bytes);
    }
    pub(super) fn refund(&self, bytes: u64) {
        if let Some(lease) = &self.lease
            && bytes != 0
        {
            let _ = lease.shrink(bytes);
        }
    }
    pub(super) fn bytes(&self) -> u64 {
        self.taken.get()
    }
    pub(super) fn disarm(&self) {
        self.taken.set(0);
    }
}
impl Drop for TempWriteStage {
    fn drop(&mut self) {
        self.refund(self.taken.replace(0));
    }
}
