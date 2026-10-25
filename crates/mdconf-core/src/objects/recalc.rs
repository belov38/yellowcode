use super::codecs::{self, Ctx, Owner};
use super::decode::{
    Decoded, Descriptor, Head, PropertyIdentity, Provenance, SelectedSource, Visitor,
    description_value, emit, header_prop, miss,
};
use super::path::find_collection;
use crate::model::{
    Diagnostics, GeneratedType, MdObject, MdRef, Prop, RecalcDimension, Recalculation, Value,
};
use brace::{Guid, Node};
use mdschema::{Codec, PropSpec, Schema};

pub(crate) const RECALC_CLASS: &str = "274bf899-db0e-4df6-8ab5-67bf6371ec0b";
pub(crate) const RECALC_DIM_CLASS: &str = "3c456b74-4ea5-4b22-a957-e9fad9133b54";
pub(crate) const RECALC_HEADER: [usize; 3] = [1, 7, 1];
pub(crate) const DIM_HEADER: [usize; 2] = [0, 1];
pub(crate) const DIM_UUID: [usize; 4] = [0, 1, 1, 2];
pub(crate) const GENERATED: [(&str, [usize; 2], [usize; 2]); 3] = [
    ("Record", [1, 1], [1, 2]),
    ("Manager", [1, 3], [1, 4]),
    ("RecordSet", [1, 5], [1, 6]),
];
const LOCK_MODE: [usize; 2] = [1, 8];
const REGISTER_DIMENSION: [usize; 2] = [0, 2];
const LEADING_DATA: [usize; 2] = [0, 3];
const PROPS: &str = "RecalculationProperties";
const DIM_PROPS: &str = "RecalculationDimensionProperties";
const CHILD_OBJECTS: &str = "RecalculationChildObjects";

pub(crate) fn recalc_header(entry: &Node) -> Option<&[Node]> {
    entry.get(&RECALC_HEADER)?.as_list()
}

fn spec(path: [usize; 2], codec: Codec) -> PropSpec {
    PropSpec {
        path: Some(path.to_vec()),
        codec,
        ..Default::default()
    }
}

pub(crate) fn lock_mode_spec(s: &Schema) -> PropSpec {
    let map = s
        .kind("CalculationRegister")
        .and_then(|k| k.props.iter().find(|(n, _)| n == "DataLockControlMode"))
        .map(|(_, p)| p.map.clone())
        .unwrap_or_default();
    PropSpec {
        map,
        ..spec(LOCK_MODE, Codec::Enum)
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "Same recalculation decoder threads transient identity and optional visitor"
)]
fn decode_props(
    ctx: &Ctx,
    node: &Node,
    hdr: &[usize],
    specs: &[(&str, PropSpec)],
    (owner, props_type): (&Owner, &str),
    diag: &mut Diagnostics,
    identity: PropertyIdentity,
    tag: &str,
    visitor: &mut Option<&mut Visitor<'_>>,
) -> Decoded {
    let mut d = Decoded::default();
    for h in [Head::Name, Head::Synonym, Head::Comment] {
        if let Some((path, raw, provenance)) =
            header_prop(ctx, node, hdr, h, props_type, &mut d, diag)
            && let Some(prop) = d.props.last()
        {
            emit(
                visitor,
                ctx,
                identity,
                tag,
                Some(owner),
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
    }
    for (name, sp) in specs {
        let raw = sp.path.as_deref().and_then(|p| node.get(p));
        let value = raw.and_then(|v| codecs::decode(ctx, name, sp, v, Some(owner), None));
        match value {
            Some(value) => {
                let prop = Prop {
                    name: (*name).into(),
                    codec: sp.codec,
                    value: description_value(value),
                };
                emit(
                    visitor,
                    ctx,
                    identity,
                    tag,
                    Some(owner),
                    &prop,
                    SelectedSource {
                        descriptor: Descriptor::Spec(sp),
                        root: node,
                        raw,
                        path: sp.path.as_deref(),
                        path2: None,
                        provenance: Provenance::Selected,
                    },
                    false,
                    false,
                );
                d.props.push(prop);
            }
            None => miss(diag, format!("fail:{props_type}.{name}")),
        }
    }
    d
}

fn find<'a>(props: &'a [Prop], name: &str) -> Option<&'a Value> {
    props.iter().find(|p| p.name == name).map(|p| &p.value)
}

