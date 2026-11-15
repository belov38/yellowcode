use std::io::Write;

use brace::binary::{Atom, BinNode};
use ibschema::columns::Column;
use ibschema::logical::LogicalSchema;
use ibschema::values::ColumnValue;

use crate::container::DtHeader;
use crate::elements::{RawEvent, RawWriter, Source};
use crate::reader::{Event, TableHead};
use crate::{Error, Result, rows};

pub(crate) const FOLDER_CHUNK: usize = 250;
const DB_CHUNK: usize = 16_384;

pub struct DtWriter<W: Write> {
    raw: RawWriter<W>,
    db: Option<Vec<TableHead>>,
    next: usize,
    cur: Option<(TableHead, usize)>,
    poisoned: bool,
    storage: Vec<(onecd::Numeric, Vec<u8>)>,
    ext: Option<Vec<TableHead>>,
    ext_next: usize,
}

fn head_diff(got: &TableHead, want: &TableHead) -> Option<String> {
    let what = if got.source != want.source {
        format!("source {:?}", got.source)
    } else if got.name != want.name {
        format!("name {}", got.name)
    } else if got.phys_name != want.phys_name {
        format!("phys_name {}", got.phys_name)
    } else if got.columns != want.columns {
        let names = |c: &[Column]| c.iter().map(|c| c.name.clone()).collect::<Vec<_>>();
        format!("columns {:?}", names(&got.columns))
    } else {
        return None;
    };
    Some(format!(
        "table head {what} does not match the expected {} ({})",
        want.phys_name, want.name
    ))
}

impl<W: Write> DtWriter<W> {
    pub fn new(w: W, header: DtHeader) -> Result<DtWriter<W>> {
        Ok(DtWriter {
            raw: RawWriter::new(w, header)?,
            db: None,
            next: 0,
            cur: None,
            poisoned: false,
            storage: Vec::new(),
            ext: None,
            ext_next: 0,
        })
    }

    pub fn event(&mut self, e: &Event) -> Result<()> {
        match e {
            Event::CompatibilityMode(v) => self.compatibility_mode(*v),
            Event::CustomFlag { name, value } => self.custom_flag(name, value),
            Event::TableStart(h) => self.begin_table(h),
            Event::Row(v) => self.row(v),
            Event::TableEnd => self.end_table(),
            Event::DataBaseStart(s) => self.begin_database(s),
            Event::DataBaseEnd => self.end_database(),
            Event::Unknown { name, items } => self.unknown(name, items),
            Event::ExtensionsStart => self.begin_extensions(),
            Event::ExtensionStart { schema_id, schema } => self.begin_extension(*schema_id, schema),
            Event::ExtensionEnd => self.end_extension(),
            Event::ExtensionsEnd => self.end_extensions(),
        }
    }

    pub fn compatibility_mode(&mut self, v: i64) -> Result<()> {
        self.raw
            .event(&RawEvent::CompatibilityMode(BinNode::Atom(Atom::int(
                i128::from(v),
            ))))
    }

    pub fn custom_flag(&mut self, name: &str, value: &BinNode) -> Result<()> {
        self.raw.event(&RawEvent::CustomFlag {
            name: name.to_string(),
            value: value.clone(),
        })
    }

    pub fn begin_table(&mut self, head: &TableHead) -> Result<()> {
        let (want, ordinal, chunk) = match TableHead::system(&head.source) {
            Some(h) => {
                let chunk = match head.source {
                    Source::Folder(_) | Source::SchemaStorage => FOLDER_CHUNK,
                    _ => 0,
                };
                (h, 0, chunk)
            }
            None if matches!(head.source, Source::Extension(_)) => {
                let ext = self.ext.as_ref().ok_or_else(|| {
                    Error::Unsupported(format!(
                        "{}: extension table outside its schema",
                        head.phys_name
                    ))
                })?;
                let h = ext.get(self.ext_next).ok_or_else(|| {
                    Error::Unsupported(format!(
                        "{}: more tables than the extension schema has ({})",
                        head.phys_name,
                        ext.len()
                    ))
                })?;
                (h.clone(), self.ext_next, DB_CHUNK)
            }
            None => {
                let db = self.db.as_ref().ok_or_else(|| {
                    Error::Unsupported(format!(
                        "{}: DataBase table outside DataBase",
                        head.phys_name
                    ))
                })?;
                let h = db.get(self.next).ok_or_else(|| {
                    Error::Unsupported(format!(
                        "{}: more tables than DBSchema has ({})",
                        head.phys_name,
                        db.len()
                    ))
                })?;
                (h.clone(), self.next, DB_CHUNK)
            }
        };
        if let Some(why) = head_diff(head, &want) {
            return Err(Error::Unsupported(why));
        }
        self.raw.event(&RawEvent::TableStart {
            source: head.source.clone(),
            ordinal,
        })?;
        match head.source {
            Source::DataBase => self.next = self.next.saturating_add(1),
            Source::Extension(_) => self.ext_next = self.ext_next.saturating_add(1),
            _ => {}
        }
        self.cur = Some((want, chunk));
        Ok(())
    }

