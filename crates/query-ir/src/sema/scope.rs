use super::expr::Clause;
use super::pos::{Frame, bare_name_at, path_prefix};
use super::{AnalyzeMode, Analyzer, R};
use crate::managed::{self as mem, CompileArena};
use crate::params::ParamShape;
use crate::schema::{FieldRole, Only, Periodicity, QField, QTableId, StdRole, TableKind};
use crate::temp::{TempColumn, TempSchema};
use crate::tir::{self, Col, TExpr};
use crate::types::QType;
use query_syntax::ast::{self, Ident, JoinKind};
use query_syntax::{Kw, MsgId, ScriptVariant, Span};
use std::sync::Arc;

#[derive(Default)]
pub(crate) struct Level<'scope> {
    pub sources: mem::ScratchVec<ScopeSource<'scope>>,
    by_alias: mem::Index<mem::NameKey, u16>,
    by_table: mem::Index<QTableId, u16>,
    max_parts: usize,
    visible: Option<(usize, usize)>,
}

pub(crate) struct ScopeSource<'scope> {
    pub alias: mem::NameKey,
    pub cols: SourceCols,
    pub nullable: bool,
    pub from: Option<mem::NativeOwned<'scope, tir::From>>,
    pub on: Option<mem::NativeOwned<'scope, TExpr>>,
    pub vt_check: Option<VtCheck>,
    pub dim_res_used: bool,
}

