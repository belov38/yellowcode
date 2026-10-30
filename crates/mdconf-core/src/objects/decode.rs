use super::bags;
use super::codecs::{self, Ctx, Owner};
use super::overlay;
use super::path::{find_collection, prop_path};
use super::recalc::{decode_recalculation, decode_recalculation_observed};
use super::types::type_desc;
use crate::entries::{Entries, EntryData};
use crate::load::LoadCtx;
use crate::model::{
    Child, ChildTag, Diagnostics, GeneratedType, LocalString, MdObject, Prop, Recalculation,
    SubKind, TypeDescription, TypeRef, TypedValue, Value,
};
use crate::{Configuration, Result};
use brace::{Guid, Node};
use mdschema::{ChildSpec, Codec, InternalInfoSpec, MdKind, PropSpec, Schema};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

pub(crate) fn child_tag(name: &str) -> Option<ChildTag> {
    Some(match name {
        "Attribute" => ChildTag::Attribute,
        "TabularSection" => ChildTag::TabularSection,
        "Dimension" => ChildTag::Dimension,
        "Resource" => ChildTag::Resource,
        "EnumValue" => ChildTag::EnumValue,
        "Command" => ChildTag::Command,
        "AddressingAttribute" => ChildTag::AddressingAttribute,
        "AccountingFlag" => ChildTag::AccountingFlag,
        "ExtDimensionAccountingFlag" => ChildTag::ExtDimensionAccountingFlag,
        "Column" => ChildTag::Column,
        "URLTemplate" => ChildTag::URLTemplate,
        "Method" => ChildTag::Method,
        "Operation" => ChildTag::Operation,
        "Parameter" => ChildTag::Parameter,
        "IntegrationServiceChannel" => ChildTag::IntegrationServiceChannel,
        "Function" => ChildTag::Function,
        _ => return None,
    })
}

fn gt_child(tag: &str) -> &'static [&'static str] {
    match tag {
        "TabularSection" => &["TabularSection", "TabularSectionRow"],
        _ => &[],
    }
}

pub fn load(cx: &LoadCtx, cfg: &mut Configuration) -> Result<()> {
    let Configuration {
        objects,
        index,
        predef,
        diagnostics,
        ..
    } = cfg;
    let ctx = Ctx {
        schema: cx.schema,
        index,
        predef,
    };
    let mut dec = Decoder::new(&ctx);
    let forget = !cx.opts.keep_source;
    for o in objects.iter_mut().filter(|o| is_set_kind(o.kind)) {
        dec.object(cx.entries, o, diagnostics, forget)?;
        dec.sets.add(o);
    }
    for o in objects.iter_mut().filter(|o| !is_set_kind(o.kind)) {
        dec.object(cx.entries, o, diagnostics, forget)?;
    }
    Ok(())
}

pub(crate) fn decode_object(
    ctx: &Ctx,
    e: &Entries,
    shell: &mut MdObject,
    diag: &mut Diagnostics,
) -> Result<()> {
    Decoder::new(ctx).object(e, shell, diag, false)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) enum TreeDomain {
    #[default]
    Original,
    Normalized,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Provenance {
    Selected,
    Default,
    Derived,
    Unavailable,
}

#[allow(
    dead_code,
    reason = "Descriptor payloads consumed by future comparison integration and actual-path observer tests"
)]
#[derive(Clone, Copy, Debug)]
pub(crate) enum Descriptor<'p> {
    Header(Head),
    Spec(&'p PropSpec),
}

#[derive(Clone, Copy, Default)]
pub(super) struct PropertyIdentity {
    pub id: Guid,
    pub entry: Guid,
    pub domain: TreeDomain,
}

