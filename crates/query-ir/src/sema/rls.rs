use super::expr::Clause;
use super::scope::{FieldScope, Level, SourceCols};
use super::{Analyzer, R};
use crate::access::PredicateForm;
use crate::schema::{QTableId, TableKind};
use crate::tir;
use bsl_prim::fold_name;
use mdschema::MdKind;
use query_syntax::ast::{self, AccessForm};
use query_syntax::{Kw, MsgId, QueryError};

impl Analyzer<'_> {
    pub(crate) fn access_form(&mut self, form: &AccessForm, table: QTableId) -> R<PredicateForm> {
        self.rls = true;
        match form {
            AccessForm::Short { cond, .. } => {
                self.restricted_kind(table, None)?;
                self.levels.push(
                    self.memory,
                    Level::of_table_scope(self.memory, table, self.table_scope(table))?,
                )?;
                let r = self.condition(cond, Clause::Where, Kw::Where);
                self.levels.pop();
                Ok(PredicateForm::Short(r?))
            }
            AccessForm::Full {
                alias,
                from,
                where_,
            } => {
                let mut level = self.declare_sources(Some(from))?;
                let (idx, at) = self.restricted_source(&level, alias, from, table)?;
                self.restricted_kind(table, at)?;
                let scope = self.table_scope(table);
                if let Some(SourceCols::Table { scope: s, .. }) =
                    level.sources.get_mut(idx).map(|x| &mut x.cols)
                {
                    *s = scope;
                }
                self.levels.push(self.memory, level)?;
                let r = self.full_clauses(from, where_.as_ref());
                let mut level = self.levels.pop().unwrap_or_default();
                let where_ = r?;
                let sources = self.assemble_sources(&mut level, from)?.into_native();
                Ok(PredicateForm::Full {
                    alias: alias.text.to_string(),
                    query: Box::new(tir::Select {
                        allowed: false,
                        distinct: false,
                        top: None,
                        items: Vec::new(),
                        sources,
                        where_,
                        group: None,
                        having: None,
                        for_update: None,
                        index_by: Vec::new(),
                        index_unique: Vec::new(),
                    }),
                })
            }
        }
    }

    fn full_clauses(
        &mut self,
        from: &ast::FromClause,
        where_: Option<&ast::Clause>,
    ) -> R<Option<tir::TExpr>> {
        self.join_conditions(from)?;
        match where_ {
            Some(c) => Ok(Some(self.condition(&c.expr, Clause::Where, Kw::Where)?)),
            None => Ok(None),
        }
    }

    fn table_scope(&self, table: QTableId) -> FieldScope {
        match self.schema.table(table).kind {
            TableKind::Object(MdKind::AccountingRegister) => FieldScope::Rls,
            _ => FieldScope::All,
        }
    }

    fn restricted_source(
        &self,
        level: &Level,
        alias: &ast::Ident,
        from: &ast::FromClause,
        table: QTableId,
    ) -> R<(usize, Option<u32>)> {
        let bad = || self.err(MsgId::SELECT_ERROR_INVALIDALIAS, alias.span.start);
        let key = fold_name(&alias.text);
        let idx = level
            .sources
            .iter()
            .position(|s| fold_name(&s.alias) == key)
            .ok_or_else(bad)?;
        let src = level.sources.get(idx).ok_or_else(bad)?;
        match &src.cols {
            SourceCols::Table { id, .. } if *id == table => {}
            _ => return Err(bad()),
        }
        let mut stack: Vec<&ast::Source> = from.sources.iter().rev().collect();
        let mut n = 0usize;
        while let Some(s) = stack.pop() {
            if n == idx {
                let at = match &s.table {
                    ast::TableRef::Named { path, .. } => Some(path.span().start),
                    _ => None,
                };
                return Ok((idx, at));
            }
            n += 1;
            stack.extend(s.joins.iter().rev().map(|j| &j.right));
        }
        Err(bad())
    }

    fn restricted_kind(&self, table: QTableId, at: Option<u32>) -> R<()> {
        let msg = match self.schema.table(table).kind {
            TableKind::Virtual(_) => MsgId::SELECT_ERROR_CANTUSEVIRTUALTABLEINSECURITYINSTRUCTION,
            TableKind::Tabular => MsgId::SELECT_ERROR_CANTUSENESTEDTABLEINSECURITYINSTRUCTION,
            _ => return Ok(()),
        };
        Err(match at {
            Some(offset) => self.err(msg, offset),
            None => QueryError::new(msg, None),
        })
    }
}
