// Boot-free config stdin comparison only. Never prints secrets, hashes, or key names.
import {createRequire} from 'node:module';
import {DatabaseSync} from 'node:sqlite';
import {readFileSync} from 'node:fs';
const require=createRequire('/usr/lib/node_modules/@deepseek-ai/dsh/package.json');const YAML=require('yaml');
let text='';for await(const chunk of process.stdin)text+=chunk;
const docs=YAML.parseAllDocuments(text,{logLevel:'silent'});
const db=new DatabaseSync(new URL('../../.runtime/app/justai.sqlite3',import.meta.url),{readOnly:true});
const config=JSON.parse(db.prepare("SELECT value FROM meta WHERE key='config'").get().value);db.close();
function walk(value){
 if(!value||typeof value!=='object')return;
 if(value.providers?.nosafe){
  const source=value.providers.nosafe;let candidate=source.apiKey??(typeof source.apiKeyEnv==='string'?process.env[source.apiKeyEnv]:undefined);
  const pid=process.argv.find(a=>/^--pid=\d+$/.test(a))?.split('=')[1];let processEnvReadable=null;
  if(candidate===undefined&&pid&&typeof source.apiKeyEnv==='string'){
    try{const bytes=readFileSync(`/proc/${pid}/environ`);const prefix=Buffer.from(source.apiKeyEnv+'=');const found=bytes.subarray().toString('utf8').split('\0').find(s=>s.startsWith(prefix.toString()));candidate=found?.slice(prefix.length);processEnvReadable=true;}catch{processEnvReadable=false;}
  }
  const literal=typeof candidate==='string'&&candidate.startsWith('sk-');
  const matches=literal?config.sites.flatMap(s=>s.keys.map(k=>({site:s.base_url,key:k.secret}))).filter(k=>k.key===candidate).length:null;
  console.log(JSON.stringify({provider:'nosafe',credential_source_kind:literal?'literal':typeof candidate,provider_has_api_key_env:typeof source.apiKeyEnv==='string',same_literal_as_any_project_key:literal?matches>0:null,matched_key_count:matches,credentials_not_printed:true,process_env_readable:processEnvReadable}));
 }
 if(Array.isArray(value))value.forEach(walk);else for(const[k,v]of Object.entries(value))if(!/key|token|password|secret|credential|authorization|header/i.test(k))walk(v);
}
for(const doc of docs)if(!doc.errors.length)walk(doc.toJS());
