use crate::lang::ScriptVariant;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Kw {
    Select,
    Allowed,
    Distinct,
    Top,
    Into,
    Add,
    From,
    Where,
    Group,
    By,
    On,
    Grouping,
    Sets,
    Having,
    For,
    Update,
    Of,
    Index,
    Unique,
    Union,
    All,
    Order,
    Asc,
    Desc,
    Hierarchy,
    InHierarchy,
    Only,
    AutoOrder,
    Totals,
    Overall,
    Periods,
    Drop,
    As,
    Join,
    Inner,
    Left,
    Right,
    Full,
    Outer,
    And,
    Or,
    Not,
    In,
    Between,
    Is,
    Null,
    Refs,
    Like,
    Escape,
    Case,
    When,
    Then,
    Else,
    End,
    Cast,
    Boolean,
    Number,
    String,
    Date,
    DateTime,
    Type,
    Value,
    True,
    False,
    Undefined,
    EmptyTable,
    RecordAutoNumber,
    Substring,
    Year,
    Quarter,
    Month,
    DayOfYear,
    Day,
    Week,
    WeekDay,
    Hour,
    Minute,
    Second,
    TenDays,
    HalfYear,
    BeginOfPeriod,
    EndOfPeriod,
    DateAdd,
    DateDiff,
    Count,
    Sum,
    Avg,
    Min,
    Max,
    IsNull,
    Presentation,
    RefPresentation,
    ValueType,
    GroupedBy,
    Uuid,
    Characteristics,
    CharacteristicTypes,
    KeyField,
    NameField,
    ValueTypeField,
    CharacteristicValues,
    ObjectField,
    TypeField,
    ValueField,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Func {
    Substring,
    String,
    StringLength,
    TrimL,
    TrimR,
    TrimAll,
    Left,
    Right,
    StrFind,
    Upper,
    Lower,
    StrReplace,
    Year,
    Quarter,
    Month,
    DayOfYear,
    Day,
    Week,
    WeekDay,
    Hour,
    Minute,
    Second,
    BeginOfPeriod,
    EndOfPeriod,
    DateAdd,
    DateDiff,
    RecordAutoNumber,
    ACos,
    ASin,
    ATan,
    Cos,
    Sin,
    Exp,
    Log,
    Log10,
    Pow,
    Sqrt,
    Round,
    Int,
    Sum,
    Min,
    Max,
    Avg,
    Count,
    IsNull,
    Presentation,
    RefPresentation,
    ValueType,
    GroupedBy,
    StoredDataSize,
    Uuid,
}

const KW: [(Kw, &str, &str); 104] = [
    (Kw::Select, "ВЫБРАТЬ", "SELECT"),
    (Kw::Allowed, "РАЗРЕШЕННЫЕ", "ALLOWED"),
    (Kw::Distinct, "РАЗЛИЧНЫЕ", "DISTINCT"),
    (Kw::Top, "ПЕРВЫЕ", "TOP"),
    (Kw::Into, "ПОМЕСТИТЬ", "INTO"),
    (Kw::Add, "ДОБАВИТЬ", "ADD"),
    (Kw::From, "ИЗ", "FROM"),
    (Kw::Where, "ГДЕ", "WHERE"),
    (Kw::Group, "СГРУППИРОВАТЬ", "GROUP"),
    (Kw::By, "ПО", "BY"),
    (Kw::On, "ПО", "ON"),
    (Kw::Grouping, "ГРУППИРУЮЩИМ", "GROUPING"),
    (Kw::Sets, "НАБОРАМ", "SETS"),
    (Kw::Having, "ИМЕЮЩИЕ", "HAVING"),
    (Kw::For, "ДЛЯ", "FOR"),
    (Kw::Update, "ИЗМЕНЕНИЯ", "UPDATE"),
    (Kw::Of, "", "OF"),
    (Kw::Index, "ИНДЕКСИРОВАТЬ", "INDEX"),
    (Kw::Unique, "УНИКАЛЬНО", "UNIQUE"),
    (Kw::Union, "ОБЪЕДИНИТЬ", "UNION"),
    (Kw::All, "ВСЕ", "ALL"),
    (Kw::Order, "УПОРЯДОЧИТЬ", "ORDER"),
    (Kw::Asc, "ВОЗР", "ASC"),
    (Kw::Desc, "УБЫВ", "DESC"),
    (Kw::Hierarchy, "ИЕРАРХИЯ", "HIERARCHY"),
    (Kw::InHierarchy, "ИЕРАРХИИ", "HIERARCHY"),
    (Kw::Only, "ТОЛЬКО", "ONLY"),
    (Kw::AutoOrder, "АВТОУПОРЯДОЧИВАНИЕ", "AUTOORDER"),
    (Kw::Totals, "ИТОГИ", "TOTALS"),
    (Kw::Overall, "ОБЩИЕ", "OVERALL"),
    (Kw::Periods, "ПЕРИОДАМИ", "PERIODS"),
    (Kw::Drop, "УНИЧТОЖИТЬ", "DROP"),
    (Kw::As, "КАК", "AS"),
    (Kw::Join, "СОЕДИНЕНИЕ", "JOIN"),
    (Kw::Inner, "ВНУТРЕННЕЕ", "INNER"),
    (Kw::Left, "ЛЕВОЕ", "LEFT"),
    (Kw::Right, "ПРАВОЕ", "RIGHT"),
    (Kw::Full, "ПОЛНОЕ", "FULL"),
    (Kw::Outer, "ВНЕШНЕЕ", "OUTER"),
    (Kw::And, "И", "AND"),
    (Kw::Or, "ИЛИ", "OR"),
    (Kw::Not, "НЕ", "NOT"),
    (Kw::In, "В", "IN"),
    (Kw::Between, "МЕЖДУ", "BETWEEN"),
    (Kw::Is, "ЕСТЬ", "IS"),
    (Kw::Null, "NULL", "NULL"),
    (Kw::Refs, "ССЫЛКА", "REFS"),
    (Kw::Like, "ПОДОБНО", "LIKE"),
    (Kw::Escape, "СПЕЦСИМВОЛ", "ESCAPE"),
    (Kw::Case, "ВЫБОР", "CASE"),
    (Kw::When, "КОГДА", "WHEN"),
    (Kw::Then, "ТОГДА", "THEN"),
    (Kw::Else, "ИНАЧЕ", "ELSE"),
    (Kw::End, "КОНЕЦ", "END"),
    (Kw::Cast, "ВЫРАЗИТЬ", "CAST"),
    (Kw::Boolean, "БУЛЕВО", "BOOLEAN"),
    (Kw::Number, "ЧИСЛО", "NUMBER"),
    (Kw::String, "СТРОКА", "STRING"),
    (Kw::Date, "ДАТА", "DATE"),
    (Kw::DateTime, "ДАТАВРЕМЯ", "DATETIME"),
    (Kw::Type, "ТИП", "TYPE"),
    (Kw::Value, "ЗНАЧЕНИЕ", "VALUE"),
    (Kw::True, "ИСТИНА", "TRUE"),
    (Kw::False, "ЛОЖЬ", "FALSE"),
    (Kw::Undefined, "НЕОПРЕДЕЛЕНО", "UNDEFINED"),
    (Kw::EmptyTable, "ПУСТАЯТАБЛИЦА", "EMPTYTABLE"),
    (Kw::RecordAutoNumber, "АВТОНОМЕРЗАПИСИ", "RECORDAUTONUMBER"),
    (Kw::Substring, "ПОДСТРОКА", "SUBSTRING"),
    (Kw::Year, "ГОД", "YEAR"),
    (Kw::Quarter, "КВАРТАЛ", "QUARTER"),
    (Kw::Month, "МЕСЯЦ", "MONTH"),
    (Kw::DayOfYear, "ДЕНЬГОДА", "DAYOFYEAR"),
    (Kw::Day, "ДЕНЬ", "DAY"),
    (Kw::Week, "НЕДЕЛЯ", "WEEK"),
    (Kw::WeekDay, "ДЕНЬНЕДЕЛИ", "WEEKDAY"),
    (Kw::Hour, "ЧАС", "HOUR"),
    (Kw::Minute, "МИНУТА", "MINUTE"),
    (Kw::Second, "СЕКУНДА", "SECOND"),
    (Kw::TenDays, "ДЕКАДА", "TENDAYS"),
    (Kw::HalfYear, "ПОЛУГОДИЕ", "HALFYEAR"),
    (Kw::BeginOfPeriod, "НАЧАЛОПЕРИОДА", "BEGINOFPERIOD"),
    (Kw::EndOfPeriod, "КОНЕЦПЕРИОДА", "ENDOFPERIOD"),
    (Kw::DateAdd, "ДОБАВИТЬКДАТЕ", "DATEADD"),
    (Kw::DateDiff, "РАЗНОСТЬДАТ", "DATEDIFF"),
    (Kw::Count, "КОЛИЧЕСТВО", "COUNT"),
    (Kw::Sum, "СУММА", "SUM"),
    (Kw::Avg, "СРЕДНЕЕ", "AVG"),
    (Kw::Min, "МИНИМУМ", "MIN"),
    (Kw::Max, "МАКСИМУМ", "MAX"),
    (Kw::IsNull, "ЕСТЬNULL", "ISNULL"),
    (Kw::Presentation, "ПРЕДСТАВЛЕНИЕ", "PRESENTATION"),
    (
        Kw::RefPresentation,
        "ПРЕДСТАВЛЕНИЕССЫЛКИ",
        "REFPRESENTATION",
    ),
    (Kw::ValueType, "ТИПЗНАЧЕНИЯ", "VALUETYPE"),
    (Kw::GroupedBy, "СГРУППИРОВАНОПО", "GROUPEDBY"),
    (Kw::Uuid, "УНИКАЛЬНЫЙИДЕНТИФИКАТОР", "UUID"),
    (Kw::Characteristics, "ХАРАКТЕРИСТИКИ", "CHARACTERISTICS"),
    (
        Kw::CharacteristicTypes,
        "ВИДЫХАРАКТЕРИСТИК",
        "CHARACTERISTICTYPES",
    ),
    (Kw::KeyField, "ПОЛЕКЛЮЧА", "KEYFIELD"),
    (Kw::NameField, "ПОЛЕИМЕНИ", "NAMEFIELD"),
    (Kw::ValueTypeField, "ПОЛЕТИПАЗНАЧЕНИЯ", "VALUETYPEFIELD"),
    (
        Kw::CharacteristicValues,
        "ЗНАЧЕНИЯХАРАКТЕРИСТИК",
        "CHARACTERISTICVALUES",
    ),
    (Kw::ObjectField, "ПОЛЕОБЪЕКТА", "OBJECTFIELD"),
    (Kw::TypeField, "ПОЛЕВИДА", "TYPEFIELD"),
    (Kw::ValueField, "ПОЛЕЗНАЧЕНИЯ", "VALUEFIELD"),
];

const FUNCS: [(Func, &str, &str); 51] = [
    (Func::Substring, "ПОДСТРОКА", "SUBSTRING"),
    (Func::String, "СТРОКА", "STRING"),
    (Func::StringLength, "ДЛИНАСТРОКИ", "STRINGLENGTH"),
    (Func::TrimL, "СОКРЛ", "TRIML"),
    (Func::TrimR, "СОКРП", "TRIMR"),
    (Func::TrimAll, "СОКРЛП", "TRIMALL"),
    (Func::Left, "ЛЕВ", "LEFT"),
    (Func::Right, "ПРАВ", "RIGHT"),
    (Func::StrFind, "СТРНАЙТИ", "STRFIND"),
    (Func::Upper, "ВРЕГ", "UPPER"),
    (Func::Lower, "НРЕГ", "LOWER"),
    (Func::StrReplace, "СТРЗАМЕНИТЬ", "STRREPLACE"),
    (Func::Year, "ГОД", "YEAR"),
    (Func::Quarter, "КВАРТАЛ", "QUARTER"),
    (Func::Month, "МЕСЯЦ", "MONTH"),
    (Func::DayOfYear, "ДЕНЬГОДА", "DAYOFYEAR"),
    (Func::Day, "ДЕНЬ", "DAY"),
    (Func::Week, "НЕДЕЛЯ", "WEEK"),
    (Func::WeekDay, "ДЕНЬНЕДЕЛИ", "WEEKDAY"),
    (Func::Hour, "ЧАС", "HOUR"),
    (Func::Minute, "МИНУТА", "MINUTE"),
    (Func::Second, "СЕКУНДА", "SECOND"),
    (Func::BeginOfPeriod, "НАЧАЛОПЕРИОДА", "BEGINOFPERIOD"),
    (Func::EndOfPeriod, "КОНЕЦПЕРИОДА", "ENDOFPERIOD"),
    (Func::DateAdd, "ДОБАВИТЬКДАТЕ", "DATEADD"),
    (Func::DateDiff, "РАЗНОСТЬДАТ", "DATEDIFF"),
    (
        Func::RecordAutoNumber,
        "АВТОНОМЕРЗАПИСИ",
        "RECORDAUTONUMBER",
    ),
    (Func::ACos, "ACOS", "ACOS"),
    (Func::ASin, "ASIN", "ASIN"),
    (Func::ATan, "ATAN", "ATAN"),
    (Func::Cos, "COS", "COS"),
    (Func::Sin, "SIN", "SIN"),
    (Func::Exp, "EXP", "EXP"),
    (Func::Log, "LOG", "LOG"),
    (Func::Log10, "LOG10", "LOG10"),
    (Func::Pow, "POW", "POW"),
    (Func::Sqrt, "SQRT", "SQRT"),
    (Func::Round, "ОКР", "ROUND"),
    (Func::Int, "ЦЕЛ", "INT"),
    (Func::Sum, "СУММА", "SUM"),
    (Func::Min, "МИНИМУМ", "MIN"),
    (Func::Max, "МАКСИМУМ", "MAX"),
    (Func::Avg, "СРЕДНЕЕ", "AVG"),
    (Func::Count, "КОЛИЧЕСТВО", "COUNT"),
    (Func::IsNull, "ЕСТЬNULL", "ISNULL"),
    (Func::Presentation, "ПРЕДСТАВЛЕНИЕ", "PRESENTATION"),
    (
        Func::RefPresentation,
        "ПРЕДСТАВЛЕНИЕССЫЛКИ",
        "REFPRESENTATION",
    ),
    (Func::ValueType, "ТИПЗНАЧЕНИЯ", "VALUETYPE"),
    (Func::GroupedBy, "СГРУППИРОВАНОПО", "GROUPEDBY"),
    (
        Func::StoredDataSize,
        "РАЗМЕРХРАНИМЫХДАННЫХ",
        "STOREDDATASIZE",
    ),
    (Func::Uuid, "УНИКАЛЬНЫЙИДЕНТИФИКАТОР", "UUID"),
];

impl Kw {
    pub const ALL: [Kw; 104] = [
        Kw::Select,
        Kw::Allowed,
        Kw::Distinct,
        Kw::Top,
        Kw::Into,
        Kw::Add,
        Kw::From,
        Kw::Where,
        Kw::Group,
        Kw::By,
        Kw::On,
        Kw::Grouping,
        Kw::Sets,
        Kw::Having,
        Kw::For,
        Kw::Update,
        Kw::Of,
        Kw::Index,
        Kw::Unique,
        Kw::Union,
        Kw::All,
        Kw::Order,
        Kw::Asc,
        Kw::Desc,
        Kw::Hierarchy,
        Kw::InHierarchy,
        Kw::Only,
        Kw::AutoOrder,
        Kw::Totals,
        Kw::Overall,
        Kw::Periods,
        Kw::Drop,
        Kw::As,
        Kw::Join,
        Kw::Inner,
        Kw::Left,
        Kw::Right,
        Kw::Full,
        Kw::Outer,
        Kw::And,
        Kw::Or,
        Kw::Not,
        Kw::In,
        Kw::Between,
        Kw::Is,
        Kw::Null,
        Kw::Refs,
        Kw::Like,
        Kw::Escape,
        Kw::Case,
        Kw::When,
        Kw::Then,
        Kw::Else,
        Kw::End,
        Kw::Cast,
        Kw::Boolean,
        Kw::Number,
        Kw::String,
        Kw::Date,
        Kw::DateTime,
        Kw::Type,
        Kw::Value,
        Kw::True,
        Kw::False,
        Kw::Undefined,
        Kw::EmptyTable,
        Kw::RecordAutoNumber,
        Kw::Substring,
        Kw::Year,
        Kw::Quarter,
        Kw::Month,
        Kw::DayOfYear,
        Kw::Day,
        Kw::Week,
        Kw::WeekDay,
        Kw::Hour,
        Kw::Minute,
        Kw::Second,
        Kw::TenDays,
        Kw::HalfYear,
        Kw::BeginOfPeriod,
        Kw::EndOfPeriod,
        Kw::DateAdd,
        Kw::DateDiff,
        Kw::Count,
        Kw::Sum,
        Kw::Avg,
        Kw::Min,
        Kw::Max,
        Kw::IsNull,
        Kw::Presentation,
        Kw::RefPresentation,
        Kw::ValueType,
        Kw::GroupedBy,
        Kw::Uuid,
        Kw::Characteristics,
        Kw::CharacteristicTypes,
        Kw::KeyField,
        Kw::NameField,
        Kw::ValueTypeField,
        Kw::CharacteristicValues,
        Kw::ObjectField,
        Kw::TypeField,
        Kw::ValueField,
    ];
}

impl Func {
    pub const ALL: [Func; 51] = [
        Func::Substring,
        Func::String,
        Func::StringLength,
        Func::TrimL,
        Func::TrimR,
        Func::TrimAll,
        Func::Left,
        Func::Right,
        Func::StrFind,
        Func::Upper,
        Func::Lower,
        Func::StrReplace,
        Func::Year,
        Func::Quarter,
        Func::Month,
        Func::DayOfYear,
        Func::Day,
        Func::Week,
        Func::WeekDay,
        Func::Hour,
        Func::Minute,
        Func::Second,
        Func::BeginOfPeriod,
        Func::EndOfPeriod,
        Func::DateAdd,
        Func::DateDiff,
        Func::RecordAutoNumber,
        Func::ACos,
        Func::ASin,
        Func::ATan,
        Func::Cos,
        Func::Sin,
        Func::Exp,
        Func::Log,
        Func::Log10,
        Func::Pow,
        Func::Sqrt,
        Func::Round,
        Func::Int,
        Func::Sum,
        Func::Min,
        Func::Max,
        Func::Avg,
        Func::Count,
        Func::IsNull,
        Func::Presentation,
        Func::RefPresentation,
        Func::ValueType,
        Func::GroupedBy,
        Func::StoredDataSize,
        Func::Uuid,
    ];
}

impl Kw {
    pub fn ru(self) -> &'static str {
        KW.get(self as usize).map_or("", |r| r.1)
    }
    pub fn en(self) -> &'static str {
        KW.get(self as usize).map_or("", |r| r.2)
    }
    pub fn name(self, s: ScriptVariant) -> &'static str {
        match s {
            ScriptVariant::Russian if !self.ru().is_empty() => self.ru(),
            _ => self.en(),
        }
    }
    pub const fn reserved_prefix(self) -> bool {
        use Kw::*;
        matches!(
            self,
            Select
                | Allowed
                | Distinct
                | Top
                | Into
                | From
                | Where
                | Group
                | By
                | On
                | Having
                | For
                | Index
                | Union
                | Order
                | AutoOrder
                | Totals
                | Drop
                | As
                | Join
                | Inner
                | Left
                | Right
                | Full
                | Outer
                | And
                | Or
                | Not
                | In
                | Between
                | Is
                | Like
                | Escape
                | When
                | Then
                | Else
                | End
        )
    }
    pub const fn reserved_alias(self) -> bool {
        use Kw::*;
        matches!(
            self,
            In | And
                | Or
                | Not
                | From
                | As
                | Where
                | Case
                | When
                | Then
                | Else
                | Null
                | True
                | False
                | Undefined
                | Like
                | Between
                | Is
                | Inner
                | Top
                | Distinct
                | Allowed
                | Into
                | For
                | On
                | Cast
                | Asc
                | Desc
                | Escape
        )
    }
    pub const fn reserved_src_alias(self) -> bool {
        matches!(
            self,
            Kw::In
                | Kw::And
                | Kw::From
                | Kw::Where
                | Kw::Case
                | Kw::On
                | Kw::By
                | Kw::Or
                | Kw::Not
                | Kw::As
        )
    }
}

