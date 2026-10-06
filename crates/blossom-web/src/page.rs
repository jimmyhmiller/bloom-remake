//! The page a program describes (BROWSER.md "The page"): its `elem`, `attr`, `text` and `focus` outputs at the end of
//! a round, and the patches that turn one page into the next.
//!
//! Elements are keyed by id: an element whose id and tag stay keeps its DOM node across rounds (and with it its
//! focus, caret and scroll position); one whose tag changes is replaced. A parent's children are ordered by `pos`,
//! then `id`. An `svg` element and its descendants (but those of a `foreignObject`) are SVG elements: the DOM makes
//! them in the SVG namespace, so an element that moves in or out of an `svg` is replaced too.
//!
//! The page is kept incrementally: each round hands it the rows its outputs gained and lost ([`Page::apply`]), it
//! re-checks only what those rows touch, and [`Page::take_patches`] turns what changed since the last call into
//! patches. They are exactly the patches a comparison of the whole page before and after would give, in the same
//! order (the tests check it against that comparison).

use std::collections::{BTreeMap, BTreeSet};

use blossom_ir::tick::Row;
use blossom_value::Value;
use blossom_value::value::IntValue;
use serde::Serialize;

use crate::HostError;

/// One element: its parent (`""`: the mount point), its position among its siblings, its tag.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Elem {
    parent: String,
    pos: i64,
    tag: String,
    /// In the SVG namespace (set once the page's elements are known).
    svg: bool,
}

/// An `elem` row's columns after the id: parent, position, tag.
type ElemRow = (String, i64, String);

/// The rows of a page's four outputs that a round added and removed.
#[derive(Clone, Debug, Default)]
pub struct Delta {
    pub elem: (Vec<Row>, Vec<Row>),
    pub attr: (Vec<Row>, Vec<Row>),
    pub text: (Vec<Row>, Vec<Row>),
    pub focus: (Vec<Row>, Vec<Row>),
}

/// What changed since the last [`Page::take_patches`]: each touched entry's value before its first change.
#[derive(Clone, Debug, Default)]
struct Journal {
    elems: BTreeMap<String, Option<Elem>>,
    attrs: BTreeMap<(String, String), Option<String>>,
    texts: BTreeMap<String, Option<String>>,
    /// A parent's children, in order (`None`: it had none).
    kids: BTreeMap<String, Option<Vec<String>>>,
    focus: BTreeMap<String, bool>,
}

/// A page: what the program's outputs hold at the end of a round.
#[derive(Clone, Debug, Default)]
pub struct Page {
    elems: BTreeMap<String, Elem>,
    attrs: BTreeMap<(String, String), String>,
    texts: BTreeMap<String, String>,
    focus: BTreeSet<String>,
    /// The rows the outputs hold, by id: a page is well formed when each holds at most one.
    elem_rows: BTreeMap<String, BTreeSet<ElemRow>>,
    attr_rows: BTreeMap<(String, String), BTreeSet<String>>,
    text_rows: BTreeMap<String, BTreeSet<String>>,
    /// Each parent's children, by position then id (a parent with none has no entry).
    kids: BTreeMap<String, BTreeSet<(i64, String)>>,
    /// What a round left ill formed, checked again in the next (as a whole-page check would).
    recheck_elems: BTreeSet<String>,
    recheck_attrs: BTreeSet<(String, String)>,
    recheck_texts: BTreeSet<String>,
    journal: Journal,
}

/// Two pages are equal when they show the same thing.
impl PartialEq for Page {
    fn eq(&self, other: &Page) -> bool {
        self.elems == other.elems && self.attrs == other.attrs && self.texts == other.texts && self.focus == other.focus
    }
}

impl Eq for Page {}

/// A change to the DOM, in the order a host applies them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "op", rename_all = "lowercase")]
pub enum Patch {
    /// Make a detached element `tag` with identity `id` (in the SVG namespace when `svg`).
    Create { id: String, tag: String, svg: bool },
    /// Drop the element `id` (its children that stay on the page are placed again by a [`Patch::Children`]).
    Remove { id: String },
    /// Set an attribute (or, for `value`, `checked` and `disabled`, the property).
    Attr { id: String, name: String, value: String },
    /// Remove an attribute (or clear the property).
    Unattr { id: String, name: String },
    /// Set the text before the element's children (`""`: none).
    Text { id: String, text: String },
    /// The element `parent`'s children (`""`: the mount point's), in order.
    Children { parent: String, ids: Vec<String> },
    /// Focus the element.
    Focus { id: String },
}

fn string(v: &Value, what: &str) -> Result<String, HostError> {
    match v {
        Value::Str(s) => Ok(s.to_string()),
        other => Err(HostError::Page(format!("{what} is {other:?}, not a String"))),
    }
}

