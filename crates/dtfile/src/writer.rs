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
                    Source::Folder(_) => FOLDER_CHUNK,
                    _ => 0,
                };
                (h, 0, chunk)
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
        if head.source == Source::DataBase {
            self.next = self.next.saturating_add(1);
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
        let DtWriter { raw, cur, .. } = self;
        let (head, chunk) = cur
            .as_ref()
            .ok_or_else(|| Error::Unsupported("element order: row outside a table".into()))?;
        rows::check_row(&head.phys_name, &head.columns, values)?;
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
