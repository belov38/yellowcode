use crate::Application;
use bsl_stdlib::{PictureNames, picture_library};
use bsl_vm::{HostError, Registry};
use md_model::MdKind;
use std::sync::Arc;

const PROPERTY_RU: &str = "БиблиотекаКартинок";

pub(crate) fn register(app: &Arc<Application>, reg: &mut Registry) {
    let Some(property) = reg.catalog().find_global_property(PROPERTY_RU) else {
        return;
    };
    let (ru, en) = (property.name_ru.clone(), property.name_en.clone());
    let names = Arc::new(PictureNames::new(
        app.model()
            .config
            .of_kind(MdKind::CommonPicture)
            .map(|o| (o.name.clone(), *o.id.as_bytes())),
    ));
    if picture_library(&names).is_none() {
        return;
    }
    reg.global_prop(
        &ru,
        &en,
        Arc::new(move |_| {
            picture_library(&names).ok_or_else(|| {
                HostError::not_implemented("Глобальный контекст", PROPERTY_RU).into()
            })
        }),
    );
}
