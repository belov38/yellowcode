use super::{Other, QTableId, QType, QuerySchema, TableKind, ValuePath, VtKind};
use crate::managed::{self as mem, CompileArena};
use crate::sema::R;
use mdschema::MdKind;

pub(crate) trait NamePart {
    fn as_name(&self) -> &str;
}
impl NamePart for &str {
    fn as_name(&self) -> &str {
        self
    }
}
impl NamePart for query_syntax::ast::Ident {
    fn as_name(&self) -> &str {
        &self.text
    }
}

fn folded_eq(name: &str, key: &str) -> bool {
    name.chars()
        .map(|c| {
            let mut lower = c.to_lowercase();
            match (lower.next(), lower.next()) {
                (Some(c), None) => c,
                _ => c,
            }
        })
        .eq(key.chars())
}

impl QuerySchema {
    pub(crate) fn find_table_managed<P: NamePart>(
        &self,
        scope: Option<&CompileArena>,
        parts: &[P],
    ) -> R<Option<QTableId>> {
        let key = mem::NameKey::joined(scope, parts.iter().map(NamePart::as_name))?;
        Ok(self.by_name.get(key.as_str()).copied())
    }

    pub(crate) fn type_by_name_managed<P: NamePart>(
        &self,
        scope: Option<&CompileArena>,
        parts: &[P],
    ) -> R<Option<QType>> {
        if let [one] = parts {
            let key = mem::NameKey::folded(scope, one.as_name())?;
            if let Some(ty) = self.sys.by_folded_name(key.as_str()) {
                return Ok(Some(mem::copy(scope, ty)?));
            }
            if let Some(index) = self.sys_enums.find_folded(key.as_str()) {
                return Ok(Some(mem::other(scope, Other::SysEnum(index))?));
            }
        }
        let Some(id) = self.find_table_managed(scope, parts)? else {
            return Ok(None);
        };
        let table = self.table(id);
        match table.kind {
            TableKind::Object(_) => table
                .reference
                .map(|atom| mem::reference(scope, atom))
                .transpose(),
            TableKind::Virtual(VtKind::BusinessProcessPoints) => {
                let Some(atom) = table.owner.and_then(|owner| self.table(owner).reference) else {
                    return Ok(None);
                };
                Ok(Some(mem::other(scope, Other::RoutePoint(atom))?))
            }
            _ => Ok(None),
        }
    }

    pub(crate) fn value_path_owned<'a, P: NamePart>(
        &self,
        scope: Option<&'a CompileArena>,
        parts: &[P],
    ) -> R<mem::PathValue<'a>> {
        let kind = match parts {
            [ty, value] => {
                let ty_key = mem::NameKey::folded(scope, ty.as_name())?;
                let value_key = mem::NameKey::folded(scope, value.as_name())?;
                match self
                    .sys_enums
                    .value_folded(ty_key.as_str(), value_key.as_str())
                {
                    Some((ty, value)) => mem::PathKind::SysEnum(ty, value),
                    None => match self.find_table_managed(scope, parts)? {
                        Some(id)
                            if matches!(self.table(id).kind, TableKind::Object(_))
                                && self.table(id).reference.is_some() =>
                        {
                            mem::PathKind::TypeRef(id)
                        }
                        _ => mem::PathKind::Unknown,
                    },
                }
            }
            [_, _, value] => {
                let Some(id) = self.find_table_managed(scope, parts.get(..2).unwrap_or(&[]))?
                else {
                    return mem::PathValue::new(scope, mem::PathKind::Unknown);
                };
                let table = self.table(id);
                let (TableKind::Object(kind), Some(_)) = (table.kind, table.reference) else {
                    return mem::PathValue::new(scope, mem::PathKind::Unknown);
                };
                let key = mem::NameKey::folded(scope, value.as_name())?;
                if super::EMPTY_REF_NAMES
                    .iter()
                    .any(|name| folded_eq(name, key.as_str()))
                {
                    mem::PathKind::EmptyRef(id)
                } else if kind == MdKind::Enum {
                    match self
                        .values
                        .enums
                        .get(&id)
                        .and_then(|values| values.get(key.as_str()))
                    {
                        Some(value) => mem::PathKind::Enum {
                            table: id,
                            value: *value,
                        },
                        None => mem::PathKind::Unknown,
                    }
                } else if super::PREDEFINED_KINDS.contains(&kind) {
                    match self
                        .values
                        .predefined
                        .get(&id)
                        .and_then(|values| values.get(key.as_str()))
                    {
                        Some(name) => mem::PathKind::Predefined {
                            table: id,
                            name: mem::OwnedString::new(scope, name)?,
                        },
                        None => mem::PathKind::Unknown,
                    }
                } else {
                    mem::PathKind::Unknown
                }
            }
            [_, _, point, name] => {
                let key = mem::NameKey::folded(scope, point.as_name())?;
                let points = self
                    .find_table_managed(scope, parts.get(..2).unwrap_or(&[]))?
                    .and_then(|id| self.values.points.get(&id));
                match points {
                    Some(table)
                        if super::ROUTE_POINT_NAMES
                            .iter()
                            .any(|name| folded_eq(name, key.as_str())) =>
                    {
                        mem::PathKind::BusinessProcessPoint {
                            table: *table,
                            name: mem::OwnedString::new(scope, name.as_name())?,
                        }
                    }
                    _ => mem::PathKind::Unknown,
                }
            }
            _ => mem::PathKind::Unknown,
        };
        mem::PathValue::new(scope, kind)
    }

    pub(crate) fn value_type_managed(
        &self,
        scope: Option<&CompileArena>,
        value: &ValuePath,
    ) -> R<Option<QType>> {
        match value {
            ValuePath::Enum { table, .. }
            | ValuePath::Predefined { table, .. }
            | ValuePath::EmptyRef(table) => self
                .table(*table)
                .reference
                .map(|atom| mem::reference(scope, atom))
                .transpose(),
            ValuePath::SysEnum(index, _) => Ok(Some(mem::other(scope, Other::SysEnum(*index))?)),
            ValuePath::TypeRef(_) => Ok(Some(mem::other(scope, Other::Type)?)),
            ValuePath::BusinessProcessPoint { table, .. } => {
                let Some(atom) = self
                    .table(*table)
                    .owner
                    .and_then(|owner| self.table(owner).reference)
                else {
                    return Ok(None);
                };
                Ok(Some(mem::other(scope, Other::RoutePoint(atom))?))
            }
            ValuePath::Unknown => Ok(None),
        }
    }
}
