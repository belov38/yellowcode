use crate::lang::Lang;
use crate::msg_gen::MESSAGES;
pub use crate::msg_gen::MsgId;

const NONE: (&str, &str, &str, &str) = ("", "", "", "");
const _: () = assert!(MESSAGES.len() == MsgId::ALL.len());

fn entry(id: MsgId) -> &'static (&'static str, &'static str, &'static str, &'static str) {
    MESSAGES.get(id as usize).unwrap_or(&NONE)
}
impl MsgId {
    pub fn module(self) -> &'static str {
        entry(self).0
    }
    pub fn key(self) -> &'static str {
        entry(self).1
    }
    pub fn text(self, lang: Lang) -> &'static str {
        match lang {
            Lang::Ru => entry(self).2,
            Lang::En => entry(self).3,
        }
    }
}
