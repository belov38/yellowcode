use super::{Builder, R, autoorder, internal, later, out};
use crate::QuerySchema;
use crate::ir::{ColSel, Expr, ExprKind, Rel, SortKey, SourceS, SrcId, SrcKind};
use crate::schema::{FieldRole, QFieldId, QTableId, StdRole, TableKind};
use crate::tir::{self, Col, From, TExpr, TKind};
use crate::types::{Other, QType};
use crate::value::QValue;
use mdschema::MdKind;
use query_syntax::ast::OrderDir;

pub(crate) fn union_autoorder_col_orderable(ty: &QType) -> bool {
    !ty.has_unlimited_string()
        && !ty
            .others
            .iter()
            .any(|o| matches!(o, Other::Binary | Other::Storage | Other::QueryResult))
}

#[derive(Clone, Copy)]
pub(crate) struct HierarchyUnion {
    pub(crate) column: usize,
    pub(crate) visible: usize,
    pub(crate) table: QTableId,
    pub(crate) desc: bool,
}

fn has_aggregate(expr: &TExpr) -> bool {
    if matches!(expr.kind, TKind::Agg { .. }) {
        return true;
    }
    let mut found = false;
    crate::sema::select::each_child(expr, &mut |child| {
        found = found || has_aggregate(child);
    });
    found
}

pub(crate) fn hierarchy_union_shape(
    query: &tir::Query,
    schema: &QuerySchema,
) -> Option<HierarchyUnion> {
    if query.parts.len() < 2
        || query.unions.len() != query.parts.len() - 1
        || query.unions.iter().any(|all| !all)
        || query.autoorder
        || query.totals.is_some()
    {
        return None;
    }
    let [order] = query.order.as_slice() else {
        return None;
    };
    let desc = match order.dir {
        OrderDir::Hierarchy => false,
        OrderDir::HierarchyDesc => true,
        _ => return None,
    };
    let column = order.col?;
    let first = query.parts.first()?;
    let visible = first.items.len();
    let [atom] = first.items.get(column)?.expr.ty.refs.as_slice() else {
        return None;
    };
    let table = schema.ref_info(*atom).table;
    if schema.table(table).kind != TableKind::Object(MdKind::Catalog)
        || schema.hierarchical_table(*atom) != Some(table)
    {
        return None;
    }
    for arm in &query.parts {
        let [source] = arm.sources.as_slice() else {
            return None;
        };
        if arm.distinct
            || arm.group.is_some()
            || arm.having.is_some()
            || arm.items.len() != visible
            || !source.joins.is_empty()
            || arm
                .items
                .iter()
                .any(|item| item.nested.is_some() || item.unknown || has_aggregate(&item.expr))
        {
            return None;
        }
        let From::Table { id, .. } = &source.from else {
            return None;
        };
        if *id != table {
            return None;
        }
        let key = &arm.items.get(column)?.expr;
        let TKind::Field {
            level: 0,
            source: 0,
            col: Col::Field(field),
            derefs,
        } = &key.kind
        else {
            return None;
        };
        if !derefs.is_empty()
            || key.ty.atom_count() != 1
            || key.ty.refs.as_slice() != [*atom]
            || schema.field(table, *field)?.role != FieldRole::Std(StdRole::Ref)
            || !schema
                .table(table)
                .fields
                .iter()
                .any(|field| field.role == FieldRole::Std(StdRole::Parent))
        {
            return None;
        }
    }
    Some(HierarchyUnion {
        column,
        visible,
        table,
        desc,
    })
}

impl HierarchyUnion {
    pub(crate) fn source(self, schema: &QuerySchema, expr: &Expr, kind: &SrcKind) -> Option<SrcId> {
        let ExprKind::Col {
            src,
            col: ColSel::Field(field),
        } = expr.kind
        else {
            return None;
        };
        let SrcKind::Table(table) = kind else {
            return None;
        };
        let [atom] = expr.ty.refs.as_slice() else {
            return None;
        };
        (*table == self.table
            && schema.table(*table).kind == TableKind::Object(MdKind::Catalog)
            && expr.ty.atom_count() == 1
            && schema.ref_info(*atom).table == *table
            && schema.hierarchical_table(*atom) == Some(*table)
            && schema.field(*table, field)?.role == FieldRole::Std(StdRole::Ref))
        .then_some(src)
    }

