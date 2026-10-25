use crate::bind::session_locale;
use bsl_intl::format::{FmtValue, FormatEnv, format};
use bsl_intl::present::{bool_to_string, date_to_string, number_to_string};
use bsl_prim::{BslDate, Number, Str};
use bsl_vm::{Exception, Execution, HostObject, Value};

const BYTES_PER_LIMB: u64 = 18;

const MAX_NUMBER_BYTES: u64 = 411 * 2;

thread_local! { static COLLECTION_PRESENT_LIVE: std::cell::Cell<u64> = const { std::cell::Cell::new(0) }; }
pub(crate) fn collection_presentation_live() -> u64 {
    COLLECTION_PRESENT_LIVE.with(std::cell::Cell::get)
}
pub(crate) struct CollectionPresentationGuard(u64);
impl CollectionPresentationGuard {
    pub(crate) fn enter(live: u64) -> Self {
        COLLECTION_PRESENT_LIVE.with(|count| {
            let old = count.get();
            count.set(old.saturating_add(live));
            Self(old)
        })
    }
}
impl Drop for CollectionPresentationGuard {
    fn drop(&mut self) {
        COLLECTION_PRESENT_LIVE.with(|count| count.set(self.0));
    }
}
pub(crate) fn present_for_collection(
    ex: &mut Execution,
    value: &Value,
    live: u64,
) -> Result<Str, Exception> {
    preflight_collection_presentation(ex, value, live)?;
    let _guard = CollectionPresentationGuard::enter(live);
    ex.present(value)
}

pub(crate) fn preflight_collection_presentation(
    ex: &mut Execution,
    value: &Value,
    live: u64,
) -> Result<u64, Exception> {
    let bytes = match value {
        Value::Number(n) => {
            ex.charge(n.size_hint_limbs() / 8 + 1)?;
            n.size_hint_limbs().saturating_mul(64).saturating_add(2048)
        }
        Value::String(_) | Value::Undefined | Value::Null => 0,
        Value::Type(t) => {
            let len = ex.runtime().types().presentation(*t, ex.lang()).len() as u64;
            ex.charge(len / 32 + 1)?;
            len.saturating_mul(4).saturating_add(64)
        }
        Value::Object(object) => {
            if let Some(td) = object
                .as_any()
                .downcast_ref::<crate::typedesc::TypeDescription>()
            {
                ex.charge(td.types().len() as u64)?;
                let mut bytes = (td.types().len() as u64)
                    .saturating_mul(64)
                    .saturating_add(2048);
                for ty in td.types() {
                    let len = ex.runtime().types().presentation(*ty, ex.lang()).len() as u64;
                    ex.charge(len / 32 + 1)?;
                    bytes = bytes.saturating_add(len.saturating_add(2).saturating_mul(16));
                }
                bytes
            } else if let Some(len) = crate::ui::presentation_input_len(value) {
                ex.charge(len / 32 + 1)?;
                len.saturating_mul(16).saturating_add(2048)
            } else if let Some(data) = crate::values::binary_data_bytes(value) {
                let len = data.len() as u64;
                ex.charge(len / 8 + 1)?;
                len.saturating_mul(16).saturating_add(256)
            } else {
                1024
            }
        }
        _ => 1024,
    };
    ex.reserve(
        collection_presentation_live()
            .saturating_add(live)
            .saturating_add(bytes),
    )?;
    Ok(bytes)
}

pub fn present(ex: &mut Execution, v: &Value) -> Result<Str, Exception> {
    Ok(match v {
        Value::Undefined | Value::Null => Str::new(),
        Value::String(s) => s.clone(),
        Value::Bool(b) => Str::from(bool_text(ex, *b)),
        Value::Number(n) => {
            ex.reserve(
                n.size_hint_limbs()
                    .saturating_mul(BYTES_PER_LIMB)
                    .min(MAX_NUMBER_BYTES),
            )?;
            Str::from(number_text(ex, n))
        }
        Value::Date(d) => Str::from(date_text(ex, *d)),
        Value::Type(t) => Str::from(ex.runtime().types().presentation(*t, ex.lang())),
        Value::Object(o) => {
            if let Some(b) = crate::values::binary_data_bytes(v) {
                ex.reserve((b.len() as u64).saturating_mul(6))?;
            }
            match o.presentation(ex).map_err(|e| ex.call_error(e))? {
                Some(s) => s,
                None => Str::from(
                    ex.runtime()
                        .types()
                        .presentation(HostObject::type_id(&**o), ex.script_variant()),
                ),
            }
        }
    })
}

fn number_text(ex: &Execution, n: &Number) -> String {
    match session_locale(ex) {
        Some(loc) => number_to_string(n, loc),
        None => n.to_xml_string(),
    }
}

fn date_text(ex: &Execution, d: BslDate) -> String {
    match session_locale(ex) {
        Some(loc) => date_to_string(d, loc),
        None => d.to_xml_string(),
    }
}

fn bool_text(ex: &Execution, b: bool) -> String {
    let lang = ex.env().config_language();
    let plain = |c: char| c.is_ascii_alphanumeric() || c == '_';
    if lang.is_empty() || lang.eq_ignore_ascii_case("ru") || !lang.chars().all(plain) {
        return bool_to_string(b).to_string();
    }
    let Some(session) = session_locale(ex) else {
        return bool_to_string(b).to_string();
    };
    let spec: Vec<u16> = format!("Л={lang}").encode_utf16().collect();
    format(FmtValue::Bool(b), &spec, &FormatEnv { session })
}
