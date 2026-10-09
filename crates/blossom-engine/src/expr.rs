//! Expression evaluation over `Value` (LANGUAGE §9, §15.1). Arithmetic is checked: overflow and division by zero are
//! the runtime hard error BLSR004; division truncates toward zero. Written independently of the oracle (ARCH-16): the
//! differential suite compares the two.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use blossom_base::{ParamId, RoleId, internal_error};
use blossom_ir::core::{
    BinOp, BuiltinFn, BuiltinScalar, CollKind, Expr, FnRef, GenSource, LatOpImpl, LatOpRef, MajorityDomain, Pattern,
    Program, RangeKind, Term, UnOp,
};
use blossom_ir::tick::EvalError;
use blossom_lattice::{Kind, LatticeError};
use blossom_value::float;
use blossom_value::time::{Instant, NodeId, Tick};
use blossom_value::value::{IntValue, LatValue};
use blossom_value::{Seed, TypeDef, Value};

/// A runtime hard error of an expression, before it is attributed to a rule and a tick.
#[derive(Debug)]
pub(crate) enum ExprError {
    /// BLSR004.
    Arithmetic(String),
    /// BLSR006: an `LPoint` conflict.
    Conflict(String),
    /// BLSR010: a host function refused its input.
    Refused(String),
    /// BLSR012: a pure function's evaluation exceeds its step budget.
    Budget(String),
    /// Anything else: a missing feature or a bug.
    Eval(EvalError),
}

impl ExprError {
    /// A copy of a program error (the kind a valuation raises; an evaluator error is never repeated).
    pub(crate) fn duplicate(&self) -> ExprError {
        match self {
            ExprError::Arithmetic(m) => ExprError::Arithmetic(m.clone()),
            ExprError::Conflict(m) => ExprError::Conflict(m.clone()),
            ExprError::Refused(m) => ExprError::Refused(m.clone()),
            ExprError::Budget(m) => ExprError::Budget(m.clone()),
            ExprError::Eval(e) => bug(format!("an evaluator error repeated per valuation: {e}")),
        }
    }
}

impl From<EvalError> for ExprError {
    fn from(e: EvalError) -> ExprError {
        ExprError::Eval(e)
    }
}

impl From<LatticeError> for ExprError {
    fn from(e: LatticeError) -> ExprError {
        match e {
            LatticeError::Conflict(..) => ExprError::Conflict(e.to_string()),
            LatticeError::Arithmetic(m) | LatticeError::Domain(m) => ExprError::Arithmetic(m),
            LatticeError::Shape(m) => bug(format!("a lattice operation on the wrong values: {m}")),
        }
    }
}

pub(crate) type ExprResult<T> = Result<T, ExprError>;

pub(crate) fn bug(msg: String) -> ExprError {
    ExprError::Eval(internal_error!("{msg}").into())
}

/// What an expression reads besides its variables: the program, the node and the tick.
pub(crate) struct Ctx<'a> {
    pub program: &'a Program,
    pub node: NodeId,
    pub incarnation: u64,
    pub tick: Tick,
    pub now: Instant,
    pub shared: &'a Shared,
    /// The step budget of the function evaluation in progress (BLSR012).
    pub fuel: Fuel,
    /// The expression nodes evaluated with this context (the work measure of `Engine::work_by_rule`).
    pub steps: std::cell::Cell<u64>,
    /// When the engine profiles functions: each function's work, and the steps the calls inside the call in
    /// progress took (what its own steps leave out).
    pub fn_work: Option<&'a std::cell::RefCell<BTreeMap<blossom_base::FnId, blossom_ir::tick::FnWork>>>,
    pub callee_steps: std::cell::Cell<u64>,
    /// The bytes of blobs created before this tick.
    pub blobs: &'a dyn blossom_value::BlobSource,
    /// The blobs this tick created, with their bytes.
    pub new_blobs: &'a std::cell::RefCell<BTreeMap<blossom_value::BlobRef, std::sync::Arc<[u8]>>>,
    /// The earliest later instant at which a comparison with `now()` evaluated so far would come out the other way
    /// (`Plan::skip`: a rule that reads the time only so is not evaluated again before then, if nothing it reads
    /// changes).
    pub flips_at: std::cell::Cell<Option<blossom_value::time::Instant>>,
    /// Whether the evaluation read the time, the tick or randomness otherwise than by ordering `now()` against
    /// an instant (then its outcome may differ at the next tick, whatever `flips_at` says).
    pub reads_time: std::cell::Cell<bool>,
}

impl Ctx<'_> {
    /// The bytes of `b`: created this tick, or before it.
    pub(crate) fn blob(&self, b: &blossom_value::BlobRef) -> Option<std::sync::Arc<[u8]>> {
        if let Some(x) = self.new_blobs.borrow().get(b) {
            return Some(x.clone());
        }
        self.blobs.get(b)
    }
}

/// The step budget of the function evaluation in progress: how many calls are open, the steps it has left
/// (`FN_STEP_BUDGET` when the outermost call starts), and whether the innermost open call is unmetered (a format's
/// generated functions, which are bounded by their input, LANGUAGE §16.1). Unmetered calls spend nothing; every
/// metered one, however deep and whatever it is inside, spends from the one budget. One step per closure application
/// and per element of a `range` built as a vector.
#[derive(Default)]
pub(crate) struct Fuel(std::cell::Cell<(u32, u64, bool)>);

/// Whether the call a call was made from was unmetered, restored when it returns.
#[derive(Clone, Copy)]
pub(crate) struct Saved(bool);

impl Fuel {
    /// Enters a call of a pure function: the outermost call starts the budget.
    pub(crate) fn enter(&self, metered: bool) -> Saved {
        let (depth, left, free) = self.0.get();
        let left = if depth == 0 {
            blossom_ir::core::FN_STEP_BUDGET
        } else {
            left
        };
        self.0.set((depth.saturating_add(1), left, !metered));
        Saved(free)
    }

    /// Leaves a call (what it spent stays spent).
    pub(crate) fn exit(&self, saved: Saved) {
        let (depth, left, _free) = self.0.get();
        self.0.set((depth.saturating_sub(1), left, saved.0));
    }

    /// Spends `n` steps; outside any function, a single `range` has the whole budget to itself.
    pub(crate) fn spend(&self, n: u64) -> ExprResult<()> {
        let (depth, left, free) = self.0.get();
        if depth > 0 && free {
            return Ok(());
        }
        let have = if depth == 0 {
            blossom_ir::core::FN_STEP_BUDGET
        } else {
            left
        };
        let Some(rest) = have.checked_sub(n) else {
            return Err(ExprError::Budget(format!(
                "a function evaluation exceeds its step budget of {} steps",
                blossom_ir::core::FN_STEP_BUDGET
            )));
        };
        if depth > 0 {
            self.0.set((depth, rest, false));
        }
        Ok(())
    }
}

