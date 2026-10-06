//! The formatter (LANGUAGE §3.5, ARCHITECTURE §13.1): one canonical format, no options.
//!
//! It prints the lossless CST again, deciding only the space between tokens and where lines break: it never adds,
//! removes, changes or reorders a token, so the formatted file is the same program token for token. Comments stay
//! where they were (a comment on its own line before what follows it, a comment after a token on its line after
//! it); `#` comments become `//` comments (a leading `#!` line stays as it is). Blank lines between items,
//! statements and members are kept (at most one).
//!
//! Layout: items, statements, struct fields, enum variants, match arms and a tree element's children each take a
//! line; brackets in expressions (arguments, tuples, collections, struct literals) stay on one line when they fit
//! in [`WIDTH`] columns and otherwise put one entry per line, indented by four; a rule's header that does not fit
//! breaks after its commas with a continuation indent of eight, and then `where` starts a line. The layout engine
//! is Wadler's pretty printer: a group is printed flat when it fits, else broken.

use blossom_base::FileId;

use crate::parser::{self, ParseError};
use crate::{SyntaxKind, SyntaxNode, SyntaxToken};

/// The line width.
pub const WIDTH: usize = 120;
const INDENT: usize = 4;
/// A rule header's continuation indent.
const CONTINUATION: usize = 8;

/// Why a source was not formatted.
#[derive(Debug)]
pub enum FormatError {
    /// The source does not parse: it is left as it is.
    Syntax(Vec<ParseError>),
}

impl std::fmt::Display for FormatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FormatError::Syntax(errors) => {
                write!(f, "the source does not parse ({} error(s))", errors.len())
            }
        }
    }
}

/// Formats a Blossom source; a source with syntax errors is refused, not guessed at.
pub fn format(source: &str) -> Result<String, FormatError> {
    let parse = parser::parse(FileId::from_raw(0), source);
    if !parse.errors.is_empty() {
        return Err(FormatError::Syntax(parse.errors));
    }
    let root = parse.syntax();
    let mut p = Printer::new(&root);
    let doc = p.file(&root);
    Ok(render(&doc))
}

// ---------------------------------------------------------------- the document

/// A layout: text and the places it may break.
#[derive(Clone, Debug)]
enum Doc {
    Text(String),
    /// A space.
    Space,
    /// A space, or a line break when its group breaks.
    Line,
    /// Nothing, or a line break when its group breaks.
    SoftLine,
    /// In a broken group: a space if what follows (up to the next `FillLine` or break) fits on the line, else a line
    /// break. Flat: a space.
    FillLine,
    /// A line break.
    Hard,
    /// A blank line, if a hard break is due here (the source had one between two items or statements).
    BlankHint,
    Indent(usize, Vec<Doc>),
    /// Flat when it fits, else broken.
    Group(Vec<Doc>),
    /// Always flat (an interpolated string's holes).
    Flat(Vec<Doc>),
}

