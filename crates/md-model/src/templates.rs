#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemplateType {
    SpreadsheetDocument,
    BinaryData,
    ActiveDocument,
    HTMLDocument,
    TextDocument,
    GeographicalSchema,
    DataCompositionSchema,
    DataCompositionAppearanceTemplate,
    GraphicalSchema,
    AddIn,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Template {
    pub kind: Option<TemplateType>,
    pub files: Vec<(String, Vec<u8>)>,
}

impl TemplateType {
    #[doc(hidden)]
    pub fn from_name(name: &str) -> Option<TemplateType> {
        use TemplateType::*;
        Some(match name {
            "SpreadsheetDocument" => SpreadsheetDocument,
            "BinaryData" => BinaryData,
            "ActiveDocument" => ActiveDocument,
            "HTMLDocument" => HTMLDocument,
            "TextDocument" => TextDocument,
            "GeographicalSchema" => GeographicalSchema,
            "DataCompositionSchema" => DataCompositionSchema,
            "DataCompositionAppearanceTemplate" => DataCompositionAppearanceTemplate,
            "GraphicalSchema" => GraphicalSchema,
            "AddIn" => AddIn,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        use TemplateType::*;
        match self {
            SpreadsheetDocument => "SpreadsheetDocument",
            BinaryData => "BinaryData",
            ActiveDocument => "ActiveDocument",
            HTMLDocument => "HTMLDocument",
            TextDocument => "TextDocument",
            GeographicalSchema => "GeographicalSchema",
            DataCompositionSchema => "DataCompositionSchema",
            DataCompositionAppearanceTemplate => "DataCompositionAppearanceTemplate",
            GraphicalSchema => "GraphicalSchema",
            AddIn => "AddIn",
        }
    }
}