/// Per-program facts every tick's expressions read.
pub(crate) struct Shared {
    pub params: BTreeMap<ParamId, Value>,
    /// The choice seed σc (SEM-084).
    pub choice: Option<Seed>,
    /// Each node's seed σn, by node id.
    pub node_seeds: Vec<Seed>,
    /// This node's seed when it is a client member (its id is outside the deployment) or a keyed member (seeded by
    /// its member name).
    pub own_seed: Option<Seed>,
    /// Each node's role, by node id.
    pub roles: Vec<Option<RoleId>>,
    /// The keyed members the host gave node ids.
    pub members: Arc<blossom_ir::members::Members>,
    /// Each role's name, by id (a keyed member's value carries it).
    pub role_names: Vec<Arc<str>>,
    /// The built-in lattice of each declared lattice, by lattice id.
    pub kinds: Vec<Option<Kind>>,
    /// The host functions of the program's `extern fn`s, bound when the engine was built.
    pub externs: Arc<blossom_value::ExternRegistry>,
    /// Each node's name, by node id (`to_string` writes nodes by name).
    pub node_names: Vec<Arc<str>>,
}

impl Shared {
    /// Node `n`'s seed σn.
    pub fn seed_of(&self, n: blossom_value::time::NodeId) -> Option<Seed> {
        if n.is_client() || self.members.get(n).is_some() {
            self.own_seed
        } else {
            self.node_seeds.get(n.0 as usize).copied()
        }
    }

    pub fn role_size(&self, r: RoleId) -> u64 {
        self.roles.iter().filter(|x| **x == Some(r)).count() as u64
    }
}

/// The variables an expression reads. Its binders (`let`, a `match` arm, a closure's parameters) bind their
/// variables in place and restore what the slots held when they end, so evaluation never copies the frame; a frame
/// borrowed from a rule's valuation is copied once, at its first binding.
pub(crate) struct Frame<'a> {
    slots: std::borrow::Cow<'a, [Option<Value>]>,
    /// What the binders in progress displaced, innermost last.
    saved: Vec<(usize, Option<Value>)>,
    /// The slots the `match` arms in progress bound (each was unbound before).
    newly: Vec<usize>,
}

impl<'a> Frame<'a> {
    pub(crate) fn borrowed(slots: &'a [Option<Value>]) -> Self {
        Frame {
            slots: std::borrow::Cow::Borrowed(slots),
            saved: Vec::new(),
            newly: Vec::new(),
        }
    }

    pub(crate) fn owned(slots: Vec<Option<Value>>) -> Frame<'static> {
        Frame {
            slots: std::borrow::Cow::Owned(slots),
            saved: Vec::new(),
            newly: Vec::new(),
        }
    }

    pub(crate) fn slots(&self) -> &[Option<Value>] {
        &self.slots
    }

    /// Where the bindings made from now on start (for `restore`).
    pub(crate) fn mark(&self) -> usize {
        self.saved.len()
    }

    /// Binds slot `i` to `v`, remembering what it held.
    pub(crate) fn bind(&mut self, i: usize, v: Value) -> ExprResult<()> {
        let slot = self
            .slots
            .to_mut()
            .get_mut(i)
            .ok_or_else(|| bug(format!("binding slot {i} outside the frame")))?;
        let old = slot.replace(v);
        self.saved.push((i, old));
        Ok(())
    }

    /// Undoes the bindings made since `mark`, innermost first.
    pub(crate) fn restore(&mut self, mark: usize) {
        while self.saved.len() > mark {
            let Some((i, old)) = self.saved.pop() else { break };
            if let Some(slot) = self.slots.to_mut().get_mut(i) {
                *slot = old;
            }
        }
    }

    /// Matches `v` against `pat` as a `match` arm does, recording the slots it binds from `newly`'s current length.
    fn match_arm(&mut self, cx: &Ctx<'_>, pat: &Pattern, v: &Value) -> ExprResult<bool> {
        let Frame { slots, newly, .. } = self;
        matches(cx, slots.to_mut(), pat, v, newly)
    }

    /// Unbinds the slots `match` arms bound since `mark` (each was unbound before).
    fn unbind_arm(&mut self, mark: usize) {
        if self.newly.len() <= mark {
            return;
        }
        let Frame { slots, newly, .. } = self;
        let slots = slots.to_mut();
        for i in newly.drain(mark..) {
            if let Some(slot) = slots.get_mut(i) {
                *slot = None;
            }
        }
    }
}

pub(crate) fn term(cx: &Ctx<'_>, env: &[Option<Value>], t: &Term) -> ExprResult<Value> {
    match t {
        Term::Var(v) => env
            .get(v.index())
            .cloned()
            .flatten()
            .ok_or_else(|| bug(format!("variable {v:?} read before it is bound"))),
        Term::Const(c) => cx
            .program
            .consts
            .get(*c)
            .cloned()
            .ok_or_else(|| bug(format!("unknown constant {c:?}"))),
        Term::Wild => Err(bug("`_` evaluated as a value".into())),
    }
}

pub(crate) fn truth(v: &Value) -> ExprResult<bool> {
    match v {
        Value::Bool(b) => Ok(*b),
        other => Err(bug(format!("a condition evaluated to {other:?}"))),
    }
}

/// A missing feature, as an expression error.
macro_rules! unimplemented {
    ($feature:literal, $what:expr) => {
        ExprError::Eval(blossom_base::unimplemented_error!($feature, "{} in the engine", $what).into())
    };
}

pub(crate) fn eval(cx: &Ctx<'_>, env: &[Option<Value>], e: &Expr) -> ExprResult<Value> {
    eval_in(cx, &mut Frame::borrowed(env), e)
}