#[allow(
    dead_code,
    reason = "C7 internal observation facts await separately reviewed comparison consumer"
)]
pub(crate) struct ResolvedProperty<'p> {
    pub id: Guid,
    pub entry: Guid,
    pub scope: Option<&'p str>,
    pub tag: &'p str,
    pub prop: &'p Prop,
    pub owner: Option<&'p Owner>,
    pub descriptor: Descriptor<'p>,
    pub root: &'p Node,
    pub domain: TreeDomain,
    pub provenance: Provenance,
    pub raw: Option<&'p Node>,
    pub path: Option<&'p [usize]>,
    pub path2: Option<&'p [usize]>,
    pub raw2: Option<&'p Node>,
    pub masked: bool,
    pub type_filtered: bool,
}

pub(super) type Visitor<'v> = dyn for<'p> FnMut(ResolvedProperty<'p>) + 'v;

pub(super) struct SelectedSource<'p> {
    pub descriptor: Descriptor<'p>,
    pub root: &'p Node,
    pub raw: Option<&'p Node>,
    pub path: Option<&'p [usize]>,
    pub path2: Option<&'p [usize]>,
    pub provenance: Provenance,
}

#[allow(
    clippy::too_many_arguments,
    reason = "Single transient emission shared by ordinary and recalculation decoder branches"
)]
pub(super) fn emit(
    visitor: &mut Option<&mut Visitor<'_>>,
    ctx: &Ctx,
    identity: PropertyIdentity,
    tag: &str,
    owner: Option<&Owner>,
    prop: &Prop,
    source: SelectedSource<'_>,
    masked: bool,
    type_filtered: bool,
) {
    if let Some(v) = visitor.as_deref_mut() {
        v(ResolvedProperty {
            id: identity.id,
            entry: identity.entry,
            scope: ctx.index.names.get(&identity.id).map(String::as_str),
            tag,
            prop,
            owner,
            descriptor: source.descriptor,
            root: source.root,
            domain: identity.domain,
            provenance: source.provenance,
            raw: source.raw,
            path: source.path,
            path2: source.path2,
            raw2: source.path2.and_then(|p| source.root.get(p)),
            masked,
            type_filtered,
        });
    }
}

#[allow(
    dead_code,
    reason = "Internal observer entrypoint; public edit/comparison integration is a later reviewed slice"
)]
pub(crate) fn visit_properties(
    cfg: &Configuration,
    entries: &Entries,
    visitor: &mut Visitor<'_>,
) -> Result<Diagnostics> {
    let ctx = Ctx {
        schema: mdschema::schema()?,
        index: &cfg.index,
        predef: &cfg.predef,
    };
    let mut objects = crate::load::build_shells(&cfg.index);
    let mut dec = Decoder::new(&ctx);
    dec.visitor = Some(visitor);
    let mut diagnostics = Diagnostics::default();
    for o in objects.iter_mut().filter(|o| is_set_kind(o.kind)) {
        dec.object(entries, o, &mut diagnostics, true)?;
        dec.sets.add(o);
    }
    for o in objects.iter_mut().filter(|o| !is_set_kind(o.kind)) {
        dec.object(entries, o, &mut diagnostics, true)?;
    }
    Ok(diagnostics)
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Head {
    Name,
    Synonym,
    Comment,
}

#[derive(Clone, Copy)]
enum Src<'a> {
    Head(Head),
    Spec(&'a PropSpec),
    Fallback {
        fd: Option<&'a PropSpec>,
        hp: Option<&'a PropSpec>,
    },
}

type Plan<'a> = Arc<[(&'a str, Src<'a>)]>;

struct PropsOf<'s, 'a> {
    identity: PropertyIdentity,
    special: Option<(&'s str, &'s Node)>,
    spec_props: &'a [(String, PropSpec)],
    hdr: &'s [usize],
    props_type: &'s str,
    tag: &'s str,
    context: Option<&'s str>,
    owner: Option<&'s Owner>,
}

#[derive(Default)]
pub(super) struct Decoded {
    pub props: Vec<Prop>,
    pub name: String,
    pub synonym: LocalString,
    pub comment: String,
}

fn is_set_kind(k: MdKind) -> bool {
    k == MdKind::DefinedType
}

#[derive(Default)]
struct SetTypes {
    by_id: HashMap<brace::Guid, TypeDescription>,
    by_name: HashMap<String, brace::Guid>,
}

impl SetTypes {
    fn add(&mut self, o: &MdObject) {
        if let Some(Value::Type(t)) = o.prop("Type") {
            self.by_id.insert(o.id, t.clone());
            self.by_name.insert(o.name.clone(), o.id);
        }
    }

    fn get(&self, index: &crate::index::Index, r: &TypeRef) -> Option<TypeDescription> {
        let name = r
            .name
            .fixed_local(crate::FixedNamespace::Cfg)?
            .strip_prefix("DefinedType.")?;
        let by_id = r
            .type_id
            .and_then(|id| index.type_owners.get(&id))
            .and_then(|ow| self.by_id.get(&ow.object));
        by_id
            .or_else(|| self.by_id.get(self.by_name.get(name)?))
            .cloned()
    }
}

struct Decoder<'c, 'a, 'v> {
    visitor: Option<&'v mut Visitor<'v>>,
    identity: PropertyIdentity,
    ctx: &'c Ctx<'a>,
    sets: SetTypes,
    plans: HashMap<String, Plan<'a>>,
}

