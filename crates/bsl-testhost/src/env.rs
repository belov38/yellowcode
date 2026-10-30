use bsl_prim::{BslDate, Collation, CompatMode, IcuCollation, Number};
use bsl_vm::{Lang, SessionEnv};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

pub struct StandEnv {
    locale: String,
    ib_locale: String,
    en: bool,
    presentation_language: Lang,
    regional: Option<Arc<bsl_vm::IbRegionalSettings>>,
    config_language: String,
    time_zone: String,
    fixed_now: Option<BslDate>,
    collation: IcuCollation,
    compat: CompatMode,
}

impl StandEnv {
    pub fn bench_default() -> StandEnv {
        StandEnv {
            locale: "ru_RU".into(),
            ib_locale: "ru_RU".into(),
            en: false,
            presentation_language: Lang::Ru,
            regional: None,
            config_language: "ru".into(),
            time_zone: "Pacific/Auckland".into(),
            fixed_now: None,
            collation: IcuCollation::new("ru_RU"),
            compat: CompatMode::DontUse,
        }
    }

    pub fn with_locale(mut self, l: &str) -> StandEnv {
        self.locale = l.into();
        self
    }

    pub fn with_ib_locale(mut self, locale: &str) -> StandEnv {
        self.ib_locale = locale.into();
        self.collation = IcuCollation::new(locale);
        self
    }

    pub fn with_regional_settings(
        mut self,
        settings: Option<Arc<bsl_vm::IbRegionalSettings>>,
    ) -> StandEnv {
        if let Some(settings) = &settings
            && matches!(
                settings.values.get("UseCurrentSessionSettings"),
                Some(bsl_vm::SharedValue::Bool(false))
            )
            && let Some(bsl_vm::SharedValue::String(code)) = settings.values.get("LocaleCode")
            && !code.is_empty()
        {
            self.locale = String::from_utf16_lossy(code);
        }
        self.regional = settings;
        self
    }

    pub fn with_interface_en(mut self, en: bool) -> StandEnv {
        self.en = en;
        self
    }

    pub fn with_presentation_language(mut self, language: Lang) -> StandEnv {
        self.presentation_language = language;
        self
    }

    pub fn with_config_language(mut self, code: &str) -> StandEnv {
        self.config_language = code.into();
        self
    }

    pub fn for_host(host: &crate::StandHost) -> StandEnv {
        StandEnv {
            compat: host.compat(),
            ..StandEnv::bench_default().with_config_language(host.config_language())
        }
    }

    pub fn with_compat(mut self, compat: CompatMode) -> StandEnv {
        self.compat = compat;
        self
    }

    pub fn with_time_zone(mut self, tz: &str) -> StandEnv {
        self.time_zone = tz.into();
        self
    }

    pub fn with_now(mut self, now: BslDate) -> StandEnv {
        self.fixed_now = Some(now);
        self
    }
}

impl SessionEnv for StandEnv {
    fn ib_regional_settings(&self) -> Option<Arc<bsl_vm::IbRegionalSettings>> {
        self.regional.clone()
    }
    fn locale(&self) -> &str {
        &self.locale
    }
    fn ib_locale(&self) -> &str {
        &self.ib_locale
    }
    fn collation(&self) -> &dyn Collation {
        &self.collation
    }
    fn interface_language(&self) -> Lang {
        if self.en { Lang::En } else { Lang::Ru }
    }
    fn presentation_language(&self) -> Lang {
        self.presentation_language
    }
    fn config_language(&self) -> &str {
        &self.config_language
    }
    fn time_zone(&self) -> &str {
        &self.time_zone
    }
    fn now_utc(&self) -> BslDate {
        if let Some(d) = self.fixed_now {
            return d;
        }
        let ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis());
        let epoch = BslDate::from_parts(1970, 1, 1, 0, 0, 0).unwrap_or(BslDate::EMPTY);
        match Number::parse(&format!("{}.{:03}", ms / 1000, ms % 1000)) {
            Some(n) => epoch.add_seconds(&n),
            None => epoch,
        }
    }
    fn compat(&self) -> CompatMode {
        self.compat
    }
    fn code_pages(&self) -> [&str; 3] {
        ["windows-1251", "IBM866", "windows-1252"]
    }
}
