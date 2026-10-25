#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Visibility {
    #[default]
    Unknown,
    Visible,
    Partial,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExternalBound {
    pub source: String,
    pub bytes: u64,
}
#[derive(Clone, Debug, Default)]
pub struct Discovery {
    pub bounds: Vec<ExternalBound>,
    pub confirmed_unlimited: Vec<String>,
    pub physical_memory: Option<u64>,
    pub soft_limit: Option<u64>,
    pub visibility: Visibility,
    pub warnings: Vec<String>,
    pub membership: String,
    pub membership_confirmed: bool,
}
impl Discovery {
    pub fn finite(source: impl Into<String>, bytes: u64) -> Self {
        Self {
            bounds: vec![ExternalBound {
                source: source.into(),
                bytes,
            }],
            visibility: Visibility::Visible,
            membership_confirmed: true,
            ..Self::default()
        }
    }
}
#[cfg(target_os = "linux")]
pub mod linux;
pub fn discover() -> Discovery {
    #[cfg(target_os = "linux")]
    {
        linux::discover()
    }
    #[cfg(not(target_os = "linux"))]
    {
        Discovery { warnings: vec!["Native process/container memory discovery unavailable; use explicit deployment envelope".into()], ..Discovery::default() }
    }
}
