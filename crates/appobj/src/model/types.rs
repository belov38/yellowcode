use super::kinds::ObjKind;
use super::{ObjId, Uuid16};
use md_model::{
    BinaryQualifiers, Configuration, DateFractions, NumberQualifiers, StringQualifiers,
    TypeDescription,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TypeAtom {
    Bool,
    Number,
    String,
    Date,
    Uuid,
    ValueStorage,
    Binary,
    Null,
    Undefined,
    Ref(ObjId),
    AnyRef(Option<ObjKind>),
    Platform(String),
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct TypeSet {
    pub atoms: Vec<TypeAtom>,
    pub number: Option<NumberQualifiers>,
    pub string: Option<StringQualifiers>,
    pub date: Option<DateFractions>,
    pub binary: Option<BinaryQualifiers>,
}

impl TypeSet {
    pub fn of_atom(a: TypeAtom) -> TypeSet {
        TypeSet {
            atoms: vec![a],
            ..TypeSet::default()
        }
    }

    pub fn string(length: u32, fixed: bool) -> TypeSet {
        TypeSet {
            atoms: vec![TypeAtom::String],
            string: Some(StringQualifiers { length, fixed }),
            ..TypeSet::default()
        }
    }

    pub fn number(digits: u32, fraction: u32, nonnegative: bool) -> TypeSet {
        TypeSet {
            atoms: vec![TypeAtom::Number],
            number: Some(NumberQualifiers {
                digits,
                fraction,
                nonnegative,
            }),
            ..TypeSet::default()
        }
    }

    pub fn date(f: DateFractions) -> TypeSet {
        TypeSet {
            atoms: vec![TypeAtom::Date],
            date: Some(f),
            ..TypeSet::default()
        }
    }

    pub fn single(&self) -> Option<&TypeAtom> {
        match self.atoms.as_slice() {
            [a] => Some(a),
            _ => None,
        }
    }

    pub fn of(
        cfg: &Configuration,
        td: &TypeDescription,
        by_type_id: &dyn Fn(Uuid16) -> Option<ObjId>,
    ) -> TypeSet {
        TypeSet::of_diag(cfg, td, by_type_id, &mut Vec::new())
    }

    pub fn of_diag(
        cfg: &Configuration,
        td: &TypeDescription,
        by_type_id: &dyn Fn(Uuid16) -> Option<ObjId>,
        diag: &mut Vec<String>,
    ) -> TypeSet {
        let td = cfg.expand_type(td);
        let mut atoms = Vec::with_capacity(td.types.len());
        for t in &td.types {
            atoms.push(atom(
                &t.name.to_string(),
                t.set,
                t.type_id,
                by_type_id,
                diag,
            ));
        }
        TypeSet {
            atoms,
            number: td.number,
            string: td.string,
            date: td.date,
            binary: td.binary,
        }
    }
}

fn atom(
    qname: &str,
    set: bool,
    type_id: Option<Uuid16>,
    by_type_id: &dyn Fn(Uuid16) -> Option<ObjId>,
    diag: &mut Vec<String>,
) -> TypeAtom {
    match qname {
        "xs:boolean" => return TypeAtom::Bool,
        "xs:decimal" => return TypeAtom::Number,
        "xs:string" => return TypeAtom::String,
        "xs:dateTime" => return TypeAtom::Date,
        "v8:UUID" => return TypeAtom::Uuid,
        "v8:ValueStorage" => return TypeAtom::ValueStorage,
        "xs:base64Binary" => return TypeAtom::Binary,
        "v8:Null" => return TypeAtom::Null,
        "v8:Undefined" => return TypeAtom::Undefined,
        "cfg:AnyRef" => return TypeAtom::AnyRef(None),
        _ => {}
    }
    let Some(rest) = qname.strip_prefix("cfg:") else {
        return TypeAtom::Platform(qname.to_string());
    };
    let (head, name) = match rest.split_once('.') {
        Some((h, n)) => (h, Some(n)),
        None => (rest, None),
    };
    let Some(kind) = head.strip_suffix("Ref").and_then(ObjKind::from_en) else {
        return TypeAtom::Platform(qname.to_string());
    };
    if name.is_none() || set {
        return TypeAtom::AnyRef(Some(kind));
    }
    match type_id.and_then(by_type_id) {
        Some(id) => TypeAtom::Ref(id),
        None => {
            diag.push(format!("dangling type reference: {qname}"));
            TypeAtom::AnyRef(Some(kind))
        }
    }
}
