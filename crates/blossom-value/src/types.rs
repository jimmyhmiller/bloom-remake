//! The type language (ARCHITECTURE §2.2; LANGUAGE §5).
//!
//! A [`TypeTable`] holds structural, hash-consed [`TypeDef`]s: inserting a definition equal to an existing one
//! returns the existing [`TypeId`]. Types are inserted bottom-up (a definition may only refer to types already in
//! the table), so the table is acyclic and every child id is smaller than its parent's. `TypeId`s therefore depend
//! on insertion order; the program digest relabels them canonically (ARCHITECTURE §2.10). Every id type is defined
//! in `blossom-base`, so `TypeDef::Node` can name a `RoleId` without depending on the IR.

use std::collections::BTreeSet;
use std::fmt;
use std::sync::Arc;

use blossom_base::{DetMap, GroupTypeId, IndexVec, LatticeTypeId, QualName, RoleId, Symbol, TypeId};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::ValueError;
use crate::value::Value;

/// A type.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum TypeDef {
    /// `bool`.
    Bool,
    /// A sized integer.
    Int(IntTy),
    /// `f64`.
    F64,
    /// `String`.
    Str,
    /// `Bytes`.
    Bytes,
    /// `()`.
    Unit,
    /// `Duration`.
    Duration,
    /// `Instant`.
    Instant,
    /// `Mod<N>`, `1 ≤ N ≤ 256` (LANG-026).
    Mod {
        /// The width `N`.
        bits: u16,
    },
    /// `Blob`: a content-addressed handle (LANG-028).
    Blob,
    /// `Session` (LANG-243).
    Session,
    /// `Conn`: a byte-stream connection (FOREIGN-PROTOCOLS §1).
    Conn,
    /// `Principal` (LANG-240).
    Principal,
    /// `Node` (`None`) or `Node<R>`.
    Node(Option<RoleId>),
    /// A tuple of at least one element.
    Tuple(Vec<TypeId>),
    /// A struct; fields in declaration order (= canonical order).
    Struct(StructDef),
    /// An enum; variants with stable numbers, one `#[unknown]` if it crosses a boundary.
    Enum(EnumDef),
    /// `Vec<T>`.
    Vec(TypeId),
    /// `Set<T>`.
    Set(TypeId),
    /// `Map<K, V>`.
    Map(TypeId, TypeId),
    /// `Option<T>`.
    Option(TypeId),
    /// A lattice type (declared in the IR's lattice table).
    Lattice(LatticeTypeId),
    /// A group or ring type (declared in the IR's group table).
    Group(GroupTypeId),
    /// An opaque host type (LANG-027).
    Extern(ExternTypeDef),
}

/// The sized integer types.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub enum IntTy {
    /// `u8`.
    U8,
    /// `u16`.
    U16,
    /// `u32`.
    U32,
    /// `u64`.
    U64,
    /// `u128`.
    U128,
    /// `i8`.
    I8,
    /// `i16`.
    I16,
    /// `i32`.
    I32,
    /// `i64`.
    I64,
    /// `i128`.
    I128,
}

impl IntTy {
    /// Every integer type, in canonical order.
    pub const ALL: [IntTy; 10] = [
        IntTy::U8,
        IntTy::U16,
        IntTy::U32,
        IntTy::U64,
        IntTy::U128,
        IntTy::I8,
        IntTy::I16,
        IntTy::I32,
        IntTy::I64,
        IntTy::I128,
    ];

    /// The width in bits.
    pub const fn bits(self) -> u32 {
        match self {
            IntTy::U8 | IntTy::I8 => 8,
            IntTy::U16 | IntTy::I16 => 16,
            IntTy::U32 | IntTy::I32 => 32,
            IntTy::U64 | IntTy::I64 => 64,
            IntTy::U128 | IntTy::I128 => 128,
        }
    }

    /// Whether the type is signed.
    pub const fn is_signed(self) -> bool {
        matches!(self, IntTy::I8 | IntTy::I16 | IntTy::I32 | IntTy::I64 | IntTy::I128)
    }

