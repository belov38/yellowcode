use super::{
    arithmetic::{self, ArithmeticError, UnaryOp},
    cast::{self, CastError},
    collation::{CollationError, ManagedCollation},
    compare,
    functions::{self, FunctionError},
    like::{self, LikeError},
    type_info::{self, TypeEvalError},
};
use memory_budget::{BudgetError, BudgetedVec, Category, MemoryBudget, Shared};
use query_ir::{
    Dec, PExpr,
    managed_value::{
        ManagedRow, ManagedValue, ScalarValue, ValueRef,
        row::{ScopedInput, ScopedRow},
    },
};
use query_syntax::ast::CmpOp;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExprError {
    Memory(BudgetError),
    Arithmetic(ArithmeticError),
    Cast(CastError),
    Collation(CollationError),
    Function(FunctionError),
    Like(LikeError),
    Type(TypeEvalError),
    Column(u16),
    Outer { depth: u16, col: u16 },
    ValueSetOutsideIn,
    SetAgainstTuple,
    TupleWidth,
    Unsupported(&'static str),
}
macro_rules! error_from {
    ($($ty:ty => $variant:ident),* $(,)?) => {$ (
        impl From<$ty> for ExprError {
            fn from(value: $ty) -> Self { Self::$variant(value) }
        }
    )*};
}
error_from!(
    BudgetError => Memory, ArithmeticError => Arithmetic, CastError => Cast,
    CollationError => Collation, FunctionError => Function, LikeError => Like,
    TypeEvalError => Type,
);
impl std::fmt::Display for ExprError {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Memory(e) => e.fmt(out),
            Self::Arithmetic(e) => e.fmt(out),
            Self::Cast(e) => e.fmt(out),
            Self::Collation(e) => e.fmt(out),
            Self::Function(e) => e.fmt(out),
            Self::Like(e) => e.fmt(out),
            Self::Type(e) => e.fmt(out),
            Self::Column(i) => write!(out, "column {i} out of range"),
            Self::Outer { depth, col } => write!(out, "outer column {depth}:{col} out of range"),
            Self::ValueSetOutsideIn => out.write_str("a value set outside IN"),
            Self::SetAgainstTuple => out.write_str("a value set against a tuple"),
            Self::TupleWidth => out.write_str("IN list is not a multiple of the tuple"),
            Self::Unsupported(kind) => write!(out, "managed expression requires {kind}"),
        }
    }
}
impl std::error::Error for ExprError {}

fn value(row: &ScopedRow) -> Result<&ManagedValue, ExprError> {
    row.values().first().ok_or(ExprError::Column(0))
}
fn one(value: ManagedValue, budget: &MemoryBudget) -> Result<ScopedRow, ExprError> {
    let mut row = ManagedRow::try_with_capacity(budget, Category::Execution, 1)?;
    row.try_push(value)?;
    Ok(ScopedRow::try_from_owned(row)?)
}
fn boolean(value: Option<bool>) -> ManagedValue {
    ManagedValue::scalar(value.map_or(ScalarValue::Null, ScalarValue::Bool))
}
fn truth(value: &ManagedValue) -> Option<bool> {
    match value.view() {
        ValueRef::Bool(value) => Some(value),
        _ => None,
    }
}
fn arguments<'a>(
    expressions: impl ExactSizeIterator<Item = &'a PExpr>,
    input: &ScopedRow,
    outer: &[ScopedInput],
    coll: &ManagedCollation,
    limbs: u64,
) -> Result<ScopedRow, ExprError> {
    let mut values =
        ManagedRow::try_with_capacity(input.budget(), Category::Execution, expressions.len())?;
    for expression in expressions {
        let evaluated = eval_context(expression, input, outer, coll, limbs)?;
        values.try_push(value(&evaluated)?.clone())?;
    }
    Ok(ScopedRow::try_from_owned(values)?)
}

pub fn try_eval(
    expression: &PExpr,
    input: &ScopedRow,
    coll: &ManagedCollation,
    max_number_limbs: u64,
) -> Result<ScopedRow, ExprError> {
    eval_context(expression, input, &[], coll, max_number_limbs)
}

pub fn try_eval_with_outer(
    expression: &PExpr,
    input: &ScopedRow,
    outer: &[Shared<ScopedRow>],
    coll: &ManagedCollation,
    max_number_limbs: u64,
) -> Result<ScopedRow, ExprError> {
    coll.check_budget(input.budget())?;
    for source in outer {
        drop(ScopedInput::try_new(source.clone(), input.budget())?);
    }
    let mut admitted =
        BudgetedVec::try_with_capacity(input.budget(), Category::Execution, outer.len())?;
    for source in outer {
        admitted.try_push(ScopedInput::try_new(source.clone(), input.budget())?)?;
    }
    eval_context(expression, input, &admitted, coll, max_number_limbs)
}

