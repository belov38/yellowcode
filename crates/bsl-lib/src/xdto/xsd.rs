use super::lexical::XS_NS;
use super::model::*;
use super::xdto_file::{Budget, apply_facet, dedupe, is_facet, parse_bool, unique_anon};
use super::{Fault, Scope, XdtoParseError, XmlElement, perr};

const MAX_NESTING: usize = 32;

struct Ctx {
    anon: std::collections::HashSet<String>,
    pkg: Package,
    el_q: bool,
    at_q: bool,
    budget: Budget,
    refs: Vec<(Option<usize>, usize, QName)>,
}

fn check_attrs(e: &XmlElement, allowed: &[&str]) -> Result<(), XdtoParseError> {
    for a in &e.attrs {
        if a.qname == "xmlns" || a.prefix == "xmlns" {
            continue;
        }
        if !a.ns.is_empty() {
            continue;
        }
        if !allowed.contains(&a.local.as_str()) {
            return perr(format!(
                "XSD: attribute `{}` of <{}> is not supported",
                a.local, e.local
            ));
        }
    }
    Ok(())
}

fn unsupported<T>(e: &XmlElement, within: &str) -> Result<T, XdtoParseError> {
    perr(format!("XSD: <{}> in <{within}> is not supported", e.local))
}

fn in_xs(e: &XmlElement, within: &str) -> Result<(), XdtoParseError> {
    if e.ns == XS_NS {
        Ok(())
    } else {
        perr(format!(
            "XSD: element <{}> of namespace `{}` in <{within}> is not supported",
            e.local, e.ns
        ))
    }
}

fn qattr(e: &XmlElement, attr: &str, scope: &Scope) -> Result<Option<QName>, XdtoParseError> {
    e.attr(attr)
        .map(|v| scope.qname(v, &format!("<{}> {attr}", e.local)))
        .transpose()
}

fn form_default(e: &XmlElement, attr: &str) -> Result<bool, XdtoParseError> {
    match e.attr(attr).map(str::trim) {
        None | Some("unqualified") => Ok(false),
        Some("qualified") => Ok(true),
        Some(o) => perr(format!("XSD: {attr}=`{o}` is not supported")),
    }
}

pub fn parse_xsd(root: &XmlElement) -> Result<Package, XdtoParseError> {
    parse_xsd_with(root, &[])
}

pub fn parse_xsd_with(root: &XmlElement, imported: &[&Package]) -> Result<Package, XdtoParseError> {
    if root.ns != XS_NS || root.local != "schema" {
        return perr(format!(
            "XSD: the root is <{}> of `{}`, expected <schema>",
            root.local, root.ns
        ));
    }
    check_attrs(
        root,
        &[
            "targetNamespace",
            "elementFormDefault",
            "attributeFormDefault",
            "version",
            "id",
        ],
    )?;
    let mut ctx = Ctx {
        anon: Default::default(),
        pkg: Package {
            ns: root.attr("targetNamespace").unwrap_or("").to_string(),
            ..Package::default()
        },
        el_q: form_default(root, "elementFormDefault")?,
        at_q: form_default(root, "attributeFormDefault")?,
        budget: Budget::default(),
        refs: Vec::new(),
    };
    ctx.pkg.element_qualified = ctx.el_q;
    ctx.pkg.attribute_qualified = ctx.at_q;
    let mut scope = Scope::default();
    scope.push(root);
    for c in root.elements() {
        in_xs(c, "schema")?;
        scope.push(c);
        let r = match c.local.as_str() {
            "annotation" => Ok(()),
            "import" => check_attrs(c, &["namespace", "schemaLocation", "id"]),
            "simpleType" => {
                check_attrs(c, &["name", "id", "final"])?;
                let Some(name) = c.attr("name") else {
                    return perr("XSD: a global <simpleType> without name");
                };
                let vt = simple_type(c, name, &mut scope, &mut ctx, 0)?;
                ctx.pkg.types.push(TypeDef::Value(vt));
                Ok(())
            }
            "complexType" => {
                let Some(name) = c.attr("name") else {
                    return perr("XSD: a global <complexType> without name");
                };
                let ot = complex_type(c, name, &mut scope, &mut ctx, 0)?;
                ctx.pkg.types.push(TypeDef::Object(ot));
                Ok(())
            }
            "element" => global_element(c, &mut scope, &mut ctx),
            _ => unsupported(c, "schema"),
        };
        scope.pop();
        r?;
    }
    scope.pop();
    let refs = std::mem::take(&mut ctx.refs);
    for (prop, ty, target) in refs {
        let found = if target.ns == ctx.pkg.ns {
            ctx.pkg
                .globals
                .iter()
                .find(|g| g.name == target.name)
                .cloned()
        } else {
            imported
                .iter()
                .find(|p| p.ns == target.ns)
                .and_then(|p| p.globals.iter().find(|g| g.name == target.name))
                .cloned()
        };
        let Some(g) = found else {
            return perr(format!(
                "XSD: the element reference {} does not name a global element of this schema or of an imported one",
                target.clark()
            ));
        };
        if let Some(TypeDef::Object(o)) = ctx.pkg.types.get_mut(ty)
            && let Some(p) = prop.and_then(|i| o.properties.get_mut(i))
        {
            p.ty = g.ty;
            p.nillable |= g.nillable;
        }
    }
    Ok(ctx.pkg)
}

