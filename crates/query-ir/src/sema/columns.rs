use super::scope::{ColKnown, Level, SourceCols};
use super::select;
use super::{Analyzer, ColumnInfo, R};
use crate::AnalyzeMode;
use crate::managed::{self as mem, CompileArena};
use crate::schema::QTableId;
use crate::temp::{TempColumn, TempSchema};
use crate::tir::{self, Col, Deref, TExpr, TKind};
use crate::types::{DateQ, NumQ, Other, QType, StrQ};
use query_syntax::ast::{self, ExprKind, ItemKind};
use query_syntax::{MsgId, ScriptVariant, Span, kw::Func};

#[derive(Default)]
pub(crate) struct Names {
    seen: mem::Index<mem::NameKey, ()>,
    suffix: mem::Index<mem::NameKey, u32>,
    unnamed: u32,
}

impl Names {
    fn alias(&mut self, memory: Option<&CompileArena>, name: &str) -> R<mem::NameKey> {
        self.seen
            .insert(memory, mem::NameKey::folded(memory, name)?, ())?;
        mem::NameKey::raw(memory, name)
    }

    fn derived(&mut self, memory: Option<&CompileArena>, name: &str) -> R<mem::NameKey> {
        let base = mem::NameKey::folded(memory, name)?;
        if self.seen.get(base.as_str()).is_none() {
            self.seen.insert(memory, base, ())?;
            return mem::NameKey::raw(memory, name);
        }
        let mut number = self.suffix.get(base.as_str()).copied().unwrap_or(0);
        loop {
            crate::probe::hit();
            let Some(next) = number.checked_add(1) else {
                return mem::allocation_error(memory, memory_budget::BudgetError::Overflow);
            };
            number = next;
            let candidate = mem::NameKey::numbered(memory, name, number)?;
            let key = mem::NameKey::folded(memory, &candidate)?;
            if self.seen.get(key.as_str()).is_none() {
                self.seen.insert(memory, key, ())?;
                self.suffix.insert(memory, base, number)?;
                return Ok(candidate);
            }
        }
    }
}

