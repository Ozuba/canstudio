//! A pragmatic, dependency-free DBC parser. Structural statements (nodes,
//! messages, signals) are parsed line by line; `;`-terminated statements are
//! joined across lines and tokenised. Anything we do not model is preserved
//! verbatim in [`Dbc::extra`]. Malformed statements are reported but never
//! abort the parse.

use super::model::*;

/// A problem found while parsing; the offending statement is skipped.
#[derive(Clone, Debug, PartialEq)]
pub struct ParseError {
    pub line: usize,
    pub message: String,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}

/// Parse a `.dbc` document, failing on the first malformed statement.
pub fn parse(input: &str) -> Result<Dbc, String> {
    let (dbc, errors) = parse_lossy(input);
    match errors.first() {
        Some(e) => Err(e.to_string()),
        None => Ok(dbc),
    }
}

/// Parse a `.dbc` document, returning best-effort data plus every problem found.
pub fn parse_lossy(input: &str) -> (Dbc, Vec<ParseError>) {
    let mut p = Parser::default();
    p.run(input);
    p.finish()
}

#[derive(Default)]
struct Parser {
    dbc: Dbc,
    errors: Vec<ParseError>,
    // Statements referencing messages/signals are resolved after the
    // structural pass, since they may precede what they reference.
    deferred: Vec<(usize, Deferred)>,
}

enum Deferred {
    Comment(Comment),
    Values(u32, String, Vec<(i64, String)>),
    DefDefault(String, AttrValue, String),
    Attr(String, AttrTarget, AttrValue, String),
    ValType(u32, String, u8, String),
    MulVal(u32, String, ExtMux, String),
}

enum Comment {
    Network(String),
    Node(String, String),
    Message(u32, String),
    Signal(u32, String, String),
}

enum AttrTarget {
    Network,
    Node(String),
    Message(u32),
    Signal(u32, String),
}

impl Parser {
    fn err(&mut self, line: usize, message: impl Into<String>) {
        self.errors.push(ParseError { line, message: message.into() });
    }

    fn run(&mut self, input: &str) {
        let lines: Vec<&str> = input.split('\n').map(|l| l.trim_end_matches('\r')).collect();
        let mut i = 0;
        while i < lines.len() {
            let lineno = i + 1;
            let trimmed = lines[i].trim();

            if trimmed.is_empty() || trimmed.starts_with("//") {
                i += 1;
                continue;
            }

            // `NS_` introduces an indented block of new-symbol entries.
            if trimmed.starts_with("NS_ ") || trimmed == "NS_:" {
                i += 1;
                while i < lines.len() && lines[i].starts_with([' ', '\t']) && !lines[i].trim().is_empty() {
                    self.dbc.new_symbols.push(lines[i].trim().to_string());
                    i += 1;
                }
                continue;
            }

            // Bit-timing section: not editable, emitted as a fixed line on save.
            if trimmed.starts_with("BS_:") || trimmed.starts_with("BS_ :") {
                i += 1;
                continue;
            }

            if let Some(rest) = trimmed.strip_prefix("VERSION") {
                self.dbc.version = read_first_quoted(rest).map(|(s, _)| s).unwrap_or_default();
                i += 1;
                continue;
            }

            if let Some(rest) = trimmed.strip_prefix("BU_:").or_else(|| trimmed.strip_prefix("BU_ :")) {
                self.dbc.nodes.extend(rest.split_whitespace().map(Node::new));
                i += 1;
                continue;
            }

            if trimmed.starts_with("BO_ ") {
                match parse_message(trimmed) {
                    Ok(msg) => self.dbc.messages.push(msg),
                    Err(e) => self.err(lineno, e),
                }
                i += 1;
                continue;
            }

            if trimmed.starts_with("SG_ ") {
                match (self.dbc.messages.last_mut(), parse_signal(trimmed)) {
                    (Some(msg), Ok(sig)) => msg.signals.push(sig),
                    (None, _) => self.err(lineno, "SG_ without preceding BO_"),
                    (_, Err(e)) => self.err(lineno, e),
                }
                i += 1;
                continue;
            }

            // The remaining constructs are `;`-terminated and may span several
            // physical lines (multi-line comments). Join them first.
            if needs_semicolon(trimmed) {
                let (statement, consumed) = join_statement(&lines, i);
                i += consumed;
                self.statement(lineno, statement.trim());
                continue;
            }

            // Anything else: preserve verbatim.
            self.dbc.extra.push(trimmed.to_string());
            i += 1;
        }
    }

