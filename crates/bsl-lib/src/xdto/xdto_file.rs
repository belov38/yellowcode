use super::lexical::{MAX_ENUMERATION, MAX_PATTERNS_PER_TYPE, validate_pattern};
use super::model::*;
use super::{Fault, Scope, XdtoParseError, XmlElement, perr, read_tree};
use crate::xml::XmlReader;

const XDTO_NS: &str = "http://v8.1c.ru/8.1/xdto";
const XSI_NS: &str = "http://www.w3.org/2001/XMLSchema-instance";
const XS_NS: &str = super::lexical::XS_NS;
const MAX_NESTING: usize = 32;

const FACET_NAMES: &[&str] = &[
    "length",
    "minLength",
    "maxLength",
    "pattern",
    "enumeration",
    "whiteSpace",
    "minInclusive",
    "maxInclusive",
    "minExclusive",
    "maxExclusive",
    "totalDigits",
    "fractionDigits",
];

pub(crate) fn is_facet(name: &str) -> bool {
    FACET_NAMES.contains(&name)
}

#[derive(Default)]
pub(crate) struct Budget {
    patterns: usize,
    pattern_chars: usize,
    enums: usize,
}

const MAX_PATTERNS_TOTAL: usize = 4096;
const MAX_PATTERN_CHARS_TOTAL: usize = 256 * 1024;
const MAX_ENUMERATION_TOTAL: usize = 200_000;

pub(crate) fn apply_facet(
    f: &mut Facets,
    name: &str,
    value: &str,
    budget: &mut Budget,
) -> Result<(), XdtoParseError> {
    fn uint(name: &str, v: &str) -> Result<u64, XdtoParseError> {
        match v.trim().parse::<u64>() {
            Ok(n) => Ok(n),
            Err(_) => perr(format!("facet {name}: `{v}` is not a non-negative integer")),
        }
    }
    fn once<T>(slot: &mut Option<T>, name: &str, v: T) -> Result<(), XdtoParseError> {
        if slot.is_some() {
            return perr(format!("facet {name} is given twice"));
        }
        *slot = Some(v);
        Ok(())
    }
    match name {
        "length" => once(&mut f.length, name, uint(name, value)?),
        "minLength" => once(&mut f.min_length, name, uint(name, value)?),
        "maxLength" => once(&mut f.max_length, name, uint(name, value)?),
        "totalDigits" => {
            let n =
                u32::try_from(uint(name, value)?).or_else(|_| perr("totalDigits is too big"))?;
            once(&mut f.total_digits, name, n)
        }
        "fractionDigits" => {
            let n =
                u32::try_from(uint(name, value)?).or_else(|_| perr("fractionDigits is too big"))?;
            once(&mut f.fraction_digits, name, n)
        }
        "minInclusive" => once(&mut f.min_inclusive, name, value.trim().to_string()),
        "maxInclusive" => once(&mut f.max_inclusive, name, value.trim().to_string()),
        "minExclusive" => once(&mut f.min_exclusive, name, value.trim().to_string()),
        "maxExclusive" => once(&mut f.max_exclusive, name, value.trim().to_string()),
        "whiteSpace" => {
            let w = match value.trim() {
                "preserve" | "Preserve" => WhiteSpace::Preserve,
                "replace" | "Replace" => WhiteSpace::Replace,
                "collapse" | "Collapse" => WhiteSpace::Collapse,
                other => return perr(format!("facet whiteSpace: unknown value `{other}`")),
            };
            once(&mut f.white_space, name, w)
        }
        "pattern" => {
            if f.patterns.len() >= MAX_PATTERNS_PER_TYPE {
                return perr(format!(
                    "more than {MAX_PATTERNS_PER_TYPE} patterns in one type"
                ));
            }
            budget.patterns += 1;
            budget.pattern_chars += value.chars().count();
            if budget.patterns > MAX_PATTERNS_TOTAL
                || budget.pattern_chars > MAX_PATTERN_CHARS_TOTAL
            {
                return perr("too many patterns in the schema");
            }
            if let Err(e) = validate_pattern(value) {
                return Err(XdtoParseError {
                    message: format!("facet pattern `{value}`: {e}"),
                    fault: Some(Fault::Pattern {
                        ty: QName::new("", ""),
                        pattern: value.to_string(),
                        reason: e,
                    }),
                });
            }
            f.patterns.push(value.to_string());
            Ok(())
        }
        "enumeration" => {
            if f.enumeration.len() >= MAX_ENUMERATION {
                return perr(format!(
                    "more than {MAX_ENUMERATION} enumeration values in one type"
                ));
            }
            budget.enums += 1;
            if budget.enums > MAX_ENUMERATION_TOTAL {
                return perr("too many enumeration values in the schema");
            }
            f.enumeration.push(value.to_string());
            Ok(())
        }
        other => perr(format!("unknown facet `{other}`")),
    }
}

