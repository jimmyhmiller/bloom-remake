//! Lossless hand-written lexer (LANGUAGE §2, ARCHITECTURE §13.2).
use crate::SyntaxKind;
use blossom_base::{Diagnostic, FileId, Span, code};

/// One token, including trivia; offsets are UTF-8 byte offsets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    /// Token classification.
    pub kind: SyntaxKind,
    /// Byte range in the source file.
    pub span: Span,
}
impl Token {
    /// The source spelling of this token.
    pub fn text<'a>(&self, text: &'a str) -> Option<&'a str> {
        text.get(self.span.lo as usize..self.span.hi as usize)
    }
}
/// Tokens and lexical diagnostics, in source order.
#[derive(Debug, Clone)]
pub struct Lexed {
    /// All source tokens, followed by EOF.
    pub tokens: Vec<Token>,
    /// Located errors (lexing continues after each error).
    pub errors: Vec<Diagnostic>,
}
/// Lex a UTF-8 source, preserving every byte including malformed tokens and trivia.
// FEATURE: LANG-002
// FEATURE: LANG-208
pub fn lex(file: FileId, text: &str) -> Lexed {
    let mut l = Lexer {
        file,
        text,
        pos: 0,
        tokens: Vec::new(),
        errors: Vec::new(),
        previous: SyntaxKind::EOF,
    };
    while l.pos < text.len() {
        l.token();
    }
    l.push(SyntaxKind::EOF, l.pos);
    Lexed {
        tokens: l.tokens,
        errors: l.errors,
    }
}
struct Lexer<'a> {
    file: FileId,
    text: &'a str,
    pos: usize,
    tokens: Vec<Token>,
    errors: Vec<Diagnostic>,
    previous: SyntaxKind,
}
fn offset(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}
fn word_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}
fn word(c: char) -> bool {
    word_start(c) || c.is_ascii_digit()
}
impl Lexer<'_> {
    fn rest(&self) -> &str {
        self.text.get(self.pos..).unwrap_or("")
    }
    fn ch(&self) -> Option<char> {
        self.rest().chars().next()
    }
    fn bump(&mut self) {
        if let Some(c) = self.ch() {
            self.pos += c.len_utf8();
        }
    }
    fn starts(&self, s: &str) -> bool {
        self.rest().starts_with(s)
    }
    fn consume(&mut self, pred: impl Fn(char) -> bool) {
        while self.ch().is_some_and(&pred) {
            self.bump();
        }
    }
    fn span(&self, start: usize) -> Span {
        Span::new(self.file, offset(start), offset(self.pos))
    }
    fn error(&mut self, start: usize, code: blossom_base::Code, message: &str) {
        self.errors
            .push(Diagnostic::new(code, message).with_primary(self.span(start)));
    }
    fn push(&mut self, kind: SyntaxKind, start: usize) {
        self.tokens.push(Token {
            kind,
            span: self.span(start),
        });
        if !kind.is_trivia() {
            self.previous = kind;
        }
    }
    fn token(&mut self) {
        use SyntaxKind::*;
        let start = self.pos;
        let Some(c) = self.ch() else {
            return;
        };
        let kind = if matches!(c, ' ' | '\t' | '\r' | '\n' | '\x0c') {
            self.consume(|c| matches!(c, ' ' | '\t' | '\r' | '\n' | '\x0c'));
            WHITESPACE
        } else if self.starts("//") {
            let kind = if self.starts("///") {
                DOC_COMMENT
            } else if self.starts("//!") {
                INNER_DOC_COMMENT
            } else {
                LINE_COMMENT
            };
            self.consume(|c| c != '\n' && c != '\r');
            kind
        } else if self.starts("/*") {
            self.pos += 2;
            let mut depth = 1usize;
            while self.pos < self.text.len() && depth > 0 {
                if self.starts("/*") {
                    depth += 1;
                    self.pos += 2;
                } else if self.starts("*/") {
                    depth -= 1;
                    self.pos += 2;
                } else {
                    self.bump();
                }
            }
            if depth != 0 {
                self.error(start, code!("BLS0002"), "unterminated block comment");
            }
            BLOCK_COMMENT
        } else if c == '#' && !self.starts("#[") && !self.starts("#![") {
            self.bump();
            if self.ch().is_some_and(|c| c.is_ascii_digit()) {
                self.consume(|c| c.is_ascii_digit());
                FIELD_NUM
            } else {
                self.consume(|c| c != '\n' && c != '\r');
                HASH_COMMENT
            }
        } else if let Some((prefix, hashes, bytes)) = self.raw_prefix() {
            self.pos += prefix;
            self.raw_string(start, hashes);
            if bytes { BYTES_LIT } else { RAW_STRING_LIT }
        } else if c == '"' || self.starts("b\"") {
            let bytes = c == 'b';
            if bytes {
                self.bump();
            }
            self.string(start);
            if bytes { BYTES_LIT } else { STRING_LIT }
        } else if self.starts("r#")
            && self
                .rest()
                .get(2..)
                .and_then(|s| s.chars().next())
                .is_some_and(word_start)
        {
            self.pos += 2;
            self.consume(word);
            IDENT
        } else if word_start(c) {
            self.consume(word);
            let spelling = self.text.get(start..self.pos).unwrap_or("");
            let keyword = SyntaxKind::keyword(spelling);
            if self.starts("!(") || self.starts("!{") {
                self.bump();
                if keyword.is_some() {
                    self.error(start, code!("BLS0004"), "a hard keyword cannot be a bang operator");
                }
                BANG_IDENT
            } else if spelling == "_" {
                UNDERSCORE
            } else {
                keyword.unwrap_or(IDENT)
            }
        } else if c.is_ascii_digit() {
            self.number(start)
        } else if let Some((s, kind)) = SyntaxKind::PUNCT.iter().find(|(s, _)| self.starts(s)).copied() {
            self.pos += s.len();
            if kind == BANG {
                self.error(
                    start,
                    code!("BLS0100"),
                    "unexpected `!`; expected an operator bang or `not`",
                );
            }
            kind
        } else {
            self.bump();
            self.error(start, code!("BLS0001"), "unexpected character");
            ERROR
        };
        self.push(kind, start);
    }
    fn raw_prefix(&self) -> Option<(usize, usize, bool)> {
        let (prefix, bytes) = if self.starts("br") {
            (2, true)
        } else if self.starts("r") {
            (1, false)
        } else {
            return None;
        };
        let mut chars = self.rest().get(prefix..)?.chars();
        let mut hashes = 0;
        loop {
            match chars.next()? {
                '#' => hashes += 1,
                '"' => return Some((prefix + hashes + 1, hashes, bytes)),
                _ => return None,
            }
        }
    }
    fn raw_string(&mut self, start: usize, hashes: usize) {
        let closing = format!("\"{}", "#".repeat(hashes));
        while self.pos < self.text.len() {
            if self.starts(&closing) {
                self.pos += closing.len();
                return;
            }
            self.bump();
        }
        self.error(start, code!("BLS0002"), "unterminated raw string");
    }
    fn string(&mut self, start: usize) {
        self.bump();
        while let Some(c) = self.ch() {
            self.bump();
            if c == '"' {
                return;
            }
            if c != '\\' {
                continue;
            }
            let escape = self.pos.saturating_sub(1);
            match self.ch() {
                Some('n' | 'r' | 't' | '\\' | '"' | '\'' | '0') => self.bump(),
                Some('u') => {
                    self.bump();
                    let mut valid = false;
                    if self.starts("{") {
                        self.bump();
                        let digits = self.pos;
                        self.consume(|c| c.is_ascii_hexdigit());
                        let s = self.text.get(digits..self.pos).unwrap_or("");
                        valid = !s.is_empty()
                            && s.len() <= 6
                            && u32::from_str_radix(s, 16).ok().and_then(char::from_u32).is_some();
                        if self.starts("}") {
                            self.bump();
                        } else {
                            valid = false;
                        }
                    }
                    if !valid {
                        self.error(escape, code!("BLS0005"), "invalid Unicode escape");
                    }
                }
                Some(_) => {
                    self.bump();
                    self.error(escape, code!("BLS0005"), "unknown string escape");
                }
                None => break,
            }
        }
        self.error(start, code!("BLS0002"), "unterminated string");
    }
    fn number(&mut self, start: usize) -> SyntaxKind {
        use SyntaxKind::*;
        let hex = self.starts("0x");
        let bin = self.starts("0b");
        let mut float = false;
        if hex || bin {
            self.pos += 2;
            let digits = self.pos;
            self.consume(|c| {
                c == '_'
                    || if hex {
                        c.is_ascii_hexdigit()
                    } else {
                        matches!(c, '0' | '1')
                    }
            });
            if self.pos == digits {
                self.error(start, code!("BLS0001"), "expected digits after radix prefix");
            }
        } else {
            self.consume(|c| c.is_ascii_digit() || c == '_');
            if self.previous != DOT
                && self.starts(".")
                && self
                    .rest()
                    .get(1..)
                    .and_then(|s| s.chars().next())
                    .is_some_and(|c| c.is_ascii_digit())
            {
                float = true;
                self.bump();
                self.consume(|c| c.is_ascii_digit() || c == '_');
            }
            if self.previous != DOT && matches!(self.ch(), Some('e' | 'E')) {
                float = true;
                self.bump();
                if matches!(self.ch(), Some('+' | '-')) {
                    self.bump();
                }
                let exp = self.pos;
                self.consume(|c| c.is_ascii_digit() || c == '_');
                if exp == self.pos {
                    self.error(start, code!("BLS0003"), "missing exponent digits");
                }
            }
        }
        let suffix_start = self.pos;
        self.consume(word);
        let suffix = self.text.get(suffix_start..self.pos).unwrap_or("");
        match suffix {
            "" => {
                if float {
                    FLOAT_LIT
                } else {
                    INT_LIT
                }
            }
            "ns" | "us" | "ms" | "s" | "m" | "h" | "d" if !hex && !bin => DURATION_LIT,
            "I" if hex => MOD_LIT,
            "f64" if !hex && !bin => FLOAT_LIT,
            "u8" | "u16" | "u32" | "u64" | "u128" | "i8" | "i16" | "i32" | "i64" | "i128" if !float => INT_LIT,
            _ => {
                self.error(start, code!("BLS0003"), "unknown numeric suffix");
                if float { FLOAT_LIT } else { INT_LIT }
            }
        }
    }
}
