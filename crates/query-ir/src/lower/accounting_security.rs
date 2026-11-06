use super::layout::{R, Row, internal, p16};
use super::rel::Lower;
use super::{AccountingChartLayoutL, BalanceInputsL, PExpr, RelL};
use crate::ir::{RegisterVtS, SrcId};
use ibschema::storage::{FieldId, TableId};

impl Lower<'_> {
    pub(super) fn accounting_consumer(&self, v: &RegisterVtS, balance: bool) -> R<()> {
        if self.membership && self.b.security.accounting_sources.contains_key(&v.src) {
            return Err(crate::bind::security::accounting::refusal(balance));
        }
        Ok(())
    }

    pub(super) fn accounting_policy_fields(
        &mut self,
        v: &RegisterVtS,
        base: usize,
    ) -> R<Vec<FieldId>> {
        let Some(src) = self
            .b
            .security
            .accounting_sources
            .get(&v.src)
            .and_then(|s| s.register)
        else {
            return Ok(Vec::new());
        };
        let cols = self
            .scan_cols
            .get(src.0 as usize)
            .cloned()
            .unwrap_or_default();
        let Some(idx) = self.scan_idx.get_mut(src.0 as usize) else {
            return internal("accounting policy source");
        };
        for (i, field) in cols.iter().enumerate() {
            idx.insert(*field, p16((base + i) as i64)?);
        }
        self.set_level(src);
        Ok(cols.iter().map(|(_, f)| *f).collect())
    }

    pub(super) fn accounting_secure(
        &mut self,
        v: &RegisterVtS,
        rel: RelL,
        base: usize,
        signal: Option<u16>,
    ) -> R<Row> {
        let src = self
            .b
            .security
            .accounting_sources
            .get(&v.src)
            .and_then(|s| s.register);
        let width = base
            + usize::from(signal.is_some())
            + src
                .and_then(|s| self.scan_cols.get(s.0 as usize))
                .map_or(0, Vec::len);
        let row = Row {
            rel,
            signal,
            width,
            rep: src,
            keys: None,
        };
        match src {
            Some(src) => self.secure_row(row, src),
            None => Ok(row),
        }
    }

    fn physical_column(&mut self, src: SrcId, table: TableId, field: FieldId) -> R<u16> {
        let Some(cols) = self.scan_cols.get_mut(src.0 as usize) else {
            return internal("chart columns");
        };
        let pos = if let Some(i) = cols.iter().position(|x| *x == (table, field)) {
            i
        } else {
            let i = cols.len();
            cols.push((table, field));
            i
        };
        let pos = p16(pos as i64)?;
        self.scan_idx
            .get_mut(src.0 as usize)
            .ok_or_else(|| crate::ExecError::Fatal("chart index".into()))?
            .insert((table, field), pos);
        Ok(pos)
    }

    pub(super) fn accounting_chart(
        &mut self,
        v: &RegisterVtS,
        inputs: &BalanceInputsL,
    ) -> R<(Option<Box<RelL>>, Option<AccountingChartLayoutL>)> {
        let Some(src) = self
            .b
            .security
            .accounting_sources
            .get(&v.src)
            .and_then(|s| s.chart)
        else {
            return Ok((None, None));
        };
        let id = self.physical_column(src, inputs.chart, inputs.chart_id)?;
        let kind = self.physical_column(src, inputs.chart, inputs.chart_kind)?;
        let table = super::balance_chart(self.schema, v.table)?;
        let mut row = self.scan(src, table)?;
        let signal = row.signal.map(|_| 2);
        let mut exprs = vec![PExpr::Col(id), PExpr::Col(kind)];
        if row.signal.is_some() {
            exprs.push(row.signal_expr());
        }
        Ok((
            Some(Box::new(RelL::Project {
                input: row.take(),
                exprs,
            })),
            Some(AccountingChartLayoutL {
                id: 0,
                kind: 1,
                signal,
            }),
        ))
    }
}