    fn statement(&mut self, line: usize, stmt: &str) {
        let (kw, rest) = stmt.split_once(char::is_whitespace).unwrap_or((stmt, ""));
        let parsed = match kw {
            "VAL_TABLE_" => parse_value_table(rest).map(|vt| self.dbc.value_tables.push(vt)),
            "VAL_" => parse_val(rest).map(|d| self.deferred.push((line, d))),
            "CM_" => parse_comment(rest).map(|c| self.deferred.push((line, Deferred::Comment(c)))),
            "BA_DEF_" => parse_attr_def(rest).map(|d| self.dbc.attribute_defs.push(d)),
            "BA_DEF_DEF_" => parse_attr_default(rest, stmt).map(|d| self.deferred.push((line, d))),
            "BA_" => parse_attr(rest, stmt).map(|d| self.deferred.push((line, d))),
            "SIG_VALTYPE_" => parse_valtype(rest, stmt).map(|d| self.deferred.push((line, d))),
            "SG_MUL_VAL_" => parse_mul_val(rest, stmt).map(|d| self.deferred.push((line, d))),
            _ => None,
        };
        if parsed.is_none() {
            self.dbc.extra.push(stmt.to_string());
        }
    }

    fn finish(mut self) -> (Dbc, Vec<ParseError>) {
        for (_, d) in std::mem::take(&mut self.deferred) {
            // A dangling reference (e.g. a default for a relation attribute,
            // or a value for a deleted signal) is kept verbatim, not lost.
            if let Err(stmt) = self.resolve(d) {
                self.dbc.extra.push(stmt);
            }
        }
        (self.dbc, self.errors)
    }

    /// Attach a deferred statement to its target; `Err(raw)` if the target is missing.
    fn resolve(&mut self, d: Deferred) -> Result<(), String> {
        let dbc = &mut self.dbc;
        match d {
            Deferred::Comment(c) => match c {
                Comment::Network(t) => dbc.comment = Some(t),
                Comment::Node(name, t) => {
                    if let Some(n) = dbc.nodes.iter_mut().find(|n| n.name == name) {
                        n.comment = Some(t);
                    }
                }
                Comment::Message(id, t) => {
                    if let Some(m) = dbc.messages.iter_mut().find(|m| m.id == id) {
                        m.comment = Some(t);
                    }
                }
                Comment::Signal(id, sig, t) => {
                    if let Some(s) = signal_mut(dbc, id, &sig) {
                        s.comment = Some(t);
                    }
                }
            },
            Deferred::Values(id, sig, values) => {
                if let Some(s) = signal_mut(dbc, id, &sig) {
                    s.value_descriptions = values;
                }
            }
            Deferred::DefDefault(name, value, raw) => {
                let def = dbc.attribute_defs.iter_mut().find(|d| d.name == name).ok_or(raw)?;
                def.default = Some(def.kind.normalize(value));
            }
            Deferred::Attr(name, target, value, raw) => {
                let value = match dbc.attribute_defs.iter().find(|d| d.name == name) {
                    Some(def) => def.kind.normalize(value),
                    None => value,
                };
                let set = match target {
                    AttrTarget::Network => Some(&mut dbc.attributes),
                    AttrTarget::Node(n) => dbc.nodes.iter_mut().find(|x| x.name == n).map(|x| &mut x.attributes),
                    AttrTarget::Message(id) => dbc.messages.iter_mut().find(|m| m.id == id).map(|m| &mut m.attributes),
                    AttrTarget::Signal(id, s) => signal_mut(dbc, id, &s).map(|s| &mut s.attributes),
                };
                set.ok_or(raw)?.push((name, value));
            }
            Deferred::ValType(id, sig, kind, raw) => {
                let s = signal_mut(dbc, id, &sig).ok_or(raw.clone())?;
                s.value_type = match kind {
                    1 => ValueType::Float32,
                    2 => ValueType::Float64,
                    _ => return Err(raw),
                };
            }
            Deferred::MulVal(id, sig, ext, raw) => {
                let s = signal_mut(dbc, id, &sig).ok_or(raw.clone())?;
                match &mut s.ext_mux {
                    // Some tools emit one statement per range.
                    Some(e) if e.switch == ext.switch => e.ranges.extend(ext.ranges),
                    Some(_) => return Err(raw),
                    slot => *slot = Some(ext),
                }
            }
        }
        Ok(())
    }
}