pub(crate) fn parse_bool(what: &str, v: &str) -> Result<bool, XdtoParseError> {
    match v.trim() {
        "true" | "1" => Ok(true),
        "false" | "0" => Ok(false),
        other => perr(format!("{what}: `{other}` is not a boolean")),
    }
}

struct Pending {
    pkg: usize,
    ty: Option<usize>,
    prop: usize,
    target: QName,
    at: String,
}

pub(crate) fn unique_anon(used: &mut std::collections::HashSet<String>, base: String) -> String {
    let mut name = base.clone();
    let mut i = 1;
    while !used.insert(name.clone()) {
        i += 1;
        name = format!("{base}~{i}");
    }
    name
}

pub(crate) fn dedupe(v: Vec<QName>) -> Vec<QName> {
    let mut out: Vec<QName> = Vec::with_capacity(v.len());
    for q in v {
        if !out.contains(&q) {
            out.push(q);
        }
    }
    out
}

struct Ctx {
    anon: std::collections::HashSet<String>,
    pkg: Package,
    pkg_index: usize,
    pending: Vec<Pending>,
    budget: Budget,
    ordered: Vec<(QName, Option<bool>)>,
}

fn check_attrs(e: &XmlElement, allowed: &[&str]) -> Result<(), XdtoParseError> {
    for a in &e.attrs {
        if a.qname == "xmlns" || a.prefix == "xmlns" || !a.ns.is_empty() {
            continue;
        }
        if !allowed.contains(&a.local.as_str()) {
            return perr(format!(
                "<{}>: unsupported attribute `{}`",
                e.local, a.local
            ));
        }
    }
    Ok(())
}

fn xsi_type(e: &XmlElement) -> Option<&str> {
    e.attrs
        .iter()
        .find(|a| a.ns == XSI_NS && a.local == "type")
        .map(|a| a.value.as_str())
}

pub fn parse_xdto_model(text: &str) -> Result<Vec<Package>, XdtoParseError> {
    let tree = read_tree(&mut XmlReader::new(text.to_string())).map_err(|e| XdtoParseError {
        message: format!("model.xdto: {} (line {})", e.message, e.line),
        fault: None,
    })?;
    if tree.ns != XDTO_NS || !matches!(tree.local.as_str(), "model" | "Model") {
        return perr(format!(
            "model.xdto: the root is <{}>, expected <model> of {XDTO_NS}",
            tree.local
        ));
    }
    let mut scope = Scope::default();
    scope.push(&tree);
    let mut packages: Vec<Package> = Vec::new();
    let mut pending: Vec<Pending> = Vec::new();
    let mut budget = Budget::default();
    let mut ordered = Vec::new();
    for child in tree.elements() {
        if child.local != "package" {
            return perr(format!("<model>: unsupported element <{}>", child.local));
        }
        let mut ctx = Ctx {
            anon: Default::default(),
            pkg: Package::default(),
            pkg_index: packages.len(),
            pending: Vec::new(),
            budget: std::mem::take(&mut budget),
            ordered: Vec::new(),
        };
        parse_package(child, &mut scope, &mut ctx)?;
        budget = std::mem::take(&mut ctx.budget);
        pending.append(&mut ctx.pending);
        ordered.append(&mut ctx.ordered);
        packages.push(ctx.pkg);
    }
    scope.pop();
    for p in pending {
        let found = packages
            .iter()
            .filter(|k| k.ns == p.target.ns)
            .flat_map(|k| k.globals.iter())
            .find(|g| g.name == p.target.name)
            .cloned();
        let Some(g) = found else {
            return perr(format!(
                "{}: unresolved property reference {}",
                p.at,
                p.target.clark()
            ));
        };
        let slot = match p.ty {
            Some(t) => match &mut packages[p.pkg].types[t] {
                TypeDef::Object(o) => &mut o.properties[p.prop],
                TypeDef::Value(_) => continue,
            },
            None => &mut packages[p.pkg].globals[p.prop],
        };
        slot.ty = g.ty;
    }
    resolve_ordered(&mut packages, ordered)?;
    Ok(packages)
}

