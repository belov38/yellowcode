use brace::Guid;
use md_model::parts::{ContentItem, Predefined, PredefinedCode, PredefinedItem};
use md_model::{
    Child, ChildTag, Configuration, DateFractions, GeneratedType, LocalString, MdObject, MdRef,
    NumberQualifiers, Prop, ScriptVariant, StringQualifiers, TypeDescription, TypeRef, Value,
};
use mdschema::MdKind;

pub fn g(n: u128) -> Guid {
    Guid::from_bytes(n.to_be_bytes())
}

pub fn p(name: &str, v: Value) -> Prop {
    Prop {
        name: name.into(),
        codec: mdschema::Codec::Unknown,
        value: v,
    }
}

pub fn text(s: &str) -> Value {
    Value::Text(s.into())
}

pub fn td(qnames: &[&str]) -> TypeDescription {
    TypeDescription {
        types: qnames
            .iter()
            .map(|q| TypeRef {
                name: md_model::XmlTypeName::from_qname(q).unwrap(),
                set: q.starts_with("cfg:DefinedType.")
                    || q.starts_with("cfg:Characteristic.")
                    || (q.starts_with("cfg:") && !q.contains('.')),
                type_id: None,
            })
            .collect(),
        ..Default::default()
    }
}

pub fn num(d: u32, f: u32, nonneg: bool) -> Value {
    let mut t = td(&["xs:decimal"]);
    t.number = Some(NumberQualifiers {
        digits: d,
        fraction: f,
        nonnegative: nonneg,
    });
    Value::Type(t)
}

pub fn string(len: u32, fixed: bool) -> Value {
    let mut t = td(&["xs:string"]);
    t.string = Some(StringQualifiers { length: len, fixed });
    Value::Type(t)
}

pub fn date(fr: DateFractions) -> Value {
    let mut t = td(&["xs:dateTime"]);
    t.date = Some(fr);
    Value::Type(t)
}

pub fn composite(
    qnames: &[&str],
    n: Option<(u32, u32, bool)>,
    s: Option<(u32, bool)>,
    d: Option<DateFractions>,
) -> Value {
    let mut t = td(qnames);
    t.number = n.map(|(digits, fraction, nonnegative)| NumberQualifiers {
        digits,
        fraction,
        nonnegative,
    });
    t.string = s.map(|(length, fixed)| StringQualifiers { length, fixed });
    t.date = d;
    Value::Type(t)
}

pub fn refs(names: &[&str]) -> Value {
    Value::RefList(
        names
            .iter()
            .map(|n| MdRef {
                id: None,
                name: (*n).into(),
            })
            .collect(),
    )
}

fn gen_type(category: &str, type_id: Guid) -> GeneratedType {
    GeneratedType {
        category: category.into(),
        type_id,
        value_id: Guid::NIL,
    }
}

const REF_KINDS: [MdKind; 9] = [
    MdKind::Catalog,
    MdKind::Document,
    MdKind::Enum,
    MdKind::ChartOfCharacteristicTypes,
    MdKind::ChartOfAccounts,
    MdKind::ChartOfCalculationTypes,
    MdKind::ExchangePlan,
    MdKind::BusinessProcess,
    MdKind::Task,
];

pub fn object(
    kind: MdKind,
    id: u128,
    name: &str,
    props: Vec<Prop>,
    children: Vec<Child>,
) -> MdObject {
    let mut generated_types = Vec::new();
    if REF_KINDS.contains(&kind) {
        generated_types.push(gen_type("Ref", g(id.wrapping_add(1))));
    }
    if kind == MdKind::BusinessProcess {
        generated_types.push(gen_type("RoutePointRef", g(id.wrapping_add(2))));
    }
    MdObject {
        kind,
        id: g(id),
        name: name.into(),
        synonym: LocalString::default(),
        comment: String::new(),
        generated_types,
        this_node: None,
        props,
        children,
        forms: vec![],
        templates: vec![],
        subsystems: vec![],
        recalculations: vec![],
        modules: vec![],
        parts: Default::default(),
        form: None,
        template: None,
        flowchart: None,
    }
}

pub fn with_ref(mut o: MdObject, type_id: u128) -> MdObject {
    for t in o.generated_types.iter_mut().filter(|t| t.category == "Ref") {
        t.type_id = g(type_id);
    }
    o
}

