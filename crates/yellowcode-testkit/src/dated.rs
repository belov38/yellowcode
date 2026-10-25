use ibschema::{fields, fields::FieldValue, names::StdField, storage::FieldId};
use onecd::{DateTime, Value};
use std::path::{Path, PathBuf};

pub const DOCUMENT: &str = "Document.Invoice";
pub const REGISTER: &str = "AccumulationRegister.Stock";

pub fn create(root: &Path) -> PathBuf {
    crate::base::create(root)
}

#[allow(dead_code)]
pub fn rename_document(dir: &Path, name: &str) {
    use mdconf::EntrySource;
    let cf = mdconf::CfFile::open(&cf_fixtures::fixture_dir("types").join("p.cf")).unwrap();
    let mut renamed = 0;
    for entry in cf.list().unwrap() {
        let stored = cf.stored(&entry.name).unwrap().unwrap();
        if let Some(raw) = zraw::try_inflate(&stored, zraw::DEFAULT_LIMIT)
            && let Ok(text) = String::from_utf8(raw)
            && text.contains("Invoice")
        {
            let stored =
                zraw::deflate(text.replace("Invoice", name).as_bytes(), zraw::LEVEL_1C).unwrap();
            crate::base::replace_config_entry(dir, &entry.name, &stored);
            renamed += 1;
        }
    }
    assert!(renamed > 0, "no fixture entry names Invoice");
}

pub fn populate(dir: &Path, query_name: &str, dates: &[DateTime]) {
    populate_numbered(dir, query_name, dates, &[]);
}

pub fn populate_numbered(dir: &Path, query_name: &str, dates: &[DateTime], numbers: &[&str]) {
    let ib = infobase::Infobase::open(
        dir,
        infobase::OpenOptions {
            schema_check: infobase::SchemaCheck::Warn,
            locking: onecd::Locking::None,
            ..Default::default()
        },
    )
    .unwrap();
    let tables = ib.storage().tables();
    let find = |name: &str| {
        tables
            .iter()
            .find(|t| t.query_name == name)
            .unwrap_or_else(|| {
                let names: Vec<_> = tables.iter().map(|t| t.query_name.as_str()).collect();
                panic!("no table {name} among {names:?}")
            })
            .clone()
    };
    let table = find(query_name);
    let document = find(DOCUMENT).number;
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
    for (n, date) in dates.iter().enumerate().map(|(i, d)| (i + 1, *d)) {
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
        let reference = |table: u32| FieldValue::Ref {
            table,
            id: ibschema::values::RefId((n as u128).to_be_bytes()),
        };
        for (i, field) in table.fields.iter().enumerate() {
            let value = match field.std {
                Some(StdField::Id) => reference(table.number),
                Some(StdField::DateTime | StdField::Period) => FieldValue::Date(date),
                Some(StdField::Recorder) => reference(document),
                Some(StdField::LineNo) => FieldValue::Number(onecd::Numeric::from_i64(n as i64)),
                Some(StdField::Active) => FieldValue::Bool(true),
                Some(StdField::Number) if n <= numbers.len() => {
                    FieldValue::String(numbers[n - 1].to_owned())
                }
                _ => continue,
            };
            let cols = fields::encode(&table, FieldId(i as u16), &value).unwrap();
            for (j, value) in field.columns.clone().zip(cols) {
                row[j] = infobase::colval::to_onecd(&value, table.columns[j].ty).unwrap();
            }
        }
        target.insert(&row).unwrap();
    }
    write.commit().unwrap();
}

pub fn date(text: &str) -> DateTime {
    let n = |r: std::ops::Range<usize>| text[r].parse::<u16>().unwrap();
    DateTime::new(
        n(0..4),
        n(5..7) as u8,
        n(8..10) as u8,
        n(11..13) as u8,
        n(14..16) as u8,
        n(17..19) as u8,
    )
    .unwrap()
}
