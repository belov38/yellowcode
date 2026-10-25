pub(crate) mod batch;
pub(crate) mod columns;
pub(crate) mod dcs;
pub(crate) mod expr;
pub(crate) mod pos;
pub(crate) mod rls;
pub(crate) mod scope;
pub(crate) mod select;
pub mod stats;
pub(crate) mod vt;

pub use batch::{analyze_batch_static, find_parameters};

pub fn query_column_types(q: &tir::Query) -> Vec<crate::types::QType> {
    select::column_types(q)
}

pub fn query_column_types_budgeted(
    q: &tir::Query,
    budget: &memory_budget::MemoryBudget,
) -> Result<memory_budget::BudgetedVec<memory_budget::Charged<QType>>, memory_budget::BudgetError> {
    select::column_types_budgeted(q, budget)
}

use crate::CompileError;
use crate::params::{ParamInfo, ParamTypes};
use crate::schema::QuerySchema;
use crate::temp::{TempColumn, TempSchema, TempSchemas};
use crate::tir::{self, TExpr};
use crate::types::{DateQ, QType};
use expr::Clause;
use pos::Frame;
use query_syntax::ast::{self, IntoKind};
use query_syntax::{Kw, MsgArg, MsgId, QueryError, SourceMap, with_deep_stack};
use scope::Level;
use select::ColumnScope;
use std::sync::Arc;

pub(crate) type R<T> = Result<T, CompileError>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnalyzeMode {
    Execute,
    Static,
}

#[derive(Clone, Copy)]
pub struct AnalyzeOptions<'a> {
    pub temp_tables: &'a dyn TempSchemas,
    pub params: &'a dyn ParamTypes,
    pub mode: AnalyzeMode,
    pub inter: bool,
}

#[derive(Clone, Copy)]
pub(crate) struct ManagedAnalyzeOptions<'a> {
    pub temp_tables: &'a dyn crate::temp::ManagedTempSchemas,
    pub params: &'a dyn crate::params::ManagedParamTypes,
    pub mode: AnalyzeMode,
    pub inter: bool,
}

