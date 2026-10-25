use ibschema::names::StdField;
use ibschema::storage::{FieldId, IndexId};
use query_ir::{ExecError, PExpr, RelL};
use query_syntax::ast::JoinKind;

use super::access::unique_id_key;
use super::{Force, Lookup, Plan, Planner, cols_end, width, wrap};

type Eq = [(PExpr, PExpr)];

impl Planner<'_> {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn join(
        &self,
        kind: JoinKind,
        l: &RelL,
        r: &RelL,
        on: &PExpr,
        eq: &Eq,
        by_id: bool,
        rep: bool,
        pending: Vec<PExpr>,
    ) -> Result<Plan, ExecError> {
        let lw = width(l);
        let rw = width(r);
        let (left, rest): (Vec<PExpr>, Vec<PExpr>) =
            if matches!(kind, JoinKind::Inner | JoinKind::Left) {
                pending.into_iter().partition(|e| cols_end(e) <= lw)
            } else {
                (Vec::new(), pending)
            };
        if self.drive_values
            && self.use_index()
            && kind == JoinKind::Inner
            && let Some(p) = self.values_join(l, r, on, eq)
        {
            let mut pending = left;
            pending.extend(rest);
            return Ok(wrap(p, pending));
        }
        let lp = self.rel(l, rep, left)?;
        let plan = match self.index_join(kind, &lp, r, on, eq, by_id) {
            Some(p) => p,
            None => {
                let rp = self.rel(r, rep, Vec::new())?;
                let hashed = !eq.is_empty() && !matches!(self.opts.force, Some(Force::NestedLoop));
                if hashed {
                    Plan::HashJoin {
                        kind,
                        l: Box::new(lp),
                        r: Box::new(rp),
                        lk: eq.iter().map(|(a, _)| a.clone()).collect(),
                        rk: eq.iter().map(|(_, b)| b.clone()).collect(),
                        on: on.clone(),
                        lw,
                        rw,
                    }
                } else {
                    Plan::NestedLoopJoin {
                        kind,
                        l: Box::new(lp),
                        r: Box::new(rp),
                        on: on.clone(),
                        lw,
                        rw,
                    }
                }
            }
        };
        Ok(wrap(plan, rest))
    }

    fn values_join(&self, l: &RelL, r: &RelL, on: &PExpr, eq: &Eq) -> Option<Plan> {
        let (RelL::Scan { table, fields }, RelL::Values { .. }) = (l, r) else {
            return None;
        };
        if eq.is_empty() {
            return None;
        }
        let info = self.map.try_table(*table)?;
        let left_field = |e: &PExpr| match e {
            PExpr::Col(c) => fields.get(usize::from(*c)).copied(),
            _ => None,
        };
        let mut candidates: Vec<(IndexId, &ibschema::storage::IndexInfo)> = Vec::new();
        for (k, ix) in info.indexes.iter().enumerate() {
            candidates.push((IndexId::Named(u16::try_from(k).ok()?), ix));
        }
        if let Some(key) = &info.key_index {
            candidates.push((IndexId::Id, key));
        }
        type Best<'i> = (
            usize,
            bool,
            IndexId,
            &'i ibschema::storage::IndexInfo,
            usize,
            Vec<PExpr>,
        );
        let mut best: Option<Best<'_>> = None;
        for (index, ix) in candidates {
            let pair = |f: FieldId| eq.iter().find(|(a, _)| left_field(a) == Some(f));
            let given = ix
                .fields
                .get(..ix.prefix)
                .unwrap_or(&[])
                .iter()
                .all(|f| pair(*f).is_some());
            let skip = if given { 0 } else { ix.prefix };
            let mut keys = Vec::new();
            for f in ix.fields.iter().skip(skip) {
                let Some((_, b)) = pair(*f) else {
                    break;
                };
                keys.push(b.clone());
            }
            let plain = skip == 0;
            if !keys.is_empty()
                && best
                    .as_ref()
                    .is_none_or(|b| (keys.len(), plain) > (b.0, b.1))
            {
                best = Some((keys.len(), plain, index, ix, skip, keys));
            }
        }
        let (_, _, index, ix, skip, keys) = best?;
        Some(Plan::ValuesIndexJoin {
            values: Box::new(self.rel(r, false, Vec::new()).ok()?),
            table: *table,
            name: self.table_name(*table),
            index,
            index_name: ix.name.clone(),
            keys,
            skip,
            fields: fields.clone(),
            on: on.clone(),
            vw: width(r),
        })
    }

    fn index_join(
        &self,
        kind: JoinKind,
        lp: &Plan,
        r: &RelL,
        on: &PExpr,
        eq: &Eq,
        by_id: bool,
    ) -> Option<Plan> {
        if !self.use_index() || !matches!(kind, JoinKind::Inner | JoinKind::Left) || eq.is_empty() {
            return None;
        }
        if let RelL::TempScan { name, width } = r {
            return self.temp_index_join(kind, lp, name, *width, on, eq);
        }
        let RelL::Scan { table, fields } = r else {
            return None;
        };
        let info = self.map.try_table(*table)?;
        let name = self.table_name(*table);
        let make = |lookup: Lookup| Plan::IndexJoin {
            kind,
            l: Box::new(lp.clone()),
            table: *table,
            name: name.clone(),
            lookup,
            fields: fields.clone(),
            on: on.clone(),
        };
        let right_field = |e: &PExpr| -> Option<FieldId> {
            match e {
                PExpr::Col(c) => fields.get(usize::from(*c)).copied(),
                _ => None,
            }
        };
        let id = info.field_by_std(StdField::Id);
        let by_id_pair = eq
            .iter()
            .find(|(_, b)| id.is_some() && right_field(b) == id);
        let area_given = info.key_index.as_ref().is_some_and(|k| {
            k.prefix > 0
                && k.fields
                    .get(..k.prefix)
                    .unwrap_or(&[])
                    .iter()
                    .all(|f| eq.iter().any(|(_, b)| right_field(b) == Some(*f)))
        });
        if let Some((a, _)) = by_id_pair
            && ((by_id && !area_given) || unique_id_key(info).is_some_and(|k| k.prefix == 0))
        {
            return Some(make(Lookup::Id(a.clone())));
        }
        let mut candidates: Vec<(IndexId, &ibschema::storage::IndexInfo)> = Vec::new();
        for (k, ix) in info.indexes.iter().enumerate() {
            candidates.push((IndexId::Named(u16::try_from(k).ok()?), ix));
        }
        if let Some(key) = info.key_index.as_ref().filter(|k| k.prefix > 0) {
            candidates.push((IndexId::Id, key));
        }
        type Best<'i> = (
            usize,
            bool,
            IndexId,
            &'i ibschema::storage::IndexInfo,
            usize,
            Vec<PExpr>,
        );
        let mut best: Option<Best<'_>> = None;
        for (index, ix) in candidates {
            let pair = |f: FieldId| eq.iter().find(|(_, b)| right_field(b) == Some(f));
            let given = ix
                .fields
                .get(..ix.prefix)
                .unwrap_or(&[])
                .iter()
                .all(|f| pair(*f).is_some());
            let skip = if given { 0 } else { ix.prefix };
            let mut keys = Vec::new();
            for f in ix.fields.iter().skip(skip) {
                let Some((a, _)) = pair(*f) else {
                    break;
                };
                keys.push(a.clone());
            }
            let covered = keys.len();
            let plain = skip == 0;
            if covered > 0 && best.as_ref().is_none_or(|b| (covered, plain) > (b.0, b.1)) {
                best = Some((covered, plain, index, ix, skip, keys));
            }
        }
        let (_, _, index, ix, skip, keys) = best?;
        Some(make(Lookup::Prefix {
            index,
            index_name: ix.name.clone(),
            keys,
            skip,
        }))
    }
}
