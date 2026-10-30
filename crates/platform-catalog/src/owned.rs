use crate::{Catalog, CatalogData, DecodeError};
use memory_budget::{BudgetError, Category, MemoryBudget, Reservation};
use std::{error::Error, fmt, ops::Deref};

pub(crate) struct Admitted<T> {
    pub(crate) value: T,
    pub(crate) charge: Reservation,
}
pub struct AdmittedCatalog {
    pub(crate) value: Catalog,
    pub(crate) _charge: Reservation,
}
impl Deref for AdmittedCatalog {
    type Target = Catalog;
    fn deref(&self) -> &Catalog {
        &self.value
    }
}
#[derive(Debug)]
pub struct OwnedDecodeError {
    value: DecodeError,
    _charge: Reservation,
}
#[derive(Debug)]
pub enum ManagedDecodeError {
    Memory(BudgetError),
    Data(OwnedDecodeError),
}
impl ManagedDecodeError {
    pub(crate) fn data(value: DecodeError, charge: Reservation) -> Self {
        match value.0.as_str() {
            "allocation failed" => {
                drop(value);
                drop(charge);
                Self::Memory(BudgetError::AllocationFailed)
            }
            "allocation size overflow" => {
                drop(value);
                drop(charge);
                Self::Memory(BudgetError::Overflow)
            }
            _ => Self::Data(OwnedDecodeError {
                value,
                _charge: charge,
            }),
        }
    }
}
impl From<BudgetError> for ManagedDecodeError {
    fn from(error: BudgetError) -> Self {
        Self::Memory(error)
    }
}
impl From<memory_budget::MemoryLimit> for ManagedDecodeError {
    fn from(error: memory_budget::MemoryLimit) -> Self {
        Self::Memory(error.into())
    }
}
impl fmt::Display for ManagedDecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Memory(e) => e.fmt(f),
            Self::Data(e) => e.value.fmt(f),
        }
    }
}
impl Error for ManagedDecodeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(match self {
            Self::Memory(e) => e,
            Self::Data(e) => &e.value,
        })
    }
}
impl Catalog {
    pub fn copy_from_data_with_memory(
        data: &CatalogData,
        budget: &MemoryBudget,
    ) -> Result<AdmittedCatalog, ManagedDecodeError> {
        let bytes = crate::codec::copy_bytes(data)?
            .checked_add(crate::index::Indexes::bytes(data)?)
            .and_then(|n| n.checked_add(std::mem::size_of::<Catalog>() as u64))
            .ok_or(BudgetError::Overflow)?;
        let charge = budget.try_reserve(Category::Metadata, bytes)?;
        let value = copy_data(data)?;
        let ix = crate::index::Indexes::try_build(&value)?;
        Ok(AdmittedCatalog {
            value: Catalog {
                data: value,
                ix,
                platform: String::new(),
            },
            _charge: charge,
        })
    }
}