const LSTR: PropSpec = PropSpec {
    path: None,
    hp: None,
    hpp: None,
    rel_header: None,
    fd: None,
    codec: Codec::LStr,
    map: mdschema::FactMap::new(),
    xml_type: None,
    path2: None,
    false_if: None,
    std: None,
};

pub(super) fn miss(diag: &mut Diagnostics, key: String) {
    *diag.misses.entry(key).or_default() += 1;
}

fn at(path: &[usize], i: usize) -> Vec<usize> {
    let mut p = path.to_vec();
    p.push(i);
    p
}

fn cut(path: &[usize], cut: usize) -> &[usize] {
    path.get(..path.len().saturating_sub(cut))
        .unwrap_or_default()
}

fn text_at(node: &Node, path: &[usize]) -> String {
    node.get(path)
        .and_then(Node::text)
        .unwrap_or_default()
        .to_string()
}

fn text_prop(name: &str, text: String) -> Prop {
    Prop {
        name: name.into(),
        codec: Codec::Str,
        value: Value::Text(text),
    }
}

pub(crate) fn description_value(v: Value) -> Value {
    match v {
        Value::LStr(mut ls) => {
            for (_, t) in &mut ls.0 {
                *t = codecs::cf_text(t).into_owned();
            }
            Value::LStr(ls)
        }
        Value::Text(t) => Value::Text(codecs::cf_text(&t).into_owned()),
        Value::StandardAttributes(mut sa) => {
            for a in &mut sa {
                for p in &mut a.props {
                    let v = std::mem::replace(&mut p.value, Value::Bool(false));
                    p.value = description_value(v);
                }
            }
            Value::StandardAttributes(sa)
        }
        other => other,
    }
}

pub(super) fn header_prop<'n>(
    ctx: &Ctx,
    node: &'n Node,
    hdr: &[usize],
    h: Head,
    props_type: &str,
    out: &mut Decoded,
    diag: &mut Diagnostics,
) -> Option<(Vec<usize>, Option<&'n Node>, Provenance)> {
    let path = at(
        hdr,
        match h {
            Head::Name => 2,
            Head::Synonym => 3,
            Head::Comment => 4,
        },
    );
    let raw = node.get(&path);
    match h {
        Head::Name => {
            out.name = raw.and_then(Node::text).unwrap_or_default().to_string();
            out.props.push(text_prop("Name", out.name.clone()));
        }
        Head::Synonym => {
            let v = raw
                .and_then(|v| codecs::decode(ctx, "Synonym", &LSTR, v, None, None))
                .map(description_value);
            if let Some(Value::LStr(ls)) = v {
                out.synonym = ls.clone();
                out.props.push(Prop {
                    name: "Synonym".into(),
                    codec: Codec::LStr,
                    value: Value::LStr(ls),
                });
            } else {
                return None;
            }
        }
        Head::Comment => {
            if raw.is_some_and(|v| v.as_list().is_some()) {
                miss(diag, format!("repr:{props_type}.Comment"));
            }
            let c = raw
                .map(|n| codecs::cf_text(&codecs::py_str(n)).into_owned())
                .unwrap_or_default();
            out.comment = c.clone();
            out.props.push(text_prop("Comment", c));
        }
    }
    let provenance = if match h {
        Head::Name => raw.and_then(Node::text).is_none(),
        _ => raw.is_none(),
    } {
        Provenance::Default
    } else {
        Provenance::Selected
    };
    Some((path, raw, provenance))
}