pub(crate) fn eval_in(cx: &Ctx<'_>, env: &mut Frame<'_>, e: &Expr) -> ExprResult<Value> {
    cx.steps.set(cx.steps.get().wrapping_add(1));
    match e {
        Expr::Term(t) => term(cx, env.slots(), t),
        Expr::Param(p) => param(cx, *p),
        Expr::Scalar(s) => match s {
            BuiltinScalar::SelfNode => Ok(cx.shared.members.value(cx.node)),
            BuiltinScalar::Tick => {
                cx.reads_time.set(true);
                Ok(Value::Int(IntValue::U64(cx.tick.0)))
            }
            BuiltinScalar::Now => {
                cx.reads_time.set(true);
                Ok(Value::Instant(cx.now))
            }
            other => Err(unimplemented!("LANG-180", &format!("`${other:?}`"))),
        },
        Expr::Unary { op, arg } => {
            let v = eval_in(cx, env, arg)?;
            match (op, v) {
                (UnOp::Not, Value::Bool(b)) => Ok(Value::Bool(!b)),
                (UnOp::Neg, Value::Int(i)) => negate(i).map(Value::Int),
                (UnOp::Neg, Value::F64(f)) => Ok(Value::F64(float::canonical(-f))),
                (UnOp::BitNot, Value::Int(i)) => Ok(Value::Int(from_bits(i.ty(), !to_bits(i)))),
                (op, v) => Err(bug(format!("{op:?} applied to {v:?}"))),
            }
        }
        Expr::Binary { op, lhs, rhs } => match op {
            BinOp::And => {
                if !truth(&eval_in(cx, env, lhs)?)? {
                    return Ok(Value::Bool(false));
                }
                Ok(Value::Bool(truth(&eval_in(cx, env, rhs)?)?))
            }
            BinOp::Or => {
                if truth(&eval_in(cx, env, lhs)?)? {
                    return Ok(Value::Bool(true));
                }
                Ok(Value::Bool(truth(&eval_in(cx, env, rhs)?)?))
            }
            _ if is_read(lhs) && is_read(rhs) => {
                let l = read(cx, env, lhs)?;
                let r = read(cx, env, rhs)?;
                binary_ref(op, l, r)
            }
            _ => {
                // `now()` ordered against an instant is read here, its flip noted (not as a free read of the time).
                let timed = matches!(op, BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge) && is_now(lhs) != is_now(rhs);
                let l = if timed && is_now(lhs) {
                    now_operand(cx, lhs)
                } else {
                    eval_in(cx, env, lhs)?
                };
                let r = if timed && is_now(rhs) {
                    now_operand(cx, rhs)
                } else {
                    eval_in(cx, env, rhs)?
                };
                if timed {
                    note_flip(cx, op, lhs, rhs, &l, &r);
                }
                binary(op, l, r)
            }
        },
        Expr::If { cond, then, els } => {
            if truth(&eval_in(cx, env, cond)?)? {
                eval_in(cx, env, then)
            } else {
                eval_in(cx, env, els)
            }
        }
        Expr::Construct { ty, variant, fields } => {
            let mut vs = Vec::with_capacity(fields.len());
            for f in fields {
                vs.push(eval_in(cx, env, f)?);
            }
            match (cx.program.types.get(*ty), variant) {
                (Some(TypeDef::Option(_)), Some(1)) => match <[Value; 1]>::try_from(vs) {
                    Ok([v]) => Ok(Value::Option(Some(Arc::new(v)))),
                    Err(_) => Err(bug("`Some` with the wrong number of values".into())),
                },
                (Some(TypeDef::Option(_)), Some(0)) => Ok(Value::Option(None)),
                (Some(TypeDef::Tuple(_)), None) => Ok(Value::Tuple(vs.into())),
                (Some(TypeDef::Struct(_)), None) => Ok(Value::Struct(vs.into())),
                (Some(TypeDef::Enum(_)), Some(v)) => Ok(Value::Enum {
                    variant: *v,
                    fields: vs.into(),
                }),
                (other, v) => Err(bug(format!("constructing {other:?} variant {v:?}"))),
            }
        }
        Expr::Field { base, index } if is_read(base) => match read(cx, env, base)? {
            Value::Tuple(fs) | Value::Struct(fs) => fs
                .get(*index as usize)
                .cloned()
                .ok_or_else(|| bug(format!("field {index} out of range"))),
            other => Err(bug(format!("field {index} of {other:?}"))),
        },
        Expr::Field { base, index } => match eval_in(cx, env, base)? {
            Value::Tuple(fs) | Value::Struct(fs) => fs
                .get(*index as usize)
                .cloned()
                .ok_or_else(|| bug(format!("field {index} out of range"))),
            other => Err(bug(format!("field {index} of {other:?}"))),
        },
        Expr::Match { scrut, arms } => {
            let v = eval_in(cx, env, scrut)?;
            for (pat, guard, body) in arms {
                // An arm's bindings are local to it: unbound again after it, whether it matched or not.
                let mark = env.newly.len();
                let r = arm(cx, env, pat, guard.as_ref(), body, &v);
                env.unbind_arm(mark);
                if let Some(out) = r? {
                    return Ok(out);
                }
            }
            Err(bug(format!("no match arm matched {v:?}")))
        }
        Expr::Call {
            f: FnRef::Builtin(BuiltinFn::Lib(f)),
            args,
        } => crate::func::library(cx, env, *f, args),
        Expr::Call {
            f: FnRef::Builtin(f),
            args,
        } => {
            if random_builtin(f) {
                cx.reads_time.set(true);
            }
            builtin(cx, env, f, args)
        }
        Expr::Call { f: FnRef::Fn(f), args } => crate::func::call(cx, env, *f, args),
        Expr::Collection { kind, elems } => {
            let mut vs = Vec::with_capacity(elems.len());
            for x in elems {
                vs.push(eval_in(cx, env, x)?);
            }
            Ok(match kind {
                CollKind::Vec => Value::Vec(vs.into()),
                CollKind::Set => Value::Set(Arc::new(vs.into_iter().collect::<BTreeSet<Value>>())),
                CollKind::Map => {
                    let mut m = BTreeMap::new();
                    for pair in vs {
                        let Value::Tuple(kv) = pair else {
                            return Err(bug(format!("a map entry {pair:?}")));
                        };
                        let [k, v] = &*kv else {
                            return Err(bug(format!("a map entry {kv:?}")));
                        };
                        m.insert(k.clone(), v.clone());
                    }
                    Value::Map(Arc::new(m))
                }
            })
        }
        Expr::Lattice { op, args } => match lattice_op(cx, op)? {
            LatEval::Builtin(kind, lop) => {
                let mut vs = Vec::with_capacity(args.len());
                for a in args {
                    vs.push(eval_in(cx, env, a)?);
                }
                Ok(kind.eval(lop, &vs)?)
            }
            LatEval::Field(kind, i) => match args.as_slice() {
                [a] => match eval_in(cx, env, a)? {
                    Value::Lattice(l) => Ok(Value::Lattice(kind.field(&l, i)?)),
                    other => Err(bug(format!("a field read of {other:?}"))),
                },
                _ => Err(bug("a field read takes one argument".into())),
            },
            LatEval::Method(f) => crate::func::call(cx, env, f, args),
        },
        Expr::Let { pat, value, body } => crate::func::let_expr(cx, env, pat, value, body),
        Expr::Closure { .. } => Err(bug("a closure evaluated outside a combinator".into())),
        Expr::Typed { expr, .. } => eval_in(cx, env, expr),
    }
}

