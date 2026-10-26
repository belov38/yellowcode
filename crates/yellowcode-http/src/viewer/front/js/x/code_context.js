const IDENT = /[\p{L}_][\p{L}\p{N}_]*/uy;
const MAX_TEXT = 256 * 1024;
const isNew = (s) => /^(новый|new)$/iu.test(s || '');

function tokensBefore(text, pos) {
  if (!Number.isInteger(pos) || pos < 0 || pos > text.length || text.length > MAX_TEXT) return null;
  text = text.slice(0, pos);
  const tokens = [];
  let i = 0;
  while (i < pos) {
    const from = i, c = text[i];
    if (/\s/u.test(c)) { i++; continue; }
    if (text.startsWith('//', i) || c === '#' || c === '&') {
      const end = text.indexOf('\n', i);
      if (end < 0 || end >= pos) return null;
      i = end + 1; continue;
    }
    if (c === '"' || c === "'") {
      i++;
      let closed = false;
      while (i < pos) {
        if (text[i] === c) {
          if (c === '"' && text[i + 1] === '"' && i + 1 < pos) { i += 2; continue; }
          i++; closed = true; break;
        }
        if (text[i] === '\n') {
          i++;
          if (c === "'") { closed = true; break; }
          const tail = text.slice(i, pos), trivia = /^(?:[^\S\n]*(?:\/\/[^\n]*)?\n)*[^\S\n]*/u.exec(tail)[0];
          i += trivia.length;
          if (i >= pos) return null;
          if (text[i] !== '|') { closed = true; break; }
          i++;
        } else i++;
      }
      if (!closed) return null;
      tokens.push({kind: 'literal', text: '', from, to: i}); continue;
    }
    IDENT.lastIndex = i;
    const word = IDENT.exec(text);
    if (word && IDENT.lastIndex <= pos) {
      i = IDENT.lastIndex; tokens.push({kind: 'word', text: word[0], from, to: i}); continue;
    }
    if (/\d/u.test(c)) {
      while (i < pos && /[\d.]/u.test(text[i])) i++;
      tokens.push({kind: 'literal', text: '', from, to: i}); continue;
    }
    tokens.push({kind: c, text: c, from, to: ++i});
  }
  return tokens;
}

function chain(tokens, end) {
  const parts = [];
  let i = end;
  while (i >= 0 && tokens[i].kind === 'word') {
    if (parts.length >= 16 || [...tokens[i].text].length > 128) return null;
    parts.unshift(tokens[i].text); i--;
    if (tokens[i]?.kind !== '.') break;
    i--;
    if (tokens[i]?.kind !== 'word') return null;
  }
  if (!parts.length || tokens[i]?.kind === '.') return null;
  return {parts, before: i};
}

export function completionContext(text, pos) {
  const tokens = tokensBefore(text, pos);
  if (!tokens) return null;
  let end = tokens.length - 1, prefix = '', from = pos;
  if (tokens[end]?.kind === 'word' && tokens[end].to === pos) {
    prefix = tokens[end].text; from = tokens[end].from; end--;
  }
  if ([...prefix].length > 128) return null;
  let path = [];
  if (tokens[end]?.kind === '.') {
    const found = chain(tokens, end - 1);
    if (!found) return null;
    path = found.parts;
  } else if (!prefix && tokens[end]?.kind === 'literal') return null;
  const constructors = !path.length && isNew(tokens[end]?.text);
  return {path, prefix, from, constructors};
}

export function callContext(text, pos) {
  const tokens = tokensBefore(text, pos);
  if (!tokens) return null;
  const stack = [];
  for (let i = 0; i < tokens.length; i++) {
    const t = tokens[i];
    if (t.kind === '(' || t.kind === '[') {
      let found = t.kind === '(' ? chain(tokens, i - 1) : null;
      if (!found && t.kind === '(' && tokens[i - 1]?.kind === 'word' &&
          tokens[i - 2]?.kind === '.' && [')', ']'].includes(tokens[i - 3]?.kind)) {
        found = {parts: [tokens[i - 1].text], before: i - 2};
      }
      stack.push({kind: t.kind, from: t.from, argument: 0, found});
    } else if (t.kind === ')' || t.kind === ']') {
      if (stack.at(-1)?.kind !== (t.kind === ')' ? '(' : '[')) return null;
      stack.pop();
    } else if (t.kind === ',') {
      if (stack.length) stack.at(-1).argument++;
    } else if (t.kind === ';') stack.length = 0;
  }
  for (let i = stack.length - 1; i >= 0; i--) {
    const t = stack[i];
    if (!t.found) continue;
    const path = t.found.parts.slice(), name = path.pop();
    return {path, name, argument: t.argument, from: t.from,
      constructors: !path.length && isNew(tokens[t.found.before]?.text)};
  }
  return null;
}

function receiverLanguage(tokens, end) {
  let i = end, budget = 32768;
  for (let hops = 0; i >= 0 && hops < 64 && budget > 0; hops++) {
    const token = tokens[i];
    if (token.kind === 'word') {
      if (tokens[i - 1]?.kind !== '.') return token.text;
      i -= 2; continue;
    }
    if (token.kind !== ')' && token.kind !== ']') return '';
    const stack = [token.kind];
    let open = i;
    while (open > 0 && stack.length && budget-- > 0) {
      const kind = tokens[--open].kind;
      if (kind === ')' || kind === ']') stack.push(kind);
      else if (kind === '(' || kind === '[') {
        if (stack.pop() !== (kind === '(' ? ')' : ']')) return '';
      }
    }
    if (stack.length) return '';
    const before = tokens[open - 1]?.kind;
    i = before === 'word' || before === ')' || before === ']' ? open - 1 : i - 1;
  }
  return '';
}

export function analysisContext(text, pos) {
  const tokens = tokensBefore(text, pos);
  if (!tokens) return null;
  let end = tokens.length - 1, prefix = '', from = pos;
  if (tokens[end]?.kind === 'word' && tokens[end].to === pos) {
    prefix = tokens[end].text; from = tokens[end].from; end--;
  }
  const member = tokens[end]?.kind === '.';
  const constructors = !member && isNew(tokens[end]?.text);
  if (!prefix && tokens[end]?.kind === 'literal') return null;
  const languageHint = prefix || (member ? receiverLanguage(tokens, end - 1) : tokens[end]?.text) || '';
  return {prefix, from, member, constructors, languageHint};
}
