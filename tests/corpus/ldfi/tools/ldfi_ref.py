#!/usr/bin/env python3
"""ldfi_ref: an independent reference checker for the Molly LDFI golden corpus (tests/corpus/ldfi/molly).

What it is for
--------------
The corpus's expectations come from the literature (FEATURES.md §11.5, R06 §12, the LDFI, SoCC'16 and Nemo papers).
Most Molly programs could not be vendored (the Molly repository carries no license), so they were re-derived. A
re-derived program is only a faithful port if it reproduces the published verdict, and a mistake in it would otherwise
surface months later as a phantom failure of the LDFI work package. This tool checks every case against its manifest:

* ``exhaustive``: the ground-truth verdict over *every* admissible fault schedule of the failure spec, with exact state
  merging per time step (feasible for most corpus configurations);
* ``ldfi``: a lineage-driven search implementing the core algorithm of ARCHITECTURE §8.3-§8.5 (hazard antichains,
  minimal models under the crash-order encoding, seeded hypotheses, a (fault count, canonical order) queue, an
  explored set, Molly's oracle). It reports the verdict and the number of runs, which is compared with ``runs_max``;
* ``falsifiers``: the Appendix-B-minimal falsifier sets of the failure-free run's post goals, by exhaustive enumeration,
  compared with ``falsifiers`` where a manifest states them.

It is a validation aid for corpus authors and for the triage work packages. It is not part of Blossom, nothing in the
corpus is derived from its output, and it never decides an expectation: a disagreement means the program or the tool
is wrong, and the literature decides which.

Semantics (R06 §3.2-§3.7, ARCHITECTURE §8.1 and §13.12, CR-13, CR-21, CR-22, CR-30, CR-31)
--------------------------------------------------------------------------------------
* Molly's dialect: ``include``, facts ``p(..)@k``, rules with ``@next`` / ``@async`` heads, ``notin``, absolute-time body
  atoms ``p(..)@k``, head aggregates ``count``/``min``/``max``/``sum``, right-nested precedence-free expressions, and
  ``//``, ``/* */`` and ``#`` comments. ``include`` resolves relative to the including file.
* Time runs 1..EOT (Molly round k is tick k). A fact ``p(..)@k`` holds at time k only. Deductive rules fire at t;
  ``@next`` heads hold at t+1 on the same node; ``@async`` heads are delivered to the head's first column at t+1.
* A rule fires only when its location (the first column of its first body predicate) is one of the nodes (Molly's
  clock guard). An ``@async`` rule fires only when its destination is a node, and its sender is up or sends to itself.
* Each time step evaluates the deductive rules stratum by stratum (temporal stratification, SEM-020). An aggregate
  groups by its non-aggregate head columns and ranges over the distinct valuations of the group columns and the
  aggregated variable (Molly's split rewrite, LANGUAGE §10.1); an empty group yields no row.
* Faults: an omission O(f,t,s) is admissible iff f != t and 1 <= s < EFF (CR-21); it drops everything f sends to t at
  s. A crash C(n,c) stops every message n sends to another node at send times >= c; n keeps receiving and computing
  and keeps its self-sends (CrashView::MollyContinue). ``crash(Observer, Node, Time)`` is the omniscient spec oracle.
  At most ``crashes`` nodes crash, at times 1..EOT-1 (Molly's hypothesis space).
* The oracle is Molly's ``isGood`` (TEST-022): pre and post are read at EOT; a run is bad iff some post tuple of the
  failure-free run is missing from post while present in pre. A missing pre or post is an error (CR-30).
* Lineage (ARCHITECTURE §8.3, MollyContinue profile): a goal is the AND of its firings, a firing the OR of its premises;
  a message leaf (f,t,s) is O(f,t,s) when admissible, or K(f,s) ("f crashed at or before s"); an EDB fact, a ``crash``
  fact and a self-send are unfalsifiable (CR-22); a negated premise gets conservative negative support (CR-31): the OR
  of the hazards of every fact whose relation reaches the negated relation, at an earlier time or at the same time
  along a purely deductive path; an aggregate depends on every contributor (Molly's conjunctive encoding).
* Minimality of falsifiers (ARCHITECTURE §8.3): fault sets are compared by the set of clock facts they remove; a crash
  C(n,c) removes n's outgoing clocks at every time >= c.

Usage
-----
  ldfi_ref.py run FILE --eot N --eff N --crashes N --nodes a,b,c [--omit a:b:1,...] [--crash a:2,...] [--dump]
  ldfi_ref.py verdict FILE --eot N --eff N --crashes N --nodes a,b,c [--mode exhaustive|ldfi|both] [--find-all]
  ldfi_ref.py falsifiers FILE --eot N --eff N --crashes N --nodes a,b,c
  ldfi_ref.py check [CASE_DIR ...] [--skip-exhaustive-over N] [--falsifiers]   # manifests under tests/corpus/ldfi
  ldfi_ref.py selftest

Standard library only (Python >= 3.11).
"""
from __future__ import annotations

import argparse
import heapq
import itertools
import pathlib
import re
import sys
import time as wallclock
import tomllib
from dataclasses import dataclass, field


class DedError(Exception):
    """A program the reference cannot accept (parse, static, stratification or evaluation error)."""


class TooLarge(Exception):
    """An exhaustive search exceeded its state budget."""


# =====================================================================================================================
# AST


@dataclass(frozen=True)
class Var:
    name: str


@dataclass(frozen=True)
class Const:
    value: object  # int | str


@dataclass(frozen=True)
class Wild:
    pass


@dataclass(frozen=True)
class BinOp:
    left: object  # Var | Const
    op: str
    right: object  # Var | Const | BinOp  (Molly's right-nested parse)


@dataclass(frozen=True)
class Agg:
    func: str
    var: str


@dataclass(frozen=True)
class Atom:
    rel: str
    args: tuple
    negated: bool = False
    time: int | None = None  # absolute time `@k` of a body atom


@dataclass(frozen=True)
class Rule:
    head: Atom
    kind: str  # "ded" | "next" | "async"
    body: tuple  # Atom | BinOp
    where: str
    index: int = 0

    @property
    def atoms(self):
        return tuple(b for b in self.body if isinstance(b, Atom))

    @property
    def quals(self):
        return tuple(b for b in self.body if isinstance(b, BinOp))

    @property
    def agg(self):
        aggs = [a for a in self.head.args if isinstance(a, Agg)]
        return aggs[0] if aggs else None


@dataclass
class Program:
    rules: list = field(default_factory=list)
    facts: dict = field(default_factory=dict)  # time -> set of (rel, tuple)
    files: list = field(default_factory=list)
    arity: dict = field(default_factory=dict)

    def relations(self):
        rels = set()
        for r in self.rules:
            rels.add(r.head.rel)
            rels.update(a.rel for a in r.atoms)
        for fs in self.facts.values():
            rels.update(rel for rel, _ in fs)
        return rels

    def is_edb(self, rel, tup, t):
        return (rel, tup) in self.facts.get(t, ())


# =====================================================================================================================
# Lexer and parser (Molly's dialect, LANGUAGE §21.1)

TOKEN = re.compile(
    r"""(?P<ws>\s+)|(?P<lc>//[^\n]*|\#[^\n]*)|(?P<bc>/\*.*?\*/)|(?P<str>"[^"\n]*")|(?P<int>\d+)
        |(?P<id>[A-Za-z_][A-Za-z0-9_]*)|(?P<op>:-|<=|>=|==|!=|<|>|\+|-|\*|/|\(|\)|,|;|@)""",
    re.S | re.X,
)
AGG_FUNCS = ("count", "min", "max", "sum")
CMP_OPS = ("<", ">", "<=", ">=", "==", "!=")
ARITH_OPS = ("+", "-", "*", "/")
RESERVED = ("clock",)


def lex(text, fname):
    toks = []
    pos = 0
    line = 1
    while pos < len(text):
        m = TOKEN.match(text, pos)
        if not m:
            raise DedError(f"{fname}:{line}: unexpected character {text[pos]!r}")
        kind = m.lastgroup
        val = m.group(kind)
        if kind not in ("ws", "lc", "bc"):
            toks.append((kind, val, line))
        line += val.count("\n")
        pos = m.end()
    toks.append(("eof", "", line))
    return toks


