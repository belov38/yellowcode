use crate::Application;
use crate::model::ObjKind;
use bsl_vm::Execution;
use ibschema::values::RefId;
use query_ir::host::PresentationInput;
use query_ir::{QStr, QValue, StdRole};

pub(super) fn text(app: &Application, ex: &mut Execution, p: &PresentationInput) -> Option<QStr> {
    let QValue::Ref { table, id } = &p.reference else {
        return None;
    };
    let obj = app.model().obj_of_ref_number(*table)?;
    let def = app.model().obj(obj);
    match def.kind {
        ObjKind::Enum => {
            let value = def
                .enum_values
                .iter()
                .find(|v| RefId::from_uuid(v.md) == RefId(*id))?;
            let lang = &app.model().lang_code;
            Some(QStr::from_str(&crate::types::synonym(
                &value.synonym,
                &value.name,
                lang,
            )))
        }
        ObjKind::Catalog => {
            if crate::present::has_handler(ex, app, obj) {
                return None;
            }
            let field = |role| p.fields.iter().find(|(r, _)| *r == role).map(|(_, v)| v);
            let by_code = field(StdRole::Description).is_none()
                || def.behavior.present_by_code && field(StdRole::Code).is_some();
            let role = if by_code {
                StdRole::Code
            } else {
                StdRole::Description
            };
            match field(role)? {
                QValue::String(s) if !by_code && s.is_empty() => Some(QStr::from_str("<>")),
                QValue::String(s) => Some(s.clone()),
                _ => None,
            }
        }
        _ => None,
    }
}
