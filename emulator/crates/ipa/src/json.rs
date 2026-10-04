//! A very small JSON reader/writer.
//!
//! Two things need JSON and neither needs a JSON *library*: the manifest the
//! importer writes next to an imported bundle, and the responses the preview
//! server's import panel reads.  Both are flat objects of strings and numbers,
//! so this is a hundred lines of recursive descent rather than a dependency —
//! the workspace has none, and `cargo build --offline` has to keep working.

use crate::error::{IpaError, Result};

#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Number(f64),
    Str(String),
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}

impl Json {
    pub fn str(value: impl Into<String>) -> Json {
        Json::Str(value.into())
    }

    pub fn number(value: impl Into<f64>) -> Json {
        Json::Number(value.into())
    }

    /// An object built from pairs; `None` values are skipped so the manifest
    /// only carries what was actually found.
    pub fn object(fields: Vec<(String, Option<Json>)>) -> Json {
        Json::Object(fields.into_iter().filter_map(|(key, value)| value.map(|v| (key, v))).collect())
    }

    pub fn optional(value: Option<String>) -> Option<Json> {
        value.map(Json::Str)
    }

    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Object(fields) => fields.iter().find(|(name, _)| name == key).map(|(_, value)| value),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Json::Number(value) => Some(*value as u64),
            Json::Str(value) => value.parse().ok(),
            _ => None,
        }
    }

    pub fn write(&self, out: &mut String) {
        match self {
            Json::Null => out.push_str("null"),
            Json::Bool(true) => out.push_str("true"),
            Json::Bool(false) => out.push_str("false"),
            Json::Number(value) => {
                if value.fract() == 0.0 && value.abs() < 1e15 {
                    out.push_str(&(*value as i64).to_string());
                } else {
                    out.push_str(&value.to_string());
                }
            }
            Json::Str(value) => {
                out.push('"');
                escape_into(value, out);
                out.push('"');
            }
            Json::Array(items) => {
                out.push('[');
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    item.write(out);
                }
                out.push(']');
            }
            Json::Object(fields) => {
                out.push('{');
                for (index, (key, value)) in fields.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    out.push('"');
                    escape_into(key, out);
                    out.push_str("\":");
                    value.write(out);
                }
                out.push('}');
            }
        }
    }

    pub fn to_string_pretty(&self) -> String {
        let mut compact = String::new();
        self.write(&mut compact);
        let mut pretty = String::new();
        let mut indent = 0usize;
        let mut in_string = false;
        let mut escaped = false;
        for character in compact.chars() {
            if in_string {
                pretty.push(character);
                if escaped {
                    escaped = false;
                } else if character == '\\' {
                    escaped = true;
                } else if character == '"' {
                    in_string = false;
                }
                continue;
            }
            match character {
                '"' => {
                    in_string = true;
                    pretty.push(character);
                }
                '{' | '[' => {
                    indent += 1;
                    pretty.push(character);
                    pretty.push('\n');
                    pretty.push_str(&"  ".repeat(indent));
                }
                '}' | ']' => {
                    indent = indent.saturating_sub(1);
                    pretty.push('\n');
                    pretty.push_str(&"  ".repeat(indent));
                    pretty.push(character);
                }
                ',' => {
                    pretty.push_str(",\n");
                    pretty.push_str(&"  ".repeat(indent));
                }
                ':' => pretty.push_str(": "),
                _ => pretty.push(character),
            }
        }
        pretty
    }
}

fn escape_into(value: &str, out: &mut String) {
    for character in value.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
}

/// Escape a string so it can be embedded in JSON.
pub fn escape(value: &str) -> String {
    let mut out = String::new();
    escape_into(value, &mut out);
    out
}

pub fn parse(input: &str) -> Result<Json> {
    let mut parser = Parser { chars: input.chars().collect(), pos: 0 };
    parser.skip_ws();
    let value = parser.value()?;
    parser.skip_ws();
    if parser.pos != parser.chars.len() {
        return Err(IpaError::Corrupt {
            what: format!("trailing data after the JSON value at offset {}", parser.pos),
        });
    }
    Ok(value)
}

struct Parser {
    chars: Vec<char>,
    pos: usize,
}