    /// The source name (`u8`, …).
    pub const fn name(self) -> &'static str {
        match self {
            IntTy::U8 => "u8",
            IntTy::U16 => "u16",
            IntTy::U32 => "u32",
            IntTy::U64 => "u64",
            IntTy::U128 => "u128",
            IntTy::I8 => "i8",
            IntTy::I16 => "i16",
            IntTy::I32 => "i32",
            IntTy::I64 => "i64",
            IntTy::I128 => "i128",
        }
    }
}

/// A stable field or variant number (`#n`, LANG-261).
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub struct FieldNo(pub u32);

/// A struct type.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct StructDef {
    /// The struct's name.
    pub name: QualName,
    /// Fields in declaration order.
    pub fields: Vec<FieldDef>,
    /// Retired field numbers (`#[reserved]`).
    pub reserved: Vec<FieldNo>,
}

/// An enum type.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct EnumDef {
    /// The enum's name.
    pub name: QualName,
    /// The variants.
    pub variants: Vec<VariantDef>,
    /// The number of the `#[unknown]` variant, if any.
    pub unknown: Option<u32>,
    /// Retired variant numbers.
    pub reserved: Vec<FieldNo>,
}

/// A struct field or variant payload field.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct FieldDef {
    /// The field name.
    pub name: Symbol,
    /// Its type.
    pub ty: TypeId,
    /// Its stable number, once assigned.
    pub field_no: Option<FieldNo>,
    /// The default for a `#[since]` field.
    pub default: Option<Value>,
    /// The version that added it.
    pub since: Option<u32>,
    /// The version that deprecated it.
    pub deprecated: Option<u32>,
    /// Its previous name (`#[renamed_from]`).
    pub renamed_from: Option<Symbol>,
}

/// An enum variant.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct VariantDef {
    /// The variant name.
    pub name: Symbol,
    /// Its stable number; variants are encoded and ordered by number, never by index.
    pub number: u32,
    /// Payload fields.
    pub payload: Vec<FieldDef>,
    /// The version that added it.
    pub since: Option<u32>,
}

/// The stable identity of an opaque host type's codec.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub struct ExternCodecId(pub Arc<str>);

/// An opaque host type (LANG-027): `extern type Regex = "regex::Regex";`.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct ExternTypeDef {
    /// The Blossom name.
    pub name: QualName,
    /// The Rust type path.
    pub rust_path: Arc<str>,
    /// The codec that encodes its values.
    pub codec: ExternCodecId,
}

/// The structural, hash-consed table of types.
#[derive(Clone, Default)]
pub struct TypeTable {
    types: IndexVec<TypeId, TypeDef>,
    dedup: DetMap<TypeDef, TypeId>,
}

impl TypeTable {
    /// An empty table.
    pub fn new() -> TypeTable {
        TypeTable::default()
    }

    /// The number of types.
    pub fn len(&self) -> usize {
        self.types.len()
    }

    /// Whether the table is empty.
    pub fn is_empty(&self) -> bool {
        self.types.is_empty()
    }

    /// The definition of `id`.
    pub fn get(&self, id: TypeId) -> Option<&TypeDef> {
        self.types.get(id)
    }

    /// The definition of `id`, or [`ValueError::UnknownType`].
    pub fn def(&self, id: TypeId) -> Result<&TypeDef, ValueError> {
        self.types.get(id).ok_or(ValueError::UnknownType(id))
    }

    /// The id of a definition already in the table.
    pub fn lookup(&self, def: &TypeDef) -> Option<TypeId> {
        self.dedup.get(def).copied()
    }