    pub fn row(&mut self, values: &[ColumnValue]) -> Result<()> {
        if self.poisoned {
            return Err(Error::Unsupported(
                "writer has an incomplete streamed row".into(),
            ));
        }
        let DtWriter {
            raw, cur, storage, ..
        } = self;
        let (head, chunk) = cur
            .as_ref()
            .ok_or_else(|| Error::Unsupported("element order: row outside a table".into()))?;
        rows::check_row(&head.phys_name, &head.columns, values)?;
        if head.source == Source::SchemaStorage {
            let (Some(ColumnValue::Number(id)), Some(ColumnValue::Bytes(schema))) =
                (values.first(), values.get(2))
            else {
                return Err(Error::Unsupported(
                    "SCHEMASTORAGE row without SCHEMAID or CURRENTSCHEMA".into(),
                ));
            };
            if storage.iter().any(|(i, _)| i.eq_value(id)) {
                return Err(Error::Unsupported(
                    "SCHEMASTORAGE: two rows with one SCHEMAID".into(),
                ));
            }
            storage.push((id.clone(), schema.clone()));
        }
        raw.row_with(|w| rows::write_row(w, &head.columns, values, *chunk))
    }

    pub fn row_folder_stream(
        &mut self,
        metadata: &[ColumnValue],
        length: u64,
        input: &mut impl std::io::Read,
    ) -> Result<()> {
        if self.poisoned {
            return Err(Error::Unsupported(
                "writer has an incomplete streamed row".into(),
            ));
        }
        let DtWriter {
            raw, cur, poisoned, ..
        } = self;
        let (head, chunk) = cur
            .as_ref()
            .ok_or_else(|| Error::Unsupported("element order: row outside a table".into()))?;
        if !matches!(head.source, Source::Folder(_)) || metadata.len() != 5 {
            return Err(Error::Unsupported("expected folder row metadata".into()));
        }
        let Some(ColumnValue::Number(size)) = metadata.get(4) else {
            return Err(Error::Unsupported("missing folder data size".into()));
        };
        let expected = onecd::Numeric::parse(&length.to_string())
            .ok_or_else(|| Error::Unsupported("invalid folder data size".into()))?;
        if !size.eq_value(&expected) {
            return Err(Error::Unsupported("folder data size mismatch".into()));
        }
        let mut values = metadata.to_vec();
        values.push(ColumnValue::Bytes(Vec::new()));
        rows::check_row(&head.phys_name, &head.columns, &values)?;
        let result = raw.row_with(|w| {
            rows::write_folder_stream(w, &head.columns, metadata, *chunk, length, input)
        });
        if result.is_err() {
            *poisoned = true;
        }
        result
    }

    pub fn end_table(&mut self) -> Result<()> {
        if self.poisoned {
            return Err(Error::Unsupported(
                "writer has an incomplete streamed row".into(),
            ));
        }
        self.raw.event(&RawEvent::TableEnd)?;
        self.cur = None;
        Ok(())
    }

    pub fn begin_database(&mut self, schema: &LogicalSchema) -> Result<()> {
        let heads = TableHead::database(schema)?;
        self.raw.event(&RawEvent::DataBaseStart(schema.to_bin()))?;
        self.db = Some(heads);
        self.next = 0;
        Ok(())
    }

    pub fn end_database(&mut self) -> Result<()> {
        if let Some(db) = &self.db
            && self.next < db.len()
        {
            return Err(Error::Unsupported(format!(
                "fewer tables than DBSchema has ({} of {})",
                self.next,
                db.len()
            )));
        }
        self.raw.event(&RawEvent::DataBaseEnd)?;
        self.db = None;
        self.next = 0;
        Ok(())
    }

    pub fn begin_extensions(&mut self) -> Result<()> {
        let db = self
            .db
            .as_ref()
            .ok_or_else(|| Error::Unsupported("extension block outside DataBase".into()))?;
        if self.next < db.len() {
            return Err(Error::Unsupported(format!(
                "extension block before all tables of DBSchema ({} of {})",
                self.next,
                db.len()
            )));
        }
        self.raw.event(&RawEvent::ExtensionsStart)?;
        self.storage.clear();
        Ok(())
    }

    pub fn begin_extension(&mut self, schema_id: u32, schema: &LogicalSchema) -> Result<()> {
        let id = onecd::Numeric::from_i64(i64::from(schema_id));
        let stored = self
            .storage
            .iter()
            .find(|(i, _)| i.eq_value(&id))
            .map(|(_, t)| t)
            .ok_or_else(|| {
                Error::Unsupported(format!(
                    "no SCHEMASTORAGE row for extension schema {schema_id}"
                ))
            })?;
        if crate::reader::extension_schema(stored).ok().as_ref() != Some(schema) {
            return Err(Error::Unsupported(format!(
                "extension schema {schema_id} differs from its SCHEMASTORAGE row"
            )));
        }
        let heads = TableHead::extension(schema_id, schema)?;
        self.raw.event(&RawEvent::ExtensionStart { schema_id })?;
        self.ext = Some(heads);
        self.ext_next = 0;
        Ok(())
    }

    pub fn end_extension(&mut self) -> Result<()> {
        if let Some(ext) = &self.ext
            && self.ext_next < ext.len()
        {
            return Err(Error::Unsupported(format!(
                "fewer tables than the extension schema has ({} of {})",
                self.ext_next,
                ext.len()
            )));
        }
        self.raw.event(&RawEvent::ExtensionEnd)?;
        self.ext = None;
        self.ext_next = 0;
        Ok(())
    }

    pub fn end_extensions(&mut self) -> Result<()> {
        self.raw.event(&RawEvent::ExtensionsEnd)?;
        self.storage.clear();
        Ok(())
    }

    pub fn unknown(&mut self, name: &str, items: &[BinNode]) -> Result<()> {
        self.raw.event(&RawEvent::Unknown {
            name: name.to_string(),
            items: items.to_vec(),
        })
    }

    pub fn finish(self) -> Result<W> {
        if self.poisoned {
            return Err(Error::Unsupported(
                "writer has an incomplete streamed row".into(),
            ));
        }
        self.raw.finish()
    }
}
