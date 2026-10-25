use super::codec::{InternalReader, InternalWriter, ValueCodec};
use super::cycle_fatal;
use super::internal::text::{decode_str, encode_str};
use brace::Node;
use bsl_prim::Str;
use bsl_vm::{
    Category, CategorySet, ErrorInfo, ErrorWireIdentity, ErrorWireIdentityParts, ErrorWireMetadata,
    Exception, Execution, HostError, ObjRef, StackEntry, Value, error_info_of, error_info_value,
};
use platform_catalog::{TypeIdx, catalog};
use std::rc::Rc;
use std::sync::Arc;

const NODE: &str = "8d366056-4d5a-4d88-a207-0ae535b7d28e";

const MAX_CHAIN: usize = 400;

struct ErrorInfoCodec;

pub(crate) fn codecs() -> Vec<(TypeIdx, Arc<dyn ValueCodec>)> {
    catalog()
        .find_type("ErrorInfo")
        .map(|t| (t, Arc::new(ErrorInfoCodec) as Arc<dyn ValueCodec>))
        .into_iter()
        .collect()
}

fn node_of(i: &ErrorInfo, cause: Option<Node>) -> Node {
    let categories = i
        .categories
        .iter()
        .map(|c| Node::Str(c.name_ru().to_string()))
        .collect();
    let mut fields = vec![
        Node::Atom(NODE.into()),
        encode_str(i.description.units()),
        encode_str(i.module_name.units()),
        Node::int(i64::from(i.line)),
        encode_str(i.source_line.units()),
        encode_str(i.code.units()),
        Node::List(categories),
        Node::List(cause.into_iter().collect()),
    ];
    if let Value::String(additional) = &i.additional {
        fields.push(encode_str(additional.units()));
    }
    Node::List(fields)
}

impl ValueCodec for ErrorInfoCodec {
    fn internal_write(
        &self,
        ex: &mut Execution,
        o: &ObjRef,
        w: &mut InternalWriter,
    ) -> Result<Option<Vec<Node>>, Exception> {
        let Some(top) = error_info_of(&Value::Object(o.clone())) else {
            return Ok(None);
        };
        let mut count = 0usize;
        let mut cursor = Some(&top);
        while let Some(info) = cursor {
            if count >= MAX_CHAIN {
                return Err(cycle_fatal(ex));
            }
            count += 1;
            cursor = info.cause.as_ref();
        }
        ex.reserve((count * std::mem::size_of::<Rc<ErrorInfo>>()) as u64)?;
        let mut chain = Vec::with_capacity(count);
        let mut cursor = Some(top);
        while let Some(info) = cursor {
            cursor = info.cause.clone();
            chain.push(info);
        }
        let has_wire = chain.iter().any(|info| info.wire_metadata().is_some());
        if has_wire {
            for info in &chain {
                ex.charge(1)?;
                if !info.wire_matches_projection() {
                    return Err(wire_projection_error(ex));
                }
            }
            let mut data = None;
            for info in chain.iter().rev() {
                let Some(metadata) = info.wire_metadata() else {
                    return Err(wire_projection_error(ex));
                };
                w.account(ex, wire_write_bytes(info, metadata))?;
                data = Some(wire_data(info, metadata, data));
            }
            return Ok(data.map(|d| vec![d]));
        }
        let mut node = None;
        for i in chain.iter().rev() {
            w.account(ex, legacy_write_bytes(i))?;
            node = Some(node_of(i, node.take()));
        }
        let inner = Node::List(node.into_iter().collect());
        Ok(Some(vec![Node::List(vec![Node::Atom(NODE.into()), inner])]))
    }

    fn internal_read(
        &self,
        ex: &mut Execution,
        items: &[Node],
        r: &mut InternalReader,
    ) -> Result<Value, Exception> {
        let chain = if legacy_shape(items) {
            legacy_preflight(ex, items, r)?;
            nodes(items).ok_or_else(|| r.format_error(ex))?
        } else {
            wire_nodes(ex, items, r)?
        };
        let mut cause: Option<Rc<ErrorInfo>> = None;
        for f in chain.into_iter().rev() {
            ex.charge(1)?;
            let mut info = ErrorInfo::new(f.description, f.categories);
            info.module_name = f.module_name;
            info.line = f.line;
            info.source_line = f.source_line;
            info.cause = cause.take();
            info.code = f.code;
            info.additional = f.additional;
            info.stack = f.stack;
            if let Some((identity, frame_ids)) = f.wire {
                info.attach_wire_metadata(identity, frame_ids)
                    .map_err(|_| r.format_error(ex))?;
            }
            cause = Some(Rc::new(info));
        }
        Ok(cause.map_or(Value::Undefined, error_info_value))
    }
}

