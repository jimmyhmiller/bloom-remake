use crate::SmtError;
use std::{fmt, sync::Arc};
/// SMT-LIB2 s-expression. Atoms retain their lexical spelling, including quoted symbols and strings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Sexp {
    Atom(Arc<str>),
    List(Vec<Sexp>),
}
impl Sexp {
    /// Raw SMT token; callers of structured builders should prefer `symbol` or `string`.
    pub fn atom(token: impl Into<Arc<str>>) -> Self {
        Self::Atom(token.into())
    }
    /// Quote a symbol only when it contains SMT-LIB punctuation or whitespace.
    pub fn symbol(name: &str) -> Self {
        if !name.is_empty()
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"~!@$%^&*_-+=<>.?/".contains(&b))
            && !name.starts_with(|c: char| c.is_ascii_digit())
        {
            Self::atom(name)
        } else {
            Self::atom(format!("|{}|", name.replace('\\', "\\\\").replace('|', "\\|")))
        }
    }
    /// Escape an SMT-LIB string using doubled quotes.
    pub fn string(value: &str) -> Self {
        Self::atom(format!("\"{}\"", value.replace('"', "\"\"")))
    }
    /// Parse exactly one expression; trailing comments and whitespace are accepted.
    pub fn parse(input: &str) -> Result<Self, SmtError> {
        let mut p = Parser::new(input);
        let value = p.expr(0)?;
        p.space();
        if p.pos != input.len() {
            return Err(p.error("trailing input"));
        }
        Ok(value)
    }
    /// Parse the next expression and return its consumed byte count. `None` means more input is needed.
    pub fn parse_prefix(input: &str) -> Result<Option<(Self, usize)>, SmtError> {
        let mut p = Parser::new(input);
        p.space();
        if p.pos == input.len() {
            return Ok(None);
        }
        match p.expr(0) {
            Ok(v) => Ok(Some((v, p.pos))),
            Err(SmtError::Parse { message, .. }) if message == "incomplete expression" => Ok(None),
            Err(e) => Err(e),
        }
    }
    /// Render canonical s-expression spacing while preserving atom spelling.
    pub fn render(&self) -> String {
        self.to_string()
    }
}
impl fmt::Display for Sexp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Atom(a) => f.write_str(a),
            Self::List(xs) => {
                f.write_str("(")?;
                for (i, x) in xs.iter().enumerate() {
                    if i > 0 {
                        f.write_str(" ")?;
                    }
                    write!(f, "{x}")?;
                }
                f.write_str(")")
            }
        }
    }
}
struct Parser<'a> {
    input: &'a str,
    pos: usize,
}
impl<'a> Parser<'a> {
    fn new(input: &'a str) -> Self {
        Self { input, pos: 0 }
    }
    fn error(&self, message: &str) -> SmtError {
        SmtError::Parse {
            offset: self.pos,
            message: message.into(),
        }
    }
    fn peek(&self) -> Option<u8> {
        self.input.as_bytes().get(self.pos).copied()
    }
    fn space(&mut self) {
        loop {
            while self.peek().is_some_and(|b| b.is_ascii_whitespace()) {
                self.pos += 1;
            }
            if self.peek() == Some(b';') {
                while self.peek().is_some_and(|b| b != b'\n') {
                    self.pos += 1;
                }
            } else {
                break;
            }
        }
    }
    fn expr(&mut self, depth: usize) -> Result<Sexp, SmtError> {
        if depth > 512 {
            return Err(self.error("nesting exceeds 512"));
        }
        self.space();
        match self.peek() {
            None => Err(self.error("incomplete expression")),
            Some(b'(') => {
                self.pos += 1;
                let mut xs = Vec::new();
                loop {
                    self.space();
                    match self.peek() {
                        Some(b')') => {
                            self.pos += 1;
                            return Ok(Sexp::List(xs));
                        }
                        None => return Err(self.error("incomplete expression")),
                        _ => xs.push(self.expr(depth + 1)?),
                    }
                }
            }
            Some(b')') => Err(self.error("unexpected closing parenthesis")),
            Some(b'"') => self.quoted(b'"'),
            Some(b'|') => self.quoted(b'|'),
            Some(_) => {
                let begin = self.pos;
                while self
                    .peek()
                    .is_some_and(|b| !b.is_ascii_whitespace() && !matches!(b, b'(' | b')' | b';'))
                {
                    self.pos += 1;
                }
                let token = self
                    .input
                    .get(begin..self.pos)
                    .ok_or_else(|| self.error("atom boundary"))?;
                if token.is_empty() {
                    return Err(self.error("empty atom"));
                }
                Ok(Sexp::atom(token))
            }
        }
    }
    fn quoted(&mut self, quote: u8) -> Result<Sexp, SmtError> {
        let begin = self.pos;
        self.pos += 1;
        loop {
            match self.peek() {
                None => return Err(self.error("incomplete expression")),
                Some(b'\\') if quote == b'|' => {
                    self.pos += 1;
                    if self.peek().is_none() {
                        return Err(self.error("incomplete expression"));
                    }
                    self.pos += 1;
                }
                Some(b) if b == quote => {
                    self.pos += 1;
                    if quote == b'"' && self.peek() == Some(b'"') {
                        self.pos += 1;
                    } else {
                        return Ok(Sexp::atom(
                            self.input
                                .get(begin..self.pos)
                                .ok_or_else(|| self.error("quoted boundary"))?,
                        ));
                    }
                }
                Some(_) => self.pos += 1,
            }
        }
    }
}
/// A typed SMT term.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Term(pub Sexp);
/// A typed SMT sort.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sort(pub Sexp);
impl fmt::Display for Term {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}
impl fmt::Display for Sort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}
impl Sort {
    /// Named sort.
    pub fn named(name: &str) -> Self {
        Self(Sexp::symbol(name))
    }
    /// Fixed-width bit vector sort.
    pub fn bitvec(width: u32) -> Self {
        Self(Sexp::List(vec![
            Sexp::atom("_"),
            Sexp::atom("BitVec"),
            Sexp::atom(width.to_string()),
        ]))
    }
    /// Array sort.
    pub fn array(index: Self, value: Self) -> Self {
        Self(Sexp::List(vec![Sexp::atom("Array"), index.0, value.0]))
    }
}
impl Term {
    /// Symbol reference.
    pub fn symbol(name: &str) -> Self {
        Self(Sexp::symbol(name))
    }
    /// Integer numeral, including negative values.
    pub fn int(value: i128) -> Self {
        if value < 0 {
            Self::app("-", [Self(Sexp::atom(value.unsigned_abs().to_string()))])
        } else {
            Self(Sexp::atom(value.to_string()))
        }
    }
    /// Boolean literal.
    pub fn bool(value: bool) -> Self {
        Self(Sexp::atom(if value { "true" } else { "false" }))
    }
    /// Bit-vector literal.
    pub fn bitvec(value: u128, width: u32) -> Self {
        Self(Sexp::List(vec![
            Sexp::atom("_"),
            Sexp::atom(format!("bv{value}")),
            Sexp::atom(width.to_string()),
        ]))
    }
    /// Function or operator application.
    pub fn app(name: &str, args: impl IntoIterator<Item = Self>) -> Self {
        let mut xs = vec![Sexp::symbol(name)];
        xs.extend(args.into_iter().map(|t| t.0));
        Self(Sexp::List(xs))
    }
    /// Equality.
    pub fn eq(a: Self, b: Self) -> Self {
        Self::app("=", [a, b])
    }
    /// Conjunction.
    pub fn and(args: impl IntoIterator<Item = Self>) -> Self {
        Self::app("and", args)
    }
    /// Disjunction.
    pub fn or(args: impl IntoIterator<Item = Self>) -> Self {
        Self::app("or", args)
    }
    /// Negation.
    #[allow(clippy::should_implement_trait)] // The named SMT operator builder is part of the public Term API.
    pub fn not(a: Self) -> Self {
        Self::app("not", [a])
    }
    /// Implication.
    pub fn implies(a: Self, b: Self) -> Self {
        Self::app("=>", [a, b])
    }
    /// Conditional.
    pub fn ite(c: Self, t: Self, e: Self) -> Self {
        Self::app("ite", [c, t, e])
    }
    /// Universal quantification.
    pub fn forall(vars: impl IntoIterator<Item = (String, Sort)>, body: Self) -> Self {
        Self::quant("forall", vars, body)
    }
    /// Existential quantification.
    pub fn exists(vars: impl IntoIterator<Item = (String, Sort)>, body: Self) -> Self {
        Self::quant("exists", vars, body)
    }
    fn quant(name: &str, vars: impl IntoIterator<Item = (String, Sort)>, body: Self) -> Self {
        let vars = vars
            .into_iter()
            .map(|(n, s)| Sexp::List(vec![Sexp::symbol(&n), s.0]))
            .collect();
        Self(Sexp::List(vec![Sexp::atom(name), Sexp::List(vars), body.0]))
    }
    /// Local bindings.
    pub fn let_(bindings: impl IntoIterator<Item = (String, Self)>, body: Self) -> Self {
        let bindings = bindings
            .into_iter()
            .map(|(n, t)| Sexp::List(vec![Sexp::symbol(&n), t.0]))
            .collect();
        Self(Sexp::List(vec![Sexp::atom("let"), Sexp::List(bindings), body.0]))
    }
    /// Array select.
    pub fn select(array: Self, index: Self) -> Self {
        Self::app("select", [array, index])
    }
    /// Array store.
    pub fn store(array: Self, index: Self, value: Self) -> Self {
        Self::app("store", [array, index, value])
    }
}
