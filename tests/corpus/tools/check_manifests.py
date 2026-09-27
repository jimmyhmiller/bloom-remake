#!/usr/bin/env python3
"""Static validator for golden-corpus manifests (schema v1, docs/design/PLAN.md §5).

Provided with the delivery plan so the M1 corpus work packages validate against one schema before the Rust
corpus runner exists. WP M5.2 re-implements every check in `cargo xtask corpus --lint` and then deletes this file.

Usage:
    python3 tests/corpus/tools/check_manifests.py <area> [--require-ids BENCH-001..048[,BENCH-050]] [--skip ID[,ID]]

<area> is a directory under tests/corpus (for example `core`, `ldfi`, `std/delivery`). Every directory below it that
contains a manifest.toml is a case. Exit status 0 when every manifest is valid and every required id has a case.
"""
import argparse
import pathlib
import re
import sys
import tomllib

ROOT = pathlib.Path(__file__).resolve().parents[3]
CORPUS = ROOT / "tests" / "corpus"
FEATURES = ROOT / "docs" / "research" / "FEATURES.md"

BACKENDS = ["compile", "analysis", "oracle", "interp", "codegen", "sim", "ldfi", "bmc", "smt", "asp"]
FLOORS = {"compile": 5, "analysis": 5, "oracle": 5, "interp": 7, "sim": 7, "ldfi": 8, "codegen": 8, "bmc": 9,
          "smt": 10, "asp": 10}
STATUSES = {"pass", "unimplemented", "known-failure"}
TOP_KEYS = {"schema", "id", "title", "priority", "source", "features", "program", "programs", "spec", "include",
            "derived", "expected_from", "notes", "deploy", "run", "input", "fault", "backend", "expect",
            "expect_send", "expect_error", "expect_diag", "expect_analysis", "expect_ldfi", "expect_verify", "perf"}
REQUIRED = {"schema", "id", "title", "priority", "source", "features", "backend"}
RUN_KEYS = {"ticks", "stop", "seeds", "replay_check", "swarm"}
DEPLOY_KEYS = {"nodes", "params", "seed"}
FAULT_KINDS = {"crash": {"node", "tick"}, "restart": {"node", "tick"}, "omit": {"from", "to", "send_tick"},
               "partition": {"from", "to", "ticks"}, "reject": {"from", "to", "send_tick", "reason"}}
ANALYSIS_KEYS = {"points_of_order", "strata", "certificates", "calm_labels", "finality", "blazes", "reclaimable",
                 "confluent", "deterministic", "fair_consistency"}
LDFI_KEYS = {"eot", "eff", "crashes", "nodes", "crash_view", "verdict", "runs_max", "falsifiers"}
VERIFY_KEYS = {"check", "result", "bounds"}
RANGE = re.compile(r"^(\d+(\.\.(=\d+)?)?|\.\.=\d+)$")
DIR_RE = re.compile(r"^(?P<id>[A-Z]+-\d{3})(?P<case>[a-z]?)-(?P<slug>[a-z0-9][a-z0-9-]*)$")
EXAMPLE_DIR_RE = re.compile(r"^E\d\d-[a-z0-9][a-z0-9-]*$")
FALSIFIER = re.compile(r"^(O\([^,()]+,[^,()]+,\d+\)|C\([^,()]+,\d+\))$")


def load_features():
    ids = {}
    for line in FEATURES.read_text().splitlines():
        m = re.match(r"^- \*\*([A-Z]+-\d+)\*\* `(P\d)`", line)
        if m:
            ids[m.group(1)] = m.group(2)
    return ids


def expand(spec, known):
    out = []
    for part in [p for p in spec.split(",") if p]:
        m = re.fullmatch(r"([A-Z]+)-(\d+)\.\.(?:[A-Z]+-)?(\d+)", part)
        if m:
            area, lo, hi = m.group(1), int(m.group(2)), int(m.group(3))
            out += [k for k in known if k.startswith(area + "-") and lo <= int(k.split("-")[1]) <= hi]
        else:
            out.append(part)
    return out


def is_rows(v):
    return isinstance(v, list) and all(isinstance(r, list) for r in v)