class Parser:
    def __init__(self, toks, fname):
        self.toks = toks
        self.i = 0
        self.fname = fname

    def peek(self, k=0):
        return self.toks[min(self.i + k, len(self.toks) - 1)]

    def take(self):
        t = self.toks[self.i]
        self.i += 1
        return t

    def err(self, tok, msg):
        return DedError(f"{self.fname}:{tok[2]}: {msg}")

    def expect(self, val):
        t = self.take()
        if t[1] != val or t[0] == "str":
            raise self.err(t, f"expected {val!r}, found {t[1]!r}")
        return t

    def clauses(self):
        out = []
        while self.peek()[0] != "eof":
            out.append(self.clause())
        return out

    def clause(self):
        t = self.peek()
        if t[0] == "id" and t[1] == "include" and self.peek(1)[0] == "str":
            self.take()
            path = self.take()[1][1:-1]
            self.expect(";")
            return ("include", path, t[2])
        head, suffix = self.predicate()
        if head.negated:
            raise self.err(t, "a clause head cannot be negated")
        if self.peek()[1] == ":-":
            self.take()
            body = [self.body_term()]
            while self.peek()[1] == ",":
                self.take()
                body.append(self.body_term())
            self.expect(";")
            if isinstance(suffix, int):
                raise self.err(t, f"a rule head cannot carry @{suffix} (R06 §3.2)")
            kind = {"next": "next", "async": "async", None: "ded"}[suffix]
            return ("rule", Rule(head, kind, tuple(body), f"{self.fname}:{t[2]}"), t[2])
        self.expect(";")
        if not isinstance(suffix, int):
            raise self.err(t, "a fact must carry @<time> (R06 §3.2)")
        vals = []
        for a in head.args:
            if not isinstance(a, Const):
                raise self.err(t, "facts contain only constants")
            vals.append(a.value)
        return ("fact", (head.rel, tuple(vals), suffix), t[2])

    def predicate(self):
        negated = False
        if self.peek()[0] == "id" and self.peek()[1] == "notin":
            self.take()
            negated = True
        name = self.take()
        if name[0] != "id":
            raise self.err(name, f"expected a relation name, found {name[1]!r}")
        self.expect("(")
        args = []
        if self.peek()[1] != ")":
            args.append(self.arg())
            while self.peek()[1] == ",":
                self.take()
                args.append(self.arg())
        self.expect(")")
        suffix = None
        if self.peek()[1] == "@" and self.peek()[0] == "op":
            self.take()
            s = self.take()
            if s[0] == "int":
                suffix = int(s[1])
            elif s[0] == "id" and s[1] in ("next", "async"):
                suffix = s[1]
            else:
                raise self.err(s, f"bad time suffix @{s[1]}")
        return Atom(name[1], tuple(args), negated, None), suffix

    def arg(self):
        t = self.peek()
        if (t[0] == "id" and t[1] in AGG_FUNCS and self.peek(1)[1] == "<" and self.peek(2)[0] == "id"
                and self.peek(3)[1] == ">"):
            self.take()
            self.take()
            v = self.take()[1]
            self.take()
            if not v[0].isupper():
                raise self.err(t, f"aggregate over {v!r}: aggregates range over a variable")
            return Agg(t[1], v)
        return self.expr()

    def constant(self):
        t = self.take()
        if t[0] == "str":
            return Const(t[1][1:-1])
        if t[0] == "int":
            return Const(int(t[1]))
        if t[0] == "id":
            if t[1] == "_":
                return Wild()
            if t[1][0].isupper():
                return Var(t[1])
            raise self.err(t, f"bare identifier {t[1]!r}: quote string constants and capitalize variables")
        raise self.err(t, f"expected a constant or variable, found {t[1]!r}")

    def expr(self):
        left = self.constant()
        nxt = self.peek()
        if nxt[0] == "op" and (nxt[1] in CMP_OPS or nxt[1] in ARITH_OPS):
            op = self.take()[1]
            if isinstance(left, Wild):
                raise self.err(nxt, "`_` cannot appear in an expression")
            return BinOp(left, op, self.expr())
        return left

    def body_term(self):
        t = self.peek()
        if (t[0] == "id" and t[1] == "notin") or (t[0] == "id" and self.peek(1)[1] == "("):
            atom, suffix = self.predicate()
            if suffix in ("next", "async"):
                raise self.err(t, f"a body atom cannot carry @{suffix}")
            return Atom(atom.rel, atom.args, atom.negated, suffix)
        e = self.expr()
        if not isinstance(e, BinOp) or e.op not in CMP_OPS:
            raise self.err(t, "a body qualifier must be a comparison")
        return e


def load_program(paths):
    prog = Program()
    seen = []

    def load(p: pathlib.Path, stack):
        p = p.resolve()
        if p in stack:
            raise DedError(f"include cycle through {p}")
        if p in seen:
            return  # Molly concatenates files; a file included twice is loaded once
        seen.append(p)
        prog.files.append(p)
        if not p.is_file():
            raise DedError(f"no such file: {p}")
        text = p.read_text()
        for kind, payload, _line in Parser(lex(text, str(p)), str(p)).clauses():
            if kind == "include":
                load(p.parent / payload, stack + [p])
            elif kind == "rule":
                prog.rules.append(Rule(payload.head, payload.kind, payload.body, payload.where, len(prog.rules)))
            else:
                rel, vals, t = payload
                prog.facts.setdefault(t, set()).add((rel, vals))

    for p in paths:
        load(pathlib.Path(p), [])
    validate(prog)
    return prog


# =====================================================================================================================
# Static checks


def expr_vars(e):
    if isinstance(e, Var):
        return {e.name}
    if isinstance(e, BinOp):
        return expr_vars(e.left) | expr_vars(e.right)
    return set()


def atom_vars(a: Atom):
    out = set()
    for x in a.args:
        out |= expr_vars(x)
    return out


def validate(prog: Program):
    arity = {}

    def note(rel, n, where):
        if rel in RESERVED:
            raise DedError(f"{where}: `{rel}` is reserved (Molly generates it)")
        if arity.setdefault(rel, n) != n:
            raise DedError(f"{where}: relation {rel} used with arity {n} and {arity[rel]}")

    for t, fs in prog.facts.items():
        if t < 1:
            raise DedError(f"fact at time {t}: Molly facts hold at times >= 1 (CR-13)")
        for rel, vals in fs:
            if rel == "crash":
                raise DedError("`crash` facts are generated from the failure spec")
            note(rel, len(vals), f"fact {rel}{vals}@{t}")
    note("crash", 3, "crash oracle")
    for r in prog.rules:
        note(r.head.rel, len(r.head.args), r.where)
        if r.head.rel == "crash":
            raise DedError(f"{r.where}: `crash` is the spec oracle and cannot be derived")
        if not r.atoms:
            raise DedError(f"{r.where}: a rule needs a body predicate (it gives the rule's location)")
        for a in r.atoms:
            note(a.rel, len(a.args), r.where)
            if a.time is not None and a.time < 1:
                raise DedError(f"{r.where}: absolute-time atom {a.rel}@{a.time}: times start at 1")
            for x in a.args:
                if isinstance(x, (BinOp, Agg)):
                    raise DedError(f"{r.where}: expressions and aggregates are not allowed inside body atoms")
        loc = r.atoms[0].args[0] if r.atoms[0].args else None
        if loc is None or isinstance(loc, (BinOp, Agg)):
            raise DedError(f"{r.where}: the first column of the first body predicate is the rule's location")
        if isinstance(loc, Wild) and r.kind == "async":
            raise DedError(f"{r.where}: an @async rule needs a located sender (its first body predicate starts with `_`)")
        pos = set()
        for a in r.atoms:
            if not a.negated:
                pos |= atom_vars(a)
        for a in r.atoms:
            if a.negated and not atom_vars(a) <= pos:
                raise DedError(f"{r.where}: variables {sorted(atom_vars(a) - pos)} of `notin {a.rel}` are not bound by a "
                               f"positive predicate")
        for q in r.quals:
            if not expr_vars(q) <= pos:
                raise DedError(f"{r.where}: unbound variable in a qualifier")
        hv = set()
        aggs = 0
        for x in r.head.args:
            if isinstance(x, Agg):
                aggs += 1
                hv.add(x.var)
            elif isinstance(x, Wild):
                raise DedError(f"{r.where}: `_` in a rule head")
            else:
                hv |= expr_vars(x)
        if not hv <= pos:
            raise DedError(f"{r.where}: head variables {sorted(hv - pos)} are not bound by positive body predicates")
        if aggs > 1:
            raise DedError(f"{r.where}: at most one aggregate per head in this reference")
        if aggs and isinstance(r.head.args[0], Agg):
            raise DedError(f"{r.where}: the first head column is the location and cannot be an aggregate")
        if aggs and r.kind != "ded":
            raise DedError(f"{r.where}: aggregate heads must be deductive in this reference")
        if aggs and any(isinstance(x, BinOp) for x in r.head.args):
            raise DedError(f"{r.where}: aggregates combined with head expressions are not supported")
        if r.kind == "async" and not r.head.args:
            raise DedError(f"{r.where}: an @async head needs a destination column")
    prog.arity = arity


def require_prepost(prog):
    heads = {r.head.rel for r in prog.rules} | {rel for fs in prog.facts.values() for rel, _ in fs}
    missing = [x for x in ("pre", "post") if x not in heads]
    if missing:
        raise DedError(f"missing {' and '.join(missing)} (CR-30, BLS0900)")
    if prog.arity.get("pre") != prog.arity.get("post"):
        raise DedError("pre and post must have the same schema (TEST-022)")


