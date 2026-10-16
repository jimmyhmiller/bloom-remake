#!/usr/bin/env python3
"""Period-weighted user-cycle samples. Inclusive groups overlap; never sum them.
Input: perf script --no-inline -i cpu.perf (default fields). Full leaf symbols
remain in output, allowing the category heuristics to be audited.
"""
import argparse,collections,json,re
p=argparse.ArgumentParser();p.add_argument('input');a=p.parse_args()
header=re.compile(r'^(.*?)\s+(\d+)(?:/(\d+))?\s+([0-9.]+):\s+(\d+)\s+(.+):\s*$')
frame=re.compile(r'^\s+[0-9a-f]+\s+(.*?)\s+\((.*)\)\s*$')
samples=[];cur=None
for line in open(a.input):
 m=header.match(line)
 if m:
  cur={'thread':m[1].strip(),'period':int(m[5]),'frames':[]};samples.append(cur)
 elif cur and (m:=frame.match(line)):
  cur['frames'].append(re.sub(r'\+0x[0-9a-f]+$','',m[1]))
assert samples,'no samples parsed'
total=sum(s['period'] for s in samples);leaf=collections.Counter();threads=collections.Counter();inclusive=collections.Counter();mechanism=collections.Counter();roots=collections.Counter();ownership=collections.Counter()
groups={
 'expression evaluator':['blossom_engine::expr::'],
 'LSM storage':['blossom_store::lsm::'],
 'engine store':['blossom_engine::store::'],
 'row support':['blossom_engine::row_support::','blossom_engine::row_bucket::'],
 'compiled kernels':['blossom_engine::compiled::'],
 'retained maps':['blossom_engine::map_cache::'],
 'delta drivers':['run_drivers','run_terms'],
 'join evaluation':['blossom_engine::rule::'],
 'value/byte ordering':['blossom_value::value::','blossom_value::bytes::'],
 'serialization':['blossom_wire::','postcard::','blossom_ir::codec::','blossom_value::codec::'],
 'BLAKE3':['blake3'],
 'hash tables':['hashbrown::'],
 'B-trees':['alloc::collections::btree::'],
 'allocation/free':['malloc','mi_free','_int_free','alloc::alloc::','__rust_alloc','__rust_dealloc'],
 'copies':['memcpy','memmove'],
}
for s in samples:
 w=s['period'];fs=s['frames'];name=fs[0] if fs else '[no stack]';leaf[name]+=w;threads[s['thread']]+=w
 for label,patterns in groups.items():
  if any(p in f for p in patterns for f in fs):inclusive[label]+=w
 # Exclusive leaf classification only. No attribution of children to several groups.
 if 'memcmp' in name or 'bcmp' in name: kind='byte comparison'
 elif 'blossom_value' in name and any(p in name for p in ['Clone','clone','drop_in_place']):kind='value cloning/destruction'
 elif 'crc32' in name:kind='checksums'
 elif any(p in name for p in ['malloc','free','__rust_alloc','__rust_dealloc','_mi_','mi_theap']): kind='allocation/free'
 elif any(p in name for p in ['memcpy','memmove']):kind='memory copying'
 elif any(p in name for p in ['blake3','xxh3','SipHasher']):kind='hash/digest'
 elif 'blossom_value' in name and any(p in name for p in ['cmp','compare','eq','hash']):kind='value equality/order/hash'
 elif any(p in name for p in ['btree','hashbrown']):kind='collection internals'
 elif 'blossom_engine::expr' in name:kind='expression evaluator'
 elif 'blossom_store' in name:kind='storage'
 elif 'blossom_engine' in name:kind='other engine'
 elif any(p in name for p in ['blossom_wire','postcard','codec']):kind='serialization'
 else:kind='other/unknown'
 mechanism[kind]+=w
 joined=' '.join(fs)
 for category,patterns in [('LSM access/maintenance',['blossom_store::lsm::']),('expression evaluation',['blossom_engine::expr::']),('row support',['blossom_engine::row_support::','blossom_engine::row_bucket::']),('other engine store operations',['blossom_engine::store::']),('other engine evaluation/maintenance',['blossom_engine::']),('serialization',['blossom_wire::','postcard::','blossom_ir::codec::','blossom_value::codec::'])]:
  if any(p in joined for p in patterns):ownership[category]+=w;break
 else:ownership['other/unknown']+=w
 roots[fs[-1] if fs else '[no stack]']+=w
out={'samples':len(samples),'total_period':total,'denominator':'all sampled broker user cycles; period weighted','thread_percent':{k:100*v/total for k,v in threads.most_common()},'exclusive_leaf_percent':{k:100*v/total for k,v in mechanism.most_common()},'inclusive_stack_percent_OVERLAPPING':{k:100*v/total for k,v in inclusive.most_common()},'exclusive_stack_partition_percent_priority_order':{k:100*v/total for k,v in ownership.most_common()},'top_leaf_symbols':[{'symbol':k,'percent':100*v/total} for k,v in leaf.most_common(100)],'stack_root_percent':{k:100*v/total for k,v in roots.most_common(20)}}
print(json.dumps(out,indent=2))
