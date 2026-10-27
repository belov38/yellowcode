use super::{CHILD_TAGS, KIND_RU_FALLBACK, PREFIX_EN, PREFIX_RU, child_tag_en, sub_kind_en};
use crate::viewer::{Lang, types};
use mdconf::{
    ChildTag, DateFractions, FixedNamespace, MdKind, SubKind, TypeDescription, XmlTypeName,
};
use memory_budget::{BudgetError, TextOutput};
use platform_catalog::ManagedCatalog;

pub struct ManagedNames<'a> {
    catalog: &'a ManagedCatalog,
}
impl<'a> ManagedNames<'a> {
    pub fn new(catalog: &'a ManagedCatalog) -> Self {
        Self { catalog }
    }
    pub fn memory_budget(&self) -> &memory_budget::MemoryBudget {
        self.catalog.memory_budget()
    }
    fn type_name_ru(&self, en: &str) -> Option<&'a str> {
        let index = self.catalog.find_type_parts(&[PREFIX_EN, en])?;
        let ru = self.catalog.type_info(index)?.name_ru;
        Some(ru.strip_prefix(PREFIX_RU).unwrap_or(ru))
    }
    fn kind_ru(&self, kind: MdKind) -> Option<&'a str> {
        self.type_name_ru(kind.as_str()).or_else(|| {
            KIND_RU_FALLBACK
                .iter()
                .find(|(en, _)| *en == kind.as_str())
                .map(|(_, ru)| *ru)
        })
    }
    pub fn kind_name(&self, kind: MdKind, lang: Lang) -> &'a str {
        if lang == Lang::Ru {
            self.kind_ru(kind).unwrap_or(kind.as_str())
        } else {
            kind.as_str()
        }
    }
    pub fn section_name(&self, kind: MdKind, lang: Lang) -> &'a str {
        let Some(folder) = kind.folder() else {
            return kind.as_str();
        };
        if lang == Lang::En {
            return folder;
        }
        self.catalog
            .find_type("ConfigurationMetadataObject")
            .and_then(|index| self.catalog.find_property(index, folder))
            .map(|p| p.name_ru)
            .unwrap_or(folder)
    }
    fn by_lang<'b>(&'b self, en: &'b str, lang: Lang) -> &'b str {
        if lang == Lang::Ru {
            self.type_name_ru(en).unwrap_or(en)
        } else {
            en
        }
    }
    pub fn child_tag_name(&self, tag: ChildTag, lang: Lang) -> &str {
        self.by_lang(child_tag_en(tag), lang)
    }
    pub fn sub_kind_name(&self, kind: SubKind, lang: Lang) -> &str {
        self.by_lang(sub_kind_en(kind), lang)
    }
    pub fn property_name_of<'b>(&'b self, type_en: &str, xml: &'b str, lang: Lang) -> &'b str {
        let index = self
            .catalog
            .find_type_parts(&[PREFIX_EN, type_en])
            .or_else(|| {
                if type_en == "Configuration" {
                    self.catalog.find_type("ConfigurationMetadataObject")
                } else {
                    None
                }
            });
        index
            .and_then(|index| self.catalog.find_property(index, xml))
            .map(|p| {
                if lang == Lang::Ru {
                    p.name_ru
                } else {
                    p.name_en
                }
            })
            .unwrap_or(xml)
    }
    pub fn kind_from_name(&self, text: &str) -> Option<MdKind> {
        MdKind::all().iter().copied().find(|kind| {
            eq_path(kind.as_str(), text) || self.kind_ru(*kind).is_some_and(|ru| eq_path(ru, text))
        })
    }
    pub fn child_tag_from_name(&self, text: &str) -> Option<ChildTag> {
        CHILD_TAGS.into_iter().find(|tag| {
            let en = child_tag_en(*tag);
            eq_path(en, text) || self.type_name_ru(en).is_some_and(|ru| eq_path(ru, text))
        })
    }
    pub fn sub_kind_from_name(&self, text: &str) -> Option<SubKind> {
        [SubKind::Form, SubKind::Template].into_iter().find(|kind| {
            let en = sub_kind_en(*kind);
            eq_path(en, text) || self.type_name_ru(en).is_some_and(|ru| eq_path(ru, text))
        })
    }
    fn category_ru(&self, name: &str) -> Option<&'a str> {
        self.catalog
            .types()
            .find_map(|ty| {
                let (en, _) = ty.name_en.split_once(".<")?;
                let (ru, _) = ty.name_ru.split_once(".<")?;
                (en == name).then_some(ru)
            })
            .or_else(|| {
                self.catalog
                    .find_type(name)
                    .and_then(|index| self.catalog.type_info(index))
                    .map(|ty| ty.name_ru)
            })
            .or_else(|| {
                types::FALLBACK_RU
                    .iter()
                    .find(|(en, _)| *en == name)
                    .map(|(_, ru)| *ru)
            })
    }
    pub fn write_type_text<O: TextOutput + std::fmt::Write>(
        &self,
        description: &TypeDescription,
        lang: Lang,
        out: &mut O,
    ) -> Result<(), BudgetError> {
        for (i, reference) in description.types.iter().enumerate() {
            if i != 0 {
                out.try_push_str(", ")?;
            }
            let ru = lang == Lang::Ru;
            let name = &reference.name;
            let fixed = FixedNamespace::from_uri(name.namespace_uri()).is_some();
            let fmt_result = match PRIMITIVE_QNAMES.iter().find(|q| name.is_qname(q)).copied() {
                Some(qname @ "xs:string") => {
                    let name = types::primitive(qname, lang).unwrap_or("String");
                    match description.string {
                        Some(q) if q.length > 0 => write!(out, "{name}({})", q.length),
                        _ => {
                            out.try_push_str(name)?;
                            Ok(())
                        }
                    }
                }
                Some(qname @ "xs:decimal") => {
                    let name = types::primitive(qname, lang).unwrap_or("Number");
                    match description.number {
                        Some(q) => {
                            let sign = match (q.nonnegative, ru) {
                                (true, true) => ", неотр.",
                                (true, false) => ", nonnegative",
                                _ => "",
                            };
                            write!(out, "{name}({}, {}{sign})", q.digits, q.fraction)
                        }
                        None => {
                            out.try_push_str(name)?;
                            Ok(())
                        }
                    }
                }
                Some("xs:dateTime") => {
                    out.try_push_str(match (description.date, ru) {
                        (Some(DateFractions::Date), true) => "Дата",
                        (Some(DateFractions::Time), true) => "Время",
                        (Some(DateFractions::Date), false) => "Date",
                        (Some(DateFractions::Time), false) => "Time",
                        (_, true) => "ДатаВремя",
                        (_, false) => "DateTime",
                    })?;
                    Ok(())
                }
                Some(other) => {
                    out.try_push_str(types::primitive(other, lang).unwrap_or(other))?;
                    Ok(())
                }
                None if !fixed => {
                    self.write_foreign_type(name, ru, out)?;
                    Ok(())
                }
                None => {
                    let local = name.local.as_str();
                    if !ru {
                        out.try_push_str(local)?;
                    } else {
                        let (prefix, rest) = local
                            .split_once('.')
                            .map_or((local, None), |(p, r)| (p, Some(r)));
                        if let Some(prefix) = self.category_ru(prefix) {
                            out.try_push_str(prefix)?;
                            if let Some(rest) = rest {
                                out.try_push_str(".")?;
                                out.try_push_str(rest)?;
                            }
                        } else {
                            out.try_push_str(local)?;
                        }
                    }
                    Ok(())
                }
            };
            fmt_result.map_err(|_| out.error().copied().unwrap_or(BudgetError::Overflow))?;
        }
        Ok(())
    }
    fn write_foreign_type<O: TextOutput>(
        &self,
        name: &XmlTypeName,
        ru: bool,
        out: &mut O,
    ) -> Result<(), BudgetError> {
        let uri = name.namespace_uri();
        if let Some((_, tail)) = uri.split_once(':') {
            let category = ru
                .then(|| tail.split_once('.'))
                .flatten()
                .and_then(|(prefix, rest)| Some((self.category_ru(prefix)?, rest)));
            if let Some((prefix, rest)) = category {
                out.try_push_str(prefix)?;
                out.try_push_str(".")?;
                out.try_push_str(rest)?;
            } else {
                out.try_push_str(tail)?;
            }
        } else {
            out.try_push_str("{")?;
            out.try_push_str(uri)?;
        }
        out.try_push_str("}")?;
        out.try_push_str(&name.local)
    }
}
const PRIMITIVE_QNAMES: [&str; 6] = [
    "xs:string",
    "xs:decimal",
    "xs:boolean",
    "xs:dateTime",
    "v8:ValueStorage",
    "v8:UUID",
];
pub(crate) fn folded_path(text: &str) -> impl Iterator<Item = char> + '_ {
    text.trim().chars().map(|c| {
        let mut lower = c.to_lowercase();
        let folded = match (lower.next(), lower.next()) {
            (Some(one), None) => one,
            _ => c,
        };
        if folded == 'ё' { 'е' } else { folded }
    })
}
pub(crate) fn eq_path(a: &str, b: &str) -> bool {
    folded_path(a).eq(folded_path(b))
}
