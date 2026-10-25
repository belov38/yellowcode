use std::collections::HashMap;
use std::sync::OnceLock;

#[derive(Debug, Clone, serde::Deserialize)]
pub struct Caption {
    pub id: String,
    pub key: String,
    pub ru: String,
    pub en: String,
    pub hotkey: Option<String>,
    pub picture: Option<String>,
}

struct Table {
    all: Vec<Caption>,
    by_id: HashMap<String, usize>,
}

#[allow(
    clippy::expect_used,
    reason = "embedded data/captions.json; parsed by the crate tests"
)]
fn table() -> &'static Table {
    static T: OnceLock<Table> = OnceLock::new();
    T.get_or_init(|| {
        let all: Vec<Caption> = serde_json::from_str(include_str!("../data/captions.json"))
            .expect("data/captions.json is valid");
        let by_id = all
            .iter()
            .enumerate()
            .map(|(i, c)| (c.id.clone(), i))
            .collect();
        Table { all, by_id }
    })
}

pub fn captions() -> &'static [Caption] {
    &table().all
}

pub fn caption(id: &str) -> Option<&'static Caption> {
    let t = table();
    t.by_id.get(id).and_then(|i| t.all.get(*i))
}

pub fn command_text(id: &str, lang: &str) -> Option<&'static str> {
    let c = caption(id)?;
    Some(if lang == "ru" { &c.ru } else { &c.en })
}
