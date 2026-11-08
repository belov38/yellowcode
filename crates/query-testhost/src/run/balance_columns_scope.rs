use query_ir::tir::{self, Col, From, Query, Select, Source, TExpr, TKind};
use query_ir::{
    CompiledStmt, FieldRole, QFieldId, QTableId, QuerySchema, StdRole, TableKind, VtKind,
};

type Lookup<'a> = dyn Fn(u16, u16) -> Option<QTableId> + 'a;

fn field(schema: &QuerySchema, table: QTableId, id: QFieldId) -> bool {
    let table_info = schema.table(table);
    table_info.kind == TableKind::Virtual(VtKind::Balance)
        && table_info.owner.is_some_and(|owner| {
            schema.table(owner).kind == TableKind::Object(md_model::MdKind::AccountingRegister)
        })
        && schema
            .field(table, id)
            .is_some_and(|field| field.role == FieldRole::Std(StdRole::ExtDimension(1)))
}

fn source_at(sources: &[Source], wanted: u16) -> Option<&Source> {
    fn visit<'a>(source: &'a Source, next: &mut usize, wanted: usize) -> Option<&'a Source> {
        if *next == wanted {
            return Some(source);
        }
        *next += 1;
        for join in &source.joins {
            if let Some(found) = visit(&join.right, next, wanted) {
                return Some(found);
            }
        }
        None
    }
    let mut next = 0;
    sources
        .iter()
        .find_map(|source| visit(source, &mut next, usize::from(wanted)))
}

fn expr(value: &TExpr, lookup: &Lookup<'_>, schema: &QuerySchema) -> bool {
    match &value.kind {
        TKind::Field {
            level,
            source,
            col,
            derefs,
        } => {
            matches!(col, Col::Field(id) if lookup(*level, *source).is_some_and(|table| field(schema, table, *id)))
                || derefs
                    .iter()
                    .flat_map(|step| &step.targets)
                    .any(|(table, id)| field(schema, *table, *id))
        }
        TKind::Neg(value) | TKind::Not(value) => expr(value, lookup, schema),
        TKind::Arith { first, steps } => {
            expr(first, lookup, schema) || steps.iter().any(|step| expr(&step.rhs, lookup, schema))
        }
        TKind::And(values) | TKind::Or(values) => {
            values.iter().any(|value| expr(value, lookup, schema))
        }
        TKind::Compare(_, left, right) => expr(left, lookup, schema) || expr(right, lookup, schema),
        TKind::InList { l, rhs, .. } => {
            l.iter().chain(rhs).any(|value| expr(value, lookup, schema))
        }
        TKind::InQuery { l, q, .. } => {
            l.iter().any(|value| expr(value, lookup, schema)) || query(q, Some(lookup), schema)
        }
        TKind::Between { l, lo, hi, .. } => {
            expr(l, lookup, schema) || expr(lo, lookup, schema) || expr(hi, lookup, schema)
        }
        TKind::IsNull { l, .. } | TKind::Refs { l, .. } => expr(l, lookup, schema),
        TKind::Like { l, pattern, .. } => expr(l, lookup, schema) || expr(pattern, lookup, schema),
        TKind::Case {
            operand,
            whens,
            else_,
        } => {
            operand
                .iter()
                .chain(else_)
                .any(|value| expr(value, lookup, schema))
                || whens
                    .iter()
                    .any(|(when, then)| expr(when, lookup, schema) || expr(then, lookup, schema))
        }
        TKind::Cast { e, derefs, .. } => {
            expr(e, lookup, schema)
                || derefs
                    .iter()
                    .flat_map(|step| &step.targets)
                    .any(|(table, id)| field(schema, *table, *id))
        }
        TKind::Func { args, .. } => args.iter().any(|value| expr(value, lookup, schema)),
        TKind::Agg { arg, .. } => arg.as_deref().is_some_and(|arg| expr(arg, lookup, schema)),
        TKind::Const(_) | TKind::Value(_) | TKind::Param(_) | TKind::Unit(_) => false,
    }
}

