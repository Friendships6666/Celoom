#!/usr/bin/env python3
"""PAID fixed-body A/B/A test: ONLY tool strict fields differ, no tool execution."""
import argparse,copy,json,sqlite3,uuid
from pathlib import Path
from paid_ab import ROOT,config_route,request

def main():
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--paid',action='store_true');p.add_argument('--output',required=True);p.add_argument('--model',default='deepseek-v4.1-flash-fast');p.add_argument('--input-items',type=int,default=0);p.add_argument('--source',choices=['child','gpt_auto'],default='child');a=p.parse_args()
    if not a.paid:raise SystemExit('Explicit paid opt-in required')
    target=Path(a.output).resolve()
    if target.exists():raise SystemExit('Refusing overwrite')
    with sqlite3.connect('file:'+str(ROOT/'.runtime/app/justai.sqlite3')+'?mode=ro',uri=True)as db:
        row=db.execute("SELECT e.session_id,e.data FROM events e JOIN sessions s ON s.id=e.session_id WHERE s.parent_id IS NOT NULL AND e.kind='request_audit' AND json_extract(e.data,'$.input_items')>50 AND EXISTS(SELECT 1 FROM events u WHERE u.session_id=e.session_id AND u.kind='usage' AND json_extract(u.data,'$.cached_tokens') BETWEEN 1 AND 1000 AND json_extract(u.data,'$.input_tokens')>50000) ORDER BY e.seq DESC LIMIT 1").fetchone()
        if a.source=='gpt_auto':
            if a.model!='gpt-6.1-sol':raise SystemExit('Only replay opaque state to its original model')
            row=db.execute("SELECT e.session_id,e.data FROM events e JOIN events u ON u.session_id=e.session_id AND json_extract(u.data,'$.attempt_id')=json_extract(e.data,'$.attempt_id') WHERE e.kind='request_audit' AND u.kind='usage' AND json_extract(u.data,'$.effective_model')='gpt-6.1-sol' AND json_extract(u.data,'$.service_tier')='auto' ORDER BY e.seq ASC LIMIT 1").fetchone()
        sid,audit=row[0],json.loads(row[1]);n=a.input_items or audit['input_items']
        items=[json.loads(r[0])for r in db.execute('SELECT item FROM context_items WHERE session_id=? AND epoch=? ORDER BY id LIMIT ?',(sid,audit['context_epoch'],n))]
    if {i.get('call_id')for i in items if i.get('type')=='function_call'}!={i.get('call_id')for i in items if i.get('type')=='function_call_output'}:raise SystemExit('Not a closed boundary')
    if a.model.startswith('gpt') and a.source=='child':
        items=[i for i in items if i.get('type')!='reasoning']
        for item in items:
            item.pop('id',None);item.pop('status',None);item.pop('metadata',None);item.pop('internal_chat_message_metadata_passthrough',None)
    url,secret,model=config_route(ROOT/'.runtime/app/justai.sqlite3',a.model)
    base=json.loads((ROOT/'tests/cache_lab/fixtures/jaa-synthetic-request-template.json').read_text());base.update({'model':a.model,'input':items,'prompt_cache_key':'strict-causality:'+str(uuid.uuid4()),'max_output_tokens':81920})
    base['input'].append({'role':'user','content':'Diagnostic only. Ignore old project assignments. Call glob once with pattern __cache_diagnostic_inert__, path null, max_results 1. No model tool is executed.'})
    order=['true','omit','true','omit','false','true','false','omit'];report={'model':a.model,'input_items':len(items),'order':order,'only_difference':'tools[*].strict','fixed_same_input_and_key_and_headers':True,'tools_executed':False,'raw_text_saved':False,'results':[]}
    def save():target.write_text(json.dumps(report,indent=2))
    save()
    for index,variant in enumerate(order):
        body=copy.deepcopy(base)
        for tool in body['tools']:
            if variant=='omit':tool.pop('strict',None)
            else:tool['strict']=variant=='true'
        record={'index':index,'variant':variant}
        try:out,stats=request(url,secret,body,{})
        except Exception as error:record['error']=type(error).__name__+': '+str(error).replace(secret,'[redacted]')[:160]
        else:record.update(stats)
        report['results'].append(record);save();print(json.dumps(record),flush=True)
    report['totals']={'requests':len(order),'known_input_tokens':sum(r.get('input_tokens')or 0 for r in report['results']),'known_cached_tokens':sum(r.get('cached_tokens')or 0 for r in report['results']),'known_output_tokens':sum(r.get('output_tokens')or 0 for r in report['results'])};save();print(json.dumps({'totals':report['totals']}),flush=True)
if __name__=='__main__':main()