fn is_empty_block(v: &Node) -> bool {
    matches!(v.as_list(), Some([x]) if x.text() == Some("0"))
}

fn is_field_desc(fd: Option<&Node>) -> bool {
    let Some(l) = fd.and_then(Node::as_list) else {
        return false;
    };
    l.len() > 20
        && l.get(1)
            .and_then(Node::as_list)
            .filter(|x| x.len() >= 3)
            .and_then(|x| x.get(2))
            .and_then(Node::as_list)
            .and_then(|p| p.first())
            .and_then(Node::text)
            == Some("Pattern")
}

fn order_props<'n>(
    xprops: &[&'n str],
    context_obs: Option<overlay::ObservedOrder<'n>>,
    tag_obs: overlay::ObservedOrder<'n>,
) -> Vec<&'n str> {
    let known: HashSet<&str> = xprops.iter().copied().collect();
    if let Some(obs) = context_obs {
        return obs.iter().filter(|n| known.contains(n)).collect();
    }
    if tag_obs.is_empty() {
        return xprops.to_vec();
    }
    let mut out: Vec<&str> = tag_obs.iter().filter(|n| known.contains(n)).collect();
    let mut seen: HashSet<&str> = out.iter().copied().collect();
    for (i, &p) in xprops.iter().enumerate() {
        if seen.contains(p) {
            continue;
        }
        let prev = xprops.iter().take(i).rev().find(|q| seen.contains(**q));
        let idx = prev
            .and_then(|q| out.iter().position(|x| x == q))
            .map_or(0, |k| k + 1);
        out.insert(idx, p);
        seen.insert(p);
    }
    out
}

fn ordered_props<'a>(
    s: &'static Schema,
    props_type: &str,
    tag: &str,
    context: Option<&str>,
) -> Vec<&'a str> {
    let xprops: Vec<&str> = s
        .xdto_props(props_type)
        .iter()
        .map(|p| p.name.as_str())
        .collect();
    let context_obs = context
        .filter(|c| overlay::order_count(s, c) >= 10)
        .map(|c| overlay::observed_order(s, c).unwrap_or_default());
    order_props(
        &xprops,
        context_obs,
        overlay::observed_order(s, tag).unwrap_or_default(),
    )
}

fn generated_types<S: AsRef<str>>(
    node: &Node,
    ii: &[InternalInfoSpec],
    cats_order: &[S],
) -> Vec<GeneratedType> {
    let mut cats: Vec<(&str, &InternalInfoSpec)> = Vec::new();
    for e in ii {
        match cats.iter_mut().find(|(c, _)| *c == e.category) {
            Some(slot) => slot.1 = e,
            None => cats.push((&e.category, e)),
        }
    }
    let ordered = cats_order.iter().map(AsRef::as_ref);
    let mut order: Vec<&str> = ordered
        .filter(|c| cats.iter().any(|(k, _)| k == c))
        .collect();
    order.extend(
        cats.iter()
            .map(|(c, _)| *c)
            .filter(|c| !cats_order.iter().any(|o| o.as_ref() == *c)),
    );
    order
        .into_iter()
        .filter_map(|cat| {
            let (_, e) = cats.iter().find(|(c, _)| *c == cat)?;
            Some(GeneratedType {
                category: cat.to_string(),
                type_id: node.get(&e.type_id)?.as_guid()?,
                value_id: node.get(&e.value_id)?.as_guid()?,
            })
        })
        .collect()
}

