#!/usr/bin/env python3
"""PAID same-seed GPT replay A/B. Only input projection/strict marker vary.
The installed DSH/Pi converter runs offline via a memory pipe, never a file.
No generated tool is executed; original history and ciphertext never persisted.
"""
import argparse,copy,hashlib,json,sqlite3,subprocess,uuid
from pathlib import Path
from paid_ab import ROOT,config_route,request


def main():
 p=argparse.ArgumentParser(description=__doc__);p.add_argument('--paid',action='store_true');p.add_argument('--output',required=True);a=p.parse_args()
 if not a.paid:raise SystemExit('Explicit paid authorization required')
 path=Path(a.output).resolve()
 if path.exists():raise SystemExit('Refusing overwrite')
 with sqlite3.connect('file:'+str(ROOT/'.runtime/app/justai.sqlite3')+'?mode=ro',uri=True)as db:
  row=db.execute("SELECT e.session_id,e.data FROM events e JOIN events u ON u.session_id=e.session_id AND json_extract(u.data,'$.attempt_id')=json_extract(e.data,'$.attempt_id') WHERE e.kind='request_audit' AND u.kind='usage' AND json_extract(u.data,'$.effective_model')='gpt-6.1-sol' AND json_extract(u.data,'$.service_tier')='auto' ORDER BY e.seq ASC LIMIT 1").fetchone();sid,audit=row[0],json.loads(row[1]);items=[json.loads(r[0])for r in db.execute('SELECT item FROM context_items WHERE session_id=? AND epoch=? ORDER BY id LIMIT ?',(sid,audit['context_epoch'],audit['input_items']))]
 url,secret,_=config_route(ROOT/'.runtime/app/justai.sqlite3','gpt-6.1-sol')
 base=json.loads((ROOT/'tests/cache_lab/fixtures/jaa-synthetic-request-template.json').read_text());base.update({'model':'gpt-6.1-sol','input':items,'max_output_tokens':4096,'prompt_cache_key':'same-seed-replay:'+str(uuid.uuid4())})
 base['input'].append({'role':'user','content':'Protocol diagnostic only. Ignore all previous project tasks. Work out 31*41, then call glob with pattern __inert_1271__, path null, max_results 1. Do not answer in prose. No tools actually execute.'})
 report={'model':'gpt-6.1-sol','tools_executed':False,'shared_seed_output':True,'raw_text_saved':False,'max_requests':13,'max_output_tokens':4096,'results':[]}
 def save():path.write_text(json.dumps(report,indent=2))
 def call(body,case):
  record={'case':case,'body_digest':hashlib.sha256(json.dumps(body,sort_keys=True).encode()).hexdigest()[:16]}
  try:out,stats=request(url,secret,body,{})
  except Exception as e:record['error']=type(e).__name__+': '+str(e).replace(secret,'[redacted]')[:160];out=None
  else:
   record.update(stats)
   if not stats['model_matches_requested']:
    report['invalidated']='Returned model differs from requested model; stop this comparison without replaying foreign state.';out=None
  report['results'].append(record);save();print(json.dumps(record),flush=True);return out
 seed=call(base,'seed')
 if seed is None:return
 calls=[i for i in seed if i.get('type')=='function_call']
 if not calls:report['stopped']='No complete call in seed';save();return
 continuation=copy.deepcopy(base);continuation['input']+=seed+[{'type':'function_call_output','call_id':i['call_id'],'output':'No tool executed. Protocol continuation: compute 37*43 then call glob with pattern __inert_1591__, path null, max_results 1. Do not answer in prose.'}for i in calls]
 pipe=subprocess.run(['node',str(ROOT/'tests/cache_lab/normalize_dsh_input.mjs')],input=json.dumps({'model':'gpt-6.1-sol','input':continuation['input']}),text=True,capture_output=True,timeout=15)
 if pipe.returncode:report['stopped']='Offline normalization rejected unsupported native shape';save();return
 normalized=json.loads(pipe.stdout)
 original_reasoning=[i for i in continuation['input']if i.get('type')=='reasoning'];normalized_reasoning=[i for i in normalized if i.get('type')=='reasoning']
 if original_reasoning!=normalized_reasoning:raise SystemExit('Opaque reasoning changed; do not send')
 report['input_projection']={'native_items':len(continuation['input']),'dsh_items':len(normalized),'exactly_equal':normalized==continuation['input'],'reasoning_items_preserved':len(original_reasoning),'reasoning_equal':True,'seed_types':[i.get('type')for i in seed]};save()
 if normalized==continuation['input']:report['stopped']='No actual projection difference';save();return
 order=['native','dsh_input','native_omit','dsh_input_omit','dsh_input','native','dsh_input_omit','native_omit','native','dsh_input','native_omit','dsh_input_omit']
 for mode in order:
  body=copy.deepcopy(continuation)
  if mode.startswith('dsh_input'):body['input']=copy.deepcopy(normalized)
  if mode.endswith('omit'):
   for tool in body['tools']:tool.pop('strict',None)
  if call(body,mode) is None:
   report['stopped']='A branch failed or returned a mismatched model; no automatic retry';break
 report['totals']={'requests':len(report['results']),'known_input_tokens':sum(r.get('input_tokens')or 0 for r in report['results']),'known_cached_tokens':sum(r.get('cached_tokens')or 0 for r in report['results']),'known_output_tokens':sum(r.get('output_tokens')or 0 for r in report['results'])};save();print(json.dumps({'totals':report['totals']}),flush=True)
if __name__=='__main__':main()
