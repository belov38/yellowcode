use super::{
    bags,
    codecs::Ctx,
    encode_cparams::{CParamsEncodeError, typed},
    encode_predefined_dtref::PredefinedContext,
    encode_typed::{TypedEncodeError, encode_typed_with_predefined},
    name_lookup::NameLookup,
};
use crate::{
    index::Index,
    model::{Value, XmlElem},
};
use brace::{Guid, Node};
use mdschema::Schema;
const MAX_RECORDS: usize = 1000;
const MAX_FILTER_VISITS: usize = 100_000;
const TYPE_GUID: &str = "fe839d42-d094-40ba-b903-75bccc21ba30";
const POS: [usize; 9] = [3, 4, 5, 6, 8, 9, 10, 11, 12];
const TYPES: [&str; 5] = [
    "xr:KeyField",
    "xr:TypesFilterField",
    "xr:TypesFilterValue",
    "xr:DataPathField",
    "xr:MultipleValuesUseField",
];
const VALUES: [&str; 5] = [
    "xr:ObjectField",
    "xr:TypeField",
    "xr:ValueField",
    "xr:MultipleValuesKeyField",
    "xr:MultipleValuesOrderField",
];
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CharacteristicsEncodeError {
    WrongValue,
    InvalidShape,
    InvalidPrefix,
    UnresolvedSource,
    AmbiguousSource,
    UnresolvedField,
    AmbiguousField,
    UnsupportedValue,
    ResourceLimit,
}
use CharacteristicsEncodeError::*;
type Result<T> = std::result::Result<T, CharacteristicsEncodeError>;
pub(crate) struct CharacteristicsFieldPrefix<'a> {
    pub prefix: &'a Node,
    pub tail: Option<&'a Node>,
    pub segment: Option<&'a Node>,
}
pub(crate) struct CharacteristicsRecordPrefixes<'a> {
    pub wrapper: &'a Node,
    pub body: &'a Node,
    pub types_source: &'a Node,
    pub values_source: &'a Node,
    pub fields: [CharacteristicsFieldPrefix<'a>; 9],
}
pub(crate) struct CharacteristicsPrefixes<'a> {
    pub outer: &'a Node,
    pub records: Vec<CharacteristicsRecordPrefixes<'a>>,
}
fn scalar(n: &Node) -> Result<()> {
    if matches!(n, Node::List(_)) {
        Err(InvalidPrefix)
    } else {
        Ok(())
    }
}
fn list(n: &Node) -> Result<&[Node]> {
    n.as_list().ok_or(InvalidShape)
}
fn at(n: &[Node], k: usize) -> Result<&Node> {
    n.get(k).ok_or(InvalidShape)
}
fn vector<T>(n: usize) -> Result<Vec<T>> {
    let mut v = Vec::new();
    v.try_reserve_exact(n).map_err(|_| ResourceLimit)?;
    Ok(v)
}
fn nodes<const N: usize>(a: [Node; N]) -> Result<Node> {
    let mut v = vector(N)?;
    v.extend(a);
    Ok(Node::list(v))
}
impl<'a> CharacteristicsPrefixes<'a> {
    pub(crate) fn from_raw(raw: &'a Node, model_records: usize) -> Result<Self> {
        if model_records > MAX_RECORDS {
            return Err(ResourceLimit);
        }
        let root = list(raw)?;
        let outer = at(root, 0)?;
        scalar(outer)?;
        let mut records = vector(model_records)?;
        if model_records > 0 {
            let raw_records = list(at(root, 1)?)?;
            for k in 0..model_records {
                let r = list(at(raw_records, k.checked_add(1).ok_or(ResourceLimit)?)?)?;
                let b = list(at(r, 2)?)?;
                let field = |k| -> Result<CharacteristicsFieldPrefix<'a>> {
                    let f = list(at(b, k)?)?;
                    Ok(CharacteristicsFieldPrefix {
                        prefix: at(f, 0)?,
                        tail: f.get(2),
                        segment: f.get(1),
                    })
                };
                let [a, c, d, e, f, g, h, i, j] = POS;
                records.push(CharacteristicsRecordPrefixes {
                    wrapper: at(r, 0)?,
                    body: at(b, 0)?,
                    types_source: at(list(at(b, 1)?)?, 0)?,
                    values_source: at(list(at(b, 2)?)?, 0)?,
                    fields: [
                        field(a)?,
                        field(c)?,
                        field(d)?,
                        field(e)?,
                        field(f)?,
                        field(g)?,
                        field(h)?,
                        field(i)?,
                        field(j)?,
                    ],
                });
                at(list(at(b, 1)?)?, 1)?;
                at(list(at(b, 2)?)?, 1)?;
            }
        }
        let out = Self { outer, records };
        out.validate(model_records)?;
        Ok(out)
    }
    fn validate(&self, n: usize) -> Result<()> {
        if n > MAX_RECORDS {
            return Err(ResourceLimit);
        }
        if self.records.len() != n {
            return Err(InvalidShape);
        }
        scalar(self.outer)?;
        for p in &self.records {
            for x in [p.wrapper, p.body, p.types_source, p.values_source] {
                scalar(x)?
            }
            for f in &p.fields {
                scalar(f.prefix)?;
                if let Some(t) = f.tail {
                    scalar(t)?
                }
            }
        }
        Ok(())
    }
}
fn plain(e: &XmlElem, name: &str) -> Result<()> {
    if e.name != name || e.expanded || e.text.is_some() || !e.attrs.is_empty() {
        return Err(InvalidShape);
    }
    Ok(())
}
fn container<'a>(e: &'a XmlElem, name: &str, children: &[&str; 5]) -> Result<&'a str> {
    let [(key, from)] = e.attrs.as_slice() else {
        return Err(InvalidShape);
    };
    if e.name != name || e.expanded || e.text.is_some() || key != "from" || e.children.len() != 5 {
        return Err(InvalidShape);
    }
    for (child, name) in e.children.iter().zip(children) {
        if child.name != *name {
            return Err(InvalidShape);
        }
    }
    Ok(from)
}
fn leaf(e: &XmlElem) -> Result<&str> {
    if e.expanded || !e.attrs.is_empty() || !e.children.is_empty() {
        return Err(InvalidShape);
    }
    e.text.as_deref().ok_or(InvalidShape)
}
fn source(names: &NameLookup<'_>, name: &str) -> Result<Guid> {
    let found = match names.ids(name) {
        [] => Err(UnresolvedSource),
        [id] => Ok(*id),
        _ => Err(AmbiguousSource),
    };
    #[cfg(test)]
    if super::name_lookup::oracle::checking() {
        assert_eq!(found, tests::source_by_enumeration(names.index, name));
        super::name_lookup::oracle::checked(3);
    }
    found
}
fn candidates(
    ctx: &Ctx<'_>,
    names: &NameLookup<'_>,
    owner: &str,
    wanted: &str,
    visit: &mut dyn FnMut(Node) -> Result<()>,
) -> Result<()> {
    visit(nodes([Node::int(-1)])?)?;
    visit(nodes([Node::int(0)])?)?;
    for g in names.ids(wanted) {
        visit(nodes([Node::int(0), Node::guid(*g)])?)?
    }
    let parts = owner.split('.');
    let root_kind = parts.clone().next().ok_or(UnresolvedField)?;
    let count = parts.clone().count();
    let kind = if count >= 4 {
        parts.clone().nth(count - 2).ok_or(UnresolvedField)?
    } else {
        root_kind
    };
    for k in [
        Some(kind),
        if kind == "TabularSection" {
            Some(root_kind)
        } else {
            None
        },
    ]
    .into_iter()
    .flatten()
    {
        for (key, _) in ctx.schema.standard_attributes(k) {
            let Ok(n) = key.parse::<i64>() else { continue };
            if n.to_string() == key {
                visit(nodes([Node::int(n)])?)?
            }
        }
    }
    Ok(())
}
fn renders(ctx: &Ctx<'_>, owner: &str, candidate: &Node, wanted: &str) -> Result<bool> {
    let wrapper = nodes([Node::int(0), candidate.clone()])?;
    Ok(bags::char_field(ctx, &wrapper, owner).as_deref() == Some(wanted))
}
fn field(
    ctx: &Ctx<'_>,
    names: &NameLookup<'_>,
    owner: &str,
    wanted: &str,
    source: Option<&Node>,
) -> Result<Node> {
    let found = resolve(ctx, names, owner, wanted, source);
    #[cfg(test)]
    if super::name_lookup::oracle::checking() {
        assert_eq!(
            found,
            tests::field_by_enumeration(ctx, owner, wanted, source)
        );
        super::name_lookup::oracle::checked(2);
    }
    found
}
fn resolve(
    ctx: &Ctx<'_>,
    names: &NameLookup<'_>,
    owner: &str,
    wanted: &str,
    source: Option<&Node>,
) -> Result<Node> {
    if let Some(source) = source {
        let mut canonical = false;
        candidates(ctx, names, owner, wanted, &mut |candidate| {
            canonical |= &candidate == source;
            Ok(())
        })?;
        if canonical && renders(ctx, owner, source, wanted)? {
            return Ok(source.clone());
        }
    }
    let mut found: Option<Node> = None;
    candidates(ctx, names, owner, wanted, &mut |candidate| {
        if renders(ctx, owner, &candidate, wanted)? {
            if found.as_ref().is_some_and(|old| old != &candidate) {
                return Err(AmbiguousField);
            }
            found = Some(candidate)
        }
        Ok(())
    })?;
    found.ok_or(UnresolvedField)
}
pub(crate) fn encode_characteristics(
    schema: &'static Schema,
    index: &Index,
    tag: &str,
    value: &Value,
    prefixes: &CharacteristicsPrefixes<'_>,
) -> Result<Node> {
    encode_characteristics_with_predefined(
        schema,
        index,
        &PredefinedContext::default(),
        tag,
        value,
        prefixes,
    )
}
pub(crate) fn encode_characteristics_with_predefined(
    schema: &'static Schema,
    index: &Index,
    predefined: &PredefinedContext<'_>,
    tag: &str,
    value: &Value,
    prefixes: &CharacteristicsPrefixes<'_>,
) -> Result<Node> {
    let Value::Xml(root) = value else {
        return Err(WrongValue);
    };
    if root.children.len() > MAX_RECORDS {
        return Err(ResourceLimit);
    }
    plain(root, tag)?;
    prefixes.validate(root.children.len())?;
    let predef = Default::default();
    let names = std::cell::OnceCell::new();
    let ctx = Ctx {
        schema,
        index,
        predef: &predef,
    };
    let mut remaining = MAX_FILTER_VISITS;
    let mut filters = vector(root.children.len())?;
    for record in &root.children {
        plain(record, "xr:Characteristic")?;
        let [t, _] = record.children.as_slice() else {
            return Err(InvalidShape);
        };
        container(t, "xr:CharacteristicTypes", &TYPES)?;
        let filter = t.children.get(2).ok_or(InvalidShape)?;
        let value = typed(
            filter,
            "xr:TypesFilterValue",
            0,
            &mut remaining,
            Some(index),
        )
        .map_err(|error| match error {
            CParamsEncodeError::ResourceLimit => ResourceLimit,
            _ => UnsupportedValue,
        })?;
        filters.push(
            encode_typed_with_predefined(index, predefined, &Value::Typed(value)).map_err(
                |error| match error {
                    TypedEncodeError::ResourceLimit => ResourceLimit,
                    _ => UnsupportedValue,
                },
            )?,
        );
    }
    let capacity = root.children.len().checked_add(1).ok_or(ResourceLimit)?;
    let count = i64::try_from(root.children.len()).map_err(|_| ResourceLimit)?;
    let mut records = vector(capacity)?;
    records.push(Node::int(count));
    for ((record, p), filter) in root.children.iter().zip(&prefixes.records).zip(filters) {
        plain(record, "xr:Characteristic")?;
        let [t, v] = record.children.as_slice() else {
            return Err(InvalidShape);
        };
        let tfrom = container(t, "xr:CharacteristicTypes", &TYPES)?;
        let vfrom = container(v, "xr:CharacteristicValues", &VALUES)?;
        let [key, types_filter, _, data_path, multi_use] = t.children.as_slice() else {
            return Err(InvalidShape);
        };
        let [object, type_, value, multi_key, multi_order] = v.children.as_slice() else {
            return Err(InvalidShape);
        };
        let texts = [
            leaf(object)?,
            leaf(type_)?,
            leaf(value)?,
            leaf(types_filter)?,
            leaf(key)?,
            leaf(data_path)?,
            leaf(multi_use)?,
            leaf(multi_key)?,
            leaf(multi_order)?,
        ];
        let names = NameLookup::once(&names, index).ok_or(ResourceLimit)?;
        let tg = source(names, tfrom)?;
        let vg = source(names, vfrom)?;
        let mut body = vector(13)?;
        body.extend([
            p.body.clone(),
            nodes([p.types_source.clone(), Node::guid(tg)])?,
            nodes([p.values_source.clone(), Node::guid(vg)])?,
        ]);
        for (j, ((text, fp), pos)) in texts.into_iter().zip(&p.fields).zip(POS).enumerate() {
            if pos == 8 {
                body.push(filter.clone())
            }
            let owner = if [0, 1, 2, 7, 8].contains(&j) {
                vfrom
            } else {
                tfrom
            };
            let segment = field(&ctx, names, owner, text, fp.segment)?;
            let mut f = vector(if fp.tail.is_some() { 3 } else { 2 })?;
            f.extend([fp.prefix.clone(), segment]);
            if let Some(t) = fp.tail {
                f.push(t.clone())
            }
            body.push(Node::list(f));
        }
        records.push(nodes([
            p.wrapper.clone(),
            Node::atom(TYPE_GUID),
            Node::list(body),
        ])?);
    }
    nodes([prefixes.outer.clone(), Node::list(records)])
}