/// Whether `e` is `now()`.
pub(crate) fn is_now(e: &Expr) -> bool {
    match e {
        Expr::Scalar(BuiltinScalar::Now) => true,
        Expr::Typed { expr, .. } => is_now(expr),
        _ => false,
    }
}

/// The value of a `now()` operand of an ordering (`is_now`), counting its nodes as `eval_in` would; not a free
/// read of the time (`Ctx::reads_time`): the ordering notes its flip.
fn now_operand(cx: &Ctx<'_>, e: &Expr) -> Value {
    cx.steps.set(cx.steps.get().wrapping_add(1));
    match e {
        Expr::Typed { expr, .. } => now_operand(cx, expr),
        _ => Value::Instant(cx.now),
    }
}

/// For an ordering of `now()` against an instant: records in `cx.flips_at` when its outcome changes, if later.
fn note_flip(cx: &Ctx<'_>, op: &BinOp, lhs: &Expr, rhs: &Expr, l: &Value, r: &Value) {
    use blossom_value::time::Instant;
    // With `now()` on the left: `now op e`.
    let (op, e) = match (is_now(lhs), is_now(rhs), l, r) {
        (true, false, _, Value::Instant(e)) => (op.clone(), *e),
        (false, true, Value::Instant(e), _) => match op {
            BinOp::Lt => (BinOp::Gt, *e),
            BinOp::Le => (BinOp::Ge, *e),
            BinOp::Gt => (BinOp::Lt, *e),
            BinOp::Ge => (BinOp::Le, *e),
            _ => return,
        },
        _ => return,
    };
    let now = cx.now;
    let after = Instant(e.0.saturating_add(1));
    let at = match op {
        // `now < e` turns false at `e`; `now >= e` turns true at `e`.
        BinOp::Lt | BinOp::Ge if now < e => e,
        // `now <= e` turns false, and `now > e` true, just past `e`.
        BinOp::Le | BinOp::Gt if now <= e => after,
        _ => return,
    };
    if cx.flips_at.get().is_none_or(|t| at < t) {
        cx.flips_at.set(Some(at));
    }
}

/// Whether `e` only reads a value that exists already: a variable, a constant, or a field of one. `read` takes it
/// without copying it.
fn is_read(e: &Expr) -> bool {
    match e {
        Expr::Term(Term::Var(_) | Term::Const(_)) => true,
        Expr::Field { base, .. } | Expr::Typed { expr: base, .. } => is_read(base),
        _ => false,
    }
}

/// The value an `is_read` expression reads, in place (counting its nodes as `eval_in` would).
fn read<'v>(cx: &'v Ctx<'_>, env: &'v Frame<'_>, e: &'v Expr) -> ExprResult<&'v Value> {
    cx.steps.set(cx.steps.get().wrapping_add(1));
    match e {
        Expr::Term(Term::Var(v)) => env
            .slots()
            .get(v.index())
            .and_then(Option::as_ref)
            .ok_or_else(|| bug(format!("variable {v:?} read before it is bound"))),
        Expr::Term(Term::Const(c)) => cx
            .program
            .consts
            .get(*c)
            .ok_or_else(|| bug(format!("unknown constant {c:?}"))),
        Expr::Field { base, index } => match read(cx, env, base)? {
            Value::Tuple(fs) | Value::Struct(fs) => fs
                .get(*index as usize)
                .ok_or_else(|| bug(format!("field {index} out of range"))),
            other => Err(bug(format!("field {index} of {other:?}"))),
        },
        Expr::Typed { expr, .. } => read(cx, env, expr),
        other => Err(bug(format!("{other:?} read in place"))),
    }
}

/// `binary` over operands read in place: comparisons copy nothing; other operators take copies (numbers, mostly).
fn binary_ref(op: &BinOp, l: &Value, r: &Value) -> ExprResult<Value> {
    use BinOp::*;
    match op {
        Eq => Ok(Value::Bool(l == r)),
        Ne => Ok(Value::Bool(l != r)),
        CanonLt => Ok(Value::Bool(l < r)),
        CanonLe => Ok(Value::Bool(l <= r)),
        _ => binary(op, l.clone(), r.clone()),
    }
}

/// One `match` arm against `v`: its value if its pattern matches and its guard holds.
fn arm(
    cx: &Ctx<'_>,
    env: &mut Frame<'_>,
    pat: &Pattern,
    guard: Option<&Expr>,
    body: &Expr,
    v: &Value,
) -> ExprResult<Option<Value>> {
    if !env.match_arm(cx, pat, v)? {
        return Ok(None);
    }
    if let Some(g) = guard
        && !truth(&eval_in(cx, env, g)?)?
    {
        return Ok(None);
    }
    eval_in(cx, env, body).map(Some)
}

fn param(cx: &Ctx<'_>, p: ParamId) -> ExprResult<Value> {
    if let Some(v) = cx.shared.params.get(&p) {
        return Ok(v.clone());
    }
    let decl = cx
        .program
        .params
        .get(p)
        .ok_or_else(|| bug(format!("parameter {p:?} is not declared")))?;
    let Some(c) = decl.default else {
        return Err(ExprError::Eval(EvalError::Unbound(decl.name.to_string())));
    };
    cx.program
        .consts
        .get(c)
        .cloned()
        .ok_or_else(|| bug(format!("the default of {} is not a constant", decl.name)))
}