    /// Every `(id, definition)`, in id order (children before parents).
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = (TypeId, &TypeDef)> + ExactSizeIterator + '_ {
        self.types.iter_enumerated()
    }

    /// Inserts a definition, returning the id of the equal existing one if there is one. The definition is checked
    /// first: its child types must be in the table and it must be well formed (see [`TypeTable::validate`]).
    pub fn insert(&mut self, def: TypeDef) -> Result<TypeId, ValueError> {
        if let Some(&id) = self.dedup.get(&def) {
            return Ok(id);
        }
        self.validate(&def)?;
        let id = self.types.push(def.clone())?;
        self.dedup.insert(def, id);
        Ok(id)
    }

    /// Inserts a definition known to be new at a fixed position (deserialization).
    fn insert_new(&mut self, def: TypeDef) -> Result<TypeId, ValueError> {
        if self.dedup.contains_key(&def) {
            return Err(ValueError::InvalidType(format!("the type table lists {def:?} twice")));
        }
        self.insert(def)
    }

    /// Checks that `def` is well formed against this table:
    /// - every child type is already in the table;
    /// - a tuple has at least one element (`()` is [`TypeDef::Unit`]);
    /// - a `Mod` width is in `1..=256`;
    /// - struct fields and variant payload fields have distinct names and distinct field numbers, none of them
    ///   reserved; field defaults conform to the field's type;
    /// - enum variants have distinct names and numbers, none reserved, and `unknown` names one of them.
    ///
    /// Lattice and group ids and `Node<R>` roles are declared by the IR and are not checked here.
    pub fn validate(&self, def: &TypeDef) -> Result<(), ValueError> {
        let child = |id: TypeId| self.def(id).map(|_| ());
        match def {
            TypeDef::Bool
            | TypeDef::Int(_)
            | TypeDef::F64
            | TypeDef::Str
            | TypeDef::Bytes
            | TypeDef::Unit
            | TypeDef::Duration
            | TypeDef::Instant
            | TypeDef::Blob
            | TypeDef::Session
            | TypeDef::Conn
            | TypeDef::Principal
            | TypeDef::Node(_)
            | TypeDef::Lattice(_)
            | TypeDef::Group(_)
            | TypeDef::Extern(_) => Ok(()),
            TypeDef::Mod { bits } => {
                if (1..=256).contains(bits) {
                    Ok(())
                } else {
                    Err(ValueError::InvalidType(format!(
                        "Mod<{bits}>: the width must be in 1..=256"
                    )))
                }
            }
            TypeDef::Tuple(items) => {
                if items.is_empty() {
                    return Err(ValueError::InvalidType(
                        "an empty tuple is written `()` (TypeDef::Unit)".into(),
                    ));
                }
                items.iter().try_for_each(|&t| child(t))
            }
            TypeDef::Vec(t) | TypeDef::Set(t) | TypeDef::Option(t) => child(*t),
            TypeDef::Map(k, v) => child(*k).and_then(|()| child(*v)),
            TypeDef::Struct(s) => self.validate_fields(&s.name.to_string(), &s.fields, &s.reserved),
            TypeDef::Enum(e) => self.validate_enum(e),
        }
    }

    fn validate_fields(&self, owner: &str, fields: &[FieldDef], reserved: &[FieldNo]) -> Result<(), ValueError> {
        let mut names = BTreeSet::new();
        let mut numbers = BTreeSet::new();
        for f in fields {
            self.def(f.ty)?;
            if !names.insert(f.name) {
                return Err(ValueError::InvalidType(format!(
                    "{owner}: field `{}` is declared twice",
                    f.name
                )));
            }
            if let Some(no) = f.field_no {
                if !numbers.insert(no) {
                    return Err(ValueError::InvalidType(format!(
                        "{owner}: field number #{} is used twice",
                        no.0
                    )));
                }
                if reserved.contains(&no) {
                    return Err(ValueError::InvalidType(format!(
                        "{owner}: field number #{} is reserved",
                        no.0
                    )));
                }
            }
            if let Some(default) = &f.default {
                self.check_value(f.ty, default).map_err(|e| {
                    ValueError::InvalidType(format!(
                        "{owner}: the default of field `{}` does not conform: {e}",
                        f.name
                    ))
                })?;
            }
        }
        Ok(())
    }

    fn validate_enum(&self, e: &EnumDef) -> Result<(), ValueError> {
        let owner = e.name.to_string();
        let mut names = BTreeSet::new();
        let mut numbers = BTreeSet::new();
        for v in &e.variants {
            if !names.insert(v.name) {
                return Err(ValueError::InvalidType(format!(
                    "{owner}: variant `{}` is declared twice",
                    v.name
                )));
            }
            if !numbers.insert(v.number) {
                return Err(ValueError::InvalidType(format!(
                    "{owner}: variant number #{} is used twice",
                    v.number
                )));
            }
            if e.reserved.contains(&FieldNo(v.number)) {
                return Err(ValueError::InvalidType(format!(
                    "{owner}: variant number #{} is reserved",
                    v.number
                )));
            }
            self.validate_fields(&format!("{owner}::{}", v.name), &v.payload, &[])?;
        }
        if let Some(u) = e.unknown
            && !numbers.contains(&u)
        {
            return Err(ValueError::InvalidType(format!(
                "{owner}: the #[unknown] variant #{u} does not exist"
            )));
        }
        Ok(())
    }

    /// Checks that `value` has the shape of type `ty` (LANGUAGE §5). The contents of lattice, group and extern
    /// values are the business of their owning crates (`blossom-lattice`, the extern codec), and `Node<R>` role
    /// membership is a deployment fact; for those only the kind of value is checked (a keyed member's role is in the
    /// value, so it is checked).
    pub fn check_value(&self, ty: TypeId, value: &Value) -> Result<(), ValueError> {
        let def = self.def(ty)?;
        let mismatch = |reason: String| ValueError::TypeMismatch { ty, reason };
        let kind_mismatch = || mismatch(format!("expected {}, found {}", def_kind(def), value_kind(value)));
        match (def, value) {
            (TypeDef::Bool, Value::Bool(_))
            | (TypeDef::F64, Value::F64(_))
            | (TypeDef::Str, Value::Str(_))
            | (TypeDef::Bytes, Value::Bytes(_))
            | (TypeDef::Unit, Value::Unit)
            | (TypeDef::Duration, Value::Duration(_))
            | (TypeDef::Instant, Value::Instant(_))
            | (TypeDef::Blob, Value::Blob(_))
            | (TypeDef::Session, Value::Session(_))
            | (TypeDef::Conn, Value::Conn(_))
            | (TypeDef::Principal, Value::Principal(_))
            | (TypeDef::Node(_), Value::Node(_))
            | (TypeDef::Node(None), Value::Member(_))
            | (TypeDef::Lattice(_), Value::Lattice(_))
            | (TypeDef::Group(_), Value::Group(_)) => Ok(()),
            // A keyed member carries its role, so `Node<R>` checks it.
            (TypeDef::Node(Some(r)), Value::Member(m)) => {
                if m.role == *r {
                    Ok(())
                } else {
                    Err(mismatch(format!("a member of role {:?}, not of {r:?}", m.role)))
                }
            }
            (TypeDef::Int(t), Value::Int(i)) => {
                if i.ty() == *t {
                    Ok(())
                } else {
                    Err(mismatch(format!("expected {}, found {}", t.name(), i.ty().name())))
                }
            }
            (TypeDef::Mod { bits }, Value::Mod(m)) => {
                if m.bits() == *bits {
                    Ok(())
                } else {
                    Err(mismatch(format!("expected Mod<{bits}>, found Mod<{}>", m.bits())))
                }
            }
            (TypeDef::Tuple(types), Value::Tuple(items)) => self.check_all(ty, types, items),
            (TypeDef::Struct(s), Value::Struct(items)) => {
                let types: Vec<TypeId> = s.fields.iter().map(|f| f.ty).collect();
                self.check_all(ty, &types, items)
            }
            (TypeDef::Enum(e), Value::Enum { variant, fields }) => {
                let v = e
                    .variants
                    .iter()
                    .find(|v| v.number == *variant)
                    .ok_or_else(|| mismatch(format!("{} has no variant #{variant}", e.name)))?;
                let types: Vec<TypeId> = v.payload.iter().map(|f| f.ty).collect();
                self.check_all(ty, &types, fields)
            }
            (TypeDef::Enum(e), Value::UnknownVariant { variant, .. }) => {
                if e.unknown == Some(*variant) {
                    Ok(())
                } else {
                    Err(mismatch(format!(
                        "{}: #{variant} is not its #[unknown] variant",
                        e.name
                    )))
                }
            }
            (TypeDef::Vec(t), Value::Vec(items)) => items.iter().try_for_each(|v| self.check_value(*t, v)),
            (TypeDef::Set(t), Value::Set(items)) => items.iter().try_for_each(|v| self.check_value(*t, v)),
            (TypeDef::Map(k, v), Value::Map(entries)) => entries
                .iter()
                .try_for_each(|(key, val)| self.check_value(*k, key).and_then(|()| self.check_value(*v, val))),
            (TypeDef::Option(t), Value::Option(o)) => match o {
                Some(v) => self.check_value(*t, v),
                None => Ok(()),
            },
            (TypeDef::Extern(x), Value::Extern { codec, .. }) => {
                if *codec == x.codec {
                    Ok(())
                } else {
                    Err(mismatch(format!("expected codec {:?}, found {:?}", x.codec.0, codec.0)))
                }
            }
            _ => Err(kind_mismatch()),
        }
    }

    fn check_all(&self, ty: TypeId, types: &[TypeId], values: &[Value]) -> Result<(), ValueError> {
        if types.len() != values.len() {
            return Err(ValueError::TypeMismatch {
                ty,
                reason: format!("expected {} fields, found {}", types.len(), values.len()),
            });
        }
        types.iter().zip(values).try_for_each(|(t, v)| self.check_value(*t, v))
    }
}