fn columns<'r, const N: usize>(row: &'r Row, rel: &str) -> Result<&'r [Value; N], HostError> {
    <&[Value; N]>::try_from(&row[..])
        .map_err(|_| HostError::Page(format!("a row of `{rel}` with {} columns, not {N}", row.len())))
}

fn elem_row(row: &Row) -> Result<(String, ElemRow), HostError> {
    let [id, parent, pos, tag] = columns::<4>(row, "elem")?;
    let id = string(id, "an element's id")?;
    let pos = match pos {
        Value::Int(IntValue::I64(p)) => *p,
        other => {
            return Err(HostError::Page(format!(
                "element `{id}`'s position is {other:?}, not an i64"
            )));
        }
    };
    let parent = string(parent, "an element's parent")?;
    let tag = string(tag, "an element's tag")?;
    Ok((id, (parent, pos, tag)))
}

fn attr_row(row: &Row) -> Result<((String, String), String), HostError> {
    let [id, name, value] = columns::<3>(row, "attr")?;
    let key = (
        string(id, "an attribute's element")?,
        string(name, "an attribute's name")?,
    );
    Ok((key, string(value, "an attribute's value")?))
}

fn text_row(row: &Row) -> Result<(String, String), HostError> {
    let [id, s] = columns::<2>(row, "text")?;
    Ok((string(id, "a text's element")?, string(s, "a text")?))
}

fn focus_row(row: &Row) -> Result<String, HostError> {
    let [id] = columns::<1>(row, "focus")?;
    string(id, "a focused element")
}

/// Adds or removes `row` from the set under `key`, dropping an empty set.
fn edit<K: Ord + Clone, V: Ord>(map: &mut BTreeMap<K, BTreeSet<V>>, key: &K, value: V, add: bool) {
    if add {
        map.entry(key.clone()).or_default().insert(value);
    } else if let Some(set) = map.get_mut(key) {
        set.remove(&value);
        if set.is_empty() {
            map.remove(key);
        }
    }
}