/// How a lattice operation is evaluated (its catalogue entry's implementation).
enum LatEval<'s> {
    Builtin(&'s Kind, blossom_lattice::Op),
    Field(&'s Kind, usize),
    Method(blossom_base::FnId),
}

fn lattice_op<'s>(cx: &'s Ctx<'_>, op: &LatOpRef) -> ExprResult<LatEval<'s>> {
    let kind = cx
        .shared
        .kinds
        .get(op.lattice.index())
        .and_then(Option::as_ref)
        .ok_or_else(|| unimplemented!("LANG-124", &format!("lattice {:?}", op.lattice)))?;
    let decl = cx
        .program
        .lattices
        .get(op.lattice)
        .and_then(|l| l.ops.iter().find(|d| d.name == op.op))
        .ok_or_else(|| bug(format!("`{}` is not in the catalogue of {kind:?}", op.op)))?;
    Ok(match decl.imp {
        LatOpImpl::Builtin => LatEval::Builtin(
            kind,
            blossom_lattice::Op::from_name(kind, op.op.as_str())
                .ok_or_else(|| bug(format!("`{}` is not an operation of {kind:?}", op.op)))?,
        ),
        LatOpImpl::Field(i) => LatEval::Field(kind, i as usize),
        LatOpImpl::Method(f) => LatEval::Method(f),
    })
}

fn builtin(cx: &Ctx<'_>, env: &mut Frame<'_>, f: &BuiltinFn, args: &[Expr]) -> ExprResult<Value> {
    let mut arg = |i: usize| -> ExprResult<Value> {
        let e = args
            .get(i)
            .ok_or_else(|| bug(format!("{f:?} is missing argument {i}")))?;
        eval_in(cx, env, e)
    };
    match f {
        BuiltinFn::Len => {
            let n = match arg(0)? {
                Value::Str(s) => s.len(),
                Value::Bytes(b) => b.len(),
                Value::Blob(b) => usize::try_from(b.len).unwrap_or(usize::MAX),
                Value::Vec(v) => v.len(),
                Value::Set(s) => s.len(),
                Value::Map(m) => m.len(),
                other => return Err(bug(format!("`len` of {other:?}"))),
            };
            Ok(Value::Int(IntValue::U64(n as u64)))
        }
        BuiltinFn::Size { role } => Ok(Value::Int(IntValue::U64(cx.shared.role_size(*role)))),
        BuiltinFn::Named { role } => match arg(0)? {
            Value::Str(key) => {
                let name = cx
                    .shared
                    .role_names
                    .get(role.index())
                    .cloned()
                    .ok_or_else(|| bug(format!("`named` of an unknown role {role:?}")))?;
                Ok(Value::Member(blossom_value::time::MemberRef::new(*role, name, key)))
            }
            other => Err(bug(format!("`named` of {other:?}"))),
        },
        BuiltinFn::MemberKey => match arg(0)? {
            Value::Member(m) => Ok(Value::Str(m.key)),
            other => Err(bug(format!("the key of {other:?}, not a keyed member"))),
        },
        BuiltinFn::IntCast(to) => match arg(0)? {
            // Truncated toward zero; NaN, infinite or out of range is BLSR004 (LANGUAGE §5.1).
            Value::F64(x) => float::to_int(x, *to)
                .map(Value::Int)
                .ok_or_else(|| ExprError::Arithmetic(format!("{x:?} as {} is out of range", to.name()))),
            Value::Int(i) => {
                let wide = match i {
                    IntValue::U128(u) => i128::try_from(u).ok(),
                    other => other.to_i128(),
                };
                let cast = match (i, to) {
                    // A u128 above i128::MAX fits only a u128.
                    (IntValue::U128(u), blossom_value::types::IntTy::U128) => Some(IntValue::U128(u)),
                    _ => wide.and_then(|w| IntValue::from_i128(*to, w)),
                };
                cast.map(Value::Int)
                    .ok_or_else(|| ExprError::Arithmetic(format!("{i:?} as {} is out of range", to.name())))
            }
            other => Err(bug(format!("an integer cast of {other:?}"))),
        },
        BuiltinFn::FloatCast => match arg(0)? {
            Value::F64(x) => Ok(Value::F64(float::canonical(x))),
            Value::Int(i) => Ok(Value::F64(float::from_int(i))),
            other => Err(bug(format!("a cast to f64 of {other:?}"))),
        },
        BuiltinFn::Concat => match (arg(0)?, arg(1)?) {
            (Value::Str(a), Value::Str(b)) => Ok(Value::Str(format!("{a}{b}").into())),
            (Value::Bytes(a), Value::Bytes(b)) => Ok(Value::Bytes(a.iter().chain(b.iter()).copied().collect())),
            (Value::Vec(a), Value::Vec(b)) => Ok(Value::Vec(a.iter().chain(b.iter()).cloned().collect())),
            (a, b) => Err(bug(format!("`++` on {a:?} and {b:?}"))),
        },
        BuiltinFn::Contains => {
            let (c, x) = (arg(0)?, arg(1)?);
            Ok(Value::Bool(match &c {
                Value::Set(s) => s.contains(&x),
                Value::Vec(v) => v.contains(&x),
                Value::Map(m) => m.contains_key(&x),
                other => return Err(bug(format!("`contains` on {other:?}"))),
            }))
        }
        BuiltinFn::Prio { site } => {
            // $prio(site, X̄, Ȳ) = (PRF_σc(site, fp(X̄), fp(Ȳ)), Ȳ) (SEM-084).
            let (x, y) = (arg(0)?, arg(1)?);
            let seed = cx
                .shared
                .choice
                .as_ref()
                .ok_or_else(|| bug("a seeded choice, but the engine was given no seed".into()))?;
            let key = cx
                .program
                .sites
                .get(*site)
                .map(|s| s.key)
                .ok_or_else(|| bug(format!("unknown site {site:?}")))?;
            let p = blossom_value::prf::prf(seed, "prio", &[fingerprint(&x)?, fingerprint(&y)?], &[key])
                .map_err(|e| bug(format!("the PRF: {e}")))?;
            Ok(Value::Tuple(vec![Value::Int(IntValue::U64(p)), y].into()))
        }
        BuiltinFn::Error { .. } => match arg(0)? {
            Value::Str(s) => Err(ExprError::Refused(s.to_string())),
            other => Err(bug(format!("`error` of {other:?}"))),
        },
        BuiltinFn::Hash64 => {
            let fp = blossom_value::fp::fingerprint(&arg(0)?).map_err(|e| bug(format!("`hash64`: {e}")))?;
            Ok(Value::Int(IntValue::U64(fp.0)))
        }
        BuiltinFn::Rand => {
            let mut key = Vec::new();
            for i in 0..args.len() {
                key.push(arg(i)?);
            }
            rand(cx, &key)
        }
        BuiltinFn::RandFloat => {
            let mut key = Vec::new();
            for i in 0..args.len() {
                key.push(arg(i)?);
            }
            match rand(cx, &key)? {
                // `rand_float(k…)` is `rand(k…)`'s draw, as a float in [0, 1).
                Value::Int(IntValue::U64(bits)) => Ok(Value::F64(float::unit_from_bits(bits))),
                other => Err(bug(format!("rand gave {other:?}"))),
            }
        }
        BuiltinFn::RandRange => {
            let (lo, hi) = (arg(0)?, arg(1)?);
            let mut key = Vec::new();
            for i in 2..args.len() {
                key.push(arg(i)?);
            }
            rand_range(cx, &lo, &hi, &key)
        }
        BuiltinFn::ToString { ty } => Ok(Value::Str(Arc::from(blossom_ir::printer::to_string_text(
            cx.program,
            &arg(0)?,
            *ty,
            &cx.shared.node_names,
        )))),
        BuiltinFn::Majority { domain } => {
            let MajorityDomain::Role(role) = domain else {
                return Err(unimplemented!("LANG-113", "`majority` over a relation"));
            };
            let members = match arg(0)? {
                Value::Lattice(LatValue::Set(xs)) | Value::Set(xs) => xs
                    .iter()
                    .filter(|v| {
                        matches!(v, Value::Node(n) if cx.shared.roles.get(n.0 as usize).copied().flatten() == Some(*role))
                    })
                    .count() as u64,
                Value::Lattice(LatValue::Bottom) => 0,
                other => return Err(bug(format!("`majority` of {other:?}"))),
            };
            Ok(Value::Bool(members > cx.shared.role_size(*role) / 2))
        }
        other => Err(unimplemented!("LANG-180", &format!("the built-in {other:?}"))),
    }
}

fn fingerprint(v: &Value) -> ExprResult<blossom_value::fp::Fingerprint> {
    blossom_value::fp::fingerprint(v).map_err(|e| bug(format!("fingerprinting {v:?}: {e}")))
}

/// `rand(k…)`: `PRF_σn("rand", fp(k̄), incarnation, tick)` (LANGUAGE §15.1).
fn rand(cx: &Ctx<'_>, key: &[Value]) -> ExprResult<Value> {
    let seed = cx
        .shared
        .seed_of(cx.node)
        .ok_or_else(|| bug(format!("a `rand` draw on node {}, which has no seed", cx.node.0)))?;
    let fp = blossom_value::fp::fingerprint_row(key).map_err(|e| bug(format!("fingerprinting a rand key: {e}")))?;
    let x = blossom_value::prf::prf(&seed, "rand", &[fp], &[cx.incarnation, cx.tick.0])
        .map_err(|e| bug(format!("rand: {e}")))?;
    Ok(Value::Int(IntValue::U64(x)))
}

/// `lo + PRF_σn("rand", fp(k̄), incarnation, tick, attempt) mod span`, redrawing from the incomplete last span so the
/// result is unbiased (LANGUAGE §15.1).
fn rand_range(cx: &Ctx<'_>, lo: &Value, hi: &Value, key: &[Value]) -> ExprResult<Value> {
    let seed = cx
        .shared
        .seed_of(cx.node)
        .ok_or_else(|| bug(format!("a `rand` draw on node {}, which has no seed", cx.node.0)))?;
    let fp = blossom_value::fp::fingerprint_row(key).map_err(|e| bug(format!("fingerprinting a rand key: {e}")))?;
    let draw = |span: u128| {
        blossom_value::prf::uniform_below(&seed, "rand", &[fp], &[cx.incarnation, cx.tick.0], span)
            .map_err(|e| bug(format!("rand: {e}")))
    };
    let empty = |l: &dyn std::fmt::Display, h: &dyn std::fmt::Display| {
        ExprError::Arithmetic(format!("rand_range: the range [{l}, {h}) is empty"))
    };
    match (lo, hi) {
        // `u128` bounds may exceed `i128`: they draw in `u128`.
        (Value::Int(IntValue::U128(l)), Value::Int(IntValue::U128(h))) => {
            if h <= l {
                return Err(empty(l, h));
            }
            Ok(Value::Int(IntValue::U128(l + draw(h - l)?)))
        }
        (Value::Duration(_), Value::Duration(_)) | (Value::Int(_), Value::Int(_)) => {
            let bound = |v: &Value| match v {
                Value::Duration(d) => Some(i128::from(d.as_nanos())),
                Value::Int(i) => i.to_i128(),
                _ => None,
            };
            let (Some(l), Some(h)) = (bound(lo), bound(hi)) else {
                return Err(bug(format!("`rand_range` over {lo:?} and {hi:?}")));
            };
            if h <= l {
                return Err(empty(&l, &h));
            }
            // An `i128` span may exceed `i128::MAX`; in two's complement it is exact as a `u128`, and so is the result.
            let span = h.cast_unsigned().wrapping_sub(l.cast_unsigned());
            let v = l.cast_unsigned().wrapping_add(draw(span)?).cast_signed();
            match lo {
                Value::Duration(_) => Ok(Value::Duration(blossom_value::time::Duration::from_nanos(
                    i64::try_from(v).map_err(|_| bug("a duration out of range".into()))?,
                ))),
                Value::Int(i) => IntValue::from_i128(i.ty(), v)
                    .map(Value::Int)
                    .ok_or_else(|| bug("a rand_range result out of its type".into())),
                _ => Err(bug(format!("`rand_range` over {lo:?}"))),
            }
        }
        (a, b) => Err(bug(format!("`rand_range` over {a:?} and {b:?}"))),
    }
}

fn negate(i: IntValue) -> ExprResult<IntValue> {
    let r = match i {
        IntValue::I8(x) => x.checked_neg().map(IntValue::I8),
        IntValue::I16(x) => x.checked_neg().map(IntValue::I16),
        IntValue::I32(x) => x.checked_neg().map(IntValue::I32),
        IntValue::I64(x) => x.checked_neg().map(IntValue::I64),
        IntValue::I128(x) => x.checked_neg().map(IntValue::I128),
        unsigned => return Err(bug(format!("negating the unsigned {unsigned:?}"))),
    };
    r.ok_or_else(|| ExprError::Arithmetic(format!("-{i:?} overflows")))
}

fn binary(op: &BinOp, l: Value, r: Value) -> ExprResult<Value> {
    use BinOp::*;
    match op {
        Eq => Ok(Value::Bool(l == r)),
        Ne => Ok(Value::Bool(l != r)),
        CanonLt => Ok(Value::Bool(l < r)),
        CanonLe => Ok(Value::Bool(l <= r)),
        Lt | Le | Gt | Ge => {
            // Values of one ordered kind compare by the canonical order, which is numeric within a kind.
            let comparable = match (&l, &r) {
                (Value::Int(a), Value::Int(b)) => a.ty() == b.ty(),
                (Value::Duration(_), Value::Duration(_))
                | (Value::Instant(_), Value::Instant(_))
                | (Value::Str(_), Value::Str(_))
                | (Value::Bytes(_), Value::Bytes(_))
                | (Value::Node(_) | Value::Member(_), Value::Node(_) | Value::Member(_)) => true,
                _ => false,
            };
            if !comparable {
                return Err(bug(format!("ordering {l:?} {op:?} {r:?}")));
            }
            Ok(Value::Bool(match op {
                Lt => l < r,
                Le => l <= r,
                Gt => l > r,
                _ => l >= r,
            }))
        }
        Add | Sub | Mul | Div | Rem => arithmetic(op, l, r),
        And | Or => Err(bug("`&&`/`||` evaluated strictly".into())),
        BitAnd | BitOr | BitXor | Shl | Shr => match (l, r) {
            (Value::Int(a), Value::Int(b)) if a.ty() == b.ty() => bitwise(op, a, b).map(Value::Int),
            (l, r) => Err(bug(format!("{l:?} {op:?} {r:?}"))),
        },
    }
}

/// An integer as its bit pattern: two's complement over the type's width, in the low bits of a `u128`.
fn to_bits(i: IntValue) -> u128 {
    match i {
        IntValue::U8(x) => u128::from(x),
        IntValue::U16(x) => u128::from(x),
        IntValue::U32(x) => u128::from(x),
        IntValue::U64(x) => u128::from(x),
        IntValue::U128(x) => x,
        IntValue::I8(x) => u128::from(x as u8),
        IntValue::I16(x) => u128::from(x as u16),
        IntValue::I32(x) => u128::from(x as u32),
        IntValue::I64(x) => u128::from(x as u64),
        IntValue::I128(x) => x as u128,
    }
}

/// The integer of type `ty` whose bit pattern is the low bits of `b`.
fn from_bits(ty: blossom_value::types::IntTy, b: u128) -> IntValue {
    use blossom_value::types::IntTy as T;
    match ty {
        T::U8 => IntValue::U8(b as u8),
        T::U16 => IntValue::U16(b as u16),
        T::U32 => IntValue::U32(b as u32),
        T::U64 => IntValue::U64(b as u64),
        T::U128 => IntValue::U128(b),
        T::I8 => IntValue::I8(b as u8 as i8),
        T::I16 => IntValue::I16(b as u16 as i16),
        T::I32 => IntValue::I32(b as u32 as i32),
        T::I64 => IntValue::I64(b as u64 as i64),
        T::I128 => IntValue::I128(b as i128),
    }
}

/// Bitwise operators and shifts (LANGUAGE §9.12): `>>` is arithmetic on signed types; a shift count at or beyond
/// the width, or negative, is BLSR004.
fn bitwise(op: &BinOp, a: IntValue, b: IntValue) -> ExprResult<IntValue> {
    let ty = a.ty();
    let (x, y) = (to_bits(a), to_bits(b));
    let width = ty.bits();
    let count = || -> ExprResult<u32> {
        let n = b.to_i128().filter(|n| (0..i128::from(width)).contains(n));
        n.map(|n| n as u32)
            .ok_or_else(|| ExprError::Arithmetic(format!("a shift by {b:?} of a {}-bit integer", width)))
    };
    Ok(match op {
        BinOp::BitAnd => from_bits(ty, x & y),
        BinOp::BitOr => from_bits(ty, x | y),
        BinOp::BitXor => from_bits(ty, x ^ y),
        BinOp::Shl => from_bits(ty, x << count()?),
        _ => {
            let n = count()?;
            if ty.is_signed() {
                // Sign-extend to 128 bits, shift arithmetically, keep the low bits.
                let widened = if width < 128 && (x >> (width - 1)) & 1 == 1 {
                    x | (!0u128 << width)
                } else {
                    x
                };
                from_bits(ty, ((widened as i128) >> n) as u128)
            } else {
                from_bits(ty, x >> n)
            }
        }
    })
}

/// An arithmetic operator as written.
fn op_text(op: &BinOp) -> &'static str {
    match op {
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        BinOp::Rem => "%",
        _ => "?",
    }
}