/// Whether a document must break: it holds a hard break outside a `Flat`.
fn hard(docs: &[Doc]) -> bool {
    docs.iter().any(|d| match d {
        Doc::Hard => true,
        Doc::Indent(_, ds) | Doc::Group(ds) => hard(ds),
        _ => false,
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Flat,
    Break,
}

/// Prints a document in [`WIDTH`] columns, with no trailing spaces and one final newline.
fn render(doc: &[Doc]) -> String {
    let mut out = String::new();
    let mut column = 0usize;
    // Breaks are pending until the next text: so a run of breaks is one (or one blank line), and a line never ends
    // in spaces. `hard` says the pending break may become a blank line.
    let mut pending = 0usize;
    let mut pending_hard = false;
    // The indentation of the line a pending break starts: the break's own.
    let mut pending_indent = 0usize;
    let mut space = false;
    let mut stack: Vec<(usize, Mode, &Doc)> = doc.iter().rev().map(|d| (0, Mode::Break, d)).collect();
    while let Some((indent, mode, d)) = stack.pop() {
        match d {
            Doc::Text(s) => {
                if pending > 0 {
                    if !out.is_empty() {
                        for _ in 0..pending.min(2) {
                            out.push('\n');
                        }
                    }
                    for _ in 0..pending_indent {
                        out.push(' ');
                    }
                    column = pending_indent;
                    pending = 0;
                    pending_hard = false;
                } else if space {
                    out.push(' ');
                    column += 1;
                }
                space = false;
                out.push_str(s);
                column += s.chars().count();
            }
            Doc::Space => space = pending == 0,
            Doc::Line | Doc::SoftLine | Doc::FillLine if mode == Mode::Flat => {
                if !matches!(d, Doc::SoftLine) {
                    space = pending == 0;
                }
            }
            Doc::FillLine if fits_segment(WIDTH.saturating_sub(column + 1), &stack) => space = pending == 0,
            // A run of breaks is one: the last one's indentation is the line's.
            Doc::Line | Doc::SoftLine | Doc::FillLine => {
                pending_indent = indent;
                pending = pending.max(1);
                space = false;
            }
            Doc::Hard => {
                pending_indent = indent;
                pending = pending.max(1);
                pending_hard = true;
                space = false;
            }
            Doc::BlankHint => {
                if pending > 0 && pending_hard {
                    pending = 2;
                }
            }
            Doc::Indent(n, ds) => {
                for x in ds.iter().rev() {
                    stack.push((indent + n, mode, x));
                }
            }
            Doc::Flat(ds) => {
                for x in ds.iter().rev() {
                    stack.push((indent, Mode::Flat, x));
                }
            }
            Doc::Group(ds) => {
                let start = if pending > 0 {
                    indent
                } else {
                    column + usize::from(space)
                };
                let flat = mode == Mode::Flat || (!hard(ds) && fits(WIDTH.saturating_sub(start), ds, &stack));
                let m = if flat { Mode::Flat } else { Mode::Break };
                for x in ds.iter().rev() {
                    stack.push((indent, m, x));
                }
            }
        }
    }
    out.push('\n');
    out
}

/// Whether `docs`, flat, and what follows them up to the next possible break fit in `width` columns.
fn fits(width: usize, docs: &[Doc], rest: &[(usize, Mode, &Doc)]) -> bool {
    let mut left = width as isize;
    let mut todo: Vec<(Mode, &Doc)> = docs.iter().rev().map(|d| (Mode::Flat, d)).collect();
    let mut rest = rest.iter().rev();
    loop {
        let Some((mode, d)) = todo.pop() else {
            // The group fits; the text after it must too, up to its next break.
            match rest.next() {
                Some((_, m, d)) => {
                    todo.push((*m, d));
                    continue;
                }
                None => return true,
            }
        };
        match d {
            Doc::Text(s) => left -= s.chars().count() as isize,
            Doc::Space => left -= 1,
            Doc::Line | Doc::FillLine if mode == Mode::Flat => left -= 1,
            Doc::SoftLine if mode == Mode::Flat => {}
            Doc::Line | Doc::SoftLine | Doc::FillLine | Doc::Hard => return left >= 0,
            Doc::BlankHint => {}
            // What follows the group keeps its mode: a later group's break ends the measure.
            Doc::Indent(_, ds) | Doc::Group(ds) => todo.extend(ds.iter().rev().map(|x| (mode, x))),
            Doc::Flat(ds) => todo.extend(ds.iter().rev().map(|x| (Mode::Flat, x))),
        }
        if left < 0 {
            return false;
        }
    }
}

/// Whether what follows a `FillLine`, up to the next `FillLine` or break, fits in `width` columns.
fn fits_segment(width: usize, rest: &[(usize, Mode, &Doc)]) -> bool {
    let mut left = width as isize;
    let mut todo: Vec<(Mode, &Doc)> = Vec::new();
    let mut rest = rest.iter().rev();
    loop {
        let (mode, d) = match todo.pop() {
            Some(x) => x,
            None => match rest.next() {
                Some((_, m, d)) => (*m, *d),
                None => return left >= 0,
            },
        };
        match d {
            Doc::Text(s) => left -= s.chars().count() as isize,
            Doc::Space => left -= 1,
            Doc::Line if mode == Mode::Flat => left -= 1,
            Doc::SoftLine if mode == Mode::Flat => {}
            Doc::FillLine | Doc::Line | Doc::SoftLine | Doc::Hard => return left >= 0,
            Doc::BlankHint => {}
            Doc::Indent(_, ds) => todo.extend(ds.iter().rev().map(|x| (mode, x))),
            Doc::Group(ds) | Doc::Flat(ds) => todo.extend(ds.iter().rev().map(|x| (Mode::Flat, x))),
        }
        if left < 0 {
            return false;
        }
    }
}

// ---------------------------------------------------------------- comments

/// A comment, as it is printed.
#[derive(Clone, Debug)]
struct Comment {
    text: String,
    /// A blank line before it in the source.
    blank: bool,
    /// A `//` comment: a line ends after it.
    line: bool,
}

/// What surrounds a token in the source: comments on their own lines before it, a comment after it on its line,
/// and whether a blank line comes before it.
#[derive(Clone, Debug, Default)]
struct Around {
    leading: Vec<Comment>,
    trailing: Vec<Comment>,
    blank: bool,
}

/// A comment's printed text: a `#` comment as a `//` one.
fn comment_text(t: &SyntaxToken, first: bool) -> String {
    let text = t.text();
    if t.kind() != SyntaxKind::HASH_COMMENT || (first && text.starts_with("#!")) {
        return text.trim_end().to_owned();
    }
    let rest = text.strip_prefix('#').unwrap_or(text).trim_end();
    if rest.is_empty() || rest.starts_with(' ') {
        format!("//{rest}")
    } else {
        format!("// {rest}")
    }
}

fn is_comment(k: SyntaxKind) -> bool {
    use SyntaxKind::*;
    matches!(
        k,
        LINE_COMMENT | DOC_COMMENT | INNER_DOC_COMMENT | BLOCK_COMMENT | HASH_COMMENT
    )
}

// ---------------------------------------------------------------- the printer

struct Printer {
    /// Each non-trivia token's surroundings, by its offset.
    around: std::collections::BTreeMap<u32, Around>,
    /// Comments after the last token.
    tail: Vec<Comment>,
    /// The last token printed, for the space before the next.
    prev: Option<SyntaxToken>,
    /// No space before the next token (a break was just placed).
    tight: bool,
}

fn offset(t: &SyntaxToken) -> u32 {
    t.text_range().start().into()
}

impl Printer {
    fn new(root: &SyntaxNode) -> Printer {
        let mut around = std::collections::BTreeMap::new();
        let mut pending: Vec<Comment> = Vec::new();
        let mut newlines = 0usize;
        let mut last: Option<u32> = None;
        let mut first = true;
        for t in root.descendants_with_tokens().filter_map(|e| e.into_token()) {
            let k = t.kind();
            if k == SyntaxKind::WHITESPACE {
                newlines += t.text().matches('\n').count();
                continue;
            }
            if is_comment(k) {
                let c = Comment {
                    text: comment_text(&t, first),
                    blank: newlines >= 2,
                    line: k != SyntaxKind::BLOCK_COMMENT,
                };
                first = false;
                match last {
                    // On the line of the token before it: that token's.
                    Some(at) if newlines == 0 && pending.is_empty() => {
                        around.entry(at).or_insert_with(Around::default).trailing.push(c);
                    }
                    _ => pending.push(c),
                }
                newlines = 0;
                continue;
            }
            first = false;
            let at = offset(&t);
            let entry = around.entry(at).or_insert_with(Around::default);
            entry.leading = std::mem::take(&mut pending);
            entry.blank = newlines >= 2;
            newlines = 0;
            last = Some(at);
        }
        Printer {
            around,
            tail: pending,
            prev: None,
            tight: true,
        }
    }

    fn file(&mut self, root: &SyntaxNode) -> Vec<Doc> {
        // Items one per line (a token between them, which an error-free parse never leaves, stays in its place).
        let mut out = Vec::new();
        let mut first = true;
        for el in root.children_with_tokens() {
            match el {
                rowan::NodeOrToken::Node(n) => {
                    if !first {
                        out.push(Doc::Hard);
                        self.tight = true;
                    }
                    out.extend(self.node(&n));
                    first = false;
                }
                rowan::NodeOrToken::Token(t) if t.kind().is_trivia() => {}
                rowan::NodeOrToken::Token(t) => out.extend(self.token(&t)),
            }
        }
        for c in std::mem::take(&mut self.tail) {
            out.push(Doc::Hard);
            if c.blank {
                out.push(Doc::BlankHint);
            }
            out.push(Doc::Text(c.text));
        }
        out
    }

    /// A token, with its comments and the space before it.
    fn token(&mut self, t: &SyntaxToken) -> Vec<Doc> {
        let mut out = Vec::new();
        let around = self.around.get(&offset(t)).cloned().unwrap_or_default();
        for c in &around.leading {
            out.push(Doc::Hard);
            if c.blank {
                out.push(Doc::BlankHint);
            }
            out.push(Doc::Text(c.text.clone()));
            out.push(Doc::Hard);
            self.tight = true;
        }
        // A blank line before the token (after its comments, if any).
        if around.blank {
            out.push(Doc::BlankHint);
        }
        if !self.tight
            && let Some(p) = &self.prev
            && space(p, t)
        {
            out.push(Doc::Space);
        }
        out.push(Doc::Text(t.text().to_owned()));
        self.tight = false;
        for c in &around.trailing {
            out.push(Doc::Space);
            out.push(Doc::Text(c.text.clone()));
            if c.line {
                out.push(Doc::Hard);
                self.tight = true;
            }
        }
        self.prev = Some(t.clone());
        out
    }

    /// A node: its own layout when it has one, else its children in a row.
    fn node(&mut self, n: &SyntaxNode) -> Vec<Doc> {
        use SyntaxKind::*;
        match n.kind() {
            BODY => self.body(n),
            FSTRINGEXPR => vec![Doc::Flat(self.flow(n))],
            BINARYEXPR => self.binary(n),
            // `if c { a } else { b }`: on one line, or every branch broken.
            IFEXPR if !else_if_chain(n) && n.parent().is_none_or(|p| p.kind() != IFEXPR) => {
                vec![Doc::Group(self.flow(n))]
            }
            _ => self.flow(n),
        }
    }

    /// `a op b`: on one line, or broken before the operator (the outermost operator first), indented.
    fn binary(&mut self, n: &SyntaxNode) -> Vec<Doc> {
        let mut docs = Vec::new();
        let mut seen_left = false;
        for el in n.children_with_tokens() {
            match el {
                rowan::NodeOrToken::Node(c) => {
                    docs.extend(self.node(&c));
                    seen_left = true;
                }
                rowan::NodeOrToken::Token(t) if t.kind().is_trivia() => {}
                rowan::NodeOrToken::Token(t) => {
                    // A range (`0..n`) stays tight; any other operator may start the next line.
                    if seen_left && !matches!(t.kind(), SyntaxKind::RANGE | SyntaxKind::RANGE_EQ) && space_before_op(&t)
                    {
                        docs.push(Doc::Line);
                        self.tight = true;
                    }
                    docs.extend(self.token(&t));
                }
            }
        }
        // The left operand stays where it is; what follows the first break is indented.
        let split = docs.iter().position(|d| matches!(d, Doc::Line)).unwrap_or(docs.len());
        let rest = docs.split_off(split);
        docs.push(Doc::Indent(INDENT, rest));
        vec![Doc::Group(docs)]
    }

    /// A rule's header (or condition): its literals and `where` guards, breaking after each comma, and before
    /// `where`, with a continuation indent when it does not fit.
    fn body(&mut self, n: &SyntaxNode) -> Vec<Doc> {
        // Comments before the header's first literal stand before the header, not at its continuation indent.
        let first = n
            .descendants_with_tokens()
            .filter_map(|e| e.into_token())
            .find(|t| !t.kind().is_trivia());
        let before = match first {
            Some(t) => self.leading_of(&t, true),
            None => Vec::new(),
        };
        let mut inner = Vec::new();
        let mut started = false;
        for el in n.children_with_tokens() {
            match el {
                rowan::NodeOrToken::Node(c) => {
                    inner.extend(self.node(&c));
                    started = true;
                }
                rowan::NodeOrToken::Token(t) if t.kind().is_trivia() => {}
                rowan::NodeOrToken::Token(t) if t.kind() == SyntaxKind::COMMA => {
                    inner.extend(self.token(&t));
                    inner.push(Doc::FillLine);
                    self.tight = true;
                }
                rowan::NodeOrToken::Token(t) if t.kind() == SyntaxKind::WHERE_KW && started => {
                    inner.push(Doc::Line);
                    self.tight = true;
                    inner.extend(self.token(&t));
                }
                rowan::NodeOrToken::Token(t) => {
                    inner.extend(self.token(&t));
                    started = true;
                }
            }
        }
        let mut out = before;
        if !out.is_empty() {
            out.push(Doc::Hard);
            self.tight = true;
        }
        out.push(Doc::Group(vec![Doc::Indent(CONTINUATION, inner)]));
        out
    }

    /// A node's children in a row; a bracket pair among its own tokens is laid out as a block or a group.
    fn flow(&mut self, n: &SyntaxNode) -> Vec<Doc> {
        let children: Vec<rowan::NodeOrToken<SyntaxNode, SyntaxToken>> = n
            .children_with_tokens()
            .filter(|e| e.as_token().is_none_or(|t| !t.kind().is_trivia()))
            .collect();
        let mut out = Vec::new();
        let mut i = 0;
        while let Some(el) = children.get(i) {
            match el {
                rowan::NodeOrToken::Node(c) => {
                    let attr = matches!(c.kind(), SyntaxKind::ATTR | SyntaxKind::INNERATTR);
                    out.extend(self.node(c));
                    if attr {
                        out.push(Doc::Hard);
                        self.tight = true;
                    }
                    i += 1;
                }
                rowan::NodeOrToken::Token(t) => {
                    // An interpolation hole's braces are the string's, printed as they are.
                    let hole = n.kind() == SyntaxKind::FSTRINGHOLE;
                    let close = closer(t.kind())
                        .filter(|_| !hole)
                        .and_then(|k| matching(&children, i, t.kind(), k));
                    match close {
                        Some(j) => {
                            let inside = children.get(i + 1..j).unwrap_or(&[]);
                            out.extend(self.bracket(n, inside, t, children.get(j)));
                            i = j + 1;
                        }
                        None => {
                            out.extend(self.token(t));
                            i += 1;
                        }
                    }
                }
            }
        }
        out
    }

    /// A bracket pair and what is inside it.
    fn bracket(
        &mut self,
        owner: &SyntaxNode,
        inside: &[rowan::NodeOrToken<SyntaxNode, SyntaxToken>],
        open: &SyntaxToken,
        close: Option<&rowan::NodeOrToken<SyntaxNode, SyntaxToken>>,
    ) -> Vec<Doc> {
        let Some(close) = close.and_then(|c| c.as_token()) else {
            return Vec::new();
        };
        let mut out = self.token(open);
        let kind = layout(owner, open.kind(), inside);
        if inside.is_empty() {
            self.tight = true;
            out.extend(self.token(close));
            return out;
        }
        match kind {
            Layout::Hug | Layout::Tight => {
                self.tight = true;
                out.extend(self.items(inside, Sep::Space));
                out.extend(self.leading_of(close, false));
                out.extend(self.token_bare(close));
            }
            Layout::Inline { spaced } => {
                let edge = if spaced { Doc::Line } else { Doc::SoftLine };
                let mut inner = vec![edge.clone()];
                self.tight = true;
                inner.extend(self.items(inside, Sep::Line));
                inner.extend(self.leading_of(close, false));
                out.push(Doc::Indent(INDENT, inner));
                out.push(edge);
                self.tight = true;
                out.extend(self.token_bare(close));
            }
            Layout::Block => {
                let mut body = vec![Doc::Hard];
                self.tight = true;
                body.extend(self.items(inside, Sep::Hard));
                let mut inner = body;
                // The closing bracket's own comments belong inside the block.
                inner.extend(self.leading_of(close, false));
                out.push(Doc::Indent(INDENT, inner));
                out.push(Doc::Hard);
                self.tight = true;
                out.extend(self.token_bare(close));
            }
            Layout::Group { spaced } => {
                let edge = if spaced { Doc::Line } else { Doc::SoftLine };
                let mut inner = vec![edge.clone()];
                self.tight = true;
                inner.extend(self.items(inside, Sep::Line));
                inner.extend(self.leading_of(close, false));
                let mut group = out;
                group.push(Doc::Indent(INDENT, inner));
                group.push(edge);
                self.tight = true;
                group.extend(self.token_bare(close));
                return vec![Doc::Group(group)];
            }
        }
        out
    }

    /// The comments on their own lines before a token, printed (and taken from it). With `blank`, the blank line
    /// before the token too (else it is dropped: none goes before a closing bracket).
    fn leading_of(&mut self, t: &SyntaxToken, blank: bool) -> Vec<Doc> {
        let mut out = Vec::new();
        if let Some(a) = self.around.get_mut(&offset(t)) {
            for c in std::mem::take(&mut a.leading) {
                out.push(Doc::Hard);
                if c.blank {
                    out.push(Doc::BlankHint);
                }
                out.push(Doc::Text(c.text));
            }
            if blank && a.blank {
                out.push(Doc::Hard);
                out.push(Doc::BlankHint);
            }
            a.blank = false;
        }
        out
    }

    /// A token without a space before it (its comments still printed).
    fn token_bare(&mut self, t: &SyntaxToken) -> Vec<Doc> {
        self.tight = true;
        self.token(t)
    }

    /// The entries inside a bracket pair: nodes and tokens, a break after each `,` or `;` (and between two nodes
    /// with no separator).
    fn items(&mut self, inside: &[rowan::NodeOrToken<SyntaxNode, SyntaxToken>], sep: Sep) -> Vec<Doc> {
        let mut out = Vec::new();
        let mut last_node = false;
        let mut i = 0;
        while let Some(el) = inside.get(i) {
            match el {
                rowan::NodeOrToken::Node(c) => {
                    if last_node && sep == Sep::Hard {
                        out.push(Doc::Hard);
                        self.tight = true;
                    }
                    out.extend(self.node(c));
                    last_node = true;
                    i += 1;
                }
                rowan::NodeOrToken::Token(t) => {
                    last_node = false;
                    // A nested bracket pair among the entries (`{ (a, b) }` written as tokens) keeps its layout.
                    let close = closer(t.kind()).and_then(|k| matching(inside, i, t.kind(), k));
                    if let Some(j) = close {
                        let owner = t.parent();
                        if let Some(owner) = owner {
                            let nested = inside.get(i + 1..j).unwrap_or(&[]);
                            out.extend(self.bracket(&owner, nested, t, inside.get(j)));
                        }
                        i = j + 1;
                        continue;
                    }
                    out.extend(self.token(t));
                    if matches!(t.kind(), SyntaxKind::COMMA | SyntaxKind::SEMI) && i + 1 < inside.len() {
                        match sep {
                            Sep::Hard => {
                                out.push(Doc::Hard);
                                self.tight = true;
                            }
                            Sep::Line => {
                                out.push(Doc::Line);
                                self.tight = true;
                            }
                            Sep::Space => {}
                        }
                    }
                    i += 1;
                }
            }
        }
        out
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Sep {
    Hard,
    Line,
    /// No break: the ordinary space after a separator.
    Space,
}

enum Layout {
    /// Each entry on its own line.
    Block,
    /// On one line if it fits; `spaced` puts spaces inside the brackets (`{ a }`).
    Group { spaced: bool },
    /// Breaks with the enclosing group (an `if`'s branches break together).
    Inline { spaced: bool },
    /// Tight around a single entry, which breaks inside itself (`f(|x| S { … })`, `Some("…")`).
    Hug,
    /// Never broken (a tuple type).
    Tight,
}

fn closer(k: SyntaxKind) -> Option<SyntaxKind> {
    use SyntaxKind::*;
    match k {
        L_PAREN => Some(R_PAREN),
        L_BRACK => Some(R_BRACK),
        L_CURLY => Some(R_CURLY),
        _ => None,
    }
}

/// The index of the token closing the bracket at `i`, among a node's own children.
fn matching(
    children: &[rowan::NodeOrToken<SyntaxNode, SyntaxToken>],
    i: usize,
    open: SyntaxKind,
    close: SyntaxKind,
) -> Option<usize> {
    let mut depth = 0usize;
    for (j, c) in children.iter().enumerate().skip(i) {
        if let Some(t) = c.as_token() {
            if t.kind() == open {
                depth += 1;
            } else if t.kind() == close {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(j);
                }
            }
        }
    }
    None
}

/// How a bracket pair owned by `owner` is laid out.
fn layout(owner: &SyntaxNode, open: SyntaxKind, inside: &[rowan::NodeOrToken<SyntaxNode, SyntaxToken>]) -> Layout {
    use SyntaxKind::*;
    let k = owner.kind();
    if matches!(k, TYPE | GENERICARGS | GENERICARG) {
        return Layout::Tight;
    }
    let commas = inside.iter().any(|c| c.as_token().is_some_and(|t| t.kind() == COMMA));
    let entries = inside.iter().filter(|c| c.as_node().is_some()).count();
    if open == L_PAREN && entries == 1 && !commas {
        return Layout::Hug;
    }
    if open != L_CURLY {
        return Layout::Group { spaced: false };
    }
    let lets = inside.iter().any(|c| c.as_node().is_some_and(|n| n.kind() == LETLIT));
    if k == BLOCKEXPR && !lets && owner.parent().is_some_and(|p| p.kind() == IFEXPR && !else_if_chain(&p)) {
        return Layout::Inline { spaced: true };
    }
    let block = match k {
        BLOCK | MATCHEXPR | VIEWDECL | TREEITEM | MODULEITEM | ATSECTION | BLOCKITEM | OVERRIDEITEM | STRUCTITEM
        | ENUMITEM | AGGREGATEITEM | LATTICETYPEITEM | EXTERNITEM | IMPLITEM | SERVICEITEM | PROTOCOLITEM
        | MIGRATEITEM | TRANSLATEITEM | FORMATITEM | SPECITEM | OPTBLOCK | INTERPOSEITEM | ACLITEM | BOOTSTRAPITEM
        | SOURCEFILE | INVARIANTITEM | SNAPSHOTITEM | CELLDECL => true,
        // A function's body, and the branches of an `if … else if …` chain; an expression's block (and a plain
        // `if c { a } else { b }`) stays on a line when it fits.
        BLOCKEXPR => {
            inside.iter().any(|c| c.as_node().is_some_and(|n| n.kind() == LETLIT))
                || owner.parent().is_some_and(|p| {
                    matches!(p.kind(), FNITEM | AGGMEMBER | IMPLITEM) || (p.kind() == IFEXPR && else_if_chain(&p))
                })
        }
        // A tree element's children: one element or content alone stays on its line when it fits.
        CHILDREN => {
            let nodes: Vec<SyntaxNode> = inside.iter().filter_map(|c| c.as_node().cloned()).collect();
            nodes.len() > 1 || nodes.iter().any(|n| !matches!(n.kind(), CONTENT | ELEMENT))
        }
        _ => {
            let name = format!("{k:?}");
            name.ends_with("ITEM") || name.ends_with("SECTION")
        }
    };
    if block {
        Layout::Block
    } else {
        Layout::Group { spaced: true }
    }
}

/// Whether an `if` expression is (part of) a chain with an `else if`.
fn else_if_chain(n: &SyntaxNode) -> bool {
    let mut top = n.clone();
    while let Some(p) = top.parent().filter(|p| p.kind() == SyntaxKind::IFEXPR) {
        top = p;
    }
    top.descendants().any(|d| d.kind() == SyntaxKind::IFEXPR && d != top)
        && top.children().any(|c| c.kind() == SyntaxKind::IFEXPR)
}

/// Whether a binary operator takes spaces around it (every one but a range's).
fn space_before_op(t: &SyntaxToken) -> bool {
    !matches!(
        t.kind(),
        SyntaxKind::RANGE | SyntaxKind::RANGE_EQ | SyntaxKind::OPEN_RANGE | SyntaxKind::OPEN_RANGE_EQ
    )
}

fn parent_kind(t: &SyntaxToken) -> Option<SyntaxKind> {
    t.parent().map(|p| p.kind())
}

/// Whether a closure's `|` is its first (the parameters open).
fn opens_closure(t: &SyntaxToken) -> bool {
    t.parent().is_some_and(|p| {
        p.kind() == SyntaxKind::CLOSUREEXPR
            && p.children_with_tokens()
                .filter_map(|e| e.into_token())
                .find(|x| x.kind() == SyntaxKind::PIPE)
                .is_some_and(|first| first == *t)
    })
}

/// Whether a space goes between two tokens printed side by side.
fn space(p: &SyntaxToken, n: &SyntaxToken) -> bool {
    use SyntaxKind::*;
    let (pk, nk) = (p.kind(), n.kind());
    let (pp, np) = (parent_kind(p), parent_kind(n));
    let in_names = |k: Option<SyntaxKind>| matches!(k, Some(ELEMNAME | PROPNAME));
    // An interpolated string is printed as written; inside a hole, `{x}` and `{x:.2}` are tight.
    if matches!(pk, FSTRING_START | FSTRING_TEXT | FSTRING_SPEC)
        || matches!(nk, FSTRING_TEXT | FSTRING_SPEC | FSTRING_END)
    {
        return false;
    }
    if (pk == L_CURLY && pp == Some(FSTRINGHOLE))
        || (nk == R_CURLY && np == Some(FSTRINGHOLE))
        || ((pk == COLON || nk == COLON) && (pp == Some(FSTRINGHOLE) || np == Some(FSTRINGHOLE)))
        || (nk == L_CURLY && np == Some(FSTRINGHOLE))
        || (pk == R_CURLY && pp == Some(FSTRINGHOLE))
    {
        return false;
    }
    // `stroke-width`, `font-face`, `a.b`: one name.
    if (in_names(np) && matches!(nk, MINUS | DOT)) || (in_names(pp) && matches!(pk, MINUS | DOT)) {
        return false;
    }
    // Generic arguments and parameters: `Vec<u64>`, `Map<K, Vec<V>>`.
    let generic = |k: Option<SyntaxKind>| matches!(k, Some(GENERICARGS | GENERICS | GENERICPARAM | GENERICARG));
    if (matches!(nk, LT | GT | SHR) && generic(np)) || (pk == LT && generic(pp)) {
        return false;
    }
    if matches!(nk, R_PAREN | R_BRACK | COMMA | SEMI | DOT | COLON2 | COLON | QUESTION) {
        return false;
    }
    if matches!(pk, L_PAREN | L_BRACK | DOT | COLON2 | ATTR_START | INNER_ATTR) {
        return false;
    }
    // Calls, index and element meta: `f(x)`, `r[k]`, `li[key: n]`, `input[…]` (a keyword as an element's name).
    if matches!(nk, L_PAREN | L_BRACK)
        && (matches!(pk, IDENT | BANG_IDENT | R_PAREN | R_BRACK | SELF_KW) || pp == Some(ELEMNAME))
    {
        return false;
    }
    if nk == L_PAREN && matches!(pk, GT | SHR) && generic(pp) {
        return false;
    }
    // Prefix operators: `-x`, `~m`.
    if matches!(pk, MINUS | TILDE) && pp == Some(PREFIXEXPR) {
        return false;
    }
    // Ranges `0..n`, and `..base`, `..{…}`, `..m` in arguments and struct literals.
    if matches!(pk, RANGE | RANGE_EQ | OPEN_RANGE | OPEN_RANGE_EQ) || matches!(nk, RANGE | RANGE_EQ) {
        return !(matches!(pp, Some(BINARYEXPR | ARG | FIELDINIT) | None) || matches!(np, Some(BINARYEXPR)));
    }
    // Closures: `|a, b| body`.
    if pk == PIPE && opens_closure(p) {
        return false;
    }
    if nk == PIPE && np == Some(CLOSUREEXPR) && !opens_closure(n) {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt(s: &str) -> String {
        format(s).unwrap_or_else(|e| panic!("{e}: {s}"))
    }

    #[test]
    fn spacing_breaking_and_comments() {
        let src = "program p version 1;\n\
                   // the input\n\
                   input go(k:u64,v :u64);# a hash comment\n\
                   table t(k:u64,name:String)key(k);\n\n\n\
                   a:on go(k,v),t(k,n)where k>1&&n!=\"x\"{emit o(-k,f\"{k}:{v:.2}\");if v>2{upsert t(k,n);}}\n";
        let out = fmt(src);
        assert_eq!(
            out,
            "program p version 1;\n\
             // the input\n\
             input go(k: u64, v: u64); // a hash comment\n\
             table t(k: u64, name: String) key(k);\n\n\
             a: on go(k, v), t(k, n) where k > 1 && n != \"x\" {\n    \
                 emit o(-k, f\"{k}:{v:.2}\");\n    \
                 if v > 2 {\n        \
                     upsert t(k, n);\n    \
                 }\n\
             }\n"
        );
        assert_eq!(fmt(&out), out, "not idempotent");
    }

    #[test]
    fn long_headers_fill_after_commas_and_put_where_on_its_line() {
        let src = "program p version 1;\nsave: on keydown(id, key, value), editing(number_of_the_todo), todos(number_of_the_todo, title, done) where id == f\"edit-{number_of_the_todo}\" && key == \"Enter\" { emit o(1); }\n";
        assert_eq!(
            fmt(src),
            "program p version 1;\n\
             save: on keydown(id, key, value), editing(number_of_the_todo), todos(number_of_the_todo, title, done)\n        \
                 where id == f\"edit-{number_of_the_todo}\" && key == \"Enter\" {\n    \
                 emit o(1);\n\
             }\n"
        );
        // Literals that do not fit on one line fill the next.
        let long = "program p version 1;\nh: on aaaaaaaaaaaaaaaaaaaa(x), bbbbbbbbbbbbbbbbbbbbbbbbbbbbbb(x), cccccccccccccccccccccccccccccc(x), dddddddddddddddddddddddddd(x), eeeeeeee(x) { emit o(x); }\n";
        assert_eq!(
            fmt(long),
            "program p version 1;\n\
             h: on aaaaaaaaaaaaaaaaaaaa(x), bbbbbbbbbbbbbbbbbbbbbbbbbbbbbb(x), cccccccccccccccccccccccccccccc(x),\n        \
                 dddddddddddddddddddddddddd(x), eeeeeeee(x) {\n    \
                 emit o(x);\n\
             }\n"
        );
    }

    #[test]
    fn comments_blank_lines_and_the_shebang_keep_their_places() {
        let src = "#!/usr/bin/env blossom\n//! A program.\n\nprogram p version 1;\n\n\n/// A view.\nview v(x) {\n  // the first way\n  a(x);\n\n  b(x); // the second\n}\nh: on go(k) {\n    emit o(k);   // last\n}\n# trailing\n";
        let out = fmt(src);
        assert_eq!(
            out,
            "#!/usr/bin/env blossom\n//! A program.\n\nprogram p version 1;\n\n/// A view.\nview v(x) {\n    \
             // the first way\n    a(x);\n\n    b(x); // the second\n}\nh: on go(k) {\n    emit o(k); // last\n}\n// trailing\n"
        );
        assert_eq!(fmt(&out), out);
    }

    #[test]
    fn expressions_hug_break_at_operators_and_chains_break() {
        let src = "program p version 1;\nfn f(h: i64, b: Vec<u64>) -> i64 { if h < 0 { 0 - h } else if h == 0 { 1 } else { h } }\n\
                   fn g(x: u64) -> u64 { if x > 1u64 { x } else { 1u64 } }\n\
                   fn s(x: u64) -> Vec<u64> { [x].map(|a| aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa(a, bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb, ccccccccccccccccccccccccccccccccccc)) }\n\
                   fn t(x: u64) -> u64 { xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx * 2u64 + yyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyy * 3u64 + zzzzzzzzzzzzzzzzzzzzzzzzzzzzz * 4u64 }\n";
        let out = fmt(src);
        assert_eq!(
            out,
            "program p version 1;\n\
             fn f(h: i64, b: Vec<u64>) -> i64 {\n    \
                 if h < 0 {\n        0 - h\n    } else if h == 0 {\n        1\n    } else {\n        h\n    }\n\
             }\n\
             fn g(x: u64) -> u64 {\n    if x > 1u64 { x } else { 1u64 }\n}\n\
             fn s(x: u64) -> Vec<u64> {\n    \
                 [x].map(|a| aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa(\n        a,\n        bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb,\n        \
                 ccccccccccccccccccccccccccccccccccc\n    ))\n\
             }\n\
             fn t(x: u64) -> u64 {\n    \
                 xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx * 2u64 + yyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyy * 3u64\n        \
                 + zzzzzzzzzzzzzzzzzzzzzzzzzzzzz * 4u64\n\
             }\n"
        );
        assert_eq!(fmt(&out), out);
    }
}
