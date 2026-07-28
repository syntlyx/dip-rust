//! Minimal YAML parser covering the subset used by Docker Compose files.
//!
//! Replaces a third-party YAML dependency: dip only ever parses compose
//! files into `serde_json::Value`, so a small, auditable parser beats a
//! full YAML implementation. Supported: block mappings and sequences,
//! plain/quoted scalars with core-schema type inference, comments, flow
//! collections (`[..]`, `{..}`, also spanning multiple lines), literal and
//! folded block scalars (`|`, `>`, with `-`/`+` chomping), anchors and
//! aliases (`&a`, `*a`) and the mapping merge key (`<<:`).
//!
//! Deliberately rejected with a clear error: tabs in indentation, tags
//! (`!!int` etc.), multi-document streams, explicit block-scalar indent
//! digits. YAML 1.1-only booleans (`yes`/`on`) stay plain strings, same
//! as any YAML 1.2 parser.

use std::collections::HashMap;
use std::fmt;

use serde_json::{Map, Number, Value};

#[derive(Debug)]
pub struct Error {
    line: usize,
    msg: String,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "YAML parse error at line {}: {}", self.line, self.msg)
    }
}

impl std::error::Error for Error {}

pub fn from_str(input: &str) -> Result<Value, Error> {
    Parser::new(input)?.parse_document()
}

#[derive(Clone, Copy)]
struct Line<'a> {
    no: usize,
    indent: usize,
    /// Content after indentation with trailing comment stripped.
    text: &'a str,
    /// Content after indentation, verbatim (for block scalars).
    raw: &'a str,
}

struct Parser<'a> {
    lines: Vec<Line<'a>>,
    pos: usize,
    anchors: HashMap<String, Value>,
}

impl<'a> Parser<'a> {
    fn new(input: &'a str) -> Result<Self, Error> {
        let mut lines = Vec::new();
        for (i, raw_line) in input.lines().enumerate() {
            let no = i + 1;
            let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
            let trimmed = line.trim_start_matches(' ');
            let indent = line.len() - trimmed.len();
            if trimmed.starts_with('\t') {
                return Err(Error {
                    line: no,
                    msg: "tabs are not allowed in indentation".into(),
                });
            }
            lines.push(Line {
                no,
                indent,
                text: strip_comment(trimmed).trim_end(),
                raw: trimmed,
            });
        }
        Ok(Parser {
            lines,
            pos: 0,
            anchors: HashMap::new(),
        })
    }

    fn err(&self, line: usize, msg: impl Into<String>) -> Error {
        Error {
            line,
            msg: msg.into(),
        }
    }