def check_case(path, m, known, err):
    def e(msg):
        err.append(f"{path.relative_to(ROOT)}: {msg}")

    for k in m:
        if k not in TOP_KEYS:
            e(f"unknown key `{k}`")
    for k in REQUIRED:
        if k not in m:
            e(f"missing required key `{k}`")
    if m.get("schema") != 1:
        e("`schema` must be 1")
    fid = m.get("id")
    if fid not in known:
        e(f"`id` {fid!r} is not a FEATURES.md id")
    elif m.get("priority") != known[fid]:
        e(f"`priority` {m.get('priority')!r} differs from FEATURES.md ({known[fid]})")
    d = path.parent.name
    dm = DIR_RE.match(d)
    if not EXAMPLE_DIR_RE.match(d):
        if not dm:
            e(f"directory name `{d}` must be `<ID>[a-z]-<slug>`")
        elif dm.group("id") != fid:
            e(f"directory id {dm.group('id')} differs from manifest id {fid}")
    for k in ("title", "source"):
        if k in m and (not isinstance(m[k], str) or not m[k].strip()):
            e(f"`{k}` must be a non-empty string")
    feats = m.get("features")
    if not isinstance(feats, list) or not feats:
        e("`features` must be a non-empty list")
    else:
        for f in feats:
            if f not in known:
                e(f"`features` contains unknown id {f!r}")
    has_prog = "program" in m
    has_progs = "programs" in m
    if has_prog == has_progs:
        e("exactly one of `program` or `programs` is required")
    files = [m["program"]] if has_prog else list((m.get("programs") or {}).values())
    if "spec" in m:
        files.append(m["spec"])
    files += m.get("include", [])
    for f in files:
        if not isinstance(f, str) or not (path.parent / f).is_file():
            e(f"referenced file {f!r} does not exist")
    if m.get("expected_from", "literature") not in ("literature", "blessed"):
        e("`expected_from` must be `literature` or `blessed`")
    dep = m.get("deploy", {})
    for k in dep:
        if k not in DEPLOY_KEYS:
            e(f"unknown [deploy] key `{k}`")
    names = set()
    for n in dep.get("nodes", []):
        if not isinstance(n, dict) or set(n) != {"name", "role"}:
            e("[deploy] nodes entries must be {name, role}")
        else:
            names.add(n["name"])
    run = m.get("run", {})
    for k in run:
        if k not in RUN_KEYS:
            e(f"unknown [run] key `{k}`")
    for k in ("ticks", "seeds"):
        if k in run and (not isinstance(run[k], int) or run[k] <= 0):
            e(f"[run] {k} must be a positive integer")
    if "stop" in run and run["stop"] not in ("quiescent", "ticks"):
        e("[run] stop must be `quiescent` or `ticks`")
    for i in m.get("input", []):
        if set(i) != {"node", "tick", "rel", "rows"} or not is_rows(i.get("rows")):
            e("[[input]] needs exactly node, tick, rel, rows (a list of rows)")
        elif names and i["node"] not in names:
            e(f"[[input]] node {i['node']!r} is not deployed")
    for f in m.get("fault", []):
        kind = f.get("kind")
        if kind not in FAULT_KINDS:
            e(f"[[fault]] kind {kind!r} unknown")
        elif set(f) - {"kind"} != FAULT_KINDS[kind]:
            e(f"[[fault]] {kind} needs exactly {sorted(FAULT_KINDS[kind])}")
    backends = m.get("backend", {})
    if not isinstance(backends, dict) or not backends:
        e("at least one [backend.<name>] table is required")
        backends = {}
    for b, t in backends.items():
        if b not in BACKENDS:
            e(f"unknown backend `{b}`")
            continue
        st = t.get("status")
        if st not in STATUSES:
            e(f"[backend.{b}] status must be one of {sorted(STATUSES)}")
        for k in t:
            if k not in ("status", "unimplemented", "until", "issue"):
                e(f"[backend.{b}] unknown key `{k}`")
        if st == "unimplemented":
            u = t.get("unimplemented")
            if not isinstance(u, list) or not u or any(x not in known for x in u):
                e(f"[backend.{b}] `unimplemented` must be a non-empty list of FEATURES ids")
        elif "unimplemented" in t:
            e(f"[backend.{b}] `unimplemented` only allowed with status = unimplemented")
        if st != "pass":
            until = t.get("until", "")
            mm = re.fullmatch(r"M(\d+)", until)
            if not mm:
                e(f"[backend.{b}] `until` must be a milestone id like M6")
            elif int(mm.group(1)) < FLOORS[b]:
                e(f"[backend.{b}] until {until} is before the backend floor M{FLOORS[b]}")
        if st == "known-failure" and not t.get("issue"):
            e(f"[backend.{b}] known-failure requires `issue`")
    if "ldfi" in backends and "expect_ldfi" not in m:
        e("[backend.ldfi] requires [expect_ldfi]")
    for v in ("bmc", "smt", "asp"):
        if v in backends and m.get("expect_verify", {}).get("check") != v:
            e(f"[backend.{v}] requires [expect_verify] check = {v!r}")
    if "analysis" in backends and "expect_analysis" not in m and "expect_diag" not in m:
        e("[backend.analysis] requires [expect_analysis] or [[expect_diag]]")
    for x in m.get("expect", []):
        keys = set(x)
        if keys == {"quiescent_from"}:
            continue
        base = {"node", "rel"}
        ok = (base | {"row"} <= keys and keys - base - {"row"} and keys - base - {"row"} <= {"holds", "absent"}) or \
             keys == base | {"tick", "rows"} or keys == base | {"final", "rows"}
        if not ok:
            e(f"[[expect]] has an unsupported key combination {sorted(keys)}")
            continue
        for r in ("holds", "absent"):
            if r in x and not RANGE.match(str(x[r])):
                e(f"[[expect]] {r} {x[r]!r} is not a tick range like 3..=5, 3.., ..=5 or 3")
        if "rows" in x and not is_rows(x["rows"]):
            e("[[expect]] rows must be a list of rows")
        if "row" in x and not isinstance(x["row"], list):
            e("[[expect]] row must be a list")
        if names and x["node"] not in names:
            e(f"[[expect]] node {x['node']!r} is not deployed")
    for x in m.get("expect_send", []):
        if not {"from", "to", "channel", "row"} <= set(x) or set(x) - {"from", "to", "channel", "row", "tick", "count"}:
            e("[[expect_send]] needs from, to, channel, row (optional tick, count)")
    for x in m.get("expect_error", []):
        if set(x) != {"code", "node", "tick"} or not re.fullmatch(r"BLSR\d{3}", str(x.get("code"))):
            e("[[expect_error]] needs code (BLSRnnn), node, tick")
    for x in m.get("expect_diag", []):
        if "code" not in x or set(x) - {"code", "line", "severity"} or not re.fullmatch(r"BLS\d{4}", str(x["code"])):
            e("[[expect_diag]] needs code (BLSnnnn) and optional line, severity")
    for k in m.get("expect_analysis", {}):
        if k not in ANALYSIS_KEYS:
            e(f"[expect_analysis] unknown key `{k}`")
    ld = m.get("expect_ldfi")
    if ld is not None:
        for k in ld:
            if k not in LDFI_KEYS:
                e(f"[expect_ldfi] unknown key `{k}`")
        for k in ("eot", "eff", "crashes", "nodes", "verdict"):
            if k not in ld:
                e(f"[expect_ldfi] missing `{k}`")
        if ld.get("verdict") not in ("counterexample", "no_counterexample", "program_error"):
            e("[expect_ldfi] verdict must be counterexample, no_counterexample or program_error")
        if ld.get("crash_view", "molly") not in ("molly", "frozen"):
            e("[expect_ldfi] crash_view must be molly or frozen")
        for fs in ld.get("falsifiers", []):
            if not isinstance(fs, list) or any(not FALSIFIER.match(str(x).replace(" ", "")) for x in fs):
                e("[expect_ldfi] falsifiers must be lists of O(from,to,send_tick) / C(node,tick)")
    ver = m.get("expect_verify")
    if ver is not None:
        for k in ver:
            if k not in VERIFY_KEYS:
                e(f"[expect_verify] unknown key `{k}`")
        if ver.get("check") not in ("bmc", "smt", "asp", "sim") or ver.get("result") not in ("holds", "fails"):
            e("[expect_verify] needs check (bmc|smt|asp|sim) and result (holds|fails)")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("area")
    ap.add_argument("--require-ids", default="")
    ap.add_argument("--skip", default="")
    a = ap.parse_args()
    known = load_features()
    base = CORPUS / a.area
    err = []
    manifests = sorted(base.rglob("manifest.toml")) if base.is_dir() else []
    if not manifests:
        err.append(f"no cases under {base.relative_to(ROOT)}")
    seen = set()
    for p in manifests:
        try:
            m = tomllib.loads(p.read_text())
        except tomllib.TOMLDecodeError as x:
            err.append(f"{p.relative_to(ROOT)}: TOML error: {x}")
            continue
        check_case(p, m, known, err)
        seen.add(m.get("id"))
    skip = set(expand(a.skip, known))
    for fid in expand(a.require_ids, known):
        if known.get(fid) in ("P0", "P1") and fid not in skip and fid not in seen:
            err.append(f"required id {fid} has no case under {base.relative_to(ROOT)}")
    for x in err:
        print(x)
    print(f"{len(manifests)} manifests checked, {len(err)} problems")
    return 1 if err else 0


if __name__ == "__main__":
    sys.exit(main())