fn global_element(e: &XmlElement, scope: &mut Scope, ctx: &mut Ctx) -> Result<(), XdtoParseError> {
    check_attrs(e, &["name", "type", "nillable", "id", "default", "fixed"])?;
    let Some(name) = e.attr("name") else {
        return perr("XSD: a global <element> without name");
    };
    let mut p = element_property(e, name, scope, ctx, "", 0)?;
    p.lower = 1;
    p.upper = 1;
    p.qualified = true;
    ctx.pkg.globals.push(p);
    Ok(())
}

fn simple_type(
    e: &XmlElement,
    name: &str,
    scope: &mut Scope,
    ctx: &mut Ctx,
    depth: usize,
) -> Result<ValueType, XdtoParseError> {
    if depth > MAX_NESTING {
        return perr("XSD: simple type nesting is too deep");
    }
    let mut def: Option<ValueType> = None;
    for c in e.elements() {
        in_xs(c, "simpleType")?;
        scope.push(c);
        let r = match c.local.as_str() {
            "annotation" => Ok(()),
            "restriction" | "list" | "union" if def.is_some() => {
                perr(format!("XSD: simpleType {name} has two derivations"))
            }
            "restriction" => {
                def = Some(restriction(c, name, scope, ctx, depth)?);
                Ok(())
            }
            "list" => {
                def = Some(list(c, name, scope, ctx, depth)?);
                Ok(())
            }
            "union" => {
                def = Some(union(c, name, scope, ctx, depth)?);
                Ok(())
            }
            _ => unsupported(c, "simpleType"),
        };
        scope.pop();
        r?;
    }
    def.map_or_else(
        || perr(format!("XSD: simpleType {name} has no derivation")),
        Ok,
    )
}

fn anon_simple(
    c: &XmlElement,
    name: &str,
    scope: &mut Scope,
    ctx: &mut Ctx,
    depth: usize,
) -> Result<QName, XdtoParseError> {
    check_attrs(c, &["id"])?;
    let vt = simple_type(c, name, scope, ctx, depth + 1)?;
    let q = vt.name.clone();
    ctx.pkg.anonymous_types.push(ctx.pkg.types.len());
    ctx.pkg.types.push(TypeDef::Value(vt));
    Ok(q)
}

