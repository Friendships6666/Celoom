#!/usr/bin/env python3
"""Explicitly authorized PAID synthetic evidence capture. No private conversation
input; no generated tool execution; no credential/cookie values persisted.
Original SSE bytes are kept for audit, beside the exact nonsecret JSON request.
"""
import argparse,datetime,hashlib,http.client,json,os,ssl,time,uuid
from pathlib import Path
from paid_ab import ROOT,config_route,DSH_UA


def utc():return datetime.datetime.now(datetime.timezone.utc).isoformat(timespec='milliseconds')
def write_new(path,content):
    raw=content.encode()if isinstance(content,str)else content
    fd=os.open(path,os.O_WRONLY|os.O_CREAT|os.O_EXCL,0o600)
    with os.fdopen(fd,'wb')as f:f.write(raw)
def digest(raw):return hashlib.sha256(raw).hexdigest()

def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--paid',action='store_true');p.add_argument('--directory',required=True)
    a=p.parse_args()
    if not a.paid:raise SystemExit('Explicit paid authorization required')
    root=Path(a.directory).resolve();root.mkdir(parents=True,exist_ok=False,mode=0o700)
    url,secret,_=config_route(ROOT/'.runtime/app/justai.sqlite3','gpt-6.1-sol')
    # This exact text is fully synthetic and may safely accompany the raw capture.
    prompt='Synthetic protocol evidence check. Reply with exactly EVIDENCE_OK. Do not call tools, inspect files, use the network, or quote any prior conversation. This request contains no private task.'
    cases=[('minimal',{'model':'gpt-6.1-sol','input':[{'role':'user','content':prompt}],'stream':True,'store':False,'max_output_tokens':256,'reasoning':{'effort':'high'}},{'User-Agent':'JustAIAgent-cache-evidence/1'}),('harness_shape',{'model':'gpt-6.1-sol','input':[{'role':'developer','content':'Answer the synthetic protocol check concisely.'},{'role':'user','content':[{'type':'input_text','text':prompt}]}],'stream':True,'store':False,'max_output_tokens':1024,'reasoning':{'effort':'high','summary':'auto'},'include':['reasoning.encrypted_content'],'prompt_cache_key':'evidence:'+str(uuid.uuid4())},{'User-Agent':DSH_UA,'session_id':'evidence-session-'+str(uuid.uuid4()),'X-Client-Request-Id':str(uuid.uuid4()),'X-Stainless-Arch':'x64','X-Stainless-Lang':'js','X-Stainless-OS':'Linux','X-Stainless-Package-Version':'7.19.0','X-Stainless-Retry-Count':'0','X-Stainless-Runtime':'node','X-Stainless-Runtime-Version':'v26.11.1'})]
    summary={'created_at_utc':utc(),'endpoint_origin':f'{url.scheme}://{url.netloc}','endpoint_path':url.path,'synthetic_only':True,'tools_executed':False,'credentials_persisted':False,'cases':[]}
    for name,body,extra in cases:
        folder=root/name;folder.mkdir(mode=0o700)
        payload=json.dumps(body,ensure_ascii=False,separators=(',',':')).encode()
        headers={'Content-Type':'application/json','Accept':'text/event-stream',**extra}
        write_new(folder/'request-body.json',payload)
        write_new(folder/'request-metadata.json',json.dumps({'method':'POST','url':f'{url.scheme}://{url.netloc}{url.path}','headers':headers,'authentication':'Bearer present on actual request; value deliberately omitted','body_sha256':digest(payload),'no_private_history':True},indent=2))
        record={'case':name,'started_at_utc':utc(),'requested_model':body['model'],'request_body_sha256':digest(payload)}
        conn=http.client.HTTPSConnection(url.hostname,url.port or 443,timeout=30,context=ssl.create_default_context());start=time.monotonic();raw=bytearray();terminal=None
        try:
            conn.connect();cert=conn.sock.getpeercert(binary_form=True);record['tls_peer_certificate_sha256']=digest(cert);record['peer_ip']=conn.sock.getpeername()[0]
            conn.request('POST',url.path,payload,headers={**headers,'Authorization':'Bearer '+secret})
            response=conn.getresponse();record['http_status']=response.status
            allow={'date','content-type','content-length','transfer-encoding','server','x-request-id','request-id','openai-request-id','cf-ray','x-amzn-requestid','x-amzn-trace-id','x-processed-by'}
            safe_headers=[(k,v)for k,v in response.getheaders()if k.lower()in allow]
            write_new(folder/'response-headers.json',json.dumps({'status':response.status,'headers':safe_headers,'cookie_and_auth_headers_excluded':True},indent=2))
            pending=[]
            if response.status!=200:
                raw.extend(response.read(262144))
                # Error bodies might contain echoed request credentials; preserve
                # a fixed marker instead if such a value ever occurs.
                if secret.encode()in raw:raw=bytearray(b'[credential-containing error body withheld]')
            else:
                while time.monotonic()-start<120 and len(raw)<4*1024*1024:
                    left=120-(time.monotonic()-start);sock=conn.sock or getattr(getattr(response.fp,'raw',None),'_sock',None)
                    if sock:sock.settimeout(min(30,max(.1,left)))
                    line=response.readline(1024*1024)
                    if not line:break
                    raw.extend(line)
                    text=line.decode('utf-8').rstrip('\r\n')
                    if text.startswith('data:'):pending.append(text[5:].lstrip())
                    elif not text and pending:
                        value='\n'.join(pending);pending=[]
                        if value=='[DONE]':break
                        event=json.loads(value)
                        if event.get('type')in('response.completed','response.failed','response.incomplete'):
                            terminal=event.get('response');break
                if secret.encode()in raw:raise RuntimeError('Credential echo detected; raw capture withheld')
            if terminal is not None:
                record.update({'terminal_status':terminal.get('status'),'reported_model':terminal.get('model'),'model_matches_requested':terminal.get('model')==body['model'],'usage':terminal.get('usage'),'response_id':terminal.get('id')})
                write_new(folder/'terminal-response.json',json.dumps(terminal,ensure_ascii=False,indent=2))
        except Exception as e:record['capture_error']=type(e).__name__+': '+str(e).replace(secret,'[redacted]')[:180]
        finally:
            conn.close();record['finished_at_utc']=utc();record['elapsed_seconds']=round(time.monotonic()-start,3)
            if secret.encode()not in raw:
                write_new(folder/'response.sse',bytes(raw));record['response_sse_sha256']=digest(raw);record['response_bytes']=len(raw)
            write_new(folder/'observation.json',json.dumps(record,ensure_ascii=False,indent=2))
        summary['cases'].append(record);print(json.dumps({k:v for k,v in record.items()if k not in('peer_ip','response_id','usage')},ensure_ascii=False),flush=True)
    write_new(root/'summary.json',json.dumps(summary,ensure_ascii=False,indent=2))
    entries=[]
    for path in sorted(root.glob('**/*')):
        if path.is_file():entries.append({'file':str(path.relative_to(root)),'bytes':path.stat().st_size,'sha256':digest(path.read_bytes())})
    write_new(root/'manifest.json',json.dumps({'created_at_utc':utc(),'hash_algorithm':'SHA-256','entries':entries,'limitations':'Local unsigned capture; hashes detect later changes but are not a third-party timestamp/signature. Reported model string does not prove actual underlying weights.'},indent=2))
    # Read-only snapshot: never rewrite evidence to annotate later conclusions.
    for path in root.glob('**/*'):
        if path.is_file():path.chmod(0o400)
    print(json.dumps({'evidence_directory':str(root),'captured_cases':len(summary['cases']),'mismatches':sum(x.get('model_matches_requested')is False for x in summary['cases']),'files_read_only':True,'credentials_saved':False}),flush=True)
if __name__=='__main__':main()