impl Page {
    /// Applies a round's changes to the outputs. The page must be well formed afterwards: an element given twice
    /// (two rows with one id), under a parent that is not on the page, or its own ancestor, and an attribute or text
    /// given twice, are program errors. An error leaves the rows applied and the ill-formed parts as they were; the
    /// next round checks them again.
    pub fn apply(&mut self, d: &Delta) -> Result<(), HostError> {
        let mut elems: BTreeSet<String> = std::mem::take(&mut self.recheck_elems);
        let mut attrs: BTreeSet<(String, String)> = std::mem::take(&mut self.recheck_attrs);
        let mut texts: BTreeSet<String> = std::mem::take(&mut self.recheck_texts);
        for (rows, add) in [(&d.elem.1, false), (&d.elem.0, true)] {
            for row in rows {
                let (id, r) = elem_row(row)?;
                edit(&mut self.elem_rows, &id, r, add);
                elems.insert(id);
            }
        }
        for (rows, add) in [(&d.attr.1, false), (&d.attr.0, true)] {
            for row in rows {
                let (key, v) = attr_row(row)?;
                edit(&mut self.attr_rows, &key, v, add);
                attrs.insert(key);
            }
        }
        for (rows, add) in [(&d.text.1, false), (&d.text.0, true)] {
            for row in rows {
                let (id, v) = text_row(row)?;
                edit(&mut self.text_rows, &id, v, add);
                texts.insert(id);
            }
        }
        for (rows, add) in [(&d.focus.1, false), (&d.focus.0, true)] {
            for row in rows {
                let id = focus_row(row)?;
                self.journal
                    .focus
                    .entry(id.clone())
                    .or_insert_with(|| self.focus.contains(&id));
                if add {
                    self.focus.insert(id);
                } else {
                    self.focus.remove(&id);
                }
            }
        }
        let mut first: Option<HostError> = None;
        let fail = |e: HostError, first: &mut Option<HostError>| {
            if first.is_none() {
                *first = Some(e);
            }
        };
        // Elements: one row each, or none.
        let mut changed = Vec::new();
        for id in &elems {
            let rows = self.elem_rows.get(id);
            let next = match rows.map(|r| r.len()).unwrap_or(0) {
                0 => None,
                1 => rows.and_then(|r| r.first()).map(|(parent, pos, tag)| Elem {
                    parent: parent.clone(),
                    pos: *pos,
                    tag: tag.clone(),
                    svg: self.elems.get(id).is_some_and(|e| e.svg),
                }),
                _ => {
                    let mut two = rows.into_iter().flatten().map(|(parent, pos, tag)| Elem {
                        parent: parent.clone(),
                        pos: *pos,
                        tag: tag.clone(),
                        svg: false,
                    });
                    let (old, e) = (two.next(), two.next());
                    fail(
                        HostError::Page(format!("element `{id}` is given twice ({old:?} and {e:?})")),
                        &mut first,
                    );
                    self.recheck_elems.insert(id.clone());
                    continue;
                }
            };
            if self.elems.get(id) != next.as_ref() {
                self.set_elem(id, next);
            }
            changed.push(id.clone());
        }
        // Parents: every element's is on the page, so the children of one that went must have gone too.
        let mut orphans = BTreeSet::new();
        for id in &changed {
            match self.elems.get(id) {
                Some(e) if !e.parent.is_empty() && !self.elems.contains_key(&e.parent) => {
                    orphans.insert((id.clone(), e.parent.clone()));
                }
                Some(_) => {}
                None => {
                    for (_, kid) in self.kids.get(id).into_iter().flatten() {
                        orphans.insert((kid.clone(), id.clone()));
                    }
                }
            }
        }
        if !orphans.is_empty() {
            // The namespaces below are not worked out this round: every changed element is checked again.
            self.recheck_elems.extend(changed.iter().cloned());
        }
        for (id, parent) in &orphans {
            fail(
                HostError::Page(format!("element `{id}`'s parent `{parent}` is not on the page")),
                &mut first,
            );
            self.recheck_elems.insert(id.clone());
        }
        // Namespaces (and cycles): an element whose tag or parent changed decides its descendants' too.
        if orphans.is_empty() {
            let mut todo: Vec<String> = changed
                .iter()
                .filter(|id| self.elems.contains_key(*id))
                .cloned()
                .collect();
            let mut seen = BTreeSet::new();
            while let Some(id) = todo.pop() {
                if !seen.insert(id.clone()) {
                    continue;
                }
                match self.in_svg(&id) {
                    Ok(svg) => {
                        if let Some(e) = self.elems.get(&id)
                            && e.svg != svg
                        {
                            let mut e = e.clone();
                            e.svg = svg;
                            self.set_elem(&id, Some(e));
                        }
                    }
                    Err(e) => {
                        fail(e, &mut first);
                        self.recheck_elems.insert(id.clone());
                        continue;
                    }
                }
                todo.extend(self.kids.get(&id).into_iter().flatten().map(|(_, k)| k.clone()));
            }
        }
        // Attributes and texts: one value each, or none.
        for key in attrs {
            let rows = self.attr_rows.get(&key);
            let next = match rows.map(|r| r.len()).unwrap_or(0) {
                0 => None,
                1 => rows.and_then(|r| r.first()).cloned(),
                _ => {
                    let mut two = rows.into_iter().flatten();
                    let (old, value) = (
                        two.next().cloned().unwrap_or_default(),
                        two.next().cloned().unwrap_or_default(),
                    );
                    fail(
                        HostError::Page(format!(
                            "attribute `{}` of `{}` is given twice (`{old}` and `{value}`)",
                            key.1, key.0
                        )),
                        &mut first,
                    );
                    self.recheck_attrs.insert(key);
                    continue;
                }
            };
            if self.attrs.get(&key) != next.as_ref() {
                self.journal
                    .attrs
                    .entry(key.clone())
                    .or_insert_with(|| self.attrs.get(&key).cloned());
                match next {
                    Some(v) => self.attrs.insert(key, v),
                    None => self.attrs.remove(&key),
                };
            }
        }
        for id in texts {
            let rows = self.text_rows.get(&id);
            let next = match rows.map(|r| r.len()).unwrap_or(0) {
                0 => None,
                1 => rows.and_then(|r| r.first()).cloned(),
                _ => {
                    let mut two = rows.into_iter().flatten();
                    let (old, s) = (
                        two.next().cloned().unwrap_or_default(),
                        two.next().cloned().unwrap_or_default(),
                    );
                    fail(
                        HostError::Page(format!("the text of `{id}` is given twice (`{old}` and `{s}`)")),
                        &mut first,
                    );
                    self.recheck_texts.insert(id);
                    continue;
                }
            };
            if self.texts.get(&id) != next.as_ref() {
                self.journal
                    .texts
                    .entry(id.clone())
                    .or_insert_with(|| self.texts.get(&id).cloned());
                match next {
                    Some(v) => self.texts.insert(id, v),
                    None => self.texts.remove(&id),
                };
            }
        }
        match first {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// Replaces element `id` (journaling what it was, and its old and new parents' children).
    fn set_elem(&mut self, id: &str, next: Option<Elem>) {
        let old = self.elems.get(id).cloned();
        self.journal.elems.entry(id.to_owned()).or_insert_with(|| old.clone());
        let moved = old.as_ref().map(|e| (&e.parent, e.pos)) != next.as_ref().map(|e| (&e.parent, e.pos));
        if moved {
            for parent in old
                .iter()
                .map(|e| e.parent.clone())
                .chain(next.iter().map(|e| e.parent.clone()))
            {
                if !self.journal.kids.contains_key(&parent) {
                    let list = self.kid_list(&parent);
                    self.journal.kids.insert(parent, list);
                }
            }
            if let Some(e) = &old {
                edit(&mut self.kids, &e.parent, (e.pos, id.to_owned()), false);
            }
            if let Some(e) = &next {
                edit(&mut self.kids, &e.parent, (e.pos, id.to_owned()), true);
            }
        }
        match next {
            Some(e) => self.elems.insert(id.to_owned(), e),
            None => self.elems.remove(id),
        };
    }

    /// `parent`'s children in order (`None`: it has none).
    fn kid_list(&self, parent: &str) -> Option<Vec<String>> {
        self.kids
            .get(parent)
            .map(|kids| kids.iter().map(|(_, id)| id.clone()).collect())
    }

    /// Whether element `id` is in the SVG namespace: the nearest `svg` or `foreignObject` above it (itself included,
    /// for an `svg`) decides. An element that is its own ancestor is an error.
    fn in_svg(&self, id: &str) -> Result<bool, HostError> {
        let mut seen = BTreeSet::new();
        let mut at = id;
        loop {
            if !seen.insert(at) {
                return Err(HostError::Page(format!("element `{at}` is its own ancestor")));
            }
            let Some(e) = self.elems.get(at) else { return Ok(false) };
            match e.tag.as_str() {
                "svg" => return Ok(true),
                "foreignObject" if at != id => return Ok(false),
                _ => {}
            }
            if e.parent.is_empty() {
                return Ok(false);
            }
            at = e.parent.as_str();
        }
    }

    /// The patches that turn the page of the last call (or the empty page) into this one; exactly those
    /// [`Page::diff`] gives for the two pages, in the same order.
    pub fn take_patches(&mut self) -> Vec<Patch> {
        let j = std::mem::take(&mut self.journal);
        let old_elem = |id: &str| -> Option<&Elem> {
            match j.elems.get(id) {
                Some(old) => old.as_ref(),
                None => self.elems.get(id),
            }
        };
        let kept = |id: &str| matches!((old_elem(id), self.elems.get(id)), (Some(a), Some(b)) if a.tag == b.tag && a.svg == b.svg);
        let mut out = Vec::new();
        for (id, old) in &j.elems {
            if old.is_some() && !kept(id) {
                out.push(Patch::Remove { id: id.clone() });
            }
        }
        let mut fresh = BTreeSet::new();
        for id in j.elems.keys() {
            if let Some(e) = self.elems.get(id)
                && !kept(id)
            {
                out.push(Patch::Create {
                    id: id.clone(),
                    tag: e.tag.clone(),
                    svg: e.svg,
                });
                fresh.insert(id.clone());
            }
        }
        // Attributes: a new element's all, a kept one's changes.
        let mut keys: BTreeSet<(String, String)> = j.attrs.keys().cloned().collect();
        for id in &fresh {
            keys.extend(
                self.attrs
                    .range((id.clone(), String::new())..)
                    .take_while(|((i, _), _)| i == id)
                    .map(|(k, _)| k.clone()),
            );
        }
        let old_attr = |key: &(String, String)| -> Option<&String> {
            match j.attrs.get(key) {
                Some(old) => old.as_ref(),
                None => self.attrs.get(key),
            }
        };
        for key in &keys {
            let Some(value) = self.attrs.get(key) else { continue };
            if !self.elems.contains_key(&key.0) {
                continue;
            }
            let old = if kept(&key.0) { old_attr(key) } else { None };
            if old != Some(value) {
                out.push(Patch::Attr {
                    id: key.0.clone(),
                    name: key.1.clone(),
                    value: value.clone(),
                });
            }
        }
        for (key, old) in &j.attrs {
            if old.is_some() && kept(&key.0) && !self.attrs.contains_key(key) {
                out.push(Patch::Unattr {
                    id: key.0.clone(),
                    name: key.1.clone(),
                });
            }
        }
        // Texts.
        let ids: BTreeSet<&String> = j.texts.keys().chain(fresh.iter()).collect();
        for id in ids {
            if !self.elems.contains_key(id) {
                continue;
            }
            let new = self.texts.get(id).map_or("", String::as_str);
            let old = if kept(id) {
                match j.texts.get(id) {
                    Some(old) => old.as_deref().unwrap_or(""),
                    None => new,
                }
            } else {
                ""
            };
            if new != old {
                out.push(Patch::Text {
                    id: id.clone(),
                    text: new.to_owned(),
                });
            }
        }
        // Children: every parent whose list changed, or that is new (its children were placed in the old one).
        let parents: BTreeSet<&String> = j.kids.keys().chain(fresh.iter()).collect();
        let old_kids = |p: &str| -> Option<Vec<String>> {
            match j.kids.get(p) {
                Some(old) => old.clone(),
                None => self.kid_list(p),
            }
        };
        let mut gone = Vec::new();
        for parent in parents {
            match self.kid_list(parent) {
                Some(kids) => {
                    let fresh = !parent.is_empty() && !kept(parent);
                    if fresh || old_kids(parent).as_ref() != Some(&kids) {
                        out.push(Patch::Children {
                            parent: parent.clone(),
                            ids: kids,
                        });
                    }
                }
                None => {
                    if old_kids(parent).is_some() && (parent.is_empty() || kept(parent)) {
                        gone.push(parent.clone());
                    }
                }
            }
        }
        out.extend(gone.into_iter().map(|parent| Patch::Children {
            parent,
            ids: Vec::new(),
        }));
        // Focus what the program newly asks to focus.
        for (id, was) in &j.focus {
            if !was && self.focus.contains(id) && self.elems.contains_key(id) {
                out.push(Patch::Focus { id: id.clone() });
            }
        }
        out
    }

    /// The page the outputs' rows describe (a fresh page with them all added).
    #[cfg(test)]
    pub fn of(elem: &[Row], attr: &[Row], text: &[Row], focus: &[Row]) -> Result<Page, HostError> {
        let mut page = Page::default();
        page.apply(&Delta {
            elem: (elem.to_vec(), Vec::new()),
            attr: (attr.to_vec(), Vec::new()),
            text: (text.to_vec(), Vec::new()),
            focus: (focus.to_vec(), Vec::new()),
        })?;
        page.journal = Journal::default();
        Ok(page)
    }

    /// Each parent's children, in order.
    #[cfg(test)]
    fn children(&self) -> BTreeMap<&str, Vec<&str>> {
        let mut by_parent: BTreeMap<&str, Vec<(i64, &str)>> = BTreeMap::new();
        for (id, e) in &self.elems {
            by_parent
                .entry(e.parent.as_str())
                .or_default()
                .push((e.pos, id.as_str()));
        }
        by_parent
            .into_iter()
            .map(|(p, mut kids)| {
                kids.sort();
                (p, kids.into_iter().map(|(_, id)| id).collect())
            })
            .collect()
    }

    /// The patches that turn this page into `next`, by comparing the whole of both: the reference
    /// [`Page::take_patches`] is checked against.
    #[cfg(test)]
    pub fn diff(&self, next: &Page) -> Vec<Patch> {
        let mut out = Vec::new();
        let kept = |id: &str| matches!((self.elems.get(id), next.elems.get(id)), (Some(a), Some(b)) if a.tag == b.tag && a.svg == b.svg);
        // Elements gone, or replaced (another tag), then elements new.
        for id in self.elems.keys() {
            if !kept(id) {
                out.push(Patch::Remove { id: id.clone() });
            }
        }
        for (id, e) in &next.elems {
            if !kept(id) {
                out.push(Patch::Create {
                    id: id.clone(),
                    tag: e.tag.clone(),
                    svg: e.svg,
                });
            }
        }
        // Attributes and texts: a new element's all, a kept one's changes.
        for ((id, name), value) in &next.attrs {
            if !next.elems.contains_key(id) {
                continue;
            }
            let old = if kept(id) {
                self.attrs.get(&(id.clone(), name.clone()))
            } else {
                None
            };
            if old != Some(value) {
                out.push(Patch::Attr {
                    id: id.clone(),
                    name: name.clone(),
                    value: value.clone(),
                });
            }
        }
        for (id, name) in self.attrs.keys() {
            if kept(id) && !next.attrs.contains_key(&(id.clone(), name.clone())) {
                out.push(Patch::Unattr {
                    id: id.clone(),
                    name: name.clone(),
                });
            }
        }
        for id in next.elems.keys() {
            let new = next.texts.get(id).map_or("", String::as_str);
            let old = if kept(id) {
                self.texts.get(id).map_or("", String::as_str)
            } else {
                ""
            };
            if new != old {
                out.push(Patch::Text {
                    id: id.clone(),
                    text: new.to_owned(),
                });
            }
        }
        // Children: every parent whose list changed, or that is new (its children were placed in the old one).
        let (before, after) = (self.children(), next.children());
        for (parent, kids) in &after {
            let fresh = !parent.is_empty() && !kept(parent);
            if fresh || before.get(parent) != Some(kids) {
                out.push(Patch::Children {
                    parent: (*parent).to_owned(),
                    ids: kids.iter().map(|k| (*k).to_owned()).collect(),
                });
            }
        }
        for parent in before.keys() {
            if !after.contains_key(parent) && (parent.is_empty() || kept(parent)) {
                out.push(Patch::Children {
                    parent: (*parent).to_owned(),
                    ids: Vec::new(),
                });
            }
        }
        // Focus what the program newly asks to focus.
        for id in next.focus.difference(&self.focus) {
            if next.elems.contains_key(id) {
                out.push(Patch::Focus { id: id.clone() });
            }
        }
        out
    }

    /// The ids of the elements on the page.
    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.elems.keys().map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn s(x: &str) -> Value {
        Value::Str(x.into())
    }

    fn elem(id: &str, parent: &str, pos: i64, tag: &str) -> Row {
        Arc::from(vec![s(id), s(parent), Value::Int(IntValue::I64(pos)), s(tag)])
    }

    fn attr(id: &str, name: &str, value: &str) -> Row {
        Arc::from(vec![s(id), s(name), s(value)])
    }

    fn page(elems: &[Row], attrs: &[Row], focus: &[&str]) -> Page {
        let focus: Vec<Row> = focus.iter().map(|f| Arc::from(vec![s(f)])).collect();
        Page::of(elems, attrs, &[], &focus).unwrap()
    }

    fn ids(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|x| (*x).to_owned()).collect()
    }

    #[test]
    fn children_are_ordered_by_position_and_moves_reorder_without_recreating() {
        let a = page(
            &[
                elem("ul", "", 0, "ul"),
                elem("x", "ul", 1, "li"),
                elem("y", "ul", 2, "li"),
            ],
            &[],
            &[],
        );
        let b = page(
            &[
                elem("ul", "", 0, "ul"),
                elem("x", "ul", 3, "li"),
                elem("y", "ul", 2, "li"),
            ],
            &[],
            &[],
        );
        assert_eq!(
            a.diff(&b),
            [Patch::Children {
                parent: "ul".to_owned(),
                ids: ids(&["y", "x"])
            }]
        );
        // Equal positions order by id.
        let c = page(
            &[
                elem("ul", "", 0, "ul"),
                elem("b", "ul", 0, "li"),
                elem("a", "ul", 0, "li"),
            ],
            &[],
            &[],
        );
        assert!(Page::default().diff(&c).contains(&Patch::Children {
            parent: "ul".to_owned(),
            ids: ids(&["a", "b"])
        }));
    }

    #[test]
    fn a_new_tag_replaces_the_element_and_its_children_are_placed_again() {
        let a = page(
            &[elem("box", "", 0, "div"), elem("t", "box", 0, "span")],
            &[attr("box", "class", "x")],
            &[],
        );
        let b = page(
            &[elem("box", "", 0, "section"), elem("t", "box", 0, "span")],
            &[attr("box", "class", "x")],
            &[],
        );
        let d = a.diff(&b);
        assert_eq!(
            d,
            [
                Patch::Remove { id: "box".to_owned() },
                Patch::Create {
                    id: "box".to_owned(),
                    tag: "section".to_owned(),
                    svg: false,
                },
                Patch::Attr {
                    id: "box".to_owned(),
                    name: "class".to_owned(),
                    value: "x".to_owned()
                },
                Patch::Children {
                    parent: "box".to_owned(),
                    ids: ids(&["t"])
                },
            ]
        );
    }

    #[test]
    fn attributes_set_change_and_go_and_focus_is_asked_once() {
        let a = page(
            &[elem("i", "", 0, "input")],
            &[attr("i", "value", "a"), attr("i", "class", "c")],
            &[],
        );
        let b = page(&[elem("i", "", 0, "input")], &[attr("i", "value", "b")], &["i"]);
        assert_eq!(
            a.diff(&b),
            [
                Patch::Attr {
                    id: "i".to_owned(),
                    name: "value".to_owned(),
                    value: "b".to_owned()
                },
                Patch::Unattr {
                    id: "i".to_owned(),
                    name: "class".to_owned()
                },
                Patch::Focus { id: "i".to_owned() },
            ]
        );
        // Still focused: not asked again.
        assert_eq!(b.diff(&b), []);
    }

    #[test]
    fn svg_elements_are_made_in_their_namespace_and_replaced_when_they_leave_it() {
        let in_svg = |parent: &str| {
            page(
                &[
                    elem("game", "", 0, "svg"),
                    elem("box", "", 1, "div"),
                    elem("bird", parent, 0, "circle"),
                    elem("html", "game", 1, "foreignObject"),
                    elem("note", "html", 0, "p"),
                ],
                &[],
                &[],
            )
        };
        let a = in_svg("game");
        let created: Vec<(String, bool)> = Page::default()
            .diff(&a)
            .into_iter()
            .filter_map(|p| match p {
                Patch::Create { id, svg, .. } => Some((id, svg)),
                _ => None,
            })
            .collect();
        assert_eq!(
            created,
            [
                ("bird".to_owned(), true),
                ("box".to_owned(), false),
                ("game".to_owned(), true),
                ("html".to_owned(), true),
                ("note".to_owned(), false),
            ]
        );
        // The circle moves out of the svg: another element.
        let b = in_svg("box");
        let d = a.diff(&b);
        assert!(d.contains(&Patch::Remove { id: "bird".to_owned() }), "{d:?}");
        assert!(
            d.contains(&Patch::Create {
                id: "bird".to_owned(),
                tag: "circle".to_owned(),
                svg: false
            }),
            "{d:?}"
        );
    }

    #[test]
    fn an_element_that_is_its_own_ancestor_is_an_error() {
        let err = Page::of(&[elem("a", "b", 0, "div"), elem("b", "a", 0, "div")], &[], &[], &[]).unwrap_err();
        assert!(err.to_string().contains("its own ancestor"), "{err}");
    }

    #[test]
    fn an_element_given_twice_or_under_a_missing_parent_is_an_error() {
        assert!(Page::of(&[elem("a", "", 0, "div"), elem("a", "", 1, "div")], &[], &[], &[]).is_err());
        assert!(Page::of(&[elem("a", "nope", 0, "div")], &[], &[], &[]).is_err());
        // The same row twice is the same element.
        assert!(Page::of(&[elem("a", "", 0, "div"), elem("a", "", 0, "div")], &[], &[], &[]).is_ok());
    }

    // ---------------------------------------------------------------- the reference: the whole page, rebuilt

    /// The page the outputs' rows describe, built whole (the page before it was kept incrementally).
    fn reference_of(elem: &[Row], attr: &[Row], text: &[Row], focus: &[Row]) -> Result<Page, HostError> {
        let mut page = Page::default();
        for row in elem {
            let [id, parent, pos, tag] = columns::<4>(row, "elem")?;
            let id = string(id, "an element's id")?;
            let pos = match pos {
                Value::Int(IntValue::I64(p)) => *p,
                other => {
                    return Err(HostError::Page(format!(
                        "element `{id}`'s position is {other:?}, not an i64"
                    )));
                }
            };
            let e = Elem {
                parent: string(parent, "an element's parent")?,
                pos,
                tag: string(tag, "an element's tag")?,
                svg: false,
            };
            if let Some(old) = page.elems.insert(id.clone(), e.clone())
                && old != e
            {
                return Err(HostError::Page(format!(
                    "element `{id}` is given twice ({old:?} and {e:?})"
                )));
            }
        }
        for (id, e) in &page.elems {
            if !e.parent.is_empty() && !page.elems.contains_key(&e.parent) {
                return Err(HostError::Page(format!(
                    "element `{id}`'s parent `{}` is not on the page",
                    e.parent
                )));
            }
        }
        let svg = reference_namespaces(&page)?;
        for (id, e) in &mut page.elems {
            e.svg = svg.contains(id);
        }
        for row in attr {
            let [id, name, value] = columns::<3>(row, "attr")?;
            let key = (
                string(id, "an attribute's element")?,
                string(name, "an attribute's name")?,
            );
            let value = string(value, "an attribute's value")?;
            if let Some(old) = page.attrs.insert(key.clone(), value.clone())
                && old != value
            {
                return Err(HostError::Page(format!(
                    "attribute `{}` of `{}` is given twice (`{old}` and `{value}`)",
                    key.1, key.0
                )));
            }
        }
        for row in text {
            let [id, s] = columns::<2>(row, "text")?;
            let id = string(id, "a text's element")?;
            let s = string(s, "a text")?;
            if let Some(old) = page.texts.insert(id.clone(), s.clone())
                && old != s
            {
                return Err(HostError::Page(format!(
                    "the text of `{id}` is given twice (`{old}` and `{s}`)"
                )));
            }
        }
        for row in focus {
            let [id] = columns::<1>(row, "focus")?;
            page.focus.insert(string(id, "a focused element")?);
        }
        Ok(page)
    }

    /// The SVG elements: an `svg`, and the children of an SVG element but a `foreignObject`. An element that is its
    /// own ancestor is an error.
    fn reference_namespaces(page: &Page) -> Result<BTreeSet<String>, HostError> {
        let this = page;
        let mut svg = BTreeSet::new();
        for id in this.elems.keys() {
            // The chain from `id` up to the mount point; the nearest `svg` or `foreignObject` decides.
            let mut seen = BTreeSet::new();
            let mut at = id.as_str();
            let inside = loop {
                if !seen.insert(at) {
                    return Err(HostError::Page(format!("element `{at}` is its own ancestor")));
                }
                let Some(e) = this.elems.get(at) else { break false };
                match e.tag.as_str() {
                    "svg" => break true,
                    "foreignObject" if at != id => break false,
                    _ => {}
                }
                if e.parent.is_empty() {
                    break false;
                }
                at = e.parent.as_str();
            };
            if inside {
                svg.insert(id.clone());
            }
        }
        Ok(svg)
    }

    /// A tiny deterministic generator (xorshift).
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
    }

