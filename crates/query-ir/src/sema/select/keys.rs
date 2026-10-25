use crate::managed::{CompileArena, NameKey};
use crate::sema::R;
use crate::tir::{self, TExpr, TKind};
use crate::{NestedResult, QValue};
use std::fmt::{self, Write};

pub(super) fn key_name(memory: Option<&CompileArena>, expression: &TExpr) -> R<NameKey> {
    NameKey::formatted(memory, |out| expression_key(memory, expression, out))
}
pub(super) fn field_key_name(
    memory: Option<&CompileArena>,
    level: u16,
    source: u16,
    col: tir::Col,
    derefs: &[tir::Deref],
) -> R<NameKey> {
    NameKey::formatted(memory, |out| {
        field_key(memory, out, level, source, col, derefs)
    })
}
fn folded(memory: Option<&CompileArena>, name: &str) -> Result<NameKey, fmt::Error> {
    NameKey::folded(memory, name).map_err(|_| fmt::Error)
}
fn field_key(
    memory: Option<&CompileArena>,
    out: &mut dyn Write,
    level: u16,
    source: u16,
    col: tir::Col,
    derefs: &[tir::Deref],
) -> fmt::Result {
    write!(out, "f{level}.{source}.{col:?}")?;
    for deref in derefs {
        write!(out, ".{}", folded(memory, &deref.name)?.as_str())?;
    }
    Ok(())
}
fn node<'a>(
    memory: Option<&CompileArena>,
    out: &mut dyn Write,
    tag: fmt::Arguments<'_>,
    children: impl IntoIterator<Item = &'a TExpr>,
) -> fmt::Result {
    write!(out, "({tag}")?;
    for child in children {
        out.write_char(' ')?;
        expression_key(memory, child, out)?;
    }
    out.write_char(')')
}
fn expression_key(
    memory: Option<&CompileArena>,
    expression: &TExpr,
    out: &mut dyn Write,
) -> fmt::Result {
    match &expression.kind {
        TKind::Const(value) => write!(out, "c{:?}", ValueDebug(value)),
        TKind::Value(value) => write!(out, "v{value:?}"),
        TKind::Param(name) => write!(out, "p{}", folded(memory, name)?.as_str()),
        TKind::Field {
            level,
            source,
            col,
            derefs,
        } => field_key(memory, out, *level, *source, *col, derefs),
        TKind::Unit(unit) => write!(out, "u{unit:?}"),
        TKind::Neg(value) => node(memory, out, format_args!("neg"), [&**value]),
        TKind::Not(value) => node(memory, out, format_args!("not"), [&**value]),
        TKind::Arith { first, steps } => {
            out.write_str("(arith ")?;
            expression_key(memory, first, out)?;
            for step in steps {
                write!(out, " {:?} ", step.op)?;
                expression_key(memory, &step.rhs, out)?;
            }
            out.write_char(')')
        }
        TKind::And(values) => node(memory, out, format_args!("and"), values),
        TKind::Or(values) => node(memory, out, format_args!("or"), values),
        TKind::Compare(op, left, right) => {
            node(memory, out, format_args!("cmp{op:?}"), [&**left, &**right])
        }
        TKind::InList {
            l,
            not,
            hierarchy,
            rhs,
        } => node(
            memory,
            out,
            format_args!("in{not}{hierarchy}{}", l.len()),
            l.iter().chain(rhs),
        ),
        TKind::InQuery { .. } => write!(out, "(subquery@{})", expression.span.start),
        TKind::Between { l, not, lo, hi } => {
            node(memory, out, format_args!("btw{not}"), [&**l, &**lo, &**hi])
        }
        TKind::IsNull { l, not } => node(memory, out, format_args!("isnull{not}"), [&**l]),
        TKind::Refs { l, table } => node(memory, out, format_args!("refs{}", table.0), [&**l]),
        TKind::Like {
            l,
            not,
            pattern,
            escape,
        } => node(
            memory,
            out,
            format_args!("like{not}{escape:?}"),
            [&**l, &**pattern],
        ),
        TKind::Case {
            operand,
            whens,
            else_,
        } => {
            out.write_str("(case")?;
            if let Some(operand) = operand {
                out.write_str(" op ")?;
                expression_key(memory, operand, out)?;
            }
            for (condition, result) in whens {
                out.write_str(" when ")?;
                expression_key(memory, condition, out)?;
                out.write_str(" then ")?;
                expression_key(memory, result, out)?;
            }
            if let Some(otherwise) = else_ {
                out.write_str(" else ")?;
                expression_key(memory, otherwise, out)?;
            }
            out.write_char(')')
        }
        TKind::Cast { e, to, derefs, num } => {
            write!(out, "(cast{to:?}{num:?}[")?;
            for (index, deref) in derefs.iter().enumerate() {
                if index != 0 {
                    out.write_str(", ")?;
                }
                write!(out, "{:?}", folded(memory, &deref.name)?.as_str())?;
            }
            out.write_str("] ")?;
            expression_key(memory, e, out)?;
            out.write_char(')')
        }
        TKind::Func { f, args } => node(memory, out, format_args!("fn{f:?}"), args),
        TKind::Agg {
            f, distinct, arg, ..
        } => node(
            memory,
            out,
            format_args!("agg{f:?}{distinct}"),
            arg.iter().map(|value| &**value),
        ),
    }
}

struct NumberDebug<'a>(&'a bsl_prim::Number);
impl fmt::Debug for NumberDebug<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.write_xml(f)
    }
}
struct ValueDebug<'a>(&'a QValue);
impl fmt::Debug for ValueDebug<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            QValue::Number(value) => f.debug_tuple("Number").field(&NumberDebug(value)).finish(),
            QValue::Nested(value) => f.debug_tuple("Nested").field(&NestedDebug(value)).finish(),
            value @ (QValue::Null
            | QValue::Undefined
            | QValue::Bool(_)
            | QValue::String(_)
            | QValue::Date(_)
            | QValue::Uuid(_)
            | QValue::Ref { .. }
            | QValue::Type(_)
            | QValue::Binary(_)
            | QValue::Storage(_)
            | QValue::SysEnum(..)
            | QValue::PointInTime(_)
            | QValue::Boundary(_)) => fmt::Debug::fmt(value, f),
        }
    }
}
struct NestedDebug<'a>(&'a NestedResult);
impl fmt::Debug for NestedDebug<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        struct Rows<'a>(&'a [Vec<QValue>]);
        impl fmt::Debug for Rows<'_> {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                struct Row<'a>(&'a [QValue]);
                impl fmt::Debug for Row<'_> {
                    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                        f.debug_list()
                            .entries(self.0.iter().map(ValueDebug))
                            .finish()
                    }
                }
                f.debug_list()
                    .entries(self.0.iter().map(|row| Row(row)))
                    .finish()
            }
        }
        f.debug_struct("NestedResult")
            .field("columns", &self.0.columns)
            .field("rows", &Rows(&self.0.rows))
            .finish()
    }
}