pub(crate) enum SourceCols {
    Table {
        id: QTableId,
        periodicity: Periodicity,
        scope: FieldScope,
        extra: mem::ScratchVec<ExtraCol>,
    },
    Columns(Columns),
    Open {
        param: mem::NameKey,
        cols: Columns,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FieldScope {
    All,
    Condition,
    Rls,
}

pub(crate) struct ExtraCol {
    pub name_ru: mem::NameKey,
    pub name_en: mem::NameKey,
    key_ru: mem::NameKey,
    key_en: mem::NameKey,
    pub ty: mem::ScopeType,
}

impl ExtraCol {
    pub(super) fn breakdown(
        memory: Option<&CompileArena>,
        name_ru: &str,
        name_en: &str,
        ty: &QType,
    ) -> R<Self> {
        let name_ru = mem::NameKey::suffixed(memory, name_ru, "Разрез")?;
        let name_en = mem::NameKey::suffixed(memory, name_en, "Dimension")?;
        let key_ru = mem::NameKey::folded(memory, &name_ru)?;
        let key_en = mem::NameKey::folded(memory, &name_en)?;
        let ty = mem::ScopeType::copy(memory, ty)?.with_null();
        Ok(Self {
            name_ru,
            name_en,
            key_ru,
            key_en,
            ty,
        })
    }
}

pub(crate) struct VtCheck {
    pub at: u32,
    pub path: mem::NameKey,
}

#[derive(Default)]
pub(crate) struct Columns {
    pub names: mem::ScratchVec<mem::NameKey>,
    pub types: mem::ScratchVec<mem::ScopeType>,
    pub known: mem::ScratchVec<ColKnown>,
    index: mem::Index<mem::NameKey, u16>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ColKnown {
    Typed,
    Untyped,
    Unknown,
}

impl Columns {
    pub(crate) fn new(memory: Option<&CompileArena>) -> R<Self> {
        Ok(Self {
            names: mem::ScratchVec::new(memory, 0)?,
            types: mem::ScratchVec::new(memory, 0)?,
            known: mem::ScratchVec::new(memory, 0)?,
            index: mem::Index::new(),
        })
    }
    pub fn push(
        &mut self,
        memory: Option<&CompileArena>,
        name: &str,
        ty: &QType,
    ) -> R<Option<u16>> {
        self.push_known(memory, name, ty, ColKnown::Typed)
    }
    pub fn push_known(
        &mut self,
        memory: Option<&CompileArena>,
        name: &str,
        ty: &QType,
        known: ColKnown,
    ) -> R<Option<u16>> {
        let name = mem::NameKey::raw(memory, name)?;
        let ty = mem::ScopeType::copy(memory, ty)?;
        self.push_owned(memory, name, ty, known)
    }
    fn push_owned(
        &mut self,
        memory: Option<&CompileArena>,
        name: mem::NameKey,
        ty: mem::ScopeType,
        known: ColKnown,
    ) -> R<Option<u16>> {
        let Ok(index) = u16::try_from(self.names.len()) else {
            return Ok(None);
        };
        let key = mem::NameKey::folded(memory, &name)?;
        if self.index.get(key.as_str()).is_none() {
            self.index.insert(memory, key, index)?;
        }
        self.names.push(memory, name)?;
        self.types.push(memory, ty)?;
        self.known.push(memory, known)?;
        Ok(Some(index))
    }

    pub fn known_at(&self, i: u16) -> ColKnown {
        self.known
            .get(usize::from(i))
            .copied()
            .unwrap_or(ColKnown::Typed)
    }

    fn find(&self, key: &str) -> Option<u16> {
        self.index.get(key).copied()
    }

    fn of_temp(
        memory: Option<&CompileArena>,
        table: &TempSchema,
        param: bool,
    ) -> R<Option<Columns>> {
        let mut columns = Columns::new(memory)?;
        for col in &table.columns {
            let ty = match &col.ty {
                Some(ty) if !ty.is_undefined() => mem::ScopeType::copy(memory, ty)?.with_null(),
                _ => mem::ScopeType::copy(memory, &QType::undefined())?,
            };
            let known = match (&col.ty, col.unknown) {
                (_, true) => ColKnown::Unknown,
                (None, false) if param => ColKnown::Untyped,
                _ => ColKnown::Typed,
            };
            let name = mem::NameKey::raw(memory, &col.name)?;
            if columns.push_owned(memory, name, ty, known)?.is_none() {
                return Ok(None);
            }
        }
        Ok(Some(columns))
    }
}

struct Declared<'scope> {
    cols: SourceCols,
    from: Option<mem::NativeOwned<'scope, tir::From>>,
    default_alias: Option<(mem::NameKey, u32)>,
    table: Option<(QTableId, usize)>,
    vt_check: Option<VtCheck>,
}

struct FieldRef {
    level: u16,
    source: u16,
    col: Col,
    ty: QType,
    nested: Option<QTableId>,
    unknown: bool,
    no_deref: bool,
}

pub(super) fn periodicity_word(w: &str) -> Periodicity {
    for (ru, en, value) in [
        ("авто", "auto", Periodicity::Auto),
        ("запись", "record", Periodicity::Record),
        ("регистратор", "recorder", Periodicity::Recorder),
    ] {
        if bsl_prim::lowercase(w).eq(ru.chars()) || bsl_prim::lowercase(w).eq(en.chars()) {
            return value;
        }
    }
    let (kws, _) = query_syntax::kw::lookup(w);
    crate::types::rules::UNITS_WITH_SECOND
        .into_iter()
        .find(|kw| kws.contains(*kw))
        .map_or(Periodicity::Absent, Periodicity::Unit)
}

fn scope_sees(schema: &crate::QuerySchema, id: QTableId, scope: FieldScope, qf: &QField) -> bool {
    match scope {
        FieldScope::All => true,
        FieldScope::Rls => match qf.role {
            FieldRole::Dimension => {
                qf.md.is_none()
                    || schema
                        .table(id)
                        .fields
                        .iter()
                        .filter(|f| f.role == FieldRole::Dimension && f.md == qf.md)
                        .count()
                        == 1
            }
            FieldRole::Std(r) => rls_no_deref(r),
            _ => false,
        },
        FieldScope::Condition => match qf.role {
            FieldRole::Dimension => true,
            FieldRole::Std(r) => matches!(
                r,
                StdRole::Account
                    | StdRole::AccountDr
                    | StdRole::AccountCr
                    | StdRole::BalancedAccount
                    | StdRole::ExtDimension(_)
                    | StdRole::ExtDimensionDr(_)
                    | StdRole::ExtDimensionCr(_)
                    | StdRole::BalancedExtDimension(_)
            ),
            _ => false,
        },
    }
}

fn rls_no_deref(r: StdRole) -> bool {
    matches!(
        r,
        StdRole::Account | StdRole::AccountDr | StdRole::AccountCr | StdRole::Recorder
    )
}

fn extra_index(extra: &[ExtraCol], key: &str) -> Option<u16> {
    let i = extra
        .iter()
        .position(|x| x.key_ru.as_str() == key || x.key_en.as_str() == key)?;
    u16::try_from(i).ok()
}

impl<'scope> Level<'scope> {
    fn new(memory: Option<&CompileArena>) -> R<Self> {
        Ok(Self {
            sources: mem::ScratchVec::new(memory, 0)?,
            ..Self::default()
        })
    }
    pub(crate) fn of_table(memory: Option<&CompileArena>, id: QTableId) -> R<Level<'scope>> {
        Self::of_table_scope(memory, id, FieldScope::All)
    }
    pub(crate) fn of_table_scope(
        memory: Option<&CompileArena>,
        id: QTableId,
        scope: FieldScope,
    ) -> R<Level<'scope>> {
        let mut level = Self::new(memory)?;
        level.sources.push(
            memory,
            ScopeSource {
                alias: mem::NameKey::raw(memory, "")?,
                cols: SourceCols::Table {
                    id,
                    periodicity: Periodicity::Absent,
                    scope,
                    extra: mem::ScratchVec::new(memory, 0)?,
                },
                nullable: false,
                from: None,
                on: None,
                vt_check: None,
                dim_res_used: false,
            },
        )?;
        Ok(level)
    }
    pub(crate) fn of_columns(memory: Option<&CompileArena>, cols: Columns) -> R<Level<'scope>> {
        let mut level = Self::new(memory)?;
        level.sources.push(
            memory,
            ScopeSource {
                alias: mem::NameKey::raw(memory, "")?,
                cols: SourceCols::Columns(cols),
                nullable: false,
                from: None,
                on: None,
                vt_check: None,
                dim_res_used: false,
            },
        )?;
        Ok(level)
    }
    fn mark_nullable(&mut self, memory: Option<&CompileArena>, ranges: &[(usize, usize)]) -> R<()> {
        if ranges.is_empty() {
            return Ok(());
        }
        let mut diff = mem::ScratchVec::new(memory, self.sources.len() + 1)?;
        for _ in 0..=self.sources.len() {
            diff.push(memory, 0i64)?;
        }
        for &(a, b) in ranges {
            crate::probe::hit();
            if let Some(d) = diff.get_mut(a) {
                *d += 1;
            }
            if let Some(d) = diff.get_mut(b) {
                *d -= 1;
            }
        }
        let mut open = 0;
        for (source, delta) in self.sources.iter_mut().zip(diff.iter()) {
            crate::probe::hit();
            open += delta;
            source.nullable |= open > 0;
        }
        Ok(())
    }

    fn visible_range(&self) -> (usize, usize) {
        self.visible.unwrap_or((0, self.sources.len()))
    }

    fn is_visible(&self, i: u16) -> bool {
        let (a, b) = self.visible_range();
        (a..b).contains(&usize::from(i))
    }

    fn find_source(
        &self,
        memory: Option<&CompileArena>,
        schema: &crate::QuerySchema,
        parts: &[Ident],
        max: usize,
    ) -> R<Option<(u16, usize)>> {
        crate::probe::hit();
        let by_name = |count: usize| -> R<Option<u16>> {
            crate::probe::hit();
            let Some(parts) = parts.get(..count) else {
                return Ok(None);
            };
            let Some(id) = schema.find_table_managed(memory, parts)? else {
                return Ok(None);
            };
            Ok(self
                .by_table
                .get(&id)
                .copied()
                .filter(|&source| self.is_visible(source)))
        };
        if !self.by_table.is_empty() {
            for count in (2..=max.min(self.max_parts)).rev() {
                if let Some(source) = by_name(count)? {
                    return Ok(Some((source, count)));
                }
            }
        }
        if max == 0 {
            return Ok(None);
        }
        let Some(first) = parts.first() else {
            return Ok(None);
        };
        let key = mem::NameKey::folded(memory, &first.text)?;
        if let Some(&source) = self.by_alias.get(key.as_str())
            && self.is_visible(source)
        {
            return Ok(Some((source, 1)));
        }
        if self.by_table.is_empty() {
            return Ok(None);
        }
        Ok(by_name(1)?.map(|source| (source, 1)))
    }
}

