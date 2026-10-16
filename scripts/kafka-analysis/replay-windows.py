#!/usr/bin/env python3
"""Select trace windows by recorded wall time, then verify every replayed outcome.
Report timings include replay verification and function-work profiling, not live
storage/network operation. Measured/unmeasured mode quantifies counter overhead.
"""
import csv,json,os,pathlib,subprocess
root=pathlib.Path.home()/'soak-results/gap-trace'
e=[l.split(' ',2) for l in (root/'events.log').read_text().splitlines()]
maxstart=next(float(t) for _,t,n in e if n.startswith('max:'))
fixed=next(float(t) for _,t,n in e if n.startswith('fixed:'))
rs=list(csv.DictReader((root/'timeline.csv').open()));windows=[]
for name,t in [('max-early',maxstart+20),('max-late',maxstart+40),('fixed',fixed+5),('recovery',fixed+35)]:
 selected=[r for r in rs if t<=int(r['now_ns'])/1e9<t+5]
 assert selected,name
 windows.append(dict(name=name,first=int(selected[0]['tick']),last=int(selected[-1]['tick']),requested_epoch=t,requested_duration_s=5,ticks=len(selected),events=sum(int(r['events']) for r in selected),delivered=sum(int(r['delivered']) for r in selected)))
(root/'windows.json').write_text(json.dumps(windows,indent=2)+'\n')
out=root/'replays';out.mkdir(exist_ok=True)
for w in windows:
 for measured in ([True,False] if w['name'].startswith('max') else [True]):
  mode='measured' if measured else 'plain';prefix=out/(w['name']+'-'+mode)
  cmd=['taskset','-c','0-1,16-17',os.environ.get('REPLAY_BINARY', str(pathlib.Path.home()/'soak-results/blossom-gap-timed')),'trace','plan',str(root/'traces/b2-1.blstrace'),'--program','examples/kafka/broker.bls','--node','b1:Broker','--node','b2:Broker','--node','b3:Broker','--first',str(w['first']),'--last',str(w['last']),'--map-cache-rows','8192','--map-cache-joins','--lazy-indexes','--workers','1','--compiled-joins','--packed-indexes','--fuse-relations']
  if not measured:cmd+=['--no-measurement']
  with prefix.with_suffix('.json').open('w') as stdout,prefix.with_suffix('.err').open('w') as stderr:
   subprocess.run(cmd,stdout=stdout,stderr=stderr,check=True,timeout=600)
  print(w['name'],mode,'verified',flush=True)