fn eval_context(
    expression: &PExpr,
    input: &ScopedRow,
    outer: &[ScopedInput],
    coll: &ManagedCollation,
    max_number_limbs: u64,
) -> Result<ScopedRow, ExprError> {
    coll.check_budget(input.budget())?;
    input
        .budget()
        .check_admission(Category::Execution)
        .map_err(BudgetError::from)?;
    let eval = |e: &PExpr| eval_context(e, input, outer, coll, max_number_limbs);
    let args = |xs: &[&PExpr]| arguments(xs.iter().copied(), input, outer, coll, max_number_limbs);
    let output = match expression {
        PExpr::Col(index) => input
            .values()
            .get(usize::from(*index))
            .cloned()
            .ok_or(ExprError::Column(*index))?,
        PExpr::Lit(value) => ManagedValue::try_copy(value, input.budget())?,
        PExpr::Unit(unit) => ManagedValue::try_string(unit.ru(), input.budget())?,
        PExpr::Neg(child) => {
            let child = eval(child)?;
            arithmetic::try_unary(UnaryOp::Neg, &child, 0, max_number_limbs)?
        }
        PExpr::Not(child) => {
            let child = eval(child)?;
            boolean(crate::eval::not3(truth(value(&child)?)))
        }
        PExpr::And(children) | PExpr::Or(children) => {
            let is_and = matches!(expression, PExpr::And(_));
            let mut unknown = false;
            for child in children {
                let child = eval(child)?;
                match truth(value(&child)?) {
                    Some(hit) if hit != is_and => {
                        return one(boolean(Some(hit)), input.budget());
                    }
                    None => unknown = true,
                    _ => {}
                }
            }
            boolean(if unknown { None } else { Some(is_and) })
        }
        PExpr::Compare(op, left, right) => {
            let values = args(&[left, right])?;
            boolean(compare_at(*op, &values, 0, 1, coll)?)
        }
        PExpr::Between { l, not, lo, hi } => {
            let values = args(&[l, lo, hi])?;
            let found = crate::eval::and3([
                compare_at(CmpOp::Ge, &values, 0, 1, coll)?,
                compare_at(CmpOp::Le, &values, 0, 2, coll)?,
            ]);
            boolean(if *not {
                crate::eval::not3(found)
            } else {
                found
            })
        }
        PExpr::IsNull { l, not } => {
            let value = eval(l)?;
            boolean(Some(
                matches!(self::value(&value)?.view(), ValueRef::Null) != *not,
            ))
        }
        PExpr::Refs { l, number } => {
            let value = eval(l)?;
            boolean(match self::value(&value)?.view() {
                ValueRef::Null => None,
                ValueRef::Ref { table, .. } => Some(table == *number),
                _ => Some(false),
            })
        }
        PExpr::Like { l, not, pat, esc } => {
            let values = args(&[l, pat])?;
            let found = like::try_like(&values, 0, 1, *esc, coll)?;
            boolean(if *not {
                crate::eval::not3(found)
            } else {
                found
            })
        }
        PExpr::Case {
            operand,
            whens,
            else_,
        } => {
            let operand = operand.as_ref().map(|e| eval(e)).transpose()?;
            for (when, then) in whens {
                let when = eval(when)?;
                let hit = match &operand {
                    Some(operand) => compare::try_eq3(
                        value(operand)?.view(),
                        value(&when)?.view(),
                        coll,
                        input.budget(),
                    )?,
                    None => truth(value(&when)?),
                };
                if hit == Some(true) {
                    return eval(then);
                }
            }
            return match else_ {
                Some(other) => eval(other),
                None => one(boolean(None), input.budget()),
            };
        }
        PExpr::Cast {
            x,
            to,
            num,
            ref_tables,
            load,
        } => {
            if *load {
                return Err(ExprError::Unsupported("value table load cast"));
            }
            let source = eval(x)?;
            match num {
                Some(number) => {
                    let ty = to.try_clone_budgeted(input.budget())?.map(|mut ty| {
                        ty.number = Some(*number);
                        ty
                    });
                    cast::try_cast(&source, 0, &ty, ref_tables)?
                }
                None => cast::try_cast(&source, 0, to, ref_tables)?,
            }
        }
        PExpr::TypeOf { x, refs } => {
            let source = eval(x)?;
            type_info::try_type_of(&source, 0, refs)?
        }
        PExpr::Func {
            f,
            args,
            ty,
            arg_dec,
        } => {
            let constant = args.iter().all(|e| matches!(e, PExpr::Lit(_)));
            let values = arguments(args.iter(), input, outer, coll, max_number_limbs)?;
            functions::try_call(*f, &values, ty, coll, *arg_dec, constant, max_number_limbs)?
        }
        PExpr::Arith {
            first,
            first_dec,
            steps,
            decs,
        } => {
            let expressions = std::iter::once(first.as_ref())
                .chain(steps.iter().map(|(_, expression, _)| expression));
            let mut values = ManagedRow::try_with_capacity(
                input.budget(),
                Category::Execution,
                steps.len().checked_add(1).ok_or(BudgetError::Overflow)?,
            )?;
            for expression in expressions {
                let evaluated = eval(expression)?;
                values.try_push(value(&evaluated)?.clone())?;
            }
            let values = ScopedRow::try_from_owned(values)?;
            let mut accumulator = one(value(&values)?.clone(), input.budget())?;
            let mut precision = *first_dec;
            for (index, (op, _, _)) in steps.iter().enumerate() {
                let rhs = values.values().get(index + 1).ok_or(ExprError::Column(0))?;
                let mut pair =
                    ManagedRow::try_with_capacity(input.budget(), Category::Execution, 2)?;
                pair.try_push(value(&accumulator)?.clone())?;
                pair.try_push(rhs.clone())?;
                let pair = ScopedRow::try_from_owned(pair)?;
                let rhs_dec = decs.get(index).map_or(Dec::UNKNOWN, |d| d.rhs);
                let (next, next_dec) =
                    arithmetic::try_step(*op, &pair, 0, precision, 1, rhs_dec, max_number_limbs)?;
                accumulator = one(next, input.budget())?;
                precision = next_dec;
            }
            return Ok(accumulator);
        }
        PExpr::Outer { depth, col } => outer
            .len()
            .checked_sub(usize::from(*depth))
            .and_then(|i| outer.get(i))
            .and_then(|row| row.values().get(usize::from(*col)))
            .cloned()
            .ok_or(ExprError::Outer {
                depth: *depth,
                col: *col,
            })?,
        PExpr::InQuery { .. } => return Err(ExprError::Unsupported("subquery context")),
        PExpr::SectionAgg { .. } => {
            return Err(ExprError::Unsupported("aggregate over a tabular section"));
        }
        PExpr::StoredSize { .. } => return Err(ExprError::Unsupported("stored data size")),
        PExpr::InList { l, not, rhs } => {
            let left = arguments(l.iter(), input, outer, coll, max_number_limbs)?;
            let found = in_list(&left, rhs, input, outer, coll, max_number_limbs)?;
            return one(
                boolean(if *not {
                    crate::eval::not3(found)
                } else {
                    found
                }),
                input.budget(),
            );
        }
        PExpr::Set(_) => return Err(ExprError::ValueSetOutsideIn),
    };
    one(output, input.budget())
}