fn resolve_ordered(
    packages: &mut [Package],
    declarations: Vec<(QName, Option<bool>)>,
) -> Result<(), XdtoParseError> {
    use std::collections::HashMap;
    let flags: HashMap<_, _> = declarations.into_iter().collect();
    let bases: HashMap<_, _> = packages
        .iter()
        .flat_map(|p| &p.types)
        .filter_map(|t| {
            if let TypeDef::Object(o) = t {
                Some((o.name.clone(), o.base.clone()))
            } else {
                None
            }
        })
        .collect();
    let mut resolved = HashMap::new();
    for name in bases.keys() {
        let mut current = name.clone();
        let mut path = Vec::new();
        let value = loop {
            if let Some(value) = resolved.get(&current) {
                break *value;
            }
            if let Some(Some(value)) = flags.get(&current) {
                break *value;
            }
            if path.contains(&current) || path.len() >= 256 {
                return perr("cyclic or excessively deep ordered inheritance");
            }
            path.push(current.clone());
            match bases.get(&current).and_then(Option::as_ref) {
                Some(base) => current = base.clone(),
                None => break true,
            }
        };
        resolved.insert(current, value);
        for item in path {
            resolved.insert(item, value);
        }
    }
    for t in packages.iter_mut().flat_map(|p| &mut p.types) {
        if let TypeDef::Object(o) = t {
            o.ordered = resolved.get(&o.name).copied().unwrap_or(true);
        }
    }
    Ok(())
}

fn parse_package(e: &XmlElement, scope: &mut Scope, ctx: &mut Ctx) -> Result<(), XdtoParseError> {
    scope.push(e);
    check_attrs(
        e,
        &[
            "targetNamespace",
            "elementFormQualified",
            "attributeFormQualified",
        ],
    )?;
    let Some(ns) = e.attr("targetNamespace") else {
        return perr("<package>: no targetNamespace");
    };
    ctx.pkg.ns = ns.to_string();
    ctx.pkg.element_qualified = match e.attr("elementFormQualified") {
        Some(v) => parse_bool("elementFormQualified", v)?,
        None => false,
    };
    ctx.pkg.attribute_qualified = match e.attr("attributeFormQualified") {
        Some(v) => parse_bool("attributeFormQualified", v)?,
        None => false,
    };
    for c in e.elements() {
        match c.local.as_str() {
            "import" => {
                check_attrs(c, &["namespace"])?;
                if c.attr("namespace").is_none() {
                    return perr(format!("<import> in package {ns}: no namespace"));
                }
            }
            "valueType" => {
                scope.push(c);
                let name = required(c, "name", "valueType")?;
                let vt = parse_value_type(c, name, scope, ctx, 0)?;
                ctx.pkg.types.push(TypeDef::Value(vt));
                scope.pop();
            }
            "objectType" => {
                scope.push(c);
                let name = required(c, "name", "objectType")?;
                let ot = parse_object_type(c, name, scope, ctx, 0)?;
                ctx.pkg.types.push(TypeDef::Object(ot));
                scope.pop();
            }
            "property" => {
                scope.push(c);
                let (p, target) = parse_property(c, "", scope, ctx, 0)?;
                let mut p = p;
                p.qualified = true;
                let idx = ctx.pkg.globals.len();
                ctx.pkg.globals.push(p);
                if let Some(t) = target {
                    ctx.pending.push(Pending {
                        pkg: ctx.pkg_index,
                        ty: None,
                        prop: idx,
                        target: t,
                        at: format!("package {ns}"),
                    });
                }
                scope.pop();
            }
            other => return perr(format!("<package>: unsupported element <{other}>")),
        }
    }
    scope.pop();
    Ok(())
}

fn required<'a>(e: &'a XmlElement, attr: &str, what: &str) -> Result<&'a str, XdtoParseError> {
    match e.attr(attr) {
        Some(v) if !v.is_empty() => Ok(v),
        _ => perr(format!("<{what}>: no `{attr}`")),
    }
}

fn opt_qname(e: &XmlElement, attr: &str, scope: &Scope) -> Result<Option<QName>, XdtoParseError> {
    e.attr(attr)
        .map(|v| scope.qname(v, &format!("<{}> {attr}", e.local)))
        .transpose()
}

fn flag(e: &XmlElement, attr: &str) -> Result<bool, XdtoParseError> {
    match e.attr(attr) {
        Some(v) => parse_bool(attr, v),
        None => Ok(false),
    }
}