pub(crate) struct AnalyzerOptions {
    pub mode: AnalyzeMode,
    pub inter: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ColumnInfo {
    pub name: String,
    pub ty: QType,
    pub width: u32,
    pub nested: Option<Vec<ColumnInfo>>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TempOut {
    Put(Arc<TempSchema>),
    Add(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct CompiledStmt {
    pub columns: Vec<ColumnInfo>,
    pub temp_out: Option<TempOut>,
    pub drop: Option<String>,
    pub reads: Vec<String>,
    pub params: Vec<ParamInfo>,
    pub value_deps: Vec<String>,
    pub table_params: Vec<String>,
    pub typed: tir::Stmt,
}

pub fn analyze_stmt(
    batch: &ast::Batch,
    text: &str,
    i: usize,
    schema: &QuerySchema,
    opts: &AnalyzeOptions<'_>,
) -> Result<Arc<CompiledStmt>, CompileError> {
    let opts = *opts;
    with_deep_stack(batch.max_depth, move || {
        crate::compile::reject_unresolved(batch, text)?;
        analyze_here(batch, text, i, schema, opts)
    })
}

pub(crate) fn analyze_here(
    batch: &ast::Batch,
    text: &str,
    i: usize,
    schema: &QuerySchema,
    opts: AnalyzeOptions<'_>,
) -> Result<Arc<CompiledStmt>, CompileError> {
    Analyzer::new(batch, text, schema, opts)
        .stmt(i)
        .map(Arc::new)
}

pub(crate) struct Analyzer<'a> {
    pub memory: Option<&'a crate::managed::CompileArena>,
    pub stmts: &'a [ast::Stmt],
    pub max_depth: u32,
    pub rls: bool,
    pub text: &'a str,
    pub map: &'a SourceMap,
    pub schema: &'a QuerySchema,
    pub opts: AnalyzerOptions,
    pub temps: crate::temp::Provider<'a>,
    pub params: crate::params::Provider<'a>,
    pub levels: crate::managed::ScratchVec<Level<'a>>,
    pub frames: crate::managed::ScratchVec<Frame>,
    pub reads: crate::managed::ScratchVec<crate::managed::NameKey>,
    pub read_set: crate::managed::Index<crate::managed::NameKey, ()>,
    pub param_uses: crate::managed::ScratchVec<ParamUse<'a>>,
    pub columns: Option<ColumnScope<'a>>,
    pub value_deps: crate::managed::ScratchVec<crate::managed::NameKey>,
    pub param_table: Option<u32>,
    pub param_nested: bool,
    pub table_params: crate::managed::ScratchVec<crate::managed::NameKey>,
}

pub(crate) struct ParamUse<'a> {
    pub name: crate::managed::NameKey,
    pub ty: crate::managed::ScratchType<'a>,
}

impl<'a> Analyzer<'a> {
    pub(crate) fn new(
        batch: &'a ast::Batch,
        text: &'a str,
        schema: &'a QuerySchema,
        opts: AnalyzeOptions<'a>,
    ) -> Analyzer<'a> {
        Analyzer::with_parts(
            &batch.stmts,
            batch.max_depth,
            &batch.map,
            text,
            schema,
            opts,
        )
    }

    pub(crate) fn new_scoped(
        batch: &'a ast::Batch,
        text: &'a str,
        schema: &'a QuerySchema,
        opts: ManagedAnalyzeOptions<'a>,
        arena: &'a crate::managed::CompileArena,
    ) -> R<Self> {
        let mut analyzer = Self::new(
            batch,
            text,
            schema,
            AnalyzeOptions {
                temp_tables: &crate::NoTempTables,
                params: &crate::NoParams,
                mode: opts.mode,
                inter: opts.inter,
            },
        );
        analyzer.levels = crate::managed::ScratchVec::new(Some(arena), 0)?;
        analyzer.frames = crate::managed::ScratchVec::new(Some(arena), 0)?;
        analyzer.reads = crate::managed::ScratchVec::new(Some(arena), 0)?;
        analyzer.param_uses = crate::managed::ScratchVec::new(Some(arena), 0)?;
        analyzer.value_deps = crate::managed::ScratchVec::new(Some(arena), 0)?;
        analyzer.table_params = crate::managed::ScratchVec::new(Some(arena), 0)?;
        analyzer.memory = Some(arena);
        analyzer.temps = crate::temp::Provider::Managed {
            arena,
            tables: opts.temp_tables,
        };
        analyzer.params = crate::params::Provider::Managed {
            arena,
            params: opts.params,
        };
        Ok(analyzer)
    }

    pub(crate) fn with_parts(
        stmts: &'a [ast::Stmt],
        max_depth: u32,
        map: &'a SourceMap,
        text: &'a str,
        schema: &'a QuerySchema,
        opts: AnalyzeOptions<'a>,
    ) -> Analyzer<'a> {
        Analyzer {
            memory: None,
            stmts,
            max_depth,
            rls: false,
            text,
            map,
            schema,
            params: crate::params::Provider::Legacy(opts.params),
            temps: crate::temp::Provider::Legacy(opts.temp_tables),
            opts: AnalyzerOptions {
                mode: opts.mode,
                inter: opts.inter,
            },
            levels: crate::managed::ScratchVec::default(),
            frames: crate::managed::ScratchVec::default(),
            reads: crate::managed::ScratchVec::default(),
            read_set: crate::managed::Index::new(),
            param_uses: crate::managed::ScratchVec::default(),
            columns: None,
            value_deps: crate::managed::ScratchVec::default(),
            param_table: None,
            param_nested: false,
            table_params: crate::managed::ScratchVec::default(),
        }
    }

    pub(crate) fn record_param_use(
        &mut self,
        name: &str,
        ty: crate::managed::ScratchType<'a>,
    ) -> R<()> {
        let name = crate::managed::NameKey::raw(self.memory, name)?;
        self.param_uses.push(self.memory, ParamUse { name, ty })
    }

