use ibschema::storage::{FieldId, TableId};
use query_syntax::Kw;
use query_syntax::ast::CmpOp;

use crate::ir::Expr;
use crate::schema::{QFieldId, QTableId};
use crate::value::QValue;
use bsl_prim::BslDate;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HierarchyMode {
    No,
    All,
    Only,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TotalsSpec {
    pub width: usize,
    pub points: Vec<PointSpec>,
    pub aggs: Vec<AggSpec>,
    pub items: Vec<(TotalsExpr, usize)>,
    pub order: Vec<OrderKey>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PointSpec {
    pub name: String,
    pub column: Option<usize>,
    pub hierarchy: HierarchyMode,
    pub periods: Option<PeriodsSpec>,
    pub hier: Vec<HierTableSpec>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OrderKey {
    pub source: KeySource,
    pub desc: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeySource {
    Row(usize),
    FirstRow { column: usize, point: usize },
    Point { column: usize, point: usize },
    Null,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HierTableSpec {
    pub number: u32,
    pub table: TableId,
    pub id: FieldId,
    pub parent: FieldId,
    pub folder: Option<FieldId>,
    pub keys: Vec<Option<(FieldId, bool)>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PeriodsSpec {
    pub unit: Kw,
    pub begin: Option<BslDate>,
    pub end: Option<BslDate>,
    pub week_start: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AggFunc {
    Sum,
    Avg,
    Count,
    Min,
    Max,
}

#[derive(Clone, Debug, PartialEq)]
pub enum AggArg {
    Star,
    Column(usize),
    Const(QValue),
}

#[derive(Clone, Debug, PartialEq)]
pub struct AggSpec {
    pub f: AggFunc,
    pub distinct: bool,
    pub arg: AggArg,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArithOp {
    Add,
    Sub,
    Mul,
    Div,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TotalsExpr {
    Agg(usize),
    Param(u16),
    Column(usize),
    Const(QValue),
    Neg(Box<TotalsExpr>),
    Bin(ArithOp, Box<TotalsExpr>, Box<TotalsExpr>),
    IsNull(Box<TotalsExpr>, Box<TotalsExpr>),
    Case {
        whens: Vec<(TotalsExpr, TotalsExpr)>,
        otherwise: Option<Box<TotalsExpr>>,
    },
    Cmp(CmpOp, Box<TotalsExpr>, Box<TotalsExpr>),
    Not(Box<TotalsExpr>),
    And(Vec<TotalsExpr>),
    Or(Vec<TotalsExpr>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct TotalsS {
    pub width: usize,
    pub points: Vec<PointS>,
    pub aggs: Vec<AggSpec>,
    pub items: Vec<(TotalsExpr, usize)>,
    pub order: Vec<OrderKey>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PointS {
    pub name: String,
    pub column: Option<usize>,
    pub hierarchy: HierarchyMode,
    pub periods: Option<PeriodsS>,
    pub hier: Vec<HierS>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HierS {
    pub table: QTableId,
    pub atom: crate::types::RefAtom,
    pub keys: Vec<Option<(QFieldId, bool)>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PeriodsS {
    pub unit: Kw,
    pub begin: Option<Expr>,
    pub end: Option<Expr>,
}
