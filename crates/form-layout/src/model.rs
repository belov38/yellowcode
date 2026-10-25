use crate::{Rect, Size};

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Layout {
    pub content: Size,
    pub window: WindowGeom,
    pub boxes: Vec<ItemBox>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct WindowGeom {
    pub floating: bool,
    pub window: Rect,
    pub workspace: Rect,
    pub content: Rect,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub enum Part {
    TableCommand(String),
    SearchString { bar: String },
    ViewStatus,
    TumblerItem(u32),
    TumblerColumn(u32),
    SpinButtons,
    More,
    PagesPreloader,
    RadioItem(u32),
    RadioGroup,
    Tab(u32),
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub enum BoxKey {
    Item(String),
    Part { owner: String, part: Part },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum FieldButton {
    Choice,
    DropList,
    Clear,
    Open,
    SpinUp,
    SpinDown,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub enum BoxKind {
    Field,
    CheckBox,
    Toggle,
    ToggleItem,
    Switch,
    RadioGroup,
    RadioItem,
    RadioColumn,
    Label,
    LabelField,
    Button,
    CommandBar,
    CommandButton,
    Grid,
    SpreadsheetField,
    Column,
    Tabs,
    Page,
    Group,
    Picture,
    ViewStatus,
    SpinButtons,
    Preloader,
    Stub(String),
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct BoxFlags {
    pub read_only: bool,
    pub disabled: bool,
    pub align_right: bool,
    pub column: bool,
    pub default_button: bool,
    pub hyperlink: bool,
    pub multi_line: bool,
    pub password: bool,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct TitleBox {
    pub rect: Rect,
    pub text: String,
    pub lines: u32,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct GridColumn {
    pub name: String,
    pub title: String,
    pub x: f32,
    pub w: f32,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct GridBox {
    pub inner: Size,
    pub columns: Vec<GridColumn>,
    pub header_h: f32,
    pub row_h: f32,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ItemBox {
    pub key: BoxKey,
    pub kind: BoxKind,
    pub rect: Rect,
    pub hidden: bool,
    pub depth: u16,
    pub title: Option<TitleBox>,
    pub input: Option<Rect>,
    pub buttons: Vec<FieldButton>,
    pub flags: BoxFlags,
    pub text: Option<String>,
    pub grid: Option<GridBox>,
}

impl ItemBox {
    pub fn new(key: BoxKey, kind: BoxKind, rect: Rect, depth: u16) -> ItemBox {
        ItemBox {
            key,
            kind,
            rect,
            hidden: false,
            depth,
            title: None,
            input: None,
            buttons: Vec::new(),
            flags: BoxFlags::default(),
            text: None,
            grid: None,
        }
    }
}
