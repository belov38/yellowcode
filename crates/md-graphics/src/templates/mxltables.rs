use brace::Node;

pub(crate) const H_ALIGN: &[(&str, &str)] = &[
    ("0", "Left"),
    ("2", "Right"),
    ("6", "Center"),
    ("4", "Justify"),
    ("5", "Auto"),
];
pub(crate) const V_ALIGN: &[(&str, &str)] = &[("0", "Top"), ("8", "Bottom"), ("24", "Center")];

#[derive(Debug, Clone, Copy)]
pub(crate) enum Codec {
    Font,
    Color,
    Int,
    Bool,
    NBool,
    VType,
    FStr,
    Uuid,
    Mask,
    Pattern,
    Map(&'static [(&'static str, &'static str)]),
}

pub(crate) const PICTURE_SIZE: [&str; 8] = [
    "RealSize",
    "Stretch",
    "Proportionally",
    "Tile",
    "AutoSize",
    "RealSizeIgnoreScale",
    "AutoSizeIgnoreScale",
    "ByFontSize",
];
const PICTURE_SIZE_MAP: &[(&str, &str)] = &[
    ("0", "RealSize"),
    ("1", "Stretch"),
    ("2", "Proportionally"),
    ("3", "Tile"),
    ("4", "AutoSize"),
    ("5", "RealSizeIgnoreScale"),
    ("6", "AutoSizeIgnoreScale"),
    ("7", "ByFontSize"),
];

pub(crate) const FORMAT_BITS: &[(u32, &str, Codec)] = &[
    (0, "font", Codec::Font),
    (5, "borderColor", Codec::Color),
    (6, "height", Codec::Int),
    (7, "width", Codec::Int),
    (8, "horizontalAlignment", Codec::Map(H_ALIGN)),
    (9, "verticalAlignment", Codec::Map(V_ALIGN)),
    (10, "textColor", Codec::Color),
    (11, "backColor", Codec::Color),
    (12, "pattern", Codec::Pattern),
    (13, "patternColor", Codec::Color),
    (
        14,
        "textPlacement",
        Codec::Map(&[("0", "Auto"), ("1", "Cut"), ("2", "Block"), ("3", "Wrap")]),
    ),
    (
        15,
        "fillType",
        Codec::Map(&[("0", "Text"), ("1", "Parameter"), ("2", "Template")]),
    ),
    (16, "protection", Codec::NBool),
    (17, "hidden", Codec::Bool),
    (18, "textOrientation", Codec::Int),
    (
        19,
        "detailsUse",
        Codec::Map(&[("0", "Cell"), ("1", "Row"), ("2", "WithoutProcessing")]),
    ),
    (20, "bySelectedColumns", Codec::Bool),
    (21, "markNegatives", Codec::Bool),
    (22, "containsValue", Codec::Bool),
    (23, "valueType", Codec::VType),
    (24, "format", Codec::FStr),
    (25, "controlType", Codec::Uuid),
    (26, "hyperLink", Codec::Bool),
    (27, "quickDrag", Codec::Bool),
    (28, "autoMarkIncomplete", Codec::Bool),
    (29, "markIncomplete", Codec::Bool),
    (30, "indent", Codec::Int),
    (31, "autoIndent", Codec::Int),
    (32, "editFormat", Codec::FStr),
    (
        33,
        "columnSizeChange",
        Codec::Map(&[("0", "Normal"), ("1", "QuickChange")]),
    ),
    (34, "mask", Codec::Mask),
    (35, "picIndex", Codec::Int),
    (36, "picHorizontalAlignment", Codec::Map(H_ALIGN)),
    (37, "picVerticalAlignment", Codec::Map(V_ALIGN)),
    (38, "pictureSizeMode", Codec::Map(PICTURE_SIZE_MAP)),
    (
        39,
        "textPosition",
        Codec::Map(&[
            ("0", "Left"),
            ("1", "Right"),
            ("2", "Top"),
            ("3", "Bottom"),
            ("4", "OnTop"),
            ("5", "Auto"),
        ]),
    ),
    (40, "autoWidthCalculation", Codec::Bool),
    (41, "widthWeightFactor", Codec::Int),
    (42, "leftMargin", Codec::Int),
    (43, "topMargin", Codec::Int),
    (44, "rightMargin", Codec::Int),
    (45, "bottomMargin", Codec::Int),
    (
        46,
        "highlightMode",
        Codec::Map(&[
            ("0", "Auto"),
            ("1", "Cell"),
            ("2", "Row"),
            ("3", "DontHighlight"),
        ]),
    ),
    (
        47,
        "showEditMode",
        Codec::Map(&[("0", "Auto"), ("1", "AsInputField"), ("2", "Regular")]),
    ),
];

pub(crate) fn format_bit(bit: u32) -> Option<(&'static str, Codec)> {
    FORMAT_BITS
        .iter()
        .find(|(b, _, _)| *b == bit)
        .map(|(_, t, c)| (*t, *c))
}

pub(crate) fn is_known_bit(bit: u32) -> bool {
    (1..=4).contains(&bit) || format_bit(bit).is_some()
}

pub(crate) fn map_value_ref(codec: Codec, key: &str) -> Option<&'static str> {
    const PATTERNS: &[(&str, &str)] = &[
        ("0", "Solid"),
        ("255", "WithoutPattern"),
        ("1", "Pattern1"),
        ("2", "Pattern2"),
        ("3", "Pattern3"),
        ("4", "Pattern4"),
        ("5", "Pattern5"),
        ("6", "Pattern6"),
        ("7", "Pattern7"),
        ("8", "Pattern8"),
        ("9", "Pattern9"),
        ("10", "Pattern10"),
        ("11", "Pattern11"),
        ("12", "Pattern12"),
        ("13", "Pattern13"),
        ("14", "Pattern14"),
        ("15", "Pattern15"),
        ("16", "Pattern16"),
        ("17", "Pattern17"),
    ];
    let values = match codec {
        Codec::Map(values) => values,
        Codec::Pattern => PATTERNS,
        _ => return None,
    };
    values
        .iter()
        .find(|(value, _)| *value == key)
        .map(|(_, text)| *text)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FormatSlot {
    Bit(u32),
    Borders,
}

pub(crate) const FORMAT_ORDER: &[FormatSlot] = {
    use FormatSlot::{Bit, Borders};
    &[
        Bit(0),
        Borders,
        Bit(6),
        Bit(5),
        Bit(7),
        Bit(40),
        Bit(41),
        Bit(8),
        Bit(9),
        Bit(10),
        Bit(11),
        Bit(13),
        Bit(12),
        Bit(14),
        Bit(15),
        Bit(16),
        Bit(17),
        Bit(18),
        Bit(19),
        Bit(20),
        Bit(21),
        Bit(22),
        Bit(23),
        Bit(24),
        Bit(25),
        Bit(26),
        Bit(27),
        Bit(28),
        Bit(29),
        Bit(30),
        Bit(31),
        Bit(32),
        Bit(33),
        Bit(34),
        Bit(35),
        Bit(38),
        Bit(36),
        Bit(37),
        Bit(39),
        Bit(42),
        Bit(43),
        Bit(44),
        Bit(45),
        Bit(46),
        Bit(47),
    ]
};

pub(crate) fn view_default_ref() -> &'static [&'static str] {
    &[
        "3", "0", "0", "100", "1", "1", "0", "1", "1", "0", "0", "0", "0", "0", "0", "0", "0", "0",
        "0", "0", "0", "", "0", "0", "0", "0", "0", "0", "0",
    ]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ViewKind {
    B,
    N,
    S,
}

pub(crate) const VIEW_FIELDS: &[(usize, &str, ViewKind)] = &[
    (1, "currentColumn", ViewKind::N),
    (2, "currentRow", ViewKind::N),
    (3, "scale", ViewKind::N),
    (4, "showGrid", ViewKind::B),
    (5, "showHeaders", ViewKind::B),
    (6, "showNames", ViewKind::B),
    (7, "showGroups", ViewKind::B),
    (8, "showGroupsText", ViewKind::B),
    (9, "showPageBreaks", ViewKind::B),
    (10, "blackAndWhite", ViewKind::B),
    (13, "fixationPointColumn", ViewKind::N),
    (14, "fixationPointRow", ViewKind::N),
    (15, "topFixationPointColumn", ViewKind::N),
    (16, "topFixationPointRow", ViewKind::N),
    (17, "screenBeginPointColumn", ViewKind::N),
    (18, "screenBeginPointRow", ViewKind::N),
    (19, "showNamedRowsAndColumns", ViewKind::B),
    (20, "showNamedRowsAndColumnsText", ViewKind::B),
    (21, "currentLanguage", ViewKind::S),
    (22, "showComments", ViewKind::B),
];

pub(crate) const DRAWING_TYPES: &[(i64, &str)] = &[
    (1, "Line"),
    (2, "Rectangle"),
    (3, "Text"),
    (5, "Picture"),
    (6, "Comment"),
    (7, "Ellipse"),
    (10, "GanttChart"),
];
pub(crate) const AREA_TYPES: &[(i64, &str)] =
    &[(0, "Table"), (1, "Rows"), (2, "Columns"), (3, "Rectangle")];

#[derive(Debug, Clone, Copy)]
pub(crate) enum PrintCodec {
    N,
    B,
    Orientation,
    Map(&'static [(&'static str, &'static str)]),
    Skip,
}

pub(crate) const PRINT_KEYS: &[(usize, &str, PrintCodec)] = &[
    (1, "pageOrientation", PrintCodec::Orientation),
    (2, "scale", PrintCodec::N),
    (3, "collate", PrintCodec::B),
    (4, "copies", PrintCodec::N),
    (5, "perPage", PrintCodec::N),
    (6, "topMargin", PrintCodec::N),
    (7, "leftMargin", PrintCodec::N),
    (8, "bottomMargin", PrintCodec::N),
    (9, "rightMargin", PrintCodec::N),
    (10, "headerSize", PrintCodec::N),
    (11, "footerSize", PrintCodec::N),
    (12, "fitToPage", PrintCodec::B),
    (13, "blackAndWhite", PrintCodec::B),
    (14, "printerName", PrintCodec::N),
    (0, "", PrintCodec::Skip),
    (15, "paperSource", PrintCodec::N),
    (16, "pageWidth", PrintCodec::N),
    (17, "pageHeight", PrintCodec::N),
    (
        18,
        "printAccuracy",
        PrintCodec::Map(&[("0", "Auto"), ("1", "Accurate")]),
    ),
    (
        19,
        "duplexType",
        PrintCodec::Map(&[
            ("1", "None"),
            ("2", "FlipPagesLeft"),
            ("3", "FlipPagesUp"),
            ("4", "UsePrinterSettings"),
        ]),
    ),
    (
        20,
        "pagePlacementAlternation",
        PrintCodec::Map(&[
            ("0", "Auto"),
            ("1", "DontUse"),
            ("2", "MirrorOnLeft"),
            ("3", "MirrorOnTop"),
        ]),
    ),
    (21, "firstPageNumber", PrintCodec::N),
];

pub(crate) fn print_value_ref(codec: PrintCodec, value: &str) -> Option<&str> {
    match codec {
        PrintCodec::N => Some(value),
        PrintCodec::B => Some(if value == "1" { "true" } else { "false" }),
        PrintCodec::Orientation => Some(match value {
            "1" => "Portrait",
            "2" => "Landscape",
            other => other,
        }),
        PrintCodec::Map(values) => values
            .iter()
            .find(|(key, _)| *key == value)
            .map(|(_, text)| *text),
        PrintCodec::Skip => None,
    }
}

#[derive(Clone, Copy)]
pub(crate) struct FormatValues<'a> {
    mask: u64,
    values: &'a [Node],
}
impl<'a> FormatValues<'a> {
    pub(crate) fn read(
        values: &'a [Node],
        read: crate::graph::read::Read<'_>,
    ) -> crate::graph::read::R<Self> {
        let mask = read
            .at(values, 0)?
            .text()
            .ok_or_else(|| read.invalid("not a scalar"))?
            .trim();
        let mask = mask
            .parse::<u64>()
            .or_else(|_| mask.parse::<i64>().map(|value| value as u64))
            .map_err(|_| read.invalid("not an integer"))?;
        let len = usize::try_from(mask.count_ones()).map_err(|_| read.invalid("format mask"))?;
        let expected = len
            .checked_add(1)
            .ok_or(memory_budget::BudgetError::Overflow)?;
        if values.len() < expected {
            let index = values.len();
            return Err(read.semantic("index out of range", || {
                format!("index {index} out of range")
            }));
        }
        if values.len() != expected {
            return Err(read.semantic("format length", || {
                format!("format length {} != {expected}", values.len())
            }));
        }
        Ok(Self {
            mask,
            values: values.get(1..).unwrap_or_default(),
        })
    }
    pub(crate) fn iter(self) -> impl Iterator<Item = (u32, &'a Node)> + Clone {
        (0..64u32)
            .filter(move |bit| self.mask >> bit & 1 == 1)
            .zip(self.values.iter())
    }
    pub(crate) fn get(self, bit: u32) -> Option<&'a Node> {
        self.iter()
            .find(|(current, _)| *current == bit)
            .map(|(_, value)| value)
    }
}
