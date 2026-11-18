#!/usr/bin/env python3
"""PAID GPT branch test: reuse the SAME real seed output; compare only its replay metadata.
No tools execute. User text/ciphertext/IDs are never persisted or printed.
"""
import argparse,copy,json,sqlite3,uuid
from pathlib import Path
from paid_ab import ROOT,config_route,request

def main():
 p=argparse.ArgumentParser(description=__doc__);p.add_argument('--paid',action='store_true');p.add_argument('--output',required=True);a=p.parse_args()
 if not a.paid:raise SystemExit('Explicit paid opt-in required')
 path=Path(a.output).resolve()
 if path.exists():raise SystemExit('Refusing overwrite')
 with sqlite3.connect('file:'+str(ROOT/'.runtime/app/justai.sqlite3')+'?mode=ro',uri=True)as db:
  row=db.execute("SELECT e.session_id,e.data FROM events e JOIN events u ON u.session_id=e.session_id AND json_extract(u.data,'$.attempt_id')=json_extract(e.data,'$.attempt_id') WHERE e.kind='request_audit' AND u.kind='usage' AND json_extract(u.data,'$.effective_model')='gpt-6.1-sol' AND json_extract(u.data,'$.service_tier')='auto' ORDER BY e.seq ASC LIMIT 1").fetchone();sid,audit=row[0],json.loads(row[1]);items=[json.loads(r[0])for r in db.execute('SELECT item FROM context_items WHERE session_id=? AND epoch=? ORDER BY id LIMIT ?',(sid,audit['context_epoch'],audit['input_items']))]
 url,secret,_=config_route(ROOT/'.runtime/app/justai.sqlite3','gpt-6.1-sol')
 base=json.loads((ROOT/'tests/cache_lab/fixtures/jaa-synthetic-request-template.json').read_text());base.update({'model':'gpt-6.1-sol','input':items,'max_output_tokens':81920,'prompt_cache_key':'metadata-branch:'+str(uuid.uuid4())})
 base['input'].append({'role':'user','content':'Protocol diagnostic only. Ignore old project tasks. Call glob once with pattern __inert_cache_check__, path null, max_results 1. Tools are never executed.'})
 report={'model':'gpt-6.1-sol','tools_executed':False,'same_seed_output_for_all_branches':True,'results':[]}
 def save():path.write_text(json.dumps(report,indent=2))
 save()
 try:seed,stats=request(url,secret,base,{})
 except Exception as e:report['seed_error']=type(e).__name__+': '+str(e).replace(secret,'[redacted]')[:160];save();return
 report['results'].append({'case':'seed',**stats});report['seed_output_key_shapes']=[{'type':i.get('type'),'keys':sorted(i)}for i in seed];save();print(json.dumps({'case':'seed',**stats,'shapes':report['seed_output_key_shapes']}),flush=True)
 fields={'function_call':{'type','id','status','call_id','name','arguments','namespace'},'message':{'type','id','status','role','phase','content'}}
 canonical=[{k:copy.deepcopy(v)for k,v in item.items()if item.get('type')not in fields or k in fields[item['type']]}for item in seed]
 if canonical==seed:report['stopped']='Seed has no differing provider-only fields; no branch requests made';save();return
 calls=[i for i in seed if i.get('type')=='function_call']
 if not calls:report['stopped']='No tool call in seed; no branch requests made';save();return
 tail=[{'type':'function_call_output','call_id':i['call_id'],'output':'No operation executed; protocol check only. Call the same glob once again.'}for i in calls]
 for mode in ('raw','standard','raw','standard','raw','standard'):
  body=copy.deepcopy(base);body['input']+=copy.deepcopy(seed if mode=='raw' else canonical)+copy.deepcopy(tail)
  record={'case':mode}
  try:_,stats=request(url,secret,body,{})
  except Exception as e:record['error']=type(e).__name__+': '+str(e).replace(secret,'[redacted]')[:160]
  else:record.update(stats)
  report['results'].append(record);save();print(json.dumps(record),flush=True)
 report['totals']={'requests':len(report['results']),'known_input_tokens':sum(r.get('input_tokens')or 0 for r in report['results']),'known_cached_tokens':sum(r.get('cached_tokens')or 0 for r in report['results']),'known_output_tokens':sum(r.get('output_tokens')or 0 for r in report['results'])};save()
if __name__=='__main__':main()