impl<'scope> Analyzer<'scope> {
    pub(crate) fn declare_sources(&mut self, from: Option<&ast::FromClause>) -> R<Level<'scope>> {
        let mut level = Level::new(self.memory)?;
        let mut outer = mem::ScratchVec::new(self.memory, 0)?;
        if let Some(f) = from {
            for s in &f.sources {
                self.declare_source(&mut level, s, &mut outer)?;
            }
        }
        level.mark_nullable(self.memory, &outer)?;
        Ok(level)
    }

    fn declare_source(
        &mut self,
        level: &mut Level<'scope>,
        s: &ast::Source,
        outer: &mut mem::ScratchVec<(usize, usize)>,
    ) -> R<()> {
        let start = level.sources.len();
        if start > usize::from(u16::MAX) {
            return Err(self.unsupported("more than 65536 sources in one select", s.span.start));
        }
        let d = self.source_of(&s.table)?;
        self.declare_alias(level, s, d)?;
        for j in &s.joins {
            let r0 = level.sources.len();
            self.declare_source(level, &j.right, outer)?;
            let r1 = level.sources.len();
            match j.kind {
                JoinKind::Inner => {}
                JoinKind::Left => outer.push(self.memory, (r0, r1))?,
                JoinKind::Right => outer.push(self.memory, (start, r0))?,
                JoinKind::Full => outer.push(self.memory, (start, r1))?,
            }
        }
        Ok(())
    }

    fn declare_alias(
        &self,
        level: &mut Level<'scope>,
        s: &ast::Source,
        d: Declared<'scope>,
    ) -> R<()> {
        crate::probe::hit();
        let idx = u16::try_from(level.sources.len())
            .map_err(|_| self.unsupported("more than 65536 sources in one select", s.span.start))?;
        let (alias, at) = match &s.alias {
            Some(a) => (
                mem::NameKey::raw(self.memory, &a.name.text)?,
                Some(a.as_kw.unwrap_or(a.name.span).start),
            ),
            None => match d.default_alias {
                Some((text, at)) => (text, Some(at)),
                None => (mem::NameKey::raw(self.memory, "")?, None),
            },
        };
        if let Some(at) = at {
            let key = mem::NameKey::folded(self.memory, &alias)?;
            if level.by_alias.get(key.as_str()).is_some() {
                return Err(self.duplicated_alias(&alias, at));
            }
            level.by_alias.insert(self.memory, key, idx)?;
        }
        if s.alias.is_none()
            && let Some((id, n)) = d.table
        {
            level.by_table.insert(self.memory, id, idx)?;
            level.max_parts = level.max_parts.max(n);
        }
        level.sources.push(
            self.memory,
            ScopeSource {
                alias,
                cols: d.cols,
                nullable: false,
                from: d.from,
                on: None,
                vt_check: d.vt_check,
                dim_res_used: false,
            },
        )?;
        Ok(())
    }

    fn source_of(&mut self, t: &ast::TableRef) -> R<Declared<'scope>> {
        match t {
            ast::TableRef::Named { path, args } => self.named_source(path, args.as_ref()),
            ast::TableRef::Subquery { query, span } => {
                let q = self.subquery_owned(query)?;
                let cols = self.query_columns(q.trusted_borrow())?.ok_or_else(|| {
                    self.unsupported("more than 65536 subquery columns", span.start)
                })?;
                Ok(Declared {
                    cols: SourceCols::Columns(cols),
                    from: Some(mem::query::subquery(self.memory, q)?),
                    default_alias: None,
                    table: None,
                    vt_check: None,
                })
            }
            ast::TableRef::Param(p) => self.param_source(p),
        }
    }

    fn named_source(
        &mut self,
        path: &ast::Path,
        args: Option<&ast::VtArgs>,
    ) -> R<Declared<'scope>> {
        let parts = &path.parts;
        let start = path.span().start;
        let default_alias = path
            .parts
            .last()
            .map(|p| mem::NameKey::raw(self.memory, &p.text).map(|text| (text, start)))
            .transpose()?;
        if let [one] = parts.as_slice()
            && let Some(t) = self.temps.get(&one.text)?
        {
            self.note_read(&one.text)?;
            let cols = Columns::of_temp(self.memory, &t, false)?.ok_or_else(|| {
                self.unsupported("more than 65536 temporary table columns", start)
            })?;
            let cols = if t.open {
                SourceCols::Open {
                    param: mem::NameKey::raw(self.memory, &t.name)?,
                    cols,
                }
            } else {
                SourceCols::Columns(cols)
            };
            return Ok(Declared {
                cols,
                from: Some(mem::owned_copy(self.memory, &t)?.map(tir::From::Temp)),
                default_alias,
                table: None,
                vt_check: None,
            });
        }
        let Some(id) = self.schema.find_table_managed(self.memory, parts)? else {
            return Err(self.table_not_found(path));
        };
        let (periodicity, complement, args, breakdown, vt_check) = match self.schema.table(id).kind
        {
            TableKind::Virtual(_) if self.rls => {
                return Err(self.err(
                    MsgId::SELECT_ERROR_CANTUSEVIRTUALTABLEINSECURITYINSTRUCTION,
                    start,
                ));
            }
            TableKind::Virtual(_) => {
                let v = self.vt_source(id, path, args)?;
                (v.periodicity, v.complement, v.args, v.breakdown, v.check)
            }
            _ => (
                Periodicity::Absent,
                None,
                mem::NativeVec::new(self.memory, 0)?.finish(),
                mem::NativeVec::new(self.memory, 0)?.finish(),
                None,
            ),
        };
        let extra = self.breakdown_columns(id, breakdown.trusted_borrow())?;
        Ok(Declared {
            cols: SourceCols::Table {
                id,
                periodicity,
                scope: FieldScope::All,
                extra,
            },
            from: Some(
                mem::allocation_result(self.memory, args.zip(breakdown))?.map(
                    |(args, breakdown)| tir::From::Table {
                        id,
                        args,
                        periodicity,
                        complement,
                        breakdown,
                    },
                ),
            ),
            default_alias,
            table: Some((id, parts.len())),
            vt_check,
        })
    }

    fn param_source(&mut self, p: &ast::Param) -> R<Declared<'scope>> {
        let name = &*p.name.text;
        self.param_table.get_or_insert(p.span.start);
        if self.nested() {
            self.param_nested = true;
        }
        self.note_table_param(name)?;
        let declared = |cols, from| Declared {
            cols,
            from,
            default_alias: None,
            table: None,
            vt_check: None,
        };
        let shape = self.params.shape(name)?;
        match (shape.as_deref(), self.opts.mode) {
            (Some(ParamShape::Table(t)), _) => {
                let cols = Columns::of_temp(self.memory, t, true)?.ok_or_else(|| {
                    self.unsupported("more than 65536 parameter table columns", p.span.start)
                })?;
                Ok(declared(
                    SourceCols::Columns(cols),
                    Some(
                        mem::allocation_result(
                            self.memory,
                            mem::OwnedString::new(self.memory, name)?
                                .into_native_owned()
                                .zip(mem::owned_copy(
                                    self.memory,
                                    &crate::QShared::from(Arc::clone(t)),
                                )?),
                        )?
                        .map(|(name, schema)| tir::From::Param { name, schema }),
                    ),
                ))
            }
            (_, AnalyzeMode::Static) => Ok(declared(
                SourceCols::Open {
                    param: mem::NameKey::raw(self.memory, name)?,
                    cols: Columns::new(self.memory)?,
                },
                None,
            )),
            (None, AnalyzeMode::Execute) => Err(self.quoted_text_error(
                MsgId::SELECT_ERROR_PARAMETERVALUENOTFOUND,
                name,
                p.span.start,
            )),
            (Some(_), AnalyzeMode::Execute) => {
                Err(self.quoted_text_error(MsgId::SELECT_ERROR_WRONGPARAMETER, name, p.span.start))
            }
        }
    }

    fn query_columns(&self, query: &tir::Query) -> R<Option<Columns>> {
        let mut columns = Columns::new(self.memory)?;
        let Some((first, rest)) = query.parts.split_first() else {
            return Ok(Some(columns));
        };
        for (index, item) in first.items.iter().enumerate() {
            let mut ty = mem::ScopeType::copy(self.memory, &item.expr.ty)?;
            for other in rest.iter().filter_map(|part| part.items.get(index)) {
                ty = ty.merge(self.memory, &other.expr.ty)?;
            }
            let unknown = item.unknown
                || rest
                    .iter()
                    .any(|part| part.items.get(index).is_some_and(|item| item.unknown));
            let known = if unknown {
                ColKnown::Unknown
            } else {
                ColKnown::Typed
            };
            let name = mem::NameKey::raw(self.memory, &item.name)?;
            if columns.push_owned(self.memory, name, ty, known)?.is_none() {
                return Ok(None);
            }
        }
        Ok(Some(columns))
    }

    pub(crate) fn note_read(&mut self, name: &str) -> R<()> {
        let key = mem::NameKey::folded(self.memory, name)?;
        if self.read_set.get(key.as_str()).is_none() {
            let read = mem::NameKey::raw(self.memory, &key)?;
            self.read_set.insert(self.memory, key, ())?;
            self.reads.push(self.memory, read)?;
        }
        Ok(())
    }

    pub(crate) fn join_conditions(&mut self, from: &ast::FromClause) -> R<()> {
        let mut next = 0usize;
        let saved = self.levels.last().and_then(|l| l.visible);
        let r = from
            .sources
            .iter()
            .try_for_each(|s| self.join_conditions_of(s, &mut next));
        self.set_visible(saved);
        r
    }

    fn join_conditions_of(&mut self, s: &ast::Source, next: &mut usize) -> R<()> {
        let start = *next;
        *next += 1;
        for j in &s.joins {
            let right = *next;
            self.join_conditions_of(&j.right, next)?;
            self.set_visible(Some((start, *next)));
            let on = self.condition_owned(&j.on, Clause::On, Kw::On)?;
            match self.current_source_mut(right) {
                Some(src) => src.on = Some(on),
                None => return Err(self.unsupported("join source out of range", j.kw.start)),
            }
        }
        Ok(())
    }

    fn set_visible(&mut self, range: Option<(usize, usize)>) {
        if let Some(l) = self.levels.last_mut() {
            l.visible = range;
        }
    }

    pub(crate) fn assemble_sources(
        &self,
        level: &mut Level<'scope>,
        from: &ast::FromClause,
    ) -> R<mem::NativeOwned<'scope, Vec<tir::Source>>> {
        let mut next = 0usize;
        let mut out = mem::NativeVec::new(self.memory, from.sources.len())?;
        for s in &from.sources {
            out.push_owned(self.assemble_source(level, s, &mut next)?)?;
        }
        Ok(out.finish())
    }

    fn assemble_source(
        &self,
        level: &mut Level<'scope>,
        s: &ast::Source,
        next: &mut usize,
    ) -> R<mem::NativeOwned<'scope, tir::Source>> {
        let idx = *next;
        *next += 1;
        let Some(src) = level.sources.get_mut(idx) else {
            return Err(self.unsupported("source out of range", s.span.start));
        };
        let alias = mem::OwnedString::new(self.memory, &src.alias)?.into_native_owned();
        let from = match (src.from.take(), &src.cols) {
            (Some(f), _) => f,
            (None, SourceCols::Open { param, cols }) => {
                let mut columns = mem::NativeVec::new(self.memory, cols.names.len())?;
                for name in &cols.names {
                    columns.push_owned(
                        mem::OwnedString::new(self.memory, name)?
                            .into_native_owned()
                            .map(|name| TempColumn {
                                name,
                                ty: None,
                                unknown: true,
                                stored: None,
                            }),
                    )?;
                }
                let name = mem::OwnedString::new(self.memory, param)?.into_native_owned();
                let schema = mem::allocation_result(self.memory, name.zip(columns.finish()))?.map(
                    |(name, columns)| TempSchema {
                        name,
                        columns,
                        open: true,
                    },
                );
                let schema = mem::share_owned(self.memory, schema)?;
                mem::OwnedString::new(self.memory, param)?
                    .into_native_owned()
                    .map(|name| tir::From::Param { name, schema })
            }
            (None, _) => return Err(self.unsupported("source taken twice", s.span.start)),
        };
        let mut joins = mem::NativeVec::new(self.memory, s.joins.len())?;
        for j in &s.joins {
            let r = *next;
            let right = self.assemble_source(level, &j.right, next)?;
            let Some(on) = level.sources.get_mut(r).and_then(|x| x.on.take()) else {
                return Err(self.unsupported("join condition missing", j.kw.start));
            };
            joins.push_owned(mem::query::join(self.memory, j.kind, right, on)?)?;
        }
        mem::query::source(self.memory, alias, from, joins.finish())
    }

    fn current_source_mut(&mut self, idx: usize) -> Option<&mut ScopeSource<'scope>> {
        self.levels.last_mut()?.sources.get_mut(idx)
    }

    fn field_path_error(&self, parts: &[Ident], last: usize, at: u32) -> crate::CompileError {
        if let Some(memory) = self.memory {
            return memory.quoted_parts_error(
                MsgId::SELECT_ERROR_FIELDNOTFOUND,
                self.map.at(self.text, at),
                parts.iter().take(last.saturating_add(1)).map(|p| &*p.text),
                ".",
            );
        }
        self.field_not_found(&path_prefix(parts, last), at)
    }
    pub(crate) fn path_expr_owned(&mut self, path: &ast::Path) -> R<mem::ExprValue<'scope>> {
        if let Some(e) = self.column_hit_owned(path)? {
            return Ok(e);
        }
        let parts = &path.parts;
        let span = path.span();
        let Some(first) = parts.first() else {
            return Err(self.unsupported("empty path", span.start));
        };
        if parts.len() == 1 && first.text.starts_with('.') {
            return Err(if self.levels.iter().any(|l| !l.sources.is_empty()) {
                self.field_not_found(&first.text, first.span.start)
            } else {
                self.err(MsgId::SELECT_ERROR_TABLENOTFOUND, first.span.start)
            });
        }
        let (found, next) = match self.source_by_prefix(parts)? {
            Some((li, si, k)) => {
                let Some(seg) = parts.get(k) else {
                    return Err(self.unsupported("path without a field", span.start));
                };
                let key = mem::NameKey::folded(self.memory, &seg.text)?;
                match self.field_of(li, si, &key, &seg.text, true)? {
                    Some(f) => (f, k + 1),
                    None => {
                        return Err(self.field_path_error(parts, k, seg.span.start));
                    }
                }
            }
            None => match self.bare_field(first)? {
                Some(f) => (f, 1),
                None => {
                    let at = if parts.len() == 1 && !first.text.contains('#') {
                        bare_name_at(&self.frames, first.span)
                    } else {
                        span.start
                    };
                    return Err(self.field_path_error(parts, parts.len(), at));
                }
            },
        };
        let found_ty = mem::ScratchType::new(self.memory, found.ty);
        if found.no_deref
            && let Some(seg) = parts.get(next)
        {
            return Err(self.field_path_error(parts, next, seg.span.start));
        }
        let (derefs, ty) = if found.unknown {
            let mut derefs = mem::OwnedDerefs::new(self.memory, parts.len().saturating_sub(next))?;
            for seg in parts.iter().skip(next) {
                derefs.push(&seg.text, &[])?;
            }
            drop(found_ty);
            (
                derefs,
                mem::ScratchType::new(self.memory, QType::undefined()),
            )
        } else {
            self.derefs_owned(found_ty, found.nested, parts, next)?
        };
        mem::ExprValue::compose(
            self.memory,
            mem::ScalarKind::Field {
                level: found.level,
                source: found.source,
                col: found.col,
                derefs,
            },
            ty,
            span,
        )
    }

    fn source_by_prefix(&self, parts: &[Ident]) -> R<Option<(usize, u16, usize)>> {
        let Some(max) = parts.len().checked_sub(1) else {
            return Ok(None);
        };
        for (index, level) in self.levels.iter().enumerate().rev() {
            if let Some((source, count)) =
                level.find_source(self.memory, self.schema, parts, max)?
            {
                return Ok(Some((index, source, count)));
            }
        }
        Ok(None)
    }

    fn bare_field(&mut self, name: &Ident) -> R<Option<FieldRef>> {
        let key = mem::NameKey::folded(self.memory, &name.text)?;
        for li in (0..self.levels.len()).rev() {
            let Some(level) = self.levels.get(li) else {
                continue;
            };
            let mut found = None;
            let mut open = None;
            let mut opens = 0usize;
            let (a, b) = level.visible_range();
            let visible = level.sources.iter().enumerate().take(b).skip(a);
            for (i, s) in visible {
                if matches!(s.cols, SourceCols::Open { .. }) {
                    opens += 1;
                    open = Some(i);
                } else if self.has_field(s, &key) {
                    if found.is_some() {
                        return Err(self.ambiguous_field(name));
                    }
                    found = Some(i);
                }
            }
            let (pick, allow_open) = match (found, open) {
                (Some(i), _) => (i, false),
                (None, Some(i)) if opens == 1 && !name.text.contains('#') => (i, true),
                _ => continue,
            };
            let si = u16::try_from(pick)
                .map_err(|_| self.unsupported("source out of range", name.span.start))?;
            return self.field_of(li, si, &key, &name.text, allow_open);
        }
        Ok(None)
    }

    fn has_field(&self, s: &ScopeSource, key: &str) -> bool {
        match &s.cols {
            SourceCols::Table {
                id,
                periodicity,
                scope,
                extra,
            } => {
                self.schema
                    .find_field_folded(*id, key)
                    .and_then(|f| self.schema.field(*id, f))
                    .is_some_and(|qf| {
                        qf.period().is_none_or(|pf| pf.selected_by(*periodicity))
                            && scope_sees(self.schema, *id, *scope, qf)
                    })
                    || extra_index(extra, key).is_some()
            }
            SourceCols::Columns(c) => c.find(key).is_some(),
            SourceCols::Open { .. } => false,
        }
    }

    fn field_of(
        &mut self,
        li: usize,
        si: u16,
        key: &str,
        name: &str,
        open: bool,
    ) -> R<Option<FieldRef>> {
        let schema = self.schema;
        let Some(distance) = self.levels.len().checked_sub(li + 1) else {
            return Ok(None);
        };
        let Ok(level) = u16::try_from(distance) else {
            return Ok(None);
        };
        let Some(src) = self
            .levels
            .get_mut(li)
            .and_then(|level| level.sources.get_mut(usize::from(si)))
        else {
            return Ok(None);
        };
        let mut unknown = false;
        let mut no_deref = false;
        let (col, ty, nested) = match &mut src.cols {
            SourceCols::Table {
                id,
                periodicity,
                scope,
                extra,
            } => match schema.find_field_folded(*id, key).and_then(|f| {
                let qf = schema.field(*id, f)?;
                let visible = qf.period().is_none_or(|pf| pf.selected_by(*periodicity))
                    && scope_sees(schema, *id, *scope, qf);
                visible.then_some((f, qf))
            }) {
                Some((f, qf)) => {
                    src.dim_res_used = true;
                    no_deref = *scope == FieldScope::Rls && qf.role != FieldRole::Dimension;
                    let ty = if qf.only == Only::Any {
                        mem::copy(self.memory, &qf.ty)?
                    } else {
                        mem::copy(self.memory, &qf.ty)?.with_null()
                    };
                    (Col::Field(f), ty, qf.nested)
                }
                None => {
                    let Some(i) = extra_index(extra, key) else {
                        return Ok(None);
                    };
                    let Some(x) = extra.get(usize::from(i)) else {
                        return Ok(None);
                    };
                    (Col::Column(i), mem::copy(self.memory, &*x.ty)?, None)
                }
            },
            SourceCols::Columns(c) => {
                let Some(i) = c.find(key) else {
                    return Ok(None);
                };
                unknown = c.known_at(i) == ColKnown::Unknown;
                let Some(ty) = c.types.get(usize::from(i)) else {
                    return Ok(None);
                };
                (Col::Column(i), mem::copy(self.memory, &**ty)?, None)
            }
            SourceCols::Open { cols, .. } => {
                let i = match cols.find(key) {
                    Some(i) => i,
                    None if open => {
                        let Some(index) = cols.push_known(
                            self.memory,
                            name,
                            &QType::undefined(),
                            ColKnown::Unknown,
                        )?
                        else {
                            return Ok(None);
                        };
                        index
                    }
                    None => return Ok(None),
                };
                unknown = true;
                (Col::Column(i), QType::undefined(), None)
            }
        };
        let ty = if src.nullable { ty.with_null() } else { ty };
        Ok(Some(FieldRef {
            level,
            source: si,
            col,
            ty,
            nested,
            unknown,
            no_deref,
        }))
    }

    pub(super) fn derefs_owned(
        &self,
        ty: mem::ScratchType<'scope>,
        nested: Option<QTableId>,
        parts: &[Ident],
        from: usize,
    ) -> R<(mem::OwnedDerefs<'scope>, mem::ScratchType<'scope>)> {
        let schema = self.schema;
        let (mut ty, mut nested) = (ty, nested);
        let mut out = mem::OwnedDerefs::new(self.memory, parts.len().saturating_sub(from))?;
        for (i, seg) in parts.iter().enumerate().skip(from) {
            let key = mem::NameKey::folded(self.memory, &seg.text)?;
            let mut targets = mem::ScratchVec::new(self.memory, 0)?;
            let tables = ty
                .refs
                .iter()
                .map(|r| schema.ref_info(*r).table)
                .chain(nested);
            for t in tables {
                if let Some(f) = schema.find_field_folded(t, &key)
                    && !targets.contains(&(t, f))
                {
                    targets.push(self.memory, (t, f))?;
                }
            }
            if targets.is_empty() {
                return Err(self.field_path_error(parts, i, seg.span.start));
            }
            let mut step = mem::ScratchType::new(self.memory, QType::undefined());
            let mut step_nested = None;
            for (t, f) in &targets {
                if let Some(qf) = schema.field(*t, *f) {
                    step = mem::type_merge(self.memory, &step, &qf.ty)?;
                    step_nested = step_nested.or(qf.nested);
                }
            }
            ty = step.with_null();
            nested = step_nested;
            out.push(&seg.text, &targets)?;
        }
        Ok((out, ty))
    }

    pub(crate) fn star_items(&mut self, span: Span) -> R<mem::FieldItems<'scope>> {
        let mut out = mem::FieldItems::new(self.memory);
        let n = self.levels.last().map_or(0, |l| l.sources.len());
        for si in 0..n {
            out.append(self.source_items(si, span)?)?;
            self.mark_all_used(si);
        }
        Ok(out)
    }

    fn mark_all_used(&mut self, si: usize) {
        if let Some(src) = self.levels.last_mut().and_then(|l| l.sources.get_mut(si)) {
            src.dim_res_used = true;
        }
    }

    pub(crate) fn path_star_items(&mut self, path: &ast::Path) -> R<mem::FieldItems<'scope>> {
        let parts = &path.parts;
        let found = match self.levels.last() {
            Some(level) => level.find_source(self.memory, self.schema, parts, parts.len())?,
            None => None,
        }
        .filter(|(_, count)| *count == parts.len());
        let Some((si, _)) = found else {
            return Err(self.quoted_path_error(
                MsgId::SELECT_ERROR_FIELDNOTFOUND,
                path,
                path.span().start,
            ));
        };
        let out = self.source_items(usize::from(si), path.span())?;
        self.mark_all_used(usize::from(si));
        Ok(out)
    }

    fn source_items(&self, si: usize, span: Span) -> R<mem::FieldItems<'scope>> {
        let mut out = mem::FieldItems::new(self.memory);
        let Some(src) = self.levels.last().and_then(|l| l.sources.get(si)) else {
            return Ok(out);
        };
        let source =
            u16::try_from(si).map_err(|_| self.unsupported("source out of range", span.start))?;
        let item = |col: Col, ty: &QType, name: &str| -> R<mem::FieldItem<'scope>> {
            let mut item = mem::FieldItem::new(self.memory, source, col, ty, name, span)?;
            item.set_nullable(src.nullable);
            item.set_unknown(match (&src.cols, col) {
                (SourceCols::Columns(c) | SourceCols::Open { cols: c, .. }, Col::Column(i)) => {
                    c.known_at(i) == ColKnown::Unknown
                }
                _ => false,
            });
            Ok(item)
        };
        match &src.cols {
            SourceCols::Table {
                id,
                periodicity,
                extra,
                ..
            } => {
                for f in self.schema.vt_fields_iter(*id, *periodicity) {
                    let Some(qf) = self.schema.field(*id, f) else {
                        continue;
                    };
                    let name = match self.schema.script() {
                        ScriptVariant::Russian => qf.name_ru.as_str(),
                        ScriptVariant::English => qf.name_en.as_str(),
                    };
                    let mut it = item(Col::Field(f), &qf.ty, name)?;
                    it.set_nullable(qf.only != Only::Any);
                    if let Some(t) = qf.nested {
                        it.set_nested(self.section_items(t, span)?)?;
                    }
                    out.push(it)?;
                }
                for (i, x) in extra.iter().enumerate() {
                    let i = u16::try_from(i)
                        .map_err(|_| self.unsupported("column out of range", span.start))?;
                    let name = match self.schema.script() {
                        ScriptVariant::Russian => x.name_ru.as_str(),
                        ScriptVariant::English => x.name_en.as_str(),
                    };
                    out.push(item(Col::Column(i), &x.ty, name)?)?;
                }
            }
            SourceCols::Columns(c) | SourceCols::Open { cols: c, .. } => {
                for (i, (name, ty)) in c.names.iter().zip(&c.types).enumerate() {
                    let i = u16::try_from(i)
                        .map_err(|_| self.unsupported("column out of range", span.start))?;
                    out.push(item(Col::Column(i), ty, name)?)?;
                }
            }
        }
        Ok(out)
    }

    pub(crate) fn subquery_owned(
        &mut self,
        q: &ast::Query,
    ) -> R<mem::NativeOwned<'scope, tir::Query>> {
        self.frames.push(self.memory, Frame::Subquery)?;
        let saved = self.columns.take();
        let r = self.query_owned(q);
        self.columns = saved;
        self.frames.pop();
        r
    }
}