fn def_kind(def: &TypeDef) -> &'static str {
    match def {
        TypeDef::Bool => "bool",
        TypeDef::Int(t) => t.name(),
        TypeDef::F64 => "f64",
        TypeDef::Str => "String",
        TypeDef::Bytes => "Bytes",
        TypeDef::Unit => "()",
        TypeDef::Duration => "Duration",
        TypeDef::Instant => "Instant",
        TypeDef::Mod { .. } => "Mod",
        TypeDef::Blob => "Blob",
        TypeDef::Session => "Session",
        TypeDef::Conn => "Conn",
        TypeDef::Principal => "Principal",
        TypeDef::Node(_) => "Node",
        TypeDef::Tuple(_) => "a tuple",
        TypeDef::Struct(_) => "a struct",
        TypeDef::Enum(_) => "an enum",
        TypeDef::Vec(_) => "Vec",
        TypeDef::Set(_) => "Set",
        TypeDef::Map(..) => "Map",
        TypeDef::Option(_) => "Option",
        TypeDef::Lattice(_) => "a lattice",
        TypeDef::Group(_) => "a group",
        TypeDef::Extern(_) => "an extern type",
    }
}

fn value_kind(value: &Value) -> &'static str {
    match value {
        Value::Unit => "()",
        Value::Bool(_) => "bool",
        Value::Int(i) => i.ty().name(),
        Value::F64(_) => "f64",
        Value::Str(_) => "String",
        Value::Bytes(_) => "Bytes",
        Value::Duration(_) => "Duration",
        Value::Instant(_) => "Instant",
        Value::Mod(_) => "Mod",
        Value::Blob(_) => "Blob",
        Value::Session(_) => "Session",
        Value::Conn(_) => "Conn",
        Value::Principal(_) => "Principal",
        Value::Node(_) | Value::Member(_) => "Node",
        Value::Tuple(_) => "a tuple",
        Value::Struct(_) => "a struct",
        Value::Enum { .. } | Value::UnknownVariant { .. } => "an enum value",
        Value::Vec(_) => "Vec",
        Value::Set(_) => "Set",
        Value::Map(_) => "Map",
        Value::Option(_) => "Option",
        Value::Lattice(_) => "a lattice value",
        Value::Group(_) => "a group value",
        Value::Extern { .. } => "an extern value",
    }
}