fn allowed_types(ctx: &Ctx, sets: &SetTypes, node: &Node, hdr: &[usize]) -> Option<Vec<String>> {
    let pat = node.get(&at(cut(hdr, 1), 2))?;
    let first = pat.as_list()?.first()?;
    if first.text() != Some("Pattern") {
        return None;
    }
    let d = type_desc(ctx, pat)?;
    let d = crate::types::expand_with(&d, &|r| sets.get(ctx.index, r));
    Some(
        d.types
            .into_iter()
            .filter(|t| !t.set)
            .map(|t| t.name.to_string())
            .collect(),
    )
}

fn fit_fill_value(v: Value, allowed: Option<&[String]>) -> Value {
    let (Value::Typed(tv), Some(allowed)) = (&v, allowed) else {
        return v;
    };
    let xs = match tv {
        TypedValue::Str(_) => "xs:string",
        TypedValue::Number(_) => "xs:decimal",
        TypedValue::Bool(_) => "xs:boolean",
        TypedValue::Date(_) => "xs:dateTime",
        _ => return v,
    };
    if allowed.iter().any(|a| a == xs) {
        v
    } else {
        Value::Typed(TypedValue::Nil)
    }
}

impl<'c, 'a, 'v> Decoder<'c, 'a, 'v> {
    fn new(ctx: &'c Ctx<'a>) -> Self {
        Decoder {
            visitor: None,
            identity: PropertyIdentity::default(),
            ctx,
            sets: SetTypes::default(),
            plans: HashMap::new(),
        }
    }

