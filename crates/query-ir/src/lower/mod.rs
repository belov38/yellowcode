mod dec;
mod deref;
mod expr;
mod layout;
mod print;
mod rel;
mod totals;

pub use dec::{
    ArithDec, Dec, MAX_DIGITS, MIN_DIV_SCALE, MIN_MUL_SCALE, Opd, agg_dec, dec_add, dec_div,
    dec_mul, dec_mul_floor, dec_of, dec_of_lit, dec_op, dec_op_k, mul_floor, opd_of,
};
pub use print::print;

use crate::bind::{BindCtx, Bound};
use crate::exec_error::ExecError;
use crate::schema::{FieldRole, QTableId, StdRole, TableKind, VtKind};
use crate::types::{NumQ, QType, RefAtom};
use crate::value::QValue;
use crate::{ColumnInfo, ValueSet};
use ibschema::storage::{FieldId, TableId};
use mdschema::MdKind;
use query_syntax::ast::{BinOp, CmpOp, JoinKind};
use query_syntax::{Func, INLINE_DEPTH, Kw, with_deep_stack};
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq)]
pub struct StmtL {
    pub root: RelL,
    pub auto_number: Option<u16>,
    pub columns: Vec<ColumnInfo>,
    pub touched: Vec<TableId>,
    pub nested: Vec<NestedL>,
    pub present: Vec<u16>,
    pub present_targets: Vec<PresentTarget>,
    pub totals: Option<crate::totals::TotalsSpec>,
    pub week_start: Option<u32>,
    pub max_depth: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PresentTarget {
    pub number: u32,
    pub table: Option<QTableId>,
    pub storage: TableId,
    pub route_point: bool,
    pub fields: Vec<(StdRole, FieldId)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct NestedL {
    pub col: u16,
    pub columns: Vec<ColumnInfo>,
    pub rows: Option<NestedRowsL>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct NestedRowsL {
    pub rel: RelL,
    pub owners: Arc<crate::bind::ValueRows>,
}

impl Drop for StmtL {
    fn drop(&mut self) {
        if self.max_depth > INLINE_DEPTH {
            let root = std::mem::replace(&mut self.root, RelL::Empty);
            with_deep_stack(self.max_depth, move || drop(root));
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum RelL {
    Empty,
    Scan {
        table: TableId,
        fields: Vec<FieldId>,
    },
    TempScan {
        name: String,
        width: u16,
    },
    Values {
        rows: Arc<crate::bind::ValueRows>,
    },
    Join {
        kind: JoinKind,
        l: Box<RelL>,
        r: Box<RelL>,
        on: PExpr,
        eq: Vec<(PExpr, PExpr)>,
        by_id: bool,
    },
    Filter {
        input: Box<RelL>,
        pred: PExpr,
    },
    Project {
        input: Box<RelL>,
        exprs: Vec<PExpr>,
    },
    Aggregate {
        input: Box<RelL>,
        keys: Vec<PExpr>,
        sets: Option<Vec<Vec<u16>>>,
        aggs: Vec<AggL>,
    },
    Distinct(Box<RelL>),
    Sort {
        input: Box<RelL>,
        keys: Vec<(PExpr, bool)>,
    },
    Top {
        input: Box<RelL>,
        n: u64,
    },
    Union {
        parts: Vec<RelL>,
        all: Vec<bool>,
    },
    SemiJoin {
        input: Box<RelL>,
        l: Vec<PExpr>,
        q: Box<RelL>,
        not: bool,
        correlated: bool,
    },
    HierarchyClosure {
        tables: Vec<HierTable>,
        seeds: HierSeeds,
    },
    HierarchyOrder {
        input: Box<RelL>,
        id: u16,
        parent: u16,
        keys: Vec<(PExpr, bool)>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct HierTable {
    pub number: u32,
    pub table: TableId,
    pub id: FieldId,
    pub parent: FieldId,
}

#[derive(Clone, Debug, PartialEq)]
pub enum HierSeeds {
    Values(Vec<PExpr>),
    Rel(Box<RelL>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct AggL {
    pub f: Func,
    pub distinct: bool,
    pub arg: Option<PExpr>,
    pub ty: QType,
    pub arg_dec: Dec,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PExpr {
    Col(u16),
    Outer {
        depth: u16,
        col: u16,
    },
    Lit(QValue),
    Set(Arc<ValueSet>),
    Neg(Box<PExpr>),
    Not(Box<PExpr>),
    Arith {
        first: Box<PExpr>,
        first_dec: Dec,
        steps: Vec<(BinOp, PExpr, QType)>,
        decs: Vec<ArithDec>,
    },
    And(Vec<PExpr>),
    Or(Vec<PExpr>),
    Compare(CmpOp, Box<PExpr>, Box<PExpr>),
    InList {
        l: Vec<PExpr>,
        not: bool,
        rhs: Vec<PExpr>,
    },
    InQuery {
        l: Vec<PExpr>,
        not: bool,
        q: Arc<RelL>,
        correlated: bool,
    },
    SectionAgg {
        owner: Box<PExpr>,
        rel: Arc<RelL>,
        empty: QValue,
    },
    Between {
        l: Box<PExpr>,
        not: bool,
        lo: Box<PExpr>,
        hi: Box<PExpr>,
    },
    IsNull {
        l: Box<PExpr>,
        not: bool,
    },
    Refs {
        l: Box<PExpr>,
        number: u32,
    },
    Like {
        l: Box<PExpr>,
        not: bool,
        pat: Box<PExpr>,
        esc: Option<char>,
    },
    Case {
        operand: Option<Box<PExpr>>,
        whens: Vec<(PExpr, PExpr)>,
        else_: Option<Box<PExpr>>,
    },
    Cast {
        x: Box<PExpr>,
        to: QType,
        num: Option<NumQ>,
        ref_tables: Vec<u32>,
        load: bool,
    },
    Func {
        f: Func,
        args: Vec<PExpr>,
        ty: QType,
        arg_dec: Dec,
    },
    TypeOf {
        x: Box<PExpr>,
        refs: Vec<(u32, RefAtom)>,
    },
    StoredSize {
        x: Box<PExpr>,
        kind: SizeKind,
    },
    Unit(Kw),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SizeKind {
    Fixed(u32),
    VarString,
    Composite,
}

pub(crate) fn lower(b: &Bound<'_>, ctx: &BindCtx<'_>) -> Result<StmtL, ExecError> {
    let (schema, storage, map) = (ctx.schema, ctx.storage, ctx.map);
    let mut names = std::collections::HashMap::new();
    loop {
        let (out, wanted) = with_deep_stack(b.stmt.max_depth, || {
            let mut l = rel::Lower::new(schema, storage, map, b, names.clone());
            let root = l.run().and_then(|root| Ok((root, l.run_nested()?)));
            let wanted = std::mem::take(&mut l.predef_wanted);
            let max_depth = b
                .stmt
                .max_depth
                .saturating_add(l.added_depth())
                .saturating_add(4);
            let out = root.map(|(root, nested)| StmtL {
                root,
                nested,
                auto_number: b.stmt.auto_number,
                columns: b.stmt.columns.clone(),
                touched: b.tables.clone(),
                present: Vec::new(),
                present_targets: Vec::new(),
                totals: None,
                week_start: None,
                max_depth,
            });
            (out, wanted)
        });
        if wanted.is_empty() {
            let mut l = out?;
            if !b.stmt.present.is_empty() {
                l.present = b.stmt.present.clone();
                l.present_targets = present_targets(schema, storage, map);
            }
            if crate::ir::uses_week(b.stmt) {
                l.week_start = Some(ctx.host.week_start().map_err(ExecError::Host)?);
            }
            if let Some(t) = &b.stmt.totals {
                l.totals = Some(totals::lower_totals(
                    t,
                    b,
                    schema,
                    storage,
                    map,
                    l.week_start,
                )?);
            }
            return Ok(l);
        }
        for q in wanted {
            let n = ctx.host.predefined_names(q).map_err(ExecError::Host)?;
            names.insert(q, n);
        }
    }
}

pub(crate) fn present_targets(
    schema: &crate::QuerySchema,
    storage: &crate::StorageBinding,
    map: &ibschema::storage::StorageMap,
) -> Vec<PresentTarget> {
    let mut out = Vec::new();
    for (number, atom) in storage.ref_atoms() {
        let info = schema.ref_info(atom);
        let roles: &[StdRole] = match info.kind {
            MdKind::Catalog
            | MdKind::ChartOfCharacteristicTypes
            | MdKind::ChartOfAccounts
            | MdKind::ChartOfCalculationTypes
            | MdKind::ExchangePlan => &[StdRole::Description, StdRole::Code],
            MdKind::Document | MdKind::BusinessProcess | MdKind::Task => {
                &[StdRole::Number, StdRole::Date]
            }
            MdKind::Enum => &[StdRole::Order],
            _ => &[],
        };
        let Some(storage_table) = storage.table(info.table) else {
            continue;
        };
        let fields = roles
            .iter()
            .filter_map(|role| {
                let t = schema.table(info.table);
                let i = t
                    .fields
                    .iter()
                    .position(|f| f.role == FieldRole::Std(*role))?;
                let f =
                    storage.field(info.table, crate::schema::QFieldId(u16::try_from(i).ok()?))?;
                Some((*role, f))
            })
            .collect();
        out.push(PresentTarget {
            number,
            table: Some(info.table),
            storage: storage_table,
            route_point: false,
            fields,
        });
    }
    for (i, t) in schema.tables().iter().enumerate() {
        if t.kind != TableKind::Virtual(VtKind::BusinessProcessPoints) {
            continue;
        }
        let Some(storage_table) = u32::try_from(i)
            .ok()
            .and_then(|i| storage.table(QTableId(i)))
        else {
            continue;
        };
        if let Some(number) = map.ref_number(storage_table) {
            out.push(PresentTarget {
                number,
                table: None,
                storage: storage_table,
                route_point: true,
                fields: Vec::new(),
            });
        }
    }
    out
}