trait CopyData: Sized {
    fn copy_data(&self) -> Result<Self, BudgetError>;
}
fn copy_data<T: CopyData>(value: &T) -> Result<T, BudgetError> {
    value.copy_data()
}
impl CopyData for String {
    fn copy_data(&self) -> Result<Self, BudgetError> {
        let mut text = String::new();
        text.try_reserve_exact(self.len())
            .map_err(|_| BudgetError::AllocationFailed)?;
        text.push_str(self);
        Ok(text)
    }
}
impl<T: CopyData> CopyData for Vec<T> {
    fn copy_data(&self) -> Result<Self, BudgetError> {
        let mut values = Vec::new();
        values
            .try_reserve_exact(self.len())
            .map_err(|_| BudgetError::AllocationFailed)?;
        for value in self {
            values.push(copy_data(value)?);
        }
        Ok(values)
    }
}
impl<T: CopyData> CopyData for Option<T> {
    fn copy_data(&self) -> Result<Self, BudgetError> {
        self.as_ref().map(copy_data).transpose()
    }
}
macro_rules! copy_scalar {($($ty:ty),* $(,)?)=>{$(impl CopyData for $ty {
    fn copy_data(&self)->Result<Self,BudgetError> {Ok(*self)}
})*};}
copy_scalar!(
    bool,
    u8,
    i32,
    [u8; 16],
    crate::Avail,
    crate::TypeKind,
    crate::NewError,
    crate::ServerCompile
);
impl CopyData for crate::TypeInfo {
    fn copy_data(&self) -> Result<Self, BudgetError> {
        Ok(Self {
            id: copy_data(&self.id)?,
            name_ru: copy_data(&self.name_ru)?,
            name_en: copy_data(&self.name_en)?,
            kind: copy_data(&self.kind)?,
            avail: copy_data(&self.avail)?,
            metadata_template: copy_data(&self.metadata_template)?,
            collection: copy_data(&self.collection)?,
            serializable: copy_data(&self.serializable)?,
            server_exchange: copy_data(&self.server_exchange)?,
            xdto: copy_data(&self.xdto)?,
            guid: copy_data(&self.guid)?,
            since: copy_data(&self.since)?,
            compat_mode: copy_data(&self.compat_mode)?,
            presentation_ru: copy_data(&self.presentation_ru)?,
            presentation_en: copy_data(&self.presentation_en)?,
            new_no_args: copy_data(&self.new_no_args)?,
            type_fn: copy_data(&self.type_fn)?,
            type_fn_en: copy_data(&self.type_fn_en)?,
            new_error: copy_data(&self.new_error)?,
            value_type_presentation_ru: copy_data(&self.value_type_presentation_ru)?,
            value_type_presentation_en: copy_data(&self.value_type_presentation_en)?,
            enum_global: copy_data(&self.enum_global)?,
            server_compile: copy_data(&self.server_compile)?,
            alt_names: copy_data(&self.alt_names)?,
            properties: copy_data(&self.properties)?,
            methods: copy_data(&self.methods)?,
            constructors: copy_data(&self.constructors)?,
            values: copy_data(&self.values)?,
        })
    }
}
impl CopyData for crate::CollectionFlags {
    fn copy_data(&self) -> Result<Self, BudgetError> {
        Ok(Self {
            for_each: copy_data(&self.for_each)?,
            index_access: copy_data(&self.index_access)?,
        })
    }
}
impl CopyData for crate::XdtoName {
    fn copy_data(&self) -> Result<Self, BudgetError> {
        Ok(Self {
            namespace: copy_data(&self.namespace)?,
            name: copy_data(&self.name)?,
        })
    }
}
impl CopyData for crate::PropertyInfo {
    fn copy_data(&self) -> Result<Self, BudgetError> {
        Ok(Self {
            name_ru: copy_data(&self.name_ru)?,
            name_en: copy_data(&self.name_en)?,
            readable: copy_data(&self.readable)?,
            writable: copy_data(&self.writable)?,
            avail: copy_data(&self.avail)?,
            types: copy_data(&self.types)?,
        })
    }
}
impl CopyData for crate::ResultType {
    fn copy_data(&self) -> Result<Self, BudgetError> {
        Ok(Self {
            id: copy_data(&self.id)?,
            name_ru: copy_data(&self.name_ru)?,
            name_en: copy_data(&self.name_en)?,
        })
    }
}
impl CopyData for crate::MethodInfo {
    fn copy_data(&self) -> Result<Self, BudgetError> {
        Ok(Self {
            name_ru: copy_data(&self.name_ru)?,
            name_en: copy_data(&self.name_en)?,
            variants: copy_data(&self.variants)?,
            returns_value: copy_data(&self.returns_value)?,
            avail: copy_data(&self.avail)?,
        })
    }
}
impl CopyData for crate::Signature {
    fn copy_data(&self) -> Result<Self, BudgetError> {
        Ok(Self {
            params: copy_data(&self.params)?,
            return_types: copy_data(&self.return_types)?,
        })
    }
}
impl CopyData for crate::ParamInfo {
    fn copy_data(&self) -> Result<Self, BudgetError> {
        Ok(Self {
            name_ru: copy_data(&self.name_ru)?,
            name_en: copy_data(&self.name_en)?,
            optional: copy_data(&self.optional)?,
            variadic: copy_data(&self.variadic)?,
        })
    }
}
impl CopyData for crate::EnumValueInfo {
    fn copy_data(&self) -> Result<Self, BudgetError> {
        Ok(Self {
            name_ru: copy_data(&self.name_ru)?,
            name_en: copy_data(&self.name_en)?,
            ordinal: copy_data(&self.ordinal)?,
            presentation_ru: copy_data(&self.presentation_ru)?,
            presentation_en: copy_data(&self.presentation_en)?,
        })
    }
}
impl CopyData for crate::GlobalMethod {
    fn copy_data(&self) -> Result<Self, BudgetError> {
        Ok(Self {
            info: copy_data(&self.info)?,
            server_known: copy_data(&self.server_known)?,
            builtin: copy_data(&self.builtin)?,
            is_function: copy_data(&self.is_function)?,
            min_args: copy_data(&self.min_args)?,
            max_args: copy_data(&self.max_args)?,
            arity_compile: copy_data(&self.arity_compile)?,
        })
    }
}
impl CopyData for crate::QueryTableInfo {
    fn copy_data(&self) -> Result<Self, BudgetError> {
        Ok(Self {
            id: copy_data(&self.id)?,
            group: copy_data(&self.group)?,
            name_ru: copy_data(&self.name_ru)?,
            name_en: copy_data(&self.name_en)?,
            fields: copy_data(&self.fields)?,
            params: copy_data(&self.params)?,
        })
    }
}
impl CopyData for crate::QueryFieldInfo {
    fn copy_data(&self) -> Result<Self, BudgetError> {
        Ok(Self {
            name_ru: copy_data(&self.name_ru)?,
            name_en: copy_data(&self.name_en)?,
            types: copy_data(&self.types)?,
        })
    }
}
impl CopyData for crate::FieldType {
    fn copy_data(&self) -> Result<Self, BudgetError> {
        Ok(Self {
            id: copy_data(&self.id)?,
            name_ru: copy_data(&self.name_ru)?,
            name_en: copy_data(&self.name_en)?,
        })
    }
}
impl CopyData for crate::QueryParamInfo {
    fn copy_data(&self) -> Result<Self, BudgetError> {
        Ok(Self {
            name_ru: copy_data(&self.name_ru)?,
            name_en: copy_data(&self.name_en)?,
            optional: copy_data(&self.optional)?,
        })
    }
}
impl CopyData for crate::QueryTableGroup {
    fn copy_data(&self) -> Result<Self, BudgetError> {
        Ok(Self {
            id: copy_data(&self.id)?,
            name_ru: copy_data(&self.name_ru)?,
            name_en: copy_data(&self.name_en)?,
        })
    }
}
impl CopyData for crate::CatalogData {
    fn copy_data(&self) -> Result<Self, BudgetError> {
        Ok(Self {
            types: copy_data(&self.types)?,
            global_properties: copy_data(&self.global_properties)?,
            global_methods: copy_data(&self.global_methods)?,
            query_tables: copy_data(&self.query_tables)?,
            query_table_groups: copy_data(&self.query_table_groups)?,
        })
    }
}

