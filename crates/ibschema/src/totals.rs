use onecd::DateTime;

pub const CURRENT_TOTALS_PERIOD: DateTime = DateTime {
    year: 3999,
    month: 11,
    day: 1,
    hour: 0,
    minute: 0,
    second: 0,
};
pub const MIN_PERIOD_INITIAL: DateTime = CURRENT_TOTALS_PERIOD;
pub const SPLITTER_FILE_MODE: u32 = 0;
pub const OPT_PERIODICITY: u32 = 9;
pub const OPT_REPETITION_FACTOR: u32 = 1;
pub const USE_TOTALS_ON: u32 = 0;
pub const USE_TOTALS_OFF: u32 = 1;
pub const CALCULATED_PERIOD_NONE: DateTime = DateTime::EMPTY;

pub mod opt_field {
    pub const REG_ID: &str = "RegID";
    pub const PERIOD: &str = "Period";
    pub const ACTUAL_PERIOD: &str = "ActualPeriod";
    pub const PERIODICITY: &str = "Periodicity";
    pub const REPETITION_FACTOR: &str = "RepetitionFactor";
    pub const USE_TOTALS: &str = "UseTotals";
    pub const MIN_PERIOD: &str = "MinPeriod";
    pub const USE_SPLITTER: &str = "UseSplitter";
    pub const MIN_CALCULATED_PERIOD: &str = "MinCalculatedPeriod";
    pub const SLICE_USING: &str = "SliceUsing";
}
