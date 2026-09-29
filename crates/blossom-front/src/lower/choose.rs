//! `choose!` (LANGUAGE §10.4): the functional dependency X̄ → Ȳ over a body's valuations in one tick. The body's
//! other literals give the candidates; per group X̄ the candidate with the least seeded priority
//! `$prio(site, (X̄), (Ȳ))` survives, after the least (`least c`) or greatest (`most c`) cost when there is one:
//!
//! ```ir
//! s$cand(X̄, Ȳ, C) :- body.
//! s$ext(X̄, min<C>) :- s$cand(X̄, Ȳ, C).                                  // `least c`; `max` for `most c`
//! s$pmin(X̄, min<P>) :- s$cand(X̄, Ȳ, C), s$ext(X̄, C), P := $prio(site, (X̄), (Ȳ)).
//! s$chosen(X̄, Ȳ) :- s$cand(X̄, Ȳ, C), s$ext(X̄, C), s$pmin(X̄, P), P == $prio(site, (X̄), (Ȳ)).
//! head :- body, s$chosen(X̄, Ȳ).
//! ```
//!
//! `sticky` keeps last tick's choice while it is still a candidate: `s$keep(X̄, Ȳ) :- s$held(X̄, Ȳ), s$cand(X̄, Ȳ, _)`,
//! the fresh choice ranges over the groups nothing kept, and `s$held(X̄, Ȳ)@next :- s$chosen(X̄, Ȳ)`.

use blossom_base::{InternalError, RelId, Symbol, TypeId, VarId, internal_error};
use blossom_ir::core::{
    AggCall, AggFunc, Atom, BinOp, BuiltinFn, ChoosePolicy, ChooseSpec, Column, ConstructKind, Expr, FnRef, Head,
    HeadArg, HeadMode, Literal, Pattern, RuleKind, SiteKind, StickySpec, Term,
};
use blossom_value::TypeDef;
use blossom_value::types::IntTy;

use super::expr::{Draft, ty_of};
use super::rules::Names;
use super::{Lowerer, atom, col_idx, column, ir, surface};
use crate::hir::HChoose;

