use mdconf::ConfigurationExt;
use mdconf::EntrySource;
use onecd::{BlobVal, DateTime, Numeric, Value, WriteTxn};
use std::path::{Path, PathBuf};

fn folder_row(write: &mut WriteTxn<'_>, table: &str, name: &str, data: &[u8]) {
    let descr = write.read_table(table).unwrap().descr().clone();
    let row = descr
        .fields
        .iter()
        .map(|field| match field.name.to_ascii_uppercase().as_str() {
            "FILENAME" => Value::VarChar(name.into()),
            "CREATION" | "MODIFIED" => Value::DateTime(DateTime::EMPTY),
            "ATTRIBUTES" | "PARTNO" => Value::Numeric(Numeric::from_i64(0)),
            "DATASIZE" => Value::Numeric(Numeric::from_i64(data.len() as i64)),
            "BINARYDATA" => Value::Image(BlobVal::Data(data.into())),
            other => panic!("unexpected folder field {other}"),
        })
        .collect();
    write.table(table).unwrap().insert(&row).unwrap();
}

fn replace(write: &mut WriteTxn<'_>, table: &str, column: &str, bytes: &[u8]) {
    let (index, rows) = {
        let read = write.read_table(table).unwrap();
        (
            read.descr().field(column).unwrap().0,
            read.scan().map(Result::unwrap).collect::<Vec<_>>(),
        )
    };
    let mut target = write.table(table).unwrap();
    for (id, mut row) in rows {
        row[index] = Value::Image(BlobVal::Data(bytes.into()));
        target.update(id, &row).unwrap();
    }
}

#[allow(dead_code)]
pub fn create(root: &Path) -> PathBuf {
    let cf = mdconf::CfFile::open(&cf_fixtures::fixture_dir("types").join("p.cf")).unwrap();
    create_from(root, &cf)
}

#[allow(dead_code)]
pub fn create_named(root: &Path, name: &str) -> PathBuf {
    let original = mdconf::CfFile::open(&cf_fixtures::fixture_dir("types").join("p.cf")).unwrap();
    let mut entries = mdconf::MemEntries::new();
    for entry in original.list().unwrap() {
        let mut stored = original.stored(&entry.name).unwrap().unwrap();
        if let Some(raw) = zraw::try_inflate(&stored, zraw::DEFAULT_LIMIT)
            && let Ok(text) = String::from_utf8(raw)
            && text.contains("Goods")
        {
            stored = zraw::deflate(text.replace("Goods", name).as_bytes(), zraw::LEVEL_1C).unwrap();
        }
        entries.insert(entry, stored);
    }
    create_from(root, &entries)
}

#[allow(dead_code)]
fn create_from(root: &Path, cf: &dyn EntrySource) -> PathBuf {
    build(root, cf, false)
}

#[allow(dead_code)]
pub fn create_restructured(root: &Path, cf: &dyn EntrySource) -> PathBuf {
    build(root, cf, true)
}

fn build(root: &Path, cf: &dyn EntrySource, restructure: bool) -> PathBuf {
    let dir = root.join("База ёжий");
    infobase::create::create_empty(&dir, &Default::default()).unwrap();
    let cfg = mdconf::Configuration::load(cf, &Default::default()).unwrap();
    let seed = infobase::seed::seed();
    let mut names = seed.dbnames.clone();
    let built = ibschema::rules::build(
        &cfg,
        &mut names,
        &seed.schema,
        &ibschema::rules::RulesOptions {
            skip_unsupported: true,
        },
    )
    .unwrap();
    let db = onecd::Database::open(
        &dir.join("1Cv8.1CD"),
        onecd::OpenOptions {
            locking: onecd::Locking::None,
            ..onecd::OpenOptions::read_write()
        },
    )
    .unwrap();
    let mut write = db.write().unwrap();
    for (table, owner) in built.schema.walk() {
        let descr =
            infobase::physical::table_descr(table, owner, infobase::physical::NameCase::Applied)
                .unwrap();
        let columns = |d: &onecd::TableDescr| {
            let mut names: Vec<_> = d.fields.iter().map(|f| f.name.to_uppercase()).collect();
            names.sort();
            names
        };
        let same = match write.read_table(&descr.name) {
            Err(_) => None,
            Ok(existing) => Some(columns(existing.descr()) == columns(&descr)),
        };
        match same {
            None => write.create_table(&descr).unwrap(),
            Some(false) if restructure => {
                write.drop_table(&descr.name).unwrap();
                write.create_table(&descr).unwrap();
            }
            Some(_) => {}
        }
    }
    for entry in cf.list().unwrap() {
        folder_row(
            &mut write,
            "CONFIG",
            &entry.name,
            &cf.stored(&entry.name).unwrap().unwrap(),
        );
    }
    let ids = {
        let params = write.read_table("PARAMS").unwrap();
        let index = params.descr().field("FILENAME").unwrap().0;
        params.scan().map(Result::unwrap).filter_map(|(id, row)| matches!(&row[index], Value::VarChar(s) | Value::Char(s) if s.eq_ignore_ascii_case("DBNames")).then_some(id)).collect::<Vec<_>>()
    };
    for id in ids {
        write.table("PARAMS").unwrap().delete(id).unwrap();
    }
    folder_row(
        &mut write,
        "PARAMS",
        "DBNames",
        &zraw::deflate(&names.to_text(), zraw::LEVEL_1C).unwrap(),
    );
    let schema = built.schema.to_text();
    replace(&mut write, "DBSCHEMA", "SERIALIZEDDATA", &schema);
    replace(&mut write, "SCHEMASTORAGE", "CURRENTSCHEMA", &schema);
    write.commit().unwrap();
    drop(db);
    query_console::Console::open(&dir).unwrap();
    dir
}