    /// Next line that has any content (skipping blank/comment-only lines).
    fn peek(&self) -> Option<Line<'a>> {
        self.lines[self.pos..]
            .iter()
            .find(|l| !l.text.is_empty())
            .copied()
    }

    fn advance_to_peeked(&mut self) {
        while self.pos < self.lines.len() && self.lines[self.pos].text.is_empty() {
            self.pos += 1;
        }
    }

    fn parse_document(&mut self) -> Result<Value, Error> {
        if let Some(line) = self.peek() {
            if line.text == "---" {
                self.advance_to_peeked();
                self.pos += 1;
            } else if line.text.starts_with('%') {
                return Err(self.err(line.no, "YAML directives are not supported"));
            }
        }
        let value = self.parse_node(0)?;
        if let Some(line) = self.peek()
            && line.text == "..."
        {
            self.advance_to_peeked();
            self.pos += 1;
        }
        if let Some(line) = self.peek() {
            let msg = if line.text == "---" {
                "multi-document YAML is not supported".to_string()
            } else {
                format!("unexpected content '{}'", line.text)
            };
            return Err(self.err(line.no, msg));
        }
        Ok(value)
    }

    /// Parse the block node starting at the next significant line, which must
    /// be indented at least `min_indent` (otherwise the node is empty/null).
    fn parse_node(&mut self, min_indent: usize) -> Result<Value, Error> {
        let Some(line) = self.peek() else {
            return Ok(Value::Null);
        };
        if line.indent < min_indent {
            return Ok(Value::Null);
        }
        if line.text == "-" || line.text.starts_with("- ") {
            return self.parse_sequence(line.indent);
        }
        if split_key(line.text, line.no)?.is_some() {
            return self.parse_mapping(line.indent);
        }
        // A single scalar / flow / alias node.
        self.advance_to_peeked();
        self.pos += 1;
        self.parse_inline_value(line.text, line.no)
    }

    fn parse_mapping(&mut self, indent: usize) -> Result<Value, Error> {
        let mut own = Map::new();
        let mut merged = Map::new();
        while let Some(line) = self.peek() {
            if line.text == "---" || line.text == "..." {
                break; // document marker — let parse_document report it
            }
            if line.indent != indent {
                if line.indent > indent {
                    return Err(self.err(line.no, "inconsistent indentation"));
                }
                break;
            }
            let Some((key, rest)) = split_key(line.text, line.no)? else {
                return Err(self.err(line.no, format!("expected 'key:', got '{}'", line.text)));
            };
            self.advance_to_peeked();
            self.pos += 1;
            let value = self.parse_value_after_key(rest, indent, line.no)?;

            if key == "<<" {
                // Merge key: explicit entries win; among multiple merge
                // sources, earlier ones win (YAML merge-key spec).
                let sources = match value {
                    Value::Object(m) => vec![Value::Object(m)],
                    Value::Array(items) => items,
                    other => {
                        return Err(self.err(
                            line.no,
                            format!("'<<' expects a mapping or list of mappings, got {other}"),
                        ));
                    }
                };
                for source in sources {
                    let Value::Object(m) = source else {
                        return Err(self.err(line.no, "'<<' merge source is not a mapping"));
                    };
                    for (k, v) in m {
                        merged.entry(k).or_insert(v);
                    }
                }
            } else {
                own.insert(key, value);
            }
        }
        if merged.is_empty() {
            return Ok(Value::Object(own));
        }
        for (k, v) in own {
            merged.insert(k, v);
        }
        Ok(Value::Object(merged))
    }

    fn parse_sequence(&mut self, indent: usize) -> Result<Value, Error> {
        let mut items = Vec::new();
        while let Some(line) = self.peek() {
            if line.indent != indent || !(line.text == "-" || line.text.starts_with("- ")) {
                break;
            }
            self.advance_to_peeked();
            if line.text == "-" {
                self.pos += 1;
                items.push(self.parse_node(indent + 1)?);
            } else {
                // `- content`: re-enter the parser as if `content` started on
                // its own line right after the dash, so `- key: value` opens
                // a mapping whose remaining keys sit at the same column.
                let rest = &line.text[2..];
                let extra = rest.len() - rest.trim_start_matches(' ').len();
                let virtual_indent = indent + 2 + extra;
                self.lines[self.pos] = Line {
                    no: line.no,
                    indent: virtual_indent,
                    text: rest.trim_start_matches(' '),
                    raw: line.raw.get(2 + extra..).unwrap_or(""),
                };
                items.push(self.parse_node(indent + 1)?);
            }
        }
        Ok(Value::Array(items))
    }

    /// Parse whatever follows `key:` — an inline value on the same line, a
    /// nested block on the following lines, a block scalar, or an anchor.
    fn parse_value_after_key(
        &mut self,
        rest: &str,
        key_indent: usize,
        line_no: usize,
    ) -> Result<Value, Error> {
        if rest.is_empty() {
            return self.parse_node(key_indent + 1);
        }
        if let Some(anchor_rest) = rest.strip_prefix('&') {
            let (name, remainder) = split_word(anchor_rest);
            if name.is_empty() {
                return Err(self.err(line_no, "anchor name missing after '&'"));
            }
            let value = if remainder.is_empty() {
                self.parse_node(key_indent + 1)?
            } else {
                self.parse_value_after_key(remainder, key_indent, line_no)?
            };
            self.anchors.insert(name.to_string(), value.clone());
            return Ok(value);
        }
        if rest.starts_with('|') || rest.starts_with('>') {
            return self.parse_block_scalar(rest, key_indent, line_no);
        }
        self.parse_inline_value(rest, line_no)
    }

    /// A value that fits on one line, except flow collections which may
    /// continue over following lines until brackets balance.
    fn parse_inline_value(&mut self, text: &str, line_no: usize) -> Result<Value, Error> {
        if let Some(alias) = text.strip_prefix('*') {
            let (name, remainder) = split_word(alias);
            if !remainder.is_empty() {
                return Err(self.err(line_no, format!("unexpected content after alias *{name}")));
            }
            return self
                .anchors
                .get(name)
                .cloned()
                .ok_or_else(|| self.err(line_no, format!("unknown anchor '{name}'")));
        }
        if let Some(anchor_rest) = text.strip_prefix('&') {
            let (name, remainder) = split_word(anchor_rest);
            if name.is_empty() || remainder.is_empty() {
                return Err(self.err(line_no, "anchor must be followed by a value"));
            }
            let value = self.parse_inline_value(remainder, line_no)?;
            self.anchors.insert(name.to_string(), value.clone());
            return Ok(value);
        }
        if text.starts_with('[') || text.starts_with('{') {
            let mut buf = text.to_string();
            while flow_depth(&buf, line_no)? > 0 {
                let Some(line) = self.peek() else {
                    return Err(self.err(line_no, "unclosed flow collection"));
                };
                self.advance_to_peeked();
                self.pos += 1;
                buf.push(' ');
                buf.push_str(line.text);
            }
            let mut flow = FlowParser {
                chars: buf.chars().collect(),
                pos: 0,
                line: line_no,
                anchors: &self.anchors,
            };
            let value = flow.parse_value()?;
            flow.skip_spaces();
            if flow.pos < flow.chars.len() {
                return Err(self.err(line_no, "unexpected content after flow collection"));
            }
            return Ok(value);
        }
        if text.starts_with('!') {
            return Err(self.err(line_no, "YAML tags are not supported"));
        }
        parse_scalar_token(text, line_no)
    }

    fn parse_block_scalar(
        &mut self,
        header: &str,
        key_indent: usize,
        line_no: usize,
    ) -> Result<Value, Error> {
        let folded = header.starts_with('>');
        let mut rest = &header[1..];
        let chomp = match rest.chars().next() {
            Some('-') => {
                rest = &rest[1..];
                Chomp::Strip
            }
            Some('+') => {
                rest = &rest[1..];
                Chomp::Keep
            }
            _ => Chomp::Clip,
        };
        if rest.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            return Err(self.err(
                line_no,
                "explicit block scalar indentation is not supported",
            ));
        }
        if !rest.trim().is_empty() {
            return Err(self.err(line_no, format!("unexpected content after '{header}'")));
        }

        // Collect raw lines deeper than the key; blanks belong to the scalar.
        let mut collected: Vec<String> = Vec::new();
        let mut block_indent: Option<usize> = None;
        while self.pos < self.lines.len() {
            let line = self.lines[self.pos];
            if line.raw.is_empty() {
                collected.push(String::new());
                self.pos += 1;
                continue;
            }
            if line.indent <= key_indent {
                break;
            }
            let bi = *block_indent.get_or_insert(line.indent);
            if line.indent < bi {
                return Err(self.err(line.no, "bad indentation inside block scalar"));
            }
            // Re-attach indentation beyond the block's base indent.
            let extra = line.indent - bi;
            collected.push(" ".repeat(extra) + line.raw);
            self.pos += 1;
        }
        // Trailing blank lines are subject to chomping, not content.
        let mut content_len = collected.len();
        while content_len > 0 && collected[content_len - 1].is_empty() {
            content_len -= 1;
        }
        let trailing_blanks = collected.len() - content_len;
        let content = &collected[..content_len];

        let mut body = if folded {
            let mut out = String::new();
            let mut prev_blank = true;
            for line in content {
                if line.is_empty() {
                    out.push('\n');
                    prev_blank = true;
                } else {
                    if !prev_blank {
                        out.push(' ');
                    }
                    out.push_str(line);
                    prev_blank = false;
                }
            }
            out
        } else {
            content.join("\n")
        };
        match chomp {
            Chomp::Strip => {}
            Chomp::Clip => {
                if !body.is_empty() {
                    body.push('\n');
                }
            }
            Chomp::Keep => {
                body.push('\n');
                for _ in 0..trailing_blanks {
                    body.push('\n');
                }
            }
        }
        Ok(Value::String(body))
    }
}