fn signal_mut<'a>(dbc: &'a mut Dbc, id: u32, name: &str) -> Option<&'a mut Signal> {
    dbc.messages.iter_mut().find(|m| m.id == id)?.signals.iter_mut().find(|s| s.name == name)
}

// --- structural parsers ----------------------------------------------------

fn parse_message(line: &str) -> Result<Message, String> {
    // BO_ <id> <name>: <size> <transmitter>
    let rest = line.strip_prefix("BO_ ").unwrap_or(line);
    let (id_str, rest) = rest.trim_start().split_once(char::is_whitespace).ok_or("BO_ missing name")?;
    let id: u32 = id_str.parse().map_err(|_| format!("bad message id '{id_str}'"))?;
    let (name, rest) = rest.split_once(':').ok_or("BO_ missing ':'")?;
    let mut parts = rest.split_whitespace();
    let size_str = parts.next().ok_or("BO_ missing size")?;
    let size: u64 = size_str.parse().map_err(|_| format!("bad dlc '{size_str}'"))?;
    let mut msg = Message::new(id, name.trim());
    msg.size = size;
    if let Some(tx) = parts.next() {
        msg.transmitter = tx.to_string();
    }
    Ok(msg)
}

fn parse_signal(line: &str) -> Result<Signal, String> {
    let rest = line.strip_prefix("SG_ ").unwrap_or(line);
    // Split the header (name + optional mux) from the spec at the colon.
    let (head, spec) = rest.split_once(':').ok_or("SG_ missing ':'")?;

    let mut head_toks = head.split_whitespace();
    let name = head_toks.next().ok_or("SG_ missing name")?;
    let multiplexer = match head_toks.next() {
        None => Multiplexer::None,
        Some("M") => Multiplexer::Multiplexor,
        Some(m) if m.starts_with('m') => {
            let digits = m[1..].trim_end_matches('M');
            let v: u64 = digits.parse().map_err(|_| format!("bad multiplexer '{m}'"))?;
            if m.ends_with('M') { Multiplexer::MultiplexedMultiplexor(v) } else { Multiplexer::Multiplexed(v) }
        }
        Some(m) => return Err(format!("bad multiplexer '{m}'")),
    };

    // <start>|<size>@<order><sign> (<factor>,<offset>) [<min>|<max>] "<unit>" <recv...>
    let (bitspec, rest) = spec.split_once('(').ok_or("SG_ missing '('")?;
    let (start, rest_bits) = bitspec.split_once('|').ok_or("SG_ missing '|'")?;
    let (size, order_sign) = rest_bits.split_once('@').ok_or("SG_ missing '@'")?;
    let order_sign = order_sign.trim();
    let (scale, rest) = rest.split_once(')').ok_or("SG_ missing ')'")?;
    let (factor, offset) = scale.split_once(',').ok_or("SG_ bad (factor,offset)")?;
    let (_, rest) = rest.split_once('[').ok_or("SG_ missing '['")?;
    let (range, tail) = rest.split_once(']').ok_or("SG_ missing ']'")?;
    let (min, max) = range.split_once('|').ok_or("SG_ bad [min|max]")?;

    let num = |s: &str, what: &str| -> Result<f64, String> { s.trim().parse().map_err(|_| format!("bad {what} '{}'", s.trim())) };
    let (unit, rest_after_unit) = read_first_quoted(tail).unwrap_or((String::new(), tail));
    let mut receivers: Vec<String> = rest_after_unit.split([' ', '\t', ',']).filter(|t| !t.is_empty()).map(str::to_string).collect();
    if receivers.is_empty() {
        receivers.push(VECTOR_XXX.to_string());
    }

    let mut sig = Signal::new(name, start.trim().parse().map_err(|_| "bad start bit")?);
    sig.multiplexer = multiplexer;
    sig.size = size.trim().parse().map_err(|_| "bad size")?;
    sig.byte_order = if order_sign.starts_with('0') { ByteOrder::BigEndian } else { ByteOrder::LittleEndian };
    sig.value_type = if order_sign.contains('-') { ValueType::Signed } else { ValueType::Unsigned };
    sig.factor = num(factor, "factor")?;
    sig.offset = num(offset, "offset")?;
    sig.min = num(min, "min")?;
    sig.max = num(max, "max")?;
    sig.unit = unit;
    sig.receivers = receivers;
    Ok(sig)
}