# =====================================================================================================================
# Stratification (temporal: only the deductive reduction must stratify, SEM-020) and reachability


def stratify(prog: Program):
    ded = [r for r in prog.rules if r.kind == "ded"]
    heads = sorted({r.head.rel for r in ded})
    edges = []  # (from_rel, to_rel, strict)
    for r in ded:
        for a in r.atoms:
            edges.append((a.rel, r.head.rel, a.negated or r.agg is not None))
    nodes = sorted({x for e in edges for x in e[:2]} | set(heads))
    graph = {v: set() for v in nodes}
    for f, t, _ in edges:
        graph[f].add(t)
    # iterative Tarjan
    index, low, on, stack, comp = {}, {}, set(), [], {}
    counter = 0
    ncomp = 0
    for root in nodes:
        if root in index:
            continue
        work = [(root, iter(sorted(graph[root])))]
        index[root] = low[root] = counter
        counter += 1
        stack.append(root)
        on.add(root)
        while work:
            v, it = work[-1]
            advanced = False
            for w in it:
                if w not in index:
                    index[w] = low[w] = counter
                    counter += 1
                    stack.append(w)
                    on.add(w)
                    work.append((w, iter(sorted(graph[w]))))
                    advanced = True
                    break
                if w in on:
                    low[v] = min(low[v], index[w])
            if advanced:
                continue
            work.pop()
            if work:
                low[work[-1][0]] = min(low[work[-1][0]], low[v])
            if low[v] == index[v]:
                while True:
                    w = stack.pop()
                    on.discard(w)
                    comp[w] = ncomp
                    if w == v:
                        break
                ncomp += 1
    for f, t, strict in edges:
        if strict and comp[f] == comp[t]:
            raise DedError(f"not temporally stratifiable: {f} -> {t} through negation or aggregation in a same-time "
                           f"cycle (SEM-020)")
    level = {c: 0 for c in set(comp.values())}
    changed = True
    while changed:
        changed = False
        for f, t, strict in edges:
            cf, ct = comp[f], comp[t]
            if cf == ct:
                continue
            need = level[cf] + (1 if strict else 0)
            if level[ct] < need:
                level[ct] = need
                changed = True
    # order components topologically within a level so that plain dependencies are evaluated first
    comp_order = {}
    remaining = set(comp.values())
    deps = {c: set() for c in remaining}
    for f, t, _ in edges:
        if comp[f] != comp[t]:
            deps[comp[t]].add(comp[f])
    order = []
    while remaining:
        ready = sorted(c for c in remaining if not (deps[c] & remaining))
        if not ready:
            raise DedError("internal: component graph is cyclic")
        for c in ready:
            order.append(c)
            remaining.discard(c)
    for i, c in enumerate(order):
        comp_order[c] = i
    groups = {}
    for r in ded:
        c = comp[r.head.rel]
        groups.setdefault((level[c], comp_order[c]), []).append(r)
    return [groups[k] for k in sorted(groups)]


def reachability(prog: Program, odd_only=False):
    """For every relation p: {q: purely_deductive_path_exists} for the relations q from which p is reachable along
    one or more rule edges (body -> head), the static analysis behind conservative negative support. With odd_only,
    only paths through an odd number of negated edges count (the optional parity filter of TEST-025: falsifying q can
    make p appear only if p depends negatively on q)."""
    edges = {}
    for r in prog.rules:
        for a in r.atoms:
            edges.setdefault(a.rel, set()).add((r.head.rel, r.kind == "ded", a.negated))
    reach_from = {}
    for src in sorted(prog.relations()):
        reached = {}
        work = [(src, True, False)]
        seen = set()
        while work:
            v, ded, odd = work.pop()
            if (v, ded, odd) in seen:
                continue
            seen.add((v, ded, odd))
            for w, is_ded, neg in edges.get(v, ()):
                d2 = ded and is_ded
                o2 = odd != neg
                if o2 or not odd_only:
                    reached[w] = reached.get(w, False) or d2
                work.append((w, d2, o2))
        reach_from[src] = reached
    preds = {}
    for src, targets in reach_from.items():
        for tgt, ded in targets.items():
            preds.setdefault(tgt, {})[src] = ded
    return preds


# =====================================================================================================================
# Expressions


def ev(e, env):
    if isinstance(e, Const):
        return e.value
    if isinstance(e, Var):
        return env[e.name]
    if isinstance(e, BinOp):
        lhs = ev(e.left, env)
        rhs = ev(e.right, env)
        op = e.op
        if op in ARITH_OPS:
            if not (isinstance(lhs, int) and isinstance(rhs, int)):
                raise DedError(f"arithmetic on non-integers: {lhs!r} {op} {rhs!r}")
            if op == "+":
                return lhs + rhs
            if op == "-":
                return lhs - rhs
            if op == "*":
                return lhs * rhs
            if rhs == 0:
                raise DedError("division by zero")
            q = abs(lhs) // abs(rhs)  # C semantics: truncate toward zero
            return q if (lhs >= 0) == (rhs >= 0) else -q
        if op in ("==", "!="):
            if type(lhs) is not type(rhs):
                raise DedError(f"comparing {lhs!r} with {rhs!r} of a different type")
            return (lhs == rhs) if op == "==" else (lhs != rhs)
        if type(lhs) is not type(rhs) or not isinstance(lhs, int):
            raise DedError(f"ordering comparison {lhs!r} {op} {rhs!r} needs two integers")
        return {"<": lhs < rhs, ">": lhs > rhs, "<=": lhs <= rhs, ">=": lhs >= rhs}[op]
    raise DedError(f"cannot evaluate {e!r}")


# =====================================================================================================================
# Failure specs and fault sets


@dataclass(frozen=True)
class Spec:
    eot: int
    eff: int
    crashes: int
    nodes: tuple

    def __post_init__(self):
        if self.eot < 1 or self.eff < 0 or self.crashes < 0:
            raise DedError(f"bad failure spec {self}")
        if self.eff >= self.eot:
            raise DedError(f"EFF {self.eff} must be < EOT {self.eot} (TEST-020)")
        if self.crashes > len(self.nodes):
            raise DedError("more crashes than nodes")
        if len(set(self.nodes)) != len(self.nodes):
            raise DedError("duplicate node names")

    def omission_ok(self, f, t, s):
        return f != t and 1 <= s < self.eff

    def label(self):
        return f"EOT {self.eot}, EFF {self.eff}, crashes {self.crashes}, nodes {','.join(self.nodes)}"


@dataclass(frozen=True)
class Faults:
    """A fault set: omissions (from, to, send_time) and crashes ((node, time), ...) sorted by node."""
    omissions: frozenset = frozenset()
    crashes: tuple = ()

    @property
    def crash_map(self):
        return dict(self.crashes)

    def count(self):
        return len(self.omissions) + len(self.crashes)

    def key(self):
        return (self.count(), self.crashes, tuple(sorted(self.omissions)))

    def labels(self):
        return sorted([f"C({n},{t})" for n, t in self.crashes] + [f"O({f},{d},{s})" for f, d, s in self.omissions])

    def render(self):
        return "{" + ", ".join(self.labels()) + "}"

    def removed_clocks(self, spec: Spec):
        out = set(self.omissions)
        for n, c in self.crashes:
            for x in spec.nodes:
                if x != n:
                    for s in range(c, spec.eot + 1):
                        out.add((n, x, s))
        return frozenset(out)


def make_faults(omissions, crashes: dict):
    """Canonical fault set: drop omissions implied by a crash of their sender at or before the send time."""
    om = frozenset(o for o in omissions if not (o[0] in crashes and crashes[o[0]] <= o[2]))
    return Faults(om, tuple(sorted(crashes.items())))


def admissible(spec: Spec, f: Faults):
    if len(f.crashes) > spec.crashes:
        return False
    for n, c in f.crashes:
        if n not in spec.nodes or not (1 <= c <= spec.eot - 1):
            return False
    for fr, to, s in f.omissions:
        if fr not in spec.nodes or to not in spec.nodes or not spec.omission_ok(fr, to, s):
            return False
    return True


def crash_schedules(spec: Spec):
    yield {}
    for k in range(1, spec.crashes + 1):
        for nodes in itertools.combinations(spec.nodes, k):
            for times in itertools.product(range(1, spec.eot), repeat=k):
                yield dict(zip(nodes, times))


# =====================================================================================================================
# Evaluation


