use super::expr::{Clause, ExprCtx};
use super::scope::Level;
use super::select::dcs_scope;
use super::{Analyzer, R};
use crate::managed as mem;
use crate::schema::QTableId;
use crate::tir;
use query_syntax::ast::{self, CharSource, DcsBlock, DcsItem, DcsKind, DcsList};

enum CharCols {
    Table(QTableId),
    Names(mem::ScratchVec<mem::NameKey>),
}

impl Analyzer<'_> {
    pub(crate) fn dcs_select_blocks(&mut self, blocks: &[DcsBlock], items: &[tir::Item]) -> R<()> {
        if blocks.is_empty() {
            return Ok(());
        }
        let saved = self.columns.replace(dcs_scope(self.memory, items)?);
        let r = blocks.iter().try_for_each(|b| self.dcs_block(b));
        self.columns = saved;
        r
    }

    pub(crate) fn dcs_query_blocks(&mut self, blocks: &[DcsBlock], first: &tir::Select) -> R<()> {
        self.dcs_select_blocks(blocks, &first.items)
    }

    fn dcs_block(&mut self, b: &DcsBlock) -> R<()> {
        match &b.kind {
            DcsKind::Select(l)
            | DcsKind::Where(l)
            | DcsKind::OrderBy(l)
            | DcsKind::GroupBy(l)
            | DcsKind::Totals(l) => self.dcs_list(l),
            DcsKind::Characteristics(c) => self.characteristics(c),
        }
    }

    fn dcs_list(&mut self, l: &DcsList) -> R<()> {
        l.items.iter().try_for_each(|it| self.dcs_item(it))
    }

    fn dcs_item(&mut self, it: &DcsItem) -> R<()> {
        let e = self.expr_value(&it.expr, ExprCtx::new(Clause::Select, it.expr.span.start))?;
        for group in &it.dcs {
            self.dcs_list(group)?;
        }
        let Some(nested) = &it.nested else {
            return Ok(());
        };
        let Some(tid) = self.nested_table(&e) else {
            return Err(self.unsupported(
                "a nested table list needs a tabular section",
                it.expr.span.start,
            ));
        };
        self.levels
            .push(self.memory, Level::of_table(self.memory, tid)?)?;
        let saved = self.columns.take();
        let r = self.dcs_list(nested);
        self.levels.pop();
        self.columns = saved;
        r
    }

    fn characteristics(&mut self, c: &ast::Characteristics) -> R<()> {
        let ty = self
            .schema
            .type_by_name_managed(self.memory, &c.type_name.parts)?
            .map(|ty| mem::ScratchType::new(self.memory, ty));
        if ty.is_none() {
            return Err(self.table_not_found(&c.type_name));
        }
        let saved = self.columns.take();
        let r = self.characteristic_sources(c);
        self.columns = saved;
        r
    }

    fn characteristic_sources(&mut self, c: &ast::Characteristics) -> R<()> {
        let kinds = self.char_source(&c.kinds)?;
        for f in [
            Some(&c.key_field),
            Some(&c.name_field),
            c.value_type_field.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            self.char_field(&kinds, f)?;
        }
        let values = self.char_source(&c.values)?;
        for f in [
            Some(&c.object_field),
            Some(&c.kind_field),
            c.value_field.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            self.char_field(&values, f)?;
        }
        Ok(())
    }

    fn char_source(&mut self, s: &CharSource) -> R<CharCols> {
        match s {
            CharSource::Table(p) => {
                if let [one] = p.parts.as_slice()
                    && let Some(t) = self.temps.get(&one.text)?
                {
                    self.note_read(&one.text)?;
                    let mut names = mem::ScratchVec::new(self.memory, t.columns.len())?;
                    for col in &t.columns {
                        names.push(self.memory, mem::NameKey::raw(self.memory, &col.name)?)?;
                    }
                    return Ok(CharCols::Names(names));
                }
                match self.schema.find_table_managed(self.memory, &p.parts)? {
                    Some(id) => Ok(CharCols::Table(id)),
                    None => Err(self.table_not_found(p)),
                }
            }
            CharSource::Query(q) => {
                let q = self.subquery_owned(q)?;
                let first = q.trusted_borrow().parts.first();
                let mut names =
                    mem::ScratchVec::new(self.memory, first.map_or(0, |part| part.items.len()))?;
                if let Some(first) = first {
                    for item in &first.items {
                        names.push(self.memory, mem::NameKey::raw(self.memory, &item.name)?)?;
                    }
                }
                Ok(CharCols::Names(names))
            }
        }
    }

    fn char_field(&self, cols: &CharCols, f: &ast::Path) -> R<()> {
        let Some(first) = f.parts.first() else {
            return Ok(());
        };
        let key = mem::NameKey::folded(self.memory, &first.text)?;
        let found = match cols {
            CharCols::Table(id) => self.schema.find_field_folded(*id, &key).is_some(),
            CharCols::Names(names) => {
                let mut found = false;
                for name in names.iter() {
                    if mem::NameKey::folded(self.memory, name)? == key {
                        found = true;
                        break;
                    }
                }
                found
            }
        };
        if found {
            Ok(())
        } else {
            Err(self.quoted_path_error(
                query_syntax::MsgId::SELECT_ERROR_FIELDNOTFOUND,
                f,
                f.span().start,
            ))
        }
    }
}