impl Parser {
    fn skip_ws(&mut self) {
        while self.chars.get(self.pos).is_some_and(|c| c.is_whitespace()) {
            self.pos += 1;
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn expect(&mut self, character: char) -> Result<()> {
        if self.peek() == Some(character) {
            self.pos += 1;
            Ok(())
        } else {
            Err(IpaError::Corrupt {
                what: format!("expected {character:?} in JSON at offset {}", self.pos),
            })
        }
    }

    fn literal(&mut self, word: &str) -> Result<()> {
        let letters: Vec<char> = word.chars().collect();
        if self.pos + letters.len() <= self.chars.len()
            && self.chars[self.pos..self.pos + letters.len()] == letters[..]
        {
            self.pos += letters.len();
            Ok(())
        } else {
            Err(IpaError::Corrupt {
                what: format!("expected `{word}` in JSON at offset {}", self.pos),
            })
        }
    }

    fn value(&mut self) -> Result<Json> {
        match self.peek() {
            Some('{') => self.object(),
            Some('[') => self.array(),
            Some('"') => Ok(Json::Str(self.string()?)),
            Some('t') => {
                self.literal("true")?;
                Ok(Json::Bool(true))
            }
            Some('f') => {
                self.literal("false")?;
                Ok(Json::Bool(false))
            }
            Some('n') => {
                self.literal("null")?;
                Ok(Json::Null)
            }
            Some(_) => self.number(),
            None => Err(IpaError::Corrupt { what: "the JSON document ends early".to_string() }),
        }
    }

    fn object(&mut self) -> Result<Json> {
        self.expect('{')?;
        let mut fields = Vec::new();
        self.skip_ws();
        if self.peek() == Some('}') {
            self.pos += 1;
            return Ok(Json::Object(fields));
        }
        loop {
            self.skip_ws();
            let key = self.string()?;
            self.skip_ws();
            self.expect(':')?;
            self.skip_ws();
            let value = self.value()?;
            fields.push((key, value));
            self.skip_ws();
            match self.peek() {
                Some(',') => self.pos += 1,
                Some('}') => {
                    self.pos += 1;
                    return Ok(Json::Object(fields));
                }
                _ => {
                    return Err(IpaError::Corrupt {
                        what: format!("expected `,` or `}}` in a JSON object at offset {}", self.pos),
                    })
                }
            }
        }
    }

    fn array(&mut self) -> Result<Json> {
        self.expect('[')?;
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek() == Some(']') {
            self.pos += 1;
            return Ok(Json::Array(items));
        }
        loop {
            self.skip_ws();
            items.push(self.value()?);
            self.skip_ws();
            match self.peek() {
                Some(',') => self.pos += 1,
                Some(']') => {
                    self.pos += 1;
                    return Ok(Json::Array(items));
                }
                _ => {
                    return Err(IpaError::Corrupt {
                        what: format!("expected `,` or `]` in a JSON array at offset {}", self.pos),
                    })
                }
            }
        }
    }

    fn string(&mut self) -> Result<String> {
        self.expect('"')?;
        let mut out = String::new();
        loop {
            let character = self.peek().ok_or_else(|| IpaError::Corrupt {
                what: "a JSON string is unterminated".to_string(),
            })?;
            self.pos += 1;
            match character {
                '"' => return Ok(out),
                '\\' => {
                    let escape = self.peek().ok_or_else(|| IpaError::Corrupt {
                        what: "a JSON string ends after a backslash".to_string(),
                    })?;
                    self.pos += 1;
                    match escape {
                        '"' => out.push('"'),
                        '\\' => out.push('\\'),
                        '/' => out.push('/'),
                        'n' => out.push('\n'),
                        'r' => out.push('\r'),
                        't' => out.push('\t'),
                        'b' => out.push('\u{8}'),
                        'f' => out.push('\u{c}'),
                        'u' => {
                            let mut code = 0u32;
                            for _ in 0..4 {
                                let digit = self.peek().ok_or_else(|| IpaError::Corrupt {
                                    what: "a JSON \\u escape is truncated".to_string(),
                                })?;
                                self.pos += 1;
                                code = code * 16
                                    + digit.to_digit(16).ok_or_else(|| IpaError::Corrupt {
                                        what: format!("`{digit}` is not a hex digit in a JSON escape"),
                                    })?;
                            }
                            out.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                        }
                        other => {
                            return Err(IpaError::Corrupt {
                                what: format!("unknown JSON escape `\\{other}`"),
                            })
                        }
                    }
                }
                _ => out.push(character),
            }
        }
    }

    fn number(&mut self) -> Result<Json> {
        let start = self.pos;
        if self.peek() == Some('-') {
            self.pos += 1;
        }
        while self.peek().is_some_and(|c| c.is_ascii_digit() || c == '.' || c == 'e' || c == 'E' || c == '+' || c == '-')
        {
            self.pos += 1;
        }
        let text: String = self.chars[start..self.pos].iter().collect();
        text.parse::<f64>().map(Json::Number).map_err(|_| IpaError::Corrupt {
            what: format!("`{text}` is not a JSON number"),
        })
    }
}
