use super::resources::{self, Graph, GuiResourceError};
use memory_budget::{BudgetError, MemoryBudget};

use mdconf::{DateFractions, TypeDescription, TypeRef};
use platform_catalog::catalog;

use super::out::Lang;

#[doc(hidden)]
#[derive(Clone, Copy)]
pub struct CategoryEntry {
    pub en: &'static str,
    pub ru: &'static str,
    ordinal: usize,
}
static CATEGORIES: Graph<Vec<CategoryEntry>> = Graph::new();
fn category_map(
    cat: &'static platform_catalog::Catalog,
    root: Option<&MemoryBudget>,
) -> Result<&'static Vec<CategoryEntry>, BudgetError> {
    let source = || {
        cat.types().iter().enumerate().filter_map(|(ordinal, t)| {
            let (en, _) = t.name_en.split_once(".<")?;
            let (ru, _) = t.name_ru.split_once(".<")?;
            Some(CategoryEntry { en, ru, ordinal })
        })
    };
    CATEGORIES.prepare(
        root,
        || {
            let mut n = 0;
            resources::rows::<CategoryEntry>(&mut n, source().count())?;
            Ok(n)
        },
        |rows| {
            let mut n = 0;
            resources::rows::<CategoryEntry>(&mut n, rows.capacity())?;
            Ok(n)
        },
        || {
            let mut rows = resources::vec(source().count())?;
            rows.extend(source());
            rows.sort_unstable_by(|a, b| a.en.cmp(b.en).then(a.ordinal.cmp(&b.ordinal)));
            Ok(rows)
        },
    )
}
#[allow(
    clippy::panic,
    reason = "legacy infallible accessor; managed readiness is fallible"
)]
fn category_ru() -> &'static [CategoryEntry] {
    match category_map(catalog(), None) {
        Ok(rows) => rows,
        Err(e) => panic!("type categories: {e}"),
    }
}
fn category_name(name: &str) -> Option<&'static str> {
    category_lookup(category_ru(), name)
}
fn category_lookup(rows: &[CategoryEntry], name: &str) -> Option<&'static str> {
    let i = rows.partition_point(|r| r.en < name);
    rows.get(i).filter(|r| r.en == name).map(|r| r.ru)
}
pub fn prepare_with_memory(budget: &MemoryBudget) -> Result<(), GuiResourceError> {
    let (cat, root) = platform_catalog::catalog_with_memory(budget)?;
    category_map(cat, Some(&root))?;
    Ok(())
}
#[cfg(feature = "testing")]
#[doc(hidden)]
pub fn diagnostic_categories() -> &'static [CategoryEntry] {
    category_ru()
}

pub(super) const FALLBACK_RU: &[(&str, &str)] = &[
    ("DefinedType", "ОпределяемыйТип"),
    ("Characteristic", "Характеристика"),
    ("AnyIBRef", "ЛюбаяСсылка"),
    ("AnyRef", "ЛюбаяСсылка"),
    ("ValueListType", "СписокЗначений"),
    ("SettingsComposer", "КомпоновщикНастроек"),
];

fn plain_ru(name: &str) -> Option<String> {
    let cat = catalog();
    cat.find_type(name)
        .map(|i| cat.type_info(i).name_ru.clone())
        .or_else(|| {
            FALLBACK_RU
                .iter()
                .find(|(en, _)| *en == name)
                .map(|(_, ru)| (*ru).to_string())
        })
}

pub(super) fn primitive(xml: &str, lang: Lang) -> Option<&'static str> {
    let (ru, en) = match xml {
        "xs:string" => ("Строка", "String"),
        "xs:decimal" => ("Число", "Number"),
        "xs:boolean" => ("Булево", "Boolean"),
        "xs:dateTime" => ("Дата", "Date"),
        "v8:ValueStorage" => ("ХранилищеЗначения", "ValueStorage"),
        "v8:UUID" => ("УникальныйИдентификатор", "UUID"),
        _ => return None,
    };
    Some(if lang == Lang::Ru { ru } else { en })
}

fn one(t: &TypeDescription, r: &TypeRef, lang: Lang) -> String {
    let ru = lang == Lang::Ru;
    let label = r.name.to_string();
    match label.as_str() {
        "xs:string" => {
            let name = primitive("xs:string", lang).unwrap_or("String");
            match t.string {
                Some(q) if q.length > 0 => format!("{name}({})", q.length),
                _ => name.to_string(),
            }
        }
        "xs:decimal" => {
            let name = primitive("xs:decimal", lang).unwrap_or("Number");
            match t.number {
                Some(q) => {
                    let sign = match (q.nonnegative, ru) {
                        (true, true) => ", неотр.",
                        (true, false) => ", nonnegative",
                        _ => "",
                    };
                    format!("{name}({}, {}{sign})", q.digits, q.fraction)
                }
                None => name.to_string(),
            }
        }
        "xs:dateTime" => match (t.date, ru) {
            (Some(DateFractions::Date), true) => "Дата".into(),
            (Some(DateFractions::Time), true) => "Время".into(),
            (Some(DateFractions::Date), false) => "Date".into(),
            (Some(DateFractions::Time), false) => "Time".into(),
            (_, true) => "ДатаВремя".into(),
            (_, false) => "DateTime".into(),
        },
        q => {
            if let Some(p) = primitive(q, lang) {
                return p.to_string();
            }
            let local = q.split_once(':').map_or(q, |(_, l)| l);
            if !ru {
                return local.to_string();
            }
            let (prefix, rest) = match local.split_once('.') {
                Some((p, r)) => (p, Some(r)),
                None => (local, None),
            };
            let ru_prefix = category_name(prefix)
                .map(str::to_owned)
                .or_else(|| plain_ru(prefix));
            match (ru_prefix, rest) {
                (Some(p), Some(r)) => format!("{p}.{r}"),
                (Some(p), None) => p,
                (None, _) => local.to_string(),
            }
        }
    }
}

pub fn type_text(t: &TypeDescription, lang: Lang) -> String {
    t.types
        .iter()
        .map(|r| one(t, r, lang))
        .collect::<Vec<_>>()
        .join(", ")
}
