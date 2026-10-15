#!/usr/bin/env python3
import collections,json,pathlib
root=pathlib.Path.home()/'soak-results/gap-trace/replays';result=[]
for path in sorted(root.glob('*.json')):
 r=json.load(path.open());rules=r['plan']['rules'];stores=r['plan']['stores']
 ops={k:sum((s['operations'] or {}).get(k,0) for s in stores) for k in next((s['operations'] for s in stores if s['operations']),{})}
 by=collections.defaultdict(lambda:dict(rules=0,elapsed_ns=0,support_writes=0,compiled_terms=0))
 for rule in rules:
  b=by[rule['operator']];b['rules']+=1;b['elapsed_ns']+=rule['elapsed_ns'] or 0;b['support_writes']+=rule['work']['support_writes'];b['compiled_terms']+=rule['work']['compiled_terms']
 batch=r['boundary_rows'].get('inserted',{}).get('batch',0)
 work={k:sum(x['work'][k] for x in rules) for k in rules[0]['work']}
 result.append(dict(window=path.stem,ticks=r['ticks'],recorded_time_bounds_ns=r['recorded_time_bounds_ns'],replay_ns=r['replay_elapsed_ns'],timing_ns=r['timing_ns'],rule_maintenance_ns=sum(x['elapsed_ns'] or 0 for x in rules),batches_inserted=batch,work=work,operations=ops,work_per_batch={k:v/batch for k,v in work.items()} if batch else {},operations_per_batch={k:v/batch for k,v in ops.items()} if batch else {},operators=by,boundary_rows=r['boundary_rows'],top_rules=sorted(rules,key=lambda x:-(x['elapsed_ns'] or 0))[:25],top_functions=sorted(r['functions'],key=lambda x:-x['self_steps'])[:20],plan_rules=len(rules),physical_stores=len(stores),declared_durable_main_operations={k:sum((s['operations'] or {}).get(k,0) for s in stores if 'declared durable state' in s['reasons']) for k in ops},scoped_operations={k:sum((s['operations'] or {}).get(k,0) for s in stores if 'tick-scoped: cleared and rebuilt each tick' in s['reasons']) for k in ops}))
print(json.dumps(result,indent=2))
