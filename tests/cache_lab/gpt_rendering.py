#!/usr/bin/env python3
"""PAID fixed-history GPT rendering A/B/BA. No tools dispatched or raw history saved."""
import argparse,copy,hashlib,json,sqlite3,uuid
from pathlib import Path
from paid_ab import ROOT,DSH_UA,config_route,request


def main():
 p=argparse.ArgumentParser(description=__doc__);p.add_argument('--paid',action='store_true');p.add_argument('--output',required=True);a=p.parse_args()
 if not a.paid:raise SystemExit('Explicit paid opt-in required')
 target=Path(a.output).resolve()
 if target.exists():raise SystemExit('Refusing overwrite')
 with sqlite3.connect('file:'+str(ROOT/'.runtime/app/justai.sqlite3')+'?mode=ro',uri=True)as db:
  row=db.execute("SELECT e.session_id,e.data FROM events e JOIN events u ON u.session_id=e.session_id AND json_extract(u.data,'$.attempt_id')=json_extract(e.data,'$.attempt_id') WHERE e.kind='request_audit' AND u.kind='usage' AND json_extract(u.data,'$.effective_model')='gpt-6.1-sol' AND json_extract(u.data,'$.service_tier')='auto' ORDER BY e.seq ASC LIMIT 1").fetchone();sid,audit=row[0],json.loads(row[1]);items=[json.loads(r[0])for r in db.execute('SELECT item FROM context_items WHERE session_id=? AND epoch=? ORDER BY id LIMIT ?',(sid,audit['context_epoch'],audit['input_items']))]
 if{i.get('call_id')for i in items if i.get('type')=='function_call'}!={i.get('call_id')for i in items if i.get('type')=='function_call_output'}:raise SystemExit('Not a closed tool boundary')
 url,secret,_=config_route(ROOT/'.runtime/app/justai.sqlite3','gpt-6.1-sol')
 base=json.loads((ROOT/'tests/cache_lab/fixtures/jaa-synthetic-request-template.json').read_text());base.update({'model':'gpt-6.1-sol','input':items,'max_output_tokens':81920,'prompt_cache_key':'gpt-rendering:'+str(uuid.uuid4())})
 base['input'].append({'role':'user','content':'Protocol diagnostic only. Ignore all old tasks. Call glob once with pattern __inert_cache_check__, path null, max_results 1. No tool will execute.'})
 order=['native','metadata_removed','all_turns','native','dsh_headers','metadata_removed','all_turns','native','tier_default','no_strict','tier_default','native','no_strict','dsh_headers']
 report={'model':'gpt-6.1-sol','order':order,'fixed_initial_context':True,'raw_text_saved':False,'tools_executed':False,'results':[]}
 def save():target.write_text(json.dumps(report,indent=2))
 save()
 for index,case in enumerate(order):
  body=copy.deepcopy(base);headers={}
  if case=='metadata_removed':
   for item in body['input']:
    item.pop('metadata',None);item.pop('internal_chat_message_metadata_passthrough',None)
  if case=='all_turns':body['reasoning']['context']='all_turns'
  if case=='tier_default':body['service_tier']='default'
  if case=='no_strict':
   for tool in body['tools']:tool.pop('strict',None)
  if case=='dsh_headers':headers={'session_id':base['prompt_cache_key'],'User-Agent':DSH_UA,'X-Client-Request-Id':str(uuid.uuid4()),'X-Stainless-Arch':'x64','X-Stainless-Lang':'js','X-Stainless-OS':'Linux','X-Stainless-Package-Version':'7.19.0','X-Stainless-Retry-Count':'0','X-Stainless-Runtime':'node','X-Stainless-Runtime-Version':'v26.11.1'}
  record={'index':index,'case':case,'body_digest':hashlib.sha256(json.dumps(body,sort_keys=True).encode()).hexdigest()[:16]}
  try:_,stats=request(url,secret,body,headers)
  except Exception as error:record['error']=type(error).__name__+': '+str(error).replace(secret,'[redacted]')[:160]
  else:record.update(stats)
  report['results'].append(record);save();print(json.dumps(record),flush=True)
 report['totals']={'requests':len(order),'known_input_tokens':sum(r.get('input_tokens')or 0 for r in report['results']),'known_cached_tokens':sum(r.get('cached_tokens')or 0 for r in report['results']),'known_output_tokens':sum(r.get('output_tokens')or 0 for r in report['results'])};save();print(json.dumps({'totals':report['totals']}),flush=True)
if __name__=='__main__':main()
