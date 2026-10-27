use mdconf::{Configuration, DateFractions};

use crate::{DateParts, FormDesc, ModelError, Resolved, TypeDesc, TypeResolver, parse_form_xml};

pub type BuiltForm = (FormDesc, Vec<(String, Vec<u8>)>);

pub fn from_configuration(
    cfg: &Configuration,
    id: brace::Guid,
) -> Result<Option<BuiltForm>, ModelError> {
    let Some((text, pictures, _deferred)) = mdconf::forms::form_xml(cfg, id) else {
        return Ok(None);
    };
    Ok(Some((
        parse_form_xml(text.trim_start_matches('\u{feff}'))?,
        pictures,
    )))
}

pub fn type_desc(t: &mdconf::TypeDescription) -> TypeDesc {
    TypeDesc {
        types: t.types.iter().map(|r| r.name.to_string()).collect(),
        string_len: t.string.map(|s| s.length),
        number: t.number.map(|n| (n.digits, n.fraction)),
        date: t.date.map(|d| match d {
            DateFractions::Date => DateParts::Date,
            DateFractions::Time => DateParts::Time,
            DateFractions::DateTime => DateParts::DateTime,
        }),
    }
}

pub struct MdResolver<'a> {
    pub cfg: &'a Configuration,
}

impl TypeResolver for MdResolver<'_> {
    fn resolve(&self, owner: &str, path: &[&str]) -> Option<Resolved> {
        let request = crate::ResolverOwner::parse(owner, path)?;
        let object = self.cfg.find(request.kind, request.name)?;
        request
            .resolve_selected(object, path)
            .map(crate::ResolvedView::into_owned)
    }
}