class Model:
    """The relations holding at one time step, with lazily built hash indexes (relations only grow within a step)."""

    __slots__ = ("rels", "_idx")

    def __init__(self):
        self.rels = {}
        self._idx = {}

    def get(self, rel):
        return self.rels.get(rel, ())

    def add(self, rel, tup):
        s = self.rels.setdefault(rel, set())
        if tup in s:
            return False
        s.add(tup)
        return True

    def lookup(self, rel, positions, values):
        s = self.rels.get(rel)
        if not s:
            return ()
        if not positions:
            return s
        key = (rel, positions)
        cached = self._idx.get(key)
        if cached is None or cached[0] != len(s):
            idx = {}
            for tup in s:
                idx.setdefault(tuple(tup[p] for p in positions), []).append(tup)
            cached = (len(s), idx)
            self._idx[key] = cached
        return cached[1].get(values, ())

    def frozen(self):
        return {k: frozenset(v) for k, v in self.rels.items()}


class StaticRel:
    """A read-only relation (history snapshot or the crash oracle) with the Model lookup interface."""

    __slots__ = ("tuples", "_idx")

    def __init__(self, tuples):
        self.tuples = frozenset(tuples)
        self._idx = {}

    def lookup(self, positions, values):
        if not positions:
            return self.tuples
        idx = self._idx.get(positions)
        if idx is None:
            idx = {}
            for tup in self.tuples:
                idx.setdefault(tuple(tup[p] for p in positions), []).append(tup)
            self._idx[positions] = idx
        return idx.get(values, ())


class Evaluator:
    """Molly's synchronous semantics under CrashView::MollyContinue, one time step at a time."""

    def __init__(self, prog: Program, spec: Spec):
        self.prog = prog
        self.spec = spec
        self.nodes = frozenset(spec.nodes)
        self.strata = stratify(prog)
        self.next_rules = [r for r in prog.rules if r.kind == "next"]
        self.async_rules = [r for r in prog.rules if r.kind == "async"]
        self.rules_by_head = {}
        for r in prog.rules:
            self.rules_by_head.setdefault(r.head.rel, []).append(r)
        self.abs_reads = sorted({(a.rel, a.time) for r in prog.rules for a in r.atoms if a.time is not None})
        self.abs_times = sorted({k for _, k in self.abs_reads})
        self._plans = {}

    # -- joins ------------------------------------------------------------------------------------------------------

    def plan(self, rule, pre_bound=frozenset()):
        """Join order for the positive atoms: greedily pick the atom with the most bound columns."""
        key = (rule.index, pre_bound)
        p = self._plans.get(key)
        if p is not None:
            return p
        pos = [a for a in rule.atoms if not a.negated]
        bound = set(pre_bound)
        order = []
        remaining = list(pos)
        while remaining:
            best = max(remaining, key=lambda a: (sum(1 for x in a.args if isinstance(x, Const) or
                                                     (isinstance(x, Var) and x.name in bound)),
                                                 -rule.atoms.index(a)))
            remaining.remove(best)
            order.append(best)
            bound |= atom_vars(best)
        p = tuple(order)
        self._plans[key] = p
        return p

    def source(self, model, history, crashes, rel, abs_time, t):
        """The relation an atom reads: a Model (current step) or a StaticRel (history or crash oracle).
        Returns None when the atom refers to a future time (the rule instance does not fire yet)."""
        if rel == "crash":
            return crashes
        if abs_time is None or abs_time == t:
            return model
        if abs_time > t:
            return None
        return history[(rel, abs_time)]

    @staticmethod
    def lookup(src, rel, positions, values):
        if isinstance(src, Model):
            return src.lookup(rel, positions, values)
        return src.lookup(positions, values)

    @staticmethod
    def match(args, tup, env):
        new = None
        for a, v in zip(args, tup):
            if isinstance(a, Const):
                if a.value != v:
                    return None
            elif isinstance(a, Var):
                cur = (new or env).get(a.name, _UNSET)
                if cur is _UNSET:
                    if new is None:
                        new = dict(env)
                    new[a.name] = v
                elif cur != v:
                    return None
        return new if new is not None else env

    def bindings(self, rule, model, history, crashes, t, env0=None, with_premises=False):
        """Every variable environment satisfying the body of `rule` at time t (optionally with the positive premise
        tuples it used, for lineage)."""
        env0 = env0 or {}
        order = self.plan(rule, frozenset(env0))
        srcs = []
        for a in order:
            s = self.source(model, history, crashes, a.rel, a.time, t)
            if s is None:
                return
            srcs.append(s)
        negs = []
        for a in rule.atoms:
            if a.negated:
                s = self.source(model, history, crashes, a.rel, a.time, t)
                if s is None:
                    return
                negs.append((a, s))
        quals = rule.quals

        def rec(i, env, used):
            if i == len(order):
                for q in quals:
                    if not ev(q, env):
                        return
                for a, s in negs:
                    positions, values = bound_positions(a, env)
                    for tup in self.lookup(s, a.rel, positions, values):
                        if self.match(a.args, tup, env) is not None:
                            return
                yield env, used
                return
            a = order[i]
            positions, values = bound_positions(a, env)
            for tup in list(self.lookup(srcs[i], a.rel, positions, values)):
                e2 = self.match(a.args, tup, env)
                if e2 is not None:
                    yield from rec(i + 1, e2, used + ((a, tup),) if with_premises else used)

        yield from rec(0, env0, ())

    def location(self, rule, env):
        """The rule's location. A `_` there matches any clock fact in Molly's rewrite (`clock(_, _, t, _)`), so such a
        deductive or @next rule is not guarded by a node; ANYWHERE stands for that."""
        first = rule.atoms[0].args[0]
        if isinstance(first, Const):
            return first.value
        if isinstance(first, Wild):
            return ANYWHERE
        return env[first.name]

    def located(self, rule, env):
        loc = self.location(rule, env)
        return loc is ANYWHERE or loc in self.nodes

    def head_tuple(self, rule, env):
        return tuple(ev(x, env) for x in rule.head.args)

    # -- aggregates ---------------------------------------------------------------------------------------------------

    def group_values(self, rule, model, history, crashes, t, env0=None, with_premises=False):
        """{group key: {aggregated value: [(valuation, premises)]}} over the rule's valuations at time t."""
        agg = rule.agg
        groups = {}
        for env, used in self.bindings(rule, model, history, crashes, t, env0, with_premises):
            if not self.located(rule, env):
                continue
            key = tuple(None if isinstance(x, Agg) else ev(x, env) for x in rule.head.args)
            groups.setdefault(key, {}).setdefault(env[agg.var], []).append((env, used))
        return groups

    @staticmethod
    def agg_value(func, values):
        vals = list(values)
        if func == "count":
            return len(vals)
        if any(not isinstance(v, int) for v in vals):
            raise DedError(f"{func}<> over non-integers {vals!r}")
        if func == "sum":
            return sum(vals)
        if func == "min":
            return min(vals)
        return max(vals)

    def agg_tuples(self, rule, model, history, crashes, t):
        """Molly's split rewrite evaluates the aggregate in a second rule whose body is the bindings relation, so the
        aggregate is computed at the location in the head's first column; a group whose first column is not a node
        yields nothing (the clock guard of that rule fails)."""
        out = set()
        for key, per_value in self.group_values(rule, model, history, crashes, t).items():
            v = self.agg_value(rule.agg.func, per_value.keys())
            tup = tuple(v if k is None else k for k in key)
            if tup[0] in self.nodes:
                out.add(tup)
        return out

    # -- one step -----------------------------------------------------------------------------------------------------

    def step(self, t, carried, history, crashes):
        """The model at time t, the async sends made at t, and the facts @next rules put at t+1."""
        model = Model()
        for rel, tup in self.prog.facts.get(t, ()):
            model.add(rel, tup)
        for rel, tup in carried:
            model.add(rel, tup)
        for stratum in self.strata:
            for r in stratum:
                if r.agg is not None:
                    for tup in self.agg_tuples(r, model, history, crashes, t):
                        model.add(r.head.rel, tup)
            plain = [r for r in stratum if r.agg is None]
            changed = True
            while changed:
                changed = False
                for r in plain:
                    new = []
                    for env, _ in self.bindings(r, model, history, crashes, t):
                        if self.located(r, env):
                            new.append(self.head_tuple(r, env))
                    for tup in new:
                        if model.add(r.head.rel, tup):
                            changed = True
        inductive = set()
        for r in self.next_rules:
            for env, _ in self.bindings(r, model, history, crashes, t):
                if self.located(r, env):
                    inductive.add((r.head.rel, self.head_tuple(r, env)))
        sends = []
        for r in self.async_rules:
            for env, _ in self.bindings(r, model, history, crashes, t):
                loc = self.location(r, env)
                if loc not in self.nodes:
                    continue
                tup = self.head_tuple(r, env)
                if tup[0] not in self.nodes:
                    raise DedError(f"{r.where}: @async to {tup[0]!r}, which is not a node ({', '.join(self.spec.nodes)})")
                sends.append((loc, tup[0], r.head.rel, tup))
        return model, sends, inductive

    def crash_rel(self, crashes: dict):
        return StaticRel((o, n, c) for o in self.spec.nodes for n, c in crashes.items())

    def snapshot(self, history, model, t):
        for rel, k in self.abs_reads:
            if k == t:
                history[(rel, k)] = StaticRel(model.get(rel))

    def run(self, faults: Faults = Faults()):
        crashes = faults.crash_map
        crash_rel = self.crash_rel(crashes)
        history = {}
        models = {}
        messages = []
        carried = set()
        for t in range(1, self.spec.eot + 1):
            model, sends, inductive = self.step(t, carried, history, crash_rel)
            self.snapshot(history, model, t)
            models[t] = model
            arriving = set()
            for f, d, rel, tup in sends:
                if f != d and f in crashes and crashes[f] <= t:
                    continue  # a crashed sender has no clock to another node: the rule does not fire
                lost = f != d and (f, d, t) in faults.omissions
                if t < self.spec.eot:
                    messages.append((rel, f, d, t, None if lost else t + 1))
                if not lost:
                    arriving.add((rel, tup))
            carried = arriving | inductive
        return Run(self, faults, models, messages, crash_rel, history)