fn parse_value_table(rest: &str) -> Option<ValueTable> {
    let rest = rest.trim().trim_end_matches(';').trim();
    if rest.is_empty() {
        return None;
    }
    // A table may have no entries (`VAL_TABLE_ Name ;`).
    let (name, after) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    Some(ValueTable { name: name.to_string(), values: parse_value_pairs(after) })
}

fn parse_val(rest: &str) -> Option<Deferred> {
    let rest = rest.trim().trim_end_matches(';').trim();
    let mut it = rest.splitn(3, char::is_whitespace);
    let id: u32 = it.next()?.parse().ok()?;
    let signal = it.next()?.to_string();
    let pairs = it.next().unwrap_or("");
    Some(Deferred::Values(id, signal, parse_value_pairs(pairs)))
}

fn parse_comment(rest: &str) -> Option<Comment> {
    let body = rest.trim().trim_end_matches(';').trim();
    let text = |s: &str| read_first_quoted(s).map(|(t, _)| t);
    if let Some(r) = body.strip_prefix("BU_ ") {
        let (node, after) = take_word(r);
        Some(Comment::Node(node.to_string(), text(after)?))
    } else if let Some(r) = body.strip_prefix("BO_ ") {
        let (id, after) = take_word(r);
        Some(Comment::Message(id.parse().ok()?, text(after)?))
    } else if let Some(r) = body.strip_prefix("SG_ ") {
        let (id, after) = take_word(r);
        let (sig, after) = take_word(after);
        Some(Comment::Signal(id.parse().ok()?, sig.to_string(), text(after)?))
    } else if body.starts_with('"') {
        Some(Comment::Network(text(body)?))
    } else {
        // `CM_ EV_ ...` and friends are preserved verbatim.
        None
    }
}

// --- attributes and other `;` statements -------------------------------------

fn parse_attr_def(rest: &str) -> Option<AttributeDef> {
    let toks = lex(rest);
    let mut it = toks.iter().peekable();
    let object = match it.peek()? {
        Tok::Word(w) => {
            let o = match w.as_str() {
                "BU_" => AttrObject::Node,
                "BO_" => AttrObject::Message,
                "SG_" => AttrObject::Signal,
                "EV_" => AttrObject::EnvVar,
                _ => return None,
            };
            it.next();
            o
        }
        _ => AttrObject::Network,
    };
    let Tok::Str(name) = it.next()? else { return None };
    let Tok::Word(kind) = it.next()? else { return None };
    let mut num = || match it.next() {
        Some(Tok::Word(w)) => w.parse::<f64>().ok(),
        _ => None,
    };
    let kind = match kind.as_str() {
        "INT" => AttrKind::Int { min: num()? as i64, max: num()? as i64 },
        "HEX" => AttrKind::Hex { min: num()? as i64, max: num()? as i64 },
        "FLOAT" => AttrKind::Float { min: num()?, max: num()? },
        "STRING" => AttrKind::Str,
        "ENUM" => AttrKind::Enum(
            toks.iter()
                .filter_map(|t| match t {
                    Tok::Str(s) => Some(s.clone()),
                    _ => None,
                })
                .skip(1) // the attribute name
                .collect(),
        ),
        _ => return None,
    };
    Some(AttributeDef { name: name.clone(), object, kind, default: None })
}

