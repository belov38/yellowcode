use crate::{Interface, Rgb};
use std::collections::{BTreeMap, HashMap};
use std::sync::OnceLock;

#[derive(Debug, Clone, serde::Deserialize)]
pub struct StyleEntry {
    pub collection: String,
    pub ru: String,
    pub en: String,
    pub id: Option<i32>,
    pub server: Option<String>,
    pub web: BTreeMap<String, BTreeMap<String, String>>,
}

pub struct Style {
    entries: Vec<StyleEntry>,
    by_name: HashMap<(String, String), usize>,
    by_id: HashMap<(String, i32), usize>,
}

static STYLE: OnceLock<Style> = OnceLock::new();

impl Style {
    #[allow(
        clippy::expect_used,
        reason = "embedded data/style.json; parsed by the crate tests"
    )]
    pub fn get() -> &'static Style {
        STYLE.get_or_init(|| {
            let entries: Vec<StyleEntry> = serde_json::from_str(include_str!("../data/style.json"))
                .expect("data/style.json is valid");
            let mut by_name = HashMap::new();
            let mut by_id = HashMap::new();
            for (i, e) in entries.iter().enumerate() {
                by_name.insert((e.collection.clone(), e.ru.clone()), i);
                by_name.insert((e.collection.clone(), e.en.clone()), i);
                if let Some(id) = e.id {
                    by_id.entry((e.collection.clone(), id)).or_insert(i);
                }
            }
            Style {
                entries,
                by_name,
                by_id,
            }
        })
    }

    pub fn entries(&self) -> &[StyleEntry] {
        &self.entries
    }

    pub fn entry(&self, collection: &str, name: &str) -> Option<&StyleEntry> {
        let i = self
            .by_name
            .get(&(collection.to_string(), name.to_string()))?;
        self.entries.get(*i)
    }

    pub fn entry_by_id(&self, collection: &str, id: i32) -> Option<&StyleEntry> {
        let i = self.by_id.get(&(collection.to_string(), id))?;
        self.entries.get(*i)
    }

    pub fn color(&self, iface: Interface, name: &str) -> Option<Rgb> {
        let key = match iface {
            Interface::Taxi => "taxi",
            Interface::V85 => "v85",
        };
        ["ЦветаСтиля", "ЦветаПалитры"].iter().find_map(|c| {
            let text = self.entry(c, name)?.web.get(key)?.get("client.text")?;
            parse_rgb(text)
        })
    }
}

fn parse_rgb(text: &str) -> Option<Rgb> {
    let (_, v) = text.rsplit_once('|')?;
    let mut it = v.split(',').map(|p| p.trim().parse::<u8>());
    let (r, g, b) = (it.next()?.ok()?, it.next()?.ok()?, it.next()?.ok()?);
    it.next().is_none().then_some(Rgb(r, g, b))
}