enum Chomp {
    Clip,
    Strip,
    Keep,
}

// ─── line-level helpers ───────────────────────────────────────────────────────

/// Strip a trailing ` # comment` that is not inside quotes. A `#` only starts
/// a comment at the beginning of the content or after whitespace.
fn strip_comment(text: &str) -> &str {
    let bytes = text.as_bytes();
    let mut in_single = false;
    let mut in_double = false;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\'' if !in_double => in_single = !in_single,
            b'"' if !in_single => in_double = !in_double,
            b'\\' if in_double => i += 1,
            b'#' if !in_single
                && !in_double
                && (i == 0 || bytes[i - 1] == b' ' || bytes[i - 1] == b'\t') =>
            {
                return &text[..i];
            }
            _ => {}
        }
        i += 1;
    }
    text
}

/// Split `key: rest` at the first `:` that ends the key (followed by a space
/// or end of line, outside quotes). Returns None when the line is not a
/// mapping entry (plain scalar, flow value, `8080:80`-style port string...).
fn split_key(text: &str, line_no: usize) -> Result<Option<(String, &str)>, Error> {
    if text.starts_with('[') || text.starts_with('{') || text.starts_with('#') {
        return Ok(None);
    }
    if text.starts_with('"') || text.starts_with('\'') {
        let (key, after) = take_quoted(text, line_no)?;
        let after = after.trim_start();
        if let Some(rest) = after.strip_prefix(':')
            && (rest.is_empty() || rest.starts_with(' '))
        {
            return Ok(Some((key, rest.trim_start())));
        }
        return Ok(None);
    }
    let bytes = text.as_bytes();
    for i in 0..bytes.len() {
        if bytes[i] == b':' && (i + 1 == bytes.len() || bytes[i + 1] == b' ') {
            let key = text[..i].trim_end();
            if key.is_empty() {
                return Ok(None);
            }
            return Ok(Some((key.to_string(), text[i + 1..].trim_start())));
        }
    }
    Ok(None)
}