impl<'scope> Analyzer<'scope> {
    pub(crate) fn expression_item(
        &mut self,
        it: &ast::SelectItem,
        expression: &ast::Expr,
        names: &mut Names,
    ) -> R<mem::ExprItem<'scope>> {
        let expr = self.owned_expr(expression)?;
        self.owned_expr_item(it, expression, expr, names)
    }

    fn owned_expr_item(
        &self,
        it: &ast::SelectItem,
        expression: &ast::Expr,
        expr: mem::OwnedExpr<'scope>,
        names: &mut Names,
    ) -> R<mem::ExprItem<'scope>> {
        self.check_selectable(&expr)?;
        let key = self.item_name_key(it, expression, &expr, names)?;
        let name = mem::OwnedString::new(self.memory, &key)?;
        let unknown = self.expr_unknown(&expr)?
            || (self.opts.mode == AnalyzeMode::Static && unknown_parameter_result(&expr));
        let nested = self
            .nested_table(&expr)
            .map(|t| self.section_items(t, expression.span))
            .transpose()?;
        let item = mem::ExprItem::new(expr, name, unknown)?;
        match nested {
            Some(nested) => item.with_nested(nested),
            None => Ok(item),
        }
    }

    pub(crate) fn field_select_items(
        &mut self,
        item: &ast::SelectItem,
        names: &mut Names,
    ) -> R<mem::FieldItems<'scope>> {
        let fields = match &item.kind {
            ItemKind::Star => self.star_items(item.span)?,
            ItemKind::PathStar(path) => self.path_star_items(path)?,
            _ => return Err(self.unsupported("field select item", item.span.start)),
        };
        self.renamed(fields, names)
    }

    pub(crate) fn item_owned(
        &mut self,
        it: &ast::SelectItem,
        out: &mut mem::SelectItems<'scope>,
        names: &mut Names,
    ) -> R<()> {
        match &it.kind {
            ItemKind::Star | ItemKind::PathStar(_) => {
                out.append_fields(self.field_select_items(it, names)?)?
            }
            ItemKind::Expr(e) => out.push(self.expression_item(it, e, names)?)?,
            ItemKind::Nested { table, fields } => {
                let expr = self.owned_expr(table)?;
                let tid = self.nested_table(&expr);
                if tid.is_none() && !self.expr_unknown(&expr)? {
                    return Err(self.unsupported(
                        "a nested table list needs a tabular section",
                        table.span.start,
                    ));
                }
                let key = self.item_name_key(it, table, &expr, names)?;
                let name = mem::OwnedString::new(self.memory, &key)?;
                let mut inner = mem::SelectItems::new(
                    self.memory,
                    if tid.is_some() { fields.len() } else { 0 },
                )?;
                if let Some(tid) = tid {
                    self.levels
                        .push(self.memory, Level::of_table(self.memory, tid)?)?;
                    let mut inner_names = Names::default();
                    let r = fields
                        .iter()
                        .try_for_each(|field| self.item_owned(field, &mut inner, &mut inner_names));
                    self.levels.pop();
                    r?;
                }
                out.push(
                    mem::ExprItem::new(expr, name, tid.is_none())?
                        .with_nested_owned(inner.into_native_owned())?,
                )?;
            }
            ItemKind::EmptyTable { kw, names: cols } => {
                let key = match &it.alias {
                    Some(a) => names.alias(self.memory, &a.name.text)?,
                    None => self.unnamed_key(names)?,
                };
                let name = mem::OwnedString::new(self.memory, &key)?;
                let null = |ty: QType| -> R<mem::OwnedExpr<'scope>> {
                    let expr = mem::OwnedExpr::new(self.memory, &ast::Lit::Null, *kw)?
                        .ok_or_else(|| self.unsupported("null literal", kw.start))?;
                    let expr = mem::ExprValue::owned(expr).replace_type(
                        self.memory,
                        mem::ScratchType::new(self.memory, ty),
                        *kw,
                    )?;
                    Ok(expr.into_owned())
                };
                let mut inner = mem::SelectItems::new(self.memory, cols.len())?;
                for col in cols {
                    let name = mem::OwnedString::new(
                        self.memory,
                        col.as_ref().map(|i| &*i.text).unwrap_or_default(),
                    )?;
                    inner.push(mem::ExprItem::new(null(QType::undefined())?, name, false)?)?;
                }
                out.push(
                    mem::ExprItem::new(
                        null(mem::other(self.memory, Other::QueryResult)?)?,
                        name,
                        false,
                    )?
                    .with_nested_owned(inner.into_native_owned())?,
                )?;
            }
        }
        Ok(())
    }

    pub(crate) fn expr_unknown(&self, e: &TExpr) -> R<bool> {
        let mut stack = mem::ScratchVec::new(self.memory, 1)?;
        stack.push(self.memory, e)?;
        while let Some(e) = stack.pop() {
            match &e.kind {
                TKind::Cast { .. } => {}
                TKind::Field {
                    level,
                    source,
                    col,
                    derefs,
                } => {
                    if derefs.iter().any(|d| d.targets.is_empty()) {
                        return Ok(true);
                    }
                    let Col::Column(i) = col else { continue };
                    let src = self
                        .levels
                        .len()
                        .checked_sub(1 + usize::from(*level))
                        .and_then(|at| self.levels.get(at))
                        .and_then(|l| l.sources.get(usize::from(*source)));
                    if let Some(SourceCols::Columns(c) | SourceCols::Open { cols: c, .. }) =
                        src.map(|s| &s.cols)
                        && c.known_at(*i) == ColKnown::Unknown
                    {
                        return Ok(true);
                    }
                }
                _ => {
                    let mut result = Ok(());
                    select::each_child(e, &mut |child| {
                        if result.is_ok() {
                            result = stack.push(self.memory, child);
                        }
                    });
                    result?;
                }
            }
        }
        Ok(false)
    }

    pub(crate) fn is_untyped_column(&self, e: &TExpr) -> bool {
        let TKind::Field {
            level: 0,
            source,
            col: Col::Column(i),
            derefs,
        } = &e.kind
        else {
            return false;
        };
        let src = self
            .levels
            .last()
            .and_then(|l| l.sources.get(usize::from(*source)));
        matches!(
            src.map(|s| &s.cols),
            Some(SourceCols::Columns(c)) if derefs.is_empty() && c.known_at(*i) == ColKnown::Untyped
        )
    }

    fn check_selectable(&self, e: &TExpr) -> R<()> {
        if self.is_untyped_column(e) {
            return Err(self.err(MsgId::SELECT_ERROR_VALUECANTBESELECTED, e.span.start));
        }
        Ok(())
    }

    pub(crate) fn section_items(&self, t: QTableId, span: Span) -> R<mem::FieldItems<'scope>> {
        let tab = self.schema.table(t);
        let mut fields = mem::FieldItems::new(self.memory);
        for f in &tab.star {
            let Some(qf) = self.schema.field(t, *f) else {
                continue;
            };
            let Some(name) = self.script_name(t, *f) else {
                continue;
            };
            fields.push(mem::FieldItem::new(
                self.memory,
                0,
                Col::Field(*f),
                &qf.ty,
                name,
                span,
            )?)?;
        }
        Ok(fields)
    }

    fn renamed(
        &self,
        mut fields: mem::FieldItems<'scope>,
        names: &mut Names,
    ) -> R<mem::FieldItems<'scope>> {
        fields.rename_with(|name| names.derived(self.memory, name))?;
        Ok(fields)
    }

    fn unnamed_key(&self, names: &mut Names) -> R<mem::NameKey> {
        let Some(next) = names.unnamed.checked_add(1) else {
            return mem::allocation_error(self.memory, memory_budget::BudgetError::Overflow);
        };
        names.unnamed = next;
        let word = match self.schema.script() {
            ScriptVariant::Russian => "Поле",
            ScriptVariant::English => "Field",
        };
        let base = mem::NameKey::numbered(self.memory, word, names.unnamed)?;
        names.derived(self.memory, &base)
    }

    fn item_name_key(
        &self,
        it: &ast::SelectItem,
        e: &ast::Expr,
        expr: &TExpr,
        names: &mut Names,
    ) -> R<mem::NameKey> {
        if let Some(a) = &it.alias {
            return names.alias(self.memory, &a.name.text);
        }
        let own = match (&e.kind, &expr.kind) {
            (ExprKind::Path(p), _) => match self.field_name(expr)? {
                Some(name) => Some(name),
                None => p
                    .parts
                    .last()
                    .map(|part| mem::NameKey::raw(self.memory, &part.text))
                    .transpose()?,
            },
            (ExprKind::Func { .. }, TKind::Agg { arg: Some(a), .. }) => self.field_name(a)?,
            _ => None,
        };
        match own {
            Some(n) if !n.is_empty() => names.derived(self.memory, &n),
            _ => self.unnamed_key(names),
        }
    }

    fn field_name(&self, e: &TExpr) -> R<Option<mem::NameKey>> {
        let TKind::Field {
            level,
            source,
            col,
            derefs,
        } = &e.kind
        else {
            return Ok(None);
        };
        let Some(src) = self
            .levels
            .len()
            .checked_sub(1 + usize::from(*level))
            .and_then(|at| self.levels.get(at))
            .and_then(|level| level.sources.get(usize::from(*source)))
        else {
            return Ok(None);
        };
        let base = match (&src.cols, col) {
            (SourceCols::Table { id, .. }, Col::Field(f)) => {
                let Some(name) = self.script_name(*id, *f) else {
                    return Ok(None);
                };
                name
            }
            (SourceCols::Columns(c) | SourceCols::Open { cols: c, .. }, Col::Column(i)) => {
                let Some(name) = c.names.get(usize::from(*i)) else {
                    return Ok(None);
                };
                name.as_str()
            }
            _ => return Ok(None),
        };
        let omit_base = !derefs.is_empty()
            && match (&src.cols, col) {
                (SourceCols::Table { id, .. }, Col::Field(f)) => {
                    self.schema.table(*id).kind == crate::schema::TableKind::Tabular
                        && self.schema.field(*id, *f).is_some_and(|field| {
                            field.role == crate::schema::FieldRole::Std(crate::schema::StdRole::Ref)
                        })
                }
                _ => false,
            };
        let mut out = mem::NameKey::raw(self.memory, if omit_base { "" } else { base })?;
        for d in derefs {
            out.push_str(self.memory, self.deref_name(d))?;
        }
        Ok(Some(out))
    }

    fn script_name(&self, t: QTableId, f: crate::schema::QFieldId) -> Option<&str> {
        let qf = self.schema.field(t, f)?;
        Some(match self.schema.script() {
            ScriptVariant::Russian => &qf.name_ru,
            ScriptVariant::English => &qf.name_en,
        })
    }

    fn deref_name<'a>(&'a self, d: &'a Deref) -> &'a str {
        d.targets
            .first()
            .and_then(|(t, f)| self.script_name(*t, *f))
            .unwrap_or(&d.name)
    }

    pub(super) fn nested_table(&self, e: &TExpr) -> Option<QTableId> {
        let by_target = |d: &Deref| {
            d.targets
                .iter()
                .find_map(|(t, f)| self.schema.field(*t, *f)?.nested)
        };
        match &e.kind {
            TKind::Field { derefs, .. } | TKind::Cast { derefs, .. } if !derefs.is_empty() => {
                derefs.last().and_then(by_target)
            }
            TKind::Field {
                level,
                source,
                col: Col::Field(f),
                ..
            } => {
                let at = self.levels.len().checked_sub(1 + usize::from(*level))?;
                let src = self.levels.get(at)?.sources.get(usize::from(*source))?;
                match &src.cols {
                    SourceCols::Table { id, .. } => self.schema.field(*id, *f)?.nested,
                    _ => None,
                }
            }
            _ => None,
        }
    }

    fn column_type(&self, ty: &QType) -> QType {
        if ty.is_undefined() || is_query_result(ty) {
            ty.clone()
        } else {
            ty.clone().with_null()
        }
    }

    fn column_info(&self, name: &str, ty: &QType, nested: Option<&[tir::Item]>) -> ColumnInfo {
        let ty = self.column_type(ty);
        ColumnInfo {
            name: name.to_string(),
            width: ty.width(),
            ty,
            nested: nested.map(|n| {
                n.iter()
                    .map(|i| self.column_info(&i.name, &i.expr.ty, i.nested.as_deref()))
                    .collect()
            }),
        }
    }

    pub(crate) fn result_columns(&self, q: &tir::Query) -> Vec<ColumnInfo> {
        let Some(first) = q.parts.first() else {
            return Vec::new();
        };
        first
            .items
            .iter()
            .zip(select::column_types(q))
            .map(|(it, ty)| self.column_info(&it.name, &ty, it.nested.as_deref()))
            .collect()
    }

    pub(crate) fn count_column(&self) -> ColumnInfo {
        let name = match self.schema.script() {
            ScriptVariant::Russian => "Количество",
            ScriptVariant::English => "Count",
        };
        ColumnInfo {
            name: name.into(),
            ty: QType::num(NumQ {
                digits: 0,
                scale: 0,
                nonneg: false,
            }),
            width: 10,
            nested: None,
        }
    }
}

