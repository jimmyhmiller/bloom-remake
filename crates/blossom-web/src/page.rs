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
//! patches, in proportion to the change: an element is placed among its siblings only when it is new, moved, or
//! under a new parent (the tests check that the patches build the same DOM a comparison of the whole page before and
//! after would).

use std::collections::{BTreeMap, BTreeSet};

use blossom_base::det::DetMap;

use blossom_ir::tick::Row;
use blossom_value::Value;
use blossom_value::value::IntValue;
use serde::Serialize;

use crate::HostError;

/// A string of the page: shared with the output rows it comes from (cloning one is a reference count).
type Text = std::sync::Arc<str>;

/// One element: its parent (`""`: the mount point), its position among its siblings, its tag.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Elem {
    parent: Text,
    pos: i64,
    tag: Text,
    /// In the SVG namespace (set once the page's elements are known).
    svg: bool,
}

/// An `elem` row's columns after the id: parent, position, tag.
type ElemRow = (Text, i64, Text);

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
    elems: BTreeMap<Text, Option<Elem>>,
    attrs: BTreeMap<(Text, Text), Option<Text>>,
    texts: BTreeMap<Text, Option<Text>>,
    focus: BTreeMap<Text, bool>,
}

/// A page: what the program's outputs hold at the end of a round.
#[derive(Clone, Debug, Default)]
pub struct Page {
    // Hashed by id (no order is read from them: the journal and each parent's children are ordered).
    elems: DetMap<Text, Elem>,
    /// Each element's attributes, by name (an element with none has no entry).
    attrs: DetMap<Text, BTreeMap<Text, Text>>,
    texts: DetMap<Text, Text>,
    focus: BTreeSet<Text>,
    /// The rows the outputs hold, by id: a page is well formed when each holds at most one.
    elem_rows: DetMap<Text, BTreeSet<ElemRow>>,
    attr_rows: DetMap<(Text, Text), BTreeSet<Text>>,
    text_rows: DetMap<Text, BTreeSet<Text>>,
    /// Each parent's children, by position then id (a parent with none has no entry).
    kids: DetMap<Text, BTreeSet<(i64, Text)>>,
    /// What a round left ill formed, checked again in the next (as a whole-page check would).
    recheck_elems: BTreeSet<Text>,
    recheck_attrs: BTreeSet<(Text, Text)>,
    recheck_texts: BTreeSet<Text>,
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
    /// Drop the element `id` (its children that stay on the page are placed again by a [`Patch::Place`]).
    Remove { id: String },
    /// Set an attribute (or, for `value`, `checked` and `disabled`, the property).
    Attr { id: String, name: String, value: String },
    /// Remove an attribute (or clear the property).
    Unattr { id: String, name: String },
    /// Set the text before the element's children (`""`: none).
    Text { id: String, text: String },
    /// Put the element `id` under `parent` (`""`: the mount point), just before its sibling `before` (`None`: last).
    /// Places come last-sibling-first, so `before` is already where it belongs.
    Place {
        parent: String,
        id: String,
        before: Option<String>,
    },
    /// Focus the element.
    Focus { id: String },
}

fn string(v: &Value, what: &str) -> Result<Text, HostError> {
    match v {
        Value::Str(s) => Ok(s.clone()),
        other => Err(HostError::Page(format!("{what} is {other:?}, not a String"))),
    }
}

fn columns<'r, const N: usize>(row: &'r Row, rel: &str) -> Result<&'r [Value; N], HostError> {
    <&[Value; N]>::try_from(&row[..])
        .map_err(|_| HostError::Page(format!("a row of `{rel}` with {} columns, not {N}", row.len())))
}

