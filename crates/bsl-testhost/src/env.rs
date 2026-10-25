use bsl_prim::{BslDate, Collation, CompatMode, IcuCollation, Number};
use bsl_vm::{Lang, SessionEnv};
use std::time::{SystemTime, UNIX_EPOCH};

pub struct StandEnv {
    locale: String,
    ib_locale: String,
    en: bool,
    config_language: String,
    time_zone: String,
    fixed_now: Option<BslDate>,
    collation: IcuCollation,
}

impl StandEnv {
    pub fn bench_default() -> StandEnv {
        StandEnv {
            locale: "ru_RU".into(),
            ib_locale: "ru_RU".into(),
            en: false,
            config_language: "ru".into(),
            time_zone: "Pacific/Auckland".into(),
            fixed_now: None,
            collation: IcuCollation::new("ru_RU"),
        }
    }

    pub fn with_locale(mut self, l: &str) -> StandEnv {
        self.locale = l.into();
        self
    }

    pub fn with_interface_en(mut self, en: bool) -> StandEnv {
        self.en = en;
        self
    }

    pub fn with_config_language(mut self, code: &str) -> StandEnv {
        self.config_language = code.into();
        self
    }

    pub fn for_host(host: &crate::StandHost) -> StandEnv {
        StandEnv::bench_default().with_config_language(host.config_language())
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
        CompatMode::DontUse
    }
    fn code_pages(&self) -> [&str; 3] {
        ["windows-1251", "IBM866", "windows-1252"]
    }
}