fn compare_at(
    op: CmpOp,
    values: &ScopedRow,
    left: usize,
    right: usize,
    coll: &ManagedCollation,
) -> Result<Option<bool>, ExprError> {
    let get = |index| values.values().get(index).ok_or(ExprError::Column(0));
    Ok(compare::try_compare(
        op,
        get(left)?.view(),
        get(right)?.view(),
        coll,
        values.budget(),
    )?)
}

fn tuple_eq(
    left: &ScopedRow,
    right: &ScopedRow,
    coll: &ManagedCollation,
) -> Result<Option<bool>, ExprError> {
    let mut unknown = false;
    for (a, b) in left.values().iter().zip(right.values()) {
        match compare::try_eq3(a.view(), b.view(), coll, left.budget())? {
            Some(false) => return Ok(Some(false)),
            None => unknown = true,
            Some(true) => {}
        }
    }
    Ok(if unknown { None } else { Some(true) })
}
fn merge(previous: Option<bool>, next: Option<bool>) -> Option<bool> {
    match next {
        Some(true) => Some(true),
        None if previous == Some(false) => None,
        _ => previous,
    }
}
fn scan_set(
    left: &ScopedRow,
    set: &query_ir::ValueSet,
    mut found: Option<bool>,
    coll: &ManagedCollation,
) -> Result<Option<bool>, ExprError> {
    for value in &set.values {
        let right = one(ManagedValue::try_copy(value, left.budget())?, left.budget())?;
        found = merge(found, tuple_eq(left, &right, coll)?);
        if found == Some(true) {
            break;
        }
    }
    Ok(found)
}
fn in_list(
    left: &ScopedRow,
    rhs: &[PExpr],
    input: &ScopedRow,
    outer: &[ScopedInput],
    coll: &ManagedCollation,
    limbs: u64,
) -> Result<Option<bool>, ExprError> {
    let width = left.values().len().max(1);
    if let [PExpr::Set(set)] = rhs {
        if width != 1 {
            return Err(ExprError::SetAgainstTuple);
        }
        return scan_set(left, set, Some(false), coll);
    }
    if !rhs.len().is_multiple_of(width) {
        return Err(ExprError::TupleWidth);
    }
    let mut found = Some(false);
    for chunk in rhs.chunks(width) {
        if let [PExpr::Set(set)] = chunk {
            found = scan_set(left, set, found, coll)?;
        } else {
            let mut right =
                ManagedRow::try_with_capacity(input.budget(), Category::Execution, width)?;
            for expression in chunk {
                if matches!(expression, PExpr::Set(_)) {
                    return Err(ExprError::SetAgainstTuple);
                }
                let evaluated = eval_context(expression, input, outer, coll, limbs)?;
                right.try_push(value(&evaluated)?.clone())?;
            }
            let right = ScopedRow::try_from_owned(right)?;
            found = merge(found, tuple_eq(left, &right, coll)?);
        }
        if found == Some(true) {
            break;
        }
    }
    Ok(found)
}