fn dependency(value: &tir::DrCrArgDependency, owner: QTableId, schema: &QuerySchema) -> bool {
    let table = match value.scope {
        tir::DrCrArgScope::Nested(table) => Some(table),
        tir::DrCrArgScope::Condition => Some(owner),
        tir::DrCrArgScope::NoSources | tir::DrCrArgScope::Account(_) => None,
    };
    let lookup = |level, source| {
        if level == 0 && source == 0 {
            table
        } else {
            None
        }
    };
    match &value.read {
        tir::DrCrArgRead::Expression(value) => expr(value, &lookup, schema),
        tir::DrCrArgRead::Wildcard(value) => {
            expr(&value.base, &lookup, schema)
                || value.targets.iter().any(|target| {
                    target
                        .fields
                        .iter()
                        .any(|id| field(schema, target.table, *id))
                        || (target.all_fields
                            && schema.table(target.table).fields.iter().enumerate().any(
                                |(index, _)| {
                                    u16::try_from(index).ok().is_some_and(|index| {
                                        field(schema, target.table, QFieldId(index))
                                    })
                                },
                            ))
                        || target
                            .path
                            .iter()
                            .flat_map(|step| &step.targets)
                            .any(|(table, id)| field(schema, *table, *id))
                })
        }
        tir::DrCrArgRead::Nested(value) => {
            expr(&value.base, &lookup, schema)
                || value
                    .dependencies
                    .iter()
                    .any(|value| dependency(value, owner, schema))
        }
    }
}

fn source(value: &Source, lookup: &Lookup<'_>, schema: &QuerySchema) -> bool {
    let used = match &value.from {
        From::Subquery(query_value) => query(query_value, None, schema),
        From::Table {
            id,
            args,
            accounting_dependencies,
            drcr,
            ..
        } => {
            let owner = schema.table(*id).owner.unwrap_or(*id);
            let arguments = |level, source| {
                if level == 0 && source == 0 {
                    Some(owner)
                } else {
                    None
                }
            };
            args.iter()
                .flatten()
                .any(|value| expr(value, &arguments, schema))
                || accounting_dependencies.as_ref().is_some_and(|capture| {
                    capture
                        .dependencies
                        .iter()
                        .any(|value| dependency(value, owner, schema))
                })
                || drcr.as_ref().is_some_and(|capture| {
                    capture
                        .dependencies
                        .iter()
                        .any(|value| dependency(value, owner, schema))
                })
        }
        From::Temp(_) | From::Param { .. } => false,
    };
    used || value
        .joins
        .iter()
        .any(|join| source(&join.right, lookup, schema) || expr(&join.on, lookup, schema))
}

fn item(value: &tir::Item, lookup: &Lookup<'_>, schema: &QuerySchema) -> bool {
    if expr(&value.expr, lookup, schema) {
        return true;
    }
    let nested = |level: u16, source| {
        if level == 0 {
            None
        } else {
            lookup(level - 1, source)
        }
    };
    value
        .nested
        .iter()
        .flatten()
        .any(|value| item(value, &nested, schema))
}

fn select(value: &Select, lookup: &Lookup<'_>, schema: &QuerySchema) -> bool {
    value
        .sources
        .iter()
        .any(|value| source(value, lookup, schema))
        || value.items.iter().any(|value| item(value, lookup, schema))
        || value
            .where_
            .iter()
            .chain(&value.having)
            .any(|value| expr(value, lookup, schema))
        || value
            .group
            .iter()
            .flatten()
            .flatten()
            .any(|value| expr(value, lookup, schema))
}

fn query(value: &Query, outer: Option<&Lookup<'_>>, schema: &QuerySchema) -> bool {
    for part in &value.parts {
        let lookup = |level: u16, index| {
            if level == 0 {
                source_at(&part.sources, index).and_then(|source| match source.from {
                    From::Table { id, .. } => Some(id),
                    _ => None,
                })
            } else {
                outer.and_then(|lookup| lookup(level - 1, index))
            }
        };
        if select(part, &lookup, schema) {
            return true;
        }
    }
    let Some(first) = value.parts.first() else {
        return false;
    };
    let lookup = |level: u16, index| {
        if level == 0 {
            source_at(&first.sources, index).and_then(|source| match source.from {
                From::Table { id, .. } => Some(id),
                _ => None,
            })
        } else {
            outer.and_then(|lookup| lookup(level - 1, index))
        }
    };
    value
        .order
        .iter()
        .any(|key| expr(&key.expr, &lookup, schema))
        || value.totals.as_ref().is_some_and(|totals| {
            totals
                .items
                .iter()
                .any(|(value, _)| expr(value, &lookup, schema))
                || totals.points.iter().any(|point| match point {
                    tir::ControlPoint::Overall => false,
                    tir::ControlPoint::Field(field) => {
                        expr(&field.expr, &lookup, schema)
                            || field.periods.as_ref().is_some_and(|periods| {
                                periods
                                    .begin
                                    .iter()
                                    .chain(&periods.end)
                                    .any(|value| expr(value, &lookup, schema))
                            })
                    }
                })
        })
}

pub(super) fn used(statement: &CompiledStmt, schema: &QuerySchema) -> bool {
    let tir::StmtKind::Query(value) = &statement.typed.kind else {
        return false;
    };
    query_syntax::with_deep_stack(statement.typed.max_depth, || query(value, None, schema))
}
