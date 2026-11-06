use crate::Application;
use crate::meta::{MdKey, MdPath, value_of};
use bsl_vm::{HostError, Registry};
use md_model::{Codec, Prop, Value};
use std::sync::Arc;

fn property<'a>(props: &'a [Prop], name: &str) -> Option<&'a Prop> {
    let mut matches = props.iter().filter(|p| p.name.eq_ignore_ascii_case(name));
    let property = matches.next()?;
    (property.name == name && matches.next().is_none()).then_some(property)
}

pub(crate) fn register(app: &Arc<Application>, reg: &mut Registry) {
    let app = app.clone();
    reg.global_fn(
        "ТекущийЯзык",
        "CurrentLanguage",
        Arc::new(move |ex, _| {
            let unavailable = || HostError::not_implemented("CurrentLanguage", "DefaultLanguage");
            let cfg = &app.model().config;
            let language = cfg.default_language().ok_or_else(unavailable)?;
            let name = property(&language.props, "Name");
            let code = property(&language.props, "LanguageCode");
            if language.name.is_empty()
                || !name.is_some_and(|p| {
                    p.codec == Codec::Str
                        && matches!(&p.value, Value::Text(n) if n == &language.name)
                })
                || !code.is_some_and(|p| {
                    p.codec == Codec::Str && matches!(&p.value, Value::Text(c) if !c.is_empty())
                })
            {
                return Err(unavailable().into());
            }
            let index = cfg
                .objects
                .iter()
                .position(|o| std::ptr::eq(o, language))
                .ok_or_else(unavailable)?;
            let cx = app.md_ctx().ok_or_else(unavailable)?;
            Ok(value_of(&cx, ex, MdKey::Path(MdPath::Other(index))))
        }),
    );
}
