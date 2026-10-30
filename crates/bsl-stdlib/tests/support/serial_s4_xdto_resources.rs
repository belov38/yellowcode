#![cfg(all(test, feature = "collate"))]

use super::{XdtoCtx, XdtoData, core, data::Factory};
use crate::collate_test_support as support;
use crate::typedesc::{TypeDescription, new_type_description};
use bsl_vm::{Exception, ExecOptions, FatalKind, TypeId};
use std::{
    rc::Rc,
    sync::{Arc, atomic::AtomicBool},
};

fn context() -> XdtoCtx {
    XdtoCtx::new(
        Rc::new(Factory::new(Arc::new(
            bsl_lib::xdto::Model::new(Vec::new()),
        ))),
        2,
    )
}

fn options() -> [(ExecOptions, FatalKind); 3] {
    [
        (
            ExecOptions {
                budget: Some(64),
                ..Default::default()
            },
            FatalKind::Budget,
        ),
        (
            ExecOptions {
                memory_limit: Some(512),
                ..Default::default()
            },
            FatalKind::Memory,
        ),
        (
            ExecOptions {
                cancel: Some(Arc::new(AtomicBool::new(true))),
                ..Default::default()
            },
            FatalKind::Cancelled,
        ),
    ]
}

#[test]
fn xdto_s4_refuses_description_copies_before_publication() {
    let codec = crate::typedesc::xdto_codecs().remove(0).1;
    let runtime = support::runtime();
    let description = new_type_description(TypeDescription::new(
        &[TypeId::NUMBER, TypeId::STRING],
        None,
        None,
        None,
        None,
        &runtime,
    ));
    let input = XdtoData::Object {
        ty: core("TypeDescription"),
        props: vec![(
            "Type".into(),
            (0..10_000)
                .map(|_| XdtoData::Value {
                    projection: None,
                    ty: bsl_lib::xdto::QName::new(crate::xdto::XS, "QName"),
                    lexical: "{http://www.w3.org/2001/XMLSchema}decimal".into(),
                    value: bsl_vm::Value::Type(TypeId::NUMBER),
                })
                .collect(),
        )],
    };
    for (options, kind) in options() {
        for reading in [false, true] {
            let mut ctx = context();
            let mut ex = runtime.execution(support::env(), options.clone());
            let result = if reading {
                codec.from_xdto(&mut ex, &input, &mut ctx).map(|_| ())
            } else {
                codec
                    .to_xdto(&mut ex, description.as_object().unwrap(), &mut ctx)
                    .map(|_| ())
            };
            assert!(matches!(result, Err(Exception::Fatal(ref error)) if error.kind == kind));
            drop(result);
            ex.reserve(1).unwrap();
        }
    }
}

#[test]
fn xdto_s4_description_roundtrip_keeps_all_four_qualifiers() {
    use crate::typedesc::{
        AllowedLength, AllowedSign, BinaryQualifiers, DateParts, DateQualifiers, NumberQualifiers,
        StringQualifiers,
    };
    let codec = crate::typedesc::xdto_codecs().remove(0).1;
    let mut ex = support::execution();
    let expected = TypeDescription::new(
        &[
            TypeId::NUMBER,
            TypeId::STRING,
            TypeId::DATE,
            TypeId::of_catalog(crate::typedesc::idx("BinaryData")),
        ],
        Some(NumberQualifiers {
            digits: 10,
            fraction: 2,
            sign: AllowedSign::Nonnegative,
        }),
        Some(StringQualifiers {
            length: 20,
            allowed: AllowedLength::Fixed,
        }),
        Some(DateQualifiers {
            parts: DateParts::Time,
        }),
        Some(BinaryQualifiers {
            length: 8,
            allowed: AllowedLength::Fixed,
        }),
        ex.runtime(),
    );
    let original = new_type_description(expected.clone());
    let data = codec
        .to_xdto(&mut ex, original.as_object().unwrap(), &mut context())
        .unwrap();
    let decoded = codec.from_xdto(&mut ex, &data, &mut context()).unwrap();
    let actual = decoded
        .as_object()
        .unwrap()
        .as_any()
        .downcast_ref::<TypeDescription>()
        .unwrap();
    assert_eq!(actual, &expected);
}