fn child(tag: ChildTag, id: u128, name: &str, props: Vec<Prop>, children: Vec<Child>) -> Child {
    Child {
        tag,
        id: g(id),
        name: name.into(),
        synonym: LocalString::default(),
        comment: String::new(),
        generated_types: vec![],
        props,
        children,
        module: None,
    }
}

pub fn attr(tag: ChildTag, id: u128, name: &str, ty: Value, extra: Vec<Prop>) -> Child {
    let mut props = vec![p("Type", ty)];
    props.extend(extra);
    child(tag, id, name, props, vec![])
}

pub fn tabular(id: u128, name: &str, attrs: Vec<Child>) -> Child {
    child(ChildTag::TabularSection, id, name, vec![], attrs)
}

pub fn enum_value(id: u128, name: &str) -> Child {
    child(ChildTag::EnumValue, id, name, vec![], vec![])
}

pub fn predefined(id: u128, name: &str, code: &str, description: &str) -> PredefinedItem {
    PredefinedItem {
        id: g(id),
        name: name.into(),
        code: PredefinedCode::Str(code.into()),
        description: description.into(),
        ..Default::default()
    }
}

#[allow(
    clippy::field_reassign_with_default,
    reason = "a struct literal would depend on the hidden codec fields of the model"
)]
pub fn config(script: ScriptVariant, objects: Vec<MdObject>) -> Configuration {
    let mut c = Configuration::default();
    c.id = g(0x0252debb_e455_515f_b2f9_08c62d66f25e);
    c.objects = objects;
    c.settings.script_variant = script;
    c
}

