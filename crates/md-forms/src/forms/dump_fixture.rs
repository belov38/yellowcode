#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::collections::HashMap;
use std::sync::Arc;

use brace::Guid;
use md_model::test_util::{guid, obj, sub};
use mdschema::forms::{form_tables, std_attrs};

use super::shared::Shared;
use super::{Form, FormShared, FormSource};
use crate::forms::py::b64_decode;
use crate::index::Index;
use crate::{Configuration, EntryData, MdKind, SubKind};

const MD: &str = include_str!("../../../mdconf/tests/fixtures/c4/codecs_b_md.txt");
const FORM: &[u8] = include_bytes!("../../../mdconf/tests/fixtures/c4/codecs_b_form.txt");

pub(crate) fn md() -> (Configuration, Shared) {
    let (mut names, mut types, mut predef) = (Vec::new(), Vec::new(), HashMap::new());
    for line in MD.lines() {
        let [k, u, n] = <[&str; 3]>::try_from(line.split('\t').collect::<Vec<_>>()).unwrap();
        let g = Guid::parse(u).unwrap();
        match k {
            "name" => names.push((g, n)),
            "type" => types.push((g, n)),
            _ => {
                predef.insert(g, n.to_string());
            }
        }
    }
    let cfg = Configuration {
        index: Index::for_tests(&names, &types),
        predef,
        ..Default::default()
    };
    let sh = Shared::new(
        false,
        form_tables(false, None).unwrap(),
        std_attrs().unwrap(),
        names.iter().map(|(u, _)| *u).collect(),
        HashMap::new(),
        &cfg.index.names,
    );
    (cfg, sh)
}

fn whole(label: &str) -> Vec<u8> {
    let rows = include_str!("../../../mdconf/tests/fixtures/c4/sections_whole.txt");
    let row = rows
        .lines()
        .find_map(|l| l.strip_prefix(&format!("{label}\t")))
        .unwrap();
    b64_decode(row.split('\t').next().unwrap()).unwrap()
}

fn form(sh: &Arc<Shared>, source: FormSource) -> Option<Form> {
    Some(Form {
        source: Some(source),
        shared: Some(FormShared::Native(Arc::<Shared>::clone(sh))),
        ..Default::default()
    })
}

fn text(t: &[u8]) -> FormSource {
    FormSource::Text(t.into())
}

fn form_with_chart() -> Vec<u8> {
    let chart = "{9,{99},0,\"Диаграмма\",{1,0},{\"Pattern\"},{0,{0,{\"B\",1},0}},\
                 {0,{0,{\"B\",1},0}},{0,0},{0,0},0,0,0,0,{0,1,\"Chart\",\
                 {\"#\",3543ef08-3316-4f7e-9447-0cd0a1cbf1d5,{11},{75}}},{0,0}}";
    let form = std::str::from_utf8(FORM).unwrap();
    let first = "{9,{1},0,\"Объект\",";
    let attrs = format!("{{4,2,{first}");
    assert_eq!(form.matches(&attrs).count(), 1);
    form.replace(&attrs, &format!("{{4,3,{chart},{first}"))
        .into_bytes()
}

pub fn configuration() -> Configuration {
    let (mut cfg, sh) = md();
    let sh = Arc::new(sh);
    let mut cat = obj(MdKind::Catalog, "Банки");
    let f = |n: u32, name: &str, src: FormSource| {
        let mut s = sub(SubKind::Form, name);
        s.id = guid(n);
        s.form = form(&sh, src);
        s
    };
    cat.forms = vec![
        f(1, "Ф", text(FORM)),
        f(2, "Диаграмма", text(&whole("chart-default"))),
        f(5, "ДиаграммаСбой", text(&whole("chart"))),
        f(
            3,
            "Сбой",
            text(include_bytes!(
                "../../../mdconf/tests/fixtures/c4/synthetic_form.txt"
            )),
        ),
        sub(SubKind::Form, "Пусто"),
        f(4, "СДиаграммой", text(&form_with_chart())),
    ];
    let mut common = obj(MdKind::CommonForm, "Общая");
    let doc = brace::parse_doc(FORM).unwrap();
    common.form = form(&sh, FormSource::Doc(Arc::new(EntryData::Brace(doc))));
    cfg.objects = vec![cat, common];
    cfg.index
        .names
        .insert(guid(1), "Catalog.Банки.Form.Ф".to_string());
    cfg
}
