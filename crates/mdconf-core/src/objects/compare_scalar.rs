use super::decode::{self, Descriptor, Head, Provenance, ResolvedProperty, TreeDomain};
use super::encode_cacontent::encode_cacontent;
use super::encode_characteristics::{
    CharacteristicsPrefixes, encode_characteristics_with_predefined,
};
use super::encode_cmdgroup::encode_cmdgroup;
use super::encode_color::encode_color;
use super::encode_cparams::encode_cparams_with_predefined;
use super::encode_cplinks::encode_cplinks;
use super::encode_enum_dtref::encode_enum_dtref;
use super::encode_fieldlist::encode_fieldlist;
use super::encode_focontent::encode_focontent;
use super::encode_linkbytype::encode_linkbytype;
use super::encode_mdlist::encode_mdlist;
use super::encode_method::encode_method;
use super::encode_named_emptyref::encode_named_emptyref;
use super::encode_picture::encode_picture;
use super::encode_predefined_dtref::{
    PredefinedContext, encode_predefined_dtref, is_predefined_shape,
};
use super::encode_purposes::encode_purposes;
use super::encode_reference::ReferenceContext;
use super::encode_scalar::{ScalarEncodeError, encode_scalar};
use super::encode_shortcut::encode_shortcut;
use super::encode_stdattrs::{
    StdAttrsEncodeError, StdAttrsSource, encode_stdattrs_owned_source_with_predefined,
    encode_stdattrs_source_with_predefined,
};
use super::encode_styleborder::{StyleBorderPrefixes, encode_styleborder};
use super::encode_stylefont::{StyleFontPrefixes, encode_stylefont};
use super::encode_stylefont_absolute::{AbsoluteFontPrefixes, encode_stylefont_absolute};
use super::encode_stylevalue::encode_stylevalue;
use super::encode_type_context::TypeContext;
use super::encode_typed::{encode_typed, encode_typed_with_index};
use super::encode_xdtopackages::{XdtoPackagesPrefixes, encode_xdtopackages};
use super::encode_xdtotype::encode_xdtotype;
use crate::Configuration;
use crate::entries::Entries;
use crate::index::Index;
use crate::model::Diagnostics;
use brace::Node;
use mdschema::{Codec, PropSpec, Schema};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub(crate) enum Outcome {
    Masked,
    TypeFiltered,
    Derived,
    Default,
    Unavailable,
    SecondarySource,
    UnsupportedCodec,
    InverseRefused,
    EqualNode,
    DifferentNode,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Encoded {
    Node(Node),
    Pair { module: Node, method: Node },
    NoInverse,
    Refused,
    Secondary,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Counts {
    pub total: u64,
    pub outcomes: [u64; 10],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Observation {
    EmptyObservation,
    Observed,
}

#[derive(Debug)]
pub(crate) struct ScalarComparisonReport {
    pub observation: Observation,
    pub total: u64,
    pub original: Counts,
    pub normalized: Counts,
    pub diagnostics: Diagnostics,
}

#[derive(Debug)]
pub(crate) enum ComparisonError {
    Replay(crate::Error),
    EventTable(String),
    CounterOverflow,
    CounterInvariant,
    AmbiguousReferenceIndex,
    InvalidTypeContext,
}

type Result<T> = std::result::Result<T, ComparisonError>;

fn header_spec(head: Head) -> PropSpec {
    PropSpec {
        codec: match head {
            Head::Name | Head::Comment => Codec::Str,
            Head::Synonym => Codec::LStr,
        },
        ..Default::default()
    }
}

fn encode_stdattrs_event(
    schema: &'static Schema,
    spec: &PropSpec,
    owner: Option<&super::codecs::Owner>,
    index: &Index,
    references: &ReferenceContext,
    value: &crate::model::Value,
    raw: &Node,
) -> std::result::Result<Node, StdAttrsEncodeError> {
    encode_stdattrs_event_with(
        schema,
        spec,
        owner,
        index,
        references,
        &PredefinedContext::default(),
        value,
        raw,
    )
}

#[allow(clippy::too_many_arguments)]
fn encode_stdattrs_event_with(
    schema: &'static Schema,
    spec: &PropSpec,
    owner: Option<&super::codecs::Owner>,
    index: &Index,
    references: &ReferenceContext,
    predefined: &PredefinedContext<'_>,
    value: &crate::model::Value,
    raw: &Node,
) -> std::result::Result<Node, StdAttrsEncodeError> {
    let crate::model::Value::StandardAttributes(attrs) = value else {
        return Err(StdAttrsEncodeError::WrongValue);
    };
    let source = StdAttrsSource::from_source(raw, attrs.len())?;
    let overridden;
    let owner = match spec.std.as_deref() {
        Some(kind) if !kind.is_empty() => {
            overridden = super::codecs::Owner {
                kind: kind.into(),
                name: String::new(),
            };
            Some(&overridden)
        }
        _ => owner,
    };
    match owner {
        Some(owner) => encode_stdattrs_owned_source_with_predefined(
            index, predefined, schema, owner, value, &source, references,
        ),
        None => encode_stdattrs_source_with_predefined(
            index, predefined, schema, "", value, &source, references,
        ),
    }
}

fn encode_tvalue_event(
    index: &Index,
    predefined: &PredefinedContext<'_>,
    value: &crate::model::Value,
) -> std::result::Result<Node, ()> {
    match value {
        crate::model::Value::Typed(crate::model::TypedValue::DesignTimeRef(text))
            if !text.is_empty() =>
        {
            if index.names.values().any(|name| name == text) {
                encode_enum_dtref(index, value).map_err(|_| ())
            } else if is_predefined_shape(text) {
                encode_predefined_dtref(index, predefined, value).map_err(|_| ())
            } else {
                encode_named_emptyref(index, value).map_err(|_| ())
            }
        }
        crate::model::Value::Typed(crate::model::TypedValue::FixedArray(_)) => {
            encode_typed_with_index(index, value).map_err(|_| ())
        }
        _ => encode_typed(value).map_err(|_| ()),
    }
}

fn classify(
    v: &ResolvedProperty<'_>,
    references: &ReferenceContext,
    types: &TypeContext,
    index: &Index,
) -> Result<Outcome> {
    classify_with(v, references, types, index, &PredefinedContext::default())
}

fn classify_with(
    v: &ResolvedProperty<'_>,
    references: &ReferenceContext,
    types: &TypeContext,
    index: &Index,
    predefined: &PredefinedContext<'_>,
) -> Result<Outcome> {
    if v.masked {
        return Ok(Outcome::Masked);
    }
    if v.type_filtered {
        return Ok(Outcome::TypeFiltered);
    }
    match v.provenance {
        Provenance::Derived => return Ok(Outcome::Derived),
        Provenance::Default => return Ok(Outcome::Default),
        Provenance::Unavailable => return Ok(Outcome::Unavailable),
        Provenance::Selected => {}
    }
    let (Some(raw), Some(_)) = (v.raw, v.path) else {
        return Ok(Outcome::Unavailable);
    };
    Ok(
        match encode_property(v, &v.prop.value, raw, references, types, index, predefined)? {
            Encoded::Node(node) if &node == raw => Outcome::EqualNode,
            Encoded::Node(_) => Outcome::DifferentNode,
            Encoded::Pair { module, method } if &module == raw && Some(&method) == v.raw2 => {
                Outcome::EqualNode
            }
            Encoded::Pair { .. } => Outcome::DifferentNode,
            Encoded::Refused => Outcome::InverseRefused,
            Encoded::NoInverse => Outcome::UnsupportedCodec,
            Encoded::Secondary => Outcome::SecondarySource,
        },
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn encode_property(
    v: &ResolvedProperty<'_>,
    value: &crate::model::Value,
    raw: &Node,
    references: &ReferenceContext,
    types: &TypeContext,
    index: &Index,
    predefined: &PredefinedContext<'_>,
) -> Result<Encoded> {
    if v.path2.is_some() {
        if let Descriptor::Spec(spec) = v.descriptor
            && spec.codec == Codec::Method
            && v.raw2.is_some()
        {
            return Ok(match encode_method(index, value) {
                Ok(pair) => Encoded::Pair {
                    module: pair.module,
                    method: pair.method,
                },
                Err(_) => Encoded::Refused,
            });
        }
        return Ok(Encoded::Secondary);
    }
    let header;
    let spec = match v.descriptor {
        Descriptor::Spec(spec) => spec,
        Descriptor::Header(head) => {
            header = header_spec(head);
            &header
        }
    };
    if spec.codec == Codec::Ref {
        return Ok(match references.encode(value) {
            Ok(node) => Encoded::Node(node),
            Err(_) => Encoded::Refused,
        });
    }
    if spec.codec == Codec::Type {
        return Ok(match types.encode(value) {
            Ok(node) => Encoded::Node(node),
            Err(_) => Encoded::Refused,
        });
    }
    if spec.codec == Codec::TValue {
        let encoded = encode_tvalue_event(index, predefined, value);
        return Ok(match encoded {
            Ok(node) => Encoded::Node(node),
            Err(_) => Encoded::Refused,
        });
    }
    if spec.codec == Codec::MdList {
        let Some(prefix) = raw.as_list().and_then(|nodes| nodes.first()) else {
            return Ok(Encoded::Refused);
        };
        return Ok(match encode_mdlist(references, value, prefix) {
            Ok(node) => Encoded::Node(node),
            Err(_) => Encoded::Refused,
        });
    }
    if spec.codec == Codec::FoContent {
        let Some(prefix) = raw.as_list().and_then(|nodes| nodes.first()) else {
            return Ok(Encoded::Refused);
        };
        return Ok(match encode_focontent(references, value, prefix) {
            Ok(node) => Encoded::Node(node),
            Err(_) => Encoded::Refused,
        });
    }
    if spec.codec == Codec::Shortcut {
        let Some(prefix) = raw.as_list().and_then(|nodes| nodes.first()) else {
            return Ok(Encoded::Refused);
        };
        let schema = mdschema::schema().map_err(|_| ComparisonError::InvalidTypeContext)?;
        return Ok(match encode_shortcut(schema.ui(), value, prefix) {
            Ok(node) => Encoded::Node(node),
            Err(_) => Encoded::Refused,
        });
    }
    if spec.codec == Codec::Purposes {
        return Ok(match encode_purposes(&v.prop.name, value) {
            Ok(node) => Encoded::Node(node),
            Err(_) => Encoded::Refused,
        });
    }
    if spec.codec == Codec::Color {
        let schema = mdschema::schema().map_err(|_| ComparisonError::InvalidTypeContext)?;
        return Ok(match encode_color(schema.ui(), value) {
            Ok(node) => Encoded::Node(node),
            Err(_) => Encoded::Refused,
        });
    }
    if spec.codec == Codec::CmdGroup {
        let schema = mdschema::schema().map_err(|_| ComparisonError::InvalidTypeContext)?;
        return Ok(match encode_cmdgroup(schema.ui(), index, value) {
            Ok(node) => Encoded::Node(node),
            Err(_) => Encoded::Refused,
        });
    }
    if spec.codec == Codec::Picture {
        let schema = mdschema::schema().map_err(|_| ComparisonError::InvalidTypeContext)?;
        return Ok(
            match encode_picture(schema.ui(), index, &v.prop.name, value) {
                Ok(node) => Encoded::Node(node),
                Err(_) => Encoded::Refused,
            },
        );
    }
    if spec.codec == Codec::FieldList {
        let Some(owner) = v.owner else {
            return Ok(Encoded::Refused);
        };
        let Some(outer_prefix) = raw.as_list().and_then(|nodes| nodes.first()) else {
            return Ok(Encoded::Refused);
        };
        let Some(inner_prefix) = raw
            .as_list()
            .and_then(|nodes| nodes.get(1))
            .and_then(Node::as_list)
            .and_then(|nodes| nodes.first())
        else {
            return Ok(Encoded::Refused);
        };
        let schema = mdschema::schema().map_err(|_| ComparisonError::InvalidTypeContext)?;
        return Ok(
            match encode_fieldlist(schema, index, owner, value, outer_prefix, inner_prefix) {
                Ok(node) => Encoded::Node(node),
                Err(_) => Encoded::Refused,
            },
        );
    }
    if spec.codec == Codec::Characteristics {
        let crate::model::Value::Xml(root) = value else {
            return Ok(Encoded::Refused);
        };
        let Ok(prefixes) = CharacteristicsPrefixes::from_raw(raw, root.children.len()) else {
            return Ok(Encoded::Refused);
        };
        let schema = mdschema::schema().map_err(|_| ComparisonError::InvalidTypeContext)?;
        return Ok(
            match encode_characteristics_with_predefined(
                schema,
                index,
                predefined,
                &v.prop.name,
                value,
                &prefixes,
            ) {
                Ok(node) => Encoded::Node(node),
                Err(_) => Encoded::Refused,
            },
        );
    }
    if spec.codec == Codec::LinkByType {
        let Some(owner) = v.owner else {
            return Ok(Encoded::Refused);
        };
        let Some(nodes) = raw.as_list() else {
            return Ok(Encoded::Refused);
        };
        let Some(prefix) = nodes.first() else {
            return Ok(Encoded::Refused);
        };
        let empty_tail = match value {
            crate::model::Value::Xml(root) if root.children.is_empty() => nodes.get(2),
            _ => None,
        };
        let schema = mdschema::schema().map_err(|_| ComparisonError::InvalidTypeContext)?;
        return Ok(
            match encode_linkbytype(
                schema,
                index,
                owner,
                &v.prop.name,
                value,
                prefix,
                empty_tail,
            ) {
                Ok(node) => Encoded::Node(node),
                Err(_) => Encoded::Refused,
            },
        );
    }
    if spec.codec == Codec::CpLinks {
        let Some(owner) = v.owner else {
            return Ok(Encoded::Refused);
        };
        let Some(prefix) = raw.as_list().and_then(|nodes| nodes.first()) else {
            return Ok(Encoded::Refused);
        };
        let schema = mdschema::schema().map_err(|_| ComparisonError::InvalidTypeContext)?;
        return Ok(
            match encode_cplinks(schema, index, owner, &v.prop.name, value, prefix) {
                Ok(node) => Encoded::Node(node),
                Err(_) => Encoded::Refused,
            },
        );
    }
    if spec.codec == Codec::StdAttrs {
        let schema = mdschema::schema().map_err(|_| ComparisonError::InvalidTypeContext)?;
        return Ok(
            match encode_stdattrs_event_with(
                schema, spec, v.owner, index, references, predefined, value, raw,
            ) {
                Ok(node) => Encoded::Node(node),
                Err(_) => Encoded::Refused,
            },
        );
    }
    if spec.codec == Codec::StyleValue {
        let schema = mdschema::schema().map_err(|_| ComparisonError::InvalidTypeContext)?;
        if let crate::model::Value::Xml(root) = value
            && root
                .attrs
                .iter()
                .any(|(k, value)| k == "xsi:type" && value == "v8ui:Border")
        {
            let Ok(prefixes) =
                StyleBorderPrefixes::from_raw(raw, root.attrs.iter().any(|(k, _)| k == "width"))
            else {
                return Ok(Encoded::Refused);
            };
            return Ok(
                match encode_styleborder(schema.ui_tables(), &v.prop.name, value, &prefixes) {
                    Ok(node) => Encoded::Node(node),
                    Err(_) => Encoded::Refused,
                },
            );
        }
        if let crate::model::Value::Xml(root) = value
            && root
                .attrs
                .iter()
                .any(|(k, value)| k == "xsi:type" && value == "v8ui:Font")
        {
            if root
                .attrs
                .iter()
                .any(|(k, value)| k == "kind" && value == "Absolute")
            {
                let Ok(prefixes) = AbsoluteFontPrefixes::from_raw(raw) else {
                    return Ok(Encoded::Refused);
                };
                return Ok(
                    match encode_stylefont_absolute(&v.prop.name, value, &prefixes) {
                        Ok(node) => Encoded::Node(node),
                        Err(_) => Encoded::Refused,
                    },
                );
            }
            let Ok(prefixes) = StyleFontPrefixes::from_raw(raw, &v.prop.name, value) else {
                return Ok(Encoded::Refused);
            };
            return Ok(
                match encode_stylefont(schema.ui_tables(), &v.prop.name, value, &prefixes) {
                    Ok(node) => Encoded::Node(node),
                    Err(_) => Encoded::Refused,
                },
            );
        }
        let selected = raw.as_list().and_then(|l| Some((l.first()?, l.get(2)?)));
        return Ok(match selected {
            Some((prefix0, prefix2)) => match encode_stylevalue(
                schema.ui(),
                schema.ui_tables(),
                &v.prop.name,
                value,
                prefix0,
                prefix2,
            ) {
                Ok(node) => Encoded::Node(node),
                Err(_) => Encoded::Refused,
            },
            None => Encoded::Refused,
        });
    }
    if spec.codec == Codec::XdtoPackages {
        let selected = XdtoPackagesPrefixes::from_raw(raw);
        return Ok(match selected {
            Some(prefixes) => match encode_xdtopackages(index, &v.prop.name, value, prefixes) {
                Ok(node) => Encoded::Node(node),
                Err(_) => Encoded::Refused,
            },
            None => Encoded::Refused,
        });
    }
    if spec.codec == Codec::XdtoType {
        return Ok(
            if let Some(prefix) = raw.as_list().and_then(|l| l.first()) {
                match encode_xdtotype(&v.prop.name, value, prefix) {
                    Ok(node) => Encoded::Node(node),
                    Err(_) => Encoded::Refused,
                }
            } else {
                Encoded::Refused
            },
        );
    }
    if spec.codec == Codec::Unknown
        && v.prop.name == super::overlay::STS
        && let Some(owner) = v.owner
    {
        let schema = mdschema::schema().map_err(|_| ComparisonError::InvalidTypeContext)?;
        return Ok(
            match super::encode_std_tabular::encode_std_tabular(
                schema, index, predefined, references, owner, value, raw,
            ) {
                Ok(node) => Encoded::Node(node),
                Err(_) => Encoded::Refused,
            },
        );
    }
    if spec.codec == Codec::CParams {
        return Ok(
            if let Some(prefix) = raw.as_list().and_then(|l| l.first()) {
                match encode_cparams_with_predefined(index, predefined, &v.prop.name, value, prefix)
                {
                    Ok(node) => Encoded::Node(node),
                    Err(_) => Encoded::Refused,
                }
            } else {
                Encoded::Refused
            },
        );
    }
    if spec.codec == Codec::CaContent {
        let Some(prefix) = raw.as_list().and_then(|nodes| nodes.first()) else {
            return Ok(Encoded::Refused);
        };
        return Ok(match encode_cacontent(index, &v.prop.name, value, prefix) {
            Ok(node) => Encoded::Node(node),
            Err(_) => Encoded::Refused,
        });
    }
    Ok(match encode_scalar(spec, value, Some(raw)) {
        Ok(node) => Encoded::Node(node),
        Err(ScalarEncodeError::NoInverse(_)) => Encoded::NoInverse,
        Err(ScalarEncodeError::BadValue(_)) => Encoded::Refused,
        Err(ScalarEncodeError::EventTable(error)) => {
            return Err(ComparisonError::EventTable(error));
        }
    })
}

impl ScalarComparisonReport {
    fn new() -> Self {
        Self {
            observation: Observation::EmptyObservation,
            total: 0,
            original: Counts::default(),
            normalized: Counts::default(),
            diagnostics: Diagnostics::default(),
        }
    }

    fn record(&mut self, domain: TreeDomain, outcome: Outcome) -> Result<()> {
        let counts = match domain {
            TreeDomain::Original => &mut self.original,
            TreeDomain::Normalized => &mut self.normalized,
        };
        let add = |n: u64| n.checked_add(1).ok_or(ComparisonError::CounterOverflow);
        let total = add(self.total)?;
        let domain_total = add(counts.total)?;
        let slot = counts
            .outcomes
            .get_mut(outcome as usize)
            .ok_or(ComparisonError::CounterInvariant)?;
        let count = add(*slot)?;
        self.total = total;
        counts.total = domain_total;
        *slot = count;
        self.observation = Observation::Observed;
        Ok(())
    }
}

#[derive(Default)]
struct Collector {
    report: ScalarComparisonReport,
    fatal: Option<ComparisonError>,
}

impl Default for ScalarComparisonReport {
    fn default() -> Self {
        Self::new()
    }
}

impl Collector {
    fn accept(&mut self, domain: TreeDomain, outcome: Result<Outcome>) {
        if self.fatal.is_none()
            && let Err(error) = outcome.and_then(|outcome| self.report.record(domain, outcome))
        {
            self.fatal = Some(error);
        }
    }

    fn finish(mut self, diagnostics: Diagnostics) -> Result<ScalarComparisonReport> {
        if let Some(error) = self.fatal {
            return Err(error);
        }
        self.report.diagnostics = diagnostics;
        Ok(self.report)
    }
}

pub(crate) fn compare_replayed_scalars(
    cfg: &Configuration,
    entries: &Entries,
) -> Result<ScalarComparisonReport> {
    let types = mdschema::schema()
        .map_err(|_| ComparisonError::InvalidTypeContext)
        .and_then(|schema| {
            TypeContext::new(&cfg.index, schema).map_err(|_| ComparisonError::InvalidTypeContext)
        });
    compare_with_type_context(cfg, entries, types)
}

fn compare_with_type_context(
    cfg: &Configuration,
    entries: &Entries,
    types: Result<TypeContext>,
) -> Result<ScalarComparisonReport> {
    let references =
        ReferenceContext::new(&cfg.index).map_err(|_| ComparisonError::AmbiguousReferenceIndex)?;
    let types = types?;
    let tables = crate::predefined::predefined_tables(entries, &cfg.index);
    let predefined = PredefinedContext::new(&cfg.predef, &tables);
    let mut collector = Collector::default();
    let diagnostics = decode::visit_properties(cfg, entries, &mut |v| {
        if collector.fatal.is_none() {
            collector.accept(
                v.domain,
                classify_with(&v, &references, &types, &cfg.index, &predefined),
            );
        }
    })
    .map_err(ComparisonError::Replay)?;
    collector.finish(diagnostics)
}