pub fn rtq_like() -> Configuration {
    let mut objects = Vec::new();
    let o = object(
        MdKind::Catalog,
        0x4cde9f80_3473_5604_80f2_41dcf8e098d5,
        "Samples",
        vec![
            p("Hierarchical", Value::Bool(false)),
            p("CodeLength", text("6")),
            p("CodeType", text("Number")),
            p("DescriptionLength", text("50")),
        ],
        vec![
            attr(
                ChildTag::Attribute,
                0xe3d81b8d_50d7_5e48_a0be_770003e89434,
                "K",
                num(5, 0, false),
                vec![p("Use", text("ForItem"))],
            ),
            attr(
                ChildTag::Attribute,
                0xe31eebd0_a78f_56db_87bd_02eee6f5ef2f,
                "S",
                string(20, false),
                vec![p("Use", text("ForItem"))],
            ),
            attr(
                ChildTag::Attribute,
                0x0315ee2a_37d6_59b1_9a0e_4b5b0674b666,
                "SF",
                string(10, true),
                vec![p("Use", text("ForItem"))],
            ),
            attr(
                ChildTag::Attribute,
                0xd687c954_7804_5126_a5e1_524224bb72ca,
                "SU",
                string(0, false),
                vec![p("Use", text("ForItem"))],
            ),
            attr(
                ChildTag::Attribute,
                0xd6476e11_70e3_533b_bdc9_19f1a38e55fb,
                "N",
                num(15, 3, false),
                vec![p("Use", text("ForItem"))],
            ),
            attr(
                ChildTag::Attribute,
                0x56354d07_cc32_519c_9068_f979118c0d8f,
                "NI",
                num(10, 0, false),
                vec![p("Use", text("ForItem"))],
            ),
            attr(
                ChildTag::Attribute,
                0xe73212fd_b31d_5424_b65f_17ea27c67453,
                "NU",
                num(10, 2, true),
                vec![p("Use", text("ForItem"))],
            ),
            attr(
                ChildTag::Attribute,
                0x02e497d0_8fbd_5f6e_80ef_2bc4b725fe55,
                "D",
                date(DateFractions::DateTime),
                vec![p("Use", text("ForItem"))],
            ),
            attr(
                ChildTag::Attribute,
                0x8a71d7ea_b4d1_5d68_bea5_35e627851e0b,
                "DD",
                date(DateFractions::Date),
                vec![p("Use", text("ForItem"))],
            ),
            attr(
                ChildTag::Attribute,
                0x5d534945_cb55_550d_8f75_d268d06b8294,
                "B",
                Value::Type(td(&["xs:boolean"])),
                vec![p("Use", text("ForItem"))],
            ),
            attr(
                ChildTag::Attribute,
                0x3f74007c_b264_576d_bc19_743dc485abbd,
                "G",
                Value::Type(td(&["cfg:CatalogRef.Goods"])),
                vec![p("Use", text("ForItem"))],
            ),
            attr(
                ChildTag::Attribute,
                0x7305034d_2229_54f2_bc3c_c7ae263a7c88,
                "V",
                composite(
                    &[
                        "xs:string",
                        "xs:decimal",
                        "xs:dateTime",
                        "xs:boolean",
                        "cfg:CatalogRef.Goods",
                        "cfg:CatalogRef.Partners",
                    ],
                    Some((15, 2, false)),
                    Some((20, false)),
                    Some(DateFractions::DateTime),
                ),
                vec![p("Use", text("ForItem"))],
            ),
        ],
    );
    objects.push(with_ref(o, 0xa1c0ea8e_385f_5bc1_9178_8aca6c6209fb));
    let o = object(
        MdKind::Catalog,
        0xd2bee9d7_1859_58a1_ad66_1e933e9b0522,
        "Goods",
        vec![
            p("Hierarchical", Value::Bool(true)),
            p("HierarchyType", text("HierarchyFoldersAndItems")),
            p("LimitLevelCount", Value::Bool(false)),
            p("CodeLength", text("8")),
            p("CodeType", text("Number")),
            p("DescriptionLength", text("100")),
        ],
        vec![
            attr(
                ChildTag::Attribute,
                0x81c35e8e_05a7_50cd_8ae3_594317b4d676,
                "Article",
                string(25, false),
                vec![p("Use", text("ForItem"))],
            ),
            attr(
                ChildTag::Attribute,
                0xd1451308_8572_5405_ac83_594dcd2bdb62,
                "Weight",
                num(15, 3, false),
                vec![p("Use", text("ForItem"))],
            ),
            attr(
                ChildTag::Attribute,
                0x6188f131_27e2_5e03_84e6_9fd1b0d251a0,
                "Note",
                string(0, false),
                vec![p("Use", text("ForItem"))],
            ),
            attr(
                ChildTag::Attribute,
                0x2bfc3c6d_84f8_5cba_882d_c64957d94b4b,
                "Unit",
                Value::Type(td(&["cfg:CatalogRef.Units"])),
                vec![p("Use", text("ForItem"))],
            ),
            attr(
                ChildTag::Attribute,
                0x7601e9d9_e996_5cbe_bd52_e71902b54ce8,
                "Color",
                Value::Type(td(&["cfg:EnumRef.Color"])),
                vec![p("Use", text("ForItem"))],
            ),
            attr(
                ChildTag::Attribute,
                0x02b5bcda_93b8_5e03_b295_1154246c37bc,
                "Created",
                date(DateFractions::DateTime),
                vec![p("Use", text("ForItem"))],
            ),
            attr(
                ChildTag::Attribute,
                0xfe412b18_cd99_59a7_b75c_0fce19a2419d,
                "Comp",
                composite(
                    &[
                        "xs:string",
                        "xs:decimal",
                        "xs:dateTime",
                        "xs:boolean",
                        "cfg:CatalogRef.Units",
                        "cfg:CatalogRef.Partners",
                    ],
                    Some((12, 2, false)),
                    Some((20, false)),
                    Some(DateFractions::DateTime),
                ),
                vec![p("Use", text("ForItem"))],
            ),
            attr(
                ChildTag::Attribute,
                0x6f739f09_99f2_5f5c_8391_a99f50dd5953,
                "CompRefs",
                composite(
                    &[
                        "cfg:CatalogRef.Units",
                        "cfg:CatalogRef.Partners",
                        "cfg:EnumRef.Color",
                    ],
                    None,
                    None,
                    None,
                ),
                vec![p("Use", text("ForItem"))],
            ),
            attr(
                ChildTag::Attribute,
                0x7a47e3a7_62f3_5bb8_8c6d_d946a3747926,
                "Supplier",
                Value::Type(td(&["cfg:CatalogRef.Partners"])),
                vec![p("Use", text("ForItem"))],
            ),
            attr(
                ChildTag::Attribute,
                0x005f4f42_e7c9_559a_81f0_fd943e197fc1,
                "FolderNote",
                string(30, false),
                vec![p("Use", text("ForFolder"))],
            ),
            tabular(
                0x7f336b85_e963_5bed_9877_48ed95e2a81d,
                "Barcodes",
                vec![
                    attr(
                        ChildTag::Attribute,
                        0xd5a26508_8a19_5c29_952f_327655818254,
                        "Barcode",
                        string(20, false),
                        vec![],
                    ),
                    attr(
                        ChildTag::Attribute,
                        0xabbcc1cc_8f66_56a1_840e_46f3863830c8,
                        "Unit",
                        Value::Type(td(&["cfg:CatalogRef.Units"])),
                        vec![],
                    ),
                    attr(
                        ChildTag::Attribute,
                        0x7b47f2ea_d023_51f3_ad65_fe43ca7ff22d,
                        "Qty",
                        num(10, 3, false),
                        vec![],
                    ),
                ],
            ),
        ],
    );
    objects.push(with_ref(o, 0x93b377c6_2d57_5225_b659_f049f8fc398d));
    let o = object(
        MdKind::Catalog,
        0x236e33cf_a648_5522_a452_64fce2edbb82,
        "Units",
        vec![
            p("Hierarchical", Value::Bool(false)),
            p("CodeLength", text("9")),
            p("CodeType", text("String")),
            p("CodeAllowedLength", text("Variable")),
            p("DescriptionLength", text("50")),
            p("Autonumbering", Value::Bool(false)),
        ],
        vec![attr(
            ChildTag::Attribute,
            0xee62e96c_0234_56f3_a2b1_cc938533dcd4,
            "Factor",
            num(15, 3, false),
            vec![p("Use", text("ForItem"))],
        )],
    );
    let mut o = with_ref(o, 0x844b7d6e_a239_5bf8_a62f_887bdd9d7ec4);
    o.parts.predefined = Some(Predefined::from_items(vec![
        predefined(0xa2b15e18_eddb_54e4_9cb2_39efd639275a, "Piece", "PCS", "шт"),
        predefined(
            0xfb385277_1f83_531a_a955_07d247dab579,
            "Kilogram",
            "KG",
            "кг",
        ),
    ]));
    objects.push(o);
    let o = object(
        MdKind::Catalog,
        0xb6d79697_c9c2_5aca_9769_8a8fcb231a2d,
        "Partners",
        vec![
            p("Hierarchical", Value::Bool(true)),
            p("HierarchyType", text("HierarchyOfItems")),
            p("CodeLength", text("12")),
            p("CodeType", text("String")),
            p("DescriptionLength", text("150")),
        ],
        vec![
            attr(
                ChildTag::Attribute,
                0x688b1aa7_0e78_5418_99cb_f05c405fcb65,
                "INN",
                string(12, true),
                vec![p("Use", text("ForItem"))],
            ),
            attr(
                ChildTag::Attribute,
                0xf252df94_a933_540e_a27b_cabf96e11ea0,
                "FullName",
                string(0, false),
                vec![p("Use", text("ForItem"))],
            ),
            attr(
                ChildTag::Attribute,
                0x440bc305_2bf3_5abf_b09e_cc0c971f1bd4,
                "IsPerson",
                Value::Type(td(&["xs:boolean"])),
                vec![p("Use", text("ForItem"))],
            ),
            attr(
                ChildTag::Attribute,
                0x3fbf4d16_6f64_58dc_b8b0_a120f00c48d3,
                "MainContract",
                Value::Type(td(&["cfg:CatalogRef.Contracts"])),
                vec![p("Use", text("ForItem"))],
            ),
        ],
    );
    objects.push(with_ref(o, 0x627d6173_2e47_50fe_8595_26d1577734b0));
    let o = object(
        MdKind::Catalog,
        0xe11bc0e0_7d86_5670_8032_f669e38f54db,
        "Warehouses",
        vec![
            p("Hierarchical", Value::Bool(false)),
            p("CodeLength", text("9")),
            p("CodeType", text("String")),
            p("DescriptionLength", text("50")),
        ],
        vec![attr(
            ChildTag::Attribute,
            0xf19b3e92_eb62_5fd2_a51f_f1dd638e303e,
            "Address",
            string(100, false),
            vec![p("Use", text("ForItem"))],
        )],
    );
    objects.push(with_ref(o, 0xbaccc613_34ec_5044_ba1b_8f1ca40e96e8));
    let o = object(
        MdKind::Catalog,
        0x7805960c_7d1c_5264_afb3_9a6398976db6,
        "Contracts",
        vec![
            p("Hierarchical", Value::Bool(false)),
            p("Owners", refs(&["Catalog.Partners"])),
            p("CodeLength", text("9")),
            p("CodeType", text("String")),
            p("DescriptionLength", text("100")),
        ],
        vec![
            attr(
                ChildTag::Attribute,
                0xe166ed85_8fba_55fa_95a3_f77bf8ba5a65,
                "Currency",
                string(3, true),
                vec![p("Use", text("ForItem"))],
            ),
            attr(
                ChildTag::Attribute,
                0xaabfb35a_f3cb_582a_a0a3_1cd6a68430a5,
                "ValidFrom",
                date(DateFractions::Date),
                vec![p("Use", text("ForItem"))],
            ),
        ],
    );
    objects.push(with_ref(o, 0x44f5bcac_be66_549b_aeca_b0e10a6d58bb));
    let o = object(
        MdKind::Enum,
        0x79c965d6_1a72_5b75_9148_71d20b590927,
        "Color",
        vec![],
        vec![
            enum_value(0x6ec1b7a8_29c1_58b9_907f_6d9412826ab4, "Red"),
            enum_value(0x65eb0182_383d_580d_a547_8971af664fa3, "Green"),
            enum_value(0xc98d8813_8710_5b04_915e_829c4a4dbce2, "Blue"),
        ],
    );
    objects.push(with_ref(o, 0x335f13ba_576b_5b26_bc2f_a58669e2177a));
    let o = object(
        MdKind::Document,
        0xe089d960_96cf_5635_b78c_204b7c1daa98,
        "Invoice",
        vec![
            p("NumberType", text("String")),
            p("NumberLength", text("11")),
            p("NumberAllowedLength", text("Variable")),
            p("NumberPeriodicity", text("Year")),
            p("Posting", text("Allow")),
            p("RealTimePosting", text("Deny")),
            p(
                "RegisterRecords",
                refs(&["AccumulationRegister.Stock", "AccumulationRegister.Sales"]),
            ),
        ],
        vec![
            attr(
                ChildTag::Attribute,
                0x1f67debb_9ecd_5832_a1ad_d1e84d41e5f8,
                "Partner",
                Value::Type(td(&["cfg:CatalogRef.Partners"])),
                vec![],
            ),
            attr(
                ChildTag::Attribute,
                0xba5c6f83_71d8_5810_a9b2_6751ae03cef0,
                "Warehouse",
                Value::Type(td(&["cfg:CatalogRef.Warehouses"])),
                vec![],
            ),
            attr(
                ChildTag::Attribute,
                0xd0b1b73d_f45e_59a9_bc71_24b56658ffc9,
                "Total",
                num(15, 2, false),
                vec![],
            ),
            attr(
                ChildTag::Attribute,
                0xfee75767_da7e_5b71_ade9_b1da8b834317,
                "Contract",
                Value::Type(td(&["cfg:CatalogRef.Contracts"])),
                vec![],
            ),
            attr(
                ChildTag::Attribute,
                0x49293a4b_884a_54d8_a05d_1a5b93f47539,
                "Note",
                string(0, false),
                vec![],
            ),
            attr(
                ChildTag::Attribute,
                0x7a8944b7_2cdd_544d_8fed_49cb403eaec7,
                "Author",
                composite(
                    &["cfg:CatalogRef.Partners", "cfg:CatalogRef.Warehouses"],
                    None,
                    None,
                    None,
                ),
                vec![],
            ),
            attr(
                ChildTag::Attribute,
                0xc66b7e16_ffa4_5e74_84d2_6d9130793a74,
                "BaseDoc",
                composite(
                    &["cfg:DocumentRef.Receipt", "cfg:DocumentRef.Accrual"],
                    None,
                    None,
                    None,
                ),
                vec![],
            ),
            tabular(
                0xb9102704_18ea_5cf1_afb7_14aed80992f9,
                "Items",
                vec![
                    attr(
                        ChildTag::Attribute,
                        0x7d84dd39_3b5d_56dc_ae6e_e980421d9f89,
                        "Good",
                        Value::Type(td(&["cfg:CatalogRef.Goods"])),
                        vec![],
                    ),
                    attr(
                        ChildTag::Attribute,
                        0xc9a8af5a_483f_51eb_b4bd_de81bff83b48,
                        "Qty",
                        num(15, 3, false),
                        vec![],
                    ),
                    attr(
                        ChildTag::Attribute,
                        0xc973df9d_d282_5195_9217_35979e98560d,
                        "Price",
                        num(15, 2, false),
                        vec![],
                    ),
                    attr(
                        ChildTag::Attribute,
                        0xf3a6bd32_d506_50d4_a8c6_d58f9a194aaa,
                        "Amount",
                        num(15, 2, false),
                        vec![],
                    ),
                ],
            ),
        ],
    );
    objects.push(with_ref(o, 0x75a45ec7_4d1d_5142_8445_67b54c538a92));
    let o = object(
        MdKind::Document,
        0xb0da9652_4331_5f50_a4ec_7588810b6506,
        "Receipt",
        vec![
            p("NumberType", text("String")),
            p("NumberLength", text("11")),
            p("NumberAllowedLength", text("Variable")),
            p("NumberPeriodicity", text("Year")),
            p("Posting", text("Allow")),
            p("RealTimePosting", text("Deny")),
            p("RegisterRecords", refs(&["AccumulationRegister.Stock"])),
        ],
        vec![
            attr(
                ChildTag::Attribute,
                0x28488652_b3b0_5974_8a48_1dde878463a8,
                "Partner",
                Value::Type(td(&["cfg:CatalogRef.Partners"])),
                vec![],
            ),
            attr(
                ChildTag::Attribute,
                0x7df0abf1_0c04_5424_888a_830da2010df9,
                "Warehouse",
                Value::Type(td(&["cfg:CatalogRef.Warehouses"])),
                vec![],
            ),
            attr(
                ChildTag::Attribute,
                0x39f6aa1b_662b_5cc9_99d6_fcd46d05a14e,
                "Total",
                num(15, 2, false),
                vec![],
            ),
            tabular(
                0x3d066302_96da_56d6_85c1_3b9f6d4279e0,
                "Items",
                vec![
                    attr(
                        ChildTag::Attribute,
                        0x444db5cc_9f4a_58e5_95d4_dc87739b4732,
                        "Good",
                        Value::Type(td(&["cfg:CatalogRef.Goods"])),
                        vec![],
                    ),
                    attr(
                        ChildTag::Attribute,
                        0xc3c22ad0_9a03_5d40_8b1c_d36c6a810ce1,
                        "Qty",
                        num(15, 3, false),
                        vec![],
                    ),
                    attr(
                        ChildTag::Attribute,
                        0xcfb043d8_4241_5535_a942_b465d915ff98,
                        "Price",
                        num(15, 2, false),
                        vec![],
                    ),
                    attr(
                        ChildTag::Attribute,
                        0x36309222_a169_5704_b474_1bb26f179103,
                        "Amount",
                        num(15, 2, false),
                        vec![],
                    ),
                ],
            ),
        ],
    );
    objects.push(with_ref(o, 0x5a0a3fed_9811_5473_9bcf_5560234fd25e));
    let o = object(
        MdKind::Document,
        0xe0a7eee6_64c9_54f0_87dc_6f290c0bcaa3,
        "Accrual",
        vec![
            p("NumberType", text("String")),
            p("NumberLength", text("11")),
            p("NumberAllowedLength", text("Variable")),
            p("NumberPeriodicity", text("Year")),
            p("Posting", text("Allow")),
            p("RealTimePosting", text("Deny")),
        ],
        vec![
            attr(
                ChildTag::Attribute,
                0x708240d1_baa0_536b_a655_6bde9b6ebb50,
                "Partner",
                Value::Type(td(&["cfg:CatalogRef.Partners"])),
                vec![],
            ),
            attr(
                ChildTag::Attribute,
                0xd4d9b62c_7161_5aca_8e31_2675a48336de,
                "Warehouse",
                Value::Type(td(&["cfg:CatalogRef.Warehouses"])),
                vec![],
            ),
            attr(
                ChildTag::Attribute,
                0x7e40854b_2ca9_5778_92ac_7e0eb0baf4f3,
                "Total",
                num(15, 2, false),
                vec![],
            ),
        ],
    );
    objects.push(with_ref(o, 0x2f36ee37_c68e_5d15_a2f4_d0db8d6a8241));
    let o = object(
        MdKind::AccumulationRegister,
        0x220e3038_339a_5a75_8c07_b7d2dcb5fc0c,
        "Stock",
        vec![p("RegisterType", text("Balance"))],
        vec![
            attr(
                ChildTag::Resource,
                0x0a7b4e29_94fa_59b4_bd07_3f6212778a87,
                "Qty",
                num(15, 3, false),
                vec![],
            ),
            attr(
                ChildTag::Resource,
                0xc39162ea_6c51_5411_a3c2_84d36ec0ff5c,
                "Amount",
                num(15, 2, false),
                vec![],
            ),
            attr(
                ChildTag::Attribute,
                0x5a639da4_8823_5e43_8a5b_edf6491c9783,
                "Note",
                string(50, false),
                vec![],
            ),
            attr(
                ChildTag::Dimension,
                0x1ea80b1e_821b_53e8_8799_1e6dddda0c9e,
                "Good",
                Value::Type(td(&["cfg:CatalogRef.Goods"])),
                vec![],
            ),
            attr(
                ChildTag::Dimension,
                0x0a1fcb7d_1a28_53fa_b42e_52c51389c333,
                "Warehouse",
                Value::Type(td(&["cfg:CatalogRef.Warehouses"])),
                vec![],
            ),
        ],
    );
    objects.push(o);
    objects.extend(rtq_registers());
    config(ScriptVariant::Russian, objects)
}

