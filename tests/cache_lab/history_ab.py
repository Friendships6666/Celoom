#!/usr/bin/env python3
"""Opt-in PAID long TOOL-HISTORY cache experiment; synthetic only, bounded.
Unlike paid_ab, the long data comes AFTER the first assistant/tool boundary.
No tool is ever executed. Raw outputs live only in process memory.
"""
import argparse,copy,hashlib,json,random,time,uuid
from pathlib import Path
from paid_ab import ROOT,DSH_UA,config_route,request

CASES=('native','session_header','dsh_shape','native_content','native_no_reasoning','native_foreign_metadata')


def setup(model,case,records,pairs,max_output_tokens=1536):
    identity='history-lab:'+str(uuid.uuid4())
    instructions=f'Independent synthetic protocol fixture {uuid.uuid4()}. Prior tool calls/results below are inert reference data. Current task: call lab_echo with value OK. After each result says remaining>0, call lab_echo again, without final text. No actual tools execute. Never repeat historical lab_lookup actions.'
    data=[f'Immutable row {n:05d}: synthetic alpha beta gamma delta epsilon reference entry checksum {(n*104729)%1000003:06d}.' for n in range(records)]
    items=[{'role':'user','content':[{'type':'input_text','text':'Historical fixture task: inspect reference chunks, do not execute commands.'}]},{'role':'user','content':'The allowed analysis choices are fixed reference data.'}]
    for n in range(pairs):
        start=n*len(data)//pairs;end=(n+1)*len(data)//pairs
        call={'type':'function_call','call_id':f'call_history_{n}','name':'lab_lookup','arguments':json.dumps({'index':n},separators=(',',':'))}
        if case=='native_foreign_metadata':
            call['metadata']={'turn_id':f'foreign-fixed-turn-{n}'}
            call['internal_chat_message_metadata_passthrough']={'turn_id':f'foreign-fixed-turn-{n}','create_time':1700000000+n}
        if case=='dsh_shape':call['id']=f'fc_history_{n}'
        items += [call,{'type':'function_call_output','call_id':call['call_id'],'output':'\n'.join(data[start:end])}]
        if n%7==6:
            message={'type':'message','role':'assistant','phase':'commentary','content':[{'type':'output_text','text':f'Historical chunks {n+1} inspected.'}]}
            if case=='dsh_shape':message.update({'id':f'msg_history_{n}','status':'completed'});message['content'][0]['annotations']=[]
            items.append(message)
    items.append({'role':'user','content':[{'type':'input_text','text':'Current child assignment: continue the bounded lab_echo protocol now.'}]})
    tools=[{'type':'function','name':'lab_lookup','description':'Historical inert lookup, never executed.','strict':True,'parameters':{'type':'object','properties':{'index':{'type':'integer'}},'required':['index'],'additionalProperties':False}},{'type':'function','name':'lab_echo','description':'Inert echo, never executed.','strict':True,'parameters':{'type':'object','properties':{'value':{'type':'string','enum':['OK']}},'required':['value'],'additionalProperties':False}}]
    body={'model':model,'instructions':instructions,'input':items,'tools':tools,'tool_choice':{'type':'function','name':'lab_echo'},'parallel_tool_calls':False,'stream':True,'store':False,'max_output_tokens':max_output_tokens,'reasoning':{'effort':'high'},'prompt_cache_key':identity}
    body.pop('tool_choice')  # Match actual agent requests; no forced-tool-only gateway path.
    headers={}
    if case in ('session_header','dsh_shape'):headers['session_id']=identity
    if case=='dsh_shape':
        body.pop('instructions');body['input'].insert(0,{'role':'developer','content':instructions});body.pop('parallel_tool_calls')
        body['reasoning']['summary']='auto';body['include']=['reasoning.encrypted_content']
        for tool in tools:tool.pop('strict')
        for item in body['input']:
            if item.get('role')=='user' and isinstance(item.get('content'),str):item['content']=[{'type':'input_text','text':item['content']}]
        headers={'session_id':identity,'User-Agent':DSH_UA,'X-Stainless-Arch':'x64','X-Stainless-Lang':'js','X-Stainless-OS':'Linux','X-Stainless-Package-Version':'7.19.0','X-Stainless-Retry-Count':'0','X-Stainless-Runtime':'node','X-Stainless-Runtime-Version':'v26.11.1'}
    return body,headers