impl Lowerer<'_> {
    fn tuple_type(&mut self, tys: Vec<TypeId>) -> Result<TypeId, InternalError> {
        self.b
            .types()
            .insert(TypeDef::Tuple(tys))
            .map_err(|e| internal_error!("interning a type: {e}"))
    }

    /// Lowers the choice `c` of the body drafted in `d`: its expansion, and `s$chosen(X̄, Ȳ)` added to `d`.
    pub(crate) fn choose(&mut self, d: &mut Draft, c: &HChoose, names: &mut Names) -> Result<(), InternalError> {
        let mut x = Vec::new();
        let mut xt = Vec::new();
        for e in &c.per {
            x.push(self.term(d, e)?);
            xt.push(ty_of(e)?);
        }
        let mut y = Vec::new();
        let mut yt = Vec::new();
        for e in &c.chosen {
            y.push(self.term(d, e)?);
            yt.push(ty_of(e)?);
        }
        let cost = match &c.cost {
            Some((e, most)) => Some((self.term(d, e)?, ty_of(e)?, *most)),
            None => None,
        };
        names.counter += 1;
        let tag = format!("$choose#{}", names.counter);
        let site_name = format!("{}::choose#{}", names.base, names.counter);
        let module = names.module.clone();
        let role = names.role;
        let placeholder = |b: &mut blossom_ir::build::IrBuilder| -> Result<_, InternalError> {
            b.begin_construct(
                ConstructKind::Choose(ChooseSpec {
                    site: blossom_base::SiteId::from_raw(0),
                    candidates: RelId::from_raw(0),
                    group: Vec::new(),
                    choice: Vec::new(),
                    policy: ChoosePolicy::Priority,
                    sticky: None,
                    overrides: None,
                    output: RelId::from_raw(0),
                }),
                surface(&module, None, c.span),
            )
            .map_err(ir)
        };
        let construct = placeholder(&mut self.b)?;
        let site_kind = match cost {
            None => SiteKind::Choose,
            Some((_, _, false)) => SiteKind::ChooseLeast,
            Some((_, _, true)) => SiteKind::ChooseMost,
        };
        let site = self.b.declare_site(site_name.into(), site_kind).map_err(ir)?;
        let (nx, ny) = (x.len(), y.len());
        let col = |name: String, ty: TypeId| column(Symbol::intern(&name), ty, false);
        let xcols: Vec<Column> = xt.iter().enumerate().map(|(i, t)| col(format!("x{i}"), *t)).collect();
        let ycols: Vec<Column> = yt.iter().enumerate().map(|(i, t)| col(format!("y{i}"), *t)).collect();
        let group: Vec<usize> = (0..nx).collect();
        let span = c.span;
        let rel_name = |suffix: &str| names.rel_segments(&format!("{tag}{suffix}"));
        // The candidates: X̄, Ȳ and the cost.
        let mut cand_cols: Vec<Column> = xcols.iter().chain(&ycols).cloned().collect();
        if let Some((_, t, _)) = cost {
            cand_cols.push(col("cost".into(), t));
        }
        let cand = self.generated(rel_name("$cand"), cand_cols, None, role, false, span)?;
        let chosen_cols: Vec<Column> = xcols.iter().chain(&ycols).cloned().collect();
        let chosen = self.generated(rel_name("$chosen"), chosen_cols.clone(), None, role, false, span)?;
        let mut cand_args: Vec<Term> = x.iter().chain(&y).cloned().collect();
        if let Some((t, _, _)) = &cost {
            cand_args.push(t.clone());
        }
        let label = self.label(format!("{}{tag}$cand", names.base));
        d.clone().build(
            &mut self.b,
            RuleKind::Deductive,
            label,
            span,
            Head {
                rel: cand,
                args: cand_args.into_iter().map(HeadArg::Term).collect(),
                mode: HeadMode::Insert,
            },
            role,
        )?;
        // Sticky: last tick's choice, kept while it is a candidate.
        let sticky = if c.sticky {
            let held = self.generated(rel_name("$held"), chosen_cols.clone(), None, role, false, span)?;
            let keep = self.generated(rel_name("$keep"), chosen_cols.clone(), None, role, false, span)?;
            let kept = self.generated(rel_name("$kept"), xcols.clone(), None, role, false, span)?;
            Some((held, keep, kept))
        } else {
            None
        };
        let ext = match cost {
            Some((_, t, _)) => {
                let mut cols = xcols.clone();
                cols.push(col("cost".into(), t));
                Some(self.generated(rel_name("$ext"), cols, Some(&group), role, false, span)?)
            }
            None => None,
        };
        let u64t = self
            .b
            .types()
            .insert(TypeDef::Int(IntTy::U64))
            .map_err(|e| internal_error!("interning a type: {e}"))?;
        let xtuple = self.tuple_type(xt.clone())?;
        let ytuple = self.tuple_type(yt.clone())?;
        let prio_ty = self.tuple_type(vec![u64t, ytuple])?;
        let mut pmin_cols = xcols.clone();
        pmin_cols.push(col("prio".into(), prio_ty));
        let pmin = self.generated(rel_name("$pmin"), pmin_cols, Some(&group), role, false, span)?;

        // Rules over the candidate's columns: X0…, Y0…, C.
        let role_id = role.map(|r| blossom_base::RoleId::from_raw(r.0));
        let cand_tys: Vec<TypeId> = xt
            .iter()
            .chain(&yt)
            .copied()
            .chain(cost.as_ref().map(|(_, t, _)| *t))
            .collect();
        let vars = |rb: &mut blossom_ir::build::RuleBuilder<'_>| -> Result<Vec<VarId>, InternalError> {
            let mut out = Vec::new();
            for (i, t) in cand_tys.iter().enumerate() {
                out.push(rb.var(Symbol::intern(&format!("V{i}")), *t).map_err(ir)?);
            }
            Ok(out)
        };
        let terms = |vs: &[VarId]| -> Vec<Term> { vs.iter().map(|v| Term::Var(*v)).collect() };
        let xs = |vs: &[VarId]| -> Vec<Term> { vs.iter().take(nx).map(|v| Term::Var(*v)).collect() };
        let xy = |vs: &[VarId]| -> Vec<Term> { vs.iter().take(nx + ny).map(|v| Term::Var(*v)).collect() };
        let at = |rel: RelId, args: Vec<Term>| -> Atom { atom(rel, args, span) };
        let prio = |vs: &[VarId]| Expr::Call {
            f: FnRef::Builtin(BuiltinFn::Prio { site }),
            args: vec![
                Expr::Construct {
                    ty: xtuple,
                    variant: None,
                    fields: vs.iter().take(nx).map(|v| Expr::Term(Term::Var(*v))).collect(),
                },
                Expr::Construct {
                    ty: ytuple,
                    variant: None,
                    fields: vs.iter().skip(nx).take(ny).map(|v| Expr::Term(Term::Var(*v))).collect(),
                },
            ],
        };
        let cost_var = |vs: &[VarId]| vs.get(nx + ny).copied();
        let ext_atom = |vs: &[VarId]| -> Option<Atom> {
            let (ext, c) = (ext?, cost_var(vs)?);
            let mut args = xs(vs);
            args.push(Term::Var(c));
            Some(at(ext, args))
        };
        let head = |rel: RelId, args: Vec<Term>| Head {
            rel,
            args: args.into_iter().map(HeadArg::Term).collect(),
            mode: HeadMode::Insert,
        };
        if let Some((held, keep, kept)) = sticky {
            // s$keep(X̄, Ȳ) :- s$held(X̄, Ȳ), s$cand(X̄, Ȳ, C).   s$kept(X̄) :- s$keep(X̄, Ȳ).
            let label = self.label(format!("{}{tag}$keep", names.base));
            let mut rb = self.b.rule(RuleKind::Deductive, label, span);
            let vs = vars(&mut rb)?;
            rb.lit(Literal::Pos(at(held, xy(&vs))));
            rb.lit(Literal::Pos(at(cand, terms(&vs))));
            rb.head(head(keep, xy(&vs)), role_id).map_err(ir)?;
            let label = self.label(format!("{}{tag}$kept", names.base));
            let mut rb = self.b.rule(RuleKind::Deductive, label, span);
            let vs = vars(&mut rb)?;
            rb.lit(Literal::Pos(at(keep, xy(&vs))));
            rb.head(head(kept, xs(&vs)), role_id).map_err(ir)?;
            // s$chosen(X̄, Ȳ) :- s$keep(X̄, Ȳ).   s$held(X̄, Ȳ)@next :- s$chosen(X̄, Ȳ).
            let label = self.label(format!("{}{tag}$chosen#kept", names.base));
            let mut rb = self.b.rule(RuleKind::Deductive, label, span);
            let vs = vars(&mut rb)?;
            rb.lit(Literal::Pos(at(keep, xy(&vs))));
            rb.head(head(chosen, xy(&vs)), role_id).map_err(ir)?;
            let label = self.label(format!("{}{tag}$held", names.base));
            let mut rb = self.b.rule(RuleKind::Inductive, label, span);
            let vs = vars(&mut rb)?;
            rb.lit(Literal::Pos(at(chosen, xy(&vs))));
            rb.head(head(held, xy(&vs)), role_id).map_err(ir)?;
        }
        let not_kept = |vs: &[VarId]| sticky.map(|(_, _, kept)| Literal::Neg(at(kept, xs(vs))));
        // The extreme cost per group.
        if let (Some(ext), Some((_, _, most))) = (ext, &cost) {
            let label = self.label(format!("{}{tag}$ext", names.base));
            let mut rb = self.b.rule(RuleKind::Deductive, label, span);
            let vs = vars(&mut rb)?;
            rb.lit(Literal::Pos(at(cand, terms(&vs))));
            if let Some(l) = not_kept(&vs) {
                rb.lit(l);
            }
            let c = cost_var(&vs).ok_or_else(|| internal_error!("a choice cost without its variable"))?;
            let mut args: Vec<HeadArg> = xs(&vs).into_iter().map(HeadArg::Term).collect();
            args.push(HeadArg::Agg(AggCall {
                func: if *most { AggFunc::Max } else { AggFunc::Min },
                args: vec![Term::Var(c)],
                order: None,
            }));
            rb.head(
                Head {
                    rel: ext,
                    args,
                    mode: HeadMode::Insert,
                },
                role_id,
            )
            .map_err(ir)?;
        }
        // The least priority per group.
        let label = self.label(format!("{}{tag}$pmin", names.base));
        let mut rb = self.b.rule(RuleKind::Deductive, label, span);
        let vs = vars(&mut rb)?;
        let p = rb.var(Symbol::intern("P"), prio_ty).map_err(ir)?;
        rb.lit(Literal::Pos(at(cand, terms(&vs))));
        if let Some(l) = not_kept(&vs) {
            rb.lit(l);
        }
        if let Some(a) = ext_atom(&vs) {
            rb.lit(Literal::Pos(a));
        }
        rb.lit(Literal::Bind {
            pat: Pattern::Var(p),
            expr: prio(&vs),
        });
        let mut args: Vec<HeadArg> = xs(&vs).into_iter().map(HeadArg::Term).collect();
        args.push(HeadArg::Agg(AggCall {
            func: AggFunc::Min,
            args: vec![Term::Var(p)],
            order: None,
        }));
        rb.head(
            Head {
                rel: pmin,
                args,
                mode: HeadMode::Insert,
            },
            role_id,
        )
        .map_err(ir)?;
        // The chosen candidate of each group.
        let label = self.label(format!("{}{tag}$chosen", names.base));
        let mut rb = self.b.rule(RuleKind::Deductive, label, span);
        let vs = vars(&mut rb)?;
        let p = rb.var(Symbol::intern("P"), prio_ty).map_err(ir)?;
        rb.lit(Literal::Pos(at(cand, terms(&vs))));
        if let Some(l) = not_kept(&vs) {
            rb.lit(l);
        }
        if let Some(a) = ext_atom(&vs) {
            rb.lit(Literal::Pos(a));
        }
        let mut pargs = xs(&vs);
        pargs.push(Term::Var(p));
        rb.lit(Literal::Pos(at(pmin, pargs)));
        rb.lit(Literal::Guard(Expr::Binary {
            op: BinOp::Eq,
            lhs: Box::new(Expr::Term(Term::Var(p))),
            rhs: Box::new(prio(&vs)),
        }));
        rb.head(head(chosen, xy(&vs)), role_id).map_err(ir)?;
        let spec = ChooseSpec {
            site,
            candidates: cand,
            group: group.iter().map(|i| col_idx(*i)).collect(),
            choice: (nx..nx + ny).map(col_idx).collect(),
            policy: match cost {
                None => ChoosePolicy::Priority,
                Some((_, _, false)) => ChoosePolicy::Least { cost: col_idx(nx + ny) },
                Some((_, _, true)) => ChoosePolicy::Most { cost: col_idx(nx + ny) },
            },
            sticky: sticky.map(|(held, _, _)| StickySpec {
                held,
                release: None,
                durable: false,
            }),
            overrides: None,
            output: chosen,
        };
        self.b
            .set_construct_kind(construct, ConstructKind::Choose(spec))
            .map_err(ir)?;
        self.b.end_construct(construct).map_err(ir)?;
        d.lits.push(Literal::Pos(at(chosen, x.into_iter().chain(y).collect())));
        Ok(())
    }
}
