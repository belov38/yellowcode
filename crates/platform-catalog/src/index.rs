use crate::model::*;
use memory_budget::BudgetError;
use std::ops::Range;

#[derive(Default)]
struct Names<T> {
    entries: Vec<(String, T)>,
}
impl<T: Copy + Ord> Names<T> {
    fn get(&self, name: &str) -> Option<&T> {
        let i = self
            .entries
            .partition_point(|(key, _)| compare_name(key, name).is_lt());
        self.entries
            .get(i)
            .filter(|(key, _)| compare_name(key, name).is_eq())
            .map(|(_, value)| value)
    }
    fn finish(&mut self) {
        self.entries.sort_unstable();
    }
    fn pair(&mut self, ru: &str, en: &str, index: T) -> Result<(), BudgetError> {
        self.entries.push((fold_exact(ru)?, index));
        self.entries.push((fold_exact(en)?, index));
        Ok(())
    }
}
fn compare_name(key: &str, query: &str) -> std::cmp::Ordering {
    key.chars().cmp(query.chars().map(folded))
}
fn folded(c: char) -> char {
    let mut lower = c.to_lowercase();
    match (lower.next(), lower.next()) {
        (Some(c), None) => c,
        _ => c,
    }
}
fn fold_len(name: &str) -> usize {
    name.chars().map(|c| folded(c).len_utf8()).sum()
}
fn fold_exact(name: &str) -> Result<String, BudgetError> {
    let mut text = String::new();
    text.try_reserve_exact(fold_len(name))
        .map_err(|_| BudgetError::AllocationFailed)?;
    text.extend(name.chars().map(folded));
    Ok(text)
}
fn entries<T>(count: usize) -> Result<Vec<T>, BudgetError> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(count)
        .map_err(|_| BudgetError::AllocationFailed)?;
    Ok(values)
}
#[derive(Default)]
pub(crate) struct Indexes {
    types_by_name: Vec<(String, Range<usize>)>,
    type_ids: Vec<TypeIdx>,
    types_by_guid: Vec<([u8; 16], TypeIdx)>,
    properties: Vec<Names<usize>>,
    methods: Vec<Names<usize>>,
    enum_values: Vec<Names<usize>>,
    global_props: Names<usize>,
    global_methods: Names<usize>,
    builtins: Names<usize>,
}
impl Indexes {
    fn types(&self, name: &str) -> &[TypeIdx] {
        let i = self
            .types_by_name
            .partition_point(|(key, _)| compare_name(key, name).is_lt());
        self.types_by_name
            .get(i)
            .filter(|(key, _)| compare_name(key, name).is_eq())
            .and_then(|(_, range)| self.type_ids.get(range.clone()))
            .unwrap_or(&[])
    }
    pub(crate) fn build(data: &CatalogData) -> Self {
        match Self::try_build(data) {
            Ok(indexes) => indexes,
            Err(_) => std::alloc::handle_alloc_error(std::alloc::Layout::new::<Self>()),
        }
    }
    pub(crate) fn try_build(data: &CatalogData) -> Result<Self, BudgetError> {
        let count = data
            .types
            .iter()
            .take(usize::from(u16::MAX) + 1)
            .map(|t| 2 + t.alt_names.len())
            .sum();
        let mut names = Names {
            entries: entries(count)?,
        };
        let mut ix = Self {
            types_by_name: entries(count)?,
            type_ids: entries(count)?,
            types_by_guid: entries(data.types.len())?,
            properties: entries(data.types.len())?,
            methods: entries(data.types.len())?,
            enum_values: entries(data.types.len())?,
            global_props: Names {
                entries: entries(data.global_properties.len() * 2)?,
            },
            global_methods: Names {
                entries: entries(data.global_methods.len() * 2)?,
            },
            builtins: Names {
                entries: entries(
                    data.global_methods
                        .iter()
                        .filter(|m| m.builtin && m.server_known)
                        .count()
                        * 2,
                )?,
            },
        };
        for (i, t) in data.types.iter().enumerate() {
            let mut props = Names {
                entries: entries(t.properties.len() * 2)?,
            };
            for (k, p) in t.properties.iter().enumerate() {
                props.pair(&p.name_ru, &p.name_en, k)?;
            }
            props.finish();
            ix.properties.push(props);
            let mut methods = Names {
                entries: entries(t.methods.len() * 2)?,
            };
            for (k, m) in t.methods.iter().enumerate() {
                methods.pair(&m.name_ru, &m.name_en, k)?;
            }
            methods.finish();
            ix.methods.push(methods);
            let mut values = Names {
                entries: entries(t.values.len() * 2)?,
            };
            for (k, v) in t.values.iter().enumerate() {
                values.pair(&v.name_ru, &v.name_en, k)?;
            }
            values.finish();
            ix.enum_values.push(values);
            let Ok(i) = u16::try_from(i) else { continue };
            let idx = TypeIdx(i);
            names.pair(&t.name_ru, &t.name_en, idx)?;
            for alt in &t.alt_names {
                names.entries.push((fold_exact(alt)?, idx));
            }
            if let Some(guid) = t.guid {
                ix.types_by_guid.push((guid, idx));
            }
        }
        names.finish();
        names.entries.dedup();
        for (name, idx) in names.entries {
            let start = ix.type_ids.len();
            ix.type_ids.push(idx);
            if let Some((previous, range)) = ix.types_by_name.last_mut()
                && previous == &name
            {
                range.end += 1;
                continue;
            }
            ix.types_by_name.push((name, start..start + 1));
        }
        ix.types_by_guid.sort_unstable();
        for (k, p) in data.global_properties.iter().enumerate() {
            ix.global_props.pair(&p.name_ru, &p.name_en, k)?;
        }
        for (k, m) in data.global_methods.iter().enumerate() {
            ix.global_methods
                .pair(&m.info.name_ru, &m.info.name_en, k)?;
            if m.builtin && m.server_known {
                ix.builtins.pair(&m.info.name_ru, &m.info.name_en, k)?;
            }
        }
        ix.global_props.finish();
        ix.global_methods.finish();
        ix.builtins.finish();
        Ok(ix)
    }
    pub(crate) fn bytes(data: &CatalogData) -> Result<u64, BudgetError> {
        let mut total = 0u64;
        fn add(total: &mut u64, bytes: usize) -> Result<(), BudgetError> {
            *total = total
                .checked_add(u64::try_from(bytes).map_err(|_| BudgetError::Overflow)?)
                .ok_or(BudgetError::Overflow)?;
            Ok(())
        }
        fn slots<T>(total: &mut u64, n: usize) -> Result<(), BudgetError> {
            add(
                total,
                n.checked_mul(std::mem::size_of::<T>())
                    .ok_or(BudgetError::Overflow)?,
            )
        }
        fn key<T>(total: &mut u64, name: &str) -> Result<(), BudgetError> {
            slots::<(String, T)>(total, 1)?;
            add(total, fold_len(name))
        }
        slots::<Names<usize>>(
            &mut total,
            data.types
                .len()
                .checked_mul(3)
                .ok_or(BudgetError::Overflow)?,
        )?;
        slots::<([u8; 16], TypeIdx)>(&mut total, data.types.len())?;
        for (i, t) in data.types.iter().enumerate() {
            if u16::try_from(i).is_ok() {
                for name in std::iter::once(&t.name_ru)
                    .chain(std::iter::once(&t.name_en))
                    .chain(&t.alt_names)
                {
                    key::<TypeIdx>(&mut total, name)?;
                    slots::<(String, Range<usize>)>(&mut total, 1)?;
                    slots::<TypeIdx>(&mut total, 1)?;
                }
            }
            for p in &t.properties {
                key::<usize>(&mut total, &p.name_ru)?;
                key::<usize>(&mut total, &p.name_en)?;
            }
            for m in &t.methods {
                key::<usize>(&mut total, &m.name_ru)?;
                key::<usize>(&mut total, &m.name_en)?;
            }
            for v in &t.values {
                key::<usize>(&mut total, &v.name_ru)?;
                key::<usize>(&mut total, &v.name_en)?;
            }
        }
        for p in &data.global_properties {
            key::<usize>(&mut total, &p.name_ru)?;
            key::<usize>(&mut total, &p.name_en)?;
        }
        for m in &data.global_methods {
            key::<usize>(&mut total, &m.info.name_ru)?;
            key::<usize>(&mut total, &m.info.name_en)?;
            if m.builtin && m.server_known {
                key::<usize>(&mut total, &m.info.name_ru)?;
                key::<usize>(&mut total, &m.info.name_en)?;
            }
        }
        Ok(total)
    }
}

