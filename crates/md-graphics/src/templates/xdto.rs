pub(crate) const XSI: &str = "http://www.w3.org/2001/XMLSchema-instance";
pub(crate) const XS: &str = "http://www.w3.org/2001/XMLSchema";
pub(crate) const V8: &str = "http://v8.1c.ru/8.1/data/core";
pub(crate) const CFG: &str = "http://v8.1c.ru/8.1/data/enterprise/current-config";
pub(crate) const V8UI: &str = "http://v8.1c.ru/8.1/data/ui";
pub(crate) const STYLE: &str = "http://v8.1c.ru/8.1/data/ui/style";

const MAX_WRITE_DEPTH: usize = 128;

const XR: &str = "http://v8.1c.ru/8.3/xcf/readable";
pub(super) mod normalize;
pub(super) mod overlay;
pub(super) mod scope;
pub(super) mod visit;
