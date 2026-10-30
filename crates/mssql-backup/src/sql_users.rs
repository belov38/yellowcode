use dtfile::{DtWriter, Source, TableHead};
use ibschema::values::ColumnValue;
use std::io::{Read, Write};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("unsupported SQL users: {0}")]
    Unsupported(&'static str),
    #[error("SQL users catalog: {0}")]
    Catalog(#[from] crate::sql_catalog::Error),
    #[error("writing DT users: {0}")]
    Dt(#[from] dtfile::Error),
    #[error("reading SQL users: {0}")]
    Sql(#[from] crate::sql_rows::Error),
}

pub fn write_users<W: Write>(
    writer: &mut DtWriter<W>,
    head: &TableHead,
    input: impl Read,
    year_offset: i32,
) -> Result<u64, Error> {
    write_user_rows(writer, head, input, year_offset, Source::Users)
}

pub fn users_catalog(
    actual: &[crate::sql_catalog::SqlColumn],
) -> Result<(Vec<crate::sql_catalog::SqlColumn>, bool), Error> {
    let (columns, email) = users_spr_catalog(actual)?;
    let head =
        TableHead::system(&Source::Users).ok_or(Error::Unsupported("missing Users layout"))?;
    let projection: Vec<_> = columns
        .into_iter()
        .filter(|column| {
            head.columns
                .iter()
                .any(|expected| expected.name.eq_ignore_ascii_case(&column.name))
        })
        .collect();
    crate::sql_catalog::validate_columns(&head.columns, &projection)?;
    Ok((projection, email))
}

pub fn users_spr_catalog(
    actual: &[crate::sql_catalog::SqlColumn],
) -> Result<(Vec<crate::sql_catalog::SqlColumn>, bool), Error> {
    let mut projection = Vec::with_capacity(actual.len());
    let mut email = false;
    for column in actual {
        if column.name.eq_ignore_ascii_case("EMAIL") {
            if email
                || !column.sql_type.eq_ignore_ascii_case("nvarchar")
                || column.max_length != 256
                || !column.nullable
            {
                return Err(Error::Unsupported("unexpected email column"));
            }
            email = true;
        } else {
            projection.push(column.clone());
        }
    }
    let head = TableHead::system(&Source::UsersSpr)
        .ok_or(Error::Unsupported("missing UsersSpr layout"))?;
    crate::sql_catalog::validate_columns(&head.columns, &projection)?;
    Ok((projection, email))
}

pub fn write_users_spr<W: Write>(
    writer: &mut DtWriter<W>,
    head: &TableHead,
    input: impl Read,
    year_offset: i32,
) -> Result<u64, Error> {
    write_user_rows(writer, head, input, year_offset, Source::UsersSpr)
}

fn write_user_rows<W: Write>(
    writer: &mut DtWriter<W>,
    head: &TableHead,
    input: impl Read,
    year_offset: i32,
    source: Source,
) -> Result<u64, Error> {
    if head.source != source || TableHead::system(&source).as_ref() != Some(head) {
        return Err(Error::Unsupported("unexpected user layout"));
    }
    if year_offset < 0 {
        return Err(Error::Unsupported("negative year offset"));
    }
    writer.begin_table(head)?;
    let mut count = 0_u64;
    crate::sql_rows::read_rows(input, 128 * 1024 * 1024, |row| {
        let values = crate::sql_json_row::convert_json_row(&head.columns, row, year_offset)
            .map_err(|e| e.to_string())?;
        if source == Source::UsersSpr {
            for name in ["EAUTH", "ADMROLE", "USSPRH"] {
                let index = head
                    .columns
                    .iter()
                    .position(|c| c.name == name)
                    .ok_or("missing separation field")?;
                if matches!(values.get(index), None | Some(ColumnValue::Null)) {
                    return Err("null separation field".into());
                }
            }
        }
        writer.row(&values).map_err(|e| e.to_string())?;
        count = count.checked_add(1).ok_or("user row count overflow")?;
        Ok(())
    })?;
    writer.end_table()?;
    Ok(count)
}
