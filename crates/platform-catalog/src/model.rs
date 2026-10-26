#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct TypeIdx(pub u16);

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Context {
    ThinClient,
    WebClient,
    MobileClient,
    Server,
    ThickClient,
    ExternalConnection,
    MobileAppClient,
    MobileAppServer,
    MobileStandaloneServer,
    Integration,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct Avail(pub u16);

impl Avail {
    pub const ALL: Avail = Avail((1 << 10) - 1);

    pub fn has(self, c: Context) -> bool {
        self.0 & (1 << (c as u16)) != 0
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TypeKind {
    Primitive,
    Class,
    Enum,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypeInfo {
    pub id: String,
    pub name_ru: String,
    pub name_en: String,
    pub kind: TypeKind,
    pub avail: Avail,
    pub metadata_template: bool,
    pub collection: Option<CollectionFlags>,
    pub serializable: bool,
    pub server_exchange: bool,
    pub xdto: Option<XdtoName>,
    pub guid: Option<[u8; 16]>,
    pub since: Option<String>,
    pub compat_mode: Option<String>,
    pub presentation_ru: Option<String>,
    pub presentation_en: Option<String>,
    pub new_no_args: Option<bool>,
    pub type_fn: Option<bool>,
    pub type_fn_en: Option<bool>,
    pub new_error: Option<NewError>,
    pub value_type_presentation_ru: Option<String>,
    pub value_type_presentation_en: Option<String>,
    pub enum_global: bool,
    pub server_compile: Option<ServerCompile>,
    pub alt_names: Vec<String>,
    pub properties: Vec<PropertyInfo>,
    pub methods: Vec<MethodInfo>,
    pub constructors: Vec<Signature>,
    pub values: Vec<EnumValueInfo>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NewError {
    CtorNotFound,
    NotObject,
    CtorFails,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServerCompile {
    pub type_known: bool,
    pub new_no_args_compiles: bool,
    pub global_value: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CollectionFlags {
    pub for_each: bool,
    pub index_access: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct XdtoName {
    pub namespace: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResultType {
    pub id: Option<String>,
    pub name_ru: String,
    pub name_en: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PropertyInfo {
    pub name_ru: String,
    pub name_en: String,
    pub readable: bool,
    pub writable: bool,
    pub avail: Avail,
    pub types: Vec<ResultType>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MethodInfo {
    pub name_ru: String,
    pub name_en: String,
    pub variants: Vec<Signature>,
    pub returns_value: bool,
    pub avail: Avail,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Signature {
    pub params: Vec<ParamInfo>,
    pub return_types: Option<Vec<ResultType>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParamInfo {
    pub name_ru: String,
    pub name_en: String,
    pub optional: bool,
    pub variadic: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnumValueInfo {
    pub name_ru: String,
    pub name_en: String,
    pub ordinal: Option<i32>,
    pub presentation_ru: Option<String>,
    pub presentation_en: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GlobalMethod {
    pub info: MethodInfo,
    pub server_known: bool,
    pub builtin: bool,
    pub is_function: bool,
    pub min_args: u8,
    pub max_args: Option<u8>,
    pub arity_compile: bool,
}

#[derive(Clone, Copy, Debug)]
pub enum Member<'a> {
    Property(&'a PropertyInfo),
    Method(&'a MethodInfo),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueryTableInfo {
    pub id: String,
    pub group: Option<String>,
    pub name_ru: String,
    pub name_en: String,
    pub fields: Vec<QueryFieldInfo>,
    pub params: Vec<QueryParamInfo>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueryFieldInfo {
    pub name_ru: String,
    pub name_en: String,
    pub types: Vec<FieldType>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldType {
    pub id: Option<String>,
    pub name_ru: String,
    pub name_en: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueryParamInfo {
    pub name_ru: String,
    pub name_en: String,
    pub optional: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueryTableGroup {
    pub id: String,
    pub name_ru: String,
    pub name_en: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct CatalogData {
    pub types: Vec<TypeInfo>,
    pub global_properties: Vec<PropertyInfo>,
    pub global_methods: Vec<GlobalMethod>,
    pub query_tables: Vec<QueryTableInfo>,
    pub query_table_groups: Vec<QueryTableGroup>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodeError(pub String);

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for DecodeError {}

impl TypeInfo {
    pub fn accepts_type_name(&self, name: &str) -> Option<bool> {
        let folded = bsl_prim::fold_name(name);
        if folded == bsl_prim::fold_name(&self.name_en)
            && folded != bsl_prim::fold_name(&self.name_ru)
        {
            self.type_fn_en.or(self.type_fn)
        } else {
            self.type_fn
        }
    }
}