fn parse_value_type(
    e: &XmlElement,
    name: &str,
    scope: &mut Scope,
    ctx: &mut Ctx,
    depth: usize,
) -> Result<ValueType, XdtoParseError> {
    if depth > MAX_NESTING {
        return perr("typeDef nesting is too deep");
    }
    let mut allowed = vec![
        "name",
        "base",
        "variety",
        "itemType",
        "memberTypes",
        "ordered",
    ];
    allowed.extend_from_slice(FACET_NAMES);
    check_attrs(e, &allowed)?;
    let base = opt_qname(e, "base", scope)?;
    let mut facets = Facets::default();
    for f in FACET_NAMES {
        if *f == "pattern" || *f == "enumeration" {
            continue;
        }
        if let Some(v) = e.attr(f) {
            apply_facet(&mut facets, f, v, &mut ctx.budget)?;
        }
    }
    let tname = QName::new(&ctx.pkg.ns, name);
    let mut anon: Vec<QName> = Vec::new();
    let mut n = 0usize;
    for c in e.elements() {
        scope.push(c);
        let r = if is_facet(&c.local) {
            let raw = c.text();
            let v = if c.local == "enumeration" || c.local == "pattern" {
                raw.as_str()
            } else {
                raw.trim()
            };
            apply_facet(&mut facets, &c.local, v, &mut ctx.budget)
        } else if c.local == "typeDef" {
            n += 1;
            let an = format!("{name}#{n}");
            let q = parse_typedef(c, &an, scope, ctx, depth + 1)?;
            anon.push(q);
            Ok(())
        } else {
            perr(format!(
                "<valueType {name}>: unsupported element <{}>",
                c.local
            ))
        };
        scope.pop();
        r?;
    }
    let declared = match (
        e.attr("variety").map(str::trim),
        e.attr("itemType"),
        e.attr("memberTypes"),
    ) {
        (Some(v), _, _) => v,
        (None, Some(_), _) => "List",
        (None, None, Some(_)) => "Union",
        (None, None, None) if base.is_none() && anon.len() == 1 => "List",
        (None, None, None) => "Atomic",
    };
    let variety = match declared {
        "Atomic" => {
            if e.attr("itemType").is_some() {
                return perr(format!("<valueType {name}>: itemType in an atomic type"));
            }
            Variety::Atomic
        }
        "List" => {
            let item = match (opt_qname(e, "itemType", scope)?, anon.first()) {
                (Some(q), _) => q,
                (None, Some(q)) => q.clone(),
                (None, None) => {
                    return perr(format!("<valueType {name}>: a list without itemType"));
                }
            };
            Variety::List { item }
        }
        "Union" => {
            let mut members = Vec::new();
            if let Some(v) = e.attr("memberTypes") {
                for part in v.split_whitespace() {
                    members.push(scope.qname(part, "memberTypes")?);
                }
            }
            members.extend(anon);
            let members = dedupe(members);
            if members.is_empty() {
                return perr(format!("<valueType {name}>: a union without members"));
            }
            Variety::Union { members }
        }
        other => return perr(format!("<valueType {name}>: unknown variety `{other}`")),
    };
    Ok(ValueType {
        name: tname,
        base,
        variety,
        facets,
    })
}

fn parse_typedef(
    e: &XmlElement,
    name: &str,
    scope: &mut Scope,
    ctx: &mut Ctx,
    depth: usize,
) -> Result<QName, XdtoParseError> {
    let is_value = match xsi_type(e).map(|t| t.rsplit(':').next().unwrap_or(t)) {
        Some("ValueType") => true,
        Some("ObjectType") => false,
        Some(other) => return perr(format!("<typeDef>: unknown xsi:type `{other}`")),
        None => {
            e.attr("variety").is_some()
                || e.attr("itemType").is_some()
                || e.attr("memberTypes").is_some()
                || e.elements()
                    .any(|c| is_facet(&c.local) || c.local == "typeDef")
        }
    };
    let def = if is_value {
        TypeDef::Value(parse_value_type(e, name, scope, ctx, depth)?)
    } else {
        TypeDef::Object(parse_object_type(e, name, scope, ctx, depth)?)
    };
    let q = def.name().clone();
    ctx.pkg.anonymous_types.push(ctx.pkg.types.len());
    ctx.pkg.types.push(def);
    Ok(q)
}

