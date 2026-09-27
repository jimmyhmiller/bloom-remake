//! A complete deterministic reference store over direct scalar words and interned values.
use crate::error::ValueError;
use crate::fp::{Fingerprint, fingerprint};
use crate::types::{TypeDef, TypeTable};
use crate::value::Value;
use crate::word::{BytesWord, Lane, StrWord, Word, decode_scalar, encode_scalar, scalar_kind};
use blossom_base::TypeId;
use smallvec::SmallVec;
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::sync::Arc;

pub trait RecordTarget {
    fn finish_record(&mut self, ty: TypeId, children: &[Word]) -> Result<Word, ValueError>;
}
pub struct RecordBuilder<'a> {
    target: &'a mut dyn RecordTarget,
    ty: TypeId,
    children: SmallVec<[Word; 8]>,
}
impl<'a> RecordBuilder<'a> {
    pub fn new(target: &'a mut dyn RecordTarget, ty: TypeId) -> Self {
        Self {
            target,
            ty,
            children: SmallVec::new(),
        }
    }
    pub fn ty(&self) -> TypeId {
        self.ty
    }
    pub fn len(&self) -> usize {
        self.children.len()
    }
    pub fn is_empty(&self) -> bool {
        self.children.is_empty()
    }
    pub fn push(&mut self, child: Word) -> &mut Self {
        self.children.push(child);
        self
    }
    pub fn extend(&mut self, children: &[Word]) -> &mut Self {
        self.children.extend_from_slice(children);
        self
    }
    pub fn finish(self) -> Result<Word, ValueError> {
        self.target.finish_record(self.ty, &self.children)
    }
}
pub trait ValueStore: RecordTarget {
    fn intern_bytes(&mut self, ty: TypeId, bytes: &[u8]) -> Result<Word, ValueError>;
    fn intern_str(&mut self, ty: TypeId, s: &str) -> Result<Word, ValueError>;
    fn record(&mut self, ty: TypeId) -> RecordBuilder<'_>;
    fn intern_value(&mut self, ty: TypeId, v: &Value) -> Result<Word, ValueError>;
    fn fingerprint(&self, ty: TypeId, w: Word) -> Result<Fingerprint, ValueError>;
    fn cmp_canonical(&self, ty: TypeId, a: Word, b: Word) -> Result<Ordering, ValueError>;
    fn to_value(&self, ty: TypeId, w: Word) -> Result<Value, ValueError>;
    fn str_of(&self, w: StrWord) -> Result<&str, ValueError>;
    fn bytes_of(&self, w: BytesWord) -> Result<&[u8], ValueError>;
}
pub struct RefValueStore {
    types: Arc<TypeTable>,
    values: Vec<(TypeId, Value)>,
    intern: BTreeMap<(TypeId, Value), Word>,
}
impl RefValueStore {
    pub fn new(types: Arc<TypeTable>) -> Self {
        Self {
            types,
            values: Vec::new(),
            intern: BTreeMap::new(),
        }
    }
    pub fn types(&self) -> &TypeTable {
        &self.types
    }
    fn stored(&self, w: Word) -> Result<&(TypeId, Value), ValueError> {
        let n =
            w.0.checked_sub(1)
                .ok_or_else(|| ValueError::InvalidValue("zero intern id".into()))?;
        self.values
            .get(usize::try_from(n).map_err(|_| ValueError::InvalidValue("intern id too wide".into()))?)
            .ok_or_else(|| ValueError::InvalidValue(format!("unknown intern id {}", w.0)))
    }
    fn fields(&self, types: &[TypeId], children: &[Word]) -> Result<Vec<Value>, ValueError> {
        if types.len() != children.len() {
            return Err(ValueError::InvalidValue(format!(
                "record expects {} children, got {}",
                types.len(),
                children.len()
            )));
        }
        types.iter().zip(children).map(|(t, w)| self.to_value(*t, *w)).collect()
    }
}
impl RecordTarget for RefValueStore {
    fn finish_record(&mut self, ty: TypeId, children: &[Word]) -> Result<Word, ValueError> {
        let value = match self.types.def(ty)? {
            TypeDef::Tuple(ts) => Value::tuple(self.fields(ts, children)?),
            TypeDef::Struct(s) => {
                Value::record(self.fields(&s.fields.iter().map(|f| f.ty).collect::<Vec<_>>(), children)?)
            }
            TypeDef::Vec(t) => Value::vec(
                children
                    .iter()
                    .map(|w| self.to_value(*t, *w))
                    .collect::<Result<Vec<_>, _>>()?,
            ),
            TypeDef::Set(t) => Value::set(
                children
                    .iter()
                    .map(|w| self.to_value(*t, *w))
                    .collect::<Result<Vec<_>, _>>()?,
            ),
            TypeDef::Map(k, v) => {
                if !children.len().is_multiple_of(2) {
                    return Err(ValueError::InvalidValue(
                        "map record needs alternating key/value children".into(),
                    ));
                }
                Value::map(
                    children
                        .chunks_exact(2)
                        .map(|pair| {
                            Ok((
                                self.to_value(
                                    *k,
                                    *pair
                                        .first()
                                        .ok_or_else(|| ValueError::InvalidValue("missing map key".into()))?,
                                )?,
                                self.to_value(
                                    *v,
                                    *pair
                                        .get(1)
                                        .ok_or_else(|| ValueError::InvalidValue("missing map value".into()))?,
                                )?,
                            ))
                        })
                        .collect::<Result<Vec<_>, ValueError>>()?,
                )?
            }
            TypeDef::Option(t) => match children {
                [] => Value::Option(None),
                [w] => Value::some(self.to_value(*t, *w)?),
                _ => return Err(ValueError::InvalidValue("option record needs zero or one child".into())),
            },
            TypeDef::Enum(e) => {
                let (tag, fields) = children
                    .split_first()
                    .ok_or_else(|| ValueError::InvalidValue("enum record needs variant number".into()))?;
                let number = u32::try_from(tag.0).map_err(|_| ValueError::InvalidValue("enum tag too wide".into()))?;
                let variant = e
                    .variants
                    .iter()
                    .find(|v| v.number == number)
                    .ok_or_else(|| ValueError::InvalidValue(format!("unknown enum tag {number}")))?;
                let ts = variant.payload.iter().map(|f| f.ty).collect::<Vec<_>>();
                Value::variant(number, self.fields(&ts, fields)?)
            }
            _ => {
                return Err(ValueError::InvalidType(
                    "this type has no record representation; use intern_value".into(),
                ));
            }
        };
        self.intern_value(ty, &value)
    }
}
impl ValueStore for RefValueStore {
    fn intern_bytes(&mut self, ty: TypeId, bytes: &[u8]) -> Result<Word, ValueError> {
        self.intern_value(ty, &Value::bytes(bytes))
    }
    fn intern_str(&mut self, ty: TypeId, s: &str) -> Result<Word, ValueError> {
        self.intern_value(ty, &Value::str(s))
    }
    fn record(&mut self, ty: TypeId) -> RecordBuilder<'_> {
        RecordBuilder::new(self, ty)
    }
    fn intern_value(&mut self, ty: TypeId, v: &Value) -> Result<Word, ValueError> {
        self.types.check_value(ty, v)?;
        if let Some(kind) = scalar_kind(&self.types, ty)? {
            return encode_scalar(kind, Lane::U64, v);
        }
        if let Some(w) = self.intern.get(&(ty, v.clone())) {
            return Ok(*w);
        }
        let w = Word(
            u64::try_from(self.values.len())
                .map_err(|_| ValueError::InvalidValue("intern table too large".into()))?
                .checked_add(1)
                .ok_or_else(|| ValueError::InvalidValue("intern table too large".into()))?,
        );
        self.values.push((ty, v.clone()));
        self.intern.insert((ty, v.clone()), w);
        Ok(w)
    }
    fn fingerprint(&self, ty: TypeId, w: Word) -> Result<Fingerprint, ValueError> {
        fingerprint(&self.to_value(ty, w)?)
    }
    fn cmp_canonical(&self, ty: TypeId, a: Word, b: Word) -> Result<Ordering, ValueError> {
        Ok(self.to_value(ty, a)?.cmp(&self.to_value(ty, b)?))
    }
    fn to_value(&self, ty: TypeId, w: Word) -> Result<Value, ValueError> {
        if let Some(kind) = scalar_kind(&self.types, ty)? {
            let value = decode_scalar(kind, Lane::U64, w)?;
            self.types.check_value(ty, &value)?;
            return Ok(value);
        }
        let (actual, value) = self.stored(w)?;
        if *actual != ty {
            return Err(ValueError::TypeMismatch {
                ty,
                reason: format!("intern id {} belongs to {actual:?}", w.0),
            });
        }
        Ok(value.clone())
    }
    fn str_of(&self, w: StrWord) -> Result<&str, ValueError> {
        match &self.stored(w.0)?.1 {
            Value::Str(s) => Ok(s),
            _ => Err(ValueError::InvalidValue("word is not a string".into())),
        }
    }
    fn bytes_of(&self, w: BytesWord) -> Result<&[u8], ValueError> {
        match &self.stored(w.0)?.1 {
            Value::Bytes(s) => Ok(s),
            _ => Err(ValueError::InvalidValue("word is not bytes".into())),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ref_store_interns_compares_and_builds() {
        let mut types = TypeTable::new();
        let str_ty = types.insert(TypeDef::Str).unwrap();
        let tup_ty = types.insert(TypeDef::Tuple(vec![str_ty])).unwrap();
        let mut store = RefValueStore::new(Arc::new(types));
        let x = store.intern_str(str_ty, "x").unwrap();
        let y = store.intern_str(str_ty, "y").unwrap();
        assert_eq!(x, store.intern_str(str_ty, "x").unwrap());
        assert_eq!(store.str_of(StrWord(x)).unwrap(), "x");
        assert_eq!(store.cmp_canonical(str_ty, x, y).unwrap(), Ordering::Less);
        let mut builder = store.record(tup_ty);
        builder.push(x);
        let row = builder.finish().unwrap();
        assert_eq!(store.to_value(tup_ty, row).unwrap(), Value::tuple([Value::str("x")]));
        assert_eq!(
            store.fingerprint(str_ty, x).unwrap(),
            fingerprint(&Value::str("x")).unwrap()
        );
        assert!(store.to_value(str_ty, Word(999)).is_err());
    }
}
