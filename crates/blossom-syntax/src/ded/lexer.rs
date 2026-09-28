//! The `.ded` lexer: Molly's tokens, with `//`, `#` and `/* … */` comments skipped.

use blossom_base::{Diagnostic, Diagnostics, FileId, Span, code};

/// A token's kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenKind {
    /// An identifier: a relation, a variable (capitalized), `_`, or a keyword (`include`, `notin`, `next`,
    /// `async`, an aggregate function), which the parser recognizes by position.
    Ident,
    /// A string literal, quotes included; Molly strings have no escapes and no line breaks.
    Str,
    /// A decimal integer literal.
    Int,
    /// `:-`
    Turnstile,
    Lt,
    Gt,
    Le,
    Ge,
    EqEq,
    Ne,
    Plus,
    Minus,
    Star,
    Slash,
    LParen,
    RParen,
    Comma,
    Semi,
    At,
    /// The end of the file.
    Eof,
}

impl TokenKind {
    /// How the token is described in a diagnostic.
    pub const fn describe(self) -> &'static str {
        match self {
            TokenKind::Ident => "an identifier",
            TokenKind::Str => "a string",
            TokenKind::Int => "an integer",
            TokenKind::Turnstile => "`:-`",
            TokenKind::Lt => "`<`",
            TokenKind::Gt => "`>`",
            TokenKind::Le => "`<=`",
            TokenKind::Ge => "`>=`",
            TokenKind::EqEq => "`==`",
            TokenKind::Ne => "`!=`",
            TokenKind::Plus => "`+`",
            TokenKind::Minus => "`-`",
            TokenKind::Star => "`*`",
            TokenKind::Slash => "`/`",
            TokenKind::LParen => "`(`",
            TokenKind::RParen => "`)`",
            TokenKind::Comma => "`,`",
            TokenKind::Semi => "`;`",
            TokenKind::At => "`@`",
            TokenKind::Eof => "the end of the file",
        }
    }
}

/// A token and its span.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

/// Tokenizes `text`. The result always ends with one [`TokenKind::Eof`]; malformed input is reported in the
/// diagnostics and skipped.
pub fn lex(file: FileId, text: &str) -> (Vec<Token>, Diagnostics) {
    Lexer {
        file,
        text,
        bytes: text.as_bytes(),
        pos: 0,
        tokens: Vec::new(),
        diags: Diagnostics::new(),
    }
    .run()
}

struct Lexer<'a> {
    file: FileId,
    text: &'a str,
    bytes: &'a [u8],
    pos: usize,
    tokens: Vec<Token>,
    diags: Diagnostics,
}

impl Lexer<'_> {
    fn run(mut self) -> (Vec<Token>, Diagnostics) {
        while let Some(&b) = self.bytes.get(self.pos) {
            let start = self.pos;
            match b {
                b' ' | b'\t' | b'\r' | b'\n' => self.pos += 1,
                b'#' => self.skip_line(),
                b'/' if self.peek(1) == Some(b'/') => self.skip_line(),
                b'/' if self.peek(1) == Some(b'*') => self.block_comment(start),
                b'"' => self.string(start),
                b'0'..=b'9' => {
                    self.eat_while(|c| c.is_ascii_digit());
                    self.push(TokenKind::Int, start);
                }
                b'a'..=b'z' | b'A'..=b'Z' | b'_' => {
                    self.eat_while(|c| c.is_ascii_alphanumeric() || c == b'_');
                    self.push(TokenKind::Ident, start);
                }
                _ => self.operator(start, b),
            }
        }
        self.push(TokenKind::Eof, self.pos);
        (self.tokens, self.diags)
    }

    fn peek(&self, ahead: usize) -> Option<u8> {
        self.bytes.get(self.pos + ahead).copied()
    }

    fn span(&self, start: usize) -> Span {
        Span::new(self.file, offset(start), offset(self.pos))
    }

    fn push(&mut self, kind: TokenKind, start: usize) {
        let span = self.span(start);
        self.tokens.push(Token { kind, span });
    }

    fn eat_while(&mut self, keep: impl Fn(u8) -> bool) {
        while self.bytes.get(self.pos).is_some_and(|&c| keep(c)) {
            self.pos += 1;
        }
    }

    fn skip_line(&mut self) {
        self.eat_while(|c| c != b'\n');
    }

    fn block_comment(&mut self, start: usize) {
        self.pos += 2;
        loop {
            match self.bytes.get(self.pos) {
                None => {
                    self.diags.push(
                        Diagnostic::new(code!("BLS0002"), "unterminated block comment").with_primary(self.span(start)),
                    );
                    return;
                }
                Some(b'*') if self.peek(1) == Some(b'/') => {
                    self.pos += 2;
                    return;
                }
                Some(_) => self.pos += 1,
            }
        }
    }

    fn string(&mut self, start: usize) {
        self.pos += 1;
        self.eat_while(|c| c != b'"' && c != b'\n');
        if self.bytes.get(self.pos) == Some(&b'"') {
            self.pos += 1;
            self.push(TokenKind::Str, start);
        } else {
            self.diags.push(
                Diagnostic::new(
                    code!("BLS0002"),
                    "unterminated string: Molly strings end on the same line",
                )
                .with_primary(self.span(start)),
            );
        }
    }

    fn operator(&mut self, start: usize, b: u8) {
        let two = |second: u8| self.peek(1) == Some(second);
        let (kind, len) = match b {
            b':' if two(b'-') => (TokenKind::Turnstile, 2),
            b'<' if two(b'=') => (TokenKind::Le, 2),
            b'>' if two(b'=') => (TokenKind::Ge, 2),
            b'=' if two(b'=') => (TokenKind::EqEq, 2),
            b'!' if two(b'=') => (TokenKind::Ne, 2),
            b'<' => (TokenKind::Lt, 1),
            b'>' => (TokenKind::Gt, 1),
            b'+' => (TokenKind::Plus, 1),
            b'-' => (TokenKind::Minus, 1),
            b'*' => (TokenKind::Star, 1),
            b'/' => (TokenKind::Slash, 1),
            b'(' => (TokenKind::LParen, 1),
            b')' => (TokenKind::RParen, 1),
            b',' => (TokenKind::Comma, 1),
            b';' => (TokenKind::Semi, 1),
            b'@' => (TokenKind::At, 1),
            _ => {
                // Skip one whole character, so a multi-byte character is reported once.
                let width = self
                    .text
                    .get(start..)
                    .and_then(|rest| rest.chars().next())
                    .map_or(1, char::len_utf8);
                self.pos += width;
                let shown = self.text.get(start..self.pos).unwrap_or("?");
                self.diags.push(
                    Diagnostic::new(code!("BLS0001"), format!("unexpected character `{shown}`"))
                        .with_primary(self.span(start)),
                );
                return;
            }
        };
        self.pos += len;
        self.push(kind, start);
    }
}

/// A byte offset as a span offset. Source files are limited to `u32::MAX` bytes by the source database.
fn offset(pos: usize) -> u32 {
    u32::try_from(pos).unwrap_or(u32::MAX)
}