#[derive(Debug)]
pub struct OwnedPlatformError {
    value: crate::PlatformError,
    _charge: Reservation,
}
impl OwnedPlatformError {
    pub(crate) fn platform(&self) -> &crate::PlatformError {
        &self.value
    }
}
#[derive(Debug)]
pub enum ManagedSelectError {
    Memory(BudgetError),
    Decode(ManagedDecodeError),
    Platform(OwnedPlatformError),
    Io(std::io::Error),
    Unreadable(memory_budget::BudgetedBox<(OwnedPlatformError, std::io::Error)>),
}
impl From<BudgetError> for ManagedSelectError {
    fn from(e: BudgetError) -> Self {
        Self::Memory(e)
    }
}
impl From<memory_budget::MemoryLimit> for ManagedSelectError {
    fn from(e: memory_budget::MemoryLimit) -> Self {
        Self::Memory(e.into())
    }
}
impl From<ManagedDecodeError> for ManagedSelectError {
    fn from(e: ManagedDecodeError) -> Self {
        Self::Decode(e)
    }
}
impl From<crate::native::Error> for ManagedSelectError {
    fn from(e: crate::native::Error) -> Self {
        match e {
            crate::native::Error::Memory(e) => Self::Memory(e),
            crate::native::Error::Io(e) => Self::Io(e),
        }
    }
}
impl From<std::io::Error> for ManagedSelectError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}
impl fmt::Display for ManagedSelectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Memory(e) => fmt::Display::fmt(e, f),
            Self::Decode(e) => fmt::Display::fmt(e, f),
            Self::Platform(e) => fmt::Display::fmt(&e.value, f),
            Self::Io(e) => fmt::Display::fmt(e, f),
            Self::Unreadable(pair) => write!(f, "{}: {}", pair.0.value, pair.1),
        }
    }
}
impl Error for ManagedSelectError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(match self {
            Self::Memory(e) => e,
            Self::Decode(e) => e,
            Self::Platform(e) => &e.value,
            Self::Io(e) => e,
            Self::Unreadable(pair) => &pair.1,
        })
    }
}
impl ManagedSelectError {
    pub(crate) fn unknown(build: &str, budget: &MemoryBudget) -> Self {
        match budget.try_reserve(Category::Metadata, build.len() as u64) {
            Ok(charge) => Self::Platform(OwnedPlatformError {
                value: crate::PlatformError::Unknown(crate::PlatformVersion::new(build)),
                _charge: charge,
            }),
            Err(e) => Self::Memory(e.into()),
        }
    }
    pub(crate) fn other(active: &str, build: &str, budget: &MemoryBudget) -> Self {
        let Some(bytes) = active.len().checked_add(build.len()) else {
            return Self::Memory(BudgetError::Overflow);
        };
        match budget.try_reserve(Category::Metadata, bytes as u64) {
            Ok(charge) => Self::Platform(OwnedPlatformError {
                value: crate::PlatformError::OtherSelected {
                    active: crate::PlatformVersion::new(active),
                    requested: crate::PlatformVersion::new(build),
                },
                _charge: charge,
            }),
            Err(e) => Self::Memory(e.into()),
        }
    }
    pub(crate) fn broken(
        build: &str,
        path: &std::path::Path,
        reason: &str,
        budget: &MemoryBudget,
    ) -> Self {
        let Some(bytes) = build
            .len()
            .checked_add(path.as_os_str().len())
            .and_then(|n| n.checked_add(reason.len()))
        else {
            return Self::Memory(BudgetError::Overflow);
        };
        match budget.try_reserve(Category::Metadata, bytes as u64) {
            Ok(charge) => Self::Platform(OwnedPlatformError {
                value: crate::PlatformError::Broken {
                    build: crate::PlatformVersion::new(build),
                    path: path.to_owned(),
                    reason: reason.to_owned(),
                },
                _charge: charge,
            }),
            Err(e) => Self::Memory(e.into()),
        }
    }
}
