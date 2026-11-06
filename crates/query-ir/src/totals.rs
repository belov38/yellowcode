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

impl TotalsSpec {
    pub fn try_clone_with<E>(&self, check: &mut impl FnMut() -> Result<(), E>) -> Result<Self, E> {
        fn copy_values<T: Clone, E>(
            values: &[T],
            check: &mut impl FnMut() -> Result<(), E>,
        ) -> Result<Vec<T>, E> {
            let mut copy = Vec::with_capacity(values.len());
            for value in values {
                check()?;
                copy.push(value.clone());
            }
            Ok(copy)
        }
        check()?;
        let points = copy_values(&self.points, check)?;
        let aggs = copy_values(&self.aggs, check)?;
        let mut items = Vec::with_capacity(self.items.len());
        for (expression, column) in &self.items {
            items.push((expression.try_clone_with(check)?, *column));
        }
        let order = copy_values(&self.order, check)?;
        Ok(Self {
            width: self.width,
            points,
            aggs,
            items,
            order,
        })
    }
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

#[derive(Debug, PartialEq)]
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

type CloneTask<'s, 'd> = (&'s TotalsExpr, &'d mut TotalsExpr);

fn clone_push<'s, 'd>(work: &mut Vec<CloneTask<'s, 'd>>, task: CloneTask<'s, 'd>) {
    if work.len() == work.capacity() {
        let capacity = work.capacity().saturating_mul(2).max(4);
        let mut replacement = Vec::with_capacity(capacity);
        replacement.append(work);
        *work = replacement;
    }
    work.push(task);
}

impl TotalsExpr {
    fn placeholder() -> Self {
        Self::Const(QValue::Null)
    }

    fn take(expression: &mut Self) -> Self {
        std::mem::replace(expression, Self::placeholder())
    }

    fn is_placeholder(&self) -> bool {
        matches!(self, Self::Const(QValue::Null))
    }

    fn leaf_clone(&self) -> Option<Self> {
        match self {
            Self::Agg(index) => Some(Self::Agg(*index)),
            Self::Param(slot) => Some(Self::Param(*slot)),
            Self::Column(column) => Some(Self::Column(*column)),
            Self::Const(value) => Some(Self::Const(value.clone())),
            _ => None,
        }
    }