fn restriction(
    e: &XmlElement,
    name: &str,
    scope: &mut Scope,
    ctx: &mut Ctx,
    depth: usize,
) -> Result<ValueType, XdtoParseError> {
    check_attrs(e, &["base", "id"])?;
    let mut base = qattr(e, "base", scope)?;
    let mut facets = Facets::default();
    for c in e.elements() {
        in_xs(c, "restriction")?;
        scope.push(c);
        let r = if c.local == "annotation" {
            Ok(())
        } else if c.local == "simpleType" {
            if base.is_some() {
                perr(format!(
                    "XSD: simpleType {name}: base and a nested simpleType"
                ))
            } else {
                anon_simple(c, &format!("{name}#base"), scope, ctx, depth).map(|q| base = Some(q))
            }
        } else if is_facet(&c.local) {
            check_attrs(c, &["value", "fixed", "id"]).and_then(|()| {
                let Some(v) = c.attr("value") else {
                    return perr(format!("XSD: facet {} without value", c.local));
                };
                if let Some(f) = c.attr("fixed") {
                    parse_bool("fixed", f)?;
                }
                let v = if c.local == "pattern" || c.local == "enumeration" {
                    v
                } else {
                    v.trim()
                };
                apply_facet(&mut facets, &c.local, v, &mut ctx.budget).map_err(|mut e| {
                    if let Some(Fault::Pattern { ty, .. }) = &mut e.fault {
                        *ty = QName::new(&ctx.pkg.ns, name);
                    }
                    e
                })
            })
        } else {
            unsupported(c, "restriction")
        };
        scope.pop();
        r?;
    }
    if base.is_none() {
        return perr(format!("XSD: restriction in {name} has no base"));
    }
    Ok(ValueType {
        name: QName::new(&ctx.pkg.ns, name),
        base,
        variety: Variety::Atomic,
        facets,
    })
}

fn list(
    e: &XmlElement,
    name: &str,
    scope: &mut Scope,
    ctx: &mut Ctx,
    depth: usize,
) -> Result<ValueType, XdtoParseError> {
    check_attrs(e, &["itemType", "id"])?;
    let mut item = qattr(e, "itemType", scope)?;
    for c in e.elements() {
        in_xs(c, "list")?;
        scope.push(c);
        let r = match c.local.as_str() {
            "annotation" => Ok(()),
            "simpleType" if item.is_none() => {
                anon_simple(c, &format!("{name}#item"), scope, ctx, depth).map(|q| item = Some(q))
            }
            _ => unsupported(c, "list"),
        };
        scope.pop();
        r?;
    }
    let Some(item) = item else {
        return perr(format!("XSD: list {name} has no item type"));
    };
    Ok(ValueType {
        name: QName::new(&ctx.pkg.ns, name),
        base: None,
        variety: Variety::List { item },
        facets: Facets::default(),
    })
}

fn union(
    e: &XmlElement,
    name: &str,
    scope: &mut Scope,
    ctx: &mut Ctx,
    depth: usize,
) -> Result<ValueType, XdtoParseError> {
    check_attrs(e, &["memberTypes", "id"])?;
    let mut members = Vec::new();
    if let Some(v) = e.attr("memberTypes") {
        for part in v.split_whitespace() {
            members.push(scope.qname(part, "memberTypes")?);
        }
    }
    let mut n = 0;
    for c in e.elements() {
        in_xs(c, "union")?;
        scope.push(c);
        let r = match c.local.as_str() {
            "annotation" => Ok(()),
            "simpleType" => {
                n += 1;
                anon_simple(c, &format!("{name}#{n}"), scope, ctx, depth).map(|q| members.push(q))
            }
            _ => unsupported(c, "union"),
        };
        scope.pop();
        r?;
    }
    let members = dedupe(members);
    if members.is_empty() {
        return perr(format!("XSD: union {name} has no member types"));
    }
    Ok(ValueType {
        name: QName::new(&ctx.pkg.ns, name),
        base: None,
        variety: Variety::Union { members },
        facets: Facets::default(),
    })
}

fn occurs(e: &XmlElement, attr: &str, default: i64) -> Result<i64, XdtoParseError> {
    match e.attr(attr).map(str::trim) {
        None => Ok(default),
        Some("unbounded") if attr == "maxOccurs" => Ok(-1),
        Some(v) => v
            .parse::<u32>()
            .map(i64::from)
            .or_else(|_| perr(format!("XSD: {attr}=`{v}` is not valid"))),
    }
}