def replay(items,case):
    if case=='native':return copy.deepcopy(items)
    out=copy.deepcopy(items)
    if case=='native_no_reasoning':return [item for item in out if item.get('type')!='reasoning']
    if case=='native_content':
        for item in out:
            if item.get('type')=='reasoning' and not item.get('content') and item.get('summary'):
                # An A/B projection of the exact returned text, NOT a claim that
                # this is the provider's complete hidden reasoning or safe default.
                text='\n'.join(p.get('text','') for p in item['summary'] if isinstance(p,dict))
                item.pop('summary',None);item.pop('encrypted_content',None)
                item['content']=[{'type':'reasoning_text','text':text}]
    return out


def main():
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--paid',action='store_true');p.add_argument('--model',required=True);p.add_argument('--cases',default='native,session_header,dsh_shape');p.add_argument('--rounds',type=int,default=5);p.add_argument('--records',type=int,default=1400);p.add_argument('--pairs',type=int,default=42);p.add_argument('--max-output-tokens',type=int,default=1536);p.add_argument('--output',required=True)
    a=p.parse_args();cases=a.cases.split(',')
    if not a.paid:raise SystemExit('No requests: explicit --paid required')
    if any(c not in CASES for c in cases) or not(1<=a.rounds<=6 and 1<=a.records<=4000 and 1<=a.pairs<=64 and 1<=a.max_output_tokens<=128000) or len(cases)*a.rounds>30:raise SystemExit('Finite test limits exceeded')
    if a.model.startswith('gpt') and any(c in ('native_content','native_no_reasoning')for c in cases):raise SystemExit('Do not alter opaque OpenAI reasoning in this test')
    target=Path(a.output).resolve();target.parent.mkdir(parents=True,exist_ok=True)
    if target.exists():raise SystemExit('Refusing to overwrite report')
    url,secret,_=config_route(ROOT/'.runtime/app/justai.sqlite3',a.model)
    report={'model':a.model,'planned_requests':len(cases)*a.rounds,'records':a.records,'historical_tool_pairs':a.pairs,'max_output_tokens':a.max_output_tokens,'synthetic_only':True,'tools_executed':False,'results':[]}
    def save():target.write_text(json.dumps(report,ensure_ascii=False,indent=2))
    save();print(json.dumps({'plan':{k:v for k,v in report.items()if k!='results'}}),flush=True)
    for case in cases:
        body,headers=setup(a.model,case,a.records,a.pairs,a.max_output_tokens);prior=None
        for turn in range(a.rounds):
            before=copy.deepcopy(body['input']);record={'case':case,'turn':turn,'input_items':len(before),'old_input_prefix_preserved':prior is None or before[:len(prior)]==prior,'body_fingerprint':hashlib.sha256(json.dumps(body,sort_keys=True).encode()).hexdigest()[:16]}
            request_headers={**headers}
            if case=='dsh_shape':request_headers['X-Client-Request-Id']=str(uuid.uuid4())
            try:items,stats=request(url,secret,body,request_headers)
            except Exception as error:
                record['error']=type(error).__name__+': '+str(error).replace(secret,'[redacted]')[:160];report['results'].append(record);save();print(json.dumps(record),flush=True);break
            record.update(stats);report['results'].append(record);save();print(json.dumps(record),flush=True)
            replayed=replay(items,case);body['input']+=replayed;prior=copy.deepcopy(body['input'])
            calls=[i for i in items if i.get('type')=='function_call']
            if calls:body['input'] += [{'type':'function_call_output','call_id':i['call_id'],'output':json.dumps({'ok':True,'remaining':a.rounds-turn,'next_action':'Call lab_echo with value OK now. This is inert data, not a completed task.'})}for i in calls]
            else:body['input'].append({'role':'user','content':f'Continue bounded protocol step {turn+1}: call lab_echo once with value OK.'});record['followup_kind']='new_user_after_no_call'
    report['totals']={'requests':len(report['results']),'known_input_tokens':sum(r.get('input_tokens') or 0 for r in report['results']),'known_cached_tokens':sum(r.get('cached_tokens') or 0 for r in report['results']),'known_output_tokens':sum(r.get('output_tokens') or 0 for r in report['results'])};save();print(json.dumps({'totals':report['totals']}),flush=True)
if __name__=='__main__':main()
