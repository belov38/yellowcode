use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use query_ir::{
    ColumnInfo, ExecError, NestedL, NestedResult, QValue, RelL, StmtL, ValueRows,
    lower::NestedRowsL,
};

use super::Cx;
use crate::model::{HashModel, columns_model, row_cap_model, vec_block, vec_push_growth};
use crate::plan::{PlanOptions, build, plan_rel, plan_rel_driven};
use crate::source::Row;
use crate::temp::TempTables;

type Owner = (u32, [u8; 16]);

fn push_growth(len: usize, cap: usize, elem: usize, cx: &Cx<'_>) -> Result<u64, ExecError> {
    let (grow, release) = vec_push_growth(len, cap, elem);
    if grow > 0 {
        cx.mem.grow(grow)?;
    }
    Ok(release)
}

pub fn stitch<'a>(
    stmt: &StmtL,
    rows: &mut [Row],
    cx: &mut Cx<'a>,
    temps: Option<&TempTables>,
    opts: PlanOptions,
) -> Result<(), ExecError> {
    for (index, n) in stmt.nested.iter().enumerate() {
        stitch_one(stmt, n, rows, cx, temps, opts, index)?;
    }
    Ok(())
}

fn stitch_one<'a>(
    stmt: &StmtL,
    n: &NestedL,
    rows: &mut [Row],
    cx: &mut Cx<'a>,
    temps: Option<&TempTables>,
    opts: PlanOptions,
    observation_index: usize,
) -> Result<(), ExecError> {
    let col = usize::from(n.col);
    cx.mem.grow(columns_model(&n.columns) + 48)?;
    let empty = Arc::new(NestedResult {
        columns: n.columns.clone(),
        rows: Vec::new(),
    });
    let Some(second) = &n.rows else {
        set_all(rows, col, &empty);
        return Ok(());
    };
    let mut owners: Vec<Owner> = Vec::new();
    let mut seen: HashSet<Owner> = HashSet::new();
    let mut seen_model = HashModel::new(std::mem::size_of::<Owner>() as u64);
    let mut transient = 0u64;
    for row in rows.iter() {
        crate::probe::hit();
        if let Some(o) = owner_of(row, col)
            && !seen.contains(&o)
        {
            let table = seen_model.growth();
            cx.mem.grow(table)?;
            transient += table;
            seen.insert(o);
            let old = seen_model.inserted();
            cx.mem.shrink(old);
            transient = transient.saturating_sub(old);
            let old = push_growth(
                owners.len(),
                owners.capacity(),
                std::mem::size_of::<Owner>(),
                cx,
            )?;
            let (grew, _) = vec_push_growth(
                owners.len(),
                owners.capacity(),
                std::mem::size_of::<Owner>(),
            );
            transient += grew;
            owners.push(o);
            cx.mem.shrink(old);
            transient = transient.saturating_sub(old);
        }
    }
    drop(seen);
    if owners.is_empty() {
        cx.mem.shrink(transient);
        set_all(rows, col, &empty);
        return Ok(());
    }
    let by_owner = read_section(
        stmt,
        (second, observation_index),
        &owners,
        &n.columns,
        cx,
        temps,
        opts,
    );
    drop(owners);
    cx.mem.shrink(transient);
    let by_owner = by_owner?;
    for row in rows.iter_mut() {
        let value = match owner_of(row, col).and_then(|o| by_owner.get(&o)) {
            Some(r) => r.clone(),
            None => empty.clone(),
        };
        if let Some(slot) = row.get_mut(col) {
            *slot = QValue::Nested(value.into());
        }
    }
    Ok(())
}

fn owner_of(row: &[QValue], col: usize) -> Option<Owner> {
    match row.get(col) {
        Some(QValue::Ref { table, id }) => Some((*table, *id)),
        _ => None,
    }
}

fn set_all(rows: &mut [Row], col: usize, v: &Arc<NestedResult>) {
    for row in rows.iter_mut() {
        if let Some(slot) = row.get_mut(col) {
            *slot = QValue::Nested(v.clone().into());
        }
    }
}

