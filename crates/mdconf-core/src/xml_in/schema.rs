use super::{
    ShellReport,
    shell_xml::{MD, N, block},
    walk::err,
};
use crate::{Codec, Diagnostics, Result};
use mdschema::{PropSpec, Schema};
use std::{collections::HashSet, path::Path};

#[allow(
    clippy::too_many_arguments,
    reason = "Schema extraction carries the source, owner and destination diagnostics explicitly"
)]
pub(super) fn properties(
    s: &Schema,
    n: N<'_, '_>,
    spec: &[(String, PropSpec)],
    tag: &str,
    scope: &str,
    path: &Path,
    report: &mut ShellReport,
    diag: &mut Diagnostics,
) -> Result<()> {
    let Some(props) = block(n, MD, "Properties", path)? else {
        return Err(err(path, "missing Properties"));
    };
    let mut seen = HashSet::new();
    for p in props.children() {
        if p.ns() != Some(MD) || !seen.insert(p.local()) {
            return Err(err(path, "wrong namespace or duplicate property"));
        }
        let key = format!("{scope}.{}", p.local());
        if tag == "Configuration" && s.config_prop(p.local()).is_some() {
            report.configuration_properties += 1;
            continue;
        }
        if let Some(codec) = resolve(s, spec, tag, scope, p.local()) {
            let entry = report.property_codecs.entry(key).or_insert((codec, 0));
            entry.1 += 1;
        } else if s.xdto_props(scope).iter().any(|x| x.name == p.local()) {
            *report.known_without_codec.entry(key).or_default() += 1;
        } else {
            *report.unknown_properties.entry(key.clone()).or_default() += 1;
            *diag.misses.entry(format!("unknown:{key}")).or_default() += 1;
        }
    }
    Ok(())
}
pub(super) fn resolve(
    s: &Schema,
    spec: &[(String, PropSpec)],
    tag: &str,
    scope: &str,
    name: &str,
) -> Option<Codec> {
    descriptor(s, spec, tag, scope, name).map(|p| p.codec)
}
pub(super) fn descriptor(
    s: &Schema,
    spec: &[(String, PropSpec)],
    tag: &str,
    scope: &str,
    name: &str,
) -> Option<PropSpec> {
    let header = match name {
        "Name" | "Comment" => Some(Codec::Str),
        "Synonym" => Some(Codec::LStr),
        _ => None,
    };
    if let Some(codec) = header {
        return Some(PropSpec {
            codec,
            ..Default::default()
        });
    }
    let find =
        |list: &[(String, PropSpec)]| list.iter().find(|(n, _)| n == name).map(|(_, p)| p.clone());
    if let Some(codec) = s.tag_props(tag).and_then(find).or_else(|| find(spec)) {
        return Some(codec);
    }
    if scope == "RecalculationProperties" && name == "DataLockControlMode" {
        return Some(crate::objects::recalc::lock_mode_spec(s));
    }
    if scope == "RecalculationDimensionProperties"
        && let Some((_, p)) = crate::objects::recalc::dimension_specs()
            .into_iter()
            .find(|(n, _)| *n == name)
    {
        return Some(p);
    }
    let known = s.xdto_props(scope).iter().any(|p| p.name == name);
    let field = known
        && s.xdto_props(scope)
            .iter()
            .any(|p| p.name == "Type" && p.type_name.as_deref() == Some("TypeDescription"));
    let fd = field.then(|| s.fd_template(name)).flatten();
    let hp = s.hp_template(tag, name);
    if let (Some(a), Some(b)) = (fd, hp)
        && a.codec != b.codec
    {
        return None;
    }
    fd.or(hp).cloned()
}