_UNSET = object()
ANYWHERE = object()


def bound_positions(atom, env):
    positions = []
    values = []
    for i, x in enumerate(atom.args):
        if isinstance(x, Const):
            positions.append(i)
            values.append(x.value)
        elif isinstance(x, Var) and x.name in env:
            positions.append(i)
            values.append(env[x.name])
    return tuple(positions), tuple(values)


@dataclass
class Run:
    ev: Evaluator
    faults: Faults
    models: dict  # t -> Model
    messages: list  # (rel, from, to, send, receive | None)
    crash_rel: StaticRel
    history: dict

    def rel_at(self, rel, t):
        m = self.models.get(t)
        return frozenset(m.get(rel)) if m else frozenset()

    def pre(self):
        return self.rel_at("pre", self.ev.spec.eot)

    def post(self):
        return self.rel_at("post", self.ev.spec.eot)


def is_good(ff_post, run: Run):
    """Molly's oracle `isGood` (TEST-022)."""
    posts = run.post()
    if posts == ff_post:
        return True
    pres = run.pre()
    return all(g not in pres for g in ff_post - posts)


# =====================================================================================================================
# Exhaustive verdict search with exact state merging


def exhaustive(prog: Program, spec: Spec, stop_at_first=True, max_states=2_000_000):
    """The verdict over every admissible fault schedule. Returns (verdict, witness Faults or None, stats)."""
    require_prepost(prog)
    evl = Evaluator(prog, spec)
    ff = evl.run()
    ff_post = ff.post()
    stats = {"schedules": 0, "states": 0, "ff_post": sorted(ff_post), "ff_pre": sorted(ff.pre())}
    witness = None
    for crashes in crash_schedules(spec):
        stats["schedules"] += 1
        crash_rel = evl.crash_rel(crashes)
        frontier = {(frozenset(), frozenset()): frozenset()}
        for t in range(1, spec.eot + 1):
            nxt = {}
            for (carried, hist_key), oms in sorted(frontier.items(), key=lambda kv: (len(kv[1]), sorted(kv[1]))):
                history = dict(hist_key)
                model, sends, inductive = evl.step(t, carried, history, crash_rel)
                evl.snapshot(history, model, t)
                if t == spec.eot:
                    run = Run(evl, Faults(), {spec.eot: model}, [], crash_rel, history)
                    if not is_good(ff_post, run):
                        f = make_faults(oms, crashes)
                        if witness is None or f.key() < witness.key():
                            witness = f
                        if stop_at_first:
                            return "counterexample", witness, stats
                    continue
                by_channel = {}
                always = set(inductive)
                for f, d, rel, tup in sends:
                    if f != d and f in crashes and crashes[f] <= t:
                        continue
                    if f != d and spec.omission_ok(f, d, t):
                        by_channel.setdefault((f, d), set()).add((rel, tup))
                    else:
                        always.add((rel, tup))
                channels = sorted(by_channel)
                hist_items = frozenset(history.items())
                for r in range(len(channels) + 1):
                    for drop in itertools.combinations(channels, r):
                        arriving = set(always)
                        dropped = set(drop)
                        for ch in channels:
                            if ch not in dropped:
                                arriving |= by_channel[ch]
                        key = (frozenset(arriving), hist_items)
                        cand = oms | {(f, d, t) for f, d in drop}
                        old = nxt.get(key)
                        if old is None or (len(cand), sorted(cand)) < (len(old), sorted(old)):
                            nxt[key] = cand
            stats["states"] += len(frontier)
            if stats["states"] > max_states:
                raise TooLarge(f"exhaustive search exceeded {max_states} states")
            frontier = nxt
    return ("counterexample" if witness else "no_counterexample"), witness, stats


# =====================================================================================================================
# Appendix-B-minimal falsifiers (exhaustive)


def sends_cannot_grow(prog: Program):
    """True when no fault can make a node send a message it does not send in the failure-free run: either no @async
    rule reads a relation downstream of a message, or every rule deriving such a relation is monotone (no negation of
    a message-dependent relation and no aggregate). Faults only remove clock facts, so in both cases the messages of a
    faulty run are a subset of the failure-free ones, and omissions of channel-times that carry no failure-free message
    change nothing."""
    edges = {}
    for r in prog.rules:
        for a in r.atoms:
            edges.setdefault(a.rel, set()).add(r.head.rel)
    influenced = set()
    work = [r.head.rel for r in prog.rules if r.kind == "async"]
    while work:
        v = work.pop()
        if v in influenced:
            continue
        influenced.add(v)
        work += list(edges.get(v, ()))
    async_reads_influenced = any(a.rel in influenced for r in prog.rules if r.kind == "async" for a in r.atoms)
    if not async_reads_influenced:
        return True
    for r in prog.rules:
        if r.head.rel not in influenced:
            continue
        if r.agg is not None or any(a.negated and a.rel in influenced for a in r.atoms):
            return False
    return True


def all_fault_sets(spec: Spec, channels=None):
    chans = [(f, d, s) for s in range(1, spec.eff) for f in spec.nodes for d in spec.nodes if f != d]
    if channels is not None:
        chans = [c for c in chans if c in channels]
    seen = set()
    for crashes in crash_schedules(spec):
        live = [c for c in chans if not (c[0] in crashes and crashes[c[0]] <= c[2])]
        for r in range(len(live) + 1):
            for om in itertools.combinations(live, r):
                fs = make_faults(frozenset(om), crashes)
                if fs not in seen:
                    seen.add(fs)
                    yield fs


def minimal_by_clocks(sets, spec: Spec):
    """The minimal elements of a collection of fault sets under inclusion of removed clock facts (ARCHITECTURE §8.3,
    LDFI Appendix B). Fault sets removing the same clock facts are one falsifier; the one with the fewest faults (then
    the canonical order) represents it."""
    by_clocks = {}
    for f in sets:
        k = f.removed_clocks(spec)
        cur = by_clocks.get(k)
        if cur is None or f.key() < cur.key():
            by_clocks[k] = f
    keys = sorted(by_clocks, key=len)
    out = []
    for i, k in enumerate(keys):
        if any(o < k for o in keys[:i] if len(o) < len(k)):
            continue
        out.append(by_clocks[k])
    return sorted(out, key=lambda f: f.labels())


def minimal_falsifiers(prog: Program, spec: Spec, limit=200_000):
    """Per post goal of the failure-free run: its Appendix-B-minimal falsifiers among the admissible fault sets."""
    require_prepost(prog)
    evl = Evaluator(prog, spec)
    ff = evl.run()
    ff_post = ff.post()
    channels = None
    if sends_cannot_grow(prog):
        channels = {(f, d, s) for _rel, f, d, s, _r in ff.messages if f != d}
    per_goal = {g: [] for g in ff_post}
    n = 0
    for fs in all_fault_sets(spec, channels):
        n += 1
        if n > limit:
            raise TooLarge(f"more than {limit} admissible fault sets")
        post = evl.run(fs).post()
        for g in ff_post:
            if g not in post:
                per_goal[g].append(fs)
    return {g: minimal_by_clocks(v, spec) for g, v in per_goal.items()}


def minimal_falsifiers_full(prog: Program, spec: Spec, limit=200_000):
    """minimal_falsifiers without the channel restriction (for self-checking the restriction)."""
    evl = Evaluator(prog, spec)
    ff_post = evl.run().post()
    per_goal = {g: [] for g in ff_post}
    for n, fs in enumerate(all_fault_sets(spec)):
        if n > limit:
            raise TooLarge(f"more than {limit} admissible fault sets")
        post = evl.run(fs).post()
        for g in ff_post:
            if g not in post:
                per_goal[g].append(fs)
    return {g: minimal_by_clocks(v, spec) for g, v in per_goal.items()}


