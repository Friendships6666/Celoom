#!/usr/bin/env python3
"""Verify active model flags through real application request construction.
All endpoint URLs and keys are replaced in memory before the isolated service
sees the configuration. Only local mock HTTP is used; no provider calls.
"""
import copy,json,sqlite3,subprocess,sys,tempfile,threading,time,urllib.error
from pathlib import Path
sys.path.insert(0,str(Path(__file__).resolve().parents[1]))
from integration import API,Provider,ThreadingHTTPServer,free_port,ROOT

def main():
    with sqlite3.connect('file:'+str(ROOT/'.runtime/app/justai.sqlite3')+'?mode=ro',uri=True)as db:
        source=json.loads(db.execute("SELECT value FROM meta WHERE key='config'").fetchone()[0])
    selections=[{'site_id':s['id'],'key_id':k['id'],'model_id':m['id']}for s in source['sites']for k in s['keys']for m in k['models']]
    assert selections
    upstream=ThreadingHTTPServer(('127.0.0.1',0),Provider);threading.Thread(target=upstream.serve_forever,daemon=True).start()
    results=[]
    try:
        with tempfile.TemporaryDirectory(prefix='cache-defaults-offline-')as temp:
            work=Path(temp)/'work';work.mkdir();(work/'fixture.txt').write_text('Synthetic local cache fixture\n')
            port=free_port();process=subprocess.Popen([str(ROOT/'target/release/justaiagent'),'--port',str(port),'--data-dir',str(Path(temp)/'data'),'--workspace',str(work)],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
            try:
                api=API(port)
                for _ in range(100):
                    try:api.request('/api/bootstrap');break
                    except urllib.error.URLError:
                        assert process.poll()is None;time.sleep(.03)
                assert api.request('/api/setup',{'password':'synthetic-only-validation-2026'})[0]==200
                test_config=copy.deepcopy(source);test_config['revision']=0
                for site in test_config['sites']:
                    site['base_url']=f'http://127.0.0.1:{upstream.server_port}/v1'
                    for key in site['keys']:key['secret']='fake-key-one'
                test_config['network']={'proxy_mode':'direct','connect_timeout_secs':2,'headers_timeout_secs':5,'idle_timeout_secs':2,'request_timeout_secs':10,'max_attempts':1}
                test_config['search']={'provider':'duckduckgo','base_url':'','api_key':None,'has_api_key':False}
                status,saved=api.request('/api/config',test_config,'PUT');assert status==200
                for selection in selections:
                    start=len(Provider.requests);sid=api.session(selection);detail=api.run(sid,'NATIVE_PREFIX_TEST')
                    assert detail['session']['status']=='completed'
                    bodies=[r[1]for r in Provider.requests[start:]];assert len(bodies)==2
                    for body in bodies:
                        assert len(body['tools'])==14
                        assert all('strict'not in t and t['parameters']['additionalProperties']is False and len(t['parameters']['required'])==len(t['parameters']['properties'])for t in body['tools'])
                        assert body['parallel_tool_calls']is False
                        assert body['prompt_cache_key']==f'justai:v1:{sid}'
                    audits=[e['data']for e in detail['events']if e['type']=='request_audit']
                    assert audits[-1]['expected_prefix_preserved']and audits[-1]['native_output_replayed']and audits[-1]['tools_unchanged']
                    assert any(e['type']=='tool_end'and e['data']['name']=='glob'and not e['data']['is_error']for e in detail['events'])
                    results.append({'model':bodies[0]['model'],'tool_count':14,'strict_marker_omitted':True,'complete_schema_preserved':True,'stable_cache_key':True,'native_prefix_preserved':True,'local_tool_roundtrip':True})
            finally:process.terminate();process.wait(timeout=5)
    finally:upstream.shutdown();upstream.server_close()
    report={'production_config_revision':source['revision'],'models':results,'provider_requests':0,'private_history_used':False,'production_settings_modified':False}
    output=ROOT/'.test-artifacts/cache-defaults-configuration-check.json'
    with output.open('x')as file:json.dump(report,file,ensure_ascii=False,indent=2)
    print(json.dumps(report,ensure_ascii=False))
if __name__=='__main__':main()