fn parse_attr_default(rest: &str, raw: &str) -> Option<Deferred> {
    let toks = lex(rest);
    let [Tok::Str(name), value, ..] = toks.as_slice() else { return None };
    Some(Deferred::DefDefault(name.clone(), value.attr_value()?, raw.to_string()))
}

fn parse_attr(rest: &str, raw: &str) -> Option<Deferred> {
    let toks = lex(rest);
    let (Tok::Str(name), rest) = toks.split_first()? else { return None };
    let word = |t: &Tok| match t {
        Tok::Word(w) => Some(w.clone()),
        _ => None,
    };
    let (target, value) = match rest {
        [Tok::Word(k), n, v, ..] if k == "BU_" => (AttrTarget::Node(word(n)?), v),
        [Tok::Word(k), id, v, ..] if k == "BO_" => (AttrTarget::Message(word(id)?.parse().ok()?), v),
        [Tok::Word(k), id, s, v, ..] if k == "SG_" => (AttrTarget::Signal(word(id)?.parse().ok()?, word(s)?), v),
        [Tok::Word(k), ..] if k == "EV_" => return None,
        [v, ..] => (AttrTarget::Network, v),
        [] => return None,
    };
    Some(Deferred::Attr(name.clone(), target, value.attr_value()?, raw.to_string()))
}

fn parse_valtype(rest: &str, raw: &str) -> Option<Deferred> {
    // SIG_VALTYPE_ <id> <signal> : <0|1|2> ;
    let toks = lex(rest);
    let [Tok::Word(id), Tok::Word(sig), Tok::Punct(':'), Tok::Word(kind), ..] = toks.as_slice() else {
        return None;
    };
    Some(Deferred::ValType(id.parse().ok()?, sig.clone(), kind.parse().ok()?, raw.to_string()))
}

fn parse_mul_val(rest: &str, raw: &str) -> Option<Deferred> {
    // SG_MUL_VAL_ <id> <signal> <switch> <lo>-<hi>[, <lo>-<hi>]* ;
    let toks = lex(rest);
    let [Tok::Word(id), Tok::Word(sig), Tok::Word(switch), ranges @ ..] = toks.as_slice() else {
        return None;
    };
    let ranges = ranges
        .iter()
        .filter_map(|t| match t {
            Tok::Word(w) => {
                let (lo, hi) = w.split_once('-')?;
                Some(Some((lo.parse().ok()?, hi.parse().ok()?)))
            }
            Tok::Punct(_) => None,
            Tok::Str(_) => Some(None),
        })
        .collect::<Option<Vec<_>>>()?;
    Some(Deferred::MulVal(id.parse().ok()?, sig.clone(), ExtMux { switch: switch.clone(), ranges }, raw.to_string()))
}

// --- small lexing helpers --------------------------------------------------

#[derive(Debug)]
enum Tok {
    Word(String),
    Str(String),
    Punct(char),
}

impl Tok {
    fn attr_value(&self) -> Option<AttrValue> {
        match self {
            Tok::Str(s) => Some(AttrValue::Str(s.clone())),
            Tok::Word(w) => w.parse::<i64>().map(AttrValue::Int).or_else(|_| w.parse::<f64>().map(AttrValue::Float)).ok(),
            Tok::Punct(_) => None,
        }
    }
}

/// Tokenise a statement body into words, quoted strings and `,;:` punctuation.
fn lex(s: &str) -> Vec<Tok> {
    let mut out = Vec::new();
    let mut rest = s;
    loop {
        rest = rest.trim_start();
        let Some(c) = rest.chars().next() else { break };
        match c {
            '"' => {
                let (text, after) = read_first_quoted(rest).unwrap_or_default();
                out.push(Tok::Str(text));
                rest = after;
            }
            ',' | ';' | ':' => {
                out.push(Tok::Punct(c));
                rest = &rest[1..];
            }
            _ => {
                let end = rest.find(|c: char| c.is_whitespace() || matches!(c, ',' | ';' | ':' | '"')).unwrap_or(rest.len());
                out.push(Tok::Word(rest[..end].to_string()));
                rest = &rest[end..];
            }
        }
    }
    out
}

