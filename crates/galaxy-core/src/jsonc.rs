//! A small JSON-with-comments parser for user files (`keybindings.json`,
//! later `settings.json`), M2-10.
//!
//! Accepts what VSCode accepts in these files: `//` and `/* */` comments
//! and trailing commas. Errors carry a 1-based line and column. Nesting is
//! capped so a hostile file cannot overflow the stack.

use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<Value>),
    /// Keys in file order; duplicates are kept (last one wins in `get`).
    Object(Vec<(String, Value)>),
}

impl Value {
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Object(fields) => fields.iter().rev().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Value::Null => "null",
            Value::Bool(_) => "a boolean",
            Value::Number(_) => "a number",
            Value::String(_) => "a string",
            Value::Array(_) => "an array",
            Value::Object(_) => "an object",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub line: usize,
    pub column: usize,
    pub message: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "line {}, column {}: {}",
            self.line, self.column, self.message
        )
    }
}

impl std::error::Error for ParseError {}

const MAX_DEPTH: usize = 64;

/// Parses one value; only comments and whitespace may follow it.
pub fn parse(text: &str) -> Result<Value, ParseError> {
    let mut p = Parser {
        src: text.as_bytes(),
        text,
        pos: 0,
    };
    // A UTF-8 BOM is common in files saved by other editors.
    if text.starts_with('\u{feff}') {
        p.pos = 3;
    }
    p.skip_space()?;
    let value = p.value(0)?;
    p.skip_space()?;
    if p.pos < p.src.len() {
        return Err(p.error("unexpected text after the end"));
    }
    Ok(value)
}

struct Parser<'a> {
    src: &'a [u8],
    text: &'a str,
    pos: usize,
}

