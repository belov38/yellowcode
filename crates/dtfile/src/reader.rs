use std::io::Read;
use std::sync::Arc;

use brace::binary::BinNode;
use ibschema::columns::{Column, dt_columns, phys_table_name};
use ibschema::logical::LogicalSchema;
use ibschema::values::ColumnValue;

use crate::container::DtHeader;
use crate::elements::{RawEvent, RawReader, Source};
use crate::error::schema_note;
use crate::{Error, Result, layout, rows};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableHead {
    pub source: Source,
    pub name: String,
    pub phys_name: String,
    pub columns: Arc<Vec<Column>>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    CompatibilityMode(i64),
    CustomFlag { name: String, value: BinNode },
    TableStart(TableHead),
    Row(Vec<ColumnValue>),
    TableEnd,
    DataBaseStart(LogicalSchema),
    DataBaseEnd,
    Unknown { name: String, items: Vec<BinNode> },
}

impl TableHead {
    pub fn system(source: &Source) -> Option<TableHead> {
        let (name, phys_name, columns) = match source {
            Source::Folder(n) => (n.clone(), n.to_uppercase(), layout::folder()),
            Source::Users => ("Users".into(), "V8USERS".into(), layout::users()),
            Source::UsersSpr => ("UsersSpr".into(), "V8USERS".into(), layout::users_spr()),
            Source::PasswordPolicies => (
                "PasswordPolicies".into(),
                "V8USERPWDPLCS".into(),
                layout::password_policies(),
            ),
            Source::CompromisedPasswords => (
                "CompromisedPasswords".into(),
                "V8CMSDPWDS".into(),
                layout::compromised_passwords(),
            ),
            Source::DataBase => return None,
        };
        Some(TableHead {
            source: source.clone(),
            name,
            phys_name,
            columns: Arc::new(columns),
        })
    }

    pub fn database(schema: &LogicalSchema) -> Result<Vec<TableHead>> {
        schema
            .walk()
            .into_iter()
            .map(|(t, o)| {
                Ok(TableHead {
                    source: Source::DataBase,
                    name: t.name.clone(),
                    phys_name: phys_table_name(t, o),
                    columns: Arc::new(dt_columns(t, o)?),
                })
            })
            .collect()
    }
}

pub struct DtReader<R: Read> {
    raw: RawReader<R>,
    db: Vec<TableHead>,
    started: usize,
    cur: Option<TableHead>,
    failed: Option<Error>,
}

impl<R: Read> DtReader<R> {
    pub fn folder_row_header(&mut self) -> Result<Option<Vec<ColumnValue>>> {
        if let Some(error) = &self.failed {
            return Err(error.again());
        }
        let result = (|| {
            let head = self
                .cur
                .as_ref()
                .filter(|h| matches!(h.source, Source::Folder(_)))
                .ok_or_else(|| Error::Unsupported("streamed row outside a folder".into()))?;
            let at = self.raw.offset();
            let Some(values) = self.raw.folder_header()? else {
                self.cur = None;
                return Ok(None);
            };
            let columns = head
                .columns
                .get(..5)
                .ok_or_else(|| Error::Unsupported("invalid folder columns".into()))?;
            rows::decode_in(&head.phys_name, columns, &values, at).map(Some)
        })();
        if let Err(error) = &result {
            self.failed = Some(error.again());
        }
        result
    }

    pub fn copy_folder_binary(
        &mut self,
        sink: &mut impl std::io::Write,
        limit: u64,
    ) -> Result<u64> {
        self.copy_folder_binary_with_chunk_limit(sink, limit, u64::MAX)
    }

    pub fn copy_folder_binary_with_chunk_limit(
        &mut self,
        sink: &mut impl std::io::Write,
        limit: u64,
        max_chunks: u64,
    ) -> Result<u64> {
        self.copy_folder_binary_bounded(sink, limit, max_chunks, false)
    }

    pub fn copy_folder_binary_with_work_limit(
        &mut self,
        sink: &mut impl std::io::Write,
        limit: u64,
        base_chunks: u64,
    ) -> Result<u64> {
        self.copy_folder_binary_bounded(sink, limit, base_chunks, true)
    }

