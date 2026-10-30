use crate::{ChildSpec, Codec, FactMap, KindSpec, PropSpec, Schema};

const FORM_CLASS: &str = "a2cb086c-db98-43e4-a1a9-0760ab048f8d";
pub const ACCOUNTING_COMMAND_CLASS: &str = "7162da60-f7fe-4d78-ad5d-e31700f9af18";
pub const TEMPLATE_CLASS: &str = "3daea016-69b7-4ed4-9453-127911372fe6";

fn spec(path: &[usize], codec: Codec) -> PropSpec {
    PropSpec {
        path: Some(path.to_vec()),
        codec,
        ..PropSpec::default()
    }
}

fn put(props: &mut Vec<(String, PropSpec)>, name: &str, p: PropSpec) {
    match props.iter_mut().find(|(n, _)| n == name) {
        Some(slot) => slot.1 = p,
        None => props.push((name.to_string(), p)),
    }
}

fn child_mut<'a>(k: &'a mut KindSpec, tag: &str) -> Option<&'a mut ChildSpec> {
    k.children
        .iter_mut()
        .find(|(_, c)| c.tag == tag)
        .map(|(_, c)| c)
}

fn add_sub(k: &mut KindSpec, tag: &str, class: &str) {
    if !k.subobjects.iter().any(|(t, c)| t == tag && c == class) {
        k.subobjects.push((tag.to_string(), class.to_string()));
    }
}

fn calculation_register(mut k: KindSpec) -> KindSpec {
    put(&mut k.props, "ActionPeriod", spec(&[1, 17], Codec::Bool));
    put(&mut k.props, "Schedule", spec(&[1, 19], Codec::Ref));
    put(&mut k.props, "ScheduleValue", spec(&[1, 20], Codec::Ref));
    put(&mut k.props, "ScheduleDate", spec(&[1, 21], Codec::Ref));
    put(
        &mut k.props,
        "ChartOfCalculationTypes",
        spec(&[1, 22], Codec::Ref),
    );
    for (tag, at) in [("Dimension", 4usize), ("Attribute", 2)] {
        if let Some(c) = child_mut(&mut k, tag) {
            put(&mut c.props, "ScheduleLink", spec(&[0, at], Codec::Ref));
        }
    }
    put(
        &mut k.props,
        "StandardAttributes",
        spec(&[1, 28], Codec::StdAttrs),
    );
    add_sub(&mut k, "Form", FORM_CLASS);
    k
}

fn sequence(mut k: KindSpec) -> KindSpec {
    put(&mut k.props, "Documents", spec(&[1, 8], Codec::MdList));
    put(
        &mut k.props,
        "RegisterRecords",
        spec(&[1, 9], Codec::MdList),
    );
    if let Some(c) = child_mut(&mut k, "Dimension") {
        put(&mut c.props, "Type", spec(&[0, 1, 2], Codec::Type));
        put(&mut c.props, "DocumentMap", spec(&[0, 2], Codec::MdList));
        put(
            &mut c.props,
            "RegisterRecordsMap",
            spec(&[0, 3], Codec::MdList),
        );
    }
    k
}

fn calculation_types(mut k: KindSpec) -> KindSpec {
    let mut dep = spec(&[1, 27], Codec::Enum);
    dep.map = [
        ("0", "DontUse"),
        ("1", "OnActionPeriod"),
        ("2", "OnRegistrationPeriod"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v.to_owned()))
    .collect();
    put(&mut k.props, "DependenceOnCalculationTypes", dep);
    put(
        &mut k.props,
        "BaseCalculationTypes",
        spec(&[1, 28], Codec::MdList),
    );
    put(&mut k.props, "BasedOn", spec(&[1, 36], Codec::MdList));
    put(&mut k.props, STS, spec(&[1, 44], Codec::Unknown));
    k
}

fn chart_of_accounts(mut k: KindSpec) -> KindSpec {
    put(&mut k.props, STS, spec(&[1, 39], Codec::Unknown));
    k
}

fn accounting_register(mut k: KindSpec) -> KindSpec {
    for (tag, props) in [
        ("Dimension", &[("AccountingFlag", 3usize)][..]),
        (
            "Resource",
            &[("AccountingFlag", 3), ("ExtDimensionAccountingFlag", 4)][..],
        ),
    ] {
        if let Some(c) = child_mut(&mut k, tag) {
            for &(name, at) in props {
                if !c.props.iter().any(|(n, _)| n == name) {
                    put(&mut c.props, name, spec(&[0, at], Codec::Ref));
                }
            }
        }
    }
    if !k.children.iter().any(|(_, c)| c.tag == "Command") {
        let cmd = ChildSpec {
            tag: "Command".into(),
            uuid: Some(vec![0, 1, 2, 9, 1, 2]),
            header: Some(vec![0, 1, 2, 9]),
            props: Vec::new(),
            internal_info: Vec::new(),
            children: Vec::new(),
        };
        k.children.push((ACCOUNTING_COMMAND_CLASS.to_string(), cmd));
    }
    k
}

fn task(mut k: KindSpec) -> KindSpec {
    if let Some(c) = child_mut(&mut k, "AddressingAttribute")
        && !c.props.iter().any(|(n, _)| n == "AddressingDimension")
    {
        put(
            &mut c.props,
            "AddressingDimension",
            spec(&[0, 3], Codec::Ref),
        );
    }
    k
}

pub const STS: &str = "StandardTabularSections";

type Patch = fn(KindSpec) -> KindSpec;

pub(crate) fn build_legacy(s: &Schema) -> FactMap<String, KindSpec> {
    let mut m = FactMap::new();
    let table: [(&str, Patch); 6] = [
        ("Task", task),
        ("AccountingRegister", accounting_register),
        ("CalculationRegister", calculation_register),
        ("Sequence", sequence),
        ("ChartOfCalculationTypes", calculation_types),
        ("ChartOfAccounts", chart_of_accounts),
    ];
    for (name, f) in table {
        if let Some(k) = s.kind(name) {
            m.insert(name.to_string(), f(k.clone()));
        }
    }
    m
}

fn kinds(s: &'static Schema) -> &'static FactMap<String, KindSpec> {
    let resource = s.resource();
    let result: Result<_, std::convert::Infallible> = resource.learned.legacy_plain(|| {
        #[cfg(any(test, feature = "testing"))]
        resource
            .learned_legacy_builds
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(build_legacy(s))
    });
    match result {
        Ok(value) => value,
        Err(never) => match never {},
    }
}

pub fn kind(s: &'static Schema, kind: &str) -> Option<&'static KindSpec> {
    kinds(s).get(kind).or_else(|| s.kind(kind))
}