fn read_section<'a>(
    stmt: &StmtL,
    second: (&NestedRowsL, usize),
    owners: &[Owner],
    columns: &[ColumnInfo],
    cx: &mut Cx<'a>,
    temps: Option<&TempTables>,
    opts: PlanOptions,
) -> Result<HashMap<Owner, Arc<NestedResult>>, ExecError> {
    let (second, observation_index) = second;
    let seeds = vec_block(owners.len(), std::mem::size_of::<Row>())
        + owners.len() as u64 * vec_block(1, crate::model::VALUE_SIZE)
        + 64;
    let _seeds = cx.mem.hold_owned(seeds)?;
    let mut transient = 0;
    let real = Arc::new(ValueRows {
        width: 1,
        rows: owners
            .iter()
            .map(|(table, id)| {
                vec![QValue::Ref {
                    table: *table,
                    id: *id,
                }]
            })
            .collect(),
    });
    let rel_bytes = rel_model(&second.rel)?;
    let _logical_copy = cx.mem.hold_owned(rel_bytes)?;
    if let Some(payloads) = &cx.input_payloads {
        payloads.borrow_mut().relation(&second.rel, &cx.mem)?;
    } else if second.requires_calculation_admission
        || crate::accounting_inputs::protected(&second.rel)?
    {
        return Err(super::fatal("unprepared accounting input payloads"));
    }
    let mut rel = second.rel.clone();
    let _plan_copy = crate::accounting_inputs::admit_plan_for_calculation(
        &rel,
        &cx.mem,
        second.requires_calculation_admission,
    )?;
    if !replace_values(&mut rel, &second.owners, &real) {
        return Err(super::fatal(
            "the owners of a nested table are not in its statement",
        ));
    }
    let mut plan = if drive_by_owners(&second.rel, &second.owners, owners.len(), cx)? {
        plan_rel_driven(&rel, stmt.max_depth, cx.src, temps, opts)?
    } else {
        plan_rel(&rel, stmt.max_depth, cx.src, temps, opts)?
    };
    if let Some(obs) = &cx.observation {
        obs.publish_nested(observation_index, &mut plan);
    }
    let mut op = build(&plan);
    let mut groups: HashMap<Owner, Vec<(QValue, Row)>> = HashMap::new();
    let mut groups_model = HashModel::new(
        (std::mem::size_of::<Owner>() + std::mem::size_of::<Vec<(QValue, Row)>>()) as u64,
    );
    let entry = std::mem::size_of::<(QValue, Row)>();
    while let Some(mut row) = op.next(cx)? {
        crate::probe::hit();
        let Some(owner) = owner_of(&row, 0) else {
            continue;
        };
        cx.mem.grow(row_cap_model(&row))?;
        let tail = row.split_off(2);
        let number = row.pop().unwrap_or(QValue::Null);
        match groups.get_mut(&owner) {
            Some(list) => {
                let old = push_growth(list.len(), list.capacity(), entry, cx)?;
                list.push((number, tail));
                cx.mem.shrink(old);
            }
            None => {
                let (list, _) = vec_push_growth(0, 0, entry);
                let table = groups_model.growth();
                cx.mem.grow(list + table)?;
                transient += table;
                groups.insert(owner, vec![(number, tail)]);
                let old = groups_model.inserted();
                cx.mem.shrink(old);
                transient = transient.saturating_sub(old);
            }
        }
    }
    op.close(cx);
    drop(op);
    drop(plan);
    let out_table = crate::model::hash_table_model(
        crate::model::hash_buckets_for(groups.len()),
        (std::mem::size_of::<Owner>() + std::mem::size_of::<Arc<NestedResult>>()) as u64,
    );
    cx.mem.grow(out_table)?;
    transient += out_table;
    let mut out = HashMap::with_capacity(groups.len());
    for (owner, rs) in groups {
        let keyed_entry = std::mem::size_of::<(crate::key::Key, (QValue, Row))>();
        let scratch = vec_block(rs.len(), keyed_entry);
        cx.mem.grow_scratch(scratch)?;
        let mut keyed = Vec::with_capacity(rs.len());
        for e in rs {
            keyed.push((super::join::key_charged(&e.0, cx)?, e));
        }
        keyed.sort_by(|a, b| a.0.cmp(&b.0));
        let rs: Vec<(QValue, Row)> = keyed.into_iter().map(|(_, e)| e).collect();
        cx.mem.release_scratch();
        let rows_vec = vec_block(rs.len(), std::mem::size_of::<Row>());
        cx.mem.grow(columns_model(columns) + 48 + rows_vec)?;
        let list = vec_block(rs.capacity(), entry);
        let nested = Arc::new(NestedResult {
            columns: columns.to_vec(),
            rows: rs.into_iter().map(|(_, r)| r).collect(),
        });
        cx.mem.shrink(list);
        out.insert(owner, nested);
    }
    cx.mem.shrink(transient.saturating_sub(out_table));
    Ok(out)
}

fn rel_model(r: &RelL) -> Result<u64, ExecError> {
    crate::accounting_inputs::structural_model(r)
}

pub const OWNER_SEEK_ROWS: u64 = 32;
pub const MAX_DRIVING_OWNERS: usize = 4096;

fn section_table(r: &RelL, placeholder: &Arc<ValueRows>) -> Option<ibschema::storage::TableId> {
    match r {
        RelL::CheckAccess { input, .. }
        | RelL::Project { input, .. }
        | RelL::Filter { input, .. } => section_table(input, placeholder),
        RelL::Join { l, r, .. } => match (&**l, &**r) {
            (RelL::Scan { table, .. }, RelL::Values { rows }) if Arc::ptr_eq(rows, placeholder) => {
                Some(*table)
            }
            _ => section_table(l, placeholder).or_else(|| section_table(r, placeholder)),
        },
        _ => None,
    }
}

fn drive_by_owners(
    rel: &RelL,
    placeholder: &Arc<ValueRows>,
    owners: usize,
    cx: &Cx<'_>,
) -> Result<bool, ExecError> {
    if owners == 0 || owners > MAX_DRIVING_OWNERS {
        return Ok(false);
    }
    let Some(table) = section_table(rel, placeholder) else {
        return Ok(false);
    };
    let estimate = cx.src.row_count_estimate(table);
    let prefix = cx
        .src
        .map()
        .try_table(table)
        .and_then(|t| t.key_index.as_ref())
        .map_or(0, |k| k.prefix);
    let areas = if prefix == 0 {
        1
    } else {
        match super::access::data_areas(cx, table, ibschema::storage::IndexId::Id, prefix, false)? {
            Some(areas) => areas.len().max(1),
            None => return Ok(false),
        }
    };
    Ok((owners as u64)
        .saturating_mul(areas as u64)
        .saturating_mul(OWNER_SEEK_ROWS)
        < estimate)
}

fn replace_values(r: &mut RelL, placeholder: &Arc<ValueRows>, real: &Arc<ValueRows>) -> bool {
    match r {
        RelL::Values { rows } if Arc::ptr_eq(rows, placeholder) => {
            *rows = real.clone();
            true
        }
        RelL::CheckAccess { input, .. }
        | RelL::Project { input, .. }
        | RelL::Filter { input, .. } => replace_values(input, placeholder, real),
        RelL::Join { l, r, .. } => {
            replace_values(l, placeholder, real) || replace_values(r, placeholder, real)
        }
        _ => false,
    }
}
