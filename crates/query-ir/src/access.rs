use crate::exec_error::ExecError;
use crate::ir::{StmtS, build_stmt};
use crate::params::{NoParams, ParamInfo};
use crate::schema::{FieldRole, QTableId, QuerySchema, StdRole};
use crate::sema::{AnalyzeMode, AnalyzeOptions, Analyzer, ColumnInfo, CompiledStmt};
use crate::temp::NoTempTables;
use crate::tir;
use crate::types::QType;
use crate::value::QValue;
use crate::{CompileError, QFieldId};
use bsl_prim::fold_name;
use query_syntax::ast::{AccessCondition, CmpOp};
use query_syntax::{INLINE_DEPTH, Span, with_deep_stack};

#[derive(Debug, PartialEq)]
pub struct AccessPredicate {
    pub(crate) table: QTableId,
    pub(crate) form: PredicateForm,
    pub(crate) params: Vec<String>,
    pub(crate) max_depth: u32,
}

#[derive(Debug, PartialEq)]
pub(crate) enum PredicateForm {
    Short(tir::TExpr),
    Full {
        alias: String,
        query: Box<tir::Select>,
    },
}

impl AccessPredicate {
    pub fn table(&self) -> QTableId {
        self.table
    }

    pub fn session_params(&self) -> Vec<String> {
        self.params.clone()
    }

    pub fn is_full(&self) -> bool {
        matches!(self.form, PredicateForm::Full { .. })
    }

    pub fn check_stmt(
        &self,
        schema: &QuerySchema,
        row_id: Option<&QValue>,
    ) -> Result<StmtS, ExecError> {
        let internal = |s: &str| ExecError::Fatal(format!("access predicate: {s}"));
        let built = with_deep_stack(self.max_depth, || -> Result<CompiledStmt, ExecError> {
            let (item, sources, where_) = match &self.form {
                PredicateForm::Short(e) => {
                    let sources = vec![tir::Source {
                        all_fields: false,
                        alias: String::new(),
                        from: tir::From::Table {
                            id: self.table,
                            args: Vec::new(),
                            periodicity: crate::schema::Periodicity::Absent,
                            complement: None,
                            span: None,
                            breakdown: Vec::new(),
                        },
                        joins: Vec::new(),
                    }];
                    (e.clone(), sources, None)
                }
                PredicateForm::Full { alias, query } => {
                    let id =
                        row_id.ok_or_else(|| internal("the full form needs the ID of the row"))?;
                    let (src, field, ty) = self.restricted_id(schema, alias, query)?;
                    let span = Span::default();
                    let at = |kind| tir::TExpr {
                        kind,
                        ty: ty.clone(),
                        span,
                    };
                    let same = tir::TExpr {
                        kind: tir::TKind::Compare(
                            CmpOp::Eq,
                            Box::new(at(tir::TKind::Field {
                                level: 0,
                                source: src,
                                col: tir::Col::Field(field),
                                derefs: Vec::new(),
                            })),
                            Box::new(at(tir::TKind::Const(id.clone()))),
                        ),
                        ty: QType::boolean(),
                        span,
                    };
                    let cond = match &query.where_ {
                        Some(w) => tir::TExpr {
                            kind: tir::TKind::And(vec![same, w.clone()]),
                            ty: QType::boolean(),
                            span,
                        },
                        None => same,
                    };
                    let yes = tir::TExpr {
                        kind: tir::TKind::Const(QValue::Bool(true)),
                        ty: QType::boolean(),
                        span,
                    };
                    (yes, query.sources.clone(), Some(cond))
                }
            };
            let ty = item.ty.clone();
            let select = tir::Select {
                allowed: false,
                distinct: false,
                top: matches!(self.form, PredicateForm::Full { .. }).then_some(1),
                items: vec![tir::Item {
                    star: false,
                    expr: item,
                    name: String::new(),
                    nested: None,
                    unknown: false,
                }],
                sources,
                where_,
                group: None,
                having: None,
                for_update: None,
                index_by: Vec::new(),
                index_unique: Vec::new(),
            };
            Ok(CompiledStmt {
                columns: vec![ColumnInfo {
                    name: String::new(),
                    width: ty.width(),
                    ty,
                    nested: None,
                }],
                temp_out: None,
                drop: None,
                reads: Vec::new(),
                params: self
                    .params
                    .iter()
                    .map(|name| ParamInfo {
                        name: name.clone(),
                        ty: QType::undefined(),
                    })
                    .collect(),
                value_deps: Vec::new(),
                table_params: Vec::new(),
                typed: tir::Stmt {
                    static_analysis: true,
                    kind: tir::StmtKind::Query(tir::Query {
                        parts: vec![select],
                        unions: Vec::new(),
                        order: Vec::new(),
                        autoorder: false,
                        totals: None,
                    }),
                    max_depth: self.max_depth,
                },
            })
        })?;
        build_stmt(&built, schema).map_err(|e| ExecError::Fatal(format!("access predicate: {e:?}")))
    }

