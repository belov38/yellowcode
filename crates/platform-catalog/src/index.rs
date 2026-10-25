use crate::model::*;
use bsl_prim::fold_name;
use std::collections::HashMap;

fn put<T: Copy>(map: &mut HashMap<String, T>, ru: &str, en: &str, idx: T) {
    map.entry(fold_name(ru)).or_insert(idx);
    map.entry(fold_name(en)).or_insert(idx);
}

#[derive(Default)]
pub(crate) struct Indexes {
    types_by_name: HashMap<String, Vec<TypeIdx>>,
    types_by_guid: HashMap<[u8; 16], TypeIdx>,
    properties: Vec<HashMap<String, usize>>,
    methods: Vec<HashMap<String, usize>>,
    enum_values: Vec<HashMap<String, usize>>,
    global_props: HashMap<String, usize>,
    global_methods: HashMap<String, usize>,
    builtins: HashMap<String, usize>,
}

impl Indexes {
    pub(crate) fn build(data: &CatalogData) -> Indexes {
        let mut ix = Indexes::default();
        for (i, t) in data.types.iter().enumerate() {
            let mut props = HashMap::new();
            for (k, p) in t.properties.iter().enumerate() {
                put(&mut props, &p.name_ru, &p.name_en, k);
            }
            ix.properties.push(props);
            let mut methods = HashMap::new();
            for (k, m) in t.methods.iter().enumerate() {
                put(&mut methods, &m.name_ru, &m.name_en, k);
            }
            ix.methods.push(methods);
            let mut values = HashMap::new();
            for (k, v) in t.values.iter().enumerate() {
                put(&mut values, &v.name_ru, &v.name_en, k);
            }
            ix.enum_values.push(values);
            let Ok(n) = u16::try_from(i) else { continue };
            let idx = TypeIdx(n);
            let ru = fold_name(&t.name_ru);
            let en = fold_name(&t.name_en);
            ix.types_by_name.entry(ru.clone()).or_default().push(idx);
            if en != ru {
                ix.types_by_name.entry(en).or_default().push(idx);
            }
            for alt in &t.alt_names {
                let alt = fold_name(alt);
                let known = ix.types_by_name.entry(alt).or_default();
                if !known.contains(&idx) {
                    known.push(idx);
                }
            }
            if let Some(g) = t.guid {
                ix.types_by_guid.entry(g).or_insert(idx);
            }
        }
        for (k, p) in data.global_properties.iter().enumerate() {
            put(&mut ix.global_props, &p.name_ru, &p.name_en, k);
        }
        for (k, m) in data.global_methods.iter().enumerate() {
            put(&mut ix.global_methods, &m.info.name_ru, &m.info.name_en, k);
            if m.builtin && m.server_known {
                put(&mut ix.builtins, &m.info.name_ru, &m.info.name_en, k);
            }
        }
        ix
    }
}

pub fn is_platform_build(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= crate::codec::MAX_PLATFORM
        && s.split('.')
            .all(|g| !g.is_empty() && g.bytes().all(|b| b.is_ascii_digit()))
}

pub struct Catalog {
    data: CatalogData,
    ix: Indexes,
    platform: String,
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
        self.ix
            .types_by_name
            .get(&fold_name(name))
            .map_or(&[], Vec::as_slice)
    }

    pub fn find_type(&self, name: &str) -> Option<TypeIdx> {
        self.find_type_folded(&fold_name(name))
    }

    pub fn find_type_folded(&self, folded: &str) -> Option<TypeIdx> {
        let all = self
            .ix
            .types_by_name
            .get(folded)
            .map_or(&[][..], Vec::as_slice);
        all.iter()
            .copied()
            .find(|t| self.type_info(*t).presentation_ru.is_some())
            .or_else(|| all.first().copied())
    }

    pub fn find_type_by_guid(&self, guid: &[u8; 16]) -> Option<TypeIdx> {
        self.ix.types_by_guid.get(guid).copied()
    }

    pub fn find_member(&self, t: TypeIdx, name: &str) -> Option<Member<'_>> {
        self.find_property(t, name)
            .map(Member::Property)
            .or_else(|| self.find_method(t, name).map(Member::Method))
    }

    pub fn find_property(&self, t: TypeIdx, name: &str) -> Option<&PropertyInfo> {
        let info = self.data.types.get(usize::from(t.0))?;
        let k = *self
            .ix
            .properties
            .get(usize::from(t.0))?
            .get(&fold_name(name))?;
        info.properties.get(k)
    }

    pub fn find_method(&self, t: TypeIdx, name: &str) -> Option<&MethodInfo> {
        let info = self.data.types.get(usize::from(t.0))?;
        let k = *self
            .ix
            .methods
            .get(usize::from(t.0))?
            .get(&fold_name(name))?;
        info.methods.get(k)
    }

    pub fn find_enum_value(&self, t: TypeIdx, name: &str) -> Option<usize> {
        self.ix
            .enum_values
            .get(usize::from(t.0))?
            .get(&fold_name(name))
            .copied()
    }

    pub fn global_properties(&self) -> &[PropertyInfo] {
        &self.data.global_properties
    }

    pub fn find_global_property(&self, name: &str) -> Option<&PropertyInfo> {
        let k = *self.ix.global_props.get(&fold_name(name))?;
        self.data.global_properties.get(k)
    }

    pub fn find_global_property_idx(&self, name: &str) -> Option<u16> {
        let k = *self.ix.global_props.get(&fold_name(name))?;
        u16::try_from(k).ok()
    }

    pub fn global_methods(&self) -> &[GlobalMethod] {
        &self.data.global_methods
    }

    pub fn find_global_method(&self, name: &str) -> Option<&GlobalMethod> {
        let k = *self.ix.global_methods.get(&fold_name(name))?;
        self.data.global_methods.get(k)
    }

    pub fn find_global_method_idx(&self, name: &str) -> Option<u16> {
        let k = *self.ix.global_methods.get(&fold_name(name))?;
        u16::try_from(k).ok()
    }

    pub fn builtin(&self, name: &str) -> Option<&GlobalMethod> {
        let k = *self.ix.builtins.get(&fold_name(name))?;
        self.data.global_methods.get(k)
    }

    pub fn query_tables(&self) -> &[QueryTableInfo] {
        &self.data.query_tables
    }

    pub fn query_table_groups(&self) -> &[QueryTableGroup] {
        &self.data.query_table_groups
    }
}