struct Fields {
    description: Str,
    module_name: Str,
    line: u32,
    source_line: Str,
    code: Str,
    categories: CategorySet,
    additional: Value,
    stack: Vec<StackEntry>,
    wire: Option<(ErrorWireIdentity, Vec<Str>)>,
}

fn legacy_write_bytes(info: &ErrorInfo) -> u64 {
    let mut bytes = 1024u64;
    for text in [
        &info.description,
        &info.module_name,
        &info.source_line,
        &info.code,
    ]
    .into_iter()
    .chain(match &info.additional {
        Value::String(text) => Some(text),
        _ => None,
    }) {
        bytes = bytes.saturating_add((text.len() as u64).saturating_mul(12).saturating_add(32));
    }
    for category in info.categories.iter() {
        bytes = bytes
            .saturating_add(2 * (std::mem::size_of::<Node>() + category.name_ru().len()) as u64);
    }
    bytes
}

fn legacy_preflight(
    ex: &mut Execution,
    items: &[Node],
    r: &mut InternalReader,
) -> Result<(), Exception> {
    let Some(mut node) = items
        .first()
        .and_then(Node::as_list)
        .and_then(|head| head.get(1))
        .and_then(Node::as_list)
        .and_then(|payload| payload.first())
    else {
        return Err(r.format_error(ex));
    };
    let mut count = 0;
    let mut retained = (4 * std::mem::size_of::<Fields>()) as u64;
    let mut folding_scratch = 1024u64;
    loop {
        ex.charge(1)?;
        let Some(fields) = node.as_list().filter(|fields| {
            matches!(fields.len(), 8 | 9) && fields.first().and_then(Node::as_atom) == Some(NODE)
        }) else {
            return Err(r.format_error(ex));
        };
        if count >= MAX_CHAIN {
            return Err(r.format_error(ex));
        }
        count += 1;
        let Some(line) = fields[3].as_atom() else {
            return Err(r.format_error(ex));
        };
        ex.charge(1 + (line.len() as u64).div_ceil(64))?;
        let mut bytes = (2 * std::mem::size_of::<Fields>()
            + std::mem::size_of::<ErrorInfo>()
            + 2 * std::mem::size_of::<usize>()
            + 6 * (std::mem::size_of::<Vec<u16>>() + 2 * std::mem::size_of::<usize>()))
            as u64;
        for index in [1, 2, 4, 5, 8] {
            let Some(field) = fields.get(index) else {
                continue;
            };
            let text = match field {
                Node::Str(text) => text,
                Node::Atom(text) if text.starts_with('"') => text,
                _ => return Err(r.format_error(ex)),
            };
            ex.charge(1 + (text.len() as u64).div_ceil(64))?;
            bytes = bytes.saturating_add((text.len() as u64).saturating_mul(8));
        }
        let Some(categories) = fields.get(6).and_then(Node::as_list) else {
            return Err(r.format_error(ex));
        };
        ex.charge(1 + categories.len() as u64)?;
        for category in categories {
            let Some(name) = category.as_str() else {
                return Err(r.format_error(ex));
            };
            ex.charge(1 + (name.len() as u64).div_ceil(64))?;
            folding_scratch =
                folding_scratch.max(1024u64.saturating_add((name.len() as u64).saturating_mul(8)));
        }
        retained = retained.saturating_add(bytes);
        ex.reserve(retained.saturating_add(folding_scratch))?;
        let Some(causes) = fields
            .get(7)
            .and_then(Node::as_list)
            .filter(|causes| causes.len() <= 1)
        else {
            return Err(r.format_error(ex));
        };
        match causes.first() {
            Some(cause) => node = cause,
            None => return Ok(()),
        }
    }
}

fn nodes(items: &[Node]) -> Option<Vec<Fields>> {
    let head = items.first()?.as_list()?;
    if head.first()?.as_atom()? != NODE {
        return None;
    }
    let mut node = head.get(1)?.as_list()?.first()?;
    let mut out = Vec::new();
    loop {
        let l = node.as_list()?;
        if l.first()?.as_atom()? != NODE || out.len() >= MAX_CHAIN {
            return None;
        }
        let text = |i: usize| decode_str(l.get(i)?).map(Str::from_vec);
        let mut categories = CategorySet::default();
        for c in l.get(6)?.as_list()? {
            categories = categories.with(Category::from_name(c.as_str()?)?);
        }
        out.push(Fields {
            description: text(1)?,
            module_name: text(2)?,
            line: l.get(3)?.as_atom()?.parse().ok()?,
            source_line: text(4)?,
            code: text(5)?,
            categories,
            additional: match l.get(8) {
                Some(_) => Value::String(text(8)?),
                None => Value::Undefined,
            },
            stack: Vec::new(),
            wire: None,
        });
        match l.get(7)?.as_list()?.first() {
            Some(next) => node = next,
            None => return Some(out),
        }
    }
}