    fn copy_folder_binary_bounded(
        &mut self,
        sink: &mut impl std::io::Write,
        limit: u64,
        chunks: u64,
        adaptive: bool,
    ) -> Result<u64> {
        if let Some(error) = &self.failed {
            return Err(error.again());
        }
        let result = if self
            .cur
            .as_ref()
            .is_some_and(|h| matches!(h.source, Source::Folder(_)))
        {
            if adaptive {
                self.raw
                    .copy_folder_binary_with_work_limit(sink, limit, chunks)
            } else {
                self.raw
                    .copy_folder_binary_with_chunk_limit(sink, limit, chunks)
            }
        } else {
            Err(Error::Unsupported("streamed data outside a folder".into()))
        };
        if let Err(error) = &result {
            self.failed = Some(error.again());
        }
        result
    }

    pub fn open(r: R) -> Result<DtReader<R>> {
        Ok(Self::from_raw(RawReader::open(r)?))
    }

    #[doc(hidden)]
    pub fn from_raw(raw: RawReader<R>) -> DtReader<R> {
        DtReader {
            raw,
            db: Vec::new(),
            started: 0,
            cur: None,
            failed: None,
        }
    }

    pub fn header(&self) -> DtHeader {
        self.raw.header()
    }

    pub fn set_read_limits(&mut self, limits: brace::binary::ReadLimits) {
        self.raw.set_read_limits(limits);
    }

    pub fn offset(&self) -> u64 {
        self.raw.offset()
    }

    pub fn next_event(&mut self) -> Result<Option<Event>> {
        if let Some(e) = &self.failed {
            return Err(e.again());
        }
        let r = self.step();
        if let Err(e) = &r {
            self.failed = Some(e.again());
        }
        r
    }

    fn step(&mut self) -> Result<Option<Event>> {
        let at = self.raw.offset();
        let Some(e) = self.raw.next()? else {
            return Ok(None);
        };
        let dt = |what: &str| Error::Dt {
            offset: at,
            what: what.to_string(),
        };
        Ok(Some(match e {
            RawEvent::CompatibilityMode(v) => {
                let mode = match &v {
                    BinNode::Atom(a) => a.as_int().and_then(|i| i64::try_from(i).ok()),
                    BinNode::List(_) => None,
                };
                Event::CompatibilityMode(
                    mode.ok_or_else(|| dt("CompatibilityMode: expected an integer"))?,
                )
            }
            RawEvent::CustomFlag { name, value } => Event::CustomFlag { name, value },
            RawEvent::TableStart { source, ordinal } => {
                let head = match TableHead::system(&source) {
                    Some(h) => h,
                    None => {
                        let h = self.db.get(ordinal).cloned();
                        let h = h.ok_or_else(|| dt("more data lists than DBSchema tables"))?;
                        self.started = ordinal.saturating_add(1);
                        h
                    }
                };
                self.cur = Some(head.clone());
                Event::TableStart(head)
            }
            RawEvent::Row(cells) => {
                let head = self.cur.as_ref().ok_or_else(|| dt("row outside a table"))?;
                Event::Row(rows::decode_in(&head.phys_name, &head.columns, &cells, at)?)
            }
            RawEvent::TableEnd => {
                self.cur = None;
                Event::TableEnd
            }
            RawEvent::DataBaseStart(node) => {
                self.db.clear();
                self.started = 0;
                let place = |e| Error::Schema(schema_note(e, &format!("DBSchema at {at}")));
                let schema = LogicalSchema::from_bin(&node).map_err(place)?;
                self.db = TableHead::database(&schema).map_err(|e| match e {
                    Error::Schema(e) => place(e),
                    other => other,
                })?;
                Event::DataBaseStart(schema)
            }
            RawEvent::DataBaseEnd => {
                if self.started < self.db.len() {
                    return Err(dt(&format!(
                        "fewer data lists than DBSchema tables ({} of {})",
                        self.started,
                        self.db.len()
                    )));
                }
                Event::DataBaseEnd
            }
            RawEvent::Unknown { name, items } => Event::Unknown { name, items },
        }))
    }
}
