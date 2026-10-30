pub const STAND_GLOBAL: &str = "ХостСтенда__";

const BLANKED: [&str; 5] = [
    "AnyGet",
    "КатегорииОшибки",
    "Предст",
    "ПредстСтрокДерева",
    "ОтветJSON",
];

fn header(line: &str) -> Option<&str> {
    let t = line.trim_start();
    let rest = t
        .strip_prefix("Функция ")
        .or_else(|| t.strip_prefix("Процедура "))?;
    let end = rest.find('(')?;
    Some(rest[..end].trim())
}

fn is_end(line: &str) -> bool {
    let t = line.trim();
    t == "КонецФункции" || t == "КонецПроцедуры"
}

fn indent(line: &str) -> &str {
    &line[..line.len() - line.trim_start().len()]
}

pub fn eval_wrapper(text: &str) -> Result<String, String> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut i = 0;
    let mut seen: Vec<&str> = Vec::new();
    while i < lines.len() {
        let line = lines[i];
        let Some(name) = header(line) else {
            out.push(line.to_owned());
            i += 1;
            continue;
        };
        let end = (i + 1..lines.len())
            .find(|&j| is_end(lines[j]))
            .ok_or_else(|| format!("{name}: no end of the body"))?;
        out.push(line.to_owned());
        let body = &lines[i + 1..end];
        seen.push(name);
        match name {
            n if BLANKED.contains(&n) => out.extend(body.iter().map(|_| String::new())),
            "ОписатьЗначение" | "ОписатьОшибку" => {
                let args = line[line.find('(').unwrap_or(0) + 1..]
                    .split(')')
                    .next()
                    .unwrap_or("")
                    .trim();
                let call = format!("\t{STAND_GLOBAL}.{name}({args});");
                if body.is_empty() {
                    return Err(format!("{name}: empty body"));
                }
                out.push(call);
                out.extend(body[1..].iter().map(|_| String::new()));
            }
            "AnyPost" => out.extend(any_post(body)?),
            "Выв__" => {
                let mut found = false;
                for b in body {
                    if b.contains("Предст(Значение)") {
                        found = true;
                        out.push(b.replace(
                            "Предст(Значение)",
                            &format!("{STAND_GLOBAL}.Предст(Значение)"),
                        ));
                    } else {
                        out.push((*b).to_owned());
                    }
                }
                if !found {
                    return Err("Выв__: no call of Предст".into());
                }
            }
            _ => out.extend(body.iter().map(|b| (*b).to_owned())),
        }
        out.push(lines[end].to_owned());
        i = end + 1;
    }
    for need in [
        "AnyPost",
        "ВыполнитьСлучай",
        "ОписатьЗначение",
        "ОписатьОшибку",
    ] {
        if !seen.contains(&need) {
            return Err(format!("{need}: not found"));
        }
    }
    let mut s = out.join("\n");
    s.push('\n');
    Ok(s)
}

fn any_post(body: &[&str]) -> Result<Vec<String>, String> {
    let mut found = [false; 5];
    let out: Vec<String> = body
        .iter()
        .map(|b| match b.trim() {
            "БезСброса = Ложь;" => (*b).to_owned(),
            "Результаты = Новый Массив;" | "Результаты.Добавить(Ответ);" | "КонецЦикла;" =>
            {
                if b.trim() == "Результаты = Новый Массив;" {
                    found[0] = true;
                } else if b.trim() == "КонецЦикла;" {
                    found[1] = true;
                } else {
                    found[2] = true;
                }
                (*b).to_owned()
            }
            "Для Каждого Случай Из Пакет.cases Цикл" => {
                found[3] = true;
                format!("{}Для Каждого Случай Из Запрос Цикл", indent(b))
            }
            "Ответ = ВыполнитьСлучай(Случай.id, Случай.code);"
            | "Ответ = ВыполнитьСлучай(Случай.id, Случай.code, БезСброса = Истина);" =>
            {
                found[4] = true;
                (*b).to_owned()
            }
            "Возврат ОтветJSON(Результаты, 200);" => {
                format!("{}Возврат Результаты;", indent(b))
            }
            _ => String::new(),
        })
        .collect();
    if found.contains(&false) {
        return Err("AnyPost: unexpected body".into());
    }
    Ok(out)
}