fn legacy_shape(items: &[Node]) -> bool {
    items.len() == 1
        && items[0].as_list().is_some_and(|head| {
            head.len() == 2
                && head[0] == Node::atom(NODE)
                && head[1].as_list().is_some_and(|payload| {
                    payload.len() == 1
                        && payload[0]
                            .as_list()
                            .is_some_and(|node| matches!(node.len(), 8 | 9))
                })
        })
}

fn wire_categories(bits: &str) -> Option<CategorySet> {
    if bits.len() != 26 || !bits.bytes().all(|b| matches!(b, b'0' | b'1')) {
        return None;
    }
    let known = [
        (5, Category::ScriptRuntimeError),
        (6, Category::ScriptUseError),
        (7, Category::ExceptionRaisedFromScript),
        (21, Category::ConfigurationError),
    ];
    let mut out = CategorySet::default();
    for (bit, value) in bits.bytes().rev().enumerate() {
        if value == b'1' {
            out = out.with(known.iter().find(|(n, _)| *n == bit)?.1);
        }
    }
    Some(out)
}

fn wire_nodes(
    ex: &mut Execution,
    items: &[Node],
    r: &mut InternalReader,
) -> Result<Vec<Fields>, Exception> {
    let invalid = |ex: &mut Execution, r: &mut InternalReader| r.format_error(ex);
    if items.len() != 1 {
        return Err(invalid(ex, r));
    }
    let mut data = &items[0];
    let mut out = Vec::new();
    let mut retained_bytes = (4 * std::mem::size_of::<Fields>()) as u64;
    loop {
        ex.charge(1)?;
        let parsed = (|| {
            let d = data.as_list()?;
            if d.len() != 2 || d[0] != Node::atom(NODE) || out.len() >= MAX_CHAIN {
                return None;
            }
            let p = d[1].as_list()?;
            if p.len() != 10 {
                return None;
            }
            let n = p[0].as_list()?;
            if n.len() != 6 {
                return None;
            }
            if n[0] != Node::atom(NODE) {
                return None;
            }
            n[1].as_str()?;
            n[3].as_str()?;
            super::kind::parse_guid(n[5].as_atom()?)?;
            let categories = wire_categories(n[4].as_str()?)?;
            if !matches!(p[1].as_atom()?, "8" | "36") {
                return None;
            }
            for index in [2, 3, 4, 6, 8, 9] {
                p[index].as_str()?;
            }
            let line: u32 = p[5].as_atom()?.parse().ok()?;
            let frames = p[7].as_list()?;
            let count: usize = frames.first()?.as_atom()?.parse().ok()?;
            if count != frames.len() - 1 {
                return None;
            }
            for frame in &frames[1..] {
                let f = frame.as_list()?;
                if f.len() != 4 {
                    return None;
                }
                f[0].as_str()?;
                f[1].as_str()?;
                f[2].as_atom()?.parse::<u32>().ok()?;
                f[3].as_str()?;
            }
            Some((p, n, line, frames, categories))
        })();
        let Some((p, n, line, frames, categories)) = parsed else {
            return Err(invalid(ex, r));
        };
        let sized = (|| {
            let mut bytes = (2 * std::mem::size_of::<Fields>()
                + std::mem::size_of::<ErrorInfo>()
                + std::mem::size_of::<ErrorWireMetadata>()
                + 2 * std::mem::size_of::<usize>()) as u64;
            let string_header =
                (std::mem::size_of::<Vec<u16>>() + 2 * std::mem::size_of::<usize>()) as u64;
            bytes = bytes.saturating_add(11 * string_header);
            for index in [2, 3, 4, 6, 8, 9] {
                bytes = bytes.saturating_add(p[index].as_str()?.len() as u64 * 2);
            }
            bytes = bytes.saturating_add((n[1].as_str()?.len() + n[3].as_str()?.len()) as u64 * 2);
            for frame in &frames[1..] {
                let f = frame.as_list()?;
                bytes = bytes.saturating_add(
                    2 * std::mem::size_of::<StackEntry>() as u64
                        + std::mem::size_of::<Str>() as u64
                        + 3 * string_header
                        + (f[0].as_str()?.len() + f[1].as_str()?.len() + f[3].as_str()?.len())
                            as u64
                            * 2,
                );
            }
            Some(bytes)
        })();
        let Some(bytes) = sized else {
            return Err(invalid(ex, r));
        };
        retained_bytes = retained_bytes.saturating_add(bytes);
        ex.reserve(retained_bytes)?;
        let text = |index: usize| p[index].as_str().map(wire_text);
        let built = (|| {
            let stack = frames[1..]
                .iter()
                .map(|frame| {
                    let f = frame.as_list()?;
                    Some(StackEntry {
                        module_name: wire_text(f[0].as_str()?),
                        line: f[2].as_atom()?.parse().ok()?,
                        source_line: wire_text(f[3].as_str()?),
                    })
                })
                .collect::<Option<Vec<_>>>()?;
            let frame_ids = frames[1..]
                .iter()
                .map(|frame| Some(wire_text(frame.as_list()?.get(1)?.as_str()?)))
                .collect::<Option<Vec<_>>>()?;
            let identity = ErrorWireIdentity::new(ErrorWireIdentityParts {
                class_id: super::kind::parse_guid(n[0].as_atom()?)?,
                reason: p[1].as_atom()?.parse().ok()?,
                positioned_description: wire_text(n[1].as_str()?),
                machine_stack: wire_text(n[3].as_str()?),
                category_width: 26,
                category_bits: u32::from_str_radix(n[4].as_str()?, 2).ok()?,
                ui_helper_id: super::kind::parse_guid(n[5].as_atom()?)?,
                module_id: text(4)?,
            })?;
            Some((stack, frame_ids, identity))
        })();
        let (Some((stack, frame_ids, identity)), Some(t2), Some(t3), Some(t6), Some(t8), Some(t9)) =
            (built, text(2), text(3), text(6), text(8), text(9))
        else {
            return Err(invalid(ex, r));
        };
        out.push(Fields {
            description: t2,
            module_name: t3,
            line,
            source_line: t6,
            code: t8,
            additional: Value::String(t9),
            categories,
            stack,
            wire: Some((identity, frame_ids)),
        });
        if n[2] == Node::list(vec![Node::atom("00000000-0000-0000-0000-000000000000")]) {
            return Ok(out);
        }
        data = &n[2];
    }
}

