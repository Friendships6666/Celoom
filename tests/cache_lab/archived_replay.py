#!/usr/bin/env python3
"""PAID read-only replay diagnostic. Existing input returns to SAME configured
provider only; no model tool dispatch. No raw user text, IDs or output persisted.
"""
import argparse,copy,json,sqlite3,uuid
from pathlib import Path
from paid_ab import ROOT,DSH_UA,config_route,request

def main():
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--paid',action='store_true');p.add_argument('--output',required=True);p.add_argument('--cases',default='native,sanitized,session_header,dsh_shape');p.add_argument('--rounds',type=int,default=3);p.add_argument('--model',default='deepseek-v4.1-flash-fast');p.add_argument('--source',choices=['child','gpt_auto'],default='child');p.add_argument('--input-size',type=int,default=0)
    a=p.parse_args();cases=a.cases.split(',')
    if a.model.startswith('gpt')and any(c in ('summary_content','no_reasoning')for c in cases):raise SystemExit('Do not alter GPT opaque reasoning state')
    if not a.paid or any(c not in ('native','sanitized','session_header','dsh_shape','summary_content','no_reasoning','developer_only','user_arrays_only','strict_omit','parallel_omit','summary_include','dsh_ua_only')for c in cases) or not(1<=a.rounds<=4):raise SystemExit('Explicit paid opt-in and bounded cases required')
    output=Path(a.output).resolve()
    if output.exists():raise SystemExit('Refusing overwrite')
    with sqlite3.connect('file:'+str(ROOT/'.runtime/app/justai.sqlite3')+'?mode=ro',uri=True)as db:
        row=db.execute("SELECT e.session_id,e.data FROM events e JOIN sessions s ON s.id=e.session_id WHERE s.parent_id IS NOT NULL AND e.kind='request_audit' AND json_extract(e.data,'$.input_items')>50 AND EXISTS(SELECT 1 FROM events u WHERE u.session_id=e.session_id AND u.kind='usage' AND json_extract(u.data,'$.cached_tokens') BETWEEN 1 AND 1000 AND json_extract(u.data,'$.input_tokens')>50000) ORDER BY e.seq DESC LIMIT 1").fetchone()
        if a.source=='gpt_auto':
            if a.model!='gpt-6.1-sol':raise SystemExit('GPT-native source must use its original model')
            row=db.execute("SELECT e.session_id,e.data FROM events e JOIN events u ON u.session_id=e.session_id AND json_extract(u.data,'$.attempt_id')=json_extract(e.data,'$.attempt_id') WHERE e.kind='request_audit' AND u.kind='usage' AND json_extract(u.data,'$.effective_model')='gpt-6.1-sol' AND json_extract(u.data,'$.service_tier')='auto' ORDER BY e.seq ASC LIMIT 1").fetchone()
        if not row:raise SystemExit('No matching archived low-cache request')
        sid,audit=row[0],json.loads(row[1]);n=a.input_size or audit['input_items']
        items=[json.loads(r[0])for r in db.execute('SELECT item FROM context_items WHERE session_id=? AND epoch=? ORDER BY id LIMIT ?',(sid,audit['context_epoch'],n))]
    # Ensure paired calls at selected boundary, never fabricate missing results.
    calls={i.get('call_id')for i in items if i.get('type')=='function_call'};results={i.get('call_id')for i in items if i.get('type')=='function_call_output'}
    if calls!=results:raise SystemExit('Archived selection is not a closed tool boundary')
    url,secret,model=config_route(ROOT/'.runtime/app/justai.sqlite3',a.model)
    template=json.loads((ROOT/'tests/cache_lab/fixtures/jaa-synthetic-request-template.json').read_text())
    report={'model':model['model'],'input_items':len(items),'archived_low_cache_shape':True,'raw_history_saved':False,'tools_executed':False,'results':[]}
    def save():output.write_text(json.dumps(report,ensure_ascii=False,indent=2))
    save();print(json.dumps({k:v for k,v in report.items()if k!='results'}),flush=True)
    for case in cases:
        body=copy.deepcopy(template);body.update({'model':model['model'],'max_output_tokens':81920,'prompt_cache_key':'archived-diagnostic:'+str(uuid.uuid4()),'input':copy.deepcopy(items)})
        # This final diagnostic turn supersedes all old project assignments. Even
        # if model ignores it, tool output is NEVER executed in this script.
        body['input'].append({'role':'user','content':[{'type':'input_text','text':'Diagnostic protocol check only. Do not resume any prior project task. Call glob once with pattern="__cache_diagnostic_inert__", path=null, max_results=1. No tool will execute. After a tool result says repeat, call the same glob once again.'}]})
        headers={}
        if case=='sanitized':
            for item in body['input']:
                item.pop('metadata',None);item.pop('internal_chat_message_metadata_passthrough',None)
        if case=='session_header':headers['session_id']=body['prompt_cache_key']
        if case=='dsh_shape':
            instructions=body.pop('instructions');body['input'].insert(0,{'role':'developer','content':instructions});body.pop('parallel_tool_calls',None);body['reasoning']['summary']='auto';body['include']=['reasoning.encrypted_content']
            for tool in body['tools']:tool.pop('strict',None)
            for item in body['input']:
                item.pop('metadata',None);item.pop('internal_chat_message_metadata_passthrough',None)
                if item.get('role')=='user' and isinstance(item.get('content'),str):item['content']=[{'type':'input_text','text':item['content']}]
            headers={'session_id':body['prompt_cache_key'],'User-Agent':DSH_UA}
        if case=='developer_only':body['input'].insert(0,{'role':'developer','content':body.pop('instructions')})
        if case=='user_arrays_only':
            for item in body['input']:
                if item.get('role')=='user'and isinstance(item.get('content'),str):item['content']=[{'type':'input_text','text':item['content']}]
        if case=='strict_omit':
            for tool in body['tools']:tool.pop('strict',None)
        if case=='parallel_omit':body.pop('parallel_tool_calls',None)
        if case=='summary_include':body['reasoning']['summary']='auto';body['include']=['reasoning.encrypted_content']
        if case=='dsh_ua_only':headers['User-Agent']=DSH_UA
        if case=='summary_content':
            for item in body['input']:
                if item.get('type')=='reasoning'and item.get('summary'):
                    text='\n'.join(part.get('text','')for part in item.pop('summary')if isinstance(part,dict));item['content']=[{'type':'reasoning_text','text':text}]
        if case=='no_reasoning':body['input']=[i for i in body['input']if i.get('type')!='reasoning']
        original=copy.deepcopy(body['input'])
        for turn in range(a.rounds):
            record={'case':case,'turn':turn,'input_items':len(body['input']),'old_input_preserved':body['input'][:len(original)]==original}
            try:out,stats=request(url,secret,body,headers)
            except Exception as error:
                record['error']=type(error).__name__+': '+str(error).replace(secret,'[redacted]')[:160];report['results'].append(record);save();print(json.dumps(record),flush=True);break
            record.update(stats);report['results'].append(record);save();print(json.dumps(record),flush=True)
            body['input']+=out
            calls=[i for i in out if i.get('type')=='function_call']
            if calls:body['input']+=[{'type':'function_call_output','call_id':i['call_id'],'output':'DIAGNOSTIC ONLY: no tool executed. Repeat the exact inert glob call once.'}for i in calls]
            else:body['input'].append({'role':'user','content':'Repeat the inert glob diagnostic call only; do not perform real project work.'})
    report['totals']={'requests':len(report['results']),'known_input_tokens':sum(r.get('input_tokens')or 0 for r in report['results']),'known_cached_tokens':sum(r.get('cached_tokens')or 0 for r in report['results']),'known_output_tokens':sum(r.get('output_tokens')or 0 for r in report['results'])};save();print(json.dumps({'totals':report['totals']}),flush=True)
if __name__=='__main__':main()