impl PartialEq for TypeTable {
    fn eq(&self, other: &Self) -> bool {
        self.types == other.types
    }
}

impl Eq for TypeTable {}

impl fmt::Debug for TypeTable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.types, f)
    }
}

/// Serialized as the definitions in id order.
impl Serialize for TypeTable {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.types.serialize(serializer)
    }
}

/// Rebuilt by inserting every definition in order, which re-checks each one and rejects duplicates, so a
/// deserialized table satisfies the same invariants as a built one.
impl<'de> Deserialize<'de> for TypeTable {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let defs = Vec::<TypeDef>::deserialize(deserializer)?;
        let mut table = TypeTable::new();
        for def in defs {
            table.insert_new(def).map_err(serde::de::Error::custom)?;
        }
        Ok(table)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::IntValue;

    fn field(name: &str, ty: TypeId, no: Option<u32>) -> FieldDef {
        FieldDef {
            name: Symbol::intern(name),
            ty,
            field_no: no.map(FieldNo),
            default: None,
            since: None,
            deprecated: None,
            renamed_from: None,
        }
    }

    fn qn(s: &str) -> QualName {
        QualName::parse_dotted(s).unwrap()
    }

    #[test]
    fn type_table_dedup() {
        let mut t = TypeTable::new();
        let u64_a = t.insert(TypeDef::Int(IntTy::U64)).unwrap();
        let str_ = t.insert(TypeDef::Str).unwrap();
        let u64_b = t.insert(TypeDef::Int(IntTy::U64)).unwrap();
        assert_eq!(u64_a, u64_b);
        let pair_a = t.insert(TypeDef::Tuple(vec![u64_a, str_])).unwrap();
        let pair_b = t.insert(TypeDef::Tuple(vec![u64_b, str_])).unwrap();
        assert_eq!(pair_a, pair_b);
        let other = t.insert(TypeDef::Tuple(vec![str_, u64_a])).unwrap();
        assert_ne!(pair_a, other);
        let set = t.insert(TypeDef::Set(pair_a)).unwrap();
        assert_eq!(t.insert(TypeDef::Set(pair_b)).unwrap(), set);
        // Nominal types are distinguished by name even with equal fields.
        let s1 = t.insert(TypeDef::Struct(StructDef {
            name: qn("m.A"),
            fields: vec![field("x", u64_a, None)],
            reserved: vec![],
        }));
        let s2 = t.insert(TypeDef::Struct(StructDef {
            name: qn("m.B"),
            fields: vec![field("x", u64_a, None)],
            reserved: vec![],
        }));
        assert_ne!(s1.unwrap(), s2.unwrap());
        assert_eq!(t.len(), 7);
        assert_eq!(t.lookup(&TypeDef::Set(pair_a)), Some(set));
        assert_eq!(t.get(str_), Some(&TypeDef::Str));
        // Children always precede their parents.
        for (id, def) in t.iter() {
            if let TypeDef::Tuple(items) = def {
                assert!(items.iter().all(|c| c < &id));
            }
        }
    }