fn arithmetic(op: &BinOp, l: Value, r: Value) -> ExprResult<Value> {
    use BinOp::*;
    let overflow = |l: &dyn std::fmt::Debug, r: &dyn std::fmt::Debug| {
        ExprError::Arithmetic(format!("{l:?} {} {r:?} overflows or divides by zero", op_text(op)))
    };
    match (l, r) {
        (Value::Int(a), Value::Int(b)) => int_op(op, a, b).map(Value::Int),
        // IEEE, canonical; never an error (LANGUAGE §5.1).
        (Value::F64(a), Value::F64(b)) => Ok(Value::F64(float::canonical(match op {
            Add => a + b,
            Sub => a - b,
            Mul => a * b,
            Div => a / b,
            _ => a % b,
        }))),
        (Value::Duration(a), Value::Duration(b)) if matches!(op, Add | Sub) => {
            let v = if *op == Add { a.checked_add(b) } else { a.checked_sub(b) };
            v.map(Value::Duration).ok_or_else(|| overflow(&a, &b))
        }
        (Value::Instant(a), Value::Duration(d)) if matches!(op, Add | Sub) => {
            let v = if *op == Add { a.checked_add(d) } else { a.checked_sub(d) };
            v.map(Value::Instant).ok_or_else(|| overflow(&a, &d))
        }
        // Scaling (LANGUAGE §5.1).
        (Value::Duration(d), Value::Int(k)) if matches!(op, Mul | Div) => {
            let v = if *op == Mul { d.times(k) } else { d.divided_by(k) };
            v.map(Value::Duration).ok_or_else(|| overflow(&d, &k))
        }
        (Value::Int(k), Value::Duration(d)) if *op == Mul => {
            d.times(k).map(Value::Duration).ok_or_else(|| overflow(&k, &d))
        }
        (Value::Duration(d), Value::Instant(a)) if *op == Add => {
            a.checked_add(d).map(Value::Instant).ok_or_else(|| overflow(&d, &a))
        }
        (Value::Instant(a), Value::Instant(b)) if *op == Sub => {
            a.checked_since(b).map(Value::Duration).ok_or_else(|| overflow(&a, &b))
        }
        (l, r) => Err(bug(format!("arithmetic {op:?} on {l:?} and {r:?}"))),
    }
}

