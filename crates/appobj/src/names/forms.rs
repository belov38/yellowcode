use super::{AppNamesImpl, CtxKind, Loc, ModuleCtx, ObjKind};
use crate::model::{AppModel, ObjId};
use bsl_prim::fold_name;
use md_model::forms::{Form, FormAttribute, FormAttributeType};
use md_model::{MdKind, SubKind};
use std::sync::Arc;

pub(super) struct FormEntry<'a> {
    pub name: String,
    pub loc: Option<Loc>,
    pub owner: Option<ObjId>,
    pub summary: Option<&'a Form>,
}

fn owner_word(kind: MdKind) -> Option<&'static str> {
    match kind {
        MdKind::SettingsStorage => Some("ХранилищеНастроек"),
        MdKind::FilterCriterion => Some("КритерийОтбора"),
        _ => ObjKind::of_md(kind).and_then(super::ru_word),
    }
}

pub(super) fn entries(model: &AppModel) -> Vec<FormEntry<'_>> {
    let mut out = Vec::new();
    for (index, object) in model.config.objects.iter().enumerate() {
        if object.kind == MdKind::CommonForm {
            out.push(FormEntry {
                name: format!("ОбщаяФорма.{}.Форма", object.name),
                loc: object
                    .modules
                    .iter()
                    .position(|m| m.kind == md_model::ModuleKind::FormModule)
                    .map(|module| Loc::Object(index, module)),
                owner: None,
                summary: object.form.as_ref(),
            });
        }
        let Some(word) = owner_word(object.kind) else {
            continue;
        };
        for (form_index, form) in object
            .forms
            .iter()
            .enumerate()
            .filter(|(_, f)| f.kind == SubKind::Form)
        {
            out.push(FormEntry {
                name: format!("{word}.{}.Форма.{}.Форма", object.name, form.name),
                loc: form.module.as_ref().map(|_| Loc::Form(index, form_index)),
                owner: model.by_uuid(object.id),
                summary: form.form.as_ref(),
            });
        }
    }
    out
}

const EXTENSIONS: [(&str, &str); 18] = [
    ("CatalogObject", "Extension for catalogs"),
    ("DocumentObject", "Extension for documents"),
    ("BusinessProcessObject", "Extension for business processes"),
    ("TaskObject", "Extension for tasks"),
    (
        "ChartOfCharacteristicTypesObject",
        "Extension for chart of characteristics types",
    ),
    ("ChartOfAccountsObject", "Extension for objects"),
    ("ChartOfCalculationTypesObject", "Extension for objects"),
    ("ExchangePlanObject", "Extension for objects"),
    ("ConstantsSet", "Extension for constants"),
    (
        "InformationRegisterRecordManager",
        "Extension for information register records",
    ),
    ("InformationRegisterRecordSet", "Extension for record set"),
    ("AccumulationRegisterRecordSet", "Extension for record set"),
    ("AccountingRegisterRecordSet", "Extension for record set"),
    ("CalculationRegisterRecordSet", "Extension for record set"),
    ("ReportObject", "Extension for reports"),
    ("DataProcessorObject", "Form extension for processings"),
    ("DynamicList", "Extension for dynamic lists"),
    ("SettingsComposer", "Extension for settings composer"),
];

impl AppNamesImpl {
    fn attribute_extension(&self, attribute: &FormAttribute) -> Option<Option<&'static str>> {
        let mut first = None;
        for identity in &attribute.types {
            let id = match identity {
                FormAttributeType::Other => {
                    first.get_or_insert(None);
                    continue;
                }
                FormAttributeType::Invalid => return None,
                FormAttributeType::Id(id) => id,
            };
            let (name, set) = if let Some(name) = self.model.config.index.types.get(id) {
                (
                    name.as_str(),
                    matches!(
                        name.split('.').next(),
                        Some("DefinedType" | "Characteristic")
                    ),
                )
            } else if let Some(entry) = mdschema::static_form_tables(false)
                .types()
                .get(&id.to_string())
            {
                let values = entry.as_array()?;
                let name = values.get(1)?.as_str()?;
                (name, values.first()?.as_str()? == "TypeSet")
            } else if let Some(ty) = mdschema::schema().ok()?.platform_type(&id.to_string()) {
                (ty.local.as_str(), ty.set)
            } else {
                let ty = platform_catalog::catalog()
                    .types()
                    .iter()
                    .find(|t| t.guid.as_ref() == Some(id.as_bytes()))?;
                (
                    ty.xdto
                        .as_ref()
                        .map_or(ty.name_en.as_str(), |x| x.name.as_str()),
                    false,
                )
            };
            if !set && first.is_none() {
                let local = name
                    .rsplit(':')
                    .next()
                    .unwrap_or(name)
                    .split('.')
                    .next()
                    .unwrap_or(name);
                first = Some(
                    EXTENSIONS
                        .iter()
                        .find(|(kind, _)| *kind == local)
                        .map(|(_, id)| *id),
                );
            }
        }
        first
    }

    pub(super) fn form_extension(&self, form: &Form) -> Option<&'static str> {
        form.attributes
            .iter()
            .filter(|a| a.main)
            .find_map(|a| self.attribute_extension(a))
            .flatten()
    }

    pub(super) fn form_context(&self, module: &str) -> Option<ModuleCtx> {
        let parts: Vec<String> = module.split('.').map(fold_name).collect();
        let words: Vec<&str> = parts.iter().map(String::as_str).collect();
        let summary = match words.as_slice() {
            ["общаяформа" | "commonform", name, "форма" | "form"] => self
                .model
                .config
                .objects
                .iter()
                .find(|o| o.kind == MdKind::CommonForm && fold_name(&o.name) == *name)
                .and_then(|o| o.form.as_ref()),
            [word, name, "форма" | "form", form, "форма" | "form"] => {
                let kind = match *word {
                    "хранилищенастроек" | "settingsstorage" => {
                        MdKind::SettingsStorage
                    }
                    "критерийотбора" | "filtercriterion" => MdKind::FilterCriterion,
                    _ => ObjKind::from_word(word)
                        .filter(|k| super::ru_word(*k).is_some())?
                        .md_kind(),
                };
                self.model
                    .config
                    .objects
                    .iter()
                    .find(|o| o.kind == kind && fold_name(&o.name) == *name)
                    .and_then(|o| {
                        o.forms
                            .iter()
                            .find(|f| f.kind == SubKind::Form && fold_name(&f.name) == *form)
                    })
                    .and_then(|f| f.form.as_ref())
            }
            _ => return None,
        };
        let extension = summary
            .and_then(|f| self.form_extension(f))
            .map(|id| {
                let key = format!("#{id}");
                if let Ok(cache) = self.templates.lock()
                    && let Some(members) = cache.get(&key)
                {
                    return members.clone();
                }
                let members = Arc::new(super::members_of(|t| t.id == id));
                if let Ok(mut cache) = self.templates.lock() {
                    cache.insert(key, members.clone());
                }
                members
            })
            .unwrap_or_default();
        Some(ModuleCtx {
            kind: CtxKind::Form,
            template: self.template_members("ФормаКлиентскогоПриложения"),
            own: summary
                .into_iter()
                .flat_map(|f| &f.attributes)
                .map(|a| fold_name(&a.name))
                .collect(),
            common_attrs: false,
            extension,
        })
    }
}