    fn note_unique_name(
        memory: Option<&crate::managed::CompileArena>,
        names: &mut crate::managed::ScratchVec<crate::managed::NameKey>,
        name: &str,
    ) -> R<()> {
        let key = crate::managed::NameKey::folded(memory, name)?;
        for existing in &*names {
            if crate::managed::NameKey::folded(memory, existing)? == key {
                return Ok(());
            }
        }
        names.push(memory, crate::managed::NameKey::raw(memory, name)?)
    }

    pub(crate) fn note_value_dep(&mut self, name: &str) -> R<()> {
        Self::note_unique_name(self.memory, &mut self.value_deps, name)
    }

    pub(crate) fn note_table_param(&mut self, name: &str) -> R<()> {
        Self::note_unique_name(self.memory, &mut self.table_params, name)
    }

    pub(crate) fn stmt(&mut self, i: usize) -> R<CompiledStmt> {
        if self.memory.is_some() {
            return crate::managed::allocation_error(
                self.memory,
                memory_budget::BudgetError::Overflow,
            );
        }
        let max_depth = self.max_depth;
        let Some(st) = self.stmts.get(i) else {
            return Err(QueryError::new(MsgId::SELECT_ERROR_SYNTAXERROR, None)
                .with_quoted(MsgArg::text("internal: statement index out of range")));
        };
        match st {
            ast::Stmt::Drop(d) => {
                let name = d.name.text.to_string();
                if self.temps.get(&name)?.is_none() {
                    return Err(self.temp_not_exists(&d.name));
                }
                Ok(CompiledStmt {
                    columns: Vec::new(),
                    temp_out: None,
                    drop: Some(name.clone()),
                    reads: Vec::new(),
                    params: Vec::new(),
                    value_deps: Vec::new(),
                    table_params: Vec::new(),
                    typed: tir::Stmt {
                        kind: tir::StmtKind::Drop(name),
                        max_depth,
                    },
                })
            }
            ast::Stmt::Query(qs) => {
                let into = qs.query.parts.first().and_then(|s| s.into.as_ref());
                let query = self.query(&qs.query)?;
                let (columns, temp_out) = match into {
                    Some(ic) => {
                        let name = ic.name.text.to_string();
                        let out = match ic.kind {
                            IntoKind::Put => {
                                if self.temps.get(&name)?.is_some() {
                                    return Err(self
                                        .err(
                                            MsgId::SELECT_ERROR_TEMPTABLEALREADYEXISTS,
                                            ic.name.span.start,
                                        )
                                        .with_quoted(MsgArg::text(&name)));
                                }
                                let open = qs
                                    .query
                                    .parts
                                    .first()
                                    .map(|f| columns::star_over_open(self.memory, f, &query))
                                    .transpose()?
                                    .unwrap_or(false);
                                TempOut::Put(Arc::new(columns::temp_schema(name, &query, open)))
                            }
                            IntoKind::Add => {
                                self.check_add(&ic.name, &query)?;
                                TempOut::Add(name)
                            }
                        };
                        let columns = if self.opts.inter {
                            let stored: Option<crate::QShared<TempSchema>> = match &out {
                                TempOut::Put(t) => Some(Arc::clone(t).into()),
                                TempOut::Add(name) => self.temps.get(name)?,
                            };
                            self.result_columns(&query)
                                .into_iter()
                                .enumerate()
                                .map(|(i, mut c)| {
                                    if let Some(t) = stored.as_ref().and_then(|t| t.columns.get(i))
                                    {
                                        c.name = t.name.clone();
                                    }
                                    c.ty = match stored.as_ref().and_then(|t| t.columns.get(i)) {
                                        Some(TempColumn { ty: Some(t), .. })
                                            if *t == columns::null_column_type() =>
                                        {
                                            t.clone().with_null()
                                        }
                                        Some(TempColumn { ty: Some(t), .. }) => t.clone(),
                                        _ => c.ty.without_null(),
                                    };
                                    c.width = c.ty.width();
                                    c
                                })
                                .collect()
                        } else {
                            vec![self.count_column()]
                        };
                        (columns, Some(out))
                    }
                    None => (self.result_columns(&query), None),
                };
                if self.param_table.is_some()
                    && (into.is_none() || self.param_nested)
                    && self.opts.mode == AnalyzeMode::Execute
                {
                    return Err(QueryError::new(
                        MsgId::SELECT_ERROR_WRONGPARAMETERTABLEUSAGE,
                        None,
                    ));
                }
                Ok(CompiledStmt {
                    columns,
                    temp_out,
                    drop: None,
                    reads: crate::managed::collect(
                        self.memory,
                        self.reads
                            .iter()
                            .map(|name| crate::managed::string(self.memory, name)),
                    )?,
                    params: self.param_infos()?,
                    value_deps: crate::managed::collect(
                        self.memory,
                        self.value_deps
                            .iter()
                            .map(|name| crate::managed::string(self.memory, name)),
                    )?,
                    table_params: crate::managed::collect(
                        self.memory,
                        self.table_params
                            .iter()
                            .map(|name| crate::managed::string(self.memory, name)),
                    )?,
                    typed: tir::Stmt {
                        kind: tir::StmtKind::Query(query),
                        max_depth,
                    },
                })
            }
        }
    }

