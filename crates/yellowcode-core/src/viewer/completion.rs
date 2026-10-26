pub mod analysis;
mod metadata;

use super::Lang;
use bsl_prim::fold_name;
use mdconf::Configuration;
use platform_catalog::{Context, MethodInfo, Signature, TypeInfo, TypeKind, catalog};
use serde::Serialize;
use unicode_normalization::UnicodeNormalization;

pub const MAX_ITEMS: usize = 200;
pub const MAX_SEGMENTS: usize = 16;
pub const MAX_NAME: usize = 128;

#[derive(Debug, Serialize)]
pub struct CompletionSignature {
    pub parameters: Vec<String>,
    pub variadic: bool,
}
#[derive(Debug, Serialize)]
pub struct CompletionItem {
    pub name: String,
    pub name_other: String,
    pub kind: &'static str,
    pub signatures: Vec<CompletionSignature>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}
#[derive(Debug, Default, Serialize)]
pub struct CompletionList {
    pub items: Vec<CompletionItem>,
    pub total: usize,
}

pub fn valid_name(s: &str) -> bool {
    s.chars().count() <= MAX_NAME && s.chars().all(|c| c.is_alphanumeric() || c == '_')
}
pub(super) fn same(a: &str, b: &str) -> bool {
    fold_name(&a.nfc().collect::<String>()) == fold_name(&b.nfc().collect::<String>())
}
fn pick(ru: &str, en: &str, lang: Lang) -> (String, String) {
    if lang == Lang::Ru {
        (ru.into(), en.into())
    } else {
        (en.into(), ru.into())
    }
}
impl CompletionItem {
    pub(super) fn named(ru: &str, en: &str, kind: &'static str, lang: Lang) -> Self {
        let (name, name_other) = pick(ru, en, lang);
        let name = name.nfc().collect();
        let name_other = name_other.nfc().collect();
        Self {
            name,
            name_other,
            kind,
            signatures: Vec::new(),
            note: None,
        }
    }
    fn signatures(mut self, signatures: &[Signature], lang: Lang) -> Self {
        self.signatures = signatures
            .iter()
            .map(|s| CompletionSignature {
                parameters: s
                    .params
                    .iter()
                    .map(|p| {
                        let mut n = pick(&p.name_ru, &p.name_en, lang).0;
                        if p.variadic {
                            n.push_str("...");
                        }
                        if p.optional {
                            n = format!("[{n}]");
                        }
                        n
                    })
                    .collect(),
                variadic: s.params.last().is_some_and(|p| p.variadic),
            })
            .collect();
        self
    }
    fn method(m: &MethodInfo, lang: Lang) -> Self {
        let mut item =
            Self::named(&m.name_ru, &m.name_en, "function", lang).signatures(&m.variants, lang);
        if item.signatures.is_empty() {
            item.signatures.push(CompletionSignature {
                parameters: Vec::new(),
                variadic: false,
            });
        }
        item
    }
}

pub(super) struct Builder {
    prefix: String,
    pub lang: Lang,
    result: CompletionList,
    seen: std::collections::HashSet<String>,
    limit: usize,
}
impl Builder {
    pub fn add(&mut self, item: CompletionItem) {
        if item.name.is_empty() || !valid_name(&item.name) {
            return;
        }
        let key = fold_name(&item.name);
        if !(key.starts_with(&self.prefix) || fold_name(&item.name_other).starts_with(&self.prefix))
        {
            return;
        }
        if self.seen.contains(&key) {
            return;
        }
        self.result.total += 1;
        if self.result.items.len() < self.limit {
            self.seen.insert(key);
            self.result.items.push(item);
        }
    }
    pub fn members(&mut self, t: &TypeInfo) {
        if !t.avail.has(Context::Server) {
            return;
        }
        for p in &t.properties {
            if p.avail.has(Context::Server) {
                self.add(CompletionItem::named(
                    &p.name_ru, &p.name_en, "property", self.lang,
                ));
            }
        }
        for m in &t.methods {
            if m.avail.has(Context::Server) {
                self.add(CompletionItem::method(m, self.lang));
            }
        }
    }
    pub fn type_members(&mut self, name: &str) {
        let cat = catalog();
        if let Some(idx) = cat.find_type(name) {
            self.members(cat.type_info(idx));
        }
    }
}

pub fn complete(
    cfg: &Configuration,
    path: &[String],
    prefix: &str,
    constructors: bool,
    lang: Lang,
) -> CompletionList {
    if path.len() > MAX_SEGMENTS
        || !valid_name(prefix)
        || path.iter().any(|s| s.is_empty() || !valid_name(s))
    {
        return CompletionList::default();
    }
    let mut out = Builder {
        prefix: fold_name(prefix),
        lang,
        result: CompletionList::default(),
        seen: Default::default(),
        limit: MAX_ITEMS,
    };
    let cat = catalog();
    if constructors && path.is_empty() {
        for t in cat.types() {
            if t.kind == TypeKind::Class
                && !t.metadata_template
                && t.avail.has(Context::Server)
                && !t.constructors.is_empty()
            {
                out.add(
                    CompletionItem::named(&t.name_ru, &t.name_en, "class", lang)
                        .signatures(&t.constructors, lang),
                );
            }
        }
    } else if path.is_empty() {
        for m in cat.global_methods() {
            if m.info.avail.has(Context::Server) {
                out.add(CompletionItem::method(&m.info, lang));
            }
        }
        for p in cat.global_properties() {
            if p.avail.has(Context::Server) {
                out.add(CompletionItem::named(
                    &p.name_ru, &p.name_en, "property", lang,
                ));
            }
        }
        for t in cat.types() {
            if t.kind == TypeKind::Enum && t.enum_global && t.avail.has(Context::Server) {
                out.add(CompletionItem::named(&t.name_ru, &t.name_en, "enum", lang));
            }
        }
    } else if !constructors {
        metadata::members(cfg, path, &mut out);
    }
    out.result.items.sort_by(|a, b| a.name.cmp(&b.name));
    out.result
}