def union_of_goal_falsifiers(per_goal):
    """TEST-028: one problem per goal, results unioned."""
    seen = {}
    for fs in per_goal.values():
        for f in fs:
            seen[tuple(f.labels())] = f
    return sorted(seen.values(), key=lambda f: f.labels())


# =====================================================================================================================
# Lineage and the lineage-driven search (ARCHITECTURE §8.3-§8.5, CrashView::MollyContinue)


class FS:
    """A fault set in the variable space of the hazard encoding: omissions O(f,t,s) and crash-order variables, where
    a crash entry (n, c) stands for K(n,c), K(n,c+1), ... ("n crashed at or before c")."""

    __slots__ = ("om", "cr", "_h")

    def __init__(self, om=frozenset(), cr=()):
        self.om = om
        self.cr = cr  # sorted ((node, time), ...)
        self._h = hash((om, cr))

    def __hash__(self):
        return self._h

    def __eq__(self, other):
        return self.om == other.om and self.cr == other.cr

    def leq(self, other):
        """self's true variables are a subset of other's."""
        if not self.om <= other.om:
            return False
        if self.cr:
            oc = dict(other.cr)
            for n, c in self.cr:
                if n not in oc or oc[n] > c:
                    return False
        return True

    def join(self, other, max_crashes):
        cr = dict(self.cr)
        for n, c in other.cr:
            if n not in cr or c < cr[n]:
                cr[n] = c
        if len(cr) > max_crashes:
            return None
        return FS(self.om | other.om, tuple(sorted(cr.items())))

    def size(self):
        return len(self.om) + len(self.cr)

    def to_faults(self):
        return make_faults(self.om, dict(self.cr))


EMPTY = FS()


def minimize(sets):
    """Minimal elements under variable inclusion (the minimal models of a monotone formula)."""
    uniq = sorted(set(sets), key=lambda s: (s.size(), s.cr, sorted(s.om)))
    out = []
    for s in uniq:
        if not any(o.leq(s) for o in out):
            out.append(s)
    return out


def cross(xs, ys, max_crashes):
    out = []
    for x in xs:
        for y in ys:
            j = x.join(y, max_crashes)
            if j is not None:
                out.append(j)
    return minimize(out)


class Lineage:
    """Hazard antichains over the rule/goal graph of one run, memoized over goals (ARCHITECTURE §8.2-§8.3).

    hazard(goal) is the list of minimal fault sets (variable space, beyond nothing) that make the goal underivable
    according to this run's lineage; [] means no admissible fault set falsifies it, [EMPTY] that it is already false.
    """

    def __init__(self, run: Run, preds, negative_support=True, crash_support=True):
        self.crash_support = crash_support
        self.run = run
        self.ev = run.ev
        self.spec = run.ev.spec
        self.prog = run.ev.prog
        self.preds = preds
        self.neg = negative_support
        self.memo = {}
        self.neg_memo = {}
        self.stack = {}
        self.low = 1 << 30
        self.crashes = run.faults.crash_map

    def model(self, t):
        return self.run.models.get(t)

    def goal(self, rel, tup, t):
        key = (rel, tup, t)
        hit = self.memo.get(key)
        if hit is not None:
            return hit
        if key in self.stack:
            # A derivation that needs the goal itself is not a support (derivation trees are finite): the repeated
            # occurrence counts as already falsified.
            self.low = min(self.low, self.stack[key])
            return [EMPTY]
        depth = len(self.stack)
        self.stack[key] = depth
        saved_low, self.low = self.low, 1 << 30
        try:
            result = self._goal(rel, tup, t)
        finally:
            del self.stack[key]
        if self.low >= depth:
            self.memo[key] = result
        self.low = min(saved_low, self.low)
        return result

    def _goal(self, rel, tup, t):
        if rel == "crash" or self.prog.is_edb(rel, tup, t):
            return []
        firings = self.firings(rel, tup, t)
        if not firings:
            raise DedError(f"lineage: no derivation found for {rel}{tup}@{t}")
        acc = [EMPTY]
        for fir in firings:
            if not fir:
                return []
            acc = cross(acc, fir, self.spec.crashes)
            if not acc:
                return []
        return acc

    def clock(self, f, d, s):
        if f == d:
            return []
        opts = []
        if self.spec.omission_ok(f, d, s):
            opts.append(FS(frozenset({(f, d, s)}), ()))
        if self.spec.crashes > 0:
            opts.append(FS(frozenset(), ((f, s),)))
        return opts

    def neg_support(self, rel, t):
        """Conservative negative support (TEST-025, CR-31) of `notin rel` read at time t."""
        if not self.neg:
            return []
        key = (rel, t)
        hit = self.neg_memo.get(key)
        if hit is not None:
            return hit
        acc = []
        for src, ded_path in sorted(self.preds.get(rel, {}).items()):
            if src == "crash":
                continue
            for tz in range(1, t + 1):
                if tz == t and not ded_path:
                    continue
                m = self.model(tz)
                if m is None:
                    continue
                for tup in sorted(m.get(src), key=repr):
                    acc += self.goal(src, tup, tz)
        res = minimize(acc)
        self.neg_memo[key] = res
        return res

    def firings(self, rel, tup, t):
        out = []
        for r in self.ev.rules_by_head.get(rel, ()):
            tb = t if r.kind == "ded" else t - 1
            if tb < 1:
                continue
            m = self.model(tb)
            if r.agg is not None:
                out += self.agg_firings(r, tup, tb, m)
                continue
            env0 = {}
            ok = True
            for x, v in zip(r.head.args, tup):
                if isinstance(x, Var):
                    if env0.get(x.name, v) != v:
                        ok = False
                        break
                    env0[x.name] = v
                elif isinstance(x, Const) and x.value != v:
                    ok = False
                    break
            if not ok:
                continue
            for env, used in self.ev.bindings(r, m, self.run.history, self.run.crash_rel, tb, env0, True):
                loc = self.ev.location(r, env)
                if not self.ev.located(r, env) or self.ev.head_tuple(r, env) != tup:
                    continue
                prem = []
                if r.kind == "async":
                    f, d = loc, tup[0]
                    if f != d and f in self.crashes and self.crashes[f] <= tb:
                        continue  # not sent
                    if f != d and (f, d, tb) in self.run.faults.omissions:
                        continue  # lost
                    prem += self.clock(f, d, tb)
                for a, ptup in used:
                    prem += self.goal(a.rel, ptup, a.time if a.time is not None else tb)
                for a in r.atoms:
                    if a.negated:
                        prem += self.negated_premise(a, env, tb)
                out.append(minimize(prem))
        return out

    def negated_premise(self, atom, env, tb):
        if atom.rel == "crash":
            return self.crash_oracle(atom, env)
        return self.neg_support(atom.rel, atom.time if atom.time is not None else tb)

    def crash_oracle(self, atom, env):
        """`notin crash(_, N, _)` holds while N is correct, so crashing N falsifies it. The crash that removes the
        fewest clock facts is the latest admissible one, K(N, EOT-1). (ARCHITECTURE §8.3 has no row for the spec
        oracle; without this premise a violation that needs a crash only for its oracle effect, such as the Kafka
        durability bug, is unreachable by lineage.)"""
        if not self.crash_support or self.spec.crashes == 0:
            return []
        x = atom.args[1]
        if isinstance(x, Const):
            cands = [x.value]
        elif isinstance(x, Var):
            cands = [env[x.name]]
        else:
            cands = list(self.spec.nodes)
        return [FS(frozenset(), ((n, self.spec.eot - 1),)) for n in cands if n in self.ev.nodes]

    def agg_firings(self, r, tup, tb, m):
        """Molly's conjunctive aggregate provenance: the aggregate depends on every contributor (TEST-024)."""
        env0 = {}
        for x, v in zip(r.head.args, tup):
            if isinstance(x, Var):
                env0[x.name] = v
        groups = self.ev.group_values(r, m, self.run.history, self.run.crash_rel, tb, env0, True)
        key = tuple(None if isinstance(x, Agg) else v for x, v in zip(r.head.args, tup))
        per_value = groups.get(key)
        if not per_value:
            return []
        agg_pos = next(i for i, x in enumerate(r.head.args) if isinstance(x, Agg))
        if self.ev.agg_value(r.agg.func, per_value.keys()) != tup[agg_pos]:
            return []
        prem = []
        for valuations in per_value.values():
            for env, used in valuations:
                for a, ptup in used:
                    prem += self.goal(a.rel, ptup, a.time if a.time is not None else tb)
                for a in r.atoms:
                    if a.negated:
                        prem += self.negated_premise(a, env, tb)
        return [minimize(prem)]


def order_key(f: Faults, spec: Spec):
    """The hypothesis queue order: fault count, then the number of clock facts removed (an Appendix-B-smaller fault
    set first, so a crash is tried at its latest useful time), then the lexicographic order of the labels."""
    return (f.count(), len(f.removed_clocks(spec)), f.labels())


