//! The `.ded` parser (LANGUAGE §21.1):
//!
//! ```text
//! clause     ::= 'include' STRING ';' | fact | rule
//! fact       ::= predicate ';'                              -- carries @<int>
//! rule       ::= predicate ':-' bodyTerm (',' bodyTerm)* ';'
//! predicate  ::= ['notin'] IDENT '(' [atom (',' atom)*] ')' ['@next' | '@async' | '@' INT]
//! atom       ::= IDENT '<' IDENT '>' | expr | constant      -- aggregates count/max/min/sum in heads
//! expr       ::= constant OP (expr | constant)              -- right-nested, no parentheses, no precedence
//! ```

use std::sync::Arc;

use blossom_base::{Diagnostic, Diagnostics, FileId, Span, Symbol, code};

use super::ast::*;
use super::lexer::{Token, TokenKind, lex};

/// The most operators one expression chains (LANGUAGE §16.1): an evaluation is at most `MAX_EVAL_DEPTH` (1024) deep.
const MAX_CHAIN: usize = 1024;

/// Parses one `.ded` file. Every malformed clause is reported and skipped up to its `;`.
pub fn parse(file: FileId, text: &str) -> (DedFile, Diagnostics) {
    let (tokens, diags) = lex(file, text);
    let mut p = Parser {
        text,
        tokens,
        pos: 0,
        diags,
        eof: Span::point(file, u32::try_from(text.len()).unwrap_or(u32::MAX)),
    };
    let mut out = DedFile::default();
    while p.kind() != TokenKind::Eof {
        let clause_start = p.pos;
        match p.clause() {
            Ok(c) => out.clauses.push(c),
            Err(d) => {
                p.diags.push(*d);
                p.recover(clause_start);
            }
        }
    }
    (out, p.diags)
}

type PResult<T> = Result<T, Box<Diagnostic>>;

/// A predicate as parsed, before it is known to be a head, a fact or a body atom.
struct Predicate {
    negated: bool,
    rel: Ident,
    args: Vec<Arg>,
    suffix: Option<Suffix>,
    span: Span,
}

#[derive(Clone, Copy)]
enum Suffix {
    Next(Span),
    Async(Span),
    Time(u64, Span),
}

struct Parser<'a> {
    text: &'a str,
    tokens: Vec<Token>,
    pos: usize,
    diags: Diagnostics,
    eof: Span,
}

