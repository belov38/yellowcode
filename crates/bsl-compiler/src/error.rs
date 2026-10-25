use bsl_syntax::{MsgId, SyntaxError};
use platform_catalog::messages::{Lang, render};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompileError {
    pub msg: MsgId,
    pub arg: Option<String>,
    pub line: u32,
    pub col: u32,
}

impl CompileError {
    pub fn from_syntax(e: &SyntaxError) -> CompileError {
        CompileError {
            msg: e.msg,
            arg: e.arg.clone(),
            line: e.pos.line,
            col: e.pos.col,
        }
    }

    pub fn render(&self, lang: Lang) -> String {
        let arg = self.arg.as_deref();
        render(self.msg.0, arg, lang).unwrap_or_else(|| match arg {
            Some(a) => format!("{} ({a})", self.msg.0),
            None => self.msg.0.to_string(),
        })
    }

    pub fn cause_text(&self, module_name: &str, lang: Lang) -> String {
        let name = if module_name.is_empty() {
            let id = "IDS_UNKNOWN_MODULE_NAME";
            format!("<{}>", render(id, None, lang).unwrap_or_else(|| id.into()))
        } else {
            module_name.to_string()
        };
        format!(
            "{{{name}({},{})}}: {}",
            self.line,
            self.col,
            self.render(lang)
        )
    }
}