    fn plan(&mut self, of: &PropsOf<'_, 'a>) -> Plan<'a> {
        let key = format!(
            "{}\n{}\n{}\n{:p}",
            of.props_type,
            of.tag,
            of.context.unwrap_or_default(),
            of.spec_props.as_ptr()
        );
        if let Some(p) = self.plans.get(&key) {
            return Arc::clone(p);
        }
        let s: &'static Schema = self.ctx.schema;
        let manual: &'a [(String, PropSpec)] = s.tag_props(of.tag).unwrap_or_default();
        let find = |l: &'a [(String, PropSpec)], n: &str| -> Option<&'a PropSpec> {
            l.iter().find(|(k, _)| k == n).map(|(_, p)| p)
        };
        let plan: Plan<'a> = ordered_props(s, of.props_type, of.tag, of.context)
            .into_iter()
            .map(|n| {
                let src = match n {
                    "Name" => Src::Head(Head::Name),
                    "Synonym" => Src::Head(Head::Synonym),
                    "Comment" => Src::Head(Head::Comment),
                    _ => match find(manual, n).or_else(|| find(of.spec_props, n)) {
                        Some(p) => Src::Spec(p),
                        None => Src::Fallback {
                            fd: s.fd_template(n),
                            hp: s.hp_template(of.tag, n),
                        },
                    },
                };
                (n, src)
            })
            .collect();
        self.plans.insert(key, Arc::clone(&plan));
        plan
    }

    fn observe(
        &mut self,
        of: &PropsOf<'_, 'a>,
        prop: &Prop,
        mut source: SelectedSource<'_>,
        masked: bool,
        type_filtered: bool,
    ) {
        if self.visitor.is_none() {
            return;
        }
        let special = of
            .special
            .and_then(|(kind, raw)| overlay::special_prop(kind, &prop.name, raw));
        let final_prop;
        let prop = if let Some(value) = special {
            final_prop = Prop {
                name: prop.name.clone(),
                codec: prop.codec,
                value,
            };
            source.provenance = Provenance::Derived;
            source.raw = None;
            source.path = None;
            source.path2 = None;
            &final_prop
        } else {
            prop
        };
        emit(
            &mut self.visitor,
            self.ctx,
            of.identity,
            of.tag,
            of.owner,
            prop,
            source,
            masked,
            type_filtered,
        );
    }

    fn props(&mut self, node: &Node, of: &PropsOf<'_, 'a>, diag: &mut Diagnostics) -> Decoded {
        let ctx = self.ctx;
        let plan = self.plan(of);
        let hdr = of.hdr;
        let mut out = Decoded::default();
        for &(name, src) in plan.iter() {
            let (ent, path) = match src {
                Src::Head(h) => {
                    if let Some((path, raw, provenance)) =
                        header_prop(ctx, node, hdr, h, of.props_type, &mut out, diag)
                        && let Some(prop) = out.props.last()
                    {
                        self.observe(
                            of,
                            prop,
                            SelectedSource {
                                descriptor: Descriptor::Header(h),
                                root: node,
                                raw,
                                path: Some(&path),
                                path2: None,
                                provenance,
                            },
                            false,
                            false,
                        );
                    }
                    continue;
                }
                Src::Spec(e) => (e, prop_path(e, Some(hdr))),
                Src::Fallback { fd, hp } => {
                    let fd = fd.filter(|_| hdr.len() >= 3 && is_field_desc(node.get(cut(hdr, 2))));
                    if let Some(t) = fd {
                        let path = t.fd.as_ref().map(|f| {
                            let mut p = cut(hdr, 2).to_vec();
                            p.extend_from_slice(f);
                            p
                        });
                        (t, path)
                    } else if let Some(h) = hp {
                        (h, prop_path(h, Some(hdr)))
                    } else {
                        miss(diag, format!("unknown:{}.{name}", of.props_type));
                        continue;
                    }
                }
            };
            let v2 = ent
                .path2
                .as_ref()
                .map(|p| node.get(p).map(codecs::py_str).unwrap_or_default());
            if let Some((p, val)) = &ent.false_if
                && node.get(p).map_or_else(|| "None".into(), codecs::py_str) == *val
            {
                let prop = Prop {
                    name: name.into(),
                    codec: ent.codec,
                    value: Value::Bool(false),
                };
                if self.visitor.is_some() {
                    let raw = path.as_deref().and_then(|p| node.get(p));
                    self.observe(
                        of,
                        &prop,
                        SelectedSource {
                            descriptor: Descriptor::Spec(ent),
                            root: node,
                            raw,
                            path: path.as_deref(),
                            path2: ent.path2.as_deref(),
                            provenance: if raw.is_some() {
                                Provenance::Selected
                            } else {
                                Provenance::Unavailable
                            },
                        },
                        true,
                        false,
                    );
                }
                out.props.push(prop);
                continue;
            }
            let allowed = if name == "FillValue" {
                allowed_types(ctx, &self.sets, node, hdr)
            } else {
                None
            };
            let raw = path.as_deref().and_then(|p| node.get(p));
            if matches!(name, "StandardAttributes" | overlay::STS)
                && raw.is_some_and(is_empty_block)
            {
                continue;
            }
            let value = raw.and_then(|v| match (name, ent.codec, of.owner) {
                (overlay::STS, Codec::Unknown, Some(o)) => bags::std_tabular_sections(ctx, o, v),
                _ => codecs::decode(ctx, name, ent, v, of.owner, v2.as_deref()),
            });
            if value.is_some()
                && matches!(ent.codec, Codec::Int | Codec::Str | Codec::Event)
                && path
                    .as_deref()
                    .and_then(|p| node.get(p))
                    .is_some_and(|v| v.as_list().is_some())
            {
                miss(diag, format!("repr:{}.{name}", of.props_type));
            }
            match value {
                Some(v) => {
                    let v = description_value(v);
                    let before = (self.visitor.is_some() && name == "FillValue").then(|| v.clone());
                    let value = fit_fill_value(v, allowed.as_deref());
                    let filtered = before.as_ref().is_some_and(|v| v != &value);
                    let prop = Prop {
                        name: name.into(),
                        codec: ent.codec,
                        value,
                    };
                    self.observe(
                        of,
                        &prop,
                        SelectedSource {
                            descriptor: Descriptor::Spec(ent),
                            root: node,
                            raw,
                            path: path.as_deref(),
                            path2: ent.path2.as_deref(),
                            provenance: Provenance::Selected,
                        },
                        false,
                        filtered,
                    );
                    out.props.push(prop);
                }
                None => miss(diag, format!("fail:{}.{name}", of.props_type)),
            }
        }
        out
    }

    fn child(
        &mut self,
        item: &Node,
        cs: &'a ChildSpec,
        obj: &Owner,
        parent_tag: Option<&str>,
        diag: &mut Diagnostics,
    ) -> Option<Child> {
        let tag = child_tag(&cs.tag)?;
        let hdr = cs.header.as_deref()?;
        let id = cs
            .uuid
            .as_deref()
            .and_then(|p| item.get(p))
            .and_then(Node::as_guid)
            .unwrap_or_default();
        let generated_types = generated_types(item, &cs.internal_info, gt_child(&cs.tag));
        let context = match parent_tag {
            Some(p) => format!("{}.{p}.{}", obj.kind, cs.tag),
            None => format!("{}.{}", obj.kind, cs.tag),
        };
        let props_type = format!("{}Properties", cs.tag);
        let d = self.props(
            item,
            &PropsOf {
                identity: PropertyIdentity {
                    id,
                    ..self.identity
                },
                special: None,
                spec_props: &cs.props,
                hdr,
                props_type: &props_type,
                tag: &cs.tag,
                context: Some(&context),
                owner: Some(obj),
            },
            diag,
        );
        let children = if !cs.children.is_empty() || cs.tag == "TabularSection" {
            let co = format!("{}ChildObjects", cs.tag);
            self.children(item, &cs.children, &[], obj, &co, Some(&cs.tag), diag)
        } else {
            Vec::new()
        };
        Some(Child {
            tag,
            id,
            name: text_at(item, &at(hdr, 2)),
            synonym: d.synonym,
            comment: d.comment,
            generated_types,
            props: d.props,
            children,
            module: None,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn children(
        &mut self,
        node: &Node,
        spec_children: &'a [(String, ChildSpec)],
        subobjects: &[(String, String)],
        obj: &Owner,
        co_type: &str,
        parent_tag: Option<&str>,
        diag: &mut Diagnostics,
    ) -> Vec<Child> {
        let s: &'static Schema = self.ctx.schema;
        let mut out = Vec::new();
        for p in s.xdto_props(co_type) {
            let tag = p.name.as_str();
            if let Some((g, cs)) = spec_children.iter().rev().find(|(_, c)| c.tag == tag) {
                for it in find_collection(node, g).unwrap_or_default() {
                    match self.child(it, cs, obj, parent_tag, diag) {
                        Some(c) => out.push(c),
                        None => miss(diag, format!("unknown:{co_type}.{tag}")),
                    }
                }
            } else if !subobjects.iter().any(|(t, _)| t == tag)
                && (tag == "Form" || tag == "Template")
            {
                miss(diag, format!("unknown:{co_type}.{tag}"));
            }
        }
        out
    }

    fn object(
        &mut self,
        e: &Entries,
        o: &mut MdObject,
        diag: &mut Diagnostics,
        forget: bool,
    ) -> Result<()> {
        let s: &'static Schema = self.ctx.schema;
        let kind = o.kind.as_str();
        if let Some(spec) = overlay::kind(s, kind)
            && let Some(hdr) = spec.header.as_deref()
        {
            let entry = o.id.to_string();
            if let Some(data) = e.doc(&entry)?
                && let EntryData::Brace(doc) = &*data
            {
                let raw = &doc.root;
                let norm = overlay::normalize(kind, raw);
                let t = &*norm;
                self.identity = PropertyIdentity {
                    id: o.id,
                    entry: o.id,
                    domain: if matches!(norm, std::borrow::Cow::Owned(_)) {
                        TreeDomain::Normalized
                    } else {
                        TreeDomain::Original
                    },
                };
                let owner = Owner {
                    kind: kind.to_string(),
                    name: o.name.clone(),
                };
                o.generated_types = generated_types(t, &spec.internal_info, s.gen_types(kind));
                o.this_node = spec
                    .this_node
                    .as_deref()
                    .and_then(|p| t.get(p))
                    .and_then(Node::as_guid);
                let props_type = format!("{kind}Properties");
                let d = self.props(
                    t,
                    &PropsOf {
                        identity: self.identity,
                        special: Some((kind, raw)),
                        spec_props: &spec.props,
                        hdr,
                        props_type: &props_type,
                        tag: kind,
                        context: None,
                        owner: Some(&owner),
                    },
                    diag,
                );
                o.props = d.props;
                for p in &mut o.props {
                    if let Some(v) = overlay::special_prop(kind, &p.name, raw) {
                        p.value = v;
                    }
                }
                o.synonym = d.synonym;
                o.comment = d.comment;
                let co = format!("{kind}ChildObjects");
                if s.has_xdto_type(&co) {
                    o.children =
                        self.children(t, &spec.children, &spec.subobjects, &owner, &co, None, diag);
                }
            }
            if forget {
                e.forget(&entry);
            }
            for sub in o.forms.iter_mut().chain(o.templates.iter_mut()) {
                self.subobject(e, sub, diag, forget)?;
            }
            let recalcs = self.recalculations(e, o, diag, forget)?;
            o.recalculations = recalcs;
        }
        for n in &mut o.subsystems {
            self.object(e, n, diag, forget)?;
        }
        Ok(())
    }

    fn recalculations(
        &mut self,
        e: &Entries,
        register: &MdObject,
        diag: &mut Diagnostics,
        forget: bool,
    ) -> Result<Vec<Recalculation>> {
        let ids = self
            .ctx
            .index
            .recalcs
            .get(&register.id)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let mut out = Vec::with_capacity(ids.len());
        for &id in ids {
            let entry = id.to_string();
            if let Some(data) = e.doc(&entry)?
                && let EntryData::Brace(doc) = &*data
                && let Some(r) = if self.visitor.is_some() {
                    decode_recalculation_observed(
                        self.ctx,
                        register,
                        id,
                        &doc.root,
                        diag,
                        &mut self.visitor,
                    )
                } else {
                    decode_recalculation(self.ctx, register, id, &doc.root, diag)
                }
            {
                out.push(r);
            }
            if forget {
                e.forget(&entry);
            }
        }
        Ok(out)
    }

    fn subobject(
        &mut self,
        e: &Entries,
        sub: &mut crate::model::SubObject,
        diag: &mut Diagnostics,
        forget: bool,
    ) -> Result<()> {
        let sk = match sub.kind {
            SubKind::Form => "Form",
            SubKind::Template => "Template",
        };
        let s: &'static Schema = self.ctx.schema;
        let Some(spec) = overlay::kind(s, sk) else {
            return Ok(());
        };
        let hdr = self
            .ctx
            .index
            .sub_hdr
            .get(&sub.id)
            .map(Vec::as_slice)
            .filter(|h| !h.is_empty())
            .or(spec.header.as_deref());
        let entry = sub.id.to_string();
        if let Some(hdr) = hdr
            && let Some(data) = e.doc(&entry)?
            && let EntryData::Brace(doc) = &*data
        {
            let props_type = format!("{sk}Properties");
            let d = self.props(
                &doc.root,
                &PropsOf {
                    identity: PropertyIdentity {
                        id: sub.id,
                        entry: sub.id,
                        domain: TreeDomain::Original,
                    },
                    special: None,
                    spec_props: &spec.props,
                    hdr,
                    props_type: &props_type,
                    tag: sk,
                    context: None,
                    owner: None,
                },
                diag,
            );
            sub.props = d.props;
        }
        if forget {
            e.forget(&entry);
        }
        Ok(())
    }
}