pub(crate) fn dimension_specs() -> [(&'static str, PropSpec); 2] {
    [
        ("RegisterDimension", spec(REGISTER_DIMENSION, Codec::Ref)),
        ("LeadingRegisterData", spec(LEADING_DATA, Codec::MdList)),
    ]
}

fn dimension(
    ctx: &Ctx,
    item: &Node,
    owner: &Owner,
    diag: &mut Diagnostics,
    entry: Guid,
    visitor: &mut Option<&mut Visitor<'_>>,
) -> Option<RecalcDimension> {
    item.get(&DIM_HEADER)?.as_list()?;
    let specs = dimension_specs();
    let id = item
        .get(&DIM_UUID)
        .and_then(Node::as_guid)
        .unwrap_or_default();
    let d = decode_props(
        ctx,
        item,
        &DIM_HEADER,
        &specs,
        (owner, DIM_PROPS),
        diag,
        PropertyIdentity {
            id,
            entry,
            ..Default::default()
        },
        "Dimension",
        visitor,
    );
    let register_dimension = match find(&d.props, "RegisterDimension") {
        Some(Value::Ref(r)) => r.clone(),
        _ => MdRef::default(),
    };
    let leading_register_data = match find(&d.props, "LeadingRegisterData") {
        Some(Value::RefList(v)) => v.clone(),
        _ => Vec::new(),
    };
    Some(RecalcDimension {
        id,
        name: d.name,
        synonym: d.synonym,
        comment: d.comment,
        register_dimension,
        leading_register_data,
        props: d.props,
    })
}

pub(crate) fn decode_recalculation(
    ctx: &Ctx,
    register: &MdObject,
    id: Guid,
    entry: &Node,
    diag: &mut Diagnostics,
) -> Option<Recalculation> {
    decode_recalculation_observed(ctx, register, id, entry, diag, &mut None)
}

pub(super) fn decode_recalculation_observed(
    ctx: &Ctx,
    register: &MdObject,
    id: Guid,
    entry: &Node,
    diag: &mut Diagnostics,
    visitor: &mut Option<&mut Visitor<'_>>,
) -> Option<Recalculation> {
    recalc_header(entry)?;
    let owner = Owner {
        kind: register.kind.as_str().into(),
        name: register.name.clone(),
    };
    let generated_types = GENERATED
        .iter()
        .filter_map(|(cat, t, v)| {
            Some(GeneratedType {
                category: (*cat).into(),
                type_id: entry.get(t)?.as_guid()?,
                value_id: entry.get(v)?.as_guid()?,
            })
        })
        .collect();
    let specs = [("DataLockControlMode", lock_mode_spec(ctx.schema))];
    let d = decode_props(
        ctx,
        entry,
        &RECALC_HEADER,
        &specs,
        (&owner, PROPS),
        diag,
        PropertyIdentity {
            id,
            entry: id,
            ..Default::default()
        },
        "Recalculation",
        visitor,
    );
    let mut dimensions = Vec::new();
    for it in find_collection(entry, RECALC_DIM_CLASS).unwrap_or_default() {
        match dimension(ctx, it, &owner, diag, id, visitor) {
            Some(x) => dimensions.push(x),
            None => miss(diag, format!("unknown:{CHILD_OBJECTS}.Dimension")),
        }
    }
    Some(Recalculation {
        id,
        name: d.name,
        synonym: d.synonym,
        comment: d.comment,
        generated_types,
        props: d.props,
        dimensions,
        module: None,
    })
}
