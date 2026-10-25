use super::{AResult, CompileArena, NativeOwned, NativeVec};
use memory_budget::BudgetError;

pub(crate) trait ManagedClone: Sized {
    fn graph_depth(&self, _remaining: u16) -> AResult<u16> {
        Ok(0)
    }
    fn clone_owned<'a>(&self, arena: &'a CompileArena) -> AResult<NativeOwned<'a, Self>>;
}
macro_rules! copy_clone {
    ($($ty:ty),* $(,)?) => { $( impl ManagedClone for $ty {
        fn clone_owned<'a>(&self, arena: &'a CompileArena) -> AResult<NativeOwned<'a, Self>> {
            Ok(NativeOwned::leaf(*self, Some(arena), 0))
        }
    })* };
}
copy_clone!(
    bool,
    u8,
    u16,
    u32,
    u64,
    usize,
    i32,
    i64,
    char,
    crate::types::NumQ,
    crate::types::StrQ,
    crate::types::DateQ,
    crate::types::RefAtom,
    crate::types::Other,
    crate::schema::QTableId,
    crate::schema::QFieldId,
    query_syntax::Kw,
    query_syntax::Span
);
copy_clone!(
    crate::schema::Periodicity,
    crate::schema::Complement,
    crate::tir::TotalsHierarchy,
    crate::tir::Col,
    query_syntax::ast::JoinKind,
    query_syntax::ast::OrderDir,
    query_syntax::ast::BinOp,
    query_syntax::ast::CmpOp,
    query_syntax::Func,
    brace::Guid,
    platform_catalog::TypeIdx,
    bsl_prim::BslDate
);