fn rtq_registers() -> Vec<MdObject> {
    let goods = || Value::Type(td(&["cfg:CatalogRef.Goods"]));
    let sales = object(
        MdKind::AccumulationRegister,
        0xabc8ab47_4f38_5cfb_a868_2e489f5874d7,
        "Sales",
        vec![p("RegisterType", text("Turnovers"))],
        vec![
            attr(
                ChildTag::Resource,
                0x56520959_85ee_533f_9b57_d80428271ea7,
                "Qty",
                num(15, 3, false),
                vec![],
            ),
            attr(
                ChildTag::Resource,
                0x0404b4af_0103_5089_ae15_ca53a1e8364f,
                "Amount",
                num(15, 2, false),
                vec![],
            ),
            attr(
                ChildTag::Dimension,
                0x855ea903_9bed_558c_8460_d3726c6165c6,
                "Good",
                goods(),
                vec![],
            ),
            attr(
                ChildTag::Dimension,
                0x35952b94_b894_5547_a480_8aa52d6164c5,
                "Partner",
                Value::Type(td(&["cfg:CatalogRef.Partners"])),
                vec![],
            ),
        ],
    );
    let info = |id: u128, name: &str, periodicity: &str, mode: &str, children: Vec<Child>| {
        object(
            MdKind::InformationRegister,
            id,
            name,
            vec![
                p("InformationRegisterPeriodicity", text(periodicity)),
                p("WriteMode", text(mode)),
                p(
                    "MainFilterOnPeriod",
                    Value::Bool(periodicity != "Nonperiodical"),
                ),
            ],
            children,
        )
    };
    let dim = |id: u128, name: &str, ty: Value| {
        attr(
            ChildTag::Dimension,
            id,
            name,
            ty,
            vec![p("MainFilter", Value::Bool(true))],
        )
    };
    let prices = info(
        0x3eeedced_9ad6_5304_85cd_245d66cb1498,
        "Prices",
        "Day",
        "Independent",
        vec![
            attr(
                ChildTag::Resource,
                0xfd6686fb_cace_54a8_a06c_64b598ebb285,
                "Price",
                num(15, 2, false),
                vec![],
            ),
            attr(
                ChildTag::Resource,
                0xbe24a8c4_452c_5e15_b9ae_9f9eea9c0029,
                "Comment",
                string(50, false),
                vec![],
            ),
            dim(0x3b4da9c5_a930_5a58_a2e5_c5926ca8aed1, "Good", goods()),
            dim(
                0x7eda403b_8781_5608_8194_88737ad10905,
                "PriceKind",
                string(10, false),
            ),
        ],
    );
    let infor = info(
        0xeef71e9e_f5e6_5e81_8754_159a8ffc156a,
        "InfoR",
        "RecorderPosition",
        "RecorderSubordinate",
        vec![
            attr(
                ChildTag::Resource,
                0xb2262c2c_8d47_5fa7_bc86_2fc9c7ebceb2,
                "Val",
                num(10, 2, false),
                vec![],
            ),
            dim(0x4897521a_7d17_5b19_b613_2a0ea31207f1, "Good", goods()),
        ],
    );
    let kv = info(
        0xbb540c69_ac3f_5db9_9d8d_7920931c394c,
        "KV",
        "Nonperiodical",
        "Independent",
        vec![
            attr(
                ChildTag::Resource,
                0x4d0f1be6_09f2_5e8c_a84f_43b683418164,
                "Val",
                composite(
                    &[
                        "xs:string",
                        "xs:decimal",
                        "xs:dateTime",
                        "xs:boolean",
                        "cfg:CatalogRef.Goods",
                    ],
                    Some((20, 5, false)),
                    Some((200, false)),
                    Some(DateFractions::DateTime),
                ),
                vec![],
            ),
            dim(
                0x6916557d_007d_5404_b659_7bfe5d8cd8de,
                "Key",
                string(50, false),
            ),
        ],
    );
    vec![sales, prices, infor, kv]
}

