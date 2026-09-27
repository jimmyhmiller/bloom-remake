#!/usr/bin/env bash
# Collects the notes of every work package at a milestone gate (PLAN §2.7, §3 step 2):
#   - each new item of a `## Bugs` section of docs/plan/notes/<WP>.md becomes a numbered row of docs/plan/BUGS.md;
#   - each new row of a `## New dependencies` table becomes a row of docs/design/DEPENDENCIES.md.
# Items already collected (same source) are skipped, so re-running is harmless. The formats are described in
# docs/plan/notes/README.md.
#
#   scripts/collect-notes.sh [--check]    --check: report what would be added, change nothing
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
exec python3 - "$@" <<'PY'
import pathlib, re, sys

check = sys.argv[1:] == ["--check"]
if sys.argv[1:] and not check:
    sys.exit("usage: scripts/collect-notes.sh [--check]")
notes_dir = pathlib.Path("docs/plan/notes")
bugs_path = pathlib.Path("docs/plan/BUGS.md")
deps_path = pathlib.Path("docs/design/DEPENDENCIES.md")
BUGS_HEADER = "| # | Source | Crate | Summary | Status |"
DEPS_HEADER = "| Crate | Version | License | Used by | Reason | Source |"

def section(text, title):
    lines, out, inside = text.splitlines(), [], False
    for line in lines:
        if line.startswith("## "):
            inside = line[3:].strip().lower() == title.lower()
            continue
        if inside:
            out.append(line)
    return out

def bullets(lines):
    items, cur = [], None
    for line in lines:
        if line.startswith("- "):
            cur = [line[2:].strip()]
            items.append(cur)
        elif cur is not None and line.startswith("  ") and line.strip():
            cur.append(line.strip())
        elif not line.strip():
            cur = None
    return [" ".join(i) for i in items]

def cell(s):
    return s.replace("|", "\\|").strip()

def table_rows(lines):
    rows = []
    for line in lines:
        line = line.strip()
        if not (line.startswith("|") and line.endswith("|")):
            continue
        cells = [c.strip() for c in re.split(r"(?<!\\)\|", line[1:-1])]
        if all(set(c) <= set("-: ") for c in cells) or cells[0].lower() == "crate":
            continue
        rows.append(cells)
    return rows

def read_table(path, header):
    text = path.read_text()
    if header not in text:
        sys.exit(f"collect-notes: {path} lacks the table header `{header}`")
    return text

bugs_text = read_table(bugs_path, BUGS_HEADER)
deps_text = read_table(deps_path, DEPS_HEADER)
known_sources = set(re.findall(r"\| \[([^\]]+)\]\(notes/", bugs_text))
numbers = [int(n) for n in re.findall(r"^\| (\d+) \|", bugs_text, re.M)]
next_no = max(numbers, default=0) + 1
known_deps = {(r[0], r[3]) for r in table_rows(deps_text.split(DEPS_HEADER, 1)[1].splitlines())}

new_bugs, new_deps = [], []
for note in sorted(notes_dir.glob("*.md")):
    if note.name == "README.md":
        continue
    wp = note.stem
    text = note.read_text()
    for k, item in enumerate(bullets(section(text, "Bugs")), start=1):
        if re.fullmatch(r"(none|n/a|—|-)\.?", item.strip(), re.I):
            continue
        source = f"{wp}#{k}"
        if source in known_sources:
            continue
        m = re.match(r"`([^`]+)`\s*[:—-]\s*(.*)", item)
        crate, summary = (m.group(1), m.group(2)) if m else ("unknown", item)
        new_bugs.append(f"| {next_no} | [{source}](notes/{note.name}) | `{cell(crate)}` | {cell(summary)} | open |")
        next_no += 1
    for row in table_rows(section(text, "New dependencies")):
        if len(row) < 5:
            sys.exit(f"collect-notes: {note}: a New dependencies row needs Crate | Version | License | Used by | Reason")
        key = (row[0], row[3])
        if key in known_deps:
            continue
        known_deps.add(key)
        new_deps.append("| " + " | ".join(row[:5] + [wp]) + " |")

for label, rows in (("BUGS.md", new_bugs), ("DEPENDENCIES.md", new_deps)):
    for r in rows:
        print(f"collect-notes: {label}: {r}")
if check:
    print(f"collect-notes: --check: {len(new_bugs)} bug(s), {len(new_deps)} dependency row(s) would be added")
    sys.exit(0)

def append_after_table(text, header, rows):
    if not rows:
        return text
    before, after = text.split(header, 1)
    lines = after.split("\n")
    # lines[0] is the rest of the header line (empty), lines[1] the separator, then the rows.
    i = 2
    while i < len(lines) and lines[i].startswith("|"):
        i += 1
    lines[i:i] = rows
    return before + header + "\n".join(lines)

bugs_path.write_text(append_after_table(bugs_text, BUGS_HEADER, new_bugs))
deps_path.write_text(append_after_table(deps_text, DEPS_HEADER, new_deps))
print(f"collect-notes: added {len(new_bugs)} bug(s) and {len(new_deps)} dependency row(s)")
PY
