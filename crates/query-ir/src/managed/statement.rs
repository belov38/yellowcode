use super::{self as mem, CompileArena, NativeOwned, NativeVec, OwnedString, ScratchVec};
use crate::sema::{R, select};
use crate::{ColumnInfo, ParamInfo, QShared, QType, TempColumn, TempSchema, tir};
use memory_budget::BudgetError;

pub(crate) enum TempOutput {
    Put(QShared<TempSchema>),
    Add(String),
}
pub(crate) struct Statement {
    pub(crate) columns: Vec<ColumnInfo>,
    pub(crate) temp_out: Option<TempOutput>,
    pub(crate) drop: Option<String>,
    pub(crate) reads: Vec<String>,
    pub(crate) params: Vec<ParamInfo>,
    pub(crate) value_deps: Vec<String>,
    pub(crate) table_params: Vec<String>,
    pub(crate) typed: tir::Stmt,
}
pub(crate) struct Parts<'a> {
    pub(crate) columns: NativeOwned<'a, Vec<ColumnInfo>>,
    pub(crate) temp_out: NativeOwned<'a, Option<TempOutput>>,
    pub(crate) drop: NativeOwned<'a, Option<String>>,
    pub(crate) reads: NativeOwned<'a, Vec<String>>,
    pub(crate) params: NativeOwned<'a, Vec<ParamInfo>>,
    pub(crate) value_deps: NativeOwned<'a, Vec<String>>,
    pub(crate) table_params: NativeOwned<'a, Vec<String>>,
    pub(crate) typed: NativeOwned<'a, tir::Stmt>,
}
pub(crate) fn assemble<'a>(
    scope: Option<&'a CompileArena>,
    parts: Parts<'a>,
) -> R<NativeOwned<'a, Statement>> {
    let value = parts
        .columns
        .zip(parts.temp_out)
        .and_then(|v| v.zip(parts.drop))
        .and_then(|v| v.zip(parts.reads))
        .and_then(|v| v.zip(parts.params))
        .and_then(|v| v.zip(parts.value_deps))
        .and_then(|v| v.zip(parts.table_params))
        .and_then(|v| v.zip(parts.typed));
    Ok(mem::allocation_result(scope, value)?.map(
        |(((((((columns, temp_out), drop), reads), params), value_deps), table_params), typed)| {
            Statement {
                columns,
                temp_out,
                drop,
                reads,
                params,
                value_deps,
                table_params,
                typed,
            }
        },
    ))
}
pub(crate) fn names<'a>(
    scope: Option<&'a CompileArena>,
    values: &[mem::NameKey],
) -> R<NativeOwned<'a, Vec<String>>> {
    let mut out = NativeVec::new(scope, values.len())?;
    for value in values {
        out.push_owned(OwnedString::new(scope, value)?.into_native_owned())?;
    }
    Ok(out.finish())
}
fn column<'a>(
    scope: Option<&'a CompileArena>,
    name: &str,
    ty: &QType,
    nested: Option<&[tir::Item]>,
    depth: usize,
    nullable: bool,
) -> R<NativeOwned<'a, ColumnInfo>> {
    if depth >= 128 {
        return mem::allocation_error(scope, BudgetError::Overflow);
    }
    let name = OwnedString::new(scope, name)?.into_native_owned();
    let ty = mem::owned_copy(scope, ty)?.map(|ty| {
        if nullable
            && !ty.is_undefined()
            && !(ty.atom_count() == 1 && !ty.null && ty.others == [crate::Other::QueryResult])
        {
            ty.with_null()
        } else {
            ty
        }
    });
    let nested = if let Some(items) = nested {
        let mut out = NativeVec::new(scope, items.len())?;
        for item in items {
            out.push_owned(column(
                scope,
                &item.name,
                &item.expr.ty,
                item.nested.as_deref(),
                depth + 1,
                true,
            )?)?;
        }
        out.finish().map(Some)
    } else {
        NativeOwned::none(scope)
    };
    let value = name.zip(ty).and_then(|v| v.zip(nested));
    Ok(
        mem::allocation_result(scope, value)?.map(|((name, ty), nested)| ColumnInfo {
            name,
            width: ty.width(),
            ty,
            nested,
        }),
    )
}
pub(crate) fn columns<'a>(
    scope: Option<&'a CompileArena>,
    query: &tir::Query,
    inter: bool,
    stored: Option<&TempSchema>,
) -> R<NativeOwned<'a, Vec<ColumnInfo>>> {
    let types = select::column_types_scoped(scope, &query.parts, query.totals.as_ref())?;
    let items = query.parts.first().map_or(&[][..], |p| p.items.as_slice());
    let mut out = NativeVec::new(scope, items.len())?;
    for (i, (item, ty)) in items.iter().zip(types.iter()).enumerate() {
        let stored_column = if inter {
            stored.and_then(|t| t.columns.get(i))
        } else {
            None
        };
        let selected = stored_column.and_then(|c| c.ty.as_ref());
        let stored_null = selected.is_some_and(|t| *t == crate::sema::columns::null_column_type());
        let ty = mem::owned_copy(scope, selected.unwrap_or(ty))?.map(|ty| {
            if inter && selected.is_none() {
                ty.without_null()
            } else if stored_null {
                ty.with_null()
            } else {
                ty
            }
        });
        let name = stored_column.map_or(item.name.as_str(), |c| c.name.as_str());
        out.push_owned(column(
            scope,
            name,
            ty.trusted_borrow(),
            item.nested.as_deref(),
            0,
            !inter,
        )?)?;
    }
    Ok(out.finish())
}
pub(crate) fn count_columns<'a>(
    scope: Option<&'a CompileArena>,
    script: query_syntax::ScriptVariant,
) -> R<NativeOwned<'a, Vec<ColumnInfo>>> {
    let name = match script {
        query_syntax::ScriptVariant::Russian => "Количество",
        query_syntax::ScriptVariant::English => "Count",
    };
    let mut out = NativeVec::new(scope, 1)?;
    out.push_owned(
        column(
            scope,
            name,
            &QType::num(crate::NumQ {
                digits: 0,
                scale: 0,
                nonneg: false,
            }),
            None,
            0,
            false,
        )?
        .map(|mut value| {
            value.width = 10;
            value
        }),
    )?;
    Ok(out.finish())
}
pub(crate) fn temp_schema<'a>(
    scope: Option<&'a CompileArena>,
    name: NativeOwned<'a, String>,
    query: &tir::Query,
    open: bool,
) -> R<NativeOwned<'a, TempSchema>> {
    let types = select::column_types_scoped(scope, &query.parts, query.totals.as_ref())?;
    let items = query.parts.first().map_or(&[][..], |p| p.items.as_slice());
    let mut out = NativeVec::new(scope, items.len())?;
    for (i, (item, ty)) in items.iter().zip(types.iter()).enumerate() {
        let unknown = query
            .parts
            .iter()
            .any(|part| part.items.get(i).is_some_and(|v| v.unknown));
        let declared = |x: &tir::Item| match &x.expr.kind {
            tir::TKind::Cast {
                num: Some(q),
                derefs,
                ..
            } if derefs.is_empty() && q.digits > 0 => Some((q.digits, q.scale)),
            _ => None,
        };
        let stored = declared(item)
            .filter(|d| {
                query
                    .parts
                    .iter()
                    .all(|part| part.items.get(i).and_then(declared) == Some(*d))
            })
            .map(|(digits, scale)| crate::NumQ {
                digits,
                scale,
                nonneg: false,
            });
        let ty = if unknown {
            NativeOwned::none(scope)
        } else {
            let mut ty = mem::owned_copy(scope, &**ty)?.map(QType::without_null);
            if types.get(i).is_some_and(|v| v.null) && ty.trusted_borrow().is_undefined() {
                let fallback = QType::boolean()
                    .merge(&QType::str_(crate::StrQ {
                        len: 1,
                        fixed: false,
                    }))
                    .merge(&QType::date(crate::DateQ::DateTime))
                    .merge(&QType::num(crate::NumQ {
                        digits: 1,
                        scale: 0,
                        nonneg: true,
                    }));
                ty = mem::owned_copy(scope, &fallback)?;
            }
            ty.map(Some)
        };
        let name = OwnedString::new(scope, &item.name)?.into_native_owned();
        out.push_owned(
            mem::allocation_result(scope, name.zip(ty))?.map(|(name, ty)| TempColumn {
                name,
                ty,
                unknown,
                stored,
            }),
        )?;
    }
    Ok(
        mem::allocation_result(scope, name.zip(out.finish()))?.map(|(name, columns)| TempSchema {
            name,
            columns,
            open,
        }),
    )
}
pub(crate) fn put<'a>(
    scope: Option<&'a CompileArena>,
    schema: NativeOwned<'a, TempSchema>,
) -> R<NativeOwned<'a, Option<TempOutput>>> {
    let shared = mem::share_owned(scope, schema)?;
    Ok(NativeOwned::leaf(Some(TempOutput::Put(shared)), scope, 0))
}
pub(crate) fn add<'a>(name: NativeOwned<'a, String>) -> NativeOwned<'a, Option<TempOutput>> {
    name.map(|v| Some(TempOutput::Add(v)))
}
pub(crate) fn typed_query<'a>(
    query: NativeOwned<'a, tir::Query>,
    static_analysis: bool,
) -> NativeOwned<'a, tir::Stmt> {
    query.map(|query| tir::Stmt {
        kind: tir::StmtKind::Query(query),
        static_analysis,
        max_depth: 0,
    })
}
pub(crate) fn typed_drop<'a>(
    name: NativeOwned<'a, String>,
    static_analysis: bool,
) -> NativeOwned<'a, tir::Stmt> {
    name.map(|name| tir::Stmt {
        kind: tir::StmtKind::Drop(name),
        static_analysis,
        max_depth: 0,
    })
}
pub(crate) fn params<'a>(
    scope: Option<&'a CompileArena>,
    uses: &[crate::sema::ParamUse<'a>],
) -> R<NativeOwned<'a, Vec<ParamInfo>>> {
    let mut values: ScratchVec<(&str, mem::ScopeType)> = ScratchVec::new(scope, 0)?;
    let mut index: mem::Index<mem::NameKey, usize> = mem::Index::new();
    for u in uses {
        let key = mem::NameKey::folded(scope, &u.name)?;
        if let Some(&i) = index.get(key.as_str()) {
            if let Some((_, ty)) = values.get_mut(i) {
                *ty = ty.merge(scope, &u.ty)?;
            }
        } else {
            index.insert(scope, key, values.len())?;
            values.push(scope, (&u.name, mem::ScopeType::copy(scope, &u.ty)?))?;
        }
    }
    let mut out = NativeVec::new(scope, values.len())?;
    for (name, ty) in &values {
        let name = OwnedString::new(scope, name)?.into_native_owned();
        let ty = mem::owned_copy(scope, &**ty)?;
        out.push_owned(
            mem::allocation_result(scope, name.zip(ty))?.map(|(name, ty)| ParamInfo { name, ty }),
        )?;
    }
    Ok(out.finish())
}
