use super::{Category, ErrorInfo, StackEntry, catalog_text};
use crate::value::Value;
use bsl_prim::CompatMode;
use platform_catalog::messages::Lang;

impl ErrorInfo {
    pub fn brief(&self) -> String {
        let mut cur = self;
        while let Some(c) = cur.cause.as_deref() {
            cur = c;
        }
        cur.description.to_string_lossy()
    }

    pub fn detailed(&self, compat: CompatMode, lang: Lang) -> String {
        let with_categories = !compat.flags().error_detail_without_categories;
        let chain = self.chain();
        let mut stack: Vec<&StackEntry> = Vec::new();
        for info in chain.iter().rev() {
            let own: Vec<&StackEntry> = info.stack.iter().collect();
            let shared = common_suffix(&stack, &own);
            stack.truncate(stack.len().saturating_sub(shared));
            stack.extend(own);
        }
        let mut out = String::new();
        for (i, info) in chain.iter().enumerate() {
            if i > 0 {
                if !out.ends_with('\n') {
                    out.push('\n');
                }
                out.push_str(&catalog_text("backbas:IDS_BECAUSE", &[], "", lang));
                out.push('\n');
            }
            let frames: &[&StackEntry] = if i == 0 { &stack } else { &[] };
            info.block(frames, with_categories, &mut out);
        }
        out
    }

    pub fn description_text(&self, lang: Lang) -> String {
        let module = if self.module_name.is_empty() {
            format!(
                "<{}>",
                catalog_text("IDS_UNKNOWN_MODULE_NAME", &[], "", lang)
            )
        } else {
            self.module_name.to_string_lossy()
        };
        format!(
            "{{{module}({})}}: {}",
            self.line,
            self.description.to_string_lossy()
        )
    }

    pub fn is_category(&self, c: Category, with_causes: bool) -> bool {
        if c == Category::AllErrors {
            return true;
        }
        let mut cur = Some(self);
        while let Some(info) = cur {
            if info.categories.contains(c) {
                return true;
            }
            cur = if with_causes {
                info.cause.as_deref()
            } else {
                None
            };
        }
        false
    }

    fn chain(&self) -> Vec<&ErrorInfo> {
        let mut out = vec![self];
        let mut cur = self;
        while let Some(c) = cur.cause.as_deref() {
            out.push(c);
            cur = c;
        }
        out
    }

    fn block(&self, frames: &[&StackEntry], with_categories: bool, out: &mut String) {
        let mut tail: Vec<String> = Vec::new();
        if !self.code.is_empty() {
            tail.push(self.code.to_string_lossy());
        }
        if let Value::String(s) = &self.additional
            && !s.is_empty()
        {
            tail.push(s.to_string_lossy());
        }
        if with_categories {
            let names = display_categories(self);
            if !names.is_empty() {
                tail.push(format!("[{}]", names.join(", ")));
            }
        }
        out.push_str(&self.description.to_string_lossy());
        if frames.is_empty() && tail.is_empty() {
            return;
        }
        out.push('\n');
        for f in frames {
            out.push('{');
            out.push_str(&f.module_name.to_string_lossy());
            out.push('(');
            out.push_str(&f.line.to_string());
            out.push_str(")}:");
            out.push_str(
                f.source_line
                    .to_string_lossy()
                    .trim_start_matches([' ', '\t']),
            );
            out.push('\n');
        }
        if !tail.is_empty() {
            if !frames.is_empty() {
                out.push('\n');
            }
            out.push_str(&tail.join("\n"));
        }
    }
}

fn display_categories(info: &ErrorInfo) -> Vec<&'static str> {
    let runtime = info.categories.contains(Category::ScriptRuntimeError);
    runtime
        .then_some(Category::ScriptRuntimeError)
        .into_iter()
        .chain(
            info.categories
                .iter()
                .filter(|c| !matches!(c, Category::AllErrors | Category::ScriptRuntimeError)),
        )
        .map(Category::name_ru)
        .collect()
}

fn common_suffix(a: &[&StackEntry], b: &[&StackEntry]) -> usize {
    a.iter()
        .rev()
        .zip(b.iter().rev())
        .take_while(|(x, y)| {
            x.line == y.line && x.module_name == y.module_name && x.source_line == y.source_line
        })
        .count()
}
