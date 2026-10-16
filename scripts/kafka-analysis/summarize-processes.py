#!/usr/bin/env python3
"""Summarize raw counters between samples nearest max-phase event boundaries.
Expose actual sampled bounds. No extrapolation across restarted processes.
"""
import argparse,collections,csv,json,pathlib,re
p=argparse.ArgumentParser();p.add_argument('directories',nargs='+');a=p.parse_args()
reports=[]
for directory in a.directories:
 root=pathlib.Path(directory); meta=json.loads((root/'proc-meta.json').read_text())
 events=[l.split(' ',2) for l in (root/'events.log').read_text().splitlines()]
 begin=next(float(t) for _,t,e in events if e.startswith('max:'))
 end=next(float(t) for _,t,e in events if e=='max done')
 samples=[json.loads(l) for l in (root/'proc.jsonl').read_text().splitlines()]
 lo=min(samples,key=lambda s:abs(s['epoch']-begin)); hi=min(samples,key=lambda s:abs(s['epoch']-end))
 seconds=hi['epoch']-lo['epoch'];hz=meta['clock_ticks']
 summary=(root/'summary.txt').read_text()
 throughput=int(next(next(csv.reader([l])) for l in summary.splitlines() if l.startswith('"max"'))[2])
 records=sum(int(re.search(r'acked=(\d+)',l).group(1)) for l in summary.splitlines() if l.startswith('producer-max-'))
 out=dict(trial=root.name,max_phase_s=end-begin,sampled_s=seconds,boundary_error_s=[lo['epoch']-begin,hi['epoch']-end],records=records,throughput=throughput,sampler_max_ms=max(s['sample_duration_s']*1000 for s in samples),brokers=[])
 for before in lo['processes']:
  after=next(q for q in hi['processes'] if q['broker']==before['broker'])
  assert before['pid']==after['pid'] and before['stat']['start_ticks']==after['stat']['start_ticks']
  def delta(key):return after['stat'][key]-before['stat'][key]
  cpu=(delta('user_ticks')+delta('system_ticks'))/hz
  b=dict(broker=before['broker'],cpu_s=cpu,system_cpu_s=delta('system_ticks')/hz,cpu_cores=cpu/seconds,io={k:after['io'][k]-v for k,v in before['io'].items()},minor_faults=delta('minor_faults'),major_faults=delta('major_faults'),threads=[])
  start={t['tid']:t for t in before['tasks']}
  for t in after['tasks']:
   old=start.get(t['tid'])
   if old is None or old['stat']['start_ticks']!=t['stat']['start_ticks']:continue
   ns=t['schedstat'][0]-old['schedstat'][0]
   b['threads'].append(dict(tid=t['tid'],name=t['stat']['comm'],cpu_s=ns/1e9,cpu_cores=ns/1e9/seconds,voluntary=t['voluntary']-old['voluntary'],involuntary=t['involuntary']-old['involuntary']))
  b['threads'].sort(key=lambda t:-t['cpu_s'])
  if 'bloom_stats' in before and 'bloom_stats' in after:
   b['bloom_delta']={k:after['bloom_stats'][k]-v for k,v in before['bloom_stats'].items() if k in after['bloom_stats'] and not k.startswith(('resident.','slow.')) and 'max' not in k and k!='resident_rows'}
   b['bloom_end']={k:v for k,v in after['bloom_stats'].items() if 'max' in k}
  out['brokers'].append(b)
 out['cluster_cpu_s']=sum(b['cpu_s'] for b in out['brokers'])
 out['cluster_cpu_us_per_record']=out['cluster_cpu_s']*1e6/records
 out['cluster_write_bytes_per_record']=sum(b['io']['write_bytes'] for b in out['brokers'])/records
 reports.append(out)
print(json.dumps(reports,indent=2))