impl ManagedClone for String {
    fn clone_owned<'a>(&self, arena: &'a CompileArena) -> AResult<NativeOwned<'a, Self>> {
        let value = arena.string(self)?;
        let bytes = value.capacity() as u64;
        Ok(NativeOwned::leaf(value, Some(arena), bytes))
    }
}
impl<T: ManagedClone> ManagedClone for Vec<T> {
    fn graph_depth(&self, remaining: u16) -> AResult<u16> {
        self.iter()
            .try_fold(0, |depth, item| Ok(depth.max(item.graph_depth(remaining)?)))
    }
    fn clone_owned<'a>(&self, arena: &'a CompileArena) -> AResult<NativeOwned<'a, Self>> {
        let mut value = NativeVec::clone_empty(arena, self.len())?;
        for child in self {
            value.clone_push(child.clone_owned(arena)?)?;
        }
        Ok(value.finish())
    }
}
impl<T: ManagedClone> ManagedClone for Option<T> {
    fn graph_depth(&self, remaining: u16) -> AResult<u16> {
        self.as_ref()
            .map_or(Ok(0), |item| item.graph_depth(remaining))
    }
    fn clone_owned<'a>(&self, arena: &'a CompileArena) -> AResult<NativeOwned<'a, Self>> {
        match self {
            Some(value) => Ok(value.clone_owned(arena)?.map(Some)),
            None => Ok(NativeOwned::leaf(None, Some(arena), 0)),
        }
    }
}
impl<T: ManagedClone> ManagedClone for Box<T> {
    fn graph_depth(&self, remaining: u16) -> AResult<u16> {
        (**self).graph_depth(remaining)
    }
    fn clone_owned<'a>(&self, arena: &'a CompileArena) -> AResult<NativeOwned<'a, Self>> {
        (**self).clone_owned(arena)?.boxed()
    }
}
impl<A: ManagedClone, B: ManagedClone> ManagedClone for (A, B) {
    fn graph_depth(&self, remaining: u16) -> AResult<u16> {
        Ok(self
            .0
            .graph_depth(remaining)?
            .max(self.1.graph_depth(remaining)?))
    }
    fn clone_owned<'a>(&self, arena: &'a CompileArena) -> AResult<NativeOwned<'a, Self>> {
        self.0.clone_owned(arena)?.zip(self.1.clone_owned(arena)?)
    }
}
macro_rules! fields {
    ($arena:expr, $($make:ident)::+ { $first:ident $(, $rest:ident)* $(,)? }) => {{
        let owned = $first.clone_owned($arena)?;
        fields!(@zip owned; $arena; $($make)::+; $first; $($rest),*)
    }};
    (@zip $owned:ident; $arena:expr; $($make:ident)::+; $pattern:tt; $next:ident $(, $rest:ident)*) => {{
        let owned = $owned.zip($next.clone_owned($arena)?)?;
        fields!(@zip owned; $arena; $($make)::+; ($pattern, $next); $($rest),*)
    }};
    (@zip $owned:ident; $arena:expr; $($make:ident)::+; $pattern:tt;) => {
        Ok($owned.map(|$pattern| fields!(@names $($make)::+; (); $pattern)))
    };
    (@names $($make:ident)::+; ($($names:ident,)*); ($left:tt, $right:ident)) => {
        fields!(@names $($make)::+; ($right, $($names,)*); $left)
    };
    (@names $($make:ident)::+; ($($names:ident,)*); $first:ident) => { $($make)::+ { $first, $($names,)* } };
}
macro_rules! struct_clone {
    ($ty:path { $($field:ident),* $(,)? }) => {
        impl ManagedClone for $ty {
            fn graph_depth(&self,remaining:u16)->AResult<u16> {
                let mut depth=0;
                $(depth=depth.max(self.$field.graph_depth(remaining)?);)*
                Ok(depth)
            }
            fn clone_owned<'a>(&self, arena: &'a CompileArena) -> AResult<NativeOwned<'a, Self>> {
                let depth=self.graph_depth(128)?;
                $(let $field = &self.$field;)*
                fields!(arena, Self { $($field),* }).map(|value|value.set_depth(depth))
            }
        }
    };
}
struct_clone!(crate::types::QType {
    null,
    boolean,
    number,
    string,
    date,
    refs,
    others
});
impl<T: ManagedClone + super::native::SharedPayload> ManagedClone for crate::QShared<T> {
    fn clone_owned<'a>(&self, arena: &'a CompileArena) -> AResult<NativeOwned<'a, Self>> {
        if let Some(value) = self.managed_on(&arena.budget) {
            return Ok(NativeOwned::leaf(value, Some(arena), 0));
        }
        if arena.shared_depth >= 128 {
            return Err(BudgetError::Overflow);
        }
        let child = CompileArena::child(&arena.budget, arena.shared_depth + 1)?;
        let value = (**self)
            .clone_owned(&child)?
            .into_durable()?
            .into_shared()?;
        Ok(NativeOwned::leaf(value, Some(arena), 0))
    }
}
struct_clone!(crate::NestedResult { columns, rows });
struct_clone!(crate::tir::Query {
    parts,
    unions,
    order,
    autoorder,
    totals
});
struct_clone!(crate::tir::OrderKey { expr, col, dir });
struct_clone!(crate::tir::Totals {
    items,
    hits,
    points
});
struct_clone!(crate::tir::TotalsField {
    expr,
    col,
    hierarchy,
    periods,
    alias
});
struct_clone!(crate::tir::TotalsPeriods { unit, begin, end });
struct_clone!(crate::tir::Select {
    allowed,
    distinct,
    top,
    items,
    sources,
    where_,
    group,
    having,
    for_update,
    index_by,
    index_unique
});
impl ManagedClone for crate::tir::Item {
    fn graph_depth(&self, remaining: u16) -> AResult<u16> {
        let expr = self.expr.graph_depth(remaining)?;
        match &self.nested {
            None => Ok(expr),
            Some(nested) => Ok(expr.max(
                nested.graph_depth(remaining.checked_sub(1).ok_or(BudgetError::Overflow)?)? + 1,
            )),
        }
    }
    fn clone_owned<'a>(&self, arena: &'a CompileArena) -> AResult<NativeOwned<'a, Self>> {
        let depth = self.graph_depth(128)?;
        let (expr, name, nested, unknown) = (&self.expr, &self.name, &self.nested, &self.unknown);
        fields!(
            arena,
            Self {
                expr,
                name,
                nested,
                unknown
            }
        )
        .map(|value| value.set_depth(depth))
    }
}
struct_clone!(crate::tir::Source { alias, from, joins });
impl ManagedClone for crate::tir::Join {
    fn graph_depth(&self, remaining: u16) -> AResult<u16> {
        Ok(self.on.graph_depth(remaining)?.max(
            self.right
                .graph_depth(remaining.checked_sub(1).ok_or(BudgetError::Overflow)?)?
                + 1,
        ))
    }
    fn clone_owned<'a>(&self, arena: &'a CompileArena) -> AResult<NativeOwned<'a, Self>> {
        let depth = self.graph_depth(128)?;
        let (kind, right, on) = (&self.kind, &self.right, &self.on);
        fields!(arena, Self { kind, right, on }).map(|value| value.set_depth(depth))
    }
}
impl ManagedClone for crate::tir::TExpr {
    fn graph_depth(&self, remaining: u16) -> AResult<u16> {
        let remaining = remaining.checked_sub(1).ok_or(BudgetError::Overflow)?;
        Ok(self.kind.graph_depth(remaining)? + 1)
    }
    fn clone_owned<'a>(&self, arena: &'a CompileArena) -> AResult<NativeOwned<'a, Self>> {
        let depth = self.graph_depth(128)?;
        let (kind, ty, span) = (&self.kind, &self.ty, &self.span);
        fields!(arena, Self { kind, ty, span }).map(|value| value.set_depth(depth))
    }
}
struct_clone!(crate::tir::ArithStep {
    op,
    op_span,
    rhs,
    ty
});
struct_clone!(crate::tir::Deref { name, targets });
struct_clone!(crate::temp::TempColumn {
    name,
    ty,
    unknown,
    stored
});
struct_clone!(crate::temp::TempSchema {
    name,
    columns,
    open
});
struct_clone!(crate::params::ParamInfo { name, ty });
impl ManagedClone for crate::tir::ControlPoint {
    fn graph_depth(&self, remaining: u16) -> AResult<u16> {
        match self {
            Self::Overall => Ok(0),
            Self::Field(value) => value.graph_depth(remaining),
        }
    }
    fn clone_owned<'a>(&self, arena: &'a CompileArena) -> AResult<NativeOwned<'a, Self>> {
        match self {
            Self::Overall => Ok(NativeOwned::leaf(Self::Overall, Some(arena), 0)),
            Self::Field(value) => Ok(value.clone_owned(arena)?.map(Self::Field)),
        }
    }
}
impl ManagedClone for crate::tir::From {
    fn graph_depth(&self, remaining: u16) -> AResult<u16> {
        match self {
            Self::Table { args, .. } => args.graph_depth(remaining),
            Self::Subquery(query) => {
                Ok(query.graph_depth(remaining.checked_sub(1).ok_or(BudgetError::Overflow)?)? + 1)
            }
            Self::Param { .. } | Self::Temp(_) => Ok(0),
        }
    }
    fn clone_owned<'a>(&self, arena: &'a CompileArena) -> AResult<NativeOwned<'a, Self>> {
        match self {
            Self::Table {
                id,
                args,
                periodicity,
                complement,
                breakdown,
            } => fields!(
                arena,
                Self::Table {
                    id,
                    args,
                    periodicity,
                    complement,
                    breakdown
                }
            ),
            Self::Param { name, schema } => fields!(arena, Self::Param { name, schema }),
            Self::Temp(value) => Ok(value.clone_owned(arena)?.map(Self::Temp)),
            Self::Subquery(value) => {
                let depth = self.graph_depth(128)?;
                Ok(value
                    .clone_owned(arena)?
                    .map(Self::Subquery)
                    .set_depth(depth))
            }
        }
    }
}
impl ManagedClone for crate::schema::ValuePath {
    fn clone_owned<'a>(&self, arena: &'a CompileArena) -> AResult<NativeOwned<'a, Self>> {
        match self {
            Self::Enum { table, value } => fields!(arena, Self::Enum { table, value }),
            Self::Predefined { table, name } => fields!(arena, Self::Predefined { table, name }),
            Self::BusinessProcessPoint { table, name } => {
                fields!(arena, Self::BusinessProcessPoint { table, name })
            }
            Self::EmptyRef(value) => Ok(value.clone_owned(arena)?.map(Self::EmptyRef)),
            Self::TypeRef(value) => Ok(value.clone_owned(arena)?.map(Self::TypeRef)),
            Self::SysEnum(ty, value) => Ok(NativeOwned::leaf(
                Self::SysEnum(*ty, *value),
                Some(arena),
                0,
            )),
            Self::Unknown => Ok(NativeOwned::leaf(Self::Unknown, Some(arena), 0)),
        }
    }
}
impl ManagedClone for crate::tir::TKind {
    fn graph_depth(&self, remaining: u16) -> AResult<u16> {
        use crate::tir::TKind::*;
        match self {
            Const(_) | Value(_) | Param(_) | Field { .. } | Unit(_) => Ok(0),
            Neg(e) | Not(e) | IsNull { l: e, .. } | Refs { l: e, .. } | Cast { e, .. } => {
                e.graph_depth(remaining)
            }
            Arith { first, steps } => Ok(first
                .graph_depth(remaining)?
                .max(steps.graph_depth(remaining)?)),
            And(xs) | Or(xs) | Func { args: xs, .. } => xs.graph_depth(remaining),
            Compare(_, a, b) => Ok(a.graph_depth(remaining)?.max(b.graph_depth(remaining)?)),
            InList { l, rhs, .. } => Ok(l.graph_depth(remaining)?.max(rhs.graph_depth(remaining)?)),
            InQuery { l, q, .. } => Ok(l
                .graph_depth(remaining)?
                .max(q.graph_depth(remaining.checked_sub(1).ok_or(BudgetError::Overflow)?)? + 1)),
            Between { l, lo, hi, .. } => Ok(l
                .graph_depth(remaining)?
                .max(lo.graph_depth(remaining)?)
                .max(hi.graph_depth(remaining)?)),
            Like { l, pattern, .. } => Ok(l
                .graph_depth(remaining)?
                .max(pattern.graph_depth(remaining)?)),
            Case {
                operand,
                whens,
                else_,
            } => Ok(operand
                .graph_depth(remaining)?
                .max(whens.graph_depth(remaining)?)
                .max(else_.graph_depth(remaining)?)),
            Agg { arg, .. } => arg.graph_depth(remaining),
        }
    }
    fn clone_owned<'a>(&self, arena: &'a CompileArena) -> AResult<NativeOwned<'a, Self>> {
        match self {
            Self::Const(value) => Ok(value.clone_owned(arena)?.map(Self::Const)),
            Self::Value(value) => Ok(value.clone_owned(arena)?.map(Self::Value)),
            Self::Param(value) => Ok(value.clone_owned(arena)?.map(Self::Param)),
            Self::Neg(value) => Ok(value.clone_owned(arena)?.map(Self::Neg)),
            Self::Not(value) => Ok(value.clone_owned(arena)?.map(Self::Not)),
            Self::And(value) => Ok(value.clone_owned(arena)?.map(Self::And)),
            Self::Or(value) => Ok(value.clone_owned(arena)?.map(Self::Or)),
            Self::Unit(value) => Ok(value.clone_owned(arena)?.map(Self::Unit)),
            Self::Field {
                level,
                source,
                col,
                derefs,
            } => fields!(
                arena,
                Self::Field {
                    level,
                    source,
                    col,
                    derefs
                }
            ),
            Self::Arith { first, steps } => fields!(arena, Self::Arith { first, steps }),
            Self::InList {
                l,
                not,
                hierarchy,
                rhs,
            } => fields!(
                arena,
                Self::InList {
                    l,
                    not,
                    hierarchy,
                    rhs
                }
            ),
            Self::InQuery {
                l,
                not,
                hierarchy,
                q,
            } => fields!(
                arena,
                Self::InQuery {
                    l,
                    not,
                    hierarchy,
                    q
                }
            ),
            Self::Between { l, not, lo, hi } => fields!(arena, Self::Between { l, not, lo, hi }),
            Self::IsNull { l, not } => fields!(arena, Self::IsNull { l, not }),
            Self::Refs { l, table } => fields!(arena, Self::Refs { l, table }),
            Self::Like {
                l,
                not,
                pattern,
                escape,
            } => fields!(
                arena,
                Self::Like {
                    l,
                    not,
                    pattern,
                    escape
                }
            ),
            Self::Case {
                operand,
                whens,
                else_,
            } => fields!(
                arena,
                Self::Case {
                    operand,
                    whens,
                    else_
                }
            ),
            Self::Cast { e, to, derefs, num } => fields!(arena, Self::Cast { e, to, derefs, num }),
            Self::Func { f, args } => fields!(arena, Self::Func { f, args }),
            Self::Agg {
                f,
                distinct,
                arg,
                nested,
            } => fields!(
                arena,
                Self::Agg {
                    f,
                    distinct,
                    arg,
                    nested
                }
            ),
            Self::Compare(op, left, right) => Ok(left
                .clone_owned(arena)?
                .zip(right.clone_owned(arena)?)?
                .map(|(left, right)| Self::Compare(*op, left, right))),
        }
    }
}
impl ManagedClone for crate::sema::ColumnInfo {
    fn clone_owned<'a>(&self, arena: &'a CompileArena) -> AResult<NativeOwned<'a, Self>> {
        struct Depth<'a>(&'a std::cell::Cell<usize>);
        impl Drop for Depth<'_> {
            fn drop(&mut self) {
                self.0.set(self.0.get() - 1);
            }
        }
        let depth = arena.column_depth.get();
        if depth >= 128 {
            return Err(BudgetError::Overflow);
        }
        arena.column_depth.set(depth + 1);
        let _depth = Depth(&arena.column_depth);
        let (name, ty, width, nested) = (&self.name, &self.ty, &self.width, &self.nested);
        fields!(
            arena,
            Self {
                name,
                ty,
                width,
                nested
            }
        )
    }
}
copy_clone!([u8; 16]);
impl ManagedClone for crate::value::QValue {
    fn clone_owned<'a>(&self, arena: &'a CompileArena) -> AResult<NativeOwned<'a, Self>> {
        match self {
            Self::Null => Ok(NativeOwned::leaf(Self::Null, Some(arena), 0)),
            Self::Undefined => Ok(NativeOwned::leaf(Self::Undefined, Some(arena), 0)),
            Self::Bool(value) => Ok(value.clone_owned(arena)?.map(Self::Bool)),
            Self::Date(value) => Ok(value.clone_owned(arena)?.map(Self::Date)),
            Self::Uuid(value) => Ok(value.clone_owned(arena)?.map(Self::Uuid)),
            Self::Type(value) => Ok(value.clone_owned(arena)?.map(Self::Type)),
            Self::Nested(value) => Ok(value.clone_owned(arena)?.map(Self::Nested)),
            Self::PointInTime(value) => Ok(NativeOwned::leaf(
                Self::PointInTime(value.clone()),
                Some(arena),
                0,
            )),
            Self::Boundary(value) => Ok(NativeOwned::leaf(
                Self::Boundary(value.clone()),
                Some(arena),
                0,
            )),
            Self::Ref { table, id } => fields!(arena, Self::Ref { table, id }),
            Self::SysEnum(ty, value) => Ok(NativeOwned::leaf(
                Self::SysEnum(*ty, *value),
                Some(arena),
                0,
            )),
            Self::String(value) => Ok(NativeOwned::leaf(
                Self::String(value.try_clone_budgeted(&arena.budget)?),
                Some(arena),
                0,
            )),
            Self::Binary(value) => Ok(NativeOwned::leaf(
                Self::Binary(value.try_clone_budgeted(&arena.budget)?),
                Some(arena),
                0,
            )),
            Self::Storage(value) => Ok(NativeOwned::leaf(
                Self::Storage(value.try_clone_budgeted(&arena.budget)?),
                Some(arena),
                0,
            )),
            Self::Number(value) => {
                let value = super::numeric::clone_number(arena, value)?;
                let bytes = (value.backing_capacity_limbs() as u64)
                    .checked_mul(4)
                    .ok_or(BudgetError::Overflow)?;
                Ok(NativeOwned::leaf(Self::Number(value), Some(arena), bytes))
            }
        }
    }
}