#[allow(dead_code)]
pub fn populate(dir: &Path, name: &str, count: usize, holes: bool) {
    populate_grouped(dir, name, count, holes, 1);
}

#[allow(dead_code)]
pub fn populate_grouped(dir: &Path, name: &str, count: usize, holes: bool, group: usize) {
    populate_with(dir, name, count, holes, group, &|_, _| None);
}

#[allow(dead_code)]
pub fn populate_with(
    dir: &Path,
    name: &str,
    count: usize,
    holes: bool,
    group: usize,
    extra: &dyn Fn(usize, &str) -> Option<ibschema::fields::FieldValue>,
) {
    use ibschema::{fields, fields::FieldValue, names::StdField, storage::FieldId};
    let ib = infobase::Infobase::open(
        dir,
        infobase::OpenOptions {
            schema_check: infobase::SchemaCheck::Warn,
            locking: onecd::Locking::None,
            ..Default::default()
        },
    )
    .unwrap();
    let table = ib
        .storage()
        .tables()
        .iter()
        .find(|t| t.query_name == format!("Catalog.{name}"))
        .unwrap()
        .clone();
    drop(ib);
    let db = onecd::Database::open(
        &dir.join("1Cv8.1CD"),
        onecd::OpenOptions {
            locking: onecd::Locking::None,
            ..onecd::OpenOptions::read_write()
        },
    )
    .unwrap();
    let mut write = db.write().unwrap();
    let mut target = write.table(&table.phys).unwrap();
    let mut removed = Vec::new();
    for n in 1..=count {
        let mut row = table
            .columns
            .iter()
            .map(|c| {
                if c.part == ibschema::columns::Part::Type {
                    Value::Binary(vec![ibschema::values::type_code::UNDEFINED])
                } else if c.ty == ibschema::columns::ColType::RV {
                    Value::Null
                } else {
                    infobase::colval::to_onecd(&fields::empty_value(c), c.ty).unwrap()
                }
            })
            .collect::<Vec<_>>();
        for (i, field) in table.fields.iter().enumerate() {
            let value = match field.std {
                Some(StdField::Id) => Some(FieldValue::Ref {
                    table: table.number,
                    id: ibschema::values::RefId((n as u128).to_be_bytes()),
                }),
                Some(StdField::Folder) => Some(FieldValue::Bool(n != 2)),
                Some(StdField::Code) => Some(FieldValue::Number(Numeric::from_i64(
                    ((n - 1) / group + 1) as i64,
                ))),
                Some(StdField::Description) => Some(FieldValue::String(format!("Товар ёжий {n}"))),
                _ if field.md_name.ends_with(".N38_0") => Some(FieldValue::Number(
                    Numeric::parse("12345678901234567890123456789012345678").unwrap(),
                )),
                _ if field.md_name.ends_with(".N32_8") => Some(FieldValue::Number(
                    Numeric::parse("12345678901234567890.12345678").unwrap(),
                )),
                _ => extra(n, &field.md_name),
            };
            if let Some(value) = value {
                let cols = fields::encode(&table, FieldId(i as u16), &value).unwrap();
                for (j, value) in field.columns.clone().zip(cols) {
                    row[j] = infobase::colval::to_onecd(&value, table.columns[j].ty).unwrap();
                }
            }
        }
        let id = target.insert(&row).unwrap();
        if holes && n % 3 == 0 {
            removed.push(id);
        }
    }
    for id in removed {
        target.delete(id).unwrap();
    }
    write.commit().unwrap();
}

#[allow(dead_code)]
pub fn replace_config_entry(dir: &Path, name: &str, stored: &[u8]) {
    let db = onecd::Database::open(
        &dir.join("1Cv8.1CD"),
        onecd::OpenOptions {
            locking: onecd::Locking::None,
            ..onecd::OpenOptions::read_write()
        },
    )
    .unwrap();
    let mut write = db.write().unwrap();
    let (rows, file, size, data) = {
        let read = write.read_table("CONFIG").unwrap();
        let descr = read.descr();
        (
            read.scan().map(Result::unwrap).collect::<Vec<_>>(),
            descr.field("FILENAME").unwrap().0,
            descr.field("DATASIZE").unwrap().0,
            descr.field("BINARYDATA").unwrap().0,
        )
    };
    let mut target = write.table("CONFIG").unwrap();
    let mut found = false;
    for (id, mut row) in rows {
        if matches!(&row[file], Value::VarChar(s) | Value::Char(s) if s.trim_end() == name) {
            row[size] = Value::Numeric(Numeric::from_i64(stored.len() as i64));
            row[data] = Value::Image(BlobVal::Data(stored.into()));
            target.update(id, &row).unwrap();
            found = true;
        }
    }
    assert!(found, "no CONFIG entry {name}");
    write.commit().unwrap();
}

#[allow(dead_code)]
pub fn main_configuration_entry() -> String {
    let cf = mdconf::CfFile::open(&cf_fixtures::fixture_dir("types").join("p.cf")).unwrap();
    let stored = cf.stored("root").unwrap().unwrap();
    let raw = zraw::try_inflate(&stored, zraw::DEFAULT_LIMIT).unwrap_or(stored);
    let text = String::from_utf8_lossy(&raw);
    text.split(',').nth(1).unwrap().trim().to_string()
}

pub fn create_empty(root: &Path) -> PathBuf {
    let dir = root.join("Пустая база ёй");
    infobase::create::create_empty(&dir, &Default::default()).unwrap();
    dir
}