    #[test]
    fn type_table_rejects_ill_formed_types() {
        let mut t = TypeTable::new();
        let u8_ = t.insert(TypeDef::Int(IntTy::U8)).unwrap();
        assert!(matches!(
            t.insert(TypeDef::Vec(TypeId::from_raw(99))),
            Err(ValueError::UnknownType(_))
        ));
        assert!(t.insert(TypeDef::Tuple(vec![])).is_err());
        assert!(t.insert(TypeDef::Mod { bits: 0 }).is_err());
        assert!(t.insert(TypeDef::Mod { bits: 257 }).is_err());
        assert!(t.insert(TypeDef::Mod { bits: 160 }).is_ok());
        let dup = StructDef {
            name: qn("m.S"),
            fields: vec![field("a", u8_, None), field("a", u8_, None)],
            reserved: vec![],
        };
        assert!(t.insert(TypeDef::Struct(dup)).is_err());
        let dup_no = StructDef {
            name: qn("m.S"),
            fields: vec![field("a", u8_, Some(1)), field("b", u8_, Some(1))],
            reserved: vec![],
        };
        assert!(t.insert(TypeDef::Struct(dup_no)).is_err());
        let reserved = StructDef {
            name: qn("m.S"),
            fields: vec![field("a", u8_, Some(4))],
            reserved: vec![FieldNo(4)],
        };
        assert!(t.insert(TypeDef::Struct(reserved)).is_err());
        let mut bad_default = field("a", u8_, Some(1));
        bad_default.default = Some(Value::Int(IntValue::U16(3)));
        assert!(
            t.insert(TypeDef::Struct(StructDef {
                name: qn("m.S"),
                fields: vec![bad_default.clone()],
                reserved: vec![]
            }))
            .is_err()
        );
        bad_default.default = Some(Value::Int(IntValue::U8(3)));
        assert!(
            t.insert(TypeDef::Struct(StructDef {
                name: qn("m.S"),
                fields: vec![bad_default],
                reserved: vec![]
            }))
            .is_ok()
        );
        let variant = |name: &str, number| VariantDef {
            name: Symbol::intern(name),
            number,
            payload: vec![],
            since: None,
        };
        let e = |variants, unknown, reserved| EnumDef {
            name: qn("m.Op"),
            variants,
            unknown,
            reserved,
        };
        assert!(
            t.insert(TypeDef::Enum(e(
                vec![variant("Put", 1), variant("Del", 1)],
                None,
                vec![]
            )))
            .is_err()
        );
        assert!(
            t.insert(TypeDef::Enum(e(
                vec![variant("Put", 1), variant("Put", 2)],
                None,
                vec![]
            )))
            .is_err()
        );
        assert!(
            t.insert(TypeDef::Enum(e(vec![variant("Put", 1)], Some(5), vec![])))
                .is_err()
        );
        assert!(
            t.insert(TypeDef::Enum(e(vec![variant("Put", 4)], None, vec![FieldNo(4)])))
                .is_err()
        );
        assert!(
            t.insert(TypeDef::Enum(e(
                vec![variant("Put", 1), variant("Unknown", 5)],
                Some(5),
                vec![FieldNo(4)]
            )))
            .is_ok()
        );
    }

