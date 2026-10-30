use super::codec::{InternalReader, InternalWriter, ValueCodec};
use crate::enums::{EnumValue, enum_value, managed_here};
use brace::Node;
use bsl_vm::{Exception, Execution, ObjRef, Value};
use platform_catalog::{TypeIdx, catalog};
use std::sync::Arc;

struct SystemEnum(TypeIdx);

pub(crate) fn codecs() -> Vec<(TypeIdx, Arc<dyn ValueCodec>)> {
    catalog()
        .types()
        .iter()
        .enumerate()
        .filter(|(_, info)| {
            managed_here(info)
                && info.guid.is_some()
                && info.values.iter().any(|value| value.ordinal.is_some())
        })
        .filter_map(|(index, _)| {
            let index = TypeIdx(u16::try_from(index).ok()?);
            Some((index, Arc::new(SystemEnum(index)) as Arc<dyn ValueCodec>))
        })
        .collect()
}

impl ValueCodec for SystemEnum {
    fn internal_write(
        &self,
        ex: &mut Execution,
        value: &ObjRef,
        writer: &mut InternalWriter,
    ) -> Result<Option<Vec<Node>>, Exception> {
        let Some(value) = value.as_any().downcast_ref::<EnumValue>() else {
            return Ok(None);
        };
        if value.type_idx() != self.0 {
            return Ok(None);
        }
        let Some(ordinal) = catalog()
            .type_info(self.0)
            .values
            .get(value.index())
            .and_then(|value| value.ordinal)
        else {
            return Ok(None);
        };
        writer.account(ex, 12)?;
        Ok(Some(vec![Node::int(i64::from(ordinal))]))
    }

    fn internal_read(
        &self,
        ex: &mut Execution,
        data: &[Node],
        reader: &mut InternalReader,
    ) -> Result<Value, Exception> {
        let [number] = data else {
            return Err(reader.format_error(ex));
        };
        let Some(number) = number.as_atom().and_then(|text| text.parse::<i32>().ok()) else {
            return Err(reader.format_error(ex));
        };
        for value in &catalog().type_info(self.0).values {
            ex.charge(1)?;
            if value.ordinal == Some(number) {
                ex.reserve(crate::collections::retained_rc_size::<EnumValue>())?;
                return enum_value(self.0, &value.name_ru).ok_or_else(|| reader.format_error(ex));
            }
        }
        Err(reader.format_error(ex))
    }
}