/// First whitespace-delimited word and the rest (trimmed).
fn split_word(text: &str) -> (&str, &str) {
    match text.find(' ') {
        Some(i) => (&text[..i], text[i..].trim_start()),
        None => (text, ""),
    }
}

/// Consume a leading quoted string, returning its unescaped content and what
/// follows the closing quote.
fn take_quoted(text: &str, line_no: usize) -> Result<(String, &str), Error> {
    let mut chars = text.char_indices();
    let (_, quote) = chars.next().expect("caller checked first char");
    let mut out = String::new();
    if quote == '\'' {
        let mut iter = chars.peekable();
        while let Some((i, c)) = iter.next() {
            if c == '\'' {
                if iter.peek().is_some_and(|&(_, n)| n == '\'') {
                    out.push('\'');
                    iter.next();
                } else {
                    return Ok((out, &text[i + 1..]));
                }
            } else {
                out.push(c);
            }
        }
    } else {
        let mut iter = chars;
        while let Some((i, c)) = iter.next() {
            match c {
                '"' => return Ok((out, &text[i + 1..])),
                '\\' => match iter.next() {
                    Some((_, 'n')) => out.push('\n'),
                    Some((_, 't')) => out.push('\t'),
                    Some((_, 'r')) => out.push('\r'),
                    Some((_, '0')) => out.push('\0'),
                    Some((_, '\\')) => out.push('\\'),
                    Some((_, '"')) => out.push('"'),
                    Some((_, other)) => {
                        return Err(Error {
                            line: line_no,
                            msg: format!("unsupported escape '\\{other}'"),
                        });
                    }
                    None => break,
                },
                _ => out.push(c),
            }
        }
    }
    Err(Error {
        line: line_no,
        msg: "unterminated quoted string".into(),
    })
}

/// Net bracket depth of a flow snippet, ignoring brackets inside quotes.
fn flow_depth(text: &str, line_no: usize) -> Result<i32, Error> {
    let bytes = text.as_bytes();
    let mut depth = 0i32;
    let mut in_single = false;
    let mut in_double = false;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\'' if !in_double => in_single = !in_single,
            b'"' if !in_single => in_double = !in_double,
            b'\\' if in_double => i += 1,
            b'[' | b'{' if !in_single && !in_double => depth += 1,
            b']' | b'}' if !in_single && !in_double => depth -= 1,
            _ => {}
        }
        if depth < 0 {
            return Err(Error {
                line: line_no,
                msg: "unbalanced brackets in flow collection".into(),
            });
        }
        i += 1;
    }
    Ok(depth)
}

