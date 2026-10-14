//! Native delta evaluation of the normative keyed-upsert deletion join.
//! The expansion and its stores remain visible; unsupported shapes use the general evaluator.

use crate::rule::{Driver, StoreKey, Stores};
use blossom_base::RelId;
use blossom_ir::core::{ConstructKind, HeadArg, HeadMode, Literal, Program, Rule, RuleKind, Term};
use blossom_ir::tick::{EvalError, Row};
use blossom_value::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug)]
pub(crate) struct UpsertDelete {
    staging: RelId,
    target: RelId,
    key: Vec<usize>,
    width: usize,
}

impl UpsertDelete {
    pub fn of(p: &Program, rule: &Rule) -> Option<Self> {
        let construct = p.constructs.get(rule.construct?)?;
        let ConstructKind::Upsert { rel, staging, del } = construct.kind else {
            return None;
        };
        if !construct.rules.contains(&rule.id)
            || rule.kind != RuleKind::Deductive
            || rule.head.rel != del
            || rule.head.mode != HeadMode::Insert
        {
            return None;
        }
        let [Literal::Pos(a), Literal::Pos(b)] = rule.body.lits.as_slice() else {
            return None;
        };
        if a.rel != staging || b.rel != rel {
            return None;
        }
        if [a, b]
            .iter()
            .any(|a| a.sender.is_some() || a.principal.is_some() || a.weight.is_some() || a.spec.is_some())
        {
            return None;
        }
        let schema = &p.rels.get(rel)?.schema;
        let key: Vec<usize> = schema.key.iter().map(|c| c.index()).collect();
        if a.args.len() != b.args.len() || b.args.len() != rule.head.args.len() {
            return None;
        }
        let mut vars = BTreeSet::new();
        for (i, (term, head)) in b.args.iter().zip(&rule.head.args).enumerate() {
            let Term::Var(v) = term else {
                return None;
            };
            if !vars.insert(*v) || head != &HeadArg::Term(term.clone()) {
                return None;
            }
            if a.args.get(i)? != &if key.contains(&i) { term.clone() } else { Term::Wild } {
                return None;
            }
        }
        Some(Self {
            staging,
            target: rel,
            key,
            width: b.args.len(),
        })
    }

    fn check_width(&self, row: &Row) -> Result<(), EvalError> {
        if row.len() != self.width {
            return Err(blossom_base::internal_error!(
                "native upsert expects {} columns, got {}",
                self.width,
                row.len()
            )
            .into());
        }
        Ok(())
    }

    fn project(&self, row: &Row) -> Result<Vec<Value>, EvalError> {
        self.check_width(row)?;
        self.key
            .iter()
            .map(|i| {
                row.get(*i)
                    .cloned()
                    .ok_or_else(|| blossom_base::internal_error!("native upsert key column {i} out of range").into())
            })
            .collect()
    }

    pub fn run(&self, stores: &Stores, drivers: &[(Driver, usize)]) -> Result<(BTreeMap<Row, i64>, u64), EvalError> {
        let get = |r| -> Result<&crate::store::Store, EvalError> {
            stores
                .get(&StoreKey::Main(r))
                .ok_or_else(|| blossom_base::internal_error!("missing upsert store {r:?}").into())
        };
        let staging = get(self.staging)?;
        let target = get(self.target)?;
        let mut heads = BTreeMap::new();
        let mut examined = 0;
        for (driver, _) in drivers {
            match driver {
                Driver::Full => {
                    for row in staging.new_rows(&[], &[])? {
                        examined += 1;
                        let values = self.project(&row)?;
                        for old in target.new_rows(&self.key, &values)? {
                            self.check_width(&old)?;
                            examined += 1;
                            *heads.entry(old).or_insert(0) += 1;
                        }
                    }
                }
                Driver::Atom { lit: 0, row, sign } => {
                    let values = self.project(row)?;
                    // Δstaging joins the target's OLD version; the other term uses staging NEW.
                    for old in target.old_rows(&self.key, &values)? {
                        self.check_width(&old)?;
                        examined += 1;
                        *heads.entry(old).or_insert(0) += sign;
                    }
                }
                Driver::Atom { lit: 1, row, sign } => {
                    let values = self.project(row)?;
                    let count = staging.new_rows(&self.key, &values)?.len();
                    examined += count as u64;
                    *heads.entry(row.clone()).or_insert(0) += *sign * count as i64;
                }
                _ => return Err(blossom_base::internal_error!("non-atom driver in native upsert deletion").into()),
            }
        }
        Ok((heads, examined))
    }
}