impl Parser<'_> {
    fn token(&self, ahead: usize) -> Token {
        self.tokens.get(self.pos + ahead).copied().unwrap_or(Token {
            kind: TokenKind::Eof,
            span: self.eof,
        })
    }

    fn kind(&self) -> TokenKind {
        self.token(0).kind
    }

    fn text_of(&self, t: Token) -> &str {
        self.text.get(t.span.lo as usize..t.span.hi as usize).unwrap_or("")
    }

    fn is_ident(&self, ahead: usize, word: &str) -> bool {
        let t = self.token(ahead);
        t.kind == TokenKind::Ident && self.text_of(t) == word
    }

    fn bump(&mut self) -> Token {
        let t = self.token(0);
        if t.kind != TokenKind::Eof {
            self.pos += 1;
        }
        t
    }

    fn expect(&mut self, kind: TokenKind) -> PResult<Token> {
        let t = self.token(0);
        if t.kind == kind {
            Ok(self.bump())
        } else {
            Err(self.unexpected(t, kind.describe()))
        }
    }

    fn unexpected(&self, t: Token, expected: &str) -> Box<Diagnostic> {
        let found = match t.kind {
            TokenKind::Eof => "the end of the file".to_owned(),
            _ => format!("`{}`", self.text_of(t)),
        };
        Box::new(Diagnostic::new(code!("BLS0100"), format!("expected {expected}, found {found}")).with_primary(t.span))
    }

    fn error(span: Span, message: impl Into<String>) -> Box<Diagnostic> {
        Box::new(Diagnostic::new(code!("BLS0100"), message).with_primary(span))
    }

    /// Skips past the next `;` (or to the end of the file).
    /// Skips past the next `;`, or up to the next token that starts a line and can start a clause (a clause whose
    /// `;` is missing must not swallow the clause after it).
    fn recover(&mut self, clause_start: usize) {
        let start = clause_start;
        loop {
            let t = self.token(0);
            match t.kind {
                TokenKind::Eof => return,
                TokenKind::Semi => {
                    self.bump();
                    return;
                }
                TokenKind::Ident
                    if self.pos > start && self.starts_line(t) && self.token(1).kind == TokenKind::LParen =>
                {
                    return;
                }
                TokenKind::Ident if self.pos > start && self.starts_line(t) && self.text_of(t) == "include" => return,
                _ => {
                    self.bump();
                }
            }
        }
    }

    /// Whether only whitespace and comments on earlier lines separate `t` from the previous token.
    fn starts_line(&self, t: Token) -> bool {
        let prev_end = self
            .pos
            .checked_sub(1)
            .and_then(|i| self.tokens.get(i))
            .map_or(0, |p| p.span.hi as usize);
        self.text
            .get(prev_end..t.span.lo as usize)
            .is_some_and(|gap| gap.contains('\n'))
    }

    fn join(a: Span, b: Span) -> Span {
        a.to(b).unwrap_or(a)
    }

    fn clause(&mut self) -> PResult<Clause> {
        let first = self.token(0);
        if self.is_ident(0, "include") && self.token(1).kind == TokenKind::Str {
            self.bump();
            let path = self.bump();
            let semi = self.expect(TokenKind::Semi)?;
            return Ok(Clause::Include(Include {
                path: Arc::from(unquote(self.text_of(path))),
                span: Self::join(first.span, semi.span),
            }));
        }
        let head = self.predicate()?;
        if head.negated {
            return Err(Self::error(head.span, "a clause head cannot be negated"));
        }
        if self.kind() == TokenKind::Turnstile {
            self.bump();
            let mut body = vec![self.body_item()?];
            while self.kind() == TokenKind::Comma {
                self.bump();
                body.push(self.body_item()?);
            }
            let semi = self.expect(TokenKind::Semi)?;
            let time = match head.suffix {
                None => HeadTime::Now,
                Some(Suffix::Next(_)) => HeadTime::Next,
                Some(Suffix::Async(_)) => HeadTime::Async,
                Some(Suffix::Time(k, span)) => {
                    return Err(Self::error(
                        span,
                        format!("a rule head cannot carry `@{k}`: only facts hold at a fixed time"),
                    ));
                }
            };
            return Ok(Clause::Rule(Rule {
                head: Head {
                    rel: head.rel,
                    args: head.args,
                    span: head.span,
                },
                time,
                body,
                span: Self::join(first.span, semi.span),
            }));
        }
        let semi = self.expect(TokenKind::Semi)?;
        let time = match head.suffix {
            Some(Suffix::Time(k, _)) => k,
            _ => {
                return Err(Self::error(
                    head.span,
                    "a fact must carry `@<time>` (Molly facts hold at one time)",
                ));
            }
        };
        let mut args = Vec::with_capacity(head.args.len());
        for a in head.args {
            match a {
                Arg::Expr(Expr::Term(t @ (Term::Int(..) | Term::Str(..)))) => args.push(t),
                Arg::Expr(e) => return Err(Self::error(e.span(), "a fact holds only constants")),
                Arg::Agg(g) => return Err(Self::error(g.span, "a fact holds only constants")),
            }
        }
        Ok(Clause::Fact(Fact {
            rel: head.rel,
            args,
            time,
            span: Self::join(first.span, semi.span),
        }))
    }

    fn predicate(&mut self) -> PResult<Predicate> {
        let first = self.token(0);
        let negated = self.is_ident(0, "notin");
        if negated {
            self.bump();
        }
        let name = self.expect(TokenKind::Ident)?;
        let rel = Ident {
            text: Symbol::intern(self.text_of(name)),
            span: name.span,
        };
        self.expect(TokenKind::LParen)?;
        let mut args = Vec::new();
        if self.kind() != TokenKind::RParen {
            args.push(self.arg()?);
            while self.kind() == TokenKind::Comma {
                self.bump();
                args.push(self.arg()?);
            }
        }
        let close = self.expect(TokenKind::RParen)?;
        let mut span = Self::join(first.span, close.span);
        let mut suffix = None;
        if self.kind() == TokenKind::At {
            let at = self.bump();
            let t = self.token(0);
            let s = match t.kind {
                TokenKind::Int => {
                    self.bump();
                    let span = Self::join(at.span, t.span);
                    Suffix::Time(
                        self.int(t)?
                            .try_into()
                            .map_err(|_| Self::error(span, "a time is never negative"))?,
                        span,
                    )
                }
                TokenKind::Ident if self.text_of(t) == "next" => {
                    self.bump();
                    Suffix::Next(Self::join(at.span, t.span))
                }
                TokenKind::Ident if self.text_of(t) == "async" => {
                    self.bump();
                    Suffix::Async(Self::join(at.span, t.span))
                }
                _ => return Err(self.unexpected(t, "`next`, `async` or a time after `@`")),
            };
            span = Self::join(span, t.span);
            suffix = Some(s);
        }
        Ok(Predicate {
            negated,
            rel,
            args,
            suffix,
            span,
        })
    }

    fn body_item(&mut self) -> PResult<BodyItem> {
        if self.is_ident(0, "notin") || (self.kind() == TokenKind::Ident && self.token(1).kind == TokenKind::LParen) {
            let p = self.predicate()?;
            let time = match p.suffix {
                None => None,
                Some(Suffix::Time(k, _)) => Some(k),
                Some(Suffix::Next(span) | Suffix::Async(span)) => {
                    return Err(Self::error(span, "a body atom cannot carry `@next` or `@async`"));
                }
            };
            return Ok(BodyItem::Atom(BodyAtom {
                negated: p.negated,
                rel: p.rel,
                args: p.args,
                time,
                span: p.span,
            }));
        }
        let e = self.expr()?;
        match &e {
            Expr::Binary { op, .. } if op.is_comparison() => Ok(BodyItem::Qual(e)),
            _ => Err(Self::error(
                e.span(),
                "a body qualifier must be a comparison (`<`, `>`, `<=`, `>=`, `==`, `!=`)",
            )),
        }
    }

    fn arg(&mut self) -> PResult<Arg> {
        let t = self.token(0);
        let func = match self.text_of(t) {
            "count" => Some(AggFunc::Count),
            "min" => Some(AggFunc::Min),
            "max" => Some(AggFunc::Max),
            "sum" => Some(AggFunc::Sum),
            _ => None,
        };
        if let Some(func) = func
            && t.kind == TokenKind::Ident
            && self.token(1).kind == TokenKind::Lt
            && self.token(2).kind == TokenKind::Ident
            && self.token(3).kind == TokenKind::Gt
        {
            self.bump();
            self.bump();
            let v = self.bump();
            let close = self.bump();
            let text = self.text_of(v);
            if !text.starts_with(|c: char| c.is_ascii_uppercase()) {
                return Err(Self::error(
                    v.span,
                    format!("`{}<{text}>`: an aggregate ranges over a variable", func.name()),
                ));
            }
            return Ok(Arg::Agg(Aggregate {
                func,
                var: Ident {
                    text: Symbol::intern(text),
                    span: v.span,
                },
                span: Self::join(t.span, close.span),
            }));
        }
        Ok(Arg::Expr(self.expr()?))
    }

    fn expr(&mut self) -> PResult<Expr> {
        // Molly's `a op b op c` is `a op (b op c)`. The chain is read in a loop and nested from its end, and it is
        // no longer than the evaluation bound: every later phase walks it recursively (LANGUAGE §16.1).
        let mut terms = vec![self.term()?];
        let mut ops = Vec::new();
        loop {
            let op = match self.kind() {
                TokenKind::Plus => BinOp::Add,
                TokenKind::Minus => BinOp::Sub,
                TokenKind::Star => BinOp::Mul,
                TokenKind::Slash => BinOp::Div,
                TokenKind::Lt => BinOp::Lt,
                TokenKind::Gt => BinOp::Gt,
                TokenKind::Le => BinOp::Le,
                TokenKind::Ge => BinOp::Ge,
                TokenKind::EqEq => BinOp::Eq,
                TokenKind::Ne => BinOp::Ne,
                _ => break,
            };
            if ops.len() >= MAX_CHAIN {
                return Err(Self::error(
                    self.token(0).span,
                    format!("an expression chains more than {MAX_CHAIN} operators: split it"),
                ));
            }
            self.bump();
            ops.push(op);
            terms.push(self.term()?);
        }
        if !ops.is_empty()
            && let Some(Term::Wild(span)) = terms.iter().find(|t| matches!(t, Term::Wild(_)))
        {
            return Err(Self::error(*span, "`_` cannot appear in an expression"));
        }
        let Some(last) = terms.pop() else {
            return Err(Self::error(self.token(0).span, "expected a term"));
        };
        let mut e = Expr::Term(last);
        while let (Some(lhs), Some(op)) = (terms.pop(), ops.pop()) {
            let span = Self::join(lhs.span(), e.span());
            e = Expr::Binary {
                lhs,
                op,
                rhs: Box::new(e),
                span,
            };
        }
        Ok(e)
    }

    fn term(&mut self) -> PResult<Term> {
        let t = self.token(0);
        match t.kind {
            TokenKind::Str => {
                self.bump();
                Ok(Term::Str(Arc::from(unquote(self.text_of(t))), t.span))
            }
            TokenKind::Int => {
                self.bump();
                Ok(Term::Int(self.int(t)?, t.span))
            }
            TokenKind::Ident => {
                self.bump();
                let text = self.text_of(t);
                if text == "_" {
                    Ok(Term::Wild(t.span))
                } else if text.starts_with(|c: char| c.is_ascii_uppercase()) {
                    Ok(Term::Var(Ident {
                        text: Symbol::intern(text),
                        span: t.span,
                    }))
                } else {
                    Err(Self::error(
                        t.span,
                        format!("bare identifier `{text}`: quote string constants and capitalize variables"),
                    ))
                }
            }
            _ => Err(self.unexpected(t, "a constant or a variable")),
        }
    }

    fn int(&self, t: Token) -> PResult<i64> {
        self.text_of(t)
            .parse::<i64>()
            .map_err(|_| Self::error(t.span, format!("integer `{}` does not fit in 64 bits", self.text_of(t))))
    }
}

/// A string token's contents, without its quotes.
fn unquote(token: &str) -> &str {
    token
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(token)
}
