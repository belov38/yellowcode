use super::layout::{R, Row, internal, p16};
use super::rel::Lower;
use super::{PExpr, RelL};
use crate::ir::RegisterVtS;
use ibschema::storage::{FieldId, TableId};

impl Lower<'_> {
    pub(super) fn calculation_consumer(&self, v: &RegisterVtS) -> R<()> {
        if self.membership
            && let Some(sources) = self.b.security.calculation_sources.get(&v.src)
        {
            let code = if sources.main.is_some() {
                crate::bind::security::calculation::code(v.kind)
                    .ok_or_else(|| crate::ExecError::Fatal("calculation source kind".into()))?
            } else {
                "Q6c-ScheduleDataScheduleRls"
            };
            return Err(crate::bind::security::calculation::refusal(code));
        }
        Ok(())
    }

    pub(super) fn calculation_scan(
        &mut self,
        v: &RegisterVtS,
        graph: bool,
        table: TableId,
        fields: &mut Vec<FieldId>,
    ) -> R<Row> {
        let src = self
            .b
            .security
            .calculation_sources
            .get(&v.src)
            .and_then(|s| if graph { s.graph } else { s.main });
        if let Some(src) = src {
            for (physical, field) in self.scan_cols.get(src.0 as usize).into_iter().flatten() {
                if *physical != table {
                    return internal("calculation policy field outside physical input");
                }
                if !fields.contains(field) {
                    fields.push(*field);
                }
            }
            let Some(idx) = self.scan_idx.get_mut(src.0 as usize) else {
                return internal("calculation policy index");
            };
            idx.clear();
            for (i, field) in fields.iter().enumerate() {
                idx.insert((table, *field), p16(i as i64)?);
            }
            self.set_level(src);
        }
        let mut row = Row {
            rel: RelL::Scan {
                table,
                fields: fields.clone(),
            },
            signal: None,
            width: fields.len(),
            rep: src,
            keys: None,
        };
        if let Some(src) = src {
            self.requires_calculation_admission = true;
            row = self.secure_row(row, src)?;
            let mut exprs = (0..fields.len())
                .map(|i| p16(i as i64).map(PExpr::Col))
                .collect::<R<Vec<_>>>()?;
            let signal = row.signal.map(|_| p16(fields.len() as i64)).transpose()?;
            if row.signal.is_some() {
                exprs.push(row.signal_expr());
            }
            row.rel = RelL::Project {
                input: row.take(),
                exprs,
            };
            row.width = fields.len() + usize::from(signal.is_some());
            row.signal = signal;
        }
        Ok(row)
    }
}
