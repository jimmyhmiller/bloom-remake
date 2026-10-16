#!/usr/bin/env python3
"""Parse bpftrace's scalar aggregate lines. Histograms remain in raw reports."""
import argparse,collections,json,pathlib,re
p=argparse.ArgumentParser();p.add_argument('files',nargs='+');a=p.parse_args();out=[]
for file in a.files:
 groups=collections.defaultdict(lambda:dict(sync_count=0,sync_ns=0,off_ns=collections.Counter(),runqueue_ns=0))
 for line in pathlib.Path(file).read_text().splitlines():
  m=re.fullmatch(r'@(sync_count|sync_ns|off_ns|runqueue_ns)\[(.*)\]: (\d+)',line)
  if not m:continue
  kind,key,n=m[1],m[2].split(', '),int(m[3]);g=groups[(int(key[0]),key[1])]
  if kind=='off_ns':g[kind][key[2]]+=n
  else:g[kind]+=n
 rows=[]
 for (pid,thread),g in sorted(groups.items()):
  rows.append(dict(pid=pid,thread=thread,sync_calls=g['sync_count'],sync_seconds=g['sync_ns']/1e9,mean_sync_ms=g['sync_ns']/1e6/g['sync_count'] if g['sync_count'] else None,off_seconds_by_state={k:v/1e9 for k,v in g['off_ns'].items()},runqueue_ms=g['runqueue_ns']/1e6))
 out.append(dict(file=file,window_seconds=30,threads=rows,total_sync_calls=sum(r['sync_calls'] for r in rows),total_sync_seconds=sum(r['sync_seconds'] for r in rows)))
print(json.dumps(out,indent=2))