    /// A random page's rows: mostly well formed, sometimes not (a duplicate, an orphan, a cycle, two values).
    fn random_rows(rng: &mut Rng) -> [BTreeSet<Row>; 4] {
        const TAGS: [&str; 6] = ["div", "span", "svg", "circle", "foreignObject", "p"];
        let id = |i: u64| format!("e{i}");
        let mut elems = BTreeSet::new();
        for i in 0..8 {
            if rng.below(4) == 0 {
                continue;
            }
            let parent = match rng.below(60) {
                0 => id(9),            // not on the page
                1 => id(rng.below(8)), // maybe below itself: a cycle
                _ if i == 0 => String::new(),
                _ => match rng.below(i + 1) {
                    0 => String::new(),
                    p => id(p - 1),
                },
            };
            let tag = TAGS[rng.below(6) as usize];
            elems.insert(elem(&id(i), &parent, rng.below(3) as i64, tag));
            if rng.below(80) == 0 {
                elems.insert(elem(&id(i), &parent, 7, "b"));
            }
        }
        let mut attrs = BTreeSet::new();
        for _ in 0..rng.below(8) {
            // Mostly one value per attribute (which changes from page to page); now and then two.
            let (i, name) = (rng.below(9), ["class", "value", "x"][rng.below(3) as usize]);
            let v = if rng.below(25) == 0 { rng.below(3) } else { i % 3 };
            let v = (v + rng.below(2)) % 3;
            attrs.retain(|r: &Row| rng.below(25) == 0 || r[0] != s(&id(i)) || r[1] != s(name));
            attrs.insert(attr(&id(i), name, &format!("v{v}")));
        }
        let mut text = BTreeSet::new();
        for _ in 0..rng.below(5) {
            let i = rng.below(9);
            text.retain(|r: &Row| rng.below(25) == 0 || r[0] != s(&id(i)));
            let row: Row = Arc::from(vec![s(&id(i)), s(&format!("t{}", rng.below(3)))]);
            text.insert(row);
        }
        let mut focus = BTreeSet::new();
        if rng.below(2) == 0 {
            let row: Row = Arc::from(vec![s(&id(rng.below(9)))]);
            focus.insert(row);
        }
        [elems, attrs, text, focus]
    }

