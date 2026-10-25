use brace::Guid;

mod cmdi;
mod configdump;
mod help;
mod misc;
mod pictures;
mod predefined;
mod rights;

pub use cmdi::CommandInterface;
pub use configdump::ConfigDumpInfo;
pub use help::Help;
pub use misc::{ContentItem, Schedule, Style};
#[doc(hidden)]
pub use misc::{MiscSource, SourceKind};
pub use pictures::Picture;
pub use predefined::{
    AccountData, AccountType, CalcTypeData, ExtDimensionType, Predefined, PredefinedCode,
    PredefinedItem,
};
pub use rights::{ObjectRights, Restriction, Right, RightTarget, RightValue, Rights};

#[derive(Debug, Clone, PartialEq)]
pub struct RawPart {
    pub node: brace::Node,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ObjectParts {
    pub rights: Option<Rights>,
    pub predefined: Option<Predefined>,
    pub command_interface: Option<CommandInterface>,
    pub help: Option<Help>,
    pub form_help: Vec<(Guid, Help)>,
    pub picture: Option<Picture>,
    pub schedule: Option<Schedule>,
    pub exchange_content: Option<Vec<ContentItem>>,
    pub style: Option<Style>,
    pub binary_modules: Vec<(String, Vec<u8>)>,
    pub ws_definition: Vec<(String, Vec<u8>)>,
    pub xdto_package: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ConfigParts {
    pub command_interface: Option<CommandInterface>,
    pub main_section: Option<CommandInterface>,
    pub home_page: Option<RawPart>,
    pub client_interface: Option<RawPart>,
    pub contained_objects: Vec<(Guid, Option<Guid>)>,
    pub dump_info: Option<ConfigDumpInfo>,
    pub pictures: Vec<(String, Picture)>,
    pub help: Option<Help>,
    pub binaries: Vec<(String, Vec<u8>)>,
}