def ldfi(prog: Program, spec: Spec, find_all=False, negative_support=True, max_runs=20_000, verbose=False,
         crash_support=True, vacuity_pruning=False, parity=False):
    """The core LDFI loop of ARCHITECTURE §8.4-§8.5 (and TEST-031 vacuity pruning when asked).
    Returns (verdict, counterexamples, runs)."""
    require_prepost(prog)
    evl = Evaluator(prog, spec)
    preds = reachability(prog, odd_only=parity)
    ff = evl.run()
    ff_post = ff.post()
    runs = 1
    explored = {Faults()}
    queue = []
    ces = []

    def hypotheses(run: Run):
        lin = Lineage(run, preds, negative_support, crash_support)
        cr = run.faults.crash_map
        seed = FS(frozenset(run.faults.omissions), tuple(sorted(cr.items())))
        out = set()
        post_h = {g: lin.goal("post", g, spec.eot) for g in sorted(run.post(), key=repr)}
        for g, hz in post_h.items():
            extras = []
            for h in hz:
                j = seed.join(h, spec.crashes)
                if j is None:
                    continue
                dc = tuple((n, c) for n, c in j.cr if not (n in cr and cr[n] <= c))
                extras.append(FS(j.om - seed.om, dc))
            extras = minimize(extras)
            if any(e.size() == 0 for e in extras):
                continue  # the seed already satisfies the goal's hazard: no new model beyond the seed
            for e in extras:
                j = seed.join(e, spec.crashes)
                f = j.to_faults()
                if admissible(spec, f):
                    out.add(f)
        if vacuity_pruning:
            # A hypothesis may be skipped only when it *certainly* falsifies the matching pre tuple. Conservative
            # negative support over-approximates what a fault can falsify (a lost message can make a guard true and
            # the pre tuple appear earlier, as in the Kafka case), so pre's hazard is taken from its positive lineage
            # and the crash oracle only; with negative support the pruning would drop real counterexamples.
            pre_lin = Lineage(run, preds, negative_support=False, crash_support=crash_support)
            pres = run.pre()
            pre_h = {g: pre_lin.goal("pre", g, spec.eot) for g in sorted(pres, key=repr)}

            def falsifies(f, hz):
                fs = FS(frozenset(f.omissions), f.crashes)
                return any(h.leq(fs) for h in hz)

            kept = set()
            for f in out:
                hit = [g for g, hz in post_h.items() if falsifies(f, hz)]
                if hit and all(g in pres and falsifies(f, pre_h[g]) for g in hit):
                    if verbose:
                        print(f"  pruned vacuous hypothesis {f.render()}", file=sys.stderr)
                    continue
                kept.add(f)
            out = kept
        return out

    def push(fs):
        for f in fs:
            if f not in explored:
                explored.add(f)
                heapq.heappush(queue, (order_key(f, spec), f))

    push(hypotheses(ff))
    while queue:
        _, h = heapq.heappop(queue)
        run = evl.run(h)
        runs += 1
        good = is_good(ff_post, run)
        if verbose:
            print(f"  run {runs}: {h.render()} -> {'good' if good else 'COUNTEREXAMPLE'}", file=sys.stderr)
        if not good:
            ces.append(h)
            if not find_all:
                break
            continue
        if runs >= max_runs:
            raise TooLarge(f"LDFI exceeded {max_runs} runs")
        push(hypotheses(run))
    return ("counterexample" if ces else "no_counterexample"), ces, runs


# =====================================================================================================================
# Corpus checks

CORPUS = pathlib.Path(__file__).resolve().parents[1]


def case_files(case: pathlib.Path, m):
    files = [case / m["program"]]
    files += [case / x for x in m.get("include", [])]
    return files


def check_case(case: pathlib.Path, opts):
    m = tomllib.loads((case / "manifest.toml").read_text())
    ld = m.get("expect_ldfi")
    lines = []
    ok = True
    if ld is not None:
        if ld.get("crash_view", "molly") != "molly":
            return True, [f"skipped: crash_view {ld.get('crash_view')} is not modelled"]
        spec = Spec(ld["eot"], ld["eff"], ld["crashes"], tuple(ld["nodes"]))
        try:
            prog = load_program(case_files(case, m))
            require_prepost(prog)
        except DedError as e:
            if ld["verdict"] == "program_error":
                return True, [f"program error as expected: {e}"]
            return False, [f"program error: {e}"]
        if ld["verdict"] == "program_error":
            return False, ["expected a program error, but the program loads"]
        want = ld["verdict"]
        t0 = wallclock.time()
        if not opts.no_exhaustive:
            try:
                v, w, st = exhaustive(prog, spec, stop_at_first=(want == "counterexample"), max_states=opts.max_states)
                good = v == want
                ok &= good
                lines.append(f"exhaustive={v}{' ' + w.render() if w else ''} states={st['states']}"
                             f"{'' if good else '  MISMATCH'}")
            except TooLarge as e:
                lines.append(f"exhaustive: {e}")
        try:
            # the search reductions a case lists are the ones it may rely on for its run count
            vac = "TEST-031" in m.get("features", [])
            v, ces, runs = ldfi(prog, spec, max_runs=opts.max_runs, vacuity_pruning=vac)
            good = v == want
            ok &= good
            note = ""
            if "runs_max" in ld:
                over = runs > ld["runs_max"]
                note = f" runs_max={ld['runs_max']}" + (" (EXCEEDED)" if over else "")
                if over and opts.strict_runs:
                    ok = False
            lines.append(f"ldfi{'+vacuity' if vac else ''}={v} runs={runs}{note}{'' if good else '  MISMATCH'}")
        except TooLarge as e:
            lines.append(f"ldfi: {e}")
            if "runs_max" in ld and opts.strict_runs:
                ok = False
        if "falsifiers" in ld:
            try:
                per_goal = minimal_falsifiers(prog, spec)
                got = sorted(f.labels() for f in union_of_goal_falsifiers(per_goal))
                want_f = sorted(sorted(x.replace(" ", "") for x in fs) for fs in ld["falsifiers"])
                good = got == want_f
                ok &= good
                lines.append("falsifiers match" if good else f"falsifiers DIFFER: got {got}")
            except TooLarge as e:
                lines.append(f"falsifiers: {e}")
        lines.append(f"({wallclock.time() - t0:.1f}s)")
    exp = [x for x in m.get("expect", []) if "rel" in x]
    if exp and m.get("program", "").endswith(".ded") and "oracle" in m.get("backend", {}):
        good, msg = check_oracle_expectations(case, m, exp)
        ok &= good
        lines.append(msg)
    return ok, lines


def parse_range(r, last):
    r = str(r)
    if ".." not in r:
        return range(int(r), int(r) + 1)
    lo, hi = r.split("..")
    lo = int(lo) if lo else 0
    if hi.startswith("="):
        return range(lo, int(hi[1:]) + 1)
    return range(lo, last + 1)


def check_oracle_expectations(case, m, exp):
    """Failure-free sync run of a `.ded` case (CR-13: tick 0 has no events; round k is tick k)."""
    nodes = tuple(n["name"] for n in m.get("deploy", {}).get("nodes", []))
    ticks = m.get("run", {}).get("ticks")
    if not nodes or not ticks:
        return False, "oracle: a .ded oracle case needs [deploy] nodes and [run] ticks"
    last = ticks - 1
    prog = load_program(case_files(case, m))
    spec = Spec(max(last, 1), 0, 0, nodes)
    run = Evaluator(prog, spec).run()

    def at(node, rel, t):
        if t == 0:
            return set()
        return {tup[1:] for tup in run.rel_at(rel, t) if tup and tup[0] == node}

    bad = []
    for x in exp:
        node, rel = x["node"], x["rel"]
        if "tick" in x:
            got = at(node, rel, x["tick"])
            want = {tuple(r) for r in x["rows"]}
            if got != want:
                bad.append(f"{rel}@{node} tick {x['tick']}: got {sorted(got)}, want {sorted(want)}")
            continue
        row = tuple(x["row"])
        for key, present in (("holds", True), ("absent", False)):
            if key in x:
                for t in parse_range(x[key], last):
                    if (row in at(node, rel, t)) != present:
                        bad.append(f"{rel}{row}@{node} tick {t}: expected {'present' if present else 'absent'}")
    return (not bad), ("oracle expectations hold" if not bad else "oracle MISMATCH: " + "; ".join(bad[:6]))