macro_rules! same_width {
    ($op:expr, $a:expr, $b:expr, $($v:ident),*) => {
        match ($a, $b) {
            $((IntValue::$v(x), IntValue::$v(y)) => {
                let r = match $op {
                    BinOp::Add => x.checked_add(y),
                    BinOp::Sub => x.checked_sub(y),
                    BinOp::Mul => x.checked_mul(y),
                    BinOp::Div => x.checked_div(y),
                    BinOp::Rem => x.checked_rem(y),
                    _ => None,
                };
                r.map(IntValue::$v)
                    .ok_or_else(|| ExprError::Arithmetic(format!("{x} {} {y} overflows or divides by zero", op_text($op))))
            })*
            (a, b) => Err(bug(format!("arithmetic on {a:?} and {b:?}"))),
        }
    };
}

pub(crate) fn int_op(op: &BinOp, a: IntValue, b: IntValue) -> ExprResult<IntValue> {
    same_width!(op, a, b, U8, U16, U32, U64, U128, I8, I16, I32, I64, I128)
}

/// Matches `v` against `pat`, binding the pattern's unbound variables (recorded in `newly`); a bound variable is an
/// equality test.
pub(crate) fn matches(
    cx: &Ctx<'_>,
    env: &mut [Option<Value>],
    pat: &Pattern,
    v: &Value,
    newly: &mut Vec<usize>,
) -> ExprResult<bool> {
    match pat {
        Pattern::Wild => Ok(true),
        Pattern::Const(c) => Ok(term(cx, env, &Term::Const(*c))? == *v),
        Pattern::Var(var) => match env.get_mut(var.index()) {
            Some(slot @ None) => {
                *slot = Some(v.clone());
                newly.push(var.index());
                Ok(true)
            }
            Some(Some(existing)) => Ok(existing == v),
            None => Err(bug(format!("variable {var:?} out of range"))),
        },
        Pattern::Tuple(ps) => {
            let Value::Tuple(fs) = v else { return Ok(false) };
            if fs.len() != ps.len() {
                return Ok(false);
            }
            for (p, f) in ps.iter().zip(fs.iter()) {
                if !matches(cx, env, p, f, newly)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        Pattern::Variant { ty, number, fields } => {
            let payload: Vec<Value> = match (cx.program.types.get(*ty), v) {
                (Some(TypeDef::Option(_)), Value::Option(o)) => match (number, o) {
                    (1, Some(x)) => vec![(**x).clone()],
                    (0, None) => Vec::new(),
                    _ => return Ok(false),
                },
                (_, Value::Enum { variant, fields: fs }) if variant == number => fs.to_vec(),
                _ => return Ok(false),
            };
            if payload.len() != fields.len() {
                return Ok(false);
            }
            for (p, f) in fields.iter().zip(&payload) {
                if !matches(cx, env, p, f, newly)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        Pattern::Struct { fields, .. } => {
            let Value::Struct(fs) = v else { return Ok(false) };
            for (i, p) in fields {
                let Some(f) = fs.get(*i as usize) else { return Ok(false) };
                if !matches(cx, env, p, f, newly)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
    }
}

/// The values a generator ranges over, in canonical order.
pub(crate) fn generate(cx: &Ctx<'_>, env: &[Option<Value>], src: &GenSource) -> ExprResult<Vec<Value>> {
    match src {
        GenSource::Range {
            lo,
            hi,
            kind,
            ring_bits: None,
        } => {
            let (Value::Int(a), Value::Int(b)) = (eval(cx, env, lo)?, eval(cx, env, hi)?) else {
                return Err(bug("a range over non-integers".into()));
            };
            let ty = a.ty();
            let (Some(a), Some(b)) = (a.to_i128(), b.to_i128()) else {
                return Err(unimplemented!("LANG-092", "ranges beyond i128"));
            };
            let (start, end) = match kind {
                RangeKind::HalfOpen => (a, b),
                RangeKind::Closed => (a, b.saturating_add(1)),
                RangeKind::OpenOpen => (a.saturating_add(1), b),
                RangeKind::OpenClosed => (a.saturating_add(1), b.saturating_add(1)),
            };
            let mut out = Vec::new();
            let mut i = start;
            while i < end {
                out.push(Value::Int(
                    IntValue::from_i128(ty, i).ok_or_else(|| bug("a range value out of its type".into()))?,
                ));
                i += 1;
            }
            Ok(out)
        }
        GenSource::Range { ring_bits: Some(_), .. } => Err(unimplemented!("LANG-026", "ring-interval generators")),
        GenSource::Value(e) => Ok(match eval(cx, env, e)? {
            Value::Vec(v) => v.to_vec(),
            Value::Set(s) => s.iter().cloned().collect(),
            Value::Map(m) => m
                .iter()
                .map(|(k, v)| Value::Tuple(vec![k.clone(), v.clone()].into()))
                .collect(),
            other => return Err(bug(format!("a generator over {other:?}"))),
        }),
        GenSource::Lattice(e) => Ok(match eval(cx, env, e)? {
            Value::Lattice(LatValue::Set(s)) => s.iter().cloned().collect(),
            Value::Lattice(LatValue::Map(m)) => m
                .iter()
                .map(|(k, v)| Value::Tuple(vec![k.clone(), Value::Lattice(v.clone())].into()))
                .collect(),
            other => return Err(bug(format!("a lattice generator over {other:?}"))),
        }),
        GenSource::TableFn { .. } => Err(unimplemented!("LANG-183", "table-function generators")),
    }
}

/// The checked sum of integers of one type.
pub(crate) fn int_sum<'a>(mut values: impl Iterator<Item = &'a Value>) -> ExprResult<Value> {
    let Some(Value::Int(mut acc)) = values.next().cloned() else {
        return Err(bug("a sum over an empty group or non-integers".into()));
    };
    for v in values {
        let Value::Int(x) = v else {
            return Err(bug(format!("a sum over {v:?}")));
        };
        acc = int_op(&BinOp::Add, acc, *x)?;
    }
    Ok(Value::Int(acc))
}

/// Whether an expression reads a time-varying scalar (LANGUAGE §15.1, ARCHITECTURE §3.4.2): its value can change from
/// tick to tick with no relation changing, so a rule reading one is re-evaluated at every tick.
/// Whether calling `f` draws randomness (its value changes from tick to tick).
pub(crate) fn draws_randomness(f: &FnRef) -> bool {
    matches!(f, FnRef::Builtin(b) if random_builtin(b))
}

fn random_builtin(f: &BuiltinFn) -> bool {
    matches!(
        f,
        BuiltinFn::Rand | BuiltinFn::RandFloat | BuiltinFn::RandRange | BuiltinFn::RandPrio { .. }
    )
}

pub(crate) fn time_varying(e: &Expr) -> bool {
    match e {
        Expr::Scalar(BuiltinScalar::Now | BuiltinScalar::Tick | BuiltinScalar::Incarnation) => true,
        Expr::Scalar(_) | Expr::Term(_) | Expr::Param(_) => false,
        Expr::Call { f, args } => draws_randomness(f) || args.iter().any(time_varying),
        Expr::Unary { arg, .. } => time_varying(arg),
        Expr::Binary { lhs, rhs, .. } => time_varying(lhs) || time_varying(rhs),
        Expr::Construct { fields, .. } => fields.iter().any(time_varying),
        Expr::Field { base, .. } => time_varying(base),
        Expr::If { cond, then, els } => time_varying(cond) || time_varying(then) || time_varying(els),
        Expr::Match { scrut, arms } => {
            time_varying(scrut)
                || arms
                    .iter()
                    .any(|(_, g, b)| g.as_ref().is_some_and(time_varying) || time_varying(b))
        }
        Expr::Collection { elems, .. } => elems.iter().any(time_varying),
        Expr::Lattice { args, .. } => args.iter().any(time_varying),
        Expr::Let { value, body, .. } => time_varying(value) || time_varying(body),
        Expr::Closure { body, .. } => time_varying(body),
        Expr::Typed { expr, .. } => time_varying(expr),
    }
}
