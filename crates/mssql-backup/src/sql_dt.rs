use std::io::{Read, Write};

use dtfile::{DtWriter, Source, TableHead};

const MAX_ROW_BYTES: usize = 128 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("unsupported SQL table: {0}")]
    Unsupported(&'static str),
    #[error("writing DT: {0}")]
    Dt(#[from] dtfile::Error),
    #[error("reading SQL table: {0}")]
    Sql(#[from] crate::sql_rows::Error),
}

pub fn write_sql_table<W: Write>(
    writer: &mut DtWriter<W>,
    head: &TableHead,
    input: impl Read,
    year_offset: i32,
) -> Result<u64, Error> {
    write_inner(writer, head, input, year_offset, None, Framing::Array)
}

pub fn write_framed_sql_table<W: Write>(
    writer: &mut DtWriter<W>,
    head: &TableHead,
    input: impl Read,
    year_offset: i32,
) -> Result<u64, Error> {
    write_inner(writer, head, input, year_offset, None, Framing::Rows)
}

pub fn write_sql_table_with_resolver<W: Write>(
    writer: &mut DtWriter<W>,
    head: &TableHead,
    input: impl Read,
    year_offset: i32,
    mut resolver: impl FnMut(
        &crate::sql_blob::StorageRef,
    ) -> Result<Vec<crate::sql_blob::BlobPart>, String>,
) -> Result<u64, Error> {
    write_inner(
        writer,
        head,
        input,
        year_offset,
        Some(&mut resolver),
        Framing::Array,
    )
}

pub fn write_framed_sql_table_with_resolver<W: Write>(
    writer: &mut DtWriter<W>,
    head: &TableHead,
    input: impl Read,
    year_offset: i32,
    mut resolver: impl FnMut(
        &crate::sql_blob::StorageRef,
    ) -> Result<Vec<crate::sql_blob::BlobPart>, String>,
) -> Result<u64, Error> {
    write_inner(
        writer,
        head,
        input,
        year_offset,
        Some(&mut resolver),
        Framing::Rows,
    )
}

enum Framing {
    Array,
    Rows,
}

type Resolver<'a> =
    dyn FnMut(&crate::sql_blob::StorageRef) -> Result<Vec<crate::sql_blob::BlobPart>, String> + 'a;

fn write_inner<W: Write>(
    writer: &mut DtWriter<W>,
    head: &TableHead,
    input: impl Read,
    year_offset: i32,
    mut resolver: Option<&mut Resolver<'_>>,
    framing: Framing,
) -> Result<u64, Error> {
    if head.source != Source::DataBase {
        return Err(Error::Unsupported("expected DataBase table"));
    }
    if year_offset < 0 {
        return Err(Error::Unsupported("negative year offset"));
    }
    writer.begin_table(head)?;
    let mut count = 0_u64;
    let mut consume = |row: &[u8]| {
        let values = match resolver.as_deref_mut() {
            Some(resolve) => crate::sql_json_row::convert_json_row_with_resolver(
                &head.columns,
                row,
                year_offset,
                resolve,
            ),
            None => crate::sql_json_row::convert_json_row(&head.columns, row, year_offset),
        }
        .map_err(|e| e.to_string())?;
        writer.row(&values).map_err(|e| e.to_string())?;
        count = count
            .checked_add(1)
            .ok_or_else(|| "row count overflow".to_string())?;
        Ok(())
    };
    match framing {
        Framing::Array => crate::sql_rows::read_rows(input, MAX_ROW_BYTES, &mut consume)?,
        Framing::Rows => crate::sql_rows::read_framed_rows(input, MAX_ROW_BYTES, &mut consume)?,
    }
    writer.end_table()?;
    Ok(count)
}