def cmd_check(a):
    cases = [pathlib.Path(c).resolve() for c in a.cases]
    if not cases:
        cases = sorted(p.parent for p in (CORPUS / "molly").rglob("manifest.toml"))
    bad = 0
    for case in cases:
        try:
            ok, lines = check_case(case, a)
        except DedError as e:
            ok, lines = False, [f"error: {e}"]
        bad += 0 if ok else 1
        print(f"{'ok ' if ok else 'BAD'} {case.relative_to(CORPUS)}: {'  '.join(lines)}", flush=True)
    print(f"{len(cases)} cases, {bad} problems")
    return 1 if bad else 0


# =====================================================================================================================
# Self tests (small programs whose answers follow directly from the definitions)

SELFTEST_43 = """
// the LDFI paper §4.3 example: two proofs of got(c,v); a also sends at time 1, so its first send is at 1
recv(D, S, V)@async :- send(S, D, V);
ping(D, S)@async :- poke(S, D);
got(N, V) :- recv(N, _, V);
got(N, V)@next :- got(N, V);
send("b", "c", "v")@1;
send("a", "c", "v")@2;
poke("a", "b")@1;
pre(V) :- send(S, "c", V)@1;
post(V) :- got(N, V);
"""


def selftest():
    import tempfile
    failures = []
    with tempfile.TemporaryDirectory() as d:
        p = pathlib.Path(d) / "f43.ded"
        p.write_text(SELFTEST_43)
        prog = load_program([p])
        cases = [
            ((4, 3, 1), [["O(a,c,2)", "O(b,c,1)"]], "counterexample"),
            ((4, 2, 1), [["C(a,2)", "O(b,c,1)"]], "counterexample"),
            ((4, 0, 1), [], "no_counterexample"),
            ((4, 0, 2), [["C(a,2)", "C(b,1)"]], "counterexample"),
        ]
        for (eot, eff, cr), want_f, want_v in cases:
            spec = Spec(eot, eff, cr, ("a", "b", "c"))
            got = sorted(f.labels() for f in union_of_goal_falsifiers(minimal_falsifiers(prog, spec)))
            if got != sorted(want_f):
                failures.append(f"§4.3 {eot}/{eff}/{cr}: falsifiers {got} != {want_f}")
            v, _, _ = exhaustive(prog, spec)
            if v != want_v:
                failures.append(f"§4.3 {eot}/{eff}/{cr}: exhaustive {v} != {want_v}")
            v2, _, runs = ldfi(prog, spec)
            if v2 != want_v:
                failures.append(f"§4.3 {eot}/{eff}/{cr}: ldfi {v2} != {want_v}")
            if want_v == "counterexample" and runs != 2:
                failures.append(f"§4.3 {eot}/{eff}/{cr}: ldfi took {runs} runs, the failure-free lineage gives the "
                                f"counterexample directly")
            # the falsifier search restricted to failure-free message channels must agree with the full search
            if sends_cannot_grow(prog):
                full = union_of_goal_falsifiers(minimal_falsifiers_full(prog, spec))
                if sorted(f.labels() for f in full) != got:
                    failures.append(f"§4.3 {eot}/{eff}/{cr}: restricted falsifier search differs from the full one")
        # the hazard formula itself (variable space): (O(a,c,2) v K(a,2)) & (O(b,c,1) v K(b,1)) with EFF 3, 2 crashes
        spec = Spec(4, 3, 2, ("a", "b", "c"))
        evl = Evaluator(prog, spec)
        ff = evl.run()
        lin = Lineage(ff, reachability(prog))
        h = lin.goal("post", ("v",), 4)
        got = sorted(sorted(FS.to_faults(x).labels()) for x in h)
        want = sorted([["O(a,c,2)", "O(b,c,1)"], ["C(a,2)", "O(b,c,1)"], ["C(b,1)", "O(a,c,2)"], ["C(a,2)", "C(b,1)"]])
        if got != want:
            failures.append(f"§4.3 minimal models {got} != {want}")
    for f in failures:
        print("FAIL", f)
    print("selftest:", "ok" if not failures else f"{len(failures)} failures")
    return 1 if failures else 0


# =====================================================================================================================
# CLI


def parse_spec(a):
    return Spec(a.eot, a.eff, a.crashes, tuple(a.nodes.split(",")))


def parse_faults(a):
    om = set()
    for x in filter(None, (a.omit or "").split(",")):
        f, d, s = x.split(":")
        om.add((f, d, int(s)))
    cr = {}
    for x in filter(None, (a.crash or "").split(",")):
        n, c = x.split(":")
        cr[n] = int(c)
    return make_faults(frozenset(om), cr)


def cmd_run(a):
    prog = load_program(a.files)
    spec = parse_spec(a)
    evl = Evaluator(prog, spec)
    faults = parse_faults(a)
    run = evl.run(faults)
    if a.dump:
        for t in range(1, spec.eot + 1):
            print(f"--- time {t}")
            m = run.models[t]
            for rel in sorted(m.rels):
                for tup in sorted(m.rels[rel], key=repr):
                    print(f"  {rel}{tup}")
        for rel, f, d, s, r in run.messages:
            print(f"msg {rel} {f}->{d} sent {s} {'LOST' if r is None else 'received ' + str(r)}")
    print(f"faults: {faults.render()}")
    if "pre" in prog.arity and "post" in prog.arity:
        ff = evl.run()
        print(f"pre@EOT:  {sorted(run.pre())}")
        print(f"post@EOT: {sorted(run.post())}")
        print("good" if is_good(ff.post(), run) else "BAD (counterexample)")
    return 0


def cmd_verdict(a):
    prog = load_program(a.files)
    spec = parse_spec(a)
    out = {}
    if a.mode in ("exhaustive", "both"):
        t0 = wallclock.time()
        v, w, st = exhaustive(prog, spec, stop_at_first=not a.find_all, max_states=a.max_states)
        out["exhaustive"] = v
        print(f"exhaustive: {v}  witness={w.render() if w else '-'}  schedules={st['schedules']} states={st['states']}"
              f"  ({wallclock.time() - t0:.1f}s)")
        print(f"  failure-free pre@EOT={st['ff_pre']}  post@EOT={st['ff_post']}")
    if a.mode in ("ldfi", "both"):
        t0 = wallclock.time()
        v, ces, runs = ldfi(prog, spec, find_all=a.find_all, verbose=a.verbose,
                            negative_support=not a.no_negative_support, max_runs=a.max_runs,
                            crash_support=not a.no_crash_support, vacuity_pruning=a.vacuity, parity=a.parity)
        out["ldfi"] = v
        print(f"ldfi: {v}  runs={runs}  counterexamples={[c.render() for c in ces]}  ({wallclock.time() - t0:.1f}s)")
    if len(set(out.values())) > 1:
        print("MISMATCH between exhaustive and ldfi")
        return 1
    return 0


def cmd_falsifiers(a):
    prog = load_program(a.files)
    spec = parse_spec(a)
    per_goal = minimal_falsifiers(prog, spec)
    for g, fs in sorted(per_goal.items(), key=lambda kv: repr(kv[0])):
        print(f"post{g}: {[f.render() for f in fs]}")
    print("union:", [f.labels() for f in union_of_goal_falsifiers(per_goal)])
    return 0


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)

    def common(p):
        p.add_argument("files", nargs="+")
        p.add_argument("--eot", type=int, required=True)
        p.add_argument("--eff", type=int, required=True)
        p.add_argument("--crashes", type=int, default=0)
        p.add_argument("--nodes", required=True)

    p = sub.add_parser("run")
    common(p)
    p.add_argument("--omit")
    p.add_argument("--crash")
    p.add_argument("--dump", action="store_true")
    p = sub.add_parser("verdict")
    common(p)
    p.add_argument("--mode", default="both", choices=["exhaustive", "ldfi", "both"])
    p.add_argument("--find-all", action="store_true")
    p.add_argument("--no-negative-support", action="store_true")
    p.add_argument("--no-crash-support", action="store_true")
    p.add_argument("--vacuity", action="store_true", help="TEST-031: prune vacuous hypotheses before running them")
    p.add_argument("--parity", action="store_true", help="TEST-025: odd-negation parity filter on negative support")
    p.add_argument("--verbose", action="store_true")
    p.add_argument("--max-states", type=int, default=2_000_000)
    p.add_argument("--max-runs", type=int, default=20_000)
    p = sub.add_parser("falsifiers")
    common(p)
    p = sub.add_parser("check")
    p.add_argument("cases", nargs="*")
    p.add_argument("--no-exhaustive", action="store_true")
    p.add_argument("--max-states", type=int, default=2_000_000)
    p.add_argument("--max-runs", type=int, default=20_000)
    p.add_argument("--strict-runs", action="store_true", help="count a run count above runs_max as a problem")
    sub.add_parser("selftest")
    a = ap.parse_args(argv)
    try:
        if a.cmd == "selftest":
            return selftest()
        return {"run": cmd_run, "verdict": cmd_verdict, "falsifiers": cmd_falsifiers, "check": cmd_check}[a.cmd](a)
    except (DedError, TooLarge) as e:
        print(f"error: {e}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
