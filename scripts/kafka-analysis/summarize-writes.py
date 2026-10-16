#!/usr/bin/env python3
"""Summarize VFS returned bytes and engine blocked stacks; not device bytes."""
import argparse
import collections
import json
import pathlib
import re

p = argparse.ArgumentParser()
p.add_argument('report', type=pathlib.Path)
a = p.parse_args()
s = a.report.read_text()
files = collections.Counter()
categories = collections.Counter()
for pid, comm, name, count in re.findall(r'@written\[(\d+), ([^,]+), ([^\]]+)\]: (\d+)', s):
    files[name] += int(count)
    category = ('WAL' if name.endswith('.seg') else
                'blob packs' if name.endswith('.pack') else
                'LSM tables' if re.fullmatch(r'\d+\.tmp', name) else
                'other')
    categories[category] += int(count)
blocked = collections.Counter()
by_pid = collections.defaultdict(collections.Counter)
for pid, stack, count in re.findall(r'@blocked_ns\[(\d+), (.*?)\]: (\d+)', s, re.S):
    category = ('journal commit' if 'jbd2_log_wait_commit' in stack else
                'file writeback' if 'folio_wait_writeback' in stack else
                'other')
    blocked[category] += int(count)
    by_pid[pid][category] += int(count)
assert files, 'No returned-byte measurements found'
total = sum(files.values())
print(json.dumps(dict(
    window_seconds=30, logical_bytes=total,
    bytes_by_category=dict(categories),
    percent_by_category={k: 100*v/total for k,v in categories.items()},
    bytes_by_basename=dict(files.most_common()),
    engine_blocked_ns=dict(blocked),
    engine_blocked_ns_by_pid=dict(by_pid),
), indent=2))