    pub(crate) fn read_stmt(&self, schema: &QuerySchema) -> Result<StmtS, ExecError> {
        let PredicateForm::Full { alias, query } = &self.form else {
            return self.check_stmt(schema, None);
        };
        let (src, field, ty) = self.restricted_id(schema, alias, query)?;
        let span = Span::default();
        let reference = |source| tir::TExpr {
            kind: tir::TKind::Field {
                level: 0,
                source,
                col: tir::Col::Field(field),
                derefs: Vec::new(),
            },
            ty: ty.clone(),
            span,
        };
        let mut query = (**query).clone();
        query.items = vec![tir::Item {
            star: false,
            expr: reference(src),
            name: String::new(),
            nested: None,
            unknown: false,
        }];
        let condition = tir::TExpr {
            kind: tir::TKind::InQuery {
                l: vec![reference(0)],
                not: false,
                hierarchy: false,
                q: Box::new(tir::Query {
                    parts: vec![query],
                    unions: Vec::new(),
                    order: Vec::new(),
                    autoorder: false,
                    totals: None,
                }),
            },
            ty: QType::boolean(),
            span,
        };
        let predicate = Self {
            table: self.table,
            form: PredicateForm::Short(condition),
            params: self.params.clone(),
            max_depth: self.max_depth.saturating_add(8),
        };
        predicate.check_stmt(schema, None)
    }

    pub fn bind_check(
        &self,
        ctx: &crate::BindCtx<'_>,
        row_id: Option<&QValue>,
    ) -> Result<crate::StmtL, ExecError> {
        let stmt = self.check_stmt(ctx.schema, row_id)?;
        let bound = crate::bind::resolve(&stmt, ctx)?;
        crate::lower::lower(&bound, ctx)
    }

    fn restricted_id(
        &self,
        schema: &QuerySchema,
        alias: &str,
        query: &tir::Select,
    ) -> Result<(u16, QFieldId, QType), ExecError> {
        let internal = |s: &str| ExecError::Fatal(format!("access predicate: {s}"));
        let key = fold_name(alias);
        let mut stack: Vec<&tir::Source> = query.sources.iter().rev().collect();
        let mut n: u16 = 0;
        let mut found = None;
        while let Some(s) = stack.pop() {
            if found.is_none()
                && fold_name(&s.alias) == key
                && matches!(&s.from, tir::From::Table { id, .. } if *id == self.table)
            {
                found = Some(n);
            }
            n = n.saturating_add(1);
            stack.extend(s.joins.iter().rev().map(|j| &j.right));
        }
        let src = found.ok_or_else(|| internal("the restricted source is not in ИЗ"))?;
        let table = schema.table(self.table);
        let (pos, field) = table
            .fields
            .iter()
            .enumerate()
            .find(|(_, f)| f.role == FieldRole::Std(StdRole::Ref))
            .ok_or_else(|| internal("the restricted table has no Ссылка"))?;
        let id = u16::try_from(pos).map_err(|_| internal("field number"))?;
        Ok((src, QFieldId(id), field.ty.clone()))
    }
}

impl Drop for AccessPredicate {
    fn drop(&mut self) {
        if self.max_depth > INLINE_DEPTH {
            let placeholder = PredicateForm::Short(tir::TExpr {
                kind: tir::TKind::Const(QValue::Null),
                ty: QType::undefined(),
                span: Span::default(),
            });
            let form = std::mem::replace(&mut self.form, placeholder);
            with_deep_stack(self.max_depth, move || drop(form));
        }
    }
}

pub fn compile_access_condition(
    cond: &AccessCondition,
    text: &str,
    schema: &QuerySchema,
    table: QTableId,
) -> Result<AccessPredicate, CompileError> {
    with_deep_stack(cond.max_depth, move || {
        let opts = AnalyzeOptions {
            temp_tables: &NoTempTables,
            params: &NoParams,
            mode: AnalyzeMode::Static,
            inter: false,
        };
        let mut a = Analyzer::with_parts(&[], cond.max_depth, &cond.map, text, schema, opts);
        let form = a.access_form(&cond.form, table)?;
        let params = a.param_infos()?.into_iter().map(|p| p.name).collect();
        Ok(AccessPredicate {
            table,
            form,
            params,
            max_depth: cond.max_depth,
        })
    })
}
