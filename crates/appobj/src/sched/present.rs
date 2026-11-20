use super::schedule::Plain;

const NBSP: char = '\u{a0}';

const MONTHS: [&str; 12] = [
    "января",
    "февраля",
    "марта",
    "апреля",
    "мая",
    "июня",
    "июля",
    "августа",
    "сентября",
    "октября",
    "ноября",
    "декабря",
];

fn grouped(n: i64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(NBSP);
        }
        out.push(c);
    }
    out
}

const WEEK_DAYS: [&str; 7] = [
    "понедельник",
    "вторник",
    "среда",
    "четверг",
    "пятница",
    "суббота",
    "воскресенье",
];

const MONTH_NAMES: [&str; 12] = [
    "Январь",
    "Февраль",
    "Март",
    "Апрель",
    "Май",
    "Июнь",
    "Июль",
    "Август",
    "Сентябрь",
    "Октябрь",
    "Ноябрь",
    "Декабрь",
];

fn agreed(n: i64) -> usize {
    let (last, teen) = (n % 10, (11..=14).contains(&(n % 100)));
    match last {
        1 if !teen => 0,
        2..=4 if !teen => 1,
        _ => 2,
    }
}

fn every_seconds(n: i64) -> String {
    match (n, agreed(n)) {
        (1, _) => "каждую секунду".to_string(),
        (_, 0) => format!("каждую {} секунду", grouped(n)),
        (_, 1) => format!("каждые {} секунды", grouped(n)),
        _ => format!("каждые {} секунд", grouped(n)),
    }
}

fn seconds(n: i64, what: &str) -> Result<String, String> {
    let teen = (11..=14).contains(&(n % 100));
    if teen || !(1..=4).contains(&(n % 10)) {
        Ok(format!("{} секунд", grouped(n)))
    } else {
        Err(format!(
            "{what}: форма слова «секунда» для {n} не наблюдалась"
        ))
    }
}

fn weeks(n: i64) -> Result<String, String> {
    if n >= 1000 {
        return Err(format!("ПериодНедель {n} не наблюдался"));
    }
    Ok(match agreed(n) {
        0 => format!("каждую {n} неделю"),
        1 => format!("каждые {n} недели"),
        _ => format!("каждые {n} недель"),
    })
}

fn names(items: &[i64], of: &[&str]) -> Result<String, String> {
    let v = items
        .iter()
        .map(|i| {
            usize::try_from(*i - 1)
                .ok()
                .and_then(|i| of.get(i))
                .copied()
                .ok_or_else(|| format!("номер {i}"))
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(format!("[{}]", v.join(", ")))
}

pub(crate) fn unseen_combination(p: &Plain) -> Result<(), String> {
    let lists = !p.week_days.is_empty() || !p.months.is_empty();
    if lists
        && (p.week_days.len() * p.months.len() != 0
            || p.days_repeat_period == 0
            || p.weeks_period > 1
            || p.day_in_month != 0
            || p.week_day_in_month != 0)
    {
        return Err(
            "неполный список дней недели или месяцев вместе с другими условиями дней".into(),
        );
    }
    if p.detailed && (p.repeat_period_in_day > 0 || p.completion_time.is_some()) {
        return Err("детальное расписание вместе с повтором или завершением".into());
    }
    Ok(())
}

fn date(days: i64) -> Result<String, String> {
    let d = bsl_vm::BslDate::from_ticks(days * 86_400 * bsl_vm::BslDate::TICKS_PER_SECOND)
        .ok_or("дата вне диапазона")?
        .parts();
    let month = MONTHS
        .get((d.month as usize).wrapping_sub(1))
        .ok_or("месяц")?;
    Ok(format!("{} {month} {:04}{NBSP}г.", d.day, d.year))
}

fn time(s: i64) -> String {
    format!("{}:{:02}:{:02}", s / 3600, s % 3600 / 60, s % 60)
}

pub(crate) fn text(p: &Plain) -> Result<String, String> {
    let mut head = Vec::new();
    if let Some(d) = p.begin_date {
        head.push(format!("c {}", date(d)?));
    }
    if let Some(d) = p.end_date {
        head.push(format!("по {}", date(d)?));
    }
    let mut days = match p.days_repeat_period {
        0 => "один день".to_string(),
        1 => "каждый день".to_string(),
        n => format!("каждый {}-й день", grouped(n)),
    };
    unseen_combination(p)?;
    if p.days_repeat_period != 0 && p.weeks_period > 1 {
        days.push_str(", ");
        days.push_str(&weeks(p.weeks_period)?);
    }
    match p.day_in_month {
        0 => {}
        -1 => days.push_str(", последнего числа месяца"),
        n if n > 0 => days.push_str(&format!(", {n}-го числа месяца")),
        n => days.push_str(&format!(", {}-го дня с конца месяца", -n)),
    }
    match p.week_day_in_month {
        0 => {}
        -1 => days.push_str(", последнего дня недели в месяце"),
        n if n > 0 => days.push_str(&format!(", {n}-го дня недели в месяце")),
        n => days.push_str(&format!(", {}-го дня недели с конца месяца", -n)),
    }
    if !p.week_days.is_empty() {
        days.push_str(&format!(
            ", по дням недели {}",
            names(&p.week_days, &WEEK_DAYS)?
        ));
    }
    if !p.months.is_empty() {
        days.push_str(&format!(", по месяцам {}", names(&p.months, &MONTH_NAMES)?));
    }
    head.push(days);
    let mut day = Vec::new();
    match (p.begin_time, p.end_time) {
        (Some(a), b) => {
            day.push(format!("с {}", time(a)));
            if let Some(b) = b {
                day.push(format!("по {}", time(b)));
            }
        }
        (None, Some(b)) => day.push(format!("по {}", time(b))),
        (None, None) => {}
    }
    if p.repeat_period_in_day > 0 {
        day.push(every_seconds(p.repeat_period_in_day));
    } else {
        day.push("один раз в день".into());
    }
    let mut tail = day.join(" ");
    if let Some(c) = p.completion_time {
        tail.push_str(&format!(", завершать после {}", time(c)));
    }
    if p.completion_interval > 0 {
        tail.push_str(&format!(
            ", завершать через {}",
            seconds(p.completion_interval, "ИнтервалЗавершения")?
        ));
    }
    if p.repeat_pause > 0 && p.repeat_period_in_day > 0 {
        tail.push_str(&format!(
            ", повторять после завершения через {}",
            seconds(p.repeat_pause, "ПаузаПовтора")?
        ));
    }
    Ok(format!("{}; {tail}", head.join(" ")))
}