    pub(crate) fn slots(self) -> Option<(u16, u16)> {
        Some((
            u16::try_from(self.visible).ok()?,
            u16::try_from(self.visible.checked_add(1)?).ok()?,
        ))
    }
}

fn projection(part: &Rel) -> Option<&[Expr]> {
    match part {
        Rel::Project { exprs, .. } => Some(exprs),
        Rel::Top { input, .. } => match input.as_ref() {
            Rel::Project { exprs, .. } => Some(exprs),
            _ => None,
        },
        _ => None,
    }
}

impl Builder<'_> {
    pub(super) fn hierarchy_union(
        &self,
        query: &tir::Query,
        mut rel: Rel,
        types: &[crate::QType],
    ) -> R<(Rel, bool)> {
        if self.query_depth != 1 || self.temp_out {
            return Ok((rel, false));
        }
        let Some(shape) = hierarchy_union_shape(query, self.schema) else {
            return Ok((rel, false));
        };
        let Rel::Union { parts, .. } = &mut rel else {
            return internal("a hierarchy union without union input");
        };
        for part in parts.iter() {
            let Some(exprs) = projection(part).filter(|exprs| exprs.len() == shape.visible) else {
                return Ok((rel, false));
            };
            let Some(base) = exprs.get(shape.column) else {
                return Ok((rel, false));
            };
            let ExprKind::Col { src, .. } = base.kind else {
                return Ok((rel, false));
            };
            let Some(source) = self.sources.get(src.0 as usize) else {
                return Ok((rel, false));
            };
            if shape.source(self.schema, base, &source.kind).is_none()
                || self.hierarchy_of(src).is_none()
            {
                return Ok((rel, false));
            }
        }
        let Some((id, parent)) = shape.slots() else {
            return internal("too many columns");
        };
        for part in parts {
            let project = match part {
                Rel::Top { input, .. } => input.as_mut(),
                other => other,
            };
            let Rel::Project { exprs, .. } = project else {
                return internal("a qualified hierarchy union arm without projection");
            };
            let Some(base) = exprs.get(shape.column) else {
                return internal("a qualified hierarchy union key without source");
            };
            let ExprKind::Col { src, .. } = base.kind else {
                return internal("a qualified hierarchy union key without source");
            };
            let Some((reference, ancestor)) = self.hierarchy_of(src) else {
                return internal("a qualified hierarchy union source without parent");
            };
            exprs.push(reference);
            exprs.push(ancestor);
        }
        let Some(ty) = types.get(shape.column) else {
            return internal("a hierarchy union key outside visible types");
        };
        let keys = vec![SortKey {
            expr: out(shape.column, ty.clone())?,
            desc: shape.desc,
        }];
        Ok((
            Rel::Hierarchy {
                input: Box::new(rel),
                id,
                parent,
                keys,
            },
            true,
        ))
    }

    pub(super) fn order_union(
        &mut self,
        query: &tir::Query,
        first: &tir::Select,
        mut rel: Rel,
        types: &[QType],
    ) -> R<Rel> {
        let hier_count = query
            .order
            .iter()
            .filter(|k| matches!(k.dir, OrderDir::Hierarchy | OrderDir::HierarchyDesc))
            .count();
        if hier_count > 1 {
            return later("ИЕРАРХИЯ с несколькими ключами или группировкой", "Q3");
        }
        let mut order_items: Vec<(usize, OrderDir)> = Vec::new();
        if !query.order.is_empty() {
            order_items.reserve(query.order.len());
            for k in &query.order {
                let Some(col) = k.col else {
                    return internal("an order key of a union outside the select list");
                };
                if first
                    .items
                    .get(col)
                    .is_some_and(|item| item.nested.is_some())
                {
                    return later("сортировка по вложенной таблице", "Q3");
                }
                if u16::try_from(col).is_ok_and(|c| self.present.contains(&c)) {
                    return later("сортировка по представлению", "Q3");
                }
                order_items.push((col, k.dir));
            }
        } else if query.autoorder {
            for (col, ty) in types.iter().enumerate() {
                if first
                    .items
                    .get(col)
                    .is_some_and(|item| item.nested.is_some())
                    || u16::try_from(col).is_ok_and(|c| self.present.contains(&c))
                    || !union_autoorder_col_orderable(ty)
                {
                    continue;
                }
                order_items.push((col, OrderDir::Asc));
            }
            if order_items.is_empty() {
                return Ok(rel);
            }
        }
        let hier_col = order_items.iter().find_map(|(col, dir)| {
            matches!(dir, OrderDir::Hierarchy | OrderDir::HierarchyDesc).then_some(*col)
        });
        let mut hier_targets: Vec<(QTableId, QFieldId)> = Vec::new();
        if let Some(h_col) = hier_col
            && let Some(ty) = types.get(h_col)
        {
            for &atom in &ty.refs {
                if let Some(table) = self.schema.hierarchical_table(atom)
                    && let Some(parent_field) = self.role_field(table, StdRole::Parent)
                    && !hier_targets.contains(&(table, parent_field))
                {
                    hier_targets.push((table, parent_field));
                }
            }
        }
        let source_hier = if let Some(h_col) = hier_col
            && hier_targets.is_empty()
            && query.unions.iter().all(|all| *all)
        {
            self.attach_union_source_hierarchy(&mut rel, h_col, types.len())?
        } else {
            None
        };
        let needs_auto_refs = query.autoorder
            && order_items
                .iter()
                .any(|(col, _)| types.get(*col).is_some_and(|ty| !ty.refs.is_empty()));
        if hier_targets.is_empty() && !needs_auto_refs {
            let mut keys = Vec::with_capacity(order_items.len());
            for (col, dir) in order_items {
                let desc = matches!(dir, OrderDir::Desc | OrderDir::HierarchyDesc);
                let ty = types.get(col).cloned().unwrap_or_default();
                keys.push(SortKey {
                    expr: out(col, ty)?,
                    desc,
                });
            }
            return Ok(match source_hier {
                Some((id, parent)) => Rel::Hierarchy {
                    input: Box::new(rel),
                    id,
                    parent,
                    keys,
                },
                None => Rel::Sort {
                    input: Box::new(rel),
                    keys,
                },
            });
        }
        let mut union_types = types.to_vec();
        if source_hier.is_some() {
            union_types.push(QType::default());
            union_types.push(QType::default());
        }
        let union_width = union_types.len();
        let Ok(src_idx) = u32::try_from(self.sources.len()) else {
            return internal("too many sources");
        };
        let union_src = SrcId(src_idx);
        self.sources.push(SourceS {
            id: union_src,
            all_fields: false,
            kind: SrcKind::Subquery {
                columns: union_types.clone(),
            },
        });
        let subq = Rel::Subquery {
            src: union_src,
            input: Box::new(rel),
        };
        let mut project_exprs = Vec::with_capacity(union_width + 2);
        for (i, ty) in union_types.iter().enumerate() {
            let Ok(col_idx) = u16::try_from(i) else {
                return internal("too many columns");
            };
            project_exprs.push(Expr {
                kind: ExprKind::Col {
                    src: union_src,
                    col: ColSel::Column(col_idx),
                },
                ty: ty.clone(),
            });
        }
        let mut hier = source_hier;
        if !hier_targets.is_empty()
            && let Some(h_col) = hier_col
        {
            for &(table, _) in &hier_targets {
                self.touch(table);
            }
            let Ok(h_col_u16) = u16::try_from(h_col) else {
                return internal("too many columns");
            };
            let base = Expr {
                kind: ExprKind::Col {
                    src: union_src,
                    col: ColSel::Column(h_col_u16),
                },
                ty: types.get(h_col).cloned().unwrap_or_default(),
            };
            let parent_expr = Expr {
                kind: ExprKind::Deref {
                    base: Box::new(base),
                    steps: vec![tir::Deref {
                        name: "\u{0}hier".to_string(),
                        targets: hier_targets,
                    }],
                },
                ty: QType::default(),
            };
            let Ok(parent_slot) = u16::try_from(project_exprs.len()) else {
                return internal("too many columns");
            };
            project_exprs.push(parent_expr);
            hier = Some((h_col_u16, parent_slot));
        }
        let mut sort_keys = Vec::with_capacity(order_items.len());
        for (col, dir) in order_items {
            let desc = matches!(dir, OrderDir::Desc | OrderDir::HierarchyDesc);
            let ty = types.get(col).cloned().unwrap_or_default();
            if query.autoorder && !ty.refs.is_empty() {
                let Ok(col_u16) = u16::try_from(col) else {
                    return internal("too many columns");
                };
                let base = Expr {
                    kind: ExprKind::Col {
                        src: union_src,
                        col: ColSel::Column(col_u16),
                    },
                    ty,
                };
                for key in self.auto_keys(&base)? {
                    let key_ty = key.ty.clone();
                    let slot = project_exprs.len();
                    project_exprs.push(key);
                    sort_keys.push(SortKey {
                        expr: out(slot, key_ty)?,
                        desc,
                    });
                }
            } else {
                sort_keys.push(SortKey {
                    expr: out(col, ty)?,
                    desc,
                });
            }
        }
        let proj = Rel::Project {
            input: Box::new(subq),
            exprs: project_exprs,
        };
        Ok(match hier {
            Some((id, parent)) => Rel::Hierarchy {
                input: Box::new(proj),
                id,
                parent,
                keys: sort_keys,
            },
            None => Rel::Sort {
                input: Box::new(proj),
                keys: sort_keys,
            },
        })
    }

    fn attach_union_source_hierarchy(
        &self,
        rel: &mut Rel,
        col: usize,
        visible: usize,
    ) -> R<Option<(u16, u16)>> {
        let Rel::Union { parts, .. } = rel else {
            return Ok(None);
        };
        let mut any_hier = false;
        for part in parts.iter() {
            let Some(exprs) = projection(part).filter(|exprs| exprs.len() == visible) else {
                return Ok(None);
            };
            let Some(base) = exprs.get(col) else {
                return Ok(None);
            };
            if autoorder::first_src(base)
                .and_then(|src| self.hierarchy_of(src))
                .is_some()
            {
                any_hier = true;
            }
        }
        if !any_hier {
            return Ok(None);
        }
        let (Ok(id_slot), Ok(parent_slot)) = (
            u16::try_from(visible),
            visible
                .checked_add(1)
                .ok_or(())
                .and_then(|n| u16::try_from(n).map_err(|_| ())),
        ) else {
            return internal("too many columns");
        };
        for part in parts.iter_mut() {
            let project = match part {
                Rel::Top { input, .. } => input.as_mut(),
                other => other,
            };
            let Rel::Project { exprs, .. } = project else {
                return internal("a qualified hierarchy union arm without projection");
            };
            let pair = exprs
                .get(col)
                .and_then(autoorder::first_src)
                .and_then(|src| self.hierarchy_of(src));
            let (id_expr, parent_expr) = match pair {
                Some(p) => p,
                None => (
                    Expr {
                        kind: ExprKind::Lit(QValue::Null),
                        ty: QType::default(),
                    },
                    Expr {
                        kind: ExprKind::Lit(QValue::Null),
                        ty: QType::default(),
                    },
                ),
            };
            exprs.push(id_expr);
            exprs.push(parent_expr);
        }
        Ok(Some((id_slot, parent_slot)))
    }
}