    #[test]
    fn type_table_check_value() {
        let mut t = TypeTable::new();
        let u64_ = t.insert(TypeDef::Int(IntTy::U64)).unwrap();
        let s = t.insert(TypeDef::Str).unwrap();
        let tup = t.insert(TypeDef::Tuple(vec![u64_, s])).unwrap();
        let map = t.insert(TypeDef::Map(s, u64_)).unwrap();
        let opt = t.insert(TypeDef::Option(tup)).unwrap();
        let payload = vec![field("n", u64_, None)];
        let op = t
            .insert(TypeDef::Enum(EnumDef {
                name: qn("m.Op"),
                variants: vec![
                    VariantDef {
                        name: Symbol::intern("Put"),
                        number: 1,
                        payload,
                        since: None,
                    },
                    VariantDef {
                        name: Symbol::intern("Unknown"),
                        number: 9,
                        payload: vec![],
                        since: None,
                    },
                ],
                unknown: Some(9),
                reserved: vec![],
            }))
            .unwrap();
        assert!(
            t.check_value(tup, &Value::tuple([Value::u64(1), Value::str("a")]))
                .is_ok()
        );
        assert!(t.check_value(tup, &Value::tuple([Value::u64(1)])).is_err());
        assert!(
            t.check_value(tup, &Value::tuple([Value::i64(1), Value::str("a")]))
                .is_err()
        );
        assert!(
            t.check_value(map, &Value::map([(Value::str("k"), Value::u64(2))]).unwrap())
                .is_ok()
        );
        assert!(
            t.check_value(map, &Value::map([(Value::u64(2), Value::u64(2))]).unwrap())
                .is_err()
        );
        assert!(t.check_value(opt, &Value::none()).is_ok());
        assert!(
            t.check_value(opt, &Value::some(Value::tuple([Value::u64(1), Value::str("a")])))
                .is_ok()
        );
        assert!(t.check_value(op, &Value::variant(1, [Value::u64(3)])).is_ok());
        assert!(t.check_value(op, &Value::variant(1, [])).is_err());
        assert!(t.check_value(op, &Value::variant(2, [])).is_err());
        let unknown = |variant| Value::UnknownVariant {
            variant,
            wire_number: 12,
            payload: [1u8, 2].into(),
        };
        assert!(t.check_value(op, &unknown(9)).is_ok());
        assert!(t.check_value(op, &unknown(1)).is_err());
        assert!(matches!(
            t.check_value(TypeId::from_raw(77), &Value::Unit),
            Err(ValueError::UnknownType(_))
        ));
    }

    #[test]
    fn type_table_serde_roundtrip() {
        let mut t = TypeTable::new();
        let u = t.insert(TypeDef::Int(IntTy::U32)).unwrap();
        let v = t.insert(TypeDef::Vec(u)).unwrap();
        t.insert(TypeDef::Map(u, v)).unwrap();
        let json = serde_json::to_string(&t).unwrap();
        let back: TypeTable = serde_json::from_str(&json).unwrap();
        assert_eq!(back, t);
        assert_eq!(back.lookup(&TypeDef::Vec(u)), Some(v));
        // A child after its parent, or a duplicate, is rejected.
        assert!(serde_json::from_str::<TypeTable>(r#"[{"Vec":1},{"Int":"U32"}]"#).is_err());
        assert!(serde_json::from_str::<TypeTable>(r#"["Str","Str"]"#).is_err());
    }
}
