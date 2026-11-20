use crate::Application;
use crate::model::{ObjId, ObjKind};
use bsl_prim::fold_name;
use bsl_vm::HostError;
use ibschema::Row;
use ibschema::fields::{FieldValue, encode};
use ibschema::names::StdField;
use ibschema::storage::{FieldId, IndexId, TableId, TableInfo};
use ibschema::store::{Direction, RowHandle, StoreResult, WriteTxn};
use ibschema::values::RefId;
use md_model::{ChildTag, MdKind, MdRef, Value as MdValue};
use std::ops::Bound;

fn internal(text: &str) -> HostError {
    HostError::text(&format!("internal: {text}"), &format!("internal: {text}"))
}

fn refusal(journal: &str, what: &str) -> HostError {
    HostError::not_implemented(&format!("ЖурналДокументов.{journal}"), what)
}

enum Source {
    Field(FieldId),
    Null,
}

struct Entry<'a> {
    name: String,
    table: TableId,
    ti: &'a TableInfo,
    by_doc: IndexId,
    document: usize,
    std: Vec<(usize, StdField)>,
    columns: Vec<(FieldId, Source)>,
}

pub(crate) struct Journals<'a> {
    doc: &'a TableInfo,
    number: u32,
    entries: Vec<Entry<'a>>,
}

fn names_object(app: &Application, r: &MdRef, doc: ObjId) -> bool {
    let model = app.model();
    if let Some(id) = r.id {
        return model.by_uuid(id) == Some(doc);
    }
    let mut parts = r.name.split('.');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(word), Some(name), None) => ObjKind::from_word(word)
            .and_then(|k| model.find(k, name))
            .is_some_and(|o| o == doc),
        _ => false,
    }
}

fn refs(v: Option<&MdValue>) -> &[MdRef] {
    match v {
        Some(MdValue::RefList(list)) => list,
        Some(MdValue::Ref(r)) => std::slice::from_ref(r),
        _ => &[],
    }
}