impl Func {
    pub fn ru(self) -> &'static str {
        FUNCS.get(self as usize).map_or("", |r| r.1)
    }
    pub fn en(self) -> &'static str {
        FUNCS.get(self as usize).map_or("", |r| r.2)
    }
    pub fn name(self, s: ScriptVariant) -> &'static str {
        match s {
            ScriptVariant::Russian => self.ru(),
            ScriptVariant::English => self.en(),
        }
    }
    pub fn canonical(self) -> &'static str {
        match self {
            Func::Substring => "substring",
            Func::String => "string",
            Func::StringLength => "stringlength",
            Func::TrimL => "triml",
            Func::TrimR => "trimr",
            Func::TrimAll => "trimall",
            Func::Left => "left",
            Func::Right => "right",
            Func::StrFind => "strfind",
            Func::Upper => "upper",
            Func::Lower => "lower",
            Func::StrReplace => "strreplace",
            Func::Year => "year",
            Func::Quarter => "quarter",
            Func::Month => "month",
            Func::DayOfYear => "dayofyear",
            Func::Day => "day",
            Func::Week => "week",
            Func::WeekDay => "weekday",
            Func::Hour => "hour",
            Func::Minute => "minute",
            Func::Second => "second",
            Func::BeginOfPeriod => "beginofperiod",
            Func::EndOfPeriod => "endofperiod",
            Func::DateAdd => "dateadd",
            Func::DateDiff => "datediff",
            Func::RecordAutoNumber => "recordautonumber",
            Func::ACos => "acos",
            Func::ASin => "asin",
            Func::ATan => "atan",
            Func::Cos => "cos",
            Func::Sin => "sin",
            Func::Exp => "exp",
            Func::Log => "log",
            Func::Log10 => "log10",
            Func::Pow => "pow",
            Func::Sqrt => "sqrt",
            Func::Round => "round",
            Func::Int => "int",
            Func::Sum => "sum",
            Func::Min => "min",
            Func::Max => "max",
            Func::Avg => "avg",
            Func::Count => "count",
            Func::IsNull => "isnull",
            Func::Presentation => "presentation",
            Func::RefPresentation => "refpresentation",
            Func::ValueType => "valuetype",
            Func::GroupedBy => "groupedby",
            Func::StoredDataSize => "storeddatasize",
            Func::Uuid => "uuid",
        }
    }
    pub fn is_aggregate(self) -> bool {
        matches!(
            self,
            Func::Sum | Func::Avg | Func::Min | Func::Max | Func::Count
        )
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct KwSet(u128);

impl KwSet {
    pub fn contains(self, k: Kw) -> bool {
        self.0 & (1u128 << (k as u8)) != 0
    }
    pub(crate) const fn intersects(self, other: KwSet) -> bool {
        self.0 & other.0 != 0
    }
    const fn of(pick: Pick) -> KwSet {
        let (mut bits, mut rest) = (0u128, Kw::ALL.as_slice());
        while let Some((k, tail)) = rest.split_first() {
            let on = match pick {
                Pick::ReservedPrefix => k.reserved_prefix(),
                Pick::ReservedAlias => k.reserved_alias(),
                Pick::ReservedSrcAlias => k.reserved_src_alias(),
                Pick::CmpRunEnd => {
                    k.reserved_prefix()
                        && !matches!(
                            k,
                            Kw::Not | Kw::In | Kw::Between | Kw::Is | Kw::Like | Kw::Escape
                        )
                }
            };
            if on {
                bits |= 1u128 << (*k as u8);
            }
            rest = tail;
        }
        KwSet(bits)
    }
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }
    pub fn iter(self) -> impl Iterator<Item = Kw> {
        Kw::ALL.into_iter().filter(move |k| self.contains(*k))
    }
}