pub fn is_platform_build(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= crate::codec::MAX_PLATFORM
        && s.split('.')
            .all(|g| !g.is_empty() && g.bytes().all(|b| b.is_ascii_digit()))
}

pub struct Catalog {
    pub(crate) data: CatalogData,
    pub(crate) ix: Indexes,
    pub(crate) platform: String,
}

impl Catalog {
    pub fn from_data(data: CatalogData) -> Catalog {
        let ix = Indexes::build(&data);
        Catalog {
            data,
            ix,
            platform: String::new(),
        }
    }

    pub fn with_platform(mut self, build: &str) -> Result<Catalog, crate::DecodeError> {
        if !is_platform_build(build) {
            return Err(crate::DecodeError(format!(
                "invalid platform build {build:?}"
            )));
        }
        self.platform = build.to_owned();
        Ok(self)
    }

    pub fn platform(&self) -> &str {
        &self.platform
    }

    pub fn data(&self) -> &CatalogData {
        &self.data
    }

    pub fn types(&self) -> &[TypeInfo] {
        &self.data.types
    }

    pub fn type_info(&self, t: TypeIdx) -> &TypeInfo {
        &self.data.types[usize::from(t.0)]
    }

    pub fn find_types(&self, name: &str) -> &[TypeIdx] {
        self.ix.types(name)
    }