fn parse_object_type(
    e: &XmlElement,
    name: &str,
    scope: &mut Scope,
    ctx: &mut Ctx,
    depth: usize,
) -> Result<ObjectType, XdtoParseError> {
    if depth > MAX_NESTING {
        return perr("typeDef nesting is too deep");
    }
    check_attrs(
        e,
        &[
            "name",
            "base",
            "open",
            "abstract",
            "mixed",
            "sequenced",
            "ordered",
        ],
    )?;
    let base = opt_qname(e, "base", scope)?;
    let mut props: Vec<Property> = Vec::new();
    let mut refs: Vec<(usize, QName)> = Vec::new();
    for c in e.elements() {
        if c.local != "property" {
            return perr(format!(
                "<objectType {name}>: unsupported element <{}>",
                c.local
            ));
        }
        scope.push(c);
        let (p, target) = parse_property(c, name, scope, ctx, depth + 1)?;
        scope.pop();
        if let Some(t) = target {
            refs.push((props.len(), t));
        }
        props.push(p);
    }
    let mut order: Vec<usize> = (0..props.len()).collect();
    order.sort_by_key(|&i| usize::from(props[i].form != Form::Attribute));
    let mut remap = vec![0usize; props.len()];
    for (new, &old) in order.iter().enumerate() {
        remap[old] = new;
    }
    let mut sorted: Vec<Option<Property>> = props.into_iter().map(Some).collect();
    let properties: Vec<Property> = order.iter().filter_map(|&i| sorted[i].take()).collect();
    let ty_index = ctx.pkg.types.len();
    for (old, target) in refs {
        ctx.pending.push(Pending {
            pkg: ctx.pkg_index,
            ty: Some(ty_index),
            prop: remap[old],
            target,
            at: format!("objectType {name}"),
        });
    }
    let declared_ordered = e
        .attr("ordered")
        .map(|v| parse_bool("ordered", v))
        .transpose()?;
    ctx.ordered
        .push((QName::new(&ctx.pkg.ns, name), declared_ordered));
    Ok(ObjectType {
        name: QName::new(&ctx.pkg.ns, name),
        base,
        open: flag(e, "open")?,
        abstract_: flag(e, "abstract")?,
        mixed: flag(e, "mixed")?,
        sequenced: flag(e, "sequenced")?,
        ordered: declared_ordered.unwrap_or(true),
        properties,
    })
}

fn parse_property(
    e: &XmlElement,
    owner: &str,
    scope: &mut Scope,
    ctx: &mut Ctx,
    depth: usize,
) -> Result<(Property, Option<QName>), XdtoParseError> {
    check_attrs(
        e,
        &[
            "name",
            "type",
            "ref",
            "localName",
            "lowerBound",
            "upperBound",
            "nillable",
            "form",
            "qualified",
            "default",
            "fixed",
        ],
    )?;
    let any = QName::new(XS_NS, "anyType");
    let target = opt_qname(e, "ref", scope)?;
    let name = match (&target, e.attr("name")) {
        (Some(t), _) => t.name.clone(),
        (None, Some(n)) if !n.is_empty() => n.to_string(),
        _ => return perr(format!("<property> in {owner}: no name")),
    };
    let lower = match e.attr("lowerBound") {
        Some(v) => v
            .trim()
            .parse::<u32>()
            .or_else(|_| perr(format!("property {name}: bad lowerBound `{v}`")))?,
        None => 1,
    };
    let upper = match e.attr("upperBound") {
        Some(v) => v
            .trim()
            .parse::<i64>()
            .ok()
            .filter(|n| *n >= -1)
            .map_or_else(
                || perr(format!("property {name}: bad upperBound `{v}`")),
                Ok,
            )?,
        None => 1,
    };
    let form = match e.attr("form").map(str::trim) {
        None | Some("Element") => Form::Element,
        Some("Attribute") => Form::Attribute,
        Some("Text") => Form::Text,
        Some(other) => return perr(format!("property {name}: unknown form `{other}`")),
    };
    let qualified = match e.attr("qualified") {
        Some(v) => parse_bool("qualified", v)?,
        None if target.is_some() => true,
        None => match form {
            Form::Attribute => ctx.pkg.attribute_qualified,
            Form::Element => ctx.pkg.element_qualified,
            Form::Text => false,
        },
    };
    let mut ty = opt_qname(e, "type", scope)?.unwrap_or(any);
    let mut n = 0;
    for c in e.elements() {
        if c.local != "typeDef" {
            return perr(format!(
                "property {name}: unsupported element <{}>",
                c.local
            ));
        }
        n += 1;
        if n > 1 || e.attr("type").is_some() {
            return perr(format!("property {name}: more than one type"));
        }
        scope.push(c);
        let an = unique_anon(&mut ctx.anon, format!("{owner}#{name}"));
        ty = parse_typedef(c, &an, scope, ctx, depth + 1)?;
        scope.pop();
    }
    Ok((
        Property {
            name,
            ty,
            lower,
            upper,
            nillable: flag(e, "nillable")?,
            form,
            qualified,
            default: e.attr("default").map(str::to_string),
            local_name: if form == Form::Text {
                Some(String::new())
            } else {
                e.attr("localName").map(str::to_string)
            },
            fixed: None,
        },
        target,
    ))
}