/// Plain-scalar type inference following the YAML 1.2 core schema.
fn parse_scalar_token(text: &str, line_no: usize) -> Result<Value, Error> {
    if text.starts_with('"') || text.starts_with('\'') {
        let (s, after) = take_quoted(text, line_no)?;
        if !after.trim().is_empty() {
            return Err(Error {
                line: line_no,
                msg: format!("unexpected content after quoted string: '{}'", after.trim()),
            });
        }
        return Ok(Value::String(s));
    }
    Ok(plain_scalar(text))
}

fn plain_scalar(text: &str) -> Value {
    match text {
        "" | "~" | "null" | "Null" | "NULL" => return Value::Null,
        "true" | "True" | "TRUE" => return Value::Bool(true),
        "false" | "False" | "FALSE" => return Value::Bool(false),
        _ => {}
    }
    let first = text.as_bytes()[0];
    if first.is_ascii_digit() || first == b'-' || first == b'+' || first == b'.' {
        if let Ok(i) = text.parse::<i64>() {
            return Value::Number(Number::from(i));
        }
        // Floats: require a digit somewhere so "-", ".", "+" stay strings.
        if text.bytes().any(|b| b.is_ascii_digit())
            && let Ok(f) = text.parse::<f64>()
            && let Some(n) = Number::from_f64(f)
        {
            return Value::Number(n);
        }
    }
    Value::String(text.to_string())
}

// ─── flow collections ─────────────────────────────────────────────────────────

struct FlowParser<'a> {
    chars: Vec<char>,
    pos: usize,
    line: usize,
    anchors: &'a HashMap<String, Value>,
}

