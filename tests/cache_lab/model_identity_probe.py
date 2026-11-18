#!/usr/bin/env python3
"""PAID finite endpoint-model identity experiment; no output replay or tool execution.
Same requested model/input, only strict marker and max_output_tokens vary.
"""
import argparse,copy,hashlib,json,sqlite3,subprocess,uuid
from pathlib import Path
from paid_ab import ROOT,DSH_UA,config_route,request

def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--paid',action='store_true');p.add_argument('--output',required=True);p.add_argument('--harness-comparison',action='store_true')
    args=p.parse_args()
    if not args.paid:raise SystemExit('Explicit --paid authorization required')
    output=Path(args.output).resolve()
    if output.exists():raise SystemExit('Refusing to overwrite report')
    with sqlite3.connect('file:'+str(ROOT/'.runtime/app/justai.sqlite3')+'?mode=ro',uri=True)as db:
        row=db.execute("SELECT e.session_id,e.data FROM events e JOIN events u ON u.session_id=e.session_id AND json_extract(u.data,'$.attempt_id')=json_extract(e.data,'$.attempt_id') WHERE e.kind='request_audit' AND u.kind='usage' AND json_extract(u.data,'$.effective_model')='gpt-6.1-sol' AND json_extract(u.data,'$.service_tier')='auto' ORDER BY e.seq ASC LIMIT 1").fetchone()
        if not row:raise SystemExit('No archived matching boundary')
        sid,audit=row[0],json.loads(row[1])
        items=[json.loads(r[0])for r in db.execute('SELECT item FROM context_items WHERE session_id=? AND epoch=? ORDER BY id LIMIT ?',(sid,audit['context_epoch'],audit['input_items']))]
    url,secret,_=config_route(ROOT/'.runtime/app/justai.sqlite3','gpt-6.1-sol')
    base=json.loads((ROOT/'tests/cache_lab/fixtures/jaa-synthetic-request-template.json').read_text())
    base.update({'model':'gpt-6.1-sol','input':items,'prompt_cache_key':'identity-probe:'+str(uuid.uuid4())})
    base['input'].append({'role':'user','content':'Protocol diagnostic only. Ignore all previous project tasks. Work out 31*41, then call glob with pattern __inert_1271__, path null, max_results 1. Do not answer in prose. No tools actually execute.'})
    plan=[(4096,True),(81920,True),(4096,False),(81920,False),(81920,True),(4096,True),(81920,False),(4096,False)]
    dsh_input=None
    if args.harness_comparison:
        plan=[(81920,True),(128000,False),(81920,True),(128000,False)]
        pipe=subprocess.run(['node',str(ROOT/'tests/cache_lab/normalize_dsh_input.mjs')],input=json.dumps({'model':'gpt-6.1-sol','instructions':base['instructions'],'input':base['input']}),text=True,capture_output=True,timeout=15)
        if pipe.returncode:raise SystemExit('Offline DSH normalization rejected shape')
        dsh_input=json.loads(pipe.stdout)
        if [i for i in dsh_input if i.get('type')=='reasoning']!=[i for i in base['input']if i.get('type')=='reasoning']:raise SystemExit('Opaque reasoning changed; stop')
    report={'requested_model':'gpt-6.1-sol','planned_requests':len(plan),'fixed_input':True,'no_output_replay':True,'tools_executed':False,'raw_history_saved':False,'results':[]}
    def save():output.write_text(json.dumps(report,indent=2))
    save()
    for index,(budget,strict)in enumerate(plan):
        body=copy.deepcopy(base);body['max_output_tokens']=budget;headers={}
        if args.harness_comparison and not strict:
            body.pop('instructions',None);body.pop('parallel_tool_calls',None);body['input']=copy.deepcopy(dsh_input);body['reasoning']['summary']='auto';body['include']=['reasoning.encrypted_content']
            headers={'User-Agent':DSH_UA,'session_id':base['prompt_cache_key'],'X-Client-Request-Id':str(uuid.uuid4()),'X-Stainless-Arch':'x64','X-Stainless-Lang':'js','X-Stainless-OS':'Linux','X-Stainless-Package-Version':'7.19.0','X-Stainless-Retry-Count':'0','X-Stainless-Runtime':'node','X-Stainless-Runtime-Version':'v26.11.1'}
        if not strict:
            for tool in body['tools']:tool.pop('strict',None)
        record={'index':index,'harness_shape':bool(args.harness_comparison and not strict),'max_output_tokens':budget,'strict_present':strict,'body_digest':hashlib.sha256(json.dumps(body,sort_keys=True).encode()).hexdigest()[:16]}
        try:_,stats=request(url,secret,body,headers)
        except Exception as error:record['error']=type(error).__name__+': '+str(error).replace(secret,'[redacted]')[:160]
        else:record.update(stats)
        # A mismatched result is the observation. It is NEVER used as input.
        report['results'].append(record);save();print(json.dumps(record),flush=True)
    report['totals']={'completed':sum('input_tokens'in r for r in report['results']),'mismatched_models':sum(r.get('model_matches_requested')is False for r in report['results']),'input_tokens':sum(r.get('input_tokens')or 0 for r in report['results']),'cached_tokens':sum(r.get('cached_tokens')or 0 for r in report['results']),'output_tokens':sum(r.get('output_tokens')or 0 for r in report['results'])}
    save();print(json.dumps({'totals':report['totals']}),flush=True)
if __name__=='__main__':main()
