pub mod blocks;
pub mod check;
pub mod code_consoles;
pub mod configurator;
pub mod consoles;
pub mod definition;
pub mod find;
pub mod form;
pub mod helper;
pub mod highlight;
pub mod icon_table;
pub mod icons;
pub mod module;
pub mod names;
pub mod object_window;
pub mod out;
pub mod outline;
pub mod path;
pub mod predefined_view;
pub mod rights_view;
pub mod show;
pub mod skin;
pub mod source;
pub mod state;
pub mod stats;
pub mod template_view;
pub mod tree;
pub mod types;

pub use out::{Color, Lang};
pub use state::{Loader, Need, State, Unavailable, Viewer, source_loader};

use path::Target;

pub fn identity(target: &Target, lang: Lang) -> (String, String) {
    match target {
        Target::Configuration(cfg) => (
            names::kind_name(mdconf::MdKind::Configuration, lang),
            tree::config_name(cfg),
        ),
        Target::Recalc { recalc, .. } => (
            (if lang == Lang::Ru {
                path::RECALC_RU
            } else {
                path::RECALC_EN
            })
            .to_string(),
            recalc.name.clone(),
        ),
        Target::Object(obj) => (names::kind_name(obj.kind, lang), obj.name.clone()),
        Target::Child { chain, .. } => match chain.last() {
            Some(c) => (names::child_tag_name(c.tag, lang), c.name.clone()),
            None => (String::new(), String::new()),
        },
        Target::Sub { sub, .. } => (names::sub_kind_name(sub.kind, lang), sub.name.clone()),
    }
}