    fn drained(&self) -> bool {
        match self {
            Self::Agg(_) | Self::Param(_) | Self::Column(_) | Self::Const(_) => true,
            Self::Neg(child) | Self::Not(child) => child.is_placeholder(),
            Self::Bin(_, left, right) | Self::Cmp(_, left, right) | Self::IsNull(left, right) => {
                left.is_placeholder() && right.is_placeholder()
            }
            Self::And(values) | Self::Or(values) => values.is_empty(),
            Self::Case { whens, otherwise } => whens.is_empty() && otherwise.is_none(),
        }
    }
}

impl TotalsExpr {
    pub fn try_clone_with<E>(&self, check: &mut impl FnMut() -> Result<(), E>) -> Result<Self, E> {
        check()?;
        if let Some(leaf) = self.leaf_clone() {
            return Ok(leaf);
        }
        let mut result = Self::placeholder();
        let mut work = Vec::new();
        clone_push(&mut work, (self, &mut result));
        while let Some((source, target)) = work.pop() {
            check()?;
            if let Some(leaf) = source.leaf_clone() {
                *target = leaf;
                continue;
            }
            let boxed = || Box::new(Self::placeholder());
            *target = match source {
                Self::Neg(_) => Self::Neg(boxed()),
                Self::Not(_) => Self::Not(boxed()),
                Self::Bin(op, _, _) => Self::Bin(*op, boxed(), boxed()),
                Self::Cmp(op, _, _) => Self::Cmp(*op, boxed(), boxed()),
                Self::IsNull(_, _) => Self::IsNull(boxed(), boxed()),
                Self::And(values) | Self::Or(values) => {
                    let mut copy = Vec::with_capacity(values.len());
                    for _ in values {
                        check()?;
                        copy.push(Self::placeholder());
                    }
                    if matches!(source, Self::And(_)) {
                        Self::And(copy)
                    } else {
                        Self::Or(copy)
                    }
                }
                Self::Case { whens, otherwise } => {
                    let mut copy = Vec::with_capacity(whens.len());
                    for _ in whens {
                        check()?;
                        copy.push((Self::placeholder(), Self::placeholder()));
                    }
                    Self::Case {
                        whens: copy,
                        otherwise: otherwise.as_ref().map(|_| boxed()),
                    }
                }
                _ => Self::placeholder(),
            };
            match (source, target) {
                (Self::Neg(a), Self::Neg(b)) | (Self::Not(a), Self::Not(b)) => {
                    clone_push(&mut work, (a, b));
                }
                (Self::Bin(_, a, b), Self::Bin(_, x, y))
                | (Self::Cmp(_, a, b), Self::Cmp(_, x, y))
                | (Self::IsNull(a, b), Self::IsNull(x, y)) => {
                    clone_push(&mut work, (b, y));
                    clone_push(&mut work, (a, x));
                }
                (Self::And(a), Self::And(b)) | (Self::Or(a), Self::Or(b)) => {
                    for (source, target) in a.iter().zip(b.iter_mut()).rev() {
                        check()?;
                        clone_push(&mut work, (source, target));
                    }
                }
                (
                    Self::Case {
                        whens: a,
                        otherwise: b,
                    },
                    Self::Case {
                        whens: x,
                        otherwise: y,
                    },
                ) => {
                    if let (Some(source), Some(target)) = (b, y) {
                        clone_push(&mut work, (source, target));
                    }
                    for ((a, b), (x, y)) in a.iter().zip(x.iter_mut()).rev() {
                        check()?;
                        clone_push(&mut work, (b, y));
                        clone_push(&mut work, (a, x));
                    }
                }
                _ => {}
            }
        }
        drop(work);
        Ok(result)
    }
}

impl Clone for TotalsExpr {
    fn clone(&self) -> Self {
        match self.try_clone_with(&mut || Ok::<(), std::convert::Infallible>(())) {
            Ok(copy) => copy,
            Err(never) => match never {},
        }
    }
}

struct DropStack {
    head: TotalsExpr,
}

impl DropStack {
    fn next(&mut self) -> Option<TotalsExpr> {
        loop {
            match &mut self.head {
                TotalsExpr::Bin(_, previous, child)
                | TotalsExpr::Cmp(_, previous, child)
                | TotalsExpr::IsNull(previous, child) => {
                    let previous = TotalsExpr::take(previous);
                    let child = TotalsExpr::take(child);
                    self.head = previous;
                    return Some(child);
                }
                TotalsExpr::And(values) | TotalsExpr::Or(values) => {
                    let previous = values.pop()?;
                    if let Some(child) = values.pop() {
                        values.push(previous);
                        return Some(child);
                    }
                    self.head = previous;
                }
                TotalsExpr::Case { whens, otherwise } => {
                    if let Some(mut link) = otherwise.take() {
                        let previous = TotalsExpr::take(&mut link);
                        drop(link);
                        return Some(std::mem::replace(&mut self.head, previous));
                    }
                    let (previous, value) = whens.pop()?;
                    if !value.is_placeholder() {
                        whens.push((previous, TotalsExpr::placeholder()));
                        return Some(value);
                    }
                    return Some(std::mem::replace(&mut self.head, previous));
                }
                _ => return None,
            }
        }
    }
}

impl Drop for TotalsExpr {
    fn drop(&mut self) {
        if self.drained() {
            return;
        }
        let mut current = Self::take(self);
        let mut pending = DropStack {
            head: Self::placeholder(),
        };
        loop {
            match &mut current {
                Self::Neg(child) | Self::Not(child) => {
                    let child = Self::take(child);
                    current = child;
                    continue;
                }
                Self::Bin(_, first, _) | Self::Cmp(_, first, _) | Self::IsNull(first, _) => {
                    let child = Self::take(first);
                    **first = Self::take(&mut pending.head);
                    pending.head = std::mem::replace(&mut current, child);
                    continue;
                }
                Self::And(values) | Self::Or(values) => {
                    if let Some(child) = values.pop() {
                        values.push(Self::take(&mut pending.head));
                        pending.head = std::mem::replace(&mut current, child);
                        continue;
                    }
                }
                Self::Case { whens, otherwise } => {
                    if let Some(link) = otherwise {
                        let child = Self::take(link);
                        **link = Self::take(&mut pending.head);
                        pending.head = std::mem::replace(&mut current, child);
                        continue;
                    }
                    if let Some((condition, value)) = whens.pop() {
                        whens.push((Self::take(&mut pending.head), value));
                        pending.head = std::mem::replace(&mut current, condition);
                        continue;
                    }
                }
                _ => {}
            }
            drop(Self::take(&mut current));
            match pending.next() {
                Some(child) => current = child,
                None => break,
            }
        }
    }
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
