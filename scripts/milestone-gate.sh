#!/usr/bin/env bash
# Closes milestone Mk (PLAN §3). Run by the orchestrator on the integration branch, from a clean tree.
#
#   scripts/milestone-gate.sh Mk [--no-merge] [--no-commit]
#
#   1. merges every wp/<id> branch of Mk (ids from docs/design/plan.json) into the current branch; a Cargo.lock
#      conflict takes the base version, and the lock is regenerated with `cargo metadata` and
#      `cargo check --workspace --all-targets`; any other conflict aborts the gate;
#   2. scripts/collect-notes.sh (bugs to docs/plan/BUGS.md, dependencies to docs/design/DEPENDENCIES.md);
#   3. scripts/ci.sh gate;
#   4. from M5 on: `xtask corpus --ratchet --milestone Mk`, then `xtask corpus --check --gate`;
#   5. from M5 on: `xtask coverage`, committing docs/plan/coverage.md;
#   6. writes the next milestone id into docs/plan/MILESTONE and commits "Mk: <title>".
#
# --no-merge skips step 1 (the branches are merged already); --no-commit stops before step 6's commit. Any failing
# step stops the gate with a non-zero status; the orchestrator then runs a gate-fix agent (PLAN §3) and re-runs it.
# Merges already done are no-ops, so re-running is safe.
#
# The script works on the git repository of the current directory. For M1 the integration branch does not have
# the scripts yet (M1.1 adds them), so run this copy from there, e.g.
#   git show wp/M1.1:scripts/milestone-gate.sh > /tmp/gate.sh && bash /tmp/gate.sh M1
# docs/plan/MILESTONE may be missing before the merge only for M1.
set -uo pipefail

ROOT="$(git rev-parse --show-toplevel 2>/dev/null)" || { echo "gate: not inside a git repository" >&2; exit 2; }
cd "$ROOT" || exit 1

usage() { sed -n '2,26p' "$0" >&2; exit 2; }
[ "$#" -ge 1 ] || usage
MS="$1"
shift
merge=1
commit=1
for arg in "$@"; do
  case "$arg" in
    --no-merge) merge=0 ;;
    --no-commit) commit=0 ;;
    *) usage ;;
  esac
done
[[ "$MS" =~ ^M([0-9]+)$ ]] || { echo "gate: '$MS' is not a milestone id (M1 … M15)" >&2; exit 2; }
N="${BASH_REMATCH[1]}"

die() { echo "gate $MS: FAILED: $*" >&2; exit 1; }
note() { echo "gate $MS: $*"; }

milestone_file_matches() {
  local current
  current="$(tr -d '[:space:]' < docs/plan/MILESTONE 2>/dev/null || true)"
  [ "$current" = "$MS" ] || die "docs/plan/MILESTONE says '${current:-nothing}', not $MS"
}
if [ -f docs/plan/MILESTONE ] || [ "$MS" != M1 ]; then
  milestone_file_matches
fi
[ -z "$(git status --porcelain)" ] || die "the working tree is not clean"

read_plan() {
  python3 - "$MS" <<'PY'
import json, sys
plan = json.load(open("docs/design/plan.json"))
ms = next((m for m in plan["milestones"] if m["id"] == sys.argv[1]), None)
if ms is None:
    sys.exit(f"{sys.argv[1]} is not in docs/design/plan.json")
print(ms["title"])
for wp in ms["wps"]:
    print(wp["id"])
PY
}
plan_out="$(read_plan)" || die "cannot read the plan"
TITLE="$(printf '%s\n' "$plan_out" | head -n1)"
WPS="$(printf '%s\n' "$plan_out" | tail -n +2)"

commit_if_changed() {
  # $1: message; remaining: paths to add.
  local message="$1"
  shift
  git add -A -- "$@"
  if ! git diff --cached --quiet; then
    git commit -q -m "$message" || die "cannot commit: $message"
    note "committed: $message"
  fi
}

# ---- 1. merge ----------------------------------------------------------------------------------------------------
if [ "$merge" = 1 ]; then
  branch_now="$(git rev-parse --abbrev-ref HEAD)"
  for wp in $WPS; do
    branch="wp/$wp"
    git rev-parse --verify --quiet "$branch" >/dev/null || die "branch $branch does not exist"
    if git merge-base --is-ancestor "$branch" HEAD; then
      note "$branch is merged already"
      continue
    fi
    note "merging $branch into $branch_now"
    if ! git merge -q --no-ff --no-edit -m "Merge $branch for the $MS gate" "$branch"; then
      conflicted="$(git diff --name-only --diff-filter=U)"
      if [ "$conflicted" = "Cargo.lock" ]; then
        git checkout --ours -- Cargo.lock && git add Cargo.lock && git commit -q --no-edit \
          || die "cannot resolve the Cargo.lock conflict of $branch"
        note "$branch: Cargo.lock conflict resolved with the base version (regenerated below)"
      else
        git merge --abort
        die "$branch conflicts outside Cargo.lock (paths are disjoint by plan): $(echo "$conflicted" | tr '\n' ' ')"
      fi
    fi
  done
  note "regenerating Cargo.lock"
  cargo metadata --format-version 1 >/dev/null || die "cargo metadata failed"
  cargo check --workspace --all-targets || die "cargo check --workspace --all-targets failed after merging"
  commit_if_changed "$MS gate: regenerate Cargo.lock" Cargo.lock
fi
milestone_file_matches

# ---- 2. notes ----------------------------------------------------------------------------------------------------
scripts/collect-notes.sh || die "collect-notes failed"
commit_if_changed "$MS gate: collect bugs and dependencies from the WP notes" docs/plan/BUGS.md docs/design/DEPENDENCIES.md

# ---- 3. CI -------------------------------------------------------------------------------------------------------
scripts/ci.sh gate || die "scripts/ci.sh gate failed"

# ---- 4-5. corpus and coverage (from M5 on) ------------------------------------------------------------------------
if [ "$N" -ge 5 ]; then
  cargo run -q -p xtask -- corpus --ratchet --milestone "$MS" || die "corpus --ratchet failed"
  commit_if_changed "$MS gate: corpus status ratchet" tests/corpus
  cargo run -q -p xtask -- corpus --check --gate || die "corpus --check --gate failed"
  cargo run -q -p xtask -- coverage || die "coverage failed"
  commit_if_changed "$MS gate: coverage report" docs/plan/coverage.md
fi

# ---- 6. close ----------------------------------------------------------------------------------------------------
if [ "$N" -ge 15 ]; then
  next="done"
else
  next="M$((N + 1))"
fi
if [ "$commit" = 0 ]; then
  note "passed; --no-commit: docs/plan/MILESTONE left at $MS (next would be $next)"
  exit 0
fi
echo "$next" > docs/plan/MILESTONE
git add docs/plan/MILESTONE
git commit -q -m "$MS: $TITLE" || die "cannot commit the milestone"
note "closed: \"$MS: $TITLE\"; docs/plan/MILESTONE is now $next"
