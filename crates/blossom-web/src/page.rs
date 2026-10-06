//! The page a program describes (BROWSER.md "The page"): its `elem`, `attr`, `text` and `focus` outputs at the end of
//! a round, and the patches that turn one page into the next.
//!
//! Elements are keyed by id: an element whose id and tag stay keeps its DOM node across rounds (and with it its
//! focus, caret and scroll position); one whose tag changes is replaced. A parent's children are ordered by `pos`,
//! then `id`. An `svg` element and its descendants (but those of a `foreignObject`) are SVG elements: the DOM makes
//! them in the SVG namespace, so an element that moves in or out of an `svg` is replaced too.

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

/// A page: what the program's outputs hold at the end of a round.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Page {
    elems: BTreeMap<String, Elem>,
    attrs: BTreeMap<(String, String), String>,
    texts: BTreeMap<String, String>,
    focus: BTreeSet<String>,
}

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

impl Page {
    /// The page the outputs' rows describe. An element given twice (two rows with one id) or under a parent that is
    /// not on the page is a program error.
    pub fn of(elem: &[Row], attr: &[Row], text: &[Row], focus: &[Row]) -> Result<Page, HostError> {
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
        let svg = page.namespaces()?;
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
    fn namespaces(&self) -> Result<BTreeSet<String>, HostError> {
        let mut svg = BTreeSet::new();
        for id in self.elems.keys() {
            // The chain from `id` up to the mount point; the nearest `svg` or `foreignObject` decides.
            let mut seen = BTreeSet::new();
            let mut at = id.as_str();
            let inside = loop {
                if !seen.insert(at) {
                    return Err(HostError::Page(format!("element `{at}` is its own ancestor")));
                }
                let Some(e) = self.elems.get(at) else { break false };
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

    /// Each parent's children, in order.
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

    /// The patches that turn this page into `next`.
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
}
