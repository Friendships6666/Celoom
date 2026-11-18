#!/usr/bin/env python3
"""Capture a SYNTHETIC JustAIAgent request via a local mock, never production data."""
import json,subprocess,sys,tempfile,threading,time,urllib.error
from pathlib import Path
sys.path.insert(0,str(Path(__file__).resolve().parents[1]))
from integration import API,Provider,ThreadingHTTPServer,free_port,ROOT

def main():
    upstream=ThreadingHTTPServer(('127.0.0.1',0),Provider);threading.Thread(target=upstream.serve_forever,daemon=True).start()
    with tempfile.TemporaryDirectory(prefix='jaa-cache-shape-') as temp:
        port=free_port();process=subprocess.Popen([str(ROOT/'target/release/justaiagent'),'--port',str(port),'--data-dir',temp+'/db','--workspace',temp],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
        try:
            api=API(port)
            for _ in range(100):
                try:api.request('/api/bootstrap');break
                except urllib.error.URLError:time.sleep(.03)
            assert api.request('/api/setup',{'password':'synthetic-only-password-2026'})[0]==200
            model={'id':'m','name':'Synthetic','model':'test-model','supports_images':True,'max_output_tokens':81920,'context_window':1000000,'reasoning_efforts':['high'],'reasoning_effort':'high','strict_tools':True,'reasoning_replay':False,'send_store':True}
            config={'revision':0,'sites':[{'id':'s','name':'Mock','base_url':f'http://127.0.0.1:{upstream.server_port}/v1','keys':[{'id':'k','name':'Mock','secret':'fake-key-one','models':[model]}]}],'network':{'proxy_mode':'direct','max_attempts':1}}
            status,response=api.request('/api/config',config,'PUT');assert status==200,response
            result=api.run(api.session({'site_id':'s','key_id':'k','model_id':'m'}),'Synthetic protocol capture only.')
            assert result['session']['status']=='completed'
            body=Provider.requests[0][1];body.pop('prompt_cache_key',None);body['input']=[]
            target=ROOT/'tests/cache_lab/fixtures/jaa-synthetic-request-template.json'
            if target.exists():raise SystemExit('Refusing to overwrite fixture')
            target.write_text(json.dumps(body,ensure_ascii=False,indent=2));print(json.dumps({'synthetic_fixture_saved':True,'tool_names':[t['name']for t in body['tools']],'top_level_fields':sorted(body),'no_paid_requests':True,'credentials_saved':False}))
        finally:process.terminate();process.wait(timeout=5)
    upstream.shutdown();upstream.server_close()
if __name__=='__main__':main()
