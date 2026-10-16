#!/usr/bin/env python3
"""Checked replay comparison of existing physical representations on identical input.
Run separately from live controls. Each run verifies the complete prefix and measured window.
"""
import argparse
import itertools
import json
import pathlib
import subprocess

p = argparse.ArgumentParser()
p.add_argument('--binary', required=True)
p.add_argument('--program', required=True)
p.add_argument('--trace', required=True)
p.add_argument('--out', type=pathlib.Path, required=True)
p.add_argument('--first', type=int, required=True)
p.add_argument('--last', type=int, required=True)
a = p.parse_args()
a.out.mkdir(exist_ok=False, parents=True)
for inferred, batches in itertools.product([False, True], repeat=2):
    name = f'keys-{int(inferred)}-batches-{int(batches)}'
    cmd = ['taskset', '-c', '0-1,16-17', a.binary, 'trace', 'plan', a.trace,
           '--program', a.program, '--node', 'b1:Broker', '--node', 'b2:Broker',
           '--node', 'b3:Broker', '--first', str(a.first), '--last', str(a.last),
           '--map-cache-rows', '8192', '--map-cache-joins', '--lazy-indexes',
           '--workers', '1', '--compiled-joins', '--packed-indexes', '--fuse-relations']
    if inferred:
        cmd.append('--infer-keys')
    if batches:
        cmd.append('--pipeline-batches')
    (a.out / (name + '.command.json')).write_text(json.dumps(cmd, indent=2) + '\n')
    with (a.out / (name + '.json')).open('x') as output, (a.out / (name + '.err')).open('x') as errors:
        subprocess.run(cmd, stdout=output, stderr=errors, check=True, timeout=900)
    result = json.loads((a.out / (name + '.json')).read_text())
    rules = result['plan']['rules']
    stores = result['plan']['stores']
    operations = {key: sum((store['operations'] or {}).get(key, 0) for store in stores)
                  for key in ['batch_weight_updates', 'membership_changes',
                              'secondary_index_row_updates', 'secondary_index_build_rows',
                              'bulk_retracted_rows', 'bulk_index_clears', 'probe_calls']}
    print(json.dumps({'case': name, 'timing_ns': result['timing_ns'],
                      'support_writes': sum(r['work']['support_writes'] for r in rules),
                      'compiled_terms': sum(r['work']['compiled_terms'] for r in rules),
                      'operations': operations}), flush=True)