    pub fn find_type(&self, name: &str) -> Option<TypeIdx> {
        self.find_type_folded(name)
    }

    pub fn find_type_folded(&self, folded: &str) -> Option<TypeIdx> {
        let all = self.ix.types(folded);
        all.iter()
            .copied()
            .find(|t| self.type_info(*t).presentation_ru.is_some())
            .or_else(|| all.first().copied())
    }

    pub fn find_type_by_guid(&self, guid: &[u8; 16]) -> Option<TypeIdx> {
        let i = self.ix.types_by_guid.partition_point(|(key, _)| key < guid);
        self.ix
            .types_by_guid
            .get(i)
            .filter(|(key, _)| key == guid)
            .map(|(_, value)| *value)
    }

    pub fn find_member(&self, t: TypeIdx, name: &str) -> Option<Member<'_>> {
        self.find_property(t, name)
            .map(Member::Property)
            .or_else(|| self.find_method(t, name).map(Member::Method))
    }

    pub fn find_property(&self, t: TypeIdx, name: &str) -> Option<&PropertyInfo> {
        let info = self.data.types.get(usize::from(t.0))?;
        let k = *self.ix.properties.get(usize::from(t.0))?.get(name)?;
        info.properties.get(k)
    }

    pub fn find_method(&self, t: TypeIdx, name: &str) -> Option<&MethodInfo> {
        let info = self.data.types.get(usize::from(t.0))?;
        let k = *self.ix.methods.get(usize::from(t.0))?.get(name)?;
        info.methods.get(k)
    }

    pub fn find_enum_value(&self, t: TypeIdx, name: &str) -> Option<usize> {
        self.ix
            .enum_values
            .get(usize::from(t.0))?
            .get(name)
            .copied()
    }

    pub fn global_properties(&self) -> &[PropertyInfo] {
        &self.data.global_properties
    }

    pub fn find_global_property(&self, name: &str) -> Option<&PropertyInfo> {
        let k = *self.ix.global_props.get(name)?;
        self.data.global_properties.get(k)
    }

    pub fn find_global_property_idx(&self, name: &str) -> Option<u16> {
        let k = *self.ix.global_props.get(name)?;
        u16::try_from(k).ok()
    }

    pub fn global_methods(&self) -> &[GlobalMethod] {
        &self.data.global_methods
    }

    pub fn find_global_method(&self, name: &str) -> Option<&GlobalMethod> {
        let k = *self.ix.global_methods.get(name)?;
        self.data.global_methods.get(k)
    }

    pub fn find_global_method_idx(&self, name: &str) -> Option<u16> {
        let k = *self.ix.global_methods.get(name)?;
        u16::try_from(k).ok()
    }

    pub fn builtin(&self, name: &str) -> Option<&GlobalMethod> {
        let k = *self.ix.builtins.get(name)?;
        self.data.global_methods.get(k)
    }

    pub fn query_tables(&self) -> &[QueryTableInfo] {
        &self.data.query_tables
    }

    pub fn query_table_groups(&self) -> &[QueryTableGroup] {
        &self.data.query_table_groups
    }
}
