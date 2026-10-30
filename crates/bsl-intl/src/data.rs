use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{Read, Write};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct LocaleData {
    pub name: String,
    pub decimal: String,
    pub group: String,
    pub minus: String,
    pub first_day_of_week: u8,
    pub months: [Vec<String>; 4],
    pub days: [Vec<String>; 4],
    pub am_pm: [String; 2],
    pub date_patterns: [String; 4],
    pub time_patterns: [String; 4],
    pub date_time: String,
    pub zone_names: BTreeMap<String, String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct LocalesData {
    pub icu: String,
    pub locales: Vec<LocaleData>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum TzZone {
    Tzif {
        tzif_b64: String,
        std: Vec<(i64, i32)>,
    },
    Alias(String),
    Posix(String),
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct MetazoneSpan {
    pub from: i64,
    pub to: i64,
    pub metazone: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct TzData {
    pub tzdata: String,
    pub names: Vec<String>,
    pub zones: BTreeMap<String, TzZone>,
    pub metazones: BTreeMap<String, Vec<MetazoneSpan>>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct SlavicGroup {
    pub forms: [String; 3],
    pub gender: u8,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct SlavicWords {
    pub zero: String,
    pub one: [String; 3],
    pub two: [String; 2],
    pub units: [String; 7],
    pub teens: [String; 9],
    pub tens: [String; 9],
    pub hundreds: [String; 9],
    pub groups: [SlavicGroup; 4],
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum Hundreds {
    Word(Vec<String>),
    List([String; 9]),
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct EnWords {
    pub units: [String; 10],
    pub teens: [String; 9],
    pub tens: [String; 9],
    pub hundreds: Hundreds,
    pub groups: [[String; 2]; 4],
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct DeGroup {
    pub forms: [String; 2],
    pub gender: u8,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct DeWords {
    pub und: String,
    pub zero: String,
    pub one: [String; 3],
    pub units: [String; 8],
    pub teens: [String; 9],
    pub tens: [String; 9],
    pub hundreds: [String; 9],
    pub thousand: (String, u8),
    pub groups: [DeGroup; 3],
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct KkWords {
    pub units: [String; 10],
    pub tens: [String; 9],
    pub hundreds: Hundreds,
    pub groups: [String; 4],
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum Spell {
    Slavic(SlavicWords),
    En(EnWords),
    De(DeWords),
    Kk(KkWords),
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    Spaced,
    Compact,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Casing {
    Plain,
    Turkic,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Forms {
    Cldr,
    LastDigitOne,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct SpellLang {
    pub layout: Layout,
    pub casing: Casing,
    pub forms: Forms,
    pub words: Spell,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(untagged)]
pub enum SplNode {
    Str(String),
    Int(i64),
    List(Vec<SplNode>),
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum PluralOperand {
    N,
    I,
    V,
    W,
    F,
    T,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct PluralRelation {
    pub operand: PluralOperand,
    pub modulus: Option<u64>,
    pub negated: bool,
    pub ranges: Vec<(u64, u64)>,
}

pub type PluralCondition = Vec<Vec<PluralRelation>>;

#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq)]
pub struct PluralData {
    pub cardinal: BTreeMap<String, String>,
    pub ordinal: BTreeMap<String, String>,
    pub rules: BTreeMap<String, [Option<PluralCondition>; 5]>,
}

#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq)]
pub struct WordsData {
    pub bool_words: BTreeMap<String, (String, Option<String>)>,
    pub spell: BTreeMap<String, SpellLang>,
    pub spell_other: BTreeMap<String, SplNode>,
    pub period: BTreeMap<String, Vec<String>>,
    pub plurals: PluralData,
}

fn encode<T: Serialize>(data: &T) -> Result<Vec<u8>, String> {
    let json = serde_json::to_vec(data).map_err(|e| e.to_string())?;
    let mut enc = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::new(9));
    enc.write_all(&json).map_err(|e| e.to_string())?;
    enc.finish().map_err(|e| e.to_string())
}

fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, String> {
    let mut json = Vec::new();
    flate2::read::DeflateDecoder::new(bytes)
        .read_to_end(&mut json)
        .map_err(|e| e.to_string())?;
    serde_json::from_slice(&json).map_err(|e| e.to_string())
}

pub fn encode_locales(data: &LocalesData) -> Result<Vec<u8>, String> {
    encode(data)
}

pub fn decode_locales(bytes: &[u8]) -> Result<LocalesData, String> {
    decode(bytes)
}

pub fn encode_tz(data: &TzData) -> Result<Vec<u8>, String> {
    encode(data)
}

pub fn decode_tz(bytes: &[u8]) -> Result<TzData, String> {
    decode(bytes)
}

pub fn encode_words(data: &WordsData) -> Result<Vec<u8>, String> {
    encode(data)
}

pub fn decode_words(bytes: &[u8]) -> Result<WordsData, String> {
    decode(bytes)
}