pub fn rtqr_like() -> Configuration {
    let mut c = rtq_like();
    let mut sync = object(
        MdKind::ExchangePlan,
        0x87636d65_1a8f_5723_b1dd_166dbe3e5348,
        "Sync",
        vec![
            p("CodeLength", text("9")),
            p("CodeAllowedLength", text("Variable")),
            p("DescriptionLength", text("100")),
        ],
        vec![],
    );
    sync = with_ref(sync, 0xc92ef4a4_c1fc_5a0d_bf8b_a793a3e170d2);
    let item = |id: u128, name: &str| ContentItem {
        object: MdRef {
            id: Some(g(id)),
            name: name.into(),
        },
        auto_record: false,
    };
    sync.parts.exchange_content = Some(vec![
        item(0xd2bee9d7_1859_58a1_ad66_1e933e9b0522, "Catalog.Goods"),
        item(0xe089d960_96cf_5635_b78c_204b7c1daa98, "Document.Invoice"),
    ]);
    c.objects.push(sync);
    for (id, name, ty) in [
        (
            0x06ac9990_40cc_52b0_a597_78fd82894d2c,
            "CNum",
            num(15, 2, false),
        ),
        (
            0xe54301d9_0a29_59ff_8ffc_b7e59c7e1393,
            "CStr",
            string(100, false),
        ),
        (
            0xca72b1c7_cf64_5e1b_8bbf_859891337023,
            "CGood",
            Value::Type(td(&["cfg:CatalogRef.Goods"])),
        ),
    ] {
        c.objects.push(object(
            MdKind::Constant,
            id,
            name,
            vec![p("Type", ty)],
            vec![],
        ));
    }
    let column = child(
        ChildTag::Column,
        0xd15661ea_0f0c_5921_a042_2398387e3bfd,
        "Partner",
        vec![p(
            "References",
            Value::RefList(vec![
                MdRef {
                    id: Some(g(0x1f67debb_9ecd_5832_a1ad_d1e84d41e5f8)),
                    name: "Document.Invoice.Attribute.Partner".into(),
                },
                MdRef {
                    id: Some(g(0x28488652_b3b0_5974_8a48_1dde878463a8)),
                    name: "Document.Receipt.Attribute.Partner".into(),
                },
            ]),
        )],
        vec![],
    );
    c.objects.push(object(
        MdKind::DocumentJournal,
        0x30541ae0_6d3a_5da2_8aae_ddc1c675ac68,
        "AllDocs",
        vec![p(
            "RegisteredDocuments",
            refs(&["Document.Invoice", "Document.Receipt"]),
        )],
        vec![column],
    ));
    c
}