    pub(crate) fn stmt_owned(
        &mut self,
        i: usize,
    ) -> R<crate::managed::NativeOwned<'a, crate::managed::statement::Statement>> {
        use crate::managed::statement::{self as statement, Parts, TempOutput};
        use crate::managed::{self as mem, NativeOwned, NativeVec, OwnedString};
        let scope = self.memory;
        let Some(st) = self.stmts.get(i) else {
            return mem::allocation_error(scope, memory_budget::BudgetError::Overflow);
        };
        match st {
            ast::Stmt::Drop(drop) => {
                if self.temps.get(&drop.name.text)?.is_none() {
                    return Err(self.temp_not_exists(&drop.name));
                }
                let name = OwnedString::new(scope, &drop.name.text)?.into_native_owned();
                let typed = statement::typed_drop(
                    OwnedString::new(scope, &drop.name.text)?.into_native_owned(),
                );
                statement::assemble(
                    scope,
                    Parts {
                        columns: NativeVec::new(scope, 0)?.finish(),
                        temp_out: NativeOwned::none(scope),
                        drop: name.map(Some),
                        reads: NativeVec::new(scope, 0)?.finish(),
                        params: NativeVec::new(scope, 0)?.finish(),
                        value_deps: NativeVec::new(scope, 0)?.finish(),
                        table_params: NativeVec::new(scope, 0)?.finish(),
                        typed,
                    },
                )
            }
            ast::Stmt::Query(qs) => {
                let into = qs.query.parts.first().and_then(|part| part.into.as_ref());
                let query = self.query_owned(&qs.query)?;
                let q = query.trusted_borrow();
                let (columns, temp_out) = if let Some(ic) = into {
                    let name = OwnedString::new(scope, &ic.name.text)?.into_native_owned();
                    let output = match ic.kind {
                        IntoKind::Put => {
                            if self.temps.get(&ic.name.text)?.is_some() {
                                return Err(self.quoted_text_error(
                                    MsgId::SELECT_ERROR_TEMPTABLEALREADYEXISTS,
                                    &ic.name.text,
                                    ic.name.span.start,
                                ));
                            }
                            let open = qs
                                .query
                                .parts
                                .first()
                                .map(|first| columns::star_over_open(scope, first, q))
                                .transpose()?
                                .unwrap_or(false);
                            statement::put(scope, statement::temp_schema(scope, name, q, open)?)?
                        }
                        IntoKind::Add => {
                            self.check_add_owned(&ic.name, q)?;
                            statement::add(name)
                        }
                    };
                    let columns = if self.opts.inter {
                        let added = match &ic.kind {
                            IntoKind::Add => self.temps.get(&ic.name.text)?,
                            IntoKind::Put => None,
                        };
                        let stored = match output.trusted_borrow() {
                            Some(TempOutput::Put(schema)) => Some(&**schema),
                            _ => added.as_deref(),
                        };
                        statement::columns(scope, q, true, stored)?
                    } else {
                        statement::count_columns(scope, self.schema.script())?
                    };
                    (columns, output)
                } else {
                    (
                        statement::columns(scope, q, false, None)?,
                        NativeOwned::none(scope),
                    )
                };
                if self.param_table.is_some()
                    && (into.is_none() || self.param_nested)
                    && self.opts.mode == AnalyzeMode::Execute
                {
                    return Err(match scope {
                        Some(arena) => {
                            arena.unpositioned_error(MsgId::SELECT_ERROR_WRONGPARAMETERTABLEUSAGE)
                        }
                        None => QueryError::new(MsgId::SELECT_ERROR_WRONGPARAMETERTABLEUSAGE, None),
                    });
                }
                let params = statement::params(scope, &self.param_uses)?;
                self.param_uses.clear();
                statement::assemble(
                    scope,
                    Parts {
                        columns,
                        temp_out,
                        drop: NativeOwned::none(scope),
                        reads: statement::names(scope, &self.reads)?,
                        params,
                        value_deps: statement::names(scope, &self.value_deps)?,
                        table_params: statement::names(scope, &self.table_params)?,
                        typed: statement::typed_query(query),
                    },
                )
            }
        }
    }

    fn check_add_owned(&self, name: &ast::Ident, query: &tir::Query) -> R<()> {
        let Some(table) = self.temps.get(&name.text)? else {
            return Err(self.temp_not_exists(name));
        };
        let Some(first) = query.parts.first() else {
            return Ok(());
        };
        if first.items.len() != table.columns.len() {
            return Err(self.quoted_text_error(
                MsgId::SELECT_ERROR_WRONGADDTEMPTABLEFIELDS,
                &name.text,
                name.span.start,
            ));
        }
        let types = select::column_types_scoped(self.memory, &query.parts, query.totals.as_ref())?;
        for ((item, col), ty) in first.items.iter().zip(&table.columns).zip(types.iter()) {
            let Some(want) = &col.ty else {
                continue;
            };
            let ty = crate::managed::type_copy(self.memory, ty)?.without_null();
            if want.is_undefined() || (ty.is_subtype_of(want) && qualifiers_fit(want, &ty)) {
                continue;
            }
            return Err(self.quoted_text_error(
                MsgId::SELECT_ERROR_WRONGADDTEMPTABLEFIELDTYPE,
                &item.name,
                item.expr.span.start,
            ));
        }
        Ok(())
    }

    fn temp_not_exists(&self, name: &ast::Ident) -> CompileError {
        self.quoted_text_error(
            MsgId::SELECT_ERROR_TEMPTABLENOTEXISTS,
            &name.text,
            name.span.start,
        )
    }

    fn check_add(&self, name: &ast::Ident, query: &tir::Query) -> R<()> {
        let Some(table) = self.temps.get(&name.text)? else {
            return Err(self.temp_not_exists(name));
        };
        let Some(first) = query.parts.first() else {
            return Ok(());
        };
        if first.items.len() != table.columns.len() {
            return Err(self.quoted_text_error(
                MsgId::SELECT_ERROR_WRONGADDTEMPTABLEFIELDS,
                &name.text,
                name.span.start,
            ));
        }
        let types = select::column_types(query);
        for ((item, col), ty) in first.items.iter().zip(&table.columns).zip(&types) {
            let Some(want) = &col.ty else { continue };
            let ty = ty.clone().without_null();
            if want.is_undefined() || (ty.is_subtype_of(want) && qualifiers_fit(want, &ty)) {
                continue;
            }
            return Err(self.quoted_text_error(
                MsgId::SELECT_ERROR_WRONGADDTEMPTABLEFIELDTYPE,
                &item.name,
                item.expr.span.start,
            ));
        }
        Ok(())
    }

    pub(crate) fn param_infos(&mut self) -> R<Vec<ParamInfo>> {
        let mut out: Vec<ParamInfo> = Vec::new();
        let mut index: crate::managed::Index<crate::managed::NameKey, usize> =
            crate::managed::Index::new();
        for u in &self.param_uses {
            let key = crate::managed::NameKey::folded(self.memory, &u.name)?;
            match index.get(key.as_str()) {
                Some(i) => {
                    if let Some(p) = out.get_mut(*i) {
                        let merged = crate::managed::merge(self.memory, &p.ty, &u.ty)?;
                        crate::managed::replace_type(self.memory, &mut p.ty, merged)?;
                    }
                }
                None => {
                    index.insert(self.memory, key, out.len())?;
                    crate::managed::push(
                        self.memory,
                        &mut out,
                        ParamInfo {
                            name: crate::managed::string(self.memory, &u.name)?,
                            ty: crate::managed::copy(self.memory, &*u.ty)?,
                        },
                    )?;
                }
            }
        }
        self.param_uses.clear();
        Ok(out)
    }

    pub(crate) fn query(&mut self, q: &ast::Query) -> R<tir::Query> {
        if self.memory.is_some() {
            return crate::managed::allocation_error(
                self.memory,
                memory_budget::BudgetError::Overflow,
            );
        }
        self.query_owned(q)
            .map(crate::managed::NativeOwned::into_native)
    }

    pub(crate) fn query_owned(
        &mut self,
        q: &ast::Query,
    ) -> R<crate::managed::NativeOwned<'a, tir::Query>> {
        let nested = self.nested();
        let mut parts = crate::managed::NativeVec::new(self.memory, q.parts.len())?;
        let mut levels = crate::managed::ScratchVec::new(self.memory, q.parts.len())?;
        for (i, s) in q.parts.iter().enumerate() {
            if let Some(ic) = &s.into
                && (nested || i > 0)
            {
                let kw = match ic.kind {
                    IntoKind::Put => Kw::Into,
                    IntoKind::Add => Kw::Add,
                };
                return Err(self.substituted_error(
                    MsgId::SELECT_ERROR_INTOCANBEONLYINMAINSELECT,
                    ic.kw.start,
                    [crate::managed::Argument::Kw(kw)],
                ));
            }
            let (part, level) = self.select(s)?;
            parts.push_owned(part)?;
            levels.push(self.memory, level)?;
        }
        self.union_columns(parts.items())?;
        let into = q.parts.first().is_some_and(|s| s.into.is_some());
        self.autonumber_checks(parts.items(), into)?;
        let first_level = levels.first_mut().map(std::mem::take).unwrap_or_default();
        self.levels.push(self.memory, first_level)?;
        let r = self
            .order_and_totals(q, parts.items(), nested)
            .and_then(|ot| {
                if let Some(first) = parts.items().first() {
                    self.dcs_query_blocks(&q.dcs, first)?;
                }
                Ok(ot)
            })
            .and_then(|(order, totals)| {
                let index = match q.parts.last().and_then(|s| s.index_by.as_ref()) {
                    Some(ix) => {
                        let types = select::column_types_scoped(
                            self.memory,
                            parts.items(),
                            totals.trusted_borrow().as_ref(),
                        )?;
                        Some(self.index_sets(ix, parts.items(), &types)?)
                    }
                    None => None,
                };
                Ok((order, totals, index))
            });
        let first_level = self.levels.pop().unwrap_or_default();
        if let Some(slot) = levels.first_mut() {
            *slot = first_level;
        }
        let (order, totals, index) = r?;
        self.vt_checks(&levels)?;
        for (index, (level, s)) in levels.iter_mut().zip(&q.parts).enumerate() {
            if let Some(f) = &s.from {
                parts.install_sources(index, self.assemble_sources(level, f)?)?;
            }
        }
        if let Some(sets) = index {
            parts.install_indexes(sets)?;
            let kind = q
                .parts
                .last()
                .and_then(|s| s.index_by.as_ref())
                .map(|i| &i.kind);
            let mut unique = crate::managed::NativeVec::new(
                self.memory,
                match kind {
                    Some(ast::IndexKind::Fields { .. }) => 1,
                    Some(ast::IndexKind::Sets(sets)) => sets.len(),
                    None => 0,
                },
            )?;
            match kind {
                Some(ast::IndexKind::Fields { unique: flag, .. }) => {
                    unique.push_copy(flag.is_some())?
                }
                Some(ast::IndexKind::Sets(sets)) => {
                    for set in sets {
                        unique.push_copy(set.unique.is_some())?;
                    }
                }
                None => {}
            }
            parts.install_index_unique(unique.finish())?;
        }
        let mut unions = crate::managed::NativeVec::new(self.memory, q.unions.len())?;
        for union in &q.unions {
            unions.push_copy(union.all)?;
        }
        crate::managed::query::query(
            self.memory,
            parts.finish(),
            unions.finish(),
            order,
            q.autoorder.is_some(),
            totals,
        )
    }

    fn select(
        &mut self,
        s: &ast::Select,
    ) -> R<(crate::managed::NativeOwned<'a, tir::Select>, Level<'a>)> {
        let level = self.declare_sources(s.from.as_ref())?;
        self.levels.push(self.memory, level)?;
        let r = self.select_in_level(s);
        let level = self.levels.pop().unwrap_or_default();
        r.map(|sel| (sel, level))
    }

    fn select_in_level(
        &mut self,
        s: &ast::Select,
    ) -> R<crate::managed::NativeOwned<'a, tir::Select>> {
        if let Some(f) = &s.from {
            self.join_conditions(f)?;
        }
        let mut names = columns::Names::default();
        let mut items = crate::managed::SelectItems::new(self.memory, s.items.len())?;
        for item in &s.items {
            self.item_owned(item, &mut items, &mut names)?;
        }
        self.select_tail(s, items)
    }

    fn select_tail(
        &mut self,
        s: &ast::Select,
        items: crate::managed::SelectItems<'a>,
    ) -> R<crate::managed::NativeOwned<'a, tir::Select>> {
        use crate::managed::{NativeOwned, NativeVec};
        let where_ = match &s.where_ {
            Some(c) => self
                .condition_owned(&c.expr, Clause::Where, Kw::Where)?
                .into_some(),
            None => NativeOwned::none(self.memory),
        };
        self.dcs_select_blocks(&s.dcs, items.items())?;
        let group = match &s.group_by {
            Some(g) => self.group_by(g)?.into_some(),
            None => NativeOwned::none(self.memory),
        };
        let having = match &s.having {
            Some(h) => self
                .condition_owned(&h.expr, Clause::Having, Kw::Having)?
                .into_some(),
            None => NativeOwned::none(self.memory),
        };
        if let Some(g) = select::grouping_of(
            self.memory,
            items.items(),
            group.trusted_borrow().as_deref(),
            having.trusted_borrow().as_ref(),
        )? {
            for item in items.items() {
                self.check_group(&item.expr, &g)?;
            }
            if let Some(h) = having.trusted_borrow() {
                self.check_group(h, &g)?;
            }
        }
        let for_update = match &s.for_update {
            Some(f) if f.tables.is_empty() => NativeVec::new(self.memory, 0)?.finish().into_some(),
            Some(f) => self.for_update_tables(f)?.into_some(),
            None => NativeOwned::none(self.memory),
        };
        crate::managed::query::select(
            self.memory,
            crate::managed::query::SelectOptions {
                allowed: s.allowed.is_some(),
                distinct: s.distinct.is_some(),
                top: s
                    .top
                    .as_ref()
                    .map(|t| t.count.digits.parse().unwrap_or(u64::MAX)),
            },
            items.into_native_owned(),
            where_,
            group,
            having,
            for_update,
        )
    }
}

pub(crate) fn boolean_of(xs: &[&TExpr]) -> QType {
    let t = QType::boolean();
    if xs.iter().any(|x| x.ty.null) {
        t.with_null()
    } else {
        t
    }
}

fn qualifiers_fit(want: &QType, ty: &QType) -> bool {
    let string = match (want.string, ty.string) {
        (Some(w), Some(t)) => w.len == 0 || (t.len != 0 && t.len <= w.len),
        _ => true,
    };
    let number = match (want.number, ty.number) {
        (Some(w), Some(t)) => w.digits == 0 || t.digits != 0,
        _ => true,
    };
    let date = match (want.date, ty.date) {
        (Some(w), Some(t)) => w == DateQ::DateTime || w == t,
        _ => true,
    };
    string && number && date
}
