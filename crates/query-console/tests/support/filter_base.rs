#![allow(dead_code)]
use ibschema::fields::{self, FieldValue};
use ibschema::names::StdField;
use ibschema::storage::{FieldId, TableInfo};
use mdconf::ConfigurationExt;
use mdconf::EntrySource;
use onecd::{BlobVal, DateTime, Numeric, Value, WriteTxn};
use std::path::{Path, PathBuf};

pub fn posted(n: u64) -> bool {
    !n.is_multiple_of(3)
}
pub fn invoice_marked(n: u64) -> bool {
    n.is_multiple_of(7)
}
pub fn goods_marked(n: u64) -> bool {
    n.is_multiple_of(5)
}
pub const GOODS_FOLDERS: [u64; 2] = [2, 5];

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

pub fn create(root: &Path) -> PathBuf {
    let cf = mdconf::CfFile::open(&cf_fixtures::fixture_dir("types").join("p.cf")).unwrap();
    let dir = root.join("База ёжий");
    infobase::create::create_empty(&dir, &Default::default()).unwrap();
    let cfg = mdconf::Configuration::load(&cf, &Default::default()).unwrap();
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
    let db = open_rw(&dir);
    let mut write = db.write().unwrap();
    for (table, owner) in built.schema.walk() {
        let descr =
            infobase::physical::table_descr(table, owner, infobase::physical::NameCase::Applied)
                .unwrap();
        if write.read_table(&descr.name).is_err() {
            write.create_table(&descr).unwrap();
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
        params
            .scan()
            .map(Result::unwrap)
            .filter_map(|(id, row)| {
                matches!(&row[index], Value::VarChar(s) | Value::Char(s) if s.eq_ignore_ascii_case("DBNames"))
                    .then_some(id)
            })
            .collect::<Vec<_>>()
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
    dir
}

fn open_rw(dir: &Path) -> onecd::Database {
    onecd::Database::open(
        &dir.join("1Cv8.1CD"),
        onecd::OpenOptions {
            locking: onecd::Locking::None,
            ..onecd::OpenOptions::read_write()
        },
    )
    .unwrap()
}

fn table_info(dir: &Path, query_name: &str) -> TableInfo {
    let ib = infobase::Infobase::open(
        dir,
        infobase::OpenOptions {
            schema_check: infobase::SchemaCheck::Warn,
            locking: onecd::Locking::None,
            ..Default::default()
        },
    )
    .unwrap();
    ib.storage()
        .tables()
        .iter()
        .find(|t| t.query_name == query_name)
        .unwrap_or_else(|| panic!("no table {query_name}"))
        .clone()
}

fn populate(
    dir: &Path,
    query_name: &str,
    count: u64,
    holes: bool,
    value: &dyn Fn(u64, StdField, &TableInfo) -> Option<FieldValue>,
) {
    populate_table(dir, &table_info(dir, query_name), count, holes, value);
}

fn populate_table(
    dir: &Path,
    table: &TableInfo,
    count: u64,
    holes: bool,
    value: &dyn Fn(u64, StdField, &TableInfo) -> Option<FieldValue>,
) {
    let db = open_rw(dir);
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
            let Some(v) = field.std.and_then(|std| value(n, std, table)) else {
                continue;
            };
            let cols = fields::encode(table, FieldId(i as u16), &v).unwrap();
            for (j, c) in field.columns.clone().zip(cols) {
                row[j] = infobase::colval::to_onecd(&c, table.columns[j].ty).unwrap();
            }
        }
        let id = target.insert(&row).unwrap();
        if holes && n.is_multiple_of(3) {
            removed.push(id);
        }
    }
    for id in removed {
        target.delete(id).unwrap();
    }
    write.commit().unwrap();
}

pub fn mark_predefined_initialized(dir: &Path) {
    let ib = infobase::Infobase::open(
        dir,
        infobase::OpenOptions {
            schema_check: infobase::SchemaCheck::Warn,
            locking: onecd::Locking::None,
            ..Default::default()
        },
    )
    .unwrap();
    let tables: Vec<TableInfo> = ib
        .storage()
        .tables()
        .iter()
        .filter(|t| t.field_by_std(StdField::PdInitialized).is_some())
        .cloned()
        .collect();
    drop(ib);
    assert!(!tables.is_empty(), "no table of the predefined data flag");
    for table in &tables {
        populate_table(dir, table, 1, false, &|_, std, _| {
            (std == StdField::PdInitialized).then_some(FieldValue::Bool(true))
        });
    }
}

fn reference(table: &TableInfo, n: u64) -> FieldValue {
    FieldValue::Ref {
        table: table.number,
        id: ibschema::values::RefId(u128::from(n).to_be_bytes()),
    }
}

pub fn populate_invoices(dir: &Path, count: u64, holes: bool) {
    populate_invoices_with(dir, count, holes, &posted, &invoice_marked);
}

pub fn populate_invoices_with(
    dir: &Path,
    count: u64,
    holes: bool,
    posted: &dyn Fn(u64) -> bool,
    marked: &dyn Fn(u64) -> bool,
) {
    populate(
        dir,
        "Document.Invoice",
        count,
        holes,
        &|n, std, t| match std {
            StdField::Id => Some(reference(t, n)),
            StdField::Posted => Some(FieldValue::Bool(posted(n))),
            StdField::Marked => Some(FieldValue::Bool(marked(n))),
            StdField::DateTime => {
                DateTime::new(2026, 1 + (n / 28 % 12) as u8, 1 + (n % 28) as u8, 10, 0, 0)
                    .map(FieldValue::Date)
            }
            StdField::Number => Some(FieldValue::String(format!("ё-{n:05}"))),
            _ => None,
        },
    );
}

pub fn populate_goods(dir: &Path, count: u64) {
    populate(dir, "Catalog.Goods", count, false, &|n, std, t| match std {
        StdField::Id => Some(reference(t, n)),
        StdField::Folder => Some(FieldValue::Bool(!GOODS_FOLDERS.contains(&n))),
        StdField::Marked => Some(FieldValue::Bool(goods_marked(n))),
        StdField::Code => Some(FieldValue::Number(Numeric::from_i64(n as i64))),
        StdField::Description => Some(FieldValue::String(format!("Товар ёжий {n}"))),
        StdField::ParentId if n == 6 || n == 7 => Some(reference(t, 5)),
        _ => None,
    });
}
