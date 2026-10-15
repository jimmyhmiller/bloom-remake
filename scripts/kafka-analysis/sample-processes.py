#!/usr/bin/env python3
"""Read-only /proc sampling. Counters are cumulative; preserve raw timestamps.
Thread states/wchan are samples, not measured blocked durations. schedstat wait
is meaningful only when kernel sched_schedstats is enabled. No kernel knobs change.
"""
import argparse, json, os, pathlib, signal, time
p=argparse.ArgumentParser()
p.add_argument('--bench-dir',required=True)
p.add_argument('--out',required=True)
p.add_argument('--interval',type=float,default=1)
a=p.parse_args(); root=pathlib.Path(a.bench_dir); out=pathlib.Path(a.out)
out.mkdir(parents=True,exist_ok=True)
running=True

def stop(*_):
 global running
 running=False
signal.signal(signal.SIGTERM,stop); signal.signal(signal.SIGINT,stop)
def read(p):
 return pathlib.Path(p).read_text().strip()
def stat(path):
 s=read(path); end=s.rindex(')'); f=s[end+2:].split()
 return dict(comm=s[s.index('(')+1:end],state=f[0],minor_faults=int(f[7]),major_faults=int(f[9]),user_ticks=int(f[11]),system_ticks=int(f[12]),threads=int(f[17]),start_ticks=int(f[19]),rss_pages=int(f[21]),processor=int(f[36]),blkio_ticks=int(f[39]))
def keyed(path):
 return {k.strip():v.strip() for k,v in (l.split(':',1) for l in read(path).splitlines() if ':' in l)}
meta=dict(clock_ticks=os.sysconf('SC_CLK_TCK'),page_size=os.sysconf('SC_PAGE_SIZE'),interval_s=a.interval,boot_id=read('/proc/sys/kernel/random/boot_id'),schedstats=read('/proc/sys/kernel/sched_schedstats'))
(out/'proc-meta.json').write_text(json.dumps(meta,indent=2)+'\n')
with (out/'proc.jsonl').open('w',buffering=1) as stream:
 while running:
  start=time.monotonic(); sample=dict(epoch=time.time(),monotonic=start,processes=[])
  for i in (1,2,3):
   try:
    pid=int(read(root/f'pid.{i}')); pr=pathlib.Path('/proc')/str(pid)
    proc=dict(broker=i,pid=pid,stat=stat(pr/'stat'),io={k:int(v) for k,v in keyed(pr/'io').items()},tasks=[])
    try:
     proc['bloom_stats']={k:int(v) for k,v in (l.rsplit(None,1) for l in read(root/'blossom'/f'b{i}.stats').splitlines() if l.strip())}
    except (OSError,ValueError): pass
    for task in (pr/'task').iterdir():
     try:
      st=keyed(task/'status')
      proc['tasks'].append(dict(tid=int(task.name),stat=stat(task/'stat'),schedstat=list(map(int,read(task/'schedstat').split())),wchan=read(task/'wchan'),voluntary=int(st['voluntary_ctxt_switches']),involuntary=int(st['nonvoluntary_ctxt_switches'])))
     except (OSError,ValueError,KeyError): pass # A task can exit between reads.
    sample['processes'].append(proc)
   except (OSError,ValueError,KeyError): pass # Startup and intentional crash windows.
  for kind in ('cpu','io','memory'):
   try: sample['pressure_'+kind]=read('/proc/pressure/'+kind)
   except OSError: pass
  sample['sample_duration_s']=time.monotonic()-start
  stream.write(json.dumps(sample,separators=(',',':'))+'\n')
  time.sleep(max(0,a.interval-(time.monotonic()-start)))