fn element_property(
    e: &XmlElement,
    name: &str,
    scope: &mut Scope,
    ctx: &mut Ctx,
    owner: &str,
    depth: usize,
) -> Result<Property, XdtoParseError> {
    let mut ty = qattr(e, "type", scope)?;
    let mut inline = 0;
    for c in e.elements() {
        in_xs(c, "element")?;
        match c.local.as_str() {
            "annotation" => {}
            "simpleType" | "complexType" => {
                inline += 1;
                if inline > 1 || ty.is_some() {
                    return perr(format!("XSD: element {name} has more than one type"));
                }
                scope.push(c);
                let an = unique_anon(&mut ctx.anon, format!("{owner}#{name}"));
                let def = if c.local == "simpleType" {
                    check_attrs(c, &["id"])?;
                    TypeDef::Value(simple_type(c, &an, scope, ctx, depth + 1)?)
                } else {
                    TypeDef::Object(complex_type(c, &an, scope, ctx, depth + 1)?)
                };
                scope.pop();
                ty = Some(def.name().clone());
                ctx.pkg.anonymous_types.push(ctx.pkg.types.len());
                ctx.pkg.types.push(def);
            }
            _ => return unsupported(c, "element"),
        }
    }
    let lower = occurs(e, "minOccurs", 1)?;
    let upper = occurs(e, "maxOccurs", 1)?;
    if upper != -1 && upper < lower {
        return perr(format!(
            "XSD: element {name}: maxOccurs is less than minOccurs"
        ));
    }
    let qualified = match e.attr("form").map(str::trim) {
        None => ctx.el_q,
        Some("qualified") => true,
        Some("unqualified") => false,
        Some(o) => return perr(format!("XSD: element {name}: form=`{o}` is not supported")),
    };
    Ok(Property {
        name: name.to_string(),
        ty: ty.unwrap_or_else(|| QName::new(XS_NS, "anyType")),
        lower: lower as u32,
        upper,
        nillable: match e.attr("nillable") {
            Some(v) => parse_bool("nillable", v)?,
            None => false,
        },
        form: Form::Element,
        qualified,
        default: e.attr("default").or(e.attr("fixed")).map(str::to_string),
        local_name: None,
        fixed: e.attr("fixed").map(str::to_string),
    })
}

fn attribute_property(
    e: &XmlElement,
    scope: &mut Scope,
    ctx: &mut Ctx,
    owner: &str,
    depth: usize,
) -> Result<Property, XdtoParseError> {
    check_attrs(
        e,
        &["name", "type", "use", "default", "fixed", "form", "id"],
    )?;
    let Some(name) = e.attr("name") else {
        return perr(format!(
            "XSD: <attribute> in {owner} without name (ref is not supported)"
        ));
    };
    let mut ty = qattr(e, "type", scope)?;
    for c in e.elements() {
        in_xs(c, "attribute")?;
        match c.local.as_str() {
            "annotation" => {}
            "simpleType" if ty.is_none() => {
                scope.push(c);
                let q = {
                    let an = unique_anon(&mut ctx.anon, format!("{owner}#{name}"));
                    anon_simple(c, &an, scope, ctx, depth)
                };
                scope.pop();
                ty = Some(q?);
            }
            _ => return unsupported(c, "attribute"),
        }
    }
    let lower = match e.attr("use").map(str::trim) {
        None | Some("optional") => 0,
        Some("required") => 1,
        Some(o) => return perr(format!("XSD: attribute {name}: use=`{o}` is not supported")),
    };
    let qualified = match e.attr("form").map(str::trim) {
        None => ctx.at_q,
        Some("qualified") => true,
        Some("unqualified") => false,
        Some(o) => {
            return perr(format!(
                "XSD: attribute {name}: form=`{o}` is not supported"
            ));
        }
    };
    Ok(Property {
        name: name.to_string(),
        ty: ty.unwrap_or_else(|| QName::new(XS_NS, "anySimpleType")),
        lower,
        upper: 1,
        nillable: false,
        form: Form::Attribute,
        qualified,
        default: e.attr("default").or(e.attr("fixed")).map(str::to_string),
        local_name: None,
        fixed: e.attr("fixed").map(str::to_string),
    })
}

