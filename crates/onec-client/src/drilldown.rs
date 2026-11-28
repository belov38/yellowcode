use roxmltree::{Document, Node};

use crate::answer::{AnswerError, NS_WS, boolean, unsigned};
use crate::dcs::{DcsError, NS_DETAILS, NS_SETTINGS};
use crate::spreadsheet::Sheet;
use crate::xml::escape_text;

pub const START: &str = "/e1cib/dcs?cmd=startDetailsProcessingExecute";
pub const PARAMETER: &str = "Расшифровка";
pub const DRILL_DOWN: &str = "DrillDown";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub text: String,
    pub action: Option<String>,
    pub enabled: bool,
    pub items: Vec<Command>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Menu {
    pub commands: Vec<Command>,
    pub restricted: Vec<String>,
    pub main_action: Option<String>,
}

impl Menu {
    pub fn drills_down(&self) -> bool {
        fn any(c: &[Command]) -> bool {
            c.iter()
                .any(|c| (c.enabled && c.action.as_deref() == Some(DRILL_DOWN)) || any(&c.items))
        }
        any(&self.commands)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrillDown {
    pub id: u32,
    pub field: String,
    pub menu: Menu,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellRef {
    Id(u32),
    At { row: u32, column: u32 },
}

pub fn parse_cell(cell: &str) -> Result<CellRef, DcsError> {
    let bad = || DcsError::Value {
        name: cell.to_string(),
        reason: "a cell is R<row>C<column> (from 1, e.g. R6C2) or #<id of its details>".into(),
    };
    let c = cell.trim();
    if let Some(id) = c.strip_prefix('#') {
        return unsigned(id.trim()).map(CellRef::Id).ok_or_else(bad);
    }
    let upper = c.to_uppercase();
    let rest = upper.strip_prefix('R').ok_or_else(bad)?;
    rest.split_once('C')
        .and_then(|(r, k)| Some((unsigned(r)?, unsigned(k)?)))
        .filter(|(r, k)| *r > 0 && *k > 0)
        .map(|(row, column)| CellRef::At { row, column })
        .ok_or_else(bad)
}

pub fn details_id<'s>(
    sheets: impl IntoIterator<Item = &'s Sheet>,
    cell: &str,
) -> Result<u32, DcsError> {
    let (row, column) = match parse_cell(cell)? {
        CellRef::Id(id) => return Ok(id),
        CellRef::At { row, column } => (row, column),
    };
    let mut found_cell = false;
    for s in sheets {
        if let Some(c) = s
            .cells
            .iter()
            .find(|x| x.row == row - 1 && x.column == column - 1)
        {
            found_cell = true;
            if let Some(id) = c.details_id {
                return Ok(id);
            }
        }
    }
    Err(DcsError::Value {
        name: cell.trim().to_string(),
        reason: if found_cell {
            "the cell has no details".into()
        } else {
            "the report has no such cell".into()
        },
    })
}

pub fn start_request(report_type: &str, address: &str, id: u32, fid: &str) -> Vec<u8> {
    format!(
        "\u{feff}<?xml version=\"1.0\" encoding=\"UTF-8\"?><request xmlns=\"{NS_WS}\" \
         xmlns:xs=\"http://www.w3.org/2001/XMLSchema\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\">\
         <reportObjectType>{}</reportObjectType><detailsInformation>{}</detailsInformation><detailsID>{id}</detailsID>\
         <tempStorageId>{}</tempStorageId><multipleDetailsID>{id}</multipleDetailsID></request>",
        escape_text(report_type),
        escape_text(address),
        escape_text(fid)
    )
    .into_bytes()
}

pub fn read_start(body: &[u8]) -> Result<Menu, DcsError> {
    const ANSWER: &str = "dcs/startDetailsProcessingExecute";
    let text = String::from_utf8_lossy(body.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(body));
    let doc = Document::parse(&text).map_err(|e| AnswerError::Parse {
        answer: ANSWER,
        message: e.to_string(),
    })?;
    let root = doc.root_element();
    if root.tag_name().namespace() != Some(NS_WS) || root.tag_name().name() != "request" {
        return Err(AnswerError::Bad {
            answer: ANSWER,
            field: "root".into(),
            value: root.tag_name().name().to_string(),
        }
        .into());
    }
    let texts = |local: &str| -> Vec<String> {
        root.children()
            .filter(|n| is_ws(*n, local))
            .filter_map(|n| n.text())
            .map(str::to_string)
            .collect()
    };
    Ok(Menu {
        commands: commands(root, "topCommands"),
        restricted: texts("rectrictionFields"),
        main_action: texts("mainAction").into_iter().next(),
    })
}

fn commands(n: Node, local: &str) -> Vec<Command> {
    n.children()
        .filter(|c| is_ws(*c, local))
        .filter(|c| child_text(*c, "id").and_then(unsigned) != Some(0))
        .map(|c| Command {
            text: child_text(c, "text").unwrap_or("").to_string(),
            action: child_text(c, "parameter").map(str::to_string),
            enabled: child_text(c, "enable").and_then(boolean).unwrap_or(true),
            items: commands(c, "subMenu"),
        })
        .collect()
}

fn is_ws(n: Node, local: &str) -> bool {
    n.is_element() && n.tag_name().namespace() == Some(NS_WS) && n.tag_name().name() == local
}

fn child_text<'a>(n: Node<'a, '_>, local: &str) -> Option<&'a str> {
    n.children()
        .find(|c| is_ws(*c, local))
        .and_then(|c| c.text())
}

pub fn check(menu: &Menu, field: &str) -> Result<(), DcsError> {
    if !menu.drills_down() {
        return Err(DcsError::NoDrillDown);
    }
    if menu
        .restricted
        .iter()
        .any(|r| crate::value::fold_key(r) == crate::value::fold_key(field))
    {
        return Err(DcsError::Value {
            name: field.to_string(),
            reason: format!(
                "the cell is grouped by it already (the fields of the choice leave out {})",
                menu.restricted.join(", ")
            ),
        });
    }
    Ok(())
}

pub fn parameter_value(address: &str, id: u32, field: &str) -> String {
    format!(
        "<val xmlns:d5p1=\"{NS_DETAILS}\" xsi:type=\"d5p1:DetailsProcessDescription\"><d5p1:data>{}</d5p1:data>\
<d5p1:settingsToApply><item xmlns=\"{NS_SETTINGS}\" xsi:type=\"StructureItemGroup\"><name>{PARAMETER}</name><groupItems>\
<item xsi:type=\"GroupItemField\"><field>{}</field><groupType>Items</groupType><periodAdditionType>None\
</periodAdditionType></item></groupItems><order><item xsi:type=\"OrderItemAuto\"/></order><selection>\
<item xsi:type=\"SelectedItemAuto\"/></selection></item></d5p1:settingsToApply><d5p1:ID>{id}</d5p1:ID></val>",
        escape_text(address),
        escape_text(field)
    )
}

pub fn open_request(key: &str, address: &str, id: u32, field: &str) -> Vec<u8> {
    crate::report::query_request_raw(
        key,
        &format!(
            "<prms><prm><name>{PARAMETER}</name>{}</prm></prms>",
            parameter_value(address, id, field)
        ),
    )
}