impl Parser<'_> {
    fn error(&self, message: impl Into<String>) -> ParseError {
        let before = &self.text[..self.pos.min(self.text.len())];
        let line = before.matches('\n').count() + 1;
        let column = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
        ParseError {
            line,
            column,
            message: message.into(),
        }
    }

    fn peek(&self) -> Option<u8> {
        self.src.get(self.pos).copied()
    }

    fn skip_space(&mut self) -> Result<(), ParseError> {
        loop {
            match self.peek() {
                Some(b' ' | b'\t' | b'\n' | b'\r') => self.pos += 1,
                Some(b'/') if self.src.get(self.pos + 1) == Some(&b'/') => {
                    while let Some(c) = self.peek() {
                        if c == b'\n' {
                            break;
                        }
                        self.pos += 1;
                    }
                }
                Some(b'/') if self.src.get(self.pos + 1) == Some(&b'*') => {
                    let start = self.pos;
                    self.pos += 2;
                    loop {
                        match self.peek() {
                            None => {
                                self.pos = start;
                                return Err(self.error("comment is not closed"));
                            }
                            Some(b'*') if self.src.get(self.pos + 1) == Some(&b'/') => {
                                self.pos += 2;
                                break;
                            }
                            Some(_) => self.pos += 1,
                        }
                    }
                }
                _ => return Ok(()),
            }
        }
    }

    fn value(&mut self, depth: usize) -> Result<Value, ParseError> {
        if depth > MAX_DEPTH {
            return Err(self.error("nested too deeply"));
        }
        match self.peek() {
            None => Err(self.error("unexpected end of file")),
            Some(b'{') => self.object(depth),
            Some(b'[') => self.array(depth),
            Some(b'"') => self.string().map(Value::String),
            Some(b't') => self.word("true", Value::Bool(true)),
            Some(b'f') => self.word("false", Value::Bool(false)),
            Some(b'n') => self.word("null", Value::Null),
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(_) => Err(self.error("expected a value")),
        }
    }

    fn word(&mut self, word: &str, value: Value) -> Result<Value, ParseError> {
        if self.src[self.pos..].starts_with(word.as_bytes()) {
            self.pos += word.len();
            Ok(value)
        } else {
            Err(self.error("expected a value"))
        }
    }

    fn number(&mut self) -> Result<Value, ParseError> {
        let start = self.pos;
        while let Some(b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9') = self.peek() {
            self.pos += 1;
        }
        self.text[start..self.pos]
            .parse::<f64>()
            .map(Value::Number)
            .map_err(|_| {
                self.pos = start;
                self.error("invalid number")
            })
    }

    fn string(&mut self) -> Result<String, ParseError> {
        let start = self.pos;
        self.pos += 1; // opening quote
        let mut out = String::new();
        loop {
            let run = self.pos;
            while let Some(c) = self.peek() {
                if c == b'"' || c == b'\\' || c < 0x20 {
                    break;
                }
                self.pos += 1;
            }
            out.push_str(&self.text[run..self.pos]);
            match self.peek() {
                None => {
                    self.pos = start;
                    return Err(self.error("string is not closed"));
                }
                Some(b'"') => {
                    self.pos += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.pos += 1;
                    let c = self
                        .peek()
                        .ok_or_else(|| self.error("string is not closed"))?;
                    self.pos += 1;
                    match c {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => out.push(self.unicode_escape()?),
                        _ => {
                            self.pos -= 2;
                            return Err(self.error("invalid escape"));
                        }
                    }
                }
                Some(_) => return Err(self.error("control character in string")),
            }
        }
    }

    fn hex4(&mut self) -> Result<u32, ParseError> {
        let digits = self
            .text
            .get(self.pos..self.pos + 4)
            .ok_or_else(|| self.error("invalid \\u escape"))?;
        let v = u32::from_str_radix(digits, 16).map_err(|_| self.error("invalid \\u escape"))?;
        self.pos += 4;
        Ok(v)
    }

    fn unicode_escape(&mut self) -> Result<char, ParseError> {
        let hi = self.hex4()?;
        let code = if (0xD800..0xDC00).contains(&hi) {
            // A surrogate pair: `\uD83D\uDE00`.
            if !self.src[self.pos..].starts_with(b"\\u") {
                return Err(self.error("unpaired surrogate in \\u escape"));
            }
            self.pos += 2;
            let lo = self.hex4()?;
            if !(0xDC00..0xE000).contains(&lo) {
                return Err(self.error("unpaired surrogate in \\u escape"));
            }
            0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
        } else {
            hi
        };
        char::from_u32(code).ok_or_else(|| self.error("invalid \\u escape"))
    }

    fn array(&mut self, depth: usize) -> Result<Value, ParseError> {
        self.pos += 1;
        let mut items = Vec::new();
        loop {
            self.skip_space()?;
            if self.peek() == Some(b']') {
                self.pos += 1;
                return Ok(Value::Array(items));
            }
            items.push(self.value(depth + 1)?);
            self.skip_space()?;
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b']') => {}
                _ => return Err(self.error("expected , or ]")),
            }
        }
    }

    fn object(&mut self, depth: usize) -> Result<Value, ParseError> {
        self.pos += 1;
        let mut fields = Vec::new();
        loop {
            self.skip_space()?;
            match self.peek() {
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(Value::Object(fields));
                }
                Some(b'"') => {}
                _ => return Err(self.error("expected a \"key\" or }")),
            }
            let key = self.string()?;
            self.skip_space()?;
            if self.peek() != Some(b':') {
                return Err(self.error("expected :"));
            }
            self.pos += 1;
            self.skip_space()?;
            let value = self.value(depth + 1)?;
            fields.push((key, value));
            self.skip_space()?;
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b'}') => {}
                _ => return Err(self.error("expected , or }")),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_json_with_comments_and_trailing_commas() {
        let v = parse(
            "\u{feff}// keys\n[\n  { \"key\": \"cmd+k\", /* inline */ \"n\": -1.5e2, \"ok\": true, },\n  null,\n]\n",
        )
        .unwrap();
        let Value::Array(items) = &v else {
            panic!("{v:?}")
        };
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].get("key").and_then(Value::as_str), Some("cmd+k"));
        assert_eq!(items[0].get("n"), Some(&Value::Number(-150.0)));
        assert_eq!(items[0].get("ok"), Some(&Value::Bool(true)));
        assert_eq!(items[1], Value::Null);
    }

    #[test]
    fn decodes_escapes_including_surrogate_pairs() {
        let v = parse(r#""a\"b\\c\n\u00e9\uD83D\uDE00""#).unwrap();
        assert_eq!(v, Value::String("a\"b\\c\né😀".into()));
        assert!(parse(r#""\uD83D""#).is_err());
        assert!(parse(r#""\x""#).is_err());
    }

    #[test]
    fn errors_point_at_the_problem() {
        let e = parse("[\n  { \"key\": \"a\" \"command\": \"b\" }\n]").unwrap_err();
        // Points at the second key, where the comma is missing.
        assert_eq!((e.line, e.column), (2, 16), "{e}");
        assert_eq!(e.message, "expected , or }");
        assert!(parse("[1, 2").is_err());
        assert!(parse("/* open").unwrap_err().message.contains("comment"));
        assert!(parse("[] x").unwrap_err().message.contains("after the end"));
        assert!(parse("\"é").unwrap_err().message.contains("not closed"));
    }

    #[test]
    fn deep_nesting_is_refused_without_overflowing() {
        let deep = "[".repeat(10_000);
        assert_eq!(parse(&deep).unwrap_err().message, "nested too deeply");
    }
}