fn complex_type(
    e: &XmlElement,
    name: &str,
    scope: &mut Scope,
    ctx: &mut Ctx,
    depth: usize,
) -> Result<ObjectType, XdtoParseError> {
    if depth > MAX_NESTING {
        return perr("XSD: complex type nesting is too deep");
    }
    check_attrs(e, &["name", "abstract", "mixed", "id"])?;
    let mut base = None;
    let mut props: Vec<Property> = Vec::new();
    let mut refs: Vec<(usize, QName)> = Vec::new();
    let mut seen_content = false;
    let mut mixed: Option<bool> = None;
    for c in e.elements() {
        in_xs(c, "complexType")?;
        scope.push(c);
        let r: Result<(), XdtoParseError> = (|| {
            match c.local.as_str() {
                "annotation" => {}
                "sequence" | "choice" | "all" => {
                    if seen_content {
                        return perr(format!("XSD: complexType {name} has two content models"));
                    }
                    seen_content = true;
                    group(c, name, scope, ctx, depth, &mut props, &mut refs)?;
                }
                "attribute" => {
                    let p = attribute_property(c, scope, ctx, name, depth)?;
                    props.push(p);
                }
                "simpleContent" | "complexContent" => {
                    if seen_content {
                        return perr(format!("XSD: complexType {name} has two content models"));
                    }
                    seen_content = true;
                    content(
                        c, name, scope, ctx, depth, &mut base, &mut mixed, &mut props, &mut refs,
                    )?;
                }
                _ => return unsupported(c, "complexType"),
            }
            Ok(())
        })();
        scope.pop();
        r?;
    }
    finish_object(e, name, base, mixed, props, refs, ctx)
}

fn finish_object(
    e: &XmlElement,
    name: &str,
    base: Option<QName>,
    content_mixed: Option<bool>,
    props: Vec<Property>,
    refs: Vec<(usize, QName)>,
    ctx: &mut Ctx,
) -> Result<ObjectType, XdtoParseError> {
    let mut order: Vec<usize> = (0..props.len()).collect();
    order.sort_by_key(|&i| usize::from(props[i].form != Form::Attribute));
    let mut remap = vec![0usize; props.len()];
    for (new, &old) in order.iter().enumerate() {
        remap[old] = new;
    }
    let mut slots: Vec<Option<Property>> = props.into_iter().map(Some).collect();
    let properties: Vec<Property> = order.iter().filter_map(|&i| slots[i].take()).collect();
    let ty_index = ctx.pkg.types.len();
    for (old, target) in refs {
        ctx.refs.push((Some(remap[old]), ty_index, target));
    }
    let flag = |a: &str| e.attr(a).map(|v| parse_bool(a, v)).transpose();
    Ok(ObjectType {
        name: QName::new(&ctx.pkg.ns, name),
        base,
        open: false,
        abstract_: flag("abstract")?.unwrap_or(false),
        mixed: content_mixed.map_or_else(|| flag("mixed").map(|f| f.unwrap_or(false)), Ok)?,
        sequenced: false,
        ordered: !unordered_content(e),
        properties,
    })
}