    #[test]
    fn the_incremental_page_patches_exactly_as_the_whole_page_comparison() {
        let mut rng = Rng(0x2545_f491_4f6c_dd1d);
        let (mut errors, mut steps) = (0, 0);
        for _run in 0..200 {
            let mut page = Page::default();
            let mut reference = Page::default();
            let mut rows: [BTreeSet<Row>; 4] = Default::default();
            for _step in 0..12 {
                let next = random_rows(&mut rng);
                let delta = |k: usize| -> (Vec<Row>, Vec<Row>) {
                    (
                        next[k].difference(&rows[k]).cloned().collect(),
                        rows[k].difference(&next[k]).cloned().collect(),
                    )
                };
                let d = Delta {
                    elem: delta(0),
                    attr: delta(1),
                    text: delta(2),
                    focus: delta(3),
                };
                let as_vec = |k: usize| next[k].iter().cloned().collect::<Vec<Row>>();
                let whole = reference_of(&as_vec(0), &as_vec(1), &as_vec(2), &as_vec(3));
                let applied = page.apply(&d);
                rows = next;
                steps += 1;
                match whole {
                    Ok(whole) => {
                        assert!(applied.is_ok(), "incremental refused a good page: {applied:?}");
                        assert_eq!(page.take_patches(), reference.diff(&whole));
                        assert!(page == whole, "the pages differ");
                        reference = whole;
                    }
                    Err(e) => {
                        assert!(applied.is_err(), "incremental accepted a bad page ({e})");
                        errors += 1;
                    }
                }
            }
        }
        // The generator reaches both kinds of page.
        assert!(
            errors > steps / 10 && errors < steps * 9 / 10,
            "{errors} errors in {steps} steps"
        );
    }
}
