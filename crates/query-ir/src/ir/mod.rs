mod from_tir;
pub(crate) mod print;
pub mod stats;

pub use from_tir::build_stmt;
pub(crate) use from_tir::visit_expr as visit_expr_in_place;
pub use print::print;

use crate::schema::{QFieldId, QTableId, ValuePath};
use crate::tir;
use crate::types::{NumQ, QType};
use crate::value::QValue;
use crate::{ColumnInfo, CompileError};
use query_syntax::ast::{BinOp, CmpOp, JoinKind};
use query_syntax::{Func, INLINE_DEPTH, Kw, with_deep_stack};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SrcId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ColSel {
    Field(QFieldId),
    Column(u16),
}

#[derive(Clone, Debug, PartialEq)]
pub struct StmtS {
    pub root: Rel,
    pub columns: Vec<ColumnInfo>,
    pub sources: Vec<SourceS>,
    pub params: Vec<String>,
    pub values: Vec<ValuePath>,
    pub tables: Vec<QTableId>,
    pub allowed: bool,
    pub auto_number: Option<u16>,
    pub nested: Vec<NestedS>,
    pub present: Vec<u16>,
    pub totals: Option<crate::totals::TotalsS>,
    pub max_depth: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct NestedS {
    pub col: u16,
    pub rows: Option<NestedRows>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct NestedRows {
    pub scan: Rel,
    pub owner: Expr,
    pub number: Expr,
    pub exprs: Vec<Expr>,
}

impl Drop for StmtS {
    fn drop(&mut self) {
        if self.max_depth > INLINE_DEPTH {
            let root = std::mem::replace(&mut self.root, Rel::Empty);
            with_deep_stack(self.max_depth, move || drop(root));
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SourceS {
    pub id: SrcId,
    pub kind: SrcKind,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SrcKind {
    Table(QTableId),
    Subquery {
        columns: Vec<QType>,
    },
    Temp {
        columns: Vec<QType>,
        stored: Vec<Option<NumQ>>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Rel {
    Empty,
    Scan {
        src: SrcId,
        table: QTableId,
    },
    Subquery {
        src: SrcId,
        input: Box<Rel>,
    },
    TempScan {
        src: SrcId,
        name: String,
    },
    ParamScan {
        src: SrcId,
        param: u16,
    },
    Join {
        kind: JoinKind,
        l: Box<Rel>,
        r: Box<Rel>,
        on: Expr,
    },
    Filter {
        input: Box<Rel>,
        pred: Expr,
    },
    Aggregate {
        input: Box<Rel>,
        keys: Vec<Expr>,
        sets: Option<Vec<Vec<u16>>>,
        aggs: Vec<AggCall>,
    },
    Project {
        input: Box<Rel>,
        exprs: Vec<Expr>,
    },
    Distinct(Box<Rel>),
    Sort {
        input: Box<Rel>,
        keys: Vec<SortKey>,
    },
    Top {
        input: Box<Rel>,
        n: u64,
    },
    Hierarchy {
        input: Box<Rel>,
        id: u16,
        parent: u16,
        keys: Vec<SortKey>,
    },
    Union {
        parts: Vec<Rel>,
        all: Vec<bool>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Expr {
    pub kind: ExprKind,
    pub ty: QType,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ExprKind {
    Col {
        src: SrcId,
        col: ColSel,
    },
    Deref {
        base: Box<Expr>,
        steps: Vec<tir::Deref>,
    },
    Lit(QValue),
    Param(u16),
    Value(u16),
    KeyRef(u16),
    AggRef(u16),
    OuterKey {
        src: SrcId,
        key: u16,
        orig: Box<Expr>,
    },
    Out(u16),
    Neg(Box<Expr>),
    Not(Box<Expr>),
    Arith {
        first: Box<Expr>,
        steps: Vec<(BinOp, Expr, QType)>,
    },
    And(Vec<Expr>),
    Or(Vec<Expr>),
    Compare(CmpOp, Box<Expr>, Box<Expr>),
    InList {
        l: Vec<Expr>,
        not: bool,
        hierarchy: bool,
        rhs: Vec<Expr>,
    },
    InQuery {
        l: Vec<Expr>,
        not: bool,
        hierarchy: bool,
        q: Box<Rel>,
    },
    SectionAgg {
        f: query_syntax::Func,
        owner: Box<Expr>,
        rel: Box<Rel>,
    },
    Between {
        l: Box<Expr>,
        not: bool,
        lo: Box<Expr>,
        hi: Box<Expr>,
    },
    IsNull {
        l: Box<Expr>,
        not: bool,
    },
    Refs {
        l: Box<Expr>,
        table: QTableId,
    },
    Like {
        l: Box<Expr>,
        not: bool,
        pat: Box<Expr>,
        esc: Option<char>,
    },
    Case {
        operand: Option<Box<Expr>>,
        whens: Vec<(Expr, Expr)>,
        else_: Option<Box<Expr>>,
    },
    Cast {
        x: Box<Expr>,
        to: QType,
        num: Option<NumQ>,
    },
    Func {
        f: Func,
        args: Vec<Expr>,
    },
    Unit(Kw),
}

#[derive(Clone, Debug, PartialEq)]
pub struct AggCall {
    pub f: Func,
    pub distinct: bool,
    pub arg: Option<Expr>,
    pub ty: QType,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SortKey {
    pub expr: Expr,
    pub desc: bool,
}

#[derive(Debug)]
pub enum BuildError {
    Later {
        what: &'static str,
        stage: &'static str,
    },
    Internal(String),
    Compile(CompileError),
}

impl std::fmt::Display for BuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BuildError::Later { what, stage } => write!(f, "not supported yet: {what}: {stage}"),
            BuildError::Internal(s) => write!(f, "internal error: {s}"),
            BuildError::Compile(e) => write!(f, "{e:?}"),
        }
    }
}

impl std::error::Error for BuildError {}

pub fn uses_week(s: &StmtS) -> bool {
    let mut found = false;
    let mut look = |e: &Expr| {
        if let ExprKind::Func { f, args, .. } = &e.kind {
            let week_unit = args
                .iter()
                .any(|a| matches!(a.kind, ExprKind::Unit(query_syntax::Kw::Week)));
            found |= *f == query_syntax::Func::Week
                || (matches!(
                    f,
                    query_syntax::Func::BeginOfPeriod | query_syntax::Func::EndOfPeriod
                ) && week_unit);
        }
    };
    from_tir::visit_rel(&s.root, &mut look);
    for rows in s.nested.iter().filter_map(|n| n.rows.as_ref()) {
        from_tir::visit_rel(&rows.scan, &mut look);
        for e in &rows.exprs {
            from_tir::visit_expr(e, true, &mut look);
        }
    }
    if let Some(t) = &s.totals {
        found |= t.points.iter().any(|p| {
            p.periods
                .as_ref()
                .is_some_and(|x| x.unit == query_syntax::Kw::Week)
        });
    }
    found
}