fn group(
    g: &XmlElement,
    owner: &str,
    scope: &mut Scope,
    ctx: &mut Ctx,
    depth: usize,
    props: &mut Vec<Property>,
    refs: &mut Vec<(usize, QName)>,
) -> Result<(), XdtoParseError> {
    check_attrs(g, &["minOccurs", "maxOccurs", "id"])?;
    if occurs(g, "minOccurs", 1)? != 1 || occurs(g, "maxOccurs", 1)? != 1 {
        return perr(format!(
            "XSD: <{}> with minOccurs/maxOccurs other than 1 in {owner} is not supported",
            g.local
        ));
    }
    let choice = g.local == "choice";
    for c in g.elements() {
        in_xs(c, &g.local)?;
        scope.push(c);
        let r: Result<(), XdtoParseError> = (|| {
            match c.local.as_str() {
                "annotation" => {}
                "element" => {
                    check_attrs(
                        c,
                        &[
                            "name",
                            "type",
                            "ref",
                            "minOccurs",
                            "maxOccurs",
                            "nillable",
                            "form",
                            "default",
                            "fixed",
                            "id",
                        ],
                    )?;
                    let target = qattr(c, "ref", scope)?;
                    if target.is_some() && (c.attr("name").is_some() || c.attr("type").is_some()) {
                        return perr(format!(
                            "XSD: <element> in {owner} has `ref` together with `name` or `type`"
                        ));
                    }
                    let mut p = match &target {
                        Some(t) => {
                            let mut p = element_property(c, &t.name, scope, ctx, owner, depth)?;
                            p.qualified = true;
                            refs.push((props.len(), t.clone()));
                            p
                        }
                        None => {
                            let Some(n) = c.attr("name") else {
                                return perr(format!("XSD: <element> in {owner} has no name"));
                            };
                            element_property(c, n, scope, ctx, owner, depth)?
                        }
                    };
                    if choice {
                        p.lower = 0;
                    }
                    props.push(p);
                }
                _ => return unsupported(c, &g.local),
            }
            Ok(())
        })();
        scope.pop();
        r?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn content(
    c: &XmlElement,
    owner: &str,
    scope: &mut Scope,
    ctx: &mut Ctx,
    depth: usize,
    base: &mut Option<QName>,
    mixed: &mut Option<bool>,
    props: &mut Vec<Property>,
    refs: &mut Vec<(usize, QName)>,
) -> Result<(), XdtoParseError> {
    let simple = c.local == "simpleContent";
    check_attrs(c, if simple { &["id"] } else { &["id", "mixed"] })?;
    if let Some(v) = c.attr("mixed") {
        *mixed = Some(parse_bool("mixed", v)?);
    }
    for d in c.elements() {
        in_xs(d, &c.local)?;
        scope.push(d);
        let r: Result<(), XdtoParseError> = (|| {
            match d.local.as_str() {
                "annotation" => {}
                "extension" => {
                    check_attrs(d, &["base", "id"])?;
                    let Some(b) = qattr(d, "base", scope)? else {
                        return perr(format!("XSD: extension in {owner} has no base"));
                    };
                    for x in d.elements() {
                        in_xs(x, "extension")?;
                        scope.push(x);
                        let r: Result<(), XdtoParseError> = (|| {
                            match x.local.as_str() {
                                "annotation" => {}
                                "attribute" => {
                                    props.push(attribute_property(x, scope, ctx, owner, depth)?)
                                }
                                "sequence" | "choice" | "all" if !simple => {
                                    group(x, owner, scope, ctx, depth, props, refs)?
                                }
                                _ => return unsupported(x, "extension"),
                            }
                            Ok(())
                        })();
                        scope.pop();
                        r?;
                    }
                    if simple {
                        props.push(Property {
                            name: "__content".into(),
                            ty: b,
                            lower: 1,
                            upper: 1,
                            nillable: false,
                            form: Form::Text,
                            qualified: false,
                            default: None,
                            local_name: None,
                            fixed: None,
                        });
                    } else {
                        *base = Some(b);
                    }
                }
                _ => return unsupported(d, &c.local),
            }
            Ok(())
        })();
        scope.pop();
        r?;
    }
    Ok(())
}

fn unordered_content(e: &XmlElement) -> bool {
    let is = |x: &XmlElement| x.ns == XS_NS && matches!(x.local.as_str(), "all" | "choice");
    e.elements().any(|c| {
        is(c)
            || (c.ns == XS_NS
                && c.local == "complexContent"
                && c.elements().any(|x| x.elements().any(is)))
    })
}