impl FlowParser<'_> {
    fn err(&self, msg: impl Into<String>) -> Error {
        Error {
            line: self.line,
            msg: msg.into(),
        }
    }

    fn skip_spaces(&mut self) {
        while self.pos < self.chars.len() && self.chars[self.pos] == ' ' {
            self.pos += 1;
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn parse_value(&mut self) -> Result<Value, Error> {
        self.skip_spaces();
        match self.peek() {
            Some('[') => self.parse_seq(),
            Some('{') => self.parse_map(),
            Some('"') | Some('\'') => {
                let rest: String = self.chars[self.pos..].iter().collect();
                let (s, after) = take_quoted(&rest, self.line)?;
                self.pos = self.chars.len() - after.chars().count();
                Ok(Value::String(s))
            }
            Some('*') => {
                self.pos += 1;
                let name = self.take_plain_token();
                self.anchors
                    .get(name.trim())
                    .cloned()
                    .ok_or_else(|| self.err(format!("unknown anchor '{}'", name.trim())))
            }
            Some(_) => {
                let token = self.take_plain_token();
                Ok(plain_scalar(token.trim()))
            }
            None => Err(self.err("unexpected end of flow collection")),
        }
    }

    /// Consume a plain token up to the next flow separator.
    fn take_plain_token(&mut self) -> String {
        let start = self.pos;
        while let Some(c) = self.peek() {
            if matches!(c, ',' | ']' | '}' | ':') {
                if c == ':' {
                    // Only a separator when followed by space/end/flow char,
                    // so "8080:80" survives as one token.
                    let next = self.chars.get(self.pos + 1);
                    if next.is_none_or(|&n| n == ' ' || n == ',' || n == ']' || n == '}') {
                        break;
                    }
                } else {
                    break;
                }
            }
            self.pos += 1;
        }
        self.chars[start..self.pos].iter().collect()
    }

    fn parse_seq(&mut self) -> Result<Value, Error> {
        self.pos += 1; // '['
        let mut items = Vec::new();
        loop {
            self.skip_spaces();
            match self.peek() {
                Some(']') => {
                    self.pos += 1;
                    return Ok(Value::Array(items));
                }
                None => return Err(self.err("unterminated flow sequence")),
                _ => {}
            }
            items.push(self.parse_value()?);
            self.skip_spaces();
            match self.peek() {
                Some(',') => {
                    self.pos += 1;
                }
                Some(']') => {}
                other => {
                    return Err(self.err(format!("expected ',' or ']', got {other:?}")));
                }
            }
        }
    }

    fn parse_map(&mut self) -> Result<Value, Error> {
        self.pos += 1; // '{'
        let mut map = Map::new();
        loop {
            self.skip_spaces();
            match self.peek() {
                Some('}') => {
                    self.pos += 1;
                    return Ok(Value::Object(map));
                }
                None => return Err(self.err("unterminated flow mapping")),
                _ => {}
            }
            let key = match self.peek() {
                Some('"') | Some('\'') => {
                    let rest: String = self.chars[self.pos..].iter().collect();
                    let (s, after) = take_quoted(&rest, self.line)?;
                    self.pos = self.chars.len() - after.chars().count();
                    s
                }
                _ => self.take_plain_token().trim().to_string(),
            };
            self.skip_spaces();
            if self.peek() != Some(':') {
                return Err(self.err(format!("expected ':' after flow mapping key '{key}'")));
            }
            self.pos += 1;
            let value = self.parse_value()?;
            map.insert(key, value);
            self.skip_spaces();
            match self.peek() {
                Some(',') => {
                    self.pos += 1;
                }
                Some('}') => {}
                other => {
                    return Err(self.err(format!("expected ',' or '}}', got {other:?}")));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_nested_mappings_and_sequences() {
        let value = from_str(
            "services:\n  app:\n    image: nginx\n    ports:\n      - \"8080:80\"\n      - 3000:3000\n",
        )
        .unwrap();
        assert_eq!(
            value,
            json!({"services": {"app": {"image": "nginx", "ports": ["8080:80", "3000:3000"]}}})
        );
    }

    #[test]
    fn infers_core_schema_scalars() {
        let value =
            from_str("a: true\nb: null\nc: 42\nd: 3.5\ne: hello\nf: ~\ng: unless-stopped\n")
                .unwrap();
        assert_eq!(
            value,
            json!({"a": true, "b": null, "c": 42, "d": 3.5, "e": "hello", "f": null, "g": "unless-stopped"})
        );
    }

    #[test]
    fn keeps_port_strings_with_colons_as_scalars() {
        let value = from_str("ports:\n  - 8080:80\n").unwrap();
        assert_eq!(value, json!({"ports": ["8080:80"]}));
    }

    #[test]
    fn strips_comments_but_not_hashes_in_values() {
        let value = from_str("# top\na: 1 # trailing\nb: \"x # y\"\nc: pass#word\n").unwrap();
        assert_eq!(value, json!({"a": 1, "b": "x # y", "c": "pass#word"}));
    }

    #[test]
    fn parses_flow_collections_across_lines() {
        let value = from_str(
            "healthcheck:\n  test:\n    [\n      \"CMD-SHELL\",\n      \"redis-cli ping\",\n    ]\n  extra: {a: 1, b: [x, y]}\n",
        )
        .unwrap();
        assert_eq!(
            value,
            json!({"healthcheck": {"test": ["CMD-SHELL", "redis-cli ping"], "extra": {"a": 1, "b": ["x", "y"]}}})
        );
    }

    #[test]
    fn resolves_anchors_and_merge_keys() {
        let value = from_str(
            "base: &app\n  restart: always\n  image: node\nservices:\n  web:\n    <<: *app\n    image: web\n",
        )
        .unwrap();
        assert_eq!(
            value,
            json!({
                "base": {"restart": "always", "image": "node"},
                "services": {"web": {"restart": "always", "image": "web"}}
            })
        );
    }

    #[test]
    fn sequence_items_can_open_mappings() {
        let value = from_str("volumes:\n  - type: bind\n    source: /a\n    target: /b\n").unwrap();
        assert_eq!(
            value,
            json!({"volumes": [{"type": "bind", "source": "/a", "target": "/b"}]})
        );
    }

    #[test]
    fn parses_literal_and_folded_block_scalars() {
        let value = from_str("lit: |\n  line1\n  line2\nfold: >-\n  a\n  b\n").unwrap();
        assert_eq!(value, json!({"lit": "line1\nline2\n", "fold": "a b"}));
    }

    #[test]
    fn quoted_strings_unescape() {
        let value = from_str("a: \"x\\n'y'\"\nb: 'it''s'\n").unwrap();
        assert_eq!(value, json!({"a": "x\n'y'", "b": "it's"}));
    }

    #[test]
    fn rejects_tabs_tags_and_multi_documents() {
        assert!(
            from_str("a:\n\tb: 1\n")
                .unwrap_err()
                .to_string()
                .contains("tabs")
        );
        assert!(
            from_str("a: !!int 5\n")
                .unwrap_err()
                .to_string()
                .contains("tags")
        );
        assert!(
            from_str("---\na: 1\n---\nb: 2\n")
                .unwrap_err()
                .to_string()
                .contains("multi-document")
        );
    }

    #[test]
    fn reports_unknown_anchor_with_line_number() {
        let err = from_str("a: 1\nb: *nope\n").unwrap_err().to_string();
        assert!(err.contains("line 2"), "{err}");
        assert!(err.contains("nope"), "{err}");
    }

    /// Every compose file we ship must parse, and interpolation placeholders
    /// must survive as plain strings.
    #[test]
    fn parses_all_embedded_template_compose_files() {
        let mut checked = 0;
        for tmpl in crate::templates::TEMPLATES {
            if let Some(file) = tmpl.dir.get_file("docker-compose.yml") {
                let content = file.contents_utf8().unwrap();
                let value =
                    from_str(content).unwrap_or_else(|e| panic!("template {}: {e}", tmpl.name));
                assert!(value.get("services").is_some(), "{}", tmpl.name);
                checked += 1;
            }
        }
        let shared = crate::templates::shared()
            .get_file("docker-compose.yml")
            .expect("shared compose");
        from_str(shared.contents_utf8().unwrap()).expect("shared compose parses");
        assert!(
            checked > 10,
            "expected most templates to ship a compose file"
        );
    }

    /// The parser must never panic — any input returns Ok or Err. Exercises
    /// random inputs, truncations, and byte mutations of the most complex
    /// real compose file (node-multi: anchors, merge keys, flow lists).
    #[test]
    fn parser_never_panics_on_garbage() {
        let template = crate::templates::find("node-multi")
            .unwrap()
            .dir
            .get_file("docker-compose.yml")
            .unwrap()
            .contents_utf8()
            .unwrap();

        // Truncation at every byte boundary (lossy re-decode keeps it a &str).
        let bytes = template.as_bytes();
        for cut in 0..bytes.len() {
            let s = String::from_utf8_lossy(&bytes[..cut]);
            let _ = from_str(&s);
        }

        // Simple LCG for reproducible pseudo-random inputs (no rand dep).
        let mut state: u64 = 0xdead_beef_cafe_f00d;
        let mut next = move || {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (state >> 33) as u8
        };

        // Random byte soup, biased toward YAML-significant characters.
        let alphabet = b" -:#&*[]{}\"'|>\n\t.0123456789abc$";
        for _ in 0..3000 {
            let len = (next() as usize) % 120;
            let s: String = (0..len)
                .map(|_| alphabet[(next() as usize) % alphabet.len()] as char)
                .collect();
            let _ = from_str(&s);
        }

        // Single-byte mutations of the real template.
        for _ in 0..2000 {
            let mut m = bytes.to_vec();
            let pos = (next() as usize * 256 + next() as usize) % m.len();
            m[pos] = next();
            let s = String::from_utf8_lossy(&m);
            let _ = from_str(&s);
        }
    }

    /// node-multi is the anchor/merge stress test: `x-app: &app` + `<<: *app`.
    #[test]
    fn node_multi_merge_produces_full_services() {
        let file = crate::templates::find("node-multi")
            .unwrap()
            .dir
            .get_file("docker-compose.yml")
            .unwrap();
        let value = from_str(file.contents_utf8().unwrap()).unwrap();
        let web = &value["services"]["app-web"];
        // Inherited from the &app anchor via <<:
        assert_eq!(web["restart"], "unless-stopped");
        // Overridden locally
        assert!(web["ports"].is_array());
    }
}
