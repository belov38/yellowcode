pub(crate) mod bags;
pub mod codecs;
#[allow(
    dead_code,
    reason = "C7 internal comparison awaits separately reviewed public integration"
)]
pub(crate) mod compare_scalar;
pub mod decode;
#[allow(
    dead_code,
    reason = "C7 partial CaContent backend awaits separately reviewed integration"
)]
pub(crate) mod encode_cacontent;
#[allow(
    dead_code,
    reason = "C7 partial CmdGroup backend awaits separately reviewed integration"
)]
pub(crate) mod encode_cmdgroup;
#[allow(
    dead_code,
    reason = "C7 partial object Color backend awaits separately reviewed integration"
)]
pub(crate) mod encode_color;
#[allow(
    dead_code,
    reason = "C7 partial CParams backend awaits separately reviewed consumer integration"
)]
pub(crate) mod encode_cparams;
#[allow(
    dead_code,
    reason = "Partial inverse backend; live consumer requires separate review"
)]
pub(crate) mod encode_enum_dtref;
#[allow(
    dead_code,
    reason = "C7 partial FieldList backend awaits separately reviewed integration"
)]
pub(crate) mod encode_fieldlist;
#[allow(
    dead_code,
    reason = "C7 partial FoContent backend awaits separately reviewed integration"
)]
pub(crate) mod encode_focontent;
#[allow(
    dead_code,
    reason = "C7 partial MdList backend awaits separately reviewed integration"
)]
pub(crate) mod encode_mdlist;
#[allow(
    dead_code,
    reason = "C7 partial Method backend awaits separately reviewed integration"
)]
pub(crate) mod encode_method;
#[allow(
    dead_code,
    reason = "C7 named EmptyRef backend awaits separately reviewed integration"
)]
pub(crate) mod encode_named_emptyref;
#[allow(
    dead_code,
    reason = "C7 partial Picture backend awaits separately reviewed integration"
)]
pub(crate) mod encode_picture;
pub(crate) mod encode_predefined_dtref;
#[allow(
    dead_code,
    reason = "C7 partial Purposes backend awaits separately reviewed integration"
)]
pub(crate) mod encode_purposes;
#[allow(
    dead_code,
    reason = "C7 reference backend awaits separately reviewed integration"
)]
pub(crate) mod encode_reference;
#[allow(
    dead_code,
    reason = "C7 scalar backend awaits separately reviewed observer integration"
)]
pub(crate) mod encode_scalar;
#[allow(
    dead_code,
    reason = "C7 partial Shortcut backend awaits separately reviewed integration"
)]
pub(crate) mod encode_shortcut;
#[allow(
    dead_code,
    reason = "C7 partial Type backend awaits separately reviewed integration"
)]
pub(crate) mod encode_type;
#[allow(
    dead_code,
    reason = "C7 exact Type context backend awaits separately reviewed consumer integration"
)]
pub(crate) mod encode_type_context;
#[allow(
    dead_code,
    reason = "C7 partial typed-value backend awaits separately reviewed consumer integration"
)]
pub(crate) mod encode_typed;
#[allow(
    dead_code,
    reason = "C7 partial XdtoPackages awaits reviewed diagnostic integration"
)]
pub(crate) mod encode_xdtopackages;
#[allow(
    dead_code,
    reason = "C7 partial XdtoType backend awaits reviewed diagnostic integration"
)]
pub(crate) mod encode_xdtotype;
pub(crate) mod name_lookup;
pub mod overlay;
#[allow(
    dead_code,
    reason = "the edit-mode patch API awaits build_edit (C7 slice R); the check uses part of it"
)]
pub(crate) mod patch;
pub mod path;
pub(crate) mod recalc;
pub mod types;
pub(crate) use md_xmlout::codec::ui;

#[allow(
    dead_code,
    reason = "C7 partial StyleValue awaits reviewed diagnostic integration"
)]
pub(crate) mod encode_stylevalue;

#[allow(
    dead_code,
    reason = "C7 partial CpLinks awaits reviewed diagnostic integration"
)]
pub(crate) mod encode_cplinks;

#[allow(
    dead_code,
    reason = "C7 partial LinkByType awaits reviewed diagnostic integration"
)]
pub(crate) mod encode_linkbytype;

#[allow(
    dead_code,
    reason = "C7 partial Characteristics awaits reviewed diagnostic integration"
)]
pub(crate) mod encode_characteristics;

#[allow(
    dead_code,
    reason = "C7 partial StyleValue Border awaits reviewed integration"
)]
pub(crate) mod encode_styleborder;

pub(crate) mod encode_stylefont;

pub(crate) mod encode_stylefont_absolute;

pub(crate) mod encode_std_tabular;
#[allow(
    dead_code,
    reason = "C7 partial StdAttrs backend awaits separately reviewed consumer"
)]
pub(crate) mod encode_stdattrs;