#[derive(Clone, Copy)]
enum Pick {
    ReservedPrefix,
    ReservedAlias,
    ReservedSrcAlias,
    CmpRunEnd,
}
pub(crate) const RESERVED_PREFIX: KwSet = KwSet::of(Pick::ReservedPrefix);
pub(crate) const RESERVED_ALIAS: KwSet = KwSet::of(Pick::ReservedAlias);
pub(crate) const RESERVED_SRC_ALIAS: KwSet = KwSet::of(Pick::ReservedSrcAlias);
pub(crate) const CMP_RUN_END: KwSet = KwSet::of(Pick::CmpRunEnd);

type Entry = (KwSet, Option<Func>);

const MAX_KEY: usize = 48;

const LOOKUP_SLOTS: usize = 512;
type Slot = Option<(&'static str, Entry)>;
static LOOKUP: [Slot; LOOKUP_SLOTS] = build_lookup();

const fn keyword_hash(mut bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    while let Some((&byte, rest)) = bytes.split_first() {
        hash = (hash ^ byte as u64).wrapping_mul(0x0000_0100_0000_01b3);
        bytes = rest;
    }
    hash
}

#[allow(
    clippy::indexing_slicing,
    reason = "compile-time table builder with bounded indices"
)]
const fn build_lookup() -> [Slot; LOOKUP_SLOTS] {
    let mut out = [None; LOOKUP_SLOTS];
    let mut i = 0;
    while i < KW.len() {
        let (kw, ru, en) = KW[i];
        install(&mut out, ru, 1u128 << kw as u8, None);
        install(&mut out, en, 1u128 << kw as u8, None);
        i += 1;
    }
    i = 0;
    while i < FUNCS.len() {
        let (function, ru, en) = FUNCS[i];
        install(&mut out, ru, 0, Some(function));
        install(&mut out, en, 0, Some(function));
        i += 1;
    }
    install(
        &mut out,
        "AUTORECORDNUMBER",
        0,
        Some(Func::RecordAutoNumber),
    );
    out
}
const fn equal_bytes(mut left: &[u8], mut right: &[u8]) -> bool {
    loop {
        match (left.split_first(), right.split_first()) {
            (Some((a, next_a)), Some((b, next_b))) if *a == *b => {
                left = next_a;
                right = next_b;
            }
            (None, None) => return true,
            _ => return false,
        }
    }
}
#[allow(
    clippy::indexing_slicing,
    reason = "compile-time table insertion masks every index"
)]
const fn install(
    out: &mut [Slot; LOOKUP_SLOTS],
    name: &'static str,
    bits: u128,
    function: Option<Func>,
) {
    if name.is_empty() {
        return;
    }
    let mut at = keyword_hash(name.as_bytes()) as usize & (LOOKUP_SLOTS - 1);
    loop {
        match out[at] {
            Some((key, (keywords, old_function))) => {
                if equal_bytes(key.as_bytes(), name.as_bytes()) {
                    out[at] = Some((
                        key,
                        (
                            KwSet(keywords.0 | bits),
                            match function {
                                Some(value) => Some(value),
                                None => old_function,
                            },
                        ),
                    ));
                    return;
                }
            }
            None => {
                out[at] = Some((name, (KwSet(bits), function)));
                return;
            }
        }
        at = (at + 1) & (LOOKUP_SLOTS - 1);
    }
}
fn lookup_uppercase(word: &str) -> Entry {
    let mut at = keyword_hash(word.as_bytes()) as usize & (LOOKUP_SLOTS - 1);
    for _ in 0..LOOKUP_SLOTS {
        match LOOKUP.get(at) {
            Some(Some((key, entry))) if *key == word => return *entry,
            Some(Some(_)) => at = (at + 1) & (LOOKUP_SLOTS - 1),
            _ => break,
        }
    }
    Entry::default()
}

fn upper(c: char, mut push: impl FnMut(char) -> bool) -> bool {
    match c {
        'a'..='z' => push(c.to_ascii_uppercase()),
        'а'..='я' => push(char::from_u32(c as u32 - 0x20).unwrap_or(c)),
        'ё' => push('Ё'),
        _ if c.is_ascii() || matches!(c, 'А'..='Я' | 'Ё') => push(c),
        _ => c.to_uppercase().all(push),
    }
}

pub fn lookup(word: &str) -> (KwSet, Option<Func>) {
    let mut buf = [0u8; MAX_KEY];
    let mut n = 0usize;
    let mut push = |c: char| {
        let end = n + c.len_utf8();
        match buf.get_mut(n..end) {
            Some(dst) => {
                c.encode_utf8(dst);
                n = end;
                true
            }
            None => false,
        }
    };
    for c in word.chars() {
        if !upper(c, &mut push) {
            return Entry::default();
        }
    }
    let Some(up) = buf.get(..n).and_then(|b| std::str::from_utf8(b).ok()) else {
        return Entry::default();
    };
    lookup_uppercase(up)
}
