//! Schema identities: a relation's schema hash covers its name and the structure of its columns' types (field
//! numbers, struct and enum shapes, lattice constructors), so two builds agree on a channel or a durable relation
//! exactly when they encode it alike. HELLO carries one per channel; WAL and checkpoint headers one per durable
//! relation.

use std::fmt::Write as _;

use blossom_base::{RelId, TypeId};
use blossom_ir::core::{LatticeCtor, Program};
use blossom_value::TypeDef;

/// The 128-bit schema hash of relation `rel`.
pub fn schema_hash(p: &Program, rel: RelId) -> [u8; 16] {
    let mut desc = String::new();
    if let Some(r) = p.rels.get(rel) {
        let _ = write!(desc, "{}(", r.name);
        for (i, c) in r.schema.cols.iter().enumerate() {
            let n = c.field_no.map_or(i as u32 + 1, |f| f.0);
            let _ = write!(desc, "#{n}:");
            type_desc(p, c.ty, &mut desc, 0);
            desc.push(',');
        }
        desc.push(')');
    }
    let h = blake3::hash(desc.as_bytes());
    let mut out = [0u8; 16];
    out.copy_from_slice(&h.as_bytes()[..16]);
    out
}

fn type_desc(p: &Program, ty: TypeId, out: &mut String, depth: u32) {
    if depth > 64 {
        out.push('…');
        return;
    }
    let sub = |t: TypeId, out: &mut String| type_desc(p, t, out, depth + 1);
    match p.types.get(ty) {
        Some(TypeDef::Tuple(ts)) => {
            out.push('(');
            for t in ts {
                sub(*t, out);
                out.push(',');
            }
            out.push(')');
        }
        Some(TypeDef::Struct(s)) => {
            let _ = write!(out, "struct {}{{", s.name);
            for (i, f) in s.fields.iter().enumerate() {
                let n = f.field_no.map_or(i as u32 + 1, |x| x.0);
                let _ = write!(out, "#{n}:");
                sub(f.ty, out);
                out.push(',');
            }
            out.push('}');
        }
        Some(TypeDef::Enum(e)) => {
            let _ = write!(out, "enum {}{{", e.name);
            for v in &e.variants {
                let _ = write!(out, "{}#{}(", v.name, v.number);
                for f in &v.payload {
                    sub(f.ty, out);
                    out.push(',');
                }
                out.push_str("),");
            }
            out.push('}');
        }
        Some(TypeDef::Vec(t)) => {
            out.push_str("Vec<");
            sub(*t, out);
            out.push('>');
        }
        Some(TypeDef::Set(t)) => {
            out.push_str("Set<");
            sub(*t, out);
            out.push('>');
        }
        Some(TypeDef::Map(k, v)) => {
            out.push_str("Map<");
            sub(*k, out);
            out.push(',');
            sub(*v, out);
            out.push('>');
        }
        Some(TypeDef::Option(t)) => {
            out.push_str("Option<");
            sub(*t, out);
            out.push('>');
        }
        Some(TypeDef::Lattice(id)) => match p.lattices.get(*id).map(|l| &l.ctor) {
            Some(LatticeCtor::Bool) => out.push_str("LBool"),
            Some(LatticeCtor::Max(t)) => {
                out.push_str("LMax<");
                sub(*t, out);
                out.push('>');
            }
            Some(LatticeCtor::Min(t)) => {
                out.push_str("LMin<");
                sub(*t, out);
                out.push('>');
            }
            Some(LatticeCtor::Set(t)) => {
                out.push_str("LSet<");
                sub(*t, out);
                out.push('>');
            }
            Some(LatticeCtor::PSet(t)) => {
                out.push_str("LPSet<");
                sub(*t, out);
                out.push('>');
            }
            Some(LatticeCtor::Point(t)) => {
                out.push_str("LPoint<");
                sub(*t, out);
                out.push('>');
            }
            Some(LatticeCtor::Map(k, inner)) => {
                out.push_str("LMap<");
                sub(*k, out);
                let _ = write!(out, ",{inner:?}>");
            }
            other => {
                let _ = write!(out, "{other:?}");
            }
        },
        // `Node<R>` and `Node` encode alike, unless a keyed role's member may be among them (docs/design/KEYED.md):
        // it is written with its role's id, so the keyed roles' ids are part of the schema.
        Some(TypeDef::Node(r)) => match r {
            Some(r) if p.is_keyed(*r) => {
                let name = p.roles.get(*r).map(|d| d.name.to_string()).unwrap_or_default();
                let _ = write!(out, "Node<keyed {name}#{}>", r.raw());
            }
            Some(_) => out.push_str("Node"),
            None => {
                out.push_str("Node");
                for d in p.keyed_roles() {
                    let _ = write!(out, "|keyed {}#{}", d.name, d.id.raw());
                }
            }
        },
        Some(other) => {
            let _ = write!(out, "{other:?}");
        }
        None => out.push('?'),
    }
}
