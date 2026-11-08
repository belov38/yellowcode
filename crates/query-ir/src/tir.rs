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

pub fn totals_point_column(f: &TotalsField, items: &[Item]) -> Option<usize> {
    if f.col.is_some() {
        return f.col;
    }
    if !matches!(f.expr.kind, TKind::Field { .. }) {
        return None;
    }
    let mut matching = items
        .iter()
        .enumerate()
        .filter(|(_, item)| same_point_field(&item.expr, &f.expr));
    let (column, _) = matching.next()?;
    matching.next().is_none().then_some(column)
}

fn same_point_field(a: &TExpr, b: &TExpr) -> bool {
    match (&a.kind, &b.kind) {
        (
            TKind::Field {
                level: al,
                source: as_,
                col: ac,
                derefs: ad,
            },
            TKind::Field {
                level: bl,
                source: bs,
                col: bc,
                derefs: bd,
            },
        ) => {
            a.ty == b.ty
                && al == bl
                && as_ == bs
                && ac == bc
                && ad.len() == bd.len()
                && ad.iter().zip(bd).all(|(x, y)| {
                    x.targets == y.targets
                        && (!x.targets.is_empty()
                            || bsl_prim::fold_name(&x.name) == bsl_prim::fold_name(&y.name))
                })
        }
        _ => false,
    }
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

pub(crate) fn forwarded_nested_column(item: &Item) -> Option<u16> {
    match &item.expr.kind {
        TKind::Field {
            level: 0,
            source: 0,
            col: Col::Column(column),
            derefs,
        } if derefs.is_empty() => Some(*column),
        _ => None,
    }
}

pub(crate) fn nested_output_fields<'a>(select: &'a Select, item: &'a Item) -> Option<&'a [Item]> {
    if let Some(fields) = &item.nested {
        return Some(fields);
    }
    let column = forwarded_nested_column(item)?;
    let [source] = select.sources.as_slice() else {
        return None;
    };
    if !source.joins.is_empty() {
        return None;
    }
    let From::Subquery(query) = &source.from else {
        return None;
    };
    let [inner] = query.parts.as_slice() else {
        return None;
    };
    inner.items.get(usize::from(column))?.nested.as_deref()
}

pub(crate) fn nested_from_select(query: &Query) -> Option<&Select> {
    fn plain(query: &Query) -> Option<&Select> {
        let [select] = query.parts.as_slice() else {
            return None;
        };
        (!query.autoorder
            && query.totals.is_none()
            && !select.distinct
            && select.top.is_none()
            && select.group.is_none()
            && select.having.is_none())
        .then_some(select)
    }

    let outer = plain(query)?;
    let [source] = outer.sources.as_slice() else {
        return None;
    };
    if !source.joins.is_empty() {
        return None;
    }
    let From::Subquery(inner_query) = &source.from else {
        return None;
    };
    let inner = plain(inner_query)?;
    if !inner_query.order.is_empty()
        || inner.sources.len() > 1
        || inner.sources.iter().any(|source| {
            !source.joins.is_empty()
                || !matches!(&source.from, From::Table { args, records: None, drcr: None, .. } if args.is_empty())
        })
        || !inner.items.iter().any(|item| item.nested.is_some())
        || !outer.items.iter().any(|item| {
            item.nested.is_none() && nested_output_fields(outer, item).is_some()
        })
    {
        return None;
    }
    if !outer.items.iter().all(|item| {
        item.nested.is_none()
            && (nested_output_fields(outer, item).is_some()
                || nested_forward_scalar(&item.expr, 128, false))
    }) || !inner.items.iter().all(|item| {
        nested_forward_scalar(&item.expr, 128, item.nested.is_some())
            && item.nested.iter().flatten().all(|child| {
                child.nested.is_none() && nested_forward_scalar(&child.expr, 128, false)
            })
    }) || outer
        .where_
        .iter()
        .chain(&inner.where_)
        .any(|expr| !nested_forward_scalar(expr, 128, false))
    {
        return None;
    }
    if !query
        .order
        .iter()
        .all(|key| nested_forward_order(outer, inner, key))
    {
        return None;
    }
    Some(inner)
}

fn nested_forward_order(outer: &Select, inner: &Select, key: &OrderKey) -> bool {
    if !matches!(key.dir, OrderDir::Asc | OrderDir::Desc)
        || !nested_forward_order_type(&key.expr.ty)
    {
        return false;
    }
    let TKind::Field {
        level: 0,
        source: 0,
        col: Col::Column(column),
        derefs,
    } = &key.expr.kind
    else {
        return false;
    };
    if !derefs.is_empty() {
        return false;
    }
    let Some(origin) = inner.items.get(usize::from(*column)) else {
        return false;
    };
    if origin.nested.is_some() || origin.unknown || origin.expr.ty != key.expr.ty {
        return false;
    }
    let direct = match &origin.expr.kind {
        TKind::Field {
            level: 0,
            source: 0,
            col: Col::Field(_),
            derefs,
        } => inner.sources.len() == 1 && derefs.is_empty(),
        TKind::Const(
            QValue::Null
            | QValue::Bool(_)
            | QValue::Number(_)
            | QValue::String(_)
            | QValue::Date(_),
        ) => true,
        _ => false,
    };
    if !direct {
        return false;
    }
    let selected = |item: &Item| {
        item.nested.is_none()
            && !item.unknown
            && item.expr.ty == key.expr.ty
            && forwarded_nested_column(item) == Some(*column)
            && nested_output_fields(outer, item).is_none()
    };
    match key.col {
        Some(column) => outer.items.get(column).is_some_and(selected),
        None => outer.items.iter().any(selected),
    }
}

fn nested_forward_order_type(ty: &QType) -> bool {
    ty.refs.is_empty()
        && ty.others.is_empty()
        && ty.string.is_none_or(|string| string.len > 0)
        && (ty.null
            || ty.boolean
            || ty.number.is_some()
            || ty.string.is_some()
            || ty.date.is_some())
}

fn nested_forward_scalar(expr: &TExpr, remaining: u16, nested_owner: bool) -> bool {
    if remaining == 0
        || matches!(expr.kind, TKind::InQuery { .. } | TKind::Agg { .. })
        || matches!(expr.kind, TKind::Field { level, .. } if level != 0)
        || (!nested_owner && expr.ty.others.contains(&crate::Other::QueryResult))
    {
        return false;
    }
    let mut scalar = true;
    crate::sema::select::each_child(expr, &mut |child| {
        if scalar {
            scalar = nested_forward_scalar(child, remaining - 1, false);
        }
    });
    scalar
}

#[derive(Clone, Debug, PartialEq)]
pub struct Source {
    pub all_fields: bool,
    pub alias: String,
    pub from: From,
    pub joins: Vec<Join>,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct RecordsOptions {
    pub order_desc: bool,
    pub top: Option<u64>,
    pub deferred: Option<RecordsDeferred>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordsDeferred {
    Composition,
    BoundExpression,
    OrderShape,
    TopShape,
    TopRange,
}
impl RecordsDeferred {
    pub(crate) fn what(self) -> &'static str {
        match self {
            Self::Composition => "Q6a-RecordsWithExtDimensionsComposition",
            Self::BoundExpression => "Q6a-RecordsWithExtDimensionsBoundsExpression",
            Self::OrderShape => "Q6a-RecordsWithExtDimensionsOrder",
            Self::TopShape => "Q6a-RecordsWithExtDimensionsTop",
            Self::TopRange => "Q6a-RecordsWithExtDimensionsTopRange",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum From {
    Table {
        id: QTableId,
        args: Vec<Option<TExpr>>,
        records: Option<RecordsOptions>,
        accounting: Option<AccountingArguments>,
        accounting_dependencies: Option<AccountingDependencies>,
        drcr: Option<DrCrSource>,
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AccountingArguments {
    pub date_slots: u8,
    pub deferred: bool,
}

impl AccountingArguments {
    pub(crate) fn plain_date(expression: &TExpr) -> bool {
        expression.ty.date.is_some_and(|qualifier| {
            matches!(qualifier, crate::DateQ::Date | crate::DateQ::DateTime)
                && expression.ty == QType::date(qualifier)
                && matches!(
                    &expression.kind,
                    TKind::Const(QValue::Date(_)) | TKind::Param(_)
                )
        })
    }

    pub(crate) fn supports(self, args: &[Option<TExpr>], periods: usize) -> bool {
        let mask = (1u8 << periods) - 1;
        !self.deferred
            && self.date_slots & !mask == 0
            && (0..periods).all(|i| {
                let expression = args.get(i).and_then(Option::as_ref);
                match expression {
                    Some(e) => self.date_slots & (1 << i) != 0 && Self::plain_date(e),
                    None => self.date_slots & (1 << i) == 0,
                }
            })
            && args.iter().skip(periods).all(Option::is_none)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct AccountingDependencies {
    pub supplied: Vec<u8>,
    pub dependencies: Vec<DrCrArgDependency>,
    pub complete: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DrCrSource {
    pub supplied: Vec<u8>,
    pub dependencies: Vec<DrCrArgDependency>,
    pub date_slots: u8,
}

impl DrCrSource {
    pub(crate) fn date_argument(&self, argument: usize) -> Option<&TExpr> {
        if argument >= 2 || self.supplied.get(argument) != Some(&1) {
            return None;
        }
        let mut dependencies = self
            .dependencies
            .iter()
            .filter(|d| usize::from(d.argument) == argument);
        let dependency = dependencies.next()?;
        if dependencies.next().is_some()
            || dependency.ordinal != 0
            || dependency.scope != DrCrArgScope::NoSources
        {
            return None;
        }
        let DrCrArgRead::Expression(expression) = &dependency.read else {
            return None;
        };
        let qualifier = expression.ty.date?;
        if !matches!(qualifier, crate::DateQ::Date | crate::DateQ::DateTime)
            || expression.ty != QType::date(qualifier)
            || !matches!(
                &expression.kind,
                TKind::Const(QValue::Date(_)) | TKind::Param(_)
            )
        {
            return None;
        }
        Some(expression)
    }

    pub(crate) fn has_unsupported_arguments(&self) -> bool {
        self.date_slots & !0b11 != 0
            || (0..2).any(|i| self.date_slots & (1 << i) != 0 && self.date_argument(i).is_none())
            || self
                .supplied
                .iter()
                .enumerate()
                .any(|(i, supplied)| *supplied != 0 && (i >= 2 || self.date_slots & (1 << i) == 0))
    }
}
#[derive(Clone, Debug, PartialEq)]
pub struct DrCrArgDependency {
    pub argument: u16,
    pub ordinal: u32,
    pub scope: DrCrArgScope,
    pub read: DrCrArgRead,
}
#[derive(Clone, Debug, PartialEq)]
pub enum DrCrArgRead {
    Expression(TExpr),
    Wildcard(DrCrWildcard),
    Nested(DrCrNested),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DrCrArgScope {
    NoSources,
    Condition,
    Account(crate::schema::StdRole),
    Nested(QTableId),
}
#[derive(Clone, Debug, PartialEq)]
pub struct DrCrWildcard {
    pub base: TExpr,
    pub targets: Vec<DrCrWildcardTarget>,
    pub span: Span,
}
#[derive(Clone, Debug, PartialEq)]
pub struct DrCrWildcardTarget {
    pub table: QTableId,
    pub path: Vec<Deref>,
    pub fields: Vec<QFieldId>,
    pub all_fields: bool,
}
#[derive(Clone, Debug, PartialEq)]
pub struct DrCrNested {
    pub base: TExpr,
    pub table: QTableId,
    pub dependencies: Vec<DrCrArgDependency>,
    pub span: Span,
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
