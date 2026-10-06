//! Event-based recursive-descent and Pratt parser (LANGUAGE §3).
use crate::{
    SyntaxKind, SyntaxNode,
    lexer::{self, Token},
};
use SyntaxKind::*;
use blossom_base::{Diagnostic, FileId, Span, code};
use rowan::GreenNode;
use std::collections::BTreeSet;

/// A located diagnostic and the token set expected at its recovery point.
#[derive(Debug, Clone)]
pub struct ParseError {
    /// Registered, located diagnostic.
    pub diagnostic: Diagnostic,
    /// Expected tokens. Contextual spellings appear in the message.
    pub expected: BTreeSet<SyntaxKind>,
}
impl std::ops::Deref for ParseError {
    type Target = Diagnostic;
    fn deref(&self) -> &Diagnostic {
        &self.diagnostic
    }
}
/// Lossless parse result, including recovered syntax.
#[derive(Debug, Clone)]
pub struct Parse {
    /// Immutable lossless Rowan tree.
    pub green: GreenNode,
    /// Lexical and syntactic errors in canonical source order.
    pub errors: Vec<ParseError>,
}
impl Parse {
    /// A red root for typed AST access.
    pub fn syntax(&self) -> SyntaxNode {
        SyntaxNode::new_root(self.green.clone())
    }
}
/// Parse a file; malformed input remains present in ERROR nodes and MISSING tokens.
// FEATURE: LANG-002
pub fn parse(file: FileId, text: &str) -> Parse {
    let lexed = lexer::lex(file, text);
    let errors = lexed
        .errors
        .into_iter()
        .map(|diagnostic| ParseError {
            diagnostic,
            expected: BTreeSet::new(),
        })
        .collect();
    let mut p = Parser {
        file,
        text,
        tokens: lexed.tokens,
        pos: 0,
        events: Vec::new(),
        errors,
        depth: 0,
        height: 0,
        sub: 0,
        too_high: false,
        no_struct: false,
        fold_element: None,
    };
    p.source_file();
    let green = p.build();
    p.errors.sort_by_key(|a| (a.primary, a.code));
    Parse {
        green,
        errors: p.errors,
    }
}
#[derive(Debug)]
enum Event {
    Start(SyntaxKind, Option<usize>),
    Finish,
    Token(usize, SyntaxKind),
    Missing,
    Tombstone,
}
#[derive(Clone, Copy)]
struct Marker(usize);
#[derive(Clone, Copy)]
struct Completed(usize);
struct Parser<'a> {
    file: FileId,
    text: &'a str,
    tokens: Vec<Token>,
    pos: usize,
    events: Vec<Event>,
    errors: Vec<ParseError>,
    depth: usize,
    /// The height of the expression `expr` or `prefix` completed last: a leaf is 1, a node one more than its
    /// highest child expression.
    height: usize,
    /// The greatest height of the expressions completed since the enclosing expression reset it: what a node's
    /// nested expressions (arguments, operands, elements) reach.
    sub: usize,
    /// Whether an expression of the file has been reported as too high (once is enough to reject it).
    too_high: bool,
    no_struct: bool,
    fold_element: Option<usize>,
}
/// The highest expression the parser builds (LANGUAGE §16.1). Operator and method chains loop in the parser rather
/// than recurse, so the recursion guard does not bound them; every later phase walks expressions recursively, and a
/// chain higher than the evaluation bound (`MAX_EVAL_DEPTH`, 1024) could not be evaluated anyway.
const MAX_EXPR_HEIGHT: usize = 1024;
impl Parser<'_> {
    fn start(&mut self) -> Marker {
        let n = self.events.len();
        self.events.push(Event::Tombstone);
        Marker(n)
    }
    fn complete(&mut self, m: Marker, kind: SyntaxKind) -> Completed {
        if let Some(e) = self.events.get_mut(m.0) {
            *e = Event::Start(kind, None);
        }
        self.events.push(Event::Finish);
        Completed(m.0)
    }
    fn precede(&mut self, c: Completed) -> Marker {
        let m = self.start();
        if let Some(Event::Start(_, parent)) = self.events.get_mut(c.0) {
            *parent = Some(m.0 - c.0);
        }
        m
    }
    fn raw_index(&self, n: usize) -> Option<usize> {
        self.tokens
            .iter()
            .enumerate()
            .skip(self.pos)
            .filter(|(_, t)| !t.kind.is_trivia())
            .nth(n)
            .map(|(i, _)| i)
    }
    fn nth(&self, n: usize) -> SyntaxKind {
        self.raw_index(n)
            .and_then(|i| self.tokens.get(i))
            .map_or(EOF, |t| t.kind)
    }
    fn at(&self, k: SyntaxKind) -> bool {
        self.nth(0) == k
    }
    fn spelling(&self, n: usize) -> &str {
        self.raw_index(n)
            .and_then(|i| self.tokens.get(i))
            .and_then(|t| t.text(self.text))
            .unwrap_or("")
    }
    fn ctx(&self, s: &str) -> bool {
        self.at(IDENT) && self.spelling(0) == s
    }
    fn nth_ctx(&self, n: usize, s: &str) -> bool {
        self.nth(n) == IDENT && self.spelling(n) == s
    }
    fn bump(&mut self) {
        let Some(i) = self.raw_index(0) else {
            return;
        };
        while self.pos <= i {
            if let Some(t) = self.tokens.get(self.pos)
                && t.kind != EOF
            {
                self.events.push(Event::Token(self.pos, t.kind));
            }
            self.pos += 1;
        }
    }
    fn eat(&mut self, k: SyntaxKind) -> bool {
        if self.at(k) {
            self.bump();
            true
        } else {
            false
        }
    }
    fn eat_ctx(&mut self, s: &str) -> bool {
        if self.ctx(s) {
            self.bump();
            true
        } else {
            false
        }
    }
    fn span(&self) -> Span {
        self.raw_index(0).and_then(|i| self.tokens.get(i)).map_or(
            Span::point(self.file, u32::try_from(self.text.len()).unwrap_or(u32::MAX)),
            |t| t.span,
        )
    }
    fn error(&mut self, c: blossom_base::Code, msg: &str, expected: &[SyntaxKind]) {
        self.errors.push(ParseError {
            diagnostic: Diagnostic::new(c, msg).with_primary(self.span()),
            expected: expected.iter().copied().collect(),
        });
    }
    fn expect(&mut self, k: SyntaxKind) {
        if !self.eat(k) {
            let c = if k == SEMI { code!("BLS0101") } else { code!("BLS0100") };
            self.error(c, &format!("expected {k:?}, found {:?}", self.nth(0)), &[k]);
            self.events.push(Event::Missing);
        }
    }
    fn expect_ctx(&mut self, s: &str) {
        if !self.eat_ctx(s) {
            self.error(code!("BLS0100"), &format!("expected `{s}`"), &[IDENT]);
            self.events.push(Event::Missing);
        }
    }
    fn name(&mut self, field: bool) {
        let m = self.start();
        if self.at(IDENT) || field && self.nth(0).is_word() {
            self.bump();
        } else {
            self.error(code!("BLS0100"), "expected a name", &[IDENT]);
            self.events.push(Event::Missing);
        }
        self.complete(m, NAME);
    }
    fn recover(&mut self, stops: &[SyntaxKind]) {
        let m = self.start();
        while !self.at(EOF) && !stops.contains(&self.nth(0)) && !self.line_item() {
            self.bump();
        }
        self.complete(m, ERROR);
    }
    fn line_item(&self) -> bool {
        if !matches!(
            self.nth(0),
            CONST_KW
                | PARAM_KW
                | TYPE_KW
                | STRUCT_KW
                | ENUM_KW
                | FN_KW
                | EXTERN_KW
                | IMPL_KW
                | LATTICE_KW
                | MODULE_KW
                | CHOREOGRAPHY_KW
                | PROTOCOL_KW
                | TABLE_KW
                | SCRATCH_KW
                | CHANNEL_KW
                | INPUT_KW
                | OUTPUT_KW
                | STATIC_KW
                | LOOPBACK_KW
                | VIEW_KW
                | ON_KW
                | WHILE_KW
                | BOOTSTRAP_KW
                | SPEC_KW
                | USE_KW
                | IMPORT_KW
                | INCLUDE_KW
                | INVARIANT_KW
                | MIGRATE_KW
                | TRANSLATE_KW
        ) {
            return false;
        }
        let lo = self.span().lo as usize;
        self.text
            .get(..lo)
            .and_then(|s| s.rsplit('\n').next())
            .is_some_and(|s| s.trim().is_empty())
    }
    fn guard(&mut self) -> bool {
        if self.depth >= 128 {
            self.error(code!("BLS0100"), "syntax nesting exceeds 128 levels", &[SEMI, R_CURLY]);
            let m = self.start();
            if !self.at(EOF) {
                self.bump();
            }
            self.complete(m, ERROR);
            false
        } else {
            self.depth += 1;
            true
        }
    }
    fn build(&mut self) -> GreenNode {
        let mut builder = rowan::GreenNodeBuilder::new();
        for i in 0..self.events.len() {
            let event = self.events.get_mut(i).map(|e| std::mem::replace(e, Event::Tombstone));
            match event {
                Some(Event::Start(kind, parent)) => {
                    let mut kinds = vec![kind];
                    let mut next = parent;
                    let mut index = i;
                    while let Some(distance) = next {
                        index += distance;
                        match self
                            .events
                            .get_mut(index)
                            .map(|e| std::mem::replace(e, Event::Tombstone))
                        {
                            Some(Event::Start(k, p)) => {
                                kinds.push(k);
                                next = p;
                            }
                            _ => break,
                        }
                    }
                    for k in kinds.into_iter().rev() {
                        builder.start_node(rowan::SyntaxKind(k as u16));
                    }
                }
                Some(Event::Finish) => builder.finish_node(),
                Some(Event::Token(i, k)) => {
                    if let Some(text) = self.tokens.get(i).and_then(|t| t.text(self.text)) {
                        builder.token(rowan::SyntaxKind(k as u16), text);
                    }
                }
                Some(Event::Missing) => builder.token(rowan::SyntaxKind(MISSING as u16), ""),
                _ => {}
            }
        }
        builder.finish()
    }
    fn source_file(&mut self) {
        let m = self.start();
        while self.at(INNER_ATTR) {
            self.attr(true);
        }
        if self.at(PROGRAM_KW) {
            let h = self.start();
            self.bump();
            self.name(false);
            self.expect_ctx("version");
            self.expect(INT_LIT);
            if self.eat_ctx("edition") {
                self.expect(INT_LIT);
            }
            self.expect(SEMI);
            self.complete(h, PROGRAMHEADER);
        }
        while !self.at(EOF) {
            let old = self.pos;
            self.item();
            if self.pos == old {
                let e = self.start();
                self.bump();
                self.complete(e, ERROR);
            }
        }
        // Trailing trivia belongs to the file, even when there is no final item.
        while self.pos < self.tokens.len() {
            self.bump();
        }
        self.complete(m, SOURCEFILE);
    }
    fn attrs(&mut self) {
        while self.at(ATTR_START) {
            self.attr(false);
        }
    }
    fn attr(&mut self, inner: bool) {
        let m = self.start();
        self.bump();
        if self.guard() {
            loop {
                self.path(true, false);
                if self.eat(L_PAREN) {
                    self.attr_args();
                    self.expect(R_PAREN);
                } else if self.eat(EQ) {
                    self.expr(0);
                }
                if !self.eat(COMMA) || self.at(R_BRACK) {
                    break;
                }
            }
            self.depth -= 1;
        }
        self.expect(R_BRACK);
        self.complete(m, if inner { INNERATTR } else { ATTR });
    }
    fn attr_args(&mut self) {
        while !self.at(R_PAREN) && !self.at(EOF) {
            let old = self.pos;
            if self.nth(0).is_word() && self.nth(1) == EQ {
                self.name(true);
                self.bump();
                self.expr(0);
            } else if self.nth(0).is_keyword() && !matches!(self.nth(0), TRUE_KW | FALSE_KW | SELF_KW | NOT_KW) {
                self.name(true);
            } else {
                self.expr(0);
            }
            if self.pos == old {
                self.recover(&[COMMA, R_PAREN]);
            }
            if !self.eat(COMMA) {
                break;
            }
        }
    }
    fn item(&mut self) {
        if !self.guard() {
            return;
        }
        let m = self.start();
        self.attrs();
        self.eat(PUB_KW);
        let kind = self.item_kind();
        self.complete(m, kind);
        self.depth -= 1;
    }
    fn item_kind(&mut self) -> SyntaxKind {
        if self.at(IDENT) && self.nth(1) == COLON {
            self.name(false);
            self.bump();
            if !(self.at(ON_KW) || self.at(WHILE_KW) || self.ctx("monotone")) {
                self.error(
                    code!("BLS0105"),
                    "label must precede on, while or monotone",
                    &[ON_KW, WHILE_KW],
                );
                self.recover(&[SEMI, R_CURLY]);
                self.eat(SEMI);
                return ERROR;
            }
            return self.handler();
        }
        match self.nth(0) {
            USE_KW => {
                self.bump();
                self.use_tree();
                self.expect(SEMI);
                USEITEM
            }
            IMPORT_KW => {
                self.bump();
                self.path(false, false);
                if self.at(LT) {
                    self.generic_args();
                }
                if self.at(L_PAREN) {
                    self.named_args();
                }
                self.expect(AS_KW);
                self.name(false);
                if self.eat_ctx("with") {
                    self.named_args();
                }
                self.expect(SEMI);
                IMPORTITEM
            }
            INCLUDE_KW => {
                self.bump();
                if self.at(STRING_LIT) {
                    self.bump();
                } else {
                    self.path(false, false);
                }
                self.expect(SEMI);
                INCLUDEITEM
            }
            CONST_KW | PARAM_KW => {
                let c = self.nth(0);
                self.bump();
                self.name(false);
                self.expect(COLON);
                self.ty();
                if c == CONST_KW {
                    self.expect(EQ);
                    self.expr(0);
                } else if self.eat(EQ) {
                    self.expr(0);
                }
                self.expect(SEMI);
                if c == CONST_KW { CONSTITEM } else { PARAMITEM }
            }
            TYPE_KW => {
                self.bump();
                self.name(false);
                self.generics_opt();
                self.expect(EQ);
                self.ty();
                self.expect(SEMI);
                TYPEALIAS
            }
            STRUCT_KW => {
                self.bump();
                self.name(false);
                self.generics_opt();
                if self.at(L_CURLY) {
                    self.fields();
                } else {
                    self.type_tuple();
                    self.expect(SEMI);
                }
                STRUCTITEM
            }
            ENUM_KW => {
                self.bump();
                self.name(false);
                self.generics_opt();
                self.expect(L_CURLY);
                while !self.at(R_CURLY) && !self.at(EOF) {
                    let old = self.pos;
                    let v = self.start();
                    self.attrs();
                    self.name(false);
                    if self.at(L_PAREN) {
                        self.type_tuple();
                    } else if self.at(L_CURLY) {
                        self.fields();
                    }
                    self.eat(FIELD_NUM);
                    self.complete(v, VARIANT);
                    if self.pos == old {
                        self.bump();
                    }
                    if !self.eat(COMMA) {
                        break;
                    }
                }
                self.expect(R_CURLY);
                ENUMITEM
            }
            FN_KW => {
                self.fn_item(false);
                FNITEM
            }
            EXTERN_KW => {
                self.extern_item();
                EXTERNITEM
            }
            IMPL_KW => {
                self.bump();
                self.generics_opt();
                self.ty();
                if self.eat(FOR_KW) {
                    self.ty();
                }
                self.expect(L_CURLY);
                while !self.at(R_CURLY) && !self.at(EOF) {
                    let old = self.pos;
                    self.item();
                    if old == self.pos {
                        self.bump();
                    }
                }
                self.expect(R_CURLY);
                IMPLITEM
            }
            LATTICE_KW => {
                self.bump();
                self.name(false);
                self.generics_opt();
                if self.eat(EQ) {
                    self.ty();
                    self.expect(SEMI);
                } else {
                    self.fields();
                }
                LATTICETYPEITEM
            }
            MODULE_KW | CHOREOGRAPHY_KW | PROTOCOL_KW => self.module(),
            TABLE_KW | SCRATCH_KW | CHANNEL_KW | INPUT_KW | OUTPUT_KW | STATIC_KW | LOOPBACK_KW => self.relation(),
            VIEW_KW => self.view(),
            ON_KW | WHILE_KW => self.handler(),
            BOOTSTRAP_KW => {
                self.bump();
                self.eat_ctx("fresh");
                self.block();
                BOOTSTRAPITEM
            }
            INVARIANT_KW => {
                self.bump();
                self.name(false);
                self.eat(STRING_LIT);
                self.expect(COLON);
                self.expect_ctx("never");
                self.body(false);
                self.expect(SEMI);
                INVARIANTITEM
            }
            INTERPOSE_KW => {
                self.bump();
                self.relpath();
                self.expect(AS_KW);
                self.expect(L_PAREN);
                self.name(false);
                self.expect(COMMA);
                self.name(false);
                self.expect(R_PAREN);
                self.items_block(false);
                INTERPOSEITEM
            }
            OVERRIDE_KW => {
                self.bump();
                let m = self.start();
                let k = self.item_kind();
                self.complete(m, k);
                OVERRIDEITEM
            }
            MIGRATE_KW => {
                self.bump();
                self.expect_ctx("from");
                self.expect(INT_LIT);
                self.eat_ctx("down");
                self.items_block(false);
                MIGRATEITEM
            }
            TRANSLATE_KW => {
                self.bump();
                self.relpath();
                if !(self.eat_ctx("to") || self.eat_ctx("from")) {
                    self.expect_ctx("to");
                }
                self.expect(INT_LIT);
                self.items_block(false);
                TRANSLATEITEM
            }
            SPEC_KW => self.spec(),
            _ => self.contextual_item(),
        }
    }
    /// `format Name[(params)] { [name:] element [if cond] [= default], … }` or `format name[(params)] = element;`
    /// (LANGUAGE §16.7). Elements are expressions (`nullable(compact_array(Topic(version)))`).
    fn format_item(&mut self) {
        self.bump();
        self.name(false);
        if self.eat(L_PAREN) {
            while !self.at(R_PAREN) && !self.at(EOF) {
                let old = self.pos;
                let p = self.start();
                self.name(false);
                if self.eat(COLON) {
                    self.ty();
                }
                self.complete(p, FORMATPARAM);
                if old == self.pos {
                    self.bump();
                }
                if !self.eat(COMMA) {
                    break;
                }
            }
            self.expect(R_PAREN);
        }
        if self.eat(EQ) {
            self.expr(0);
            self.expect(SEMI);
            return;
        }
        self.expect(L_CURLY);
        while !self.at(R_CURLY) && !self.at(EOF) {
            let old = self.pos;
            let f = self.start();
            if self.nth(1) == COLON && (self.at(IDENT) || self.nth(0).is_word()) {
                self.name(true);
                self.bump();
            }
            self.expr(0);
            if self.at(IF_KW) {
                let c = self.start();
                self.bump();
                self.expr(0);
                self.complete(c, FORMATCOND);
            }
            if self.at(EQ) {
                let d = self.start();
                self.bump();
                self.expr(0);
                self.complete(d, FORMATDEFAULT);
            }
            self.complete(f, FORMATFIELD);
            if old == self.pos {
                self.bump();
            }
            if !self.eat(COMMA) {
                break;
            }
        }
        self.expect(R_CURLY);
    }
    fn contextual_item(&mut self) -> SyntaxKind {
        if self.ctx("monotone") {
            self.bump();
            return match self.nth(0) {
                MODULE_KW | CHOREOGRAPHY_KW => self.module(),
                VIEW_KW => self.view(),
                ON_KW | WHILE_KW => self.handler(),
                FN_KW => {
                    self.fn_item(false);
                    FNITEM
                }
                _ => {
                    self.error(
                        code!("BLS0100"),
                        "expected monotone item",
                        &[MODULE_KW, VIEW_KW, ON_KW, WHILE_KW, FN_KW],
                    );
                    ERROR
                }
            };
        }
        if self.fn_class() && self.nth(1) == FN_KW {
            let stable = self.ctx("stable");
            self.bump();
            self.fn_item(stable);
            return FNITEM;
        }
        if self.relmod() {
            return self.relation();
        }
        if self.ctx("cell") {
            self.cell();
            return CELLDECL;
        }
        if self.ctx("format") && self.nth(1) == IDENT {
            self.format_item();
            return FORMATITEM;
        }
        if self.ctx("stream") && self.nth(1) == IDENT && self.nth(2) == COLON {
            self.bump();
            self.name(false);
            self.expect(COLON);
            self.name(false);
            self.expect(SEMI);
            return STREAMITEM;
        }
        if self.ctx("role") && self.nth(1) == IDENT {
            self.bump();
            self.name(false);
            if self.eat(COLON) {
                self.name(false);
            }
            self.expect(SEMI);
            return ROLEITEM;
        }
        if self.ctx("at") && self.nth(1) == IDENT && self.nth(2) == L_CURLY {
            self.bump();
            self.name(false);
            self.items_block(false);
            return ATSECTION;
        }
        if self.ctx("fragment") && self.nth(1) == IDENT && self.nth(2) == L_PAREN {
            // `fragment NAME(param: T, …) { items }` (docs/design/SUGAR.md §4).
            self.bump();
            self.name(false);
            self.param_list();
            self.children();
            return FRAGMENTITEM;
        }
        if self.ctx("tree") && self.nth(1) == IDENT && self.nth(2) == L_CURLY {
            // `tree NAME { node rel(cols); props rel(cols); content rel(cols); }` (SUGAR.md §3).
            self.bump();
            self.name(false);
            self.expect(L_CURLY);
            while !self.at(R_CURLY) && !self.at(EOF) {
                let old = self.pos;
                let r = self.start();
                self.name(false);
                self.relpath();
                self.expect(L_PAREN);
                while !self.at(R_PAREN) && !self.at(EOF) {
                    self.name(true);
                    if !self.eat(COMMA) {
                        break;
                    }
                }
                self.expect(R_PAREN);
                self.expect(SEMI);
                self.complete(r, TREEROLE);
                if old == self.pos {
                    self.bump();
                }
            }
            self.expect(R_CURLY);
            return TREEITEM;
        }
        if self.ctx("block") && self.nth(1) == IDENT {
            self.bump();
            self.name(false);
            self.items_block(false);
            return BLOCKITEM;
        }
        if self.ctx("fact") {
            self.bump();
            self.head();
            if self.eat(AT) {
                self.expr(5);
            }
            if self.eat_ctx("from") {
                self.expr(5);
            }
            if self.eat_ctx("at") {
                self.expect_ctx("tick");
                self.expr(5);
            }
            self.expect(SEMI);
            return FACTITEM;
        }
        if self.ctx("timer") {
            self.bump();
            self.name(false);
            if self.eat_ctx("every") {
                self.expr(0);
                self.eat_ctx("ticks");
                if self.eat_ctx("times") {
                    self.expr(0);
                }
            } else {
                self.expect_ctx("once");
                if self.eat_ctx("after") {
                    self.expr(0);
                }
            }
            // `while G`: the timer fires only while the relation `G` holds (LANGUAGE §15.2).
            if self.eat(WHILE_KW) {
                self.relpath();
            }
            self.expect(SEMI);
            return TIMERDECL;
        }
        if self.ctx("acl") {
            self.bump();
            self.relpath();
            self.expect_ctx("accept");
            self.args(false);
            self.expect(SEMI);
            return ACLITEM;
        }
        if self.ctx("service") {
            self.bump();
            self.name(false);
            self.param_list();
            self.expect(ARROW);
            self.param_list();
            self.expect(SEMI);
            return SERVICEITEM;
        }
        if self.ctx("aggregate") {
            self.bump();
            self.name(false);
            self.generics_opt();
            self.param_list();
            self.expect(ARROW);
            self.ty();
            self.expect(L_CURLY);
            while !self.at(R_CURLY) && !self.at(EOF) {
                let old = self.pos;
                let a = self.start();
                let is_type = self.eat(TYPE_KW);
                self.name(false);
                self.expect(EQ);
                if is_type {
                    self.ty();
                } else {
                    self.expr(0);
                }
                self.expect(SEMI);
                self.complete(a, AGGMEMBER);
                if old == self.pos {
                    self.bump();
                }
            }
            self.expect(R_CURLY);
            return AGGREGATEITEM;
        }
        if self.ctx("snapshot") {
            self.snapshot();
            return SNAPSHOTITEM;
        }
        self.error(
            code!("BLS0100"),
            "expected an item",
            &[TABLE_KW, FN_KW, ON_KW, MODULE_KW],
        );
        self.recover(&[SEMI, R_CURLY]);
        self.eat(SEMI);
        ERROR
    }
    fn use_tree(&mut self) {
        let m = self.start();
        self.name(false);
        while self.eat(COLON2) {
            if self.eat(STAR) {
                break;
            }
            if self.eat(L_CURLY) {
                while !self.at(R_CURLY) && !self.at(EOF) {
                    let old = self.pos;
                    self.use_tree();
                    if old == self.pos {
                        self.bump();
                    }
                    if !self.eat(COMMA) {
                        break;
                    }
                }
                self.expect(R_CURLY);
                break;
            }
            self.name(false);
        }
        if self.eat(AS_KW) {
            self.name(false);
        }
        self.complete(m, USETREE);
    }
    fn path(&mut self, field: bool, turbofish: bool) {
        self.name(field);
        while self.eat(COLON2) {
            if turbofish && self.at(LT) {
                self.generic_args();
            } else {
                self.name(field);
            }
        }
    }
    fn relpath(&mut self) {
        let m = self.start();
        self.name(true);
        while self.eat(DOT) {
            self.name(true);
        }
        self.complete(m, RELPATH);
    }
    fn named_args(&mut self) {
        self.expect(L_PAREN);
        while !self.at(R_PAREN) && !self.at(EOF) {
            let old = self.pos;
            let m = self.start();
            self.name(false);
            self.expect(EQ);
            self.expr(0);
            self.complete(m, ARG);
            if old == self.pos {
                self.bump();
            }
            if !self.eat(COMMA) {
                break;
            }
        }
        self.expect(R_PAREN);
    }
    fn generics_opt(&mut self) {
        if self.at(LT) {
            let m = self.start();
            self.bump();
            while !self.type_end() {
                let old = self.pos;
                let g = self.start();
                self.name(false);
                if self.eat(COLON) {
                    self.ty();
                    while self.eat(PLUS) {
                        self.ty();
                    }
                }
                if self.eat(EQ) {
                    self.ty();
                }
                self.complete(g, GENERICPARAM);
                if old == self.pos {
                    self.bump();
                }
                if !self.eat(COMMA) {
                    break;
                }
            }
            self.type_gt();
            self.complete(m, GENERICS);
        }
    }
    fn generic_args(&mut self) {
        let m = self.start();
        self.expect(LT);
        while !self.type_end() {
            let old = self.pos;
            let a = self.start();
            if self.at(IDENT) && self.nth(1) == EQ {
                self.name(false);
                self.bump();
            }
            if self.at(INT_LIT) {
                self.bump();
            } else {
                self.ty();
            }
            self.complete(a, GENERICARG);
            if old == self.pos {
                self.bump();
            }
            if !self.eat(COMMA) {
                break;
            }
        }
        self.type_gt();
        self.complete(m, GENERICARGS);
    }
    fn type_end(&self) -> bool {
        matches!(self.nth(0), GT | SHR | EOF | R_PAREN | R_CURLY | SEMI)
    }
    fn type_gt(&mut self) {
        if self.at(SHR)
            && let Some(i) = self.raw_index(0)
            && let Some(t) = self.tokens.get(i).cloned()
        {
            let middle = t.span.lo + 1;
            let first = Token {
                kind: GT,
                span: Span::new(t.span.file, t.span.lo, middle),
            };
            let second = Token {
                kind: GT,
                span: Span::new(t.span.file, middle, t.span.hi),
            };
            if let Some(t) = self.tokens.get_mut(i) {
                *t = first;
            }
            self.tokens.insert(i + 1, second);
        }
        self.expect(GT);
    }
    fn ty(&mut self) {
        let m = self.start();
        if self.guard() {
            if self.eat_ctx("unsafe") {
                self.ty();
            } else if self.eat(FN_KW) {
                // `fn(A, B) -> R`: a function parameter's type (LANGUAGE §16.1).
                self.type_tuple();
                self.expect(ARROW);
                self.ty();
            } else if self.at(L_PAREN) {
                self.type_tuple();
            } else {
                self.path(false, false);
                if self.at(LT) {
                    self.generic_args();
                }
            }
            self.depth -= 1;
        }
        self.complete(m, TYPE);
    }
    fn type_tuple(&mut self) {
        self.expect(L_PAREN);
        while !self.at(R_PAREN) && !self.at(EOF) {
            let old = self.pos;
            self.ty();
            if old == self.pos {
                self.bump();
            }
            if !self.eat(COMMA) {
                break;
            }
        }
        self.expect(R_PAREN);
    }
    fn fields(&mut self) {
        self.expect(L_CURLY);
        while !self.at(R_CURLY) && !self.at(EOF) {
            let old = self.pos;
            let m = self.start();
            self.attrs();
            self.name(true);
            self.expect(COLON);
            self.ty();
            self.eat(FIELD_NUM);
            if self.eat(EQ) {
                self.expr(0);
            }
            self.complete(m, FIELDDECL);
            if old == self.pos {
                self.bump();
            }
            if !self.eat(COMMA) {
                break;
            }
        }
        self.expect(R_CURLY);
    }
    fn param_list(&mut self) {
        let m = self.start();
        self.expect(L_PAREN);
        while !self.at(R_PAREN) && !self.at(EOF) {
            let old = self.pos;
            let p = self.start();
            self.name(true);
            self.expect(COLON);
            self.ty();
            self.complete(p, PARAM);
            if old == self.pos {
                self.bump();
            }
            if !self.eat(COMMA) {
                break;
            }
        }
        self.expect(R_PAREN);
        self.complete(m, PARAMLIST);
    }
    fn fn_class(&self) -> bool {
        self.at(IDENT)
            && matches!(
                self.spelling(0),
                "morphism" | "bimorphism" | "monotone" | "antitone" | "threshold" | "stable"
            )
    }
    fn fn_sig(&mut self, stable: bool) {
        let m = self.start();
        self.expect(FN_KW);
        self.name(false);
        self.generics_opt();
        self.expect(L_PAREN);
        while !self.at(R_PAREN) && !self.at(EOF) {
            let old = self.pos;
            let p = self.start();
            if !self.eat(SELF_KW) {
                let prev = self.no_struct;
                self.no_struct = true;
                self.expr(0);
                self.no_struct = prev;
                self.expect(COLON);
                self.ty();
            }
            self.complete(p, FNPARAM);
            if old == self.pos {
                self.bump();
            }
            if !self.eat(COMMA) {
                break;
            }
        }
        self.expect(R_PAREN);
        self.expect(ARROW);
        self.ty();
        let after = self.eat_ctx("after");
        if after {
            self.name(false);
        }
        if after != stable {
            self.error(
                code!("BLS0108"),
                "`after` is required exactly on stable functions",
                &[IDENT],
            );
        }
        self.complete(m, FNSIG);
    }
    fn fn_item(&mut self, stable: bool) {
        self.fn_sig(stable);
        self.block_expr();
    }
    fn extern_item(&mut self) {
        self.bump();
        if self.eat(TABLE_KW) {
            self.expect(FN_KW);
            self.name(false);
            self.param_list();
            self.expect(ARROW);
            self.param_list();
            self.expect(EQ);
            self.expect(STRING_LIT);
            self.expect(SEMI);
        } else if self.at(TYPE_KW) || self.at(LATTICE_KW) {
            let lattice = self.at(LATTICE_KW);
            self.bump();
            self.name(false);
            self.generics_opt();
            self.expect(EQ);
            self.expect(STRING_LIT);
            if lattice && self.eat(L_CURLY) {
                while !self.at(R_CURLY) && !self.at(EOF) {
                    let old = self.pos;
                    let m = self.start();
                    self.attrs();
                    let stable = self.ctx("stable");
                    if self.fn_class() {
                        self.bump();
                    }
                    self.fn_sig(stable);
                    self.expect(SEMI);
                    self.complete(m, FNITEM);
                    if old == self.pos {
                        self.bump();
                    }
                }
                self.expect(R_CURLY);
            } else {
                self.expect(SEMI);
            }
        } else {
            let stable = self.ctx("stable");
            if self.fn_class() {
                self.bump();
            }
            self.fn_sig(stable);
            self.expect(EQ);
            self.expect(STRING_LIT);
            self.expect(SEMI);
        }
    }
    fn module(&mut self) -> SyntaxKind {
        let protocol = self.at(PROTOCOL_KW);
        self.bump();
        self.name(false);
        self.generics_opt();
        if self.at(L_PAREN) {
            let m = self.start();
            self.bump();
            while !self.at(R_PAREN) && !self.at(EOF) {
                let old = self.pos;
                let p = self.start();
                self.name(false);
                self.expect(COLON);
                if self.eat_ctx("rel") {
                    self.param_list();
                } else {
                    self.ty();
                    if self.eat(EQ) {
                        self.expr(0);
                    }
                }
                self.complete(p, MODPARAM);
                if old == self.pos {
                    self.bump();
                }
                if !self.eat(COMMA) {
                    break;
                }
            }
            self.expect(R_PAREN);
            self.complete(m, MODPARAMS);
        }
        if self.eat(COLON) {
            self.ty();
            while self.eat(PLUS) {
                self.ty();
            }
        }
        self.items_block(false);
        if protocol { PROTOCOLITEM } else { MODULEITEM }
    }
    fn items_block(&mut self, spec: bool) {
        self.expect(L_CURLY);
        while !self.at(R_CURLY) && !self.at(EOF) {
            let old = self.pos;
            if spec {
                self.spec_member();
            } else {
                self.item();
            }
            if old == self.pos {
                self.bump();
            }
        }
        self.expect(R_CURLY);
    }
    fn relmod(&self) -> bool {
        self.at(IDENT)
            && matches!(
                self.spelling(0),
                "durable" | "soft" | "sealed" | "zset" | "bag" | "final"
            )
    }
    fn cell(&mut self) {
        self.expect_ctx("cell");
        self.name(false);
        self.expect(COLON);
        self.ty();
        self.expect(SEMI);
    }
    fn relation(&mut self) -> SyntaxKind {
        while self.relmod() {
            self.bump();
        }
        if self.ctx("cell") {
            self.cell();
            return CELLDECL;
        }
        if self.at(SCRATCH_KW) && self.nth_ctx(1, "cell") {
            self.bump();
            self.cell();
            return CELLDECL;
        }
        let relkind = self.nth(0);
        if matches!(
            relkind,
            TABLE_KW | SCRATCH_KW | CHANNEL_KW | INPUT_KW | OUTPUT_KW | STATIC_KW | LOOPBACK_KW
        ) {
            self.bump();
        } else {
            self.expect(TABLE_KW);
        }
        self.name(false);
        if self.eat_ctx("like") {
            self.relpath();
        } else {
            self.expect(L_PAREN);
            while !self.at(R_PAREN) && !self.at(EOF) {
                let old = self.pos;
                let c = self.start();
                self.attrs();
                self.eat(AT);
                self.name(true);
                self.expect(COLON);
                self.ty();
                self.eat(FIELD_NUM);
                if self.eat(EQ) {
                    self.expr(0);
                }
                self.complete(c, COLDECL);
                if old == self.pos {
                    self.bump();
                }
                if !self.eat(COMMA) {
                    break;
                }
            }
            self.expect(R_PAREN);
        }
        let mut clauses = BTreeSet::new();
        while self.at(COLON)
            || self.at(WHILE_KW)
            || self.at(IDENT)
                && matches!(
                    self.spelling(0),
                    "key" | "ttl" | "max" | "range" | "resolve" | "partition" | "sealed" | "exactly_once"
                )
        {
            let m = self.start();
            let spelling = self.spelling(0).to_owned();
            let kind = match spelling.as_str() {
                ":" => {
                    self.bump();
                    self.name(false);
                    self.expect(ARROW);
                    self.name(false);
                    DIRECTIONCLAUSE
                }
                "key" => {
                    self.bump();
                    self.field_names();
                    KEYCLAUSE
                }
                "ttl" => {
                    self.bump();
                    self.expr(0);
                    TTLCLAUSE
                }
                "max" => {
                    self.bump();
                    self.expr(0);
                    MAXCLAUSE
                }
                "range" => {
                    self.bump();
                    self.field_names();
                    RANGECLAUSE
                }
                "resolve" => {
                    self.bump();
                    self.policy();
                    RESOLVECLAUSE
                }
                "partition" => {
                    self.bump();
                    self.expect_ctx("by");
                    self.expr(0);
                    if self.eat_ctx("over") {
                        self.relpath();
                    }
                    PARTITIONCLAUSE
                }
                // `while BODY`: the condition a row persists under (LANGUAGE §7.2); last, as its body runs to `;`.
                "while" => {
                    self.bump();
                    self.body(false);
                    WHILECLAUSE
                }
                "sealed" => {
                    self.bump();
                    self.expect_ctx("by");
                    self.field_names();
                    if self.eat_ctx("producers") {
                        self.relpath();
                    }
                    SEALEDBYCLAUSE
                }
                _ => {
                    self.bump();
                    self.expect(L_PAREN);
                    self.name(false);
                    self.expect(R_PAREN);
                    EXACTLYONCECLAUSE
                }
            };
            let valid = match kind {
                DIRECTIONCLAUSE | PARTITIONCLAUSE | EXACTLYONCECLAUSE => relkind == CHANNEL_KW,
                TTLCLAUSE | MAXCLAUSE | RANGECLAUSE | WHILECLAUSE => relkind == TABLE_KW,
                RESOLVECLAUSE => relkind == TABLE_KW,
                KEYCLAUSE => true,
                _ => true,
            };
            if !clauses.insert(kind) || !valid {
                self.error(code!("BLS0106"), "duplicate or inapplicable relation clause", &[SEMI]);
            }
            self.complete(m, kind);
            if kind == WHILECLAUSE {
                break;
            }
        }
        self.expect(SEMI);
        RELDECL
    }
    fn field_names(&mut self) {
        self.expect(L_PAREN);
        while !self.at(R_PAREN) && !self.at(EOF) {
            let old = self.pos;
            self.name(true);
            if old == self.pos {
                self.bump();
            }
            if !self.eat(COMMA) {
                break;
            }
        }
        self.expect(R_PAREN);
    }
    fn policy(&mut self) {
        let m = self.start();
        if self.eat_ctx("choose") || self.eat_ctx("choose_rand") {
            self.eat_ctx("sticky");
        } else if self.ctx("choose_least") || self.ctx("choose_most") {
            self.bump();
            self.expect(L_PAREN);
            self.expr(0);
            self.expect(R_PAREN);
        } else if self.eat_ctx("prefer") {
            // `prefer(rule, …)`: writer precedence by handler label (LANGUAGE §10.7).
            self.expect(L_PAREN);
            while !self.at(R_PAREN) && !self.at(EOF) {
                let old = self.pos;
                self.name(false);
                if old == self.pos {
                    self.bump();
                }
                if !self.eat(COMMA) {
                    break;
                }
            }
            self.expect(R_PAREN);
        } else {
            self.expect_ctx("merge");
        }
        self.complete(m, POLICY);
    }
    fn view(&mut self) -> SyntaxKind {
        self.expect(VIEW_KW);
        self.name(false);
        self.expect(L_PAREN);
        while !self.at(R_PAREN) && !self.at(EOF) {
            let old = self.pos;
            let c = self.start();
            self.name(false);
            if self.eat(COLON) {
                self.ty();
            }
            if self.eat(EQ) {
                self.expr(0);
            }
            self.complete(c, VIEWCOL);
            if old == self.pos {
                self.bump();
            }
            if !self.eat(COMMA) {
                break;
            }
        }
        self.expect(R_PAREN);
        if self.eat(EQ) {
            self.body(false);
            self.expect(SEMI);
        } else {
            self.expect(L_CURLY);
            while !self.at(R_CURLY) && !self.at(EOF) {
                let old = self.pos;
                self.body(false);
                self.expect(SEMI);
                if old == self.pos {
                    self.bump();
                }
            }
            self.expect(R_CURLY);
        }
        VIEWDECL
    }
    fn handler(&mut self) -> SyntaxKind {
        self.eat_ctx("monotone");
        if !(self.eat(ON_KW) || self.eat(WHILE_KW)) {
            self.expect(ON_KW);
        }
        self.body(true);
        self.block();
        HANDLERITEM
    }
    fn block(&mut self) {
        let m = self.start();
        self.expect(L_CURLY);
        if self.guard() {
            while !self.at(R_CURLY) && !self.at(EOF) {
                let old = self.pos;
                self.stmt();
                if old == self.pos {
                    self.bump();
                }
            }
            self.depth -= 1;
        }
        self.expect(R_CURLY);
        self.complete(m, BLOCK);
    }
    fn stmt(&mut self) {
        let m = self.start();
        self.attrs();
        let kind = match self.nth(0) {
            EMIT_KW | NEXT_KW | SEND_KW | DELETE_KW | UPSERT_KW | SEAL_KW => {
                let verb = self.nth(0);
                self.bump();
                // `emit TREE root…` writes a tree (docs/design/SUGAR.md §3): a tree's name, then an element.
                let tree = self.nth(0).is_word() && self.tree_after_name();
                if tree {
                    let t = self.start();
                    self.relpath();
                    self.complete(t, TREENAME);
                    self.element();
                } else {
                    self.head();
                }
                if matches!(verb, EMIT_KW | NEXT_KW) && self.eat_ctx("weight") {
                    self.expr(0);
                }
                if matches!(verb, SEND_KW | SEAL_KW) && self.eat_ctx("to") {
                    self.expr(0);
                }
                if verb == UPSERT_KW && self.eat_ctx("resolve") {
                    self.policy();
                }
                if !tree {
                    // A head's child heads (SUGAR.md §2), or the statement's end.
                    if self.at(L_CURLY) {
                        self.children();
                    } else {
                        self.expect(SEMI);
                    }
                }
                VERBSTMT
            }
            IF_KW => {
                self.bump();
                self.body(true);
                self.block();
                if self.eat(ELSE_KW) {
                    if self.at(IF_KW) {
                        self.stmt();
                    } else {
                        self.block();
                    }
                }
                IFSTMT
            }
            FOR_KW => {
                self.bump();
                self.body(true);
                self.block();
                FORSTMT
            }
            LET_KW => {
                self.error(
                    code!("BLS0102"),
                    "bind let in the handler header or condition",
                    &[EMIT_KW, NEXT_KW, SEND_KW, IF_KW, FOR_KW],
                );
                self.bump();
                self.recover(&[SEMI, R_CURLY]);
                self.eat(SEMI);
                ERROR
            }
            // `frag(args);`: a fragment's statements here (SUGAR.md §4).
            _ if self.element_ahead() => {
                self.element();
                CALLSTMT
            }
            _ => {
                self.error(
                    code!("BLS0100"),
                    "expected a statement verb, if or for",
                    &[EMIT_KW, NEXT_KW, SEND_KW, DELETE_KW, UPSERT_KW, SEAL_KW, IF_KW, FOR_KW],
                );
                self.recover(&[SEMI, R_CURLY]);
                self.eat(SEMI);
                ERROR
            }
        };
        self.complete(m, kind);
    }
    fn head(&mut self) {
        let m = self.start();
        self.relpath();
        self.args(false);
        self.complete(m, HEAD);
    }
    /// After a verb: whether the relation path ahead is followed by an element (a tree statement), not by `(`.
    fn tree_after_name(&self) -> bool {
        let mut n = 1;
        while self.nth(n) == DOT && self.nth(n + 1).is_word() {
            n += 2;
        }
        self.nth(n).is_word()
    }
    /// Whether a dashed name (`stroke-width`, `font-face`) starts here and ends just before `end`; the tokens it
    /// spans, or 0.
    fn dashed_name(&self, end: &[SyntaxKind]) -> usize {
        if !self.nth(0).is_word() {
            return 0;
        }
        let mut n = 1;
        while self.nth(n) == MINUS && self.nth(n + 1).is_word() {
            n += 2;
        }
        if end.contains(&self.nth(n)) { n } else { 0 }
    }
    /// A property's name: a dashed name, or a string (`"aria-label"`), before its `:`.
    fn prop_name(&mut self) -> bool {
        let n = if self.at(STRING_LIT) && self.nth(1) == COLON {
            1
        } else {
            self.dashed_name(&[COLON])
        };
        if n == 0 {
            return false;
        }
        let m = self.start();
        for _ in 0..n {
            self.bump();
        }
        self.complete(m, PROPNAME);
        self.expect(COLON);
        true
    }
    /// `{ name: value, … }` after a spread's `..` (SUGAR.md §5).
    fn record_lit(&mut self) {
        let m = self.start();
        self.expect(L_CURLY);
        while !self.at(R_CURLY) && !self.at(EOF) {
            let old = self.pos;
            let f = self.start();
            if !self.prop_name() {
                self.error(code!("BLS0100"), "expected `name: value`", &[IDENT]);
            }
            self.expr(0);
            self.complete(f, ARG);
            if old == self.pos {
                self.bump();
            }
            if !self.eat(COMMA) {
                break;
            }
        }
        self.expect(R_CURLY);
        self.complete(m, RECORDLIT);
    }
    /// An element of a tree, or a child head (SUGAR.md §§2–3): a name (dashes allowed; a path for a head), then
    /// optionally `[meta]`, `(arguments)` and `{ children }`; without children it ends with `;`.
    fn element(&mut self) {
        let m = self.start();
        let n = self.start();
        if self.nth(0).is_word() {
            self.bump();
            while (self.at(MINUS) || self.at(DOT)) && self.nth(1).is_word() {
                self.bump();
                self.bump();
            }
        } else {
            self.error(code!("BLS0100"), "expected an element or a head", &[IDENT]);
        }
        self.complete(n, ELEMNAME);
        if self.at(L_BRACK) {
            let meta = self.start();
            self.bump();
            while !self.at(R_BRACK) && !self.at(EOF) {
                let old = self.pos;
                let a = self.start();
                if self.nth(0).is_word() && self.nth(1) == COLON {
                    self.name(true);
                    self.bump();
                }
                self.expr(0);
                self.complete(a, ARG);
                if old == self.pos {
                    self.bump();
                }
                if !self.eat(COMMA) {
                    break;
                }
            }
            self.expect(R_BRACK);
            self.complete(meta, META);
        }
        if self.at(L_PAREN) {
            self.args(false);
        }
        if self.at(L_CURLY) {
            self.children();
        } else {
            self.expect(SEMI);
        }
        self.complete(m, ELEMENT);
    }
    /// `{ child… }`: elements or child heads, `if`/`for` blocks of them, and content (a bare expression).
    fn children(&mut self) {
        let m = self.start();
        self.expect(L_CURLY);
        if self.guard() {
            while !self.at(R_CURLY) && !self.at(EOF) {
                let old = self.pos;
                self.child();
                if old == self.pos {
                    self.bump();
                }
            }
            self.depth -= 1;
        }
        self.expect(R_CURLY);
        self.complete(m, CHILDREN);
    }
    fn child(&mut self) {
        match self.nth(0) {
            IF_KW => {
                let m = self.start();
                self.bump();
                self.body(true);
                self.children();
                if self.eat(ELSE_KW) {
                    if self.at(IF_KW) {
                        self.child();
                    } else {
                        self.children();
                    }
                }
                self.complete(m, IFCHILD);
            }
            FOR_KW => {
                let m = self.start();
                self.bump();
                self.body(true);
                self.children();
                self.complete(m, FORCHILD);
            }
            // A statement among a fragment's or a tree's items.
            EMIT_KW | NEXT_KW | SEND_KW | DELETE_KW | UPSERT_KW | SEAL_KW => self.stmt(),
            _ if self.element_ahead() => self.element(),
            _ => {
                let m = self.start();
                self.expr(0);
                self.eat(SEMI);
                self.complete(m, CONTENT);
            }
        }
    }
    /// Whether an element starts here: a name (dashed or dotted) followed by `[`, `(`, `{` or `;`.
    fn element_ahead(&self) -> bool {
        if !self.nth(0).is_word() {
            return false;
        }
        let mut n = 1;
        while matches!(self.nth(n), MINUS | DOT) && self.nth(n + 1).is_word() {
            n += 2;
        }
        matches!(self.nth(n), L_BRACK | L_PAREN | L_CURLY)
    }
    fn body(&mut self, no_struct: bool) {
        let m = self.start();
        let prev = self.no_struct;
        self.no_struct = no_struct;
        loop {
            let old = self.pos;
            self.literal();
            if old == self.pos {
                self.recover(&[COMMA, SEMI, L_CURLY, R_CURLY]);
            }
            if !self.eat(COMMA) {
                break;
            }
            if matches!(self.nth(0), R_CURLY | SEMI | EOF) {
                self.error(code!("BLS0100"), "expected a body literal", &[IDENT]);
                break;
            }
        }
        if self.eat(WHERE_KW) {
            loop {
                self.expr(0);
                if !self.eat(COMMA) {
                    break;
                }
            }
        }
        self.no_struct = prev;
        self.complete(m, BODY);
    }
    fn literal(&mut self) {
        let m = self.start();
        if !self.guard() {
            self.complete(m, ERROR);
            return;
        }
        let kind = if self.eat(NOT_KW) {
            if self.eat(L_CURLY) {
                self.body(false);
                self.expect(R_CURLY);
            } else {
                self.literal();
            }
            NOTLIT
        } else if self.eat(LET_KW) {
            self.expr(0);
            self.expect(EQ);
            self.expr(0);
            LETLIT
        } else if self.ctx("any") && self.nth(1) == L_CURLY {
            self.bump();
            self.bump();
            while !self.at(R_CURLY) && !self.at(EOF) {
                let old = self.pos;
                self.body(false);
                if !self.eat(SEMI) {
                    break;
                }
                if old == self.pos {
                    self.bump();
                }
            }
            self.expect(R_CURLY);
            ANYLIT
        } else if self.ctx("forall") && matches!(self.nth(1), IDENT | L_PAREN) {
            self.bump();
            let prev = self.no_struct;
            self.no_struct = true;
            self.atom();
            self.no_struct = prev;
            self.expect(L_CURLY);
            self.body(false);
            self.expect(R_CURLY);
            FORALLLIT
        } else if self.ctx("quorum") && self.nth(1) == IDENT && self.nth(2) == IN_KW {
            self.bump();
            self.name(false);
            self.bump();
            self.relpath();
            self.expect(L_CURLY);
            self.body(false);
            self.expect(R_CURLY);
            QUORUMLIT
        } else if self.at(IDENT)
            && ((self.nth(1) == IDENT
                && matches!(
                    self.spelling(0),
                    "outer" | "inserted" | "deleted" | "sealed" | "per" | "ever" | "sent"
                ))
                || (self.ctx("final") && matches!(self.nth(1), IDENT | NOT_KW)))
        {
            let k = match self.spelling(0) {
                "outer" => OUTERLIT,
                "inserted" => INSERTEDLIT,
                "deleted" => DELETEDLIT,
                "sealed" => SEALEDLIT,
                "per" => PERLIT,
                "ever" => EVERLIT,
                "sent" => SENTLIT,
                _ => FINALLIT,
            };
            self.bump();
            if k == FINALLIT {
                self.eat(NOT_KW);
            }
            self.atom();
            k
        } else {
            self.atom_inner();
            ATOMLIT
        };
        self.depth -= 1;
        self.complete(m, kind);
    }
    fn atom(&mut self) {
        let m = self.start();
        self.atom_inner();
        self.complete(m, ATOMLIT);
    }
    fn atom_inner(&mut self) {
        self.expr(0);
        loop {
            let kind = if self.ctx("from") {
                FROMSUFFIX
            } else if self.ctx("principal") {
                PRINCIPALSUFFIX
            } else if self.ctx("weight") {
                WEIGHTSUFFIX
            } else if self.at(AT) {
                ATSUFFIX
            } else if self.ctx("at") && self.nth_ctx(1, "tick") {
                ATTICKSUFFIX
            } else {
                break;
            };
            let s = self.start();
            self.bump();
            if kind == ATTICKSUFFIX {
                self.bump();
            }
            self.expr(5);
            self.complete(s, kind);
        }
        if self.bang_clause() {
            self.error(
                code!("BLS0107"),
                "operator clauses belong inside bang-call parentheses",
                &[COMMA, L_CURLY, SEMI],
            );
            while self.bang_clause() {
                self.parse_bang_clause();
            }
        }
    }
    fn bang_clause(&self) -> bool {
        self.at(IDENT)
            && matches!(
                self.spelling(0),
                "per" | "by" | "default" | "least" | "most" | "sticky" | "durable" | "release"
            )
    }
    fn parse_bang_clause(&mut self) {
        let m = self.start();
        let spelling = self.spelling(0).to_owned();
        self.bump();
        match spelling.as_str() {
            "per" | "default" | "least" | "most" => {
                self.expr(0);
            }
            "by" => {
                let o = self.start();
                if self.eat(L_PAREN) {
                    while !self.at(R_PAREN) && !self.at(EOF) {
                        let old = self.pos;
                        self.order_key();
                        if self.pos == old {
                            self.bump();
                        }
                        if !self.eat(COMMA) {
                            break;
                        }
                    }
                    self.expect(R_PAREN);
                } else {
                    self.order_key();
                }
                self.complete(o, ORDERKEYS);
            }
            _ => {}
        }
        self.complete(m, BANGCLAUSE);
    }
    fn order_key(&mut self) {
        let m = self.start();
        self.expr(0);
        if self.ctx("asc") || self.ctx("desc") {
            self.bump();
        }
        self.complete(m, ORDERKEY);
    }
    fn args(&mut self, bang: bool) {
        self.expect(L_PAREN);
        while !self.at(R_PAREN) && !self.at(EOF) {
            let old = self.pos;
            if bang && self.bang_clause() {
                self.parse_bang_clause();
            } else {
                let a = self.start();
                if self.eat(RANGE) {
                    // `..` alone (a body atom's rest), or a spread: `..{ name: value, … }`, `..map` (SUGAR.md §5).
                    if !bang && self.at(L_CURLY) {
                        self.record_lit();
                    } else if !bang && !self.at(R_PAREN) && !self.at(COMMA) {
                        self.expr(0);
                    }
                } else if !self.eat(STAR) {
                    if !bang && self.nth(0).is_word() && self.nth(1) == COLON {
                        self.name(true);
                        self.bump();
                    } else if !bang {
                        self.prop_name();
                    }
                    self.expr(0);
                }
                self.complete(a, ARG);
            }
            if old == self.pos {
                self.bump();
            }
            if !(self.eat(COMMA) || bang && self.bang_clause()) {
                break;
            }
        }
        self.expect(R_PAREN);
    }
    fn block_expr(&mut self) {
        let m = self.start();
        self.expect(L_CURLY);
        while self.at(LET_KW) {
            let l = self.start();
            self.bump();
            self.expr(0);
            if self.eat(COLON) {
                self.ty();
            }
            self.expect(EQ);
            self.expr(0);
            self.expect(SEMI);
            self.complete(l, LETLIT);
        }
        if !self.at(R_CURLY) {
            self.expr(0);
        }
        self.expect(R_CURLY);
        self.complete(m, BLOCKEXPR);
    }
    // Binding powers reproduce LANGUAGE §3.3 (level*2). Comparisons and ranges cannot chain.
    fn infix(&self) -> Option<(u8, u8, bool, bool)> {
        let k = self.nth(0);
        let (n, right, nonassoc) = match k {
            OR2 => (1, false, false),
            AND2 => (2, false, false),
            EQ2 | NEQ | LT | LE | GT | GE | IN_KW => (4, false, true),
            RANGE | RANGE_EQ | OPEN_RANGE | OPEN_RANGE_EQ => (5, false, true),
            PIPE => (6, false, false),
            CARET => (7, false, false),
            AMP => (8, false, false),
            SHL | SHR => (9, false, false),
            PLUS | MINUS | CONCAT => (10, false, false),
            STAR | SLASH | PERCENT => (11, false, false),
            POW => (12, true, false),
            AS_KW => (13, false, false),
            _ => return None,
        };
        if self.fold_element == Some(self.depth) && k == PIPE {
            return None;
        }
        let l = n * 2;
        Some((l, if right { l } else { l + 1 }, nonassoc, k == AS_KW))
    }
    fn expr(&mut self, min: u8) -> Completed {
        let m = self.start();
        let outer = std::mem::replace(&mut self.sub, 0);
        if !self.guard() {
            self.height = 1;
            self.sub = outer.max(1);
            return self.complete(m, ERROR);
        }
        let mut lhs = self.prefix(m);
        let mut h = self.height;
        let mut previous_nonassoc: Option<u8> = None;
        while let Some((left, right, nonassoc, cast)) = self.infix() {
            if left < min {
                break;
            }
            if nonassoc && previous_nonassoc == Some(left) {
                self.error(
                    code!("BLS0103"),
                    "chained comparisons or ranges require parentheses",
                    &[COMMA, SEMI, R_PAREN],
                );
            }
            previous_nonassoc = if nonassoc { Some(left) } else { None };
            // Past the height bound, the rest of the chain is parsed without nesting it further (the program is
            // already rejected).
            let wrap = !self.higher(h + 1);
            let node = wrap.then(|| self.precede(lhs));
            self.bump();
            if cast {
                self.ty();
            } else {
                self.sub = 0;
                self.expr(right);
            }
            if let Some(node) = node {
                h = h.max(if cast { 0 } else { self.height }) + 1;
                lhs = self.complete(node, if cast { CASTEXPR } else { BINARYEXPR });
            }
        }
        self.depth -= 1;
        self.height = h;
        self.sub = outer.max(h);
        lhs
    }
    /// Whether an expression of height `h` is past the bound, reporting the first in the file.
    fn higher(&mut self, h: usize) -> bool {
        if h <= MAX_EXPR_HEIGHT {
            return false;
        }
        if !self.too_high {
            self.too_high = true;
            self.error(
                code!("BLS0100"),
                &format!(
                    "an expression nested more than {MAX_EXPR_HEIGHT} levels deep (a chain of operators or calls): \
                     split it"
                ),
                &[],
            );
        }
        true
    }
    /// A prefix expression; sets `height`.
    fn prefix(&mut self, m: Marker) -> Completed {
        if self.at(NOT_KW) {
            self.bump();
            self.expr(6);
            self.height += 1;
            return self.complete(m, PREFIXEXPR);
        }
        if self.at(MINUS) || self.at(TILDE) {
            self.bump();
            self.expr(28);
            self.height += 1;
            return self.complete(m, PREFIXEXPR);
        }
        self.sub = 0;
        if self.at(PIPE) {
            self.bump();
            while !self.at(PIPE) && !self.at(EOF) {
                let old = self.pos;
                self.expr(13);
                if old == self.pos {
                    self.bump();
                }
                if !self.eat(COMMA) {
                    break;
                }
            }
            self.expect(PIPE);
            // A closure's body is an expression, or a block of `let`s and a final expression.
            if self.at(L_CURLY) {
                self.block_expr();
            } else {
                self.expr(0);
            }
            self.height = self.sub + 1;
            return self.complete(m, CLOSUREEXPR);
        }
        let mut lhs = self.primary(m);
        let mut h = self.sub + 1;
        loop {
            if matches!(self.nth(0), DOT | L_PAREN | L_BRACK | QUESTION) && self.higher(h + 1) {
                // Past the height bound: the rest of the chain is not parsed as part of this expression.
                break;
            }
            self.sub = 0;
            if self.at(DOT) {
                let node = self.precede(lhs);
                self.bump();
                if self.at(INT_LIT) {
                    self.bump();
                    lhs = self.complete(node, TUPLEINDEXEXPR);
                } else if self.at(BANG_IDENT) {
                    self.bump();
                    self.args(true);
                    lhs = self.complete(node, METHODCALLEXPR);
                } else {
                    self.name(true);
                    if self.at(L_PAREN) {
                        self.args(false);
                        lhs = self.complete(node, METHODCALLEXPR);
                    } else {
                        lhs = self.complete(node, FIELDEXPR);
                    }
                }
            } else if self.at(L_PAREN) {
                let node = self.precede(lhs);
                self.args(false);
                lhs = self.complete(node, CALLEXPR);
            } else if self.at(L_BRACK) {
                let node = self.precede(lhs);
                self.bump();
                self.expr(0);
                self.expect(R_BRACK);
                lhs = self.complete(node, INDEXEXPR);
            } else if self.at(QUESTION) {
                let node = self.precede(lhs);
                self.bump();
                lhs = self.complete(node, TRYEXPR);
            } else {
                break;
            }
            h = h.max(self.sub) + 1;
        }
        self.height = h;
        lhs
    }
    fn primary(&mut self, m: Marker) -> Completed {
        match self.nth(0) {
            INT_LIT | FLOAT_LIT | DURATION_LIT | MOD_LIT | STRING_LIT | RAW_STRING_LIT | BYTES_LIT | TRUE_KW
            | FALSE_KW => {
                self.bump();
                self.complete(m, LITERALEXPR)
            }
            FSTRING_START => {
                // `f"text {expr[:spec]} text"`: the lexer splits it into text runs and holes (LANGUAGE §2.4).
                self.bump();
                loop {
                    match self.nth(0) {
                        FSTRING_TEXT => self.bump(),
                        L_CURLY => {
                            let h = self.start();
                            self.bump();
                            let no_struct = self.no_struct;
                            self.no_struct = false;
                            self.expr(0);
                            self.no_struct = no_struct;
                            if self.eat(COLON) {
                                self.expect(FSTRING_SPEC);
                            }
                            self.expect(R_CURLY);
                            self.complete(h, FSTRINGHOLE);
                        }
                        FSTRING_END => {
                            self.bump();
                            break;
                        }
                        _ => {
                            self.error(code!("BLS0100"), "unterminated interpolated string", &[FSTRING_END]);
                            break;
                        }
                    }
                }
                self.complete(m, FSTRINGEXPR)
            }
            UNDERSCORE => {
                self.bump();
                self.complete(m, WILDCARD)
            }
            SELF_KW => {
                self.bump();
                self.complete(m, SELFEXPR)
            }
            L_PAREN => {
                self.bump();
                let no_struct = self.no_struct;
                self.no_struct = false;
                if self.eat(R_PAREN) {
                    self.no_struct = no_struct;
                    return self.complete(m, TUPLEEXPR);
                }
                self.expr(0);
                let tuple = self.eat(COMMA);
                if tuple {
                    while !self.at(R_PAREN) && !self.at(EOF) {
                        let old = self.pos;
                        self.expr(0);
                        if old == self.pos {
                            self.bump();
                        }
                        if !self.eat(COMMA) {
                            break;
                        }
                    }
                }
                self.expect(R_PAREN);
                self.no_struct = no_struct;
                self.complete(m, if tuple { TUPLEEXPR } else { PARENEXPR })
            }
            L_BRACK => {
                self.bump();
                self.expr_list(R_BRACK);
                self.complete(m, VECEXPR)
            }
            IF_KW => {
                self.bump();
                let no = self.no_struct;
                self.no_struct = true;
                self.expr(0);
                self.no_struct = no;
                self.block_expr();
                if self.eat(ELSE_KW) {
                    if self.at(L_CURLY) {
                        self.block_expr();
                    } else {
                        self.expr(0);
                    }
                } else {
                    self.error(code!("BLS0109"), "if expression requires else", &[ELSE_KW]);
                }
                self.complete(m, IFEXPR)
            }
            MATCH_KW => {
                self.bump();
                let no = self.no_struct;
                self.no_struct = true;
                self.expr(0);
                self.no_struct = no;
                self.expect(L_CURLY);
                while !self.at(R_CURLY) && !self.at(EOF) {
                    let old = self.pos;
                    let a = self.start();
                    self.expr(0);
                    if self.eat(IF_KW) {
                        self.expr(0);
                    }
                    self.expect(FAT_ARROW);
                    // An arm's body is an expression, or a block of `let`s and a final expression; after a block the
                    // comma is optional.
                    let block = self.at(L_CURLY);
                    if block {
                        self.block_expr();
                    } else {
                        self.expr(0);
                    }
                    self.complete(a, MATCHARM);
                    if old == self.pos {
                        self.bump();
                    }
                    if !self.eat(COMMA) && !block {
                        break;
                    }
                }
                self.expect(R_CURLY);
                self.complete(m, MATCHEXPR)
            }
            BANG_IDENT => {
                self.bump();
                self.args(true);
                self.complete(m, BANGCALLEXPR)
            }
            IDENT if self.spelling(0) == "set" && self.nth(1) == L_BRACK => {
                self.bump();
                self.bump();
                self.expr_list(R_BRACK);
                self.complete(m, SETEXPR)
            }
            IDENT if self.spelling(0) == "map" && self.nth(1) == L_BRACK => {
                self.bump();
                self.bump();
                while !self.at(R_BRACK) && !self.at(EOF) {
                    let old = self.pos;
                    self.expr(0);
                    self.expect(FAT_ARROW);
                    self.expr(0);
                    if old == self.pos {
                        self.bump();
                    }
                    if !self.eat(COMMA) {
                        break;
                    }
                }
                self.expect(R_BRACK);
                self.complete(m, MAPEXPR)
            }
            IDENT
                if matches!(
                    self.spelling(0),
                    "lset" | "lmax" | "lmin" | "lbool" | "lmap" | "lbag" | "lpset"
                ) && self.nth(1) == L_CURLY =>
            {
                self.bump();
                self.bump();
                let prev = self.fold_element;
                self.fold_element = Some(self.depth + 1);
                self.expr(0);
                self.fold_element = prev;
                if self.eat(FAT_ARROW) {
                    self.fold_element = Some(self.depth + 1);
                    self.expr(0);
                    self.fold_element = prev;
                }
                self.expect(PIPE);
                self.body(false);
                self.expect(R_CURLY);
                self.complete(m, FOLDEXPR)
            }
            IDENT => {
                self.path(false, true);
                if self.at(L_CURLY) && self.struct_shape() {
                    if self.no_struct {
                        if self.nth(1) == RANGE || self.nth(2) == COLON {
                            self.error(
                                code!("BLS0104"),
                                "parenthesize struct literals in this context",
                                &[L_PAREN],
                            );
                        }
                    } else {
                        self.bump();
                        while !self.at(R_CURLY) && !self.at(EOF) {
                            let old = self.pos;
                            let f = self.start();
                            if self.eat(RANGE) {
                                self.expr(0);
                            } else {
                                self.name(true);
                                if self.eat(COLON) {
                                    self.expr(0);
                                }
                            }
                            self.complete(f, FIELDINIT);
                            if old == self.pos {
                                self.bump();
                            }
                            if !self.eat(COMMA) {
                                break;
                            }
                        }
                        self.expect(R_CURLY);
                        return self.complete(m, STRUCTLITEXPR);
                    }
                }
                self.complete(m, PATHEXPR)
            }
            _ => {
                self.error(code!("BLS0100"), "expected expression", &[IDENT, INT_LIT, L_PAREN]);
                if !matches!(self.nth(0), EOF | SEMI | COMMA | R_CURLY | R_PAREN | R_BRACK | L_CURLY) {
                    self.bump();
                } else {
                    self.events.push(Event::Missing);
                }
                self.complete(m, ERROR)
            }
        }
    }
    fn struct_shape(&self) -> bool {
        self.nth(1) == R_CURLY
            || self.nth(1) == RANGE
            || self.nth(1).is_word() && matches!(self.nth(2), COLON | COMMA | R_CURLY)
    }
    fn expr_list(&mut self, end: SyntaxKind) {
        while !self.at(end) && !self.at(EOF) {
            let old = self.pos;
            self.expr(0);
            if old == self.pos {
                self.bump();
            }
            if !self.eat(COMMA) {
                break;
            }
        }
        self.expect(end);
    }
    fn snapshot(&mut self) {
        self.bump();
        self.name(false);
        self.expect_ctx("of");
        self.relpath();
        self.expect_ctx("at");
        self.expect_ctx("progress");
        if self.eat_ctx("every") {
            self.expr(0);
            self.expect_ctx("upto");
            self.expr(0);
        } else if self.eat(L_PAREN) {
            self.expr_list(R_PAREN);
        } else {
            self.error(code!("BLS0100"), "expected snapshot schedule", &[L_PAREN, IDENT]);
        }
        if self.eat_ctx("mode") {
            self.name(false);
        }
        if self.eat_ctx("estimate") {
            self.expr(0);
        }
        self.expect(SEMI);
    }
    fn spec(&mut self) -> SyntaxKind {
        self.bump();
        self.name(false);
        if self.eat(FOR_KW) {
            self.path(false, false);
            if self.at(LT) {
                self.generic_args();
            }
            if self.at(L_PAREN) {
                self.named_args();
            }
        }
        self.items_block(true);
        SPECITEM
    }
    fn spec_member(&mut self) {
        let m = self.start();
        self.attrs();
        let kind = if self.eat_ctx("nodes") {
            self.name(false);
            while self.eat(COMMA) {
                self.name(false);
            }
            self.expect(SEMI);
            NODESMEMBER
        } else if self.eat_ctx("assign") {
            self.name(false);
            self.expect(EQ);
            self.expect(L_BRACK);
            while !self.at(R_BRACK) && !self.at(EOF) {
                self.name(false);
                if !self.eat(COMMA) {
                    break;
                }
            }
            self.expect(R_BRACK);
            self.expect(SEMI);
            ASSIGNMEMBER
        } else if self.eat_ctx("faults") {
            self.opt_block();
            FAULTSMEMBER
        } else if self.eat_ctx("liveness") {
            self.name(false);
            self.expect(COLON);
            self.expect_ctx("eventually");
            self.body(false);
            self.expect_ctx("within");
            self.expr(0);
            self.expect_ctx("ticks");
            self.expect_ctx("after");
            if !self.eat_ctx("eff") {
                self.expr(0);
            }
            self.expect(SEMI);
            LIVENESSMEMBER
        } else if self.eat_ctx("prove") {
            self.name(false);
            self.expect_ctx("by");
            self.expect_ctx("induction");
            if self.eat_ctx("using") {
                self.name(false);
                while self.eat(COMMA) {
                    self.name(false);
                }
            }
            self.expect(SEMI);
            PROVEMEMBER
        } else if self.eat_ctx("expect") {
            if !(self.eat_ctx("confluent") || self.eat_ctx("deterministic")) {
                self.expect_ctx("confluent");
            }
            self.expect(L_PAREN);
            self.relpath();
            self.expect(R_PAREN);
            self.expect(SEMI);
            EXPECTMEMBER
        } else if self.eat_ctx("check") {
            self.name(false);
            if self.at(L_CURLY) {
                self.opt_block();
            }
            if self.eat_ctx("expect") {
                self.name(false);
            }
            self.expect(SEMI);
            CHECKMEMBER
        } else if matches!(self.nth(0), CONST_KW | INCLUDE_KW | VIEW_KW | INVARIANT_KW) || self.ctx("fact") {
            self.item_kind()
        } else {
            self.error(
                code!("BLS0100"),
                "expected spec member",
                &[CONST_KW, INCLUDE_KW, VIEW_KW, INVARIANT_KW],
            );
            self.recover(&[SEMI, R_CURLY]);
            self.eat(SEMI);
            ERROR
        };
        self.complete(m, kind);
    }
    fn opt_block(&mut self) {
        let m = self.start();
        self.expect(L_CURLY);
        while !self.at(R_CURLY) && !self.at(EOF) {
            let old = self.pos;
            let f = self.start();
            self.name(false);
            self.expect(COLON);
            self.expr(0);
            self.complete(f, OPTFIELD);
            if old == self.pos {
                self.bump();
            }
            if !self.eat(COMMA) {
                break;
            }
        }
        self.expect(R_CURLY);
        self.complete(m, OPTBLOCK);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    proptest! {
        #[test]
        fn cst_lossless_roundtrip(s in ".{0,2048}") {
            let p=parse(FileId::from_raw(0),&s);
            prop_assert_eq!(p.syntax().to_string(),s);
            prop_assert!(p.errors.iter().all(|e|e.primary.is_some()));
        }
    }
}