fn is_query_result(t: &QType) -> bool {
    !t.null && t.atom_count() == 1 && t.others == [Other::QueryResult]
}

pub(crate) fn temp_schema(name: String, q: &tir::Query, open: bool) -> TempSchema {
    let columns = q
        .parts
        .first()
        .map(|p| {
            p.items
                .iter()
                .zip(select::column_types(q))
                .enumerate()
                .map(|(idx, (it, ty))| {
                    let stored = ty.clone().without_null();
                    let stored = if ty.null && stored.is_undefined() {
                        null_column_type()
                    } else {
                        stored
                    };
                    let unknown = q
                        .parts
                        .iter()
                        .any(|part| part.items.get(idx).is_some_and(|x| x.unknown));
                    let declared = |x: &tir::Item| match &x.expr.kind {
                        tir::TKind::Cast {
                            num: Some(q),
                            derefs,
                            ..
                        } if derefs.is_empty() && q.digits > 0 => Some((q.digits, q.scale)),
                        _ => None,
                    };
                    let stored_num = declared(it).filter(|d| {
                        q.parts
                            .iter()
                            .all(|part| part.items.get(idx).and_then(declared) == Some(*d))
                    });
                    TempColumn {
                        name: it.name.clone(),
                        ty: if unknown { None } else { Some(stored) },
                        unknown,
                        stored: stored_num.map(|(digits, scale)| NumQ {
                            digits,
                            scale,
                            nonneg: false,
                        }),
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    TempSchema {
        name,
        columns,
        open,
    }
}

pub(crate) fn star_over_open(
    memory: Option<&CompileArena>,
    first: &ast::Select,
    q: &tir::Query,
) -> R<bool> {
    fn is_open(source: &tir::Source) -> bool {
        match &source.from {
            tir::From::Param { schema, .. } => schema.open,
            tir::From::Temp(schema) => schema.open,
            _ => false,
        }
    }
    let Some(part) = q.parts.first() else {
        return Ok(false);
    };
    let mut stack = mem::ScratchVec::new(memory, part.sources.len())?;
    stack.extend(memory, part.sources.iter())?;
    let mut all = mem::ScratchVec::new(memory, 0)?;
    while let Some(source) = stack.pop() {
        all.push(memory, (source.alias.as_str(), is_open(source)))?;
        stack.extend(memory, source.joins.iter().map(|join| &join.right))?;
    }
    let any_open = all.iter().any(|(_, open)| *open);
    for item in &first.items {
        match &item.kind {
            ItemKind::Star if any_open => return Ok(true),
            ItemKind::PathStar(path) => {
                let Some(alias) = path.parts.first() else {
                    if any_open {
                        return Ok(true);
                    }
                    continue;
                };
                let alias = mem::NameKey::folded(memory, &alias.text)?;
                let mut named = false;
                let mut open = false;
                for (name, is_open) in &all {
                    if mem::NameKey::folded(memory, name)? == alias {
                        named = true;
                        open |= *is_open;
                    }
                }
                if open || (!named && any_open) {
                    return Ok(true);
                }
            }
            _ => {}
        }
    }
    Ok(false)
}

pub(crate) fn null_column_type() -> QType {
    QType::boolean()
        .merge(&QType::str_(StrQ {
            len: 1,
            fixed: false,
        }))
        .merge(&QType::date(DateQ::DateTime))
        .merge(&QType::num(NumQ {
            digits: 1,
            scale: 0,
            nonneg: true,
        }))
}

fn unknown_parameter_result(e: &TExpr) -> bool {
    if e.ty.atom_count() != 0 {
        return false;
    }
    match &e.kind {
        TKind::Param(_) => true,
        TKind::Agg {
            f: Func::Min | Func::Max,
            arg: Some(arg),
            ..
        } => unknown_parameter_result(arg),
        TKind::Case { whens, else_, .. } => {
            whens
                .iter()
                .any(|(_, value)| unknown_parameter_result(value))
                || else_.as_deref().is_some_and(unknown_parameter_result)
        }
        _ => false,
    }
}