fn elem_row(row: &Row) -> Result<(Text, ElemRow), HostError> {
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

fn attr_row(row: &Row) -> Result<((Text, Text), Text), HostError> {
    let [id, name, value] = columns::<3>(row, "attr")?;
    let key = (
        string(id, "an attribute's element")?,
        string(name, "an attribute's name")?,
    );
    Ok((key, string(value, "an attribute's value")?))
}

fn text_row(row: &Row) -> Result<(Text, Text), HostError> {
    let [id, s] = columns::<2>(row, "text")?;
    Ok((string(id, "a text's element")?, string(s, "a text")?))
}

fn focus_row(row: &Row) -> Result<Text, HostError> {
    let [id] = columns::<1>(row, "focus")?;
    string(id, "a focused element")
}

/// Adds or removes `row` from the set under `key`, dropping an empty set.
fn edit<K: std::hash::Hash + Eq + Clone, V: Ord>(map: &mut DetMap<K, BTreeSet<V>>, key: &K, value: V, add: bool) {
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
        let mut elems: BTreeSet<Text> = std::mem::take(&mut self.recheck_elems);
        let mut attrs: BTreeSet<(Text, Text)> = std::mem::take(&mut self.recheck_attrs);
        let mut texts: BTreeSet<Text> = std::mem::take(&mut self.recheck_texts);
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
            let mut todo: Vec<Text> = changed
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
            if self.attr(&key.0, &key.1) != next.as_ref() {
                let old = self.attr(&key.0, &key.1).cloned();
                self.journal.attrs.entry(key.clone()).or_insert(old);
                let (id, name) = key;
                match next {
                    Some(v) => {
                        self.attrs.entry(id).or_default().insert(name, v);
                    }
                    None => {
                        if let Some(names) = self.attrs.get_mut(&id) {
                            names.remove(&name);
                            if names.is_empty() {
                                self.attrs.remove(&id);
                            }
                        }
                    }
                }
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

    /// The value of attribute `name` of element `id`.
    fn attr(&self, id: &str, name: &str) -> Option<&Text> {
        self.attrs.get(id).and_then(|names| names.get(name))
    }

    /// Replaces element `id` (journaling what it was).
    fn set_elem(&mut self, id: &Text, next: Option<Elem>) {
        let old = self.elems.get(id).cloned();
        self.journal.elems.entry(id.clone()).or_insert_with(|| old.clone());
        let moved = old.as_ref().map(|e| (&e.parent, e.pos)) != next.as_ref().map(|e| (&e.parent, e.pos));
        if moved {
            if let Some(e) = &old {
                edit(&mut self.kids, &e.parent, (e.pos, id.clone()), false);
            }
            if let Some(e) = &next {
                edit(&mut self.kids, &e.parent, (e.pos, id.clone()), true);
            }
        }
        match next {
            Some(e) => self.elems.insert(id.clone(), e),
            None => self.elems.remove(id),
        };
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
            match &*e.tag {
                "svg" => return Ok(true),
                "foreignObject" if at != id => return Ok(false),
                _ => {}
            }
            if e.parent.is_empty() {
                return Ok(false);
            }
            at = &e.parent;
        }
    }

    /// The patches that turn the page of the last call (or the empty page) into this one.
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
                out.push(Patch::Remove { id: id.to_string() });
            }
        }
        let mut fresh = BTreeSet::new();
        for id in j.elems.keys() {
            if let Some(e) = self.elems.get(id)
                && !kept(id)
            {
                out.push(Patch::Create {
                    id: id.to_string(),
                    tag: e.tag.to_string(),
                    svg: e.svg,
                });
                fresh.insert(id.clone());
            }
        }
        // Attributes: a new element's all, a kept one's changes.
        let mut keys: BTreeSet<(Text, Text)> = j.attrs.keys().cloned().collect();
        for id in &fresh {
            keys.extend(
                self.attrs
                    .get(id)
                    .into_iter()
                    .flat_map(|names| names.keys())
                    .map(|n| (id.clone(), n.clone())),
            );
        }
        let old_attr = |key: &(Text, Text)| -> Option<&Text> {
            match j.attrs.get(key) {
                Some(old) => old.as_ref(),
                None => self.attr(&key.0, &key.1),
            }
        };
        for key in &keys {
            let Some(value) = self.attr(&key.0, &key.1) else {
                continue;
            };
            if !self.elems.contains_key(&key.0) {
                continue;
            }
            let old = if kept(&key.0) { old_attr(key) } else { None };
            if old != Some(value) {
                out.push(Patch::Attr {
                    id: key.0.to_string(),
                    name: key.1.to_string(),
                    value: value.to_string(),
                });
            }
        }
        for (key, old) in &j.attrs {
            if old.is_some() && kept(&key.0) && self.attr(&key.0, &key.1).is_none() {
                out.push(Patch::Unattr {
                    id: key.0.to_string(),
                    name: key.1.to_string(),
                });
            }
        }
        // Texts.
        let ids: BTreeSet<&Text> = j.texts.keys().chain(fresh.iter()).collect();
        for id in ids {
            if !self.elems.contains_key(id) {
                continue;
            }
            let new = self.texts.get(id).map_or("", |t| &**t);
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
                    id: id.to_string(),
                    text: new.to_owned(),
                });
            }
        }
        // Placing: a new element, one that moved, and the children of a new one (they were in the old node). Each
        // parent's, last sibling first.
        let mut place: BTreeMap<&Text, BTreeSet<(i64, &Text)>> = BTreeMap::new();
        for (id, old) in &j.elems {
            let Some(e) = self.elems.get(id) else { continue };
            let moved = old.as_ref().is_none_or(|o| o.parent != e.parent || o.pos != e.pos);
            if moved || !kept(id) {
                place.entry(&e.parent).or_default().insert((e.pos, id));
            }
        }
        for parent in &fresh {
            for (pos, kid) in self.kids.get(parent).into_iter().flatten() {
                place.entry(parent).or_default().insert((*pos, kid));
            }
        }
        for (parent, kids) in place {
            for (pos, id) in kids.into_iter().rev() {
                let before = self
                    .kids
                    .get(parent)
                    .and_then(|s| s.range((pos, id.clone())..).nth(1))
                    .map(|(_, k)| k.to_string());
                out.push(Patch::Place {
                    parent: parent.to_string(),
                    id: id.to_string(),
                    before,
                });
            }
        }
        // Focus what the program newly asks to focus.
        for (id, was) in &j.focus {
            if !was && self.focus.contains(id) && self.elems.contains_key(id) {
                out.push(Patch::Focus { id: id.to_string() });
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

    /// The ids of the elements on the page, in order.
    pub fn ids(&self) -> Vec<&str> {
        let mut ids: Vec<&str> = self.elems.keys().map(|k| &**k).collect();
        ids.sort_unstable();
        ids
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

    fn rows(elems: &[Row], attrs: &[Row], focus: &[&str]) -> [BTreeSet<Row>; 4] {
        [
            elems.iter().cloned().collect(),
            attrs.iter().cloned().collect(),
            BTreeSet::new(),
            focus.iter().map(|f| -> Row { Arc::from(vec![s(f)]) }).collect(),
        ]
    }

    /// The changes from `before`'s rows to `after`'s.
    fn delta(before: &[BTreeSet<Row>; 4], after: &[BTreeSet<Row>; 4]) -> Delta {
        let d = |k: usize| -> (Vec<Row>, Vec<Row>) {
            (
                after[k].difference(&before[k]).cloned().collect(),
                before[k].difference(&after[k]).cloned().collect(),
            )
        };
        Delta {
            elem: d(0),
            attr: d(1),
            text: d(2),
            focus: d(3),
        }
    }

    /// The patches from the page of `a`'s rows to that of `b`'s.
    fn patches(a: &[BTreeSet<Row>; 4], b: &[BTreeSet<Row>; 4]) -> Vec<Patch> {
        let mut page = Page::default();
        page.apply(&delta(&Default::default(), a)).unwrap();
        page.take_patches();
        page.apply(&delta(a, b)).unwrap();
        page.take_patches()
    }

    fn place(parent: &str, id: &str, before: Option<&str>) -> Patch {
        Patch::Place {
            parent: parent.to_owned(),
            id: id.to_owned(),
            before: before.map(str::to_owned),
        }
    }

    #[test]
    fn children_are_ordered_by_position_and_moves_reorder_without_recreating() {
        let a = rows(
            &[
                elem("ul", "", 0, "ul"),
                elem("x", "ul", 1, "li"),
                elem("y", "ul", 2, "li"),
            ],
            &[],
            &[],
        );
        let b = rows(
            &[
                elem("ul", "", 0, "ul"),
                elem("x", "ul", 3, "li"),
                elem("y", "ul", 2, "li"),
            ],
            &[],
            &[],
        );
        // Only the element that moved is placed, last among its siblings.
        assert_eq!(patches(&a, &b), [place("ul", "x", None)]);
        // Equal positions order by id; siblings are placed last first.
        let c = rows(
            &[
                elem("ul", "", 0, "ul"),
                elem("b", "ul", 0, "li"),
                elem("a", "ul", 0, "li"),
            ],
            &[],
            &[],
        );
        let p = patches(&Default::default(), &c);
        let b_at = p.iter().position(|x| *x == place("ul", "b", None));
        let a_at = p.iter().position(|x| *x == place("ul", "a", Some("b")));
        assert!(b_at.is_some() && a_at.is_some() && b_at < a_at, "{p:?}");
    }

    #[test]
    fn adding_one_child_places_it_alone() {
        let kids: Vec<Row> = (0..50).map(|i| elem(&format!("li{i}"), "ul", i, "li")).collect();
        let mut more = kids.clone();
        more.push(elem("li50", "ul", 25, "li"));
        let base = [elem("ul", "", 0, "ul")];
        let a = rows(&[&base[..], &kids].concat(), &[], &[]);
        let b = rows(&[&base[..], &more].concat(), &[], &[]);
        // Between li25 and li26 by (position, id): ("li25", 25) < ("li50", 25) < ("li26", 26).
        let p = patches(&a, &b);
        assert_eq!(
            p,
            [
                Patch::Create {
                    id: "li50".to_owned(),
                    tag: "li".to_owned(),
                    svg: false
                },
                place("ul", "li50", Some("li26"))
            ]
        );
    }

    #[test]
    fn a_new_tag_replaces_the_element_and_its_children_are_placed_again() {
        let a = rows(
            &[elem("box", "", 0, "div"), elem("t", "box", 0, "span")],
            &[attr("box", "class", "x")],
            &[],
        );
        let b = rows(
            &[elem("box", "", 0, "section"), elem("t", "box", 0, "span")],
            &[attr("box", "class", "x")],
            &[],
        );
        assert_eq!(
            patches(&a, &b),
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
                place("", "box", None),
                place("box", "t", None),
            ]
        );
    }

    #[test]
    fn attributes_set_change_and_go_and_focus_is_asked_once() {
        let a = rows(
            &[elem("i", "", 0, "input")],
            &[attr("i", "value", "a"), attr("i", "class", "c")],
            &[],
        );
        let b = rows(&[elem("i", "", 0, "input")], &[attr("i", "value", "b")], &["i"]);
        assert_eq!(
            patches(&a, &b),
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
        assert_eq!(patches(&b, &b), []);
    }

    #[test]
    fn svg_elements_are_made_in_their_namespace_and_replaced_when_they_leave_it() {
        let in_svg = |parent: &str| {
            rows(
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
        let created: Vec<(String, bool)> = patches(&Default::default(), &a)
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
        let d = patches(&a, &in_svg("box"));
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

    // ---------------------------------------------------------------- a DOM, to check what the patches build

    #[derive(Clone, Debug, Default)]
    struct Node {
        tag: String,
        svg: bool,
        attrs: BTreeMap<String, String>,
        text: String,
        kids: Vec<usize>,
        parent: Option<usize>,
    }

    /// A DOM as the browser host builds it: nodes with identity (a created node is a new one; a removed one leaves
    /// with its subtree), the mount point node 0, each patch applied as the host applies it.
    #[derive(Clone, Debug)]
    struct Dom {
        nodes: Vec<Node>,
        by_id: BTreeMap<String, usize>,
    }

    impl Dom {
        fn new() -> Dom {
            Dom {
                nodes: vec![Node::default()],
                by_id: BTreeMap::new(),
            }
        }

        fn handle(&self, id: &str) -> usize {
            if id.is_empty() { 0 } else { self.by_id[id] }
        }

        fn detach(&mut self, n: usize) {
            if let Some(p) = self.nodes[n].parent.take() {
                self.nodes[p].kids.retain(|k| *k != n);
            }
        }

        /// `parent.insertBefore(n, before)`.
        fn insert_before(&mut self, parent: usize, n: usize, before: Option<usize>) {
            self.detach(n);
            let at = before
                .and_then(|b| self.nodes[parent].kids.iter().position(|k| *k == b))
                .unwrap_or(self.nodes[parent].kids.len());
            self.nodes[parent].kids.insert(at, n);
            self.nodes[n].parent = Some(parent);
        }

        fn create(&mut self, id: &str, tag: &str, svg: bool) {
            self.nodes.push(Node {
                tag: tag.to_owned(),
                svg,
                ..Node::default()
            });
            self.by_id.insert(id.to_owned(), self.nodes.len() - 1);
        }

        fn remove(&mut self, id: &str) {
            if let Some(n) = self.by_id.remove(id) {
                self.detach(n);
            }
        }

        fn apply(&mut self, patches: &[Patch]) {
            for p in patches {
                match p {
                    Patch::Create { id, tag, svg } => self.create(id, tag, *svg),
                    Patch::Remove { id } => self.remove(id),
                    Patch::Attr { id, name, value } => {
                        let n = self.handle(id);
                        self.nodes[n].attrs.insert(name.clone(), value.clone());
                    }
                    Patch::Unattr { id, name } => {
                        let n = self.handle(id);
                        self.nodes[n].attrs.remove(name);
                    }
                    Patch::Text { id, text } => {
                        let n = self.handle(id);
                        self.nodes[n].text = text.clone();
                    }
                    Patch::Place { parent, id, before } => {
                        let (p, n) = (self.handle(parent), self.handle(id));
                        let before = before.as_deref().map(|b| self.handle(b));
                        self.insert_before(p, n, before);
                    }
                    Patch::Focus { .. } => {}
                }
            }
        }

        /// What the mount point shows, as text.
        fn render(&self) -> String {
            fn go(d: &Dom, n: usize, out: &mut String) {
                let node = &d.nodes[n];
                out.push_str(&format!(
                    "<{} {} {:?} {:?}>[",
                    node.tag, node.svg, node.attrs, node.text
                ));
                for k in &node.kids {
                    go(d, *k, out);
                }
                out.push(']');
            }
            let mut out = String::new();
            go(self, 0, &mut out);
            out
        }
    }

    /// The page as a DOM would show it.
    fn shown(page: &Page) -> String {
        fn go(page: &Page, id: &str, out: &mut String) {
            match page.elems.get(id) {
                Some(e) => {
                    let attrs: BTreeMap<String, String> = page
                        .attrs
                        .get(id)
                        .into_iter()
                        .flatten()
                        .map(|(n, v)| (n.to_string(), v.to_string()))
                        .collect();
                    let text = page.texts.get(id).cloned().unwrap_or_default();
                    out.push_str(&format!("<{} {} {attrs:?} {text:?}>[", e.tag, e.svg));
                }
                None => out.push_str(&format!(
                    "<{} {} {:?} {:?}>[",
                    "",
                    false,
                    BTreeMap::<String, String>::new(),
                    ""
                )),
            }
            for (_, k) in page.kids.get(id).into_iter().flatten() {
                go(page, k, out);
            }
            out.push(']');
        }
        let mut out = String::new();
        go(page, "", &mut out);
        out
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
        for (id, e) in page.elems.iter_mut() {
            e.svg = svg.contains(id);
        }
        for row in attr {
            let [id, name, value] = columns::<3>(row, "attr")?;
            let key = (
                string(id, "an attribute's element")?,
                string(name, "an attribute's name")?,
            );
            let value = string(value, "an attribute's value")?;
            if let Some(old) = page
                .attrs
                .entry(key.0.clone())
                .or_default()
                .insert(key.1.clone(), value.clone())
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
    fn reference_namespaces(page: &Page) -> Result<BTreeSet<Text>, HostError> {
        let this = page;
        let mut svg = BTreeSet::new();
        for id in this.elems.keys() {
            // The chain from `id` up to the mount point; the nearest `svg` or `foreignObject` decides.
            let mut seen = BTreeSet::new();
            let mut at: &str = id;
            let inside = loop {
                if !seen.insert(at) {
                    return Err(HostError::Page(format!("element `{at}` is its own ancestor")));
                }
                let Some(e) = this.elems.get(at) else { break false };
                match &*e.tag {
                    "svg" => break true,
                    "foreignObject" if at != &**id => break false,
                    _ => {}
                }
                if e.parent.is_empty() {
                    break false;
                }
                at = &e.parent;
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

    /// The page kept incrementally is the page rebuilt whole from the rows after every round (both refuse the same
    /// ill-formed rounds), and its patches build, in the host's DOM, exactly that page: every round, from the DOM the
    /// rounds before built. (A comparison of the whole page before and after, which the page made before, misses an
    /// element replaced under a parent whose list of ids stays: its new node is never placed.)
    #[test]
    fn the_incremental_page_is_the_page_and_its_patches_build_it() {
        let mut rng = Rng(0x2545_f491_4f6c_dd1d);
        let (mut errors, mut steps) = (0, 0);
        for _run in 0..300 {
            let mut page = Page::default();
            let mut dom = Dom::new();
            let mut rows: [BTreeSet<Row>; 4] = Default::default();
            for _step in 0..12 {
                let next = random_rows(&mut rng);
                let d = delta(&rows, &next);
                let as_vec = |k: usize| next[k].iter().cloned().collect::<Vec<Row>>();
                let whole = reference_of(&as_vec(0), &as_vec(1), &as_vec(2), &as_vec(3));
                let applied = page.apply(&d);
                rows = next;
                steps += 1;
                match whole {
                    Ok(whole) => {
                        assert!(applied.is_ok(), "incremental refused a good page: {applied:?}");
                        assert!(page == whole, "the pages differ");
                        let patches = page.take_patches();
                        dom.apply(&patches);
                        assert_eq!(dom.render(), shown(&page), "the DOM is not the page after {patches:?}");
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
            errors > steps / 20 && errors < steps * 9 / 10,
            "{errors} errors in {steps} steps"
        );
    }
}
