use crate::QShared;
use crate::schema::{Periodicity, QFieldId, QTableId, ValuePath};
use crate::temp::TempSchema;
use crate::types::{NumQ, QType};
use crate::value::QValue;
use query_syntax::ast::{BinOp, CmpOp, JoinKind, OrderDir};
use query_syntax::{Func, INLINE_DEPTH, Kw, Span, with_deep_stack};

#[derive(Clone, Debug, PartialEq)]
pub struct Stmt {
    pub static_analysis: bool,
    pub kind: StmtKind,
    pub max_depth: u32,
}

impl Drop for Stmt {
    fn drop(&mut self) {
        if self.max_depth > INLINE_DEPTH {
            let kind = std::mem::replace(&mut self.kind, StmtKind::Drop(String::new()));
            with_deep_stack(self.max_depth, move || drop(kind));
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum StmtKind {
    Query(Query),
    Drop(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Query {
    pub parts: Vec<Select>,
    pub unions: Vec<bool>,
    pub order: Vec<OrderKey>,
    pub autoorder: bool,
    pub totals: Option<Totals>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct OrderKey {
    pub expr: TExpr,
    pub col: Option<usize>,
    pub dir: OrderDir,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Totals {
    pub items: Vec<(TExpr, usize)>,
    pub hits: Vec<(usize, Span)>,
    pub points: Vec<ControlPoint>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ControlPoint {
    Overall,
    Field(Box<TotalsField>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct TotalsField {
    pub expr: TExpr,
    pub col: Option<usize>,
    pub hierarchy: TotalsHierarchy,
    pub periods: Option<TotalsPeriods>,
    pub alias: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TotalsHierarchy {
    No,
    All,
    Only,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TotalsPeriods {
    pub unit: Kw,
    pub begin: Option<TExpr>,
    pub end: Option<TExpr>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Select {
    pub allowed: bool,
    pub distinct: bool,
    pub top: Option<u64>,
    pub items: Vec<Item>,
    pub sources: Vec<Source>,
    pub where_: Option<TExpr>,
    pub group: Option<Vec<Vec<TExpr>>>,
    pub having: Option<TExpr>,
    pub for_update: Option<Vec<QTableId>>,
    pub index_by: Vec<Vec<usize>>,
    pub index_unique: Vec<bool>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub star: bool,
    pub expr: TExpr,
    pub name: String,
    pub nested: Option<Vec<Item>>,
    pub unknown: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Source {
    pub all_fields: bool,
    pub alias: String,
    pub from: From,
    pub joins: Vec<Join>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum From {
    Table {
        id: QTableId,
        args: Vec<Option<TExpr>>,
        periodicity: Periodicity,
        complement: Option<crate::schema::Complement>,
        span: Option<query_syntax::Span>,
        breakdown: Vec<QFieldId>,
    },
    Temp(QShared<TempSchema>),
    Param {
        name: String,
        schema: QShared<TempSchema>,
    },
    Subquery(Box<Query>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Join {
    pub kind: JoinKind,
    pub right: Source,
    pub on: TExpr,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TExpr {
    pub kind: TKind,
    pub ty: QType,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ArithStep {
    pub op: BinOp,
    pub op_span: Span,
    pub rhs: TExpr,
    pub ty: QType,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Col {
    Field(QFieldId),
    Column(u16),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Deref {
    pub name: String,
    pub targets: Vec<(QTableId, QFieldId)>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TKind {
    Const(QValue),
    Value(ValuePath),
    Param(String),
    Field {
        level: u16,
        source: u16,
        col: Col,
        derefs: Vec<Deref>,
    },
    Neg(Box<TExpr>),
    Not(Box<TExpr>),
    Arith {
        first: Box<TExpr>,
        steps: Vec<ArithStep>,
    },
    And(Vec<TExpr>),
    Or(Vec<TExpr>),
    Compare(CmpOp, Box<TExpr>, Box<TExpr>),
    InList {
        l: Vec<TExpr>,
        not: bool,
        hierarchy: bool,
        rhs: Vec<TExpr>,
    },
    InQuery {
        l: Vec<TExpr>,
        not: bool,
        hierarchy: bool,
        q: Box<Query>,
    },
    Between {
        l: Box<TExpr>,
        not: bool,
        lo: Box<TExpr>,
        hi: Box<TExpr>,
    },
    IsNull {
        l: Box<TExpr>,
        not: bool,
    },
    Refs {
        l: Box<TExpr>,
        table: QTableId,
    },
    Like {
        l: Box<TExpr>,
        not: bool,
        pattern: Box<TExpr>,
        escape: Option<char>,
    },
    Case {
        operand: Option<Box<TExpr>>,
        whens: Vec<(TExpr, TExpr)>,
        else_: Option<Box<TExpr>>,
    },
    Cast {
        e: Box<TExpr>,
        to: QType,
        derefs: Vec<Deref>,
        num: Option<NumQ>,
    },
    Func {
        f: Func,
        args: Vec<TExpr>,
    },
    Unit(Kw),
    Agg {
        f: Func,
        distinct: bool,
        arg: Option<Box<TExpr>>,
        nested: bool,
    },
}
