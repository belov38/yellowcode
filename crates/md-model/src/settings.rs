use crate::{Configuration, MdKind, MdObject, MdRef};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ScriptVariant {
    #[default]
    English,
    Russian,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum DataLockControlMode {
    #[default]
    Automatic,
    Managed,
    AutomaticAndManaged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ObjectAutonumerationMode {
    #[default]
    AutoFree,
    NotAutoFree,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct CompatibilityMode(pub u32);

impl CompatibilityMode {
    pub fn xml_name(self) -> String {
        match self.0 {
            0 => "Version8_2_16".into(),
            1 => "Version8_1".into(),
            2 => "Version8_2_13".into(),
            n => format!("Version{}_{}_{}", n / 10000, (n / 100) % 100, n % 100),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ConfigSettings {
    pub name: String,
    pub script_variant: ScriptVariant,
    pub default_language: MdRef,
    pub compatibility_mode: CompatibilityMode,
    pub data_lock_control_mode: DataLockControlMode,
    pub object_autonumeration_mode: ObjectAutonumerationMode,
}

impl Configuration {
    pub fn settings(&self) -> &ConfigSettings {
        &self.settings
    }
    pub fn script_variant(&self) -> ScriptVariant {
        self.settings.script_variant
    }
    pub fn default_language(&self) -> Option<&MdObject> {
        let id = self.settings.default_language.id?;
        self.object(id).filter(|o| o.kind == MdKind::Language)
    }
    pub fn compatibility_mode(&self) -> CompatibilityMode {
        self.settings.compatibility_mode
    }
    pub fn data_lock_control_mode(&self) -> DataLockControlMode {
        self.settings.data_lock_control_mode
    }
    pub fn object_autonumeration_mode(&self) -> ObjectAutonumerationMode {
        self.settings.object_autonumeration_mode
    }
}