/// Keywords for statements terminated by `;` (and thus possibly multi-line).
fn needs_semicolon(trimmed: &str) -> bool {
    const KW: [&str; 18] = [
        "CM_",
        "VAL_TABLE_",
        "VAL_",
        "BA_DEF_DEF_REL_",
        "BA_DEF_DEF_",
        "BA_DEF_REL_",
        "BA_DEF_",
        "BA_REL_",
        "BA_",
        "SIG_GROUP_",
        "SIG_VALTYPE_",
        "BO_TX_BU_",
        "EV_DATA_",
        "ENVVAR_DATA_",
        "SG_MUL_VAL_",
        "FILTER",
        "CAT_",
        "SGTYPE_",
    ];
    KW.iter().any(|k| trimmed.starts_with(k))
}

/// Accumulate physical lines from `start` until the statement's terminating
/// `;` (the first one outside a quoted string). The statement is truncated at
/// that `;`, discarding any trailing `// line comment`. Returns the joined
/// statement and the number of physical lines consumed.
fn join_statement(lines: &[&str], start: usize) -> (String, usize) {
    let mut out = String::new();
    let mut consumed = 0;
    for line in &lines[start..] {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(line);
        consumed += 1;
        if let Some(pos) = unquoted_semicolon(&out) {
            out.truncate(pos + 1);
            break;
        }
    }
    (out, consumed)
}

/// Byte offset of the first `;` that lies outside a double-quoted string,
/// honouring `\"` escapes. `;`, `"` and `\` are ASCII so byte indexing is safe.
fn unquoted_semicolon(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut in_quote = false;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' if in_quote && bytes.get(i + 1) == Some(&b'"') => {
                i += 2;
                continue;
            }
            b'"' => in_quote = !in_quote,
            b';' if !in_quote => return Some(i),
            _ => {}
        }
        i += 1;
    }
    None
}

/// Split off the first whitespace-delimited word, returning (word, remainder).
fn take_word(s: &str) -> (&str, &str) {
    let s = s.trim_start();
    s.split_once(char::is_whitespace).unwrap_or((s, ""))
}

/// Read the first double-quoted string in `s`, honouring `\"` and `\\` escapes.
/// Returns the unescaped content and the slice following the closing quote.
fn read_first_quoted(s: &str) -> Option<(String, &str)> {
    let start = s.find('"')?;
    let inner_start = start + 1;
    let mut out = String::new();
    let mut chars = s[inner_start..].char_indices();
    while let Some((idx, c)) = chars.next() {
        // Only `\"` is an escape in DBC strings; other backslashes are literal.
        if c == '\\' && s[inner_start + idx + 1..].starts_with('"') {
            chars.next();
            out.push('"');
            continue;
        }
        if c == '"' {
            return Some((out, &s[inner_start + idx + 1..]));
        }
        out.push(c);
    }
    // Unterminated quote: take the rest.
    Some((out, ""))
}

/// Parse a sequence of `<number> "<label>"` pairs. Values beyond `i64` (e.g.
/// 64-bit all-ones) wrap, matching their raw bit pattern.
fn parse_value_pairs(s: &str) -> Vec<(i64, String)> {
    let mut out = Vec::new();
    let mut rest = s.trim();
    while !rest.is_empty() {
        let (num_tok, after_num) = take_word(rest);
        let value = match num_tok.parse::<i64>() {
            Ok(v) => v,
            Err(_) => match num_tok.parse::<u64>() {
                Ok(v) => v as i64,
                Err(_) => break,
            },
        };
        match read_first_quoted(after_num) {
            Some((label, after_label)) => {
                out.push((value, label));
                rest = after_label.trim();
            }
            None => break,
        }
    }
    out
}
