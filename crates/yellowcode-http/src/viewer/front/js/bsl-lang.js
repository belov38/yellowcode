const BSL_WORDS = new Set("если if тогда then иначеесли elsif иначе else конецесли endif для for каждого each из in по to пока while цикл do конеццикла enddo процедура procedure функция function конецпроцедуры endprocedure конецфункции endfunction перем var перейти goto возврат return продолжить continue прервать break и and или or не not попытка try исключение except вызватьисключение raise конецпопытки endtry новый new выполнить execute знач val экспорт export истина true ложь false неопределено undefined null добавитьобработчик addhandler удалитьобработчик removehandler".split(" "));
const QUERY_WORDS = new Set("acos add all allowed and as asc asin atan autoorder avg beginofperiod between boolean by case cast characteristics cos count date dateadd datediff datetime day dayofyear desc distinct drop else emptytable end endofperiod escape exp false for from full group groupedby grouping halfyear having hierarchy hour in index inner int into is isnull join keyfield left like log log10 lower max min minute month namefield not null number objectfield on only or order outer overall periods pow presentation quarter recordautonumber refs right round second select sets sin sqrt strfind string stringlength strreplace substring sum tendays then top totals trimall triml trimr true type typefield undefined union unique update upper uuid value valuefield valuetype valuetypefield week weekday when where year автономерзаписи автоупорядочивание булево в внешнее внутреннее возр врег все выбор выбрать выразить где год группирующим дата датавремя декада день деньгода деньнедели длинастроки для добавить добавитькдате есть естьnull значение и иерархии иерархия из изменения или имеющие иначе индексировать истина итоги как квартал когда количество конец конецпериода лев левое ложь максимум между месяц минимум минута наборам началопериода не неделя неопределено нрег общие объединить окр первые периодами по подобно подстрока полевида полезначения полеимени полеключа полеобъекта полетипазначения полное полугодие поместить прав правое представление пустаятаблица различные разностьдат разрешенные сгруппированопо сгруппировать секунда соединение сокрл сокрлп сокрп спецсимвол среднее ссылка стрзаменить стрнайти строка сумма тип типзначения тогда только убыв уникально уникальныйидентификатор уничтожить упорядочить характеристики цел час число".split(" "));
const WORD = /[\p{L}_][\p{L}\p{N}_]*/u;

function stringTail(stream, state) {
  while (!stream.eol()) {
    if (stream.next() !== '"') continue;
    if (stream.eat('"')) continue;
    state.string = false;
    if (!state.query && stream.match(/\s*(?=")/)) { stream.next(); continue; }
    return "str";
  }
  state.string = true;
  return "str";
}

export function tokenizer(query = false) {
  return {
    name: query ? "1C query" : "BSL",
    startState: () => ({ string: false, query }),
    token(stream, state) {
      if (state.string) {
        if (stream.sol()) {
          if (!state.query && !/^\s*\|/.test(stream.string)) {
            if (/^\s*(?:\/\/.*)?$/.test(stream.string)) { stream.skipToEnd(); return "str"; }
            state.string = false;
          } else if (!state.query) stream.match(/\s*\|/);
        }
        if (state.string) return stringTail(stream, state);
      }
      if (stream.eatSpace()) return null;
      if (stream.match("//")) { stream.skipToEnd(); return "com"; }
      if (!query && stream.eat("#")) { stream.skipToEnd(); return "pre"; }
      if (!query && stream.match(/&[\p{L}_][\p{L}\p{N}_]*/u)) return "ann";
      if (stream.eat('"')) return stringTail(stream, state);
      if (stream.match(/'[^'\r\n]*'/)) return "date";
      if (stream.match(/\d+(?:\.\d*)?/)) return "num";
      if (stream.match(WORD)) return (query ? QUERY_WORDS : BSL_WORDS).has(stream.current().toLowerCase()) ? "kw" : "ident";
      if (stream.match(/[+*/%=<>()[\],;?.~:&-]/)) return "op";
      stream.next();
      return null;
    },
    tokenTable: { kw: "keyword", str: "string", num: "number", com: "comment", pre: "meta", ann: "annotation", ident: "variableName", op: "operator", date: "literal" },
    languageData: { commentTokens: { line: "//" } },
  };
}
export const bsl = tokenizer();
export const query = tokenizer(true);