impl<'a> Journals<'a> {
    pub(crate) fn of(app: &'a Application, obj: ObjId) -> Result<Journals<'a>, HostError> {
        let model = app.model();
        let map = app.store().storage();
        let def = model.obj(obj);
        let st = model
            .storage_of(obj)
            .ok_or_else(|| internal("the document has no storage"))?;
        let doc = map
            .try_table(st.main)
            .ok_or_else(|| internal("no main table of the document"))?;
        let number = st
            .ref_number
            .ok_or_else(|| internal("the document has no reference table"))?;
        let mut entries = Vec::new();
        for md in model.config.of_kind(MdKind::DocumentJournal) {
            if !refs(md.prop("RegisteredDocuments"))
                .iter()
                .any(|r| names_object(app, r, obj))
            {
                continue;
            }
            let name = md.name.clone();
            let journal = model
                .by_uuid(md.id)
                .and_then(|j| model.storage_of(j))
                .ok_or_else(|| internal("a journal without storage"))?;
            let ti = map
                .try_table(journal.main)
                .ok_or_else(|| internal("no table of the journal"))?;
            if !ti.separators.is_empty() {
                return Err(refusal(&name, "разделённый журнал"));
            }
            let col = |f: StdField| {
                ti.field_by_std(f)
                    .map(|id| usize::from(id.0))
                    .ok_or_else(|| refusal(&name, &format!("нет поля {f:?}")))
            };
            let document = col(StdField::Document)?;
            let mut std = vec![
                (col(StdField::DateTime)?, StdField::DateTime),
                (col(StdField::Marked)?, StdField::Marked),
                (col(StdField::Posted)?, StdField::Posted),
            ];
            if let Some(n) = ti.field_by_std(StdField::Number) {
                if doc.field_by_std(StdField::Number).is_none() {
                    return Err(refusal(&name, "номер журнала у документа без номера"));
                }
                std.push((usize::from(n.0), StdField::Number));
            }
            let by_doc = ti
                .indexes
                .iter()
                .position(|ix| {
                    ix.unique
                        && ix.prefix == 0
                        && ix.fields.len() == 1
                        && ix
                            .fields
                            .first()
                            .is_some_and(|f| usize::from(f.0) == document)
                })
                .and_then(|k| u16::try_from(k).ok())
                .map(IndexId::Named)
                .ok_or_else(|| refusal(&name, "нет индекса по документу"))?;
            let mut columns = Vec::new();
            for c in md.children.iter().filter(|c| c.tag == ChildTag::Column) {
                let fid = ti
                    .fields
                    .iter()
                    .position(|fi| fi.md == Some(c.id) && fi.std.is_none())
                    .and_then(|i| u16::try_from(i).ok())
                    .map(FieldId)
                    .ok_or_else(|| refusal(&name, &format!("графа {} без поля", c.name)))?;
                let mut source = Source::Null;
                for r in refs(c.prop("References")) {
                    let Some(field) = attribute_of(app, obj, r) else {
                        if refers_to_document(app, obj, r) {
                            return Err(refusal(
                                &name,
                                &format!("графа {}: ссылка {}", c.name, r.name),
                            ));
                        }
                        continue;
                    };
                    let i = def
                        .fields
                        .iter()
                        .position(|f| f.md == Some(field))
                        .ok_or_else(|| internal("an attribute without a field"))?;
                    let at = st
                        .fields
                        .get(i)
                        .copied()
                        .flatten()
                        .ok_or_else(|| internal("an attribute without storage"))?;
                    if matches!(source, Source::Field(_)) {
                        return Err(refusal(
                            &name,
                            &format!("графа {}: два реквизита документа", c.name),
                        ));
                    }
                    source = Source::Field(at);
                }
                if matches!(source, Source::Null) && !ti.field(fid).is_some_and(|f| f.nullable) {
                    return Err(refusal(
                        &name,
                        &format!("графа {} без значения и без NULL", c.name),
                    ));
                }
                columns.push((fid, source));
            }
            entries.push(Entry {
                name,
                table: journal.main,
                ti,
                by_doc,
                document,
                std,
                columns,
            });
        }
        Ok(Journals {
            doc,
            number,
            entries,
        })
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn key(&self, id: RefId) -> Vec<FieldValue> {
        vec![FieldValue::Ref {
            table: self.number,
            id,
        }]
    }

    fn row_of(&self, e: &Entry<'_>, id: RefId, doc_row: &Row) -> Result<Row, String> {
        let mut row: Row = vec![FieldValue::Null; e.ti.fields.len()];
        let cell = |f: Option<FieldId>| -> Result<FieldValue, String> {
            f.and_then(|f| doc_row.get(usize::from(f.0)))
                .cloned()
                .ok_or_else(|| "короткая строка документа".to_string())
        };
        if let Some(slot) = row.get_mut(e.document) {
            *slot = FieldValue::Ref {
                table: self.number,
                id,
            };
        }
        for (at, std) in &e.std {
            let v = cell(self.doc.field_by_std(*std))?;
            if let Some(slot) = row.get_mut(*at) {
                *slot = v;
            }
        }
        for (fid, source) in &e.columns {
            let v = match source {
                Source::Field(f) => cell(Some(*f))?,
                Source::Null => FieldValue::Null,
            };
            if encode(e.ti, *fid, &v).is_err() {
                let col = e.ti.field(*fid).map_or("", |f| f.name.as_str());
                return Err(format!("значение графы {col}"));
            }
            if let Some(slot) = row.get_mut(usize::from(fid.0)) {
                *slot = v;
            }
        }
        Ok(row)
    }

    fn handle(&self, w: &dyn WriteTxn, e: &Entry<'_>, id: RefId) -> StoreResult<Option<RowHandle>> {
        Ok(self.stored(w, e, id)?.map(|(h, _)| h))
    }

    fn stored(
        &self,
        w: &dyn WriteTxn,
        e: &Entry<'_>,
        id: RefId,
    ) -> StoreResult<Option<(RowHandle, Row)>> {
        let key = self.key(id);
        let mut rows = w.range(
            e.table,
            e.by_doc,
            Bound::Included(key.as_slice()),
            Bound::Included(key.as_slice()),
            Direction::Forward,
        )?;
        rows.next().transpose()
    }

    pub(crate) fn write(
        &self,
        w: &mut dyn WriteTxn,
        id: RefId,
        doc_row: &Row,
        is_new: bool,
    ) -> StoreResult<Result<(), HostError>> {
        for e in &self.entries {
            let row = match self.row_of(e, id, doc_row) {
                Ok(r) => r,
                Err(what) => return Ok(Err(refusal(&e.name, &what))),
            };
            match self.stored(&*w, e, id)? {
                Some((_, old)) if !is_new && same_row(&old, &row) => {}
                Some((h, _)) if !is_new => w.update(e.table, h, &row)?,
                None if is_new => {
                    w.insert(e.table, &row)?;
                }
                Some(_) => return Ok(Err(refusal(&e.name, "строка нового документа уже есть"))),
                None => return Ok(Err(refusal(&e.name, "нет строки записанного документа"))),
            }
        }
        Ok(Ok(()))
    }

    pub(crate) fn delete(&self, w: &mut dyn WriteTxn, id: RefId) -> StoreResult<()> {
        for e in &self.entries {
            if let Some(h) = self.handle(&*w, e, id)? {
                w.delete(e.table, h)?;
            }
        }
        Ok(())
    }
}

fn attribute_of(app: &Application, doc: ObjId, r: &MdRef) -> Option<crate::model::Uuid16> {
    let def = app.model().obj(doc);
    if let Some(id) = r.id {
        return def
            .fields
            .iter()
            .find(|f| f.std.is_none() && f.md == Some(id))?
            .md;
    }
    let parts: Vec<&str> = r.name.split('.').collect();
    let [word, name, tag, attr] = parts.as_slice() else {
        return None;
    };
    let kind = ObjKind::from_word(word)?;
    if app.model().find(kind, name) != Some(doc) || !matches!(*tag, "Attribute" | "Реквизит")
    {
        return None;
    }
    def.fields
        .iter()
        .find(|f| f.std.is_none() && fold_name(&f.name) == fold_name(attr))?
        .md
}

fn refers_to_document(app: &Application, doc: ObjId, r: &MdRef) -> bool {
    if let Some(id) = r.id {
        let model = app.model();
        let def = model.obj(doc);
        return model
            .config
            .object(def.md)
            .is_some_and(|o| o.children.iter().any(|c| c.id == id));
    }
    let mut parts = r.name.split('.');
    match (parts.next(), parts.next()) {
        (Some(word), Some(name)) => ObjKind::from_word(word)
            .and_then(|k| app.model().find(k, name))
            .is_some_and(|o| o == doc),
        _ => false,
    }
}

fn same_row(a: &Row, b: &Row) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(x, y)| match (x, y) {
            (FieldValue::Number(m), FieldValue::Number(n)) => m.eq_value(n),
            _ => x == y,
        })
}