fn wire_text(text: &str) -> Str {
    let mut units = Vec::with_capacity(text.len());
    units.extend(text.encode_utf16());
    Str::from_vec(units)
}

fn wire_projection_error(ex: &mut Execution) -> Exception {
    ex.error(HostError::text(
        "Изменена проекция сериализованной ошибки",
        "Serialized error projection has changed",
    ))
}

fn wire_write_bytes(info: &ErrorInfo, metadata: &ErrorWireMetadata) -> u64 {
    let identity = metadata.identity().parts();
    let mut bytes = 1024u64;
    for text in [
        &info.description,
        &info.module_name,
        &info.source_line,
        &info.code,
        &identity.positioned_description,
        &identity.machine_stack,
        &identity.module_id,
    ] {
        bytes = bytes.saturating_add(text.len() as u64 * 3);
    }
    if let Value::String(additional) = &info.additional {
        bytes = bytes.saturating_add(additional.len() as u64 * 3);
    }
    for (frame, module_id) in info.stack.iter().zip(metadata.frame_module_ids()) {
        bytes = bytes.saturating_add(
            320 + (frame.module_name.len() + module_id.len() + frame.source_line.len()) as u64 * 3,
        );
    }
    bytes
}

fn wire_data(info: &ErrorInfo, metadata: &ErrorWireMetadata, cause: Option<Node>) -> Node {
    let id = metadata.identity().parts();
    let mut frames = Vec::with_capacity(info.stack.len() + 1);
    frames.push(Node::int(info.stack.len() as i64));
    for (frame, module_id) in info.stack.iter().zip(metadata.frame_module_ids()) {
        frames.push(Node::list(vec![
            encode_str(frame.module_name.units()),
            encode_str(module_id.units()),
            Node::int(i64::from(frame.line)),
            encode_str(frame.source_line.units()),
        ]));
    }
    let Value::String(additional) = &info.additional else {
        unreachable!("projection validated before writing")
    };
    let node = Node::list(vec![
        Node::atom(super::kind::guid_text(&id.class_id)),
        encode_str(id.positioned_description.units()),
        cause.unwrap_or_else(|| {
            Node::list(vec![Node::atom("00000000-0000-0000-0000-000000000000")])
        }),
        encode_str(id.machine_stack.units()),
        Node::str(format!(
            "{:0width$b}",
            id.category_bits,
            width = usize::from(id.category_width)
        )),
        Node::atom(super::kind::guid_text(&id.ui_helper_id)),
    ]);
    Node::list(vec![
        Node::atom(NODE),
        Node::list(vec![
            node,
            Node::int(i64::from(id.reason)),
            encode_str(info.description.units()),
            encode_str(info.module_name.units()),
            encode_str(id.module_id.units()),
            Node::int(i64::from(info.line)),
            encode_str(info.source_line.units()),
            Node::list(frames),
            encode_str(info.code.units()),
            encode_str(additional.units()),
        ]),
    ])
}
