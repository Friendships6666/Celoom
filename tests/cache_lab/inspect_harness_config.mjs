// Reads the boot-free CLI dump from stdin; never prints credentials or raw YAML.
import {createRequire} from 'node:module';
const require=createRequire('/usr/lib/node_modules/@deepseek-ai/dsh/package.json');
const YAML=require('yaml');let input='';for await(const chunk of process.stdin)input+=chunk;
const docs=YAML.parseAllDocuments(input,{logLevel:'silent'});
const allowed=new Set(['provider','model','api','baseURL','baseUrl','base_url','cacheRetention','reasoning','reasoningEffort','effort','transport','strict','supportsExplicitPromptCacheMode','sendStore','store','sessionId','reasoningContext','context','serviceTier','id','contextWindow','maxTokens','maxOutputTokens','supportsDeveloperRole','supportsStrictMode','supportsStore','supportsReasoningSummary','promptCacheKeyMode','supportsCacheRetention','summary']);
let count=0;
function walk(value,path=''){
 if(!value||typeof value!=='object')return;
 if(Array.isArray(value)){value.forEach((item,index)=>walk(item,`${path}/${index}`));return;}
 if(path.endsWith('/config/providers/nosafe')){
   const clean=(x)=>Array.isArray(x)?x.map(clean):x&&typeof x==='object'?Object.fromEntries(Object.entries(x).filter(([k])=>!/key|token|password|secret|credential|authorization|header/i.test(k)).map(([k,v])=>[k,clean(v)])):x;
   console.log(JSON.stringify({effective_nosafe_provider:clean(value),no_credentials:true}));
 }
 const settings={};for(const [key,item]of Object.entries(value))if(allowed.has(key)&&(key!=='id'||path.includes('/models/'))&&(item===null||['string','number','boolean'].includes(typeof item))){
   if(typeof item==='string'&&item.length>256)continue;
   // URLs are reduced to scheme/hostname/path, never user-info or query.
   if(/base.?url/i.test(key)&&typeof item==='string'){try{const u=new URL(item);settings[key]=`${u.protocol}//${u.host}${u.pathname}`;}catch{settings[key]='(expression or unresolved)';}}
   else settings[key]=item;
 }
 if(Object.keys(settings).length){console.log(JSON.stringify({path,settings}));count++;}
 for(const [key,item]of Object.entries(value))if(!/key|token|password|secret|credential|authorization|header/i.test(key))walk(item,`${path}/${key}`);
}
for(const [index,doc]of docs.entries()){if(doc.errors.length){console.log(JSON.stringify({document:index,parse_error_count:doc.errors.length}));continue;}walk(doc.toJS(),`document/${index}`);}
console.log(JSON.stringify({sanitized_settings_records:count,raw_config_persisted:false,services_started:false}));
