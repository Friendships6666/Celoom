#!/usr/bin/env node
/**
 * Offline DSH/Responses input normalization helper.
 * stdin: { model, instructions?, input: Responses-native items }
 * stdout: normalized input array only. Uses the installed Pi converter, no HTTP,
 * filesystem credential/config lookup, server, network client, or raw logging.
 */
import assert from 'node:assert/strict';
import {dirname,join} from 'node:path';
import {pathToFileURL} from 'node:url';

const ROOT='/usr/lib/node_modules/@deepseek-ai/dsh';
const PI=join(ROOT,'node_modules/@earendil-works/pi-ai/dist');
const {convertResponsesMessages}=await import(pathToFileURL(join(PI,'api/openai-responses-shared.js')));
const allowedToolCallProviders=new Set(['openai','openai-codex','opencode']);
const MAX_INPUT_BYTES=64*1024*1024;
const MAX_ITEMS=100000;

function fixedError(code){return new Error(`NORMALIZE_DSH_INPUT:${code}`)}
function parseArguments(value){
  if(typeof value!=='string')throw fixedError('INVALID_FUNCTION_ARGUMENTS');
  try{return JSON.parse(value)}catch{throw fixedError('INVALID_FUNCTION_ARGUMENTS')}
}
function imagePart(part){
  if(typeof part.image_url!=='string')throw fixedError('UNSUPPORTED_IMAGE_SOURCE');
  const match=/^data:(image\/(?:png|jpeg|webp|gif));base64,([A-Za-z0-9+/]+={0,2})$/i.exec(part.image_url);
  if(!match||match[2].length%4!==0)throw fixedError('UNSUPPORTED_IMAGE_SOURCE');
  const bytes=Buffer.from(match[2],'base64');
  if(bytes.toString('base64')!==match[2])throw fixedError('INVALID_IMAGE_DATA');
  return {type:'image',mimeType:match[1].toLowerCase(),data:match[2]};
}
function messageContent(content,allowImage){
  if(typeof content==='string')return [{type:'text',text:content}];
  if(!Array.isArray(content))throw fixedError('UNSUPPORTED_CONTENT');
  return content.map(part=>{
    if(!part||typeof part!=='object')throw fixedError('UNSUPPORTED_CONTENT_PART');
    if(['input_text','output_text','text'].includes(part.type)&&typeof part.text==='string')return {type:'text',text:part.text};
    if(part.type==='input_image'&&allowImage)return imagePart(part);
    throw fixedError('UNSUPPORTED_CONTENT_PART');
  });
}
function responseOutputToPi(input,model){
  if(!Array.isArray(input)||input.length>MAX_ITEMS)throw fixedError('INVALID_INPUT_ITEMS');
  const messages=[];const calls=new Map();const seenCalls=new Set();let assistantContent=[];
  function flushAssistant(){
    if(!assistantContent.length)return;
    messages.push({role:'assistant',api:model.api,provider:model.provider,model:model.id,content:assistantContent,stopReason:'toolUse',timestamp:0});
    assistantContent=[];
  }
  function appendTextMessage(item){
    // DSH combines a native message's textual parts into one signed text block.
    // Its actual converter restores id/phase, annotations=[], completed status.
    const parts=messageContent(item.content,false);
    const text=parts.map(part=>part.text).join('');
    const block={type:'text',text};
    if(typeof item.id==='string')block.textSignature=JSON.stringify({v:1,id:item.id,...(item.phase?{phase:item.phase}:{})});
    else if(item.phase!==undefined)block.textSignature=JSON.stringify({v:1,id:'',phase:item.phase});
    assistantContent.push(block);
  }
  for(const item of input){
    if(!item||typeof item!=='object'||Array.isArray(item)||(typeof item.type!=='string'&&!['user','system','developer','assistant'].includes(item.role)))throw fixedError('INVALID_NATIVE_ITEM');
    if((item.type==='message'||item.type===undefined)&&item.role==='assistant'){
      appendTextMessage(item);continue;
    }
    if(item.type==='message'&&!['user','system','developer'].includes(item.role))throw fixedError('UNSUPPORTED_MESSAGE_ROLE');
    if(item.type==='reasoning'){
      const signature=JSON.stringify(item);
      let visible='';
      if(item.content!==undefined){
        const parts=Array.isArray(item.content)?item.content:typeof item.content==='string'?[{type:'reasoning_text',text:item.content}]:null;
        if(!parts)throw fixedError('UNSUPPORTED_REASONING_CONTENT');
        visible=parts.filter(part=>part&&typeof part.text==='string').map(part=>part.text).join('\n');
      }else visible=Array.isArray(item.summary)?item.summary.filter(part=>part&&typeof part.text==='string').map(part=>part.text).join('\n'):'';
      // Signature is authoritative, not visible summary or guessed hidden text.
      assistantContent.push({type:'thinking',thinking:visible,thinkingSignature:signature});continue;
    }
    if(item.type==='function_call'){
      if(typeof item.call_id!=='string'||!item.call_id||typeof item.name!=='string'||!item.name)throw fixedError('INVALID_FUNCTION_IDENTITY');
      if(item.call_id.includes('|')||(typeof item.id==='string'&&item.id.includes('|')))throw fixedError('UNREPRESENTABLE_PIPE_ID');
      if(seenCalls.has(item.call_id))throw fixedError('DUPLICATE_FUNCTION_CALL_ID');
      seenCalls.add(item.call_id);
      const piId=`${item.call_id}|${typeof item.id==='string'?item.id:''}`;
      calls.set(item.call_id,{id:piId,name:item.name});
      const block={type:'toolCall',id:piId,name:item.name,arguments:parseArguments(item.arguments)};if(item.namespace!==undefined)block.namespace=item.namespace;
      assistantContent.push(block);continue;
    }
    if(item.type==='function_call_output'){
      if(typeof item.call_id!=='string'||!calls.has(item.call_id))throw fixedError('UNPAIRED_FUNCTION_OUTPUT');
      const call=calls.get(item.call_id);calls.delete(item.call_id);
      let content;
      if(typeof item.output==='string')content=item.output.length===0?[]:[{type:'text',text:item.output}];
      else if(Array.isArray(item.output))content=messageContent(item.output,true);
      else throw fixedError('UNSUPPORTED_TOOL_OUTPUT');
      flushAssistant();
      // Real Pi converts empty/whitespace-only results to "(no tool output)".
      messages.push({role:'toolResult',toolCallId:call.id,toolName:call.name,content,isError:false,timestamp:0});continue;
    }
    if(item.type==='input_text'||item.type==='input_image'||item.type==='user_message'||item.role==='user'){
      if(calls.size)throw fixedError('UNPAIRED_FUNCTION_CALL');
      flushAssistant();
      const content=item.role==='user'?messageContent(item.content,true):item.type==='input_text'?[{type:'text',text:item.text}]:item.type==='input_image'?[imagePart(item)]:messageContent(item.content,true);
      messages.push({role:'user',content:content.every(part=>part.type==='text')?content.map(part=>part.text).join(''):content,timestamp:0});continue;
    }
    if(item.type==='developer'||item.type==='system'||item.role==='system'||item.role==='developer'){
      flushAssistant();
      const content=typeof item.content==='string'?item.content:messageContent(item.content,false).map(p=>p.text??'').join('');
      messages.push({role:'system',content:content?[{type:'text',text:content}]:[],timestamp:0});continue;
    }
    throw fixedError('UNSUPPORTED_NATIVE_ITEM_TYPE');
  }
  flushAssistant();
  if(calls.size)throw fixedError('UNPAIRED_FUNCTION_CALL');
  return messages;
}
export function normalize(input){
  if(!input||typeof input!=='object'||Array.isArray(input)||!Array.isArray(input.input))throw fixedError('INVALID_REQUEST');
  const id=typeof input.model==='string'&&input.model?input.model:null;if(!id)throw fixedError('INVALID_MODEL');
  const model={id,provider:'nosafe',api:'openai-responses',reasoning:true,input:['text','image'],compat:{supportsDeveloperRole:true,supportsMidConvoSystemMessages:true,supportsStrictMode:false}};
  const messages=responseOutputToPi(input.input,model);
  if(input.instructions!==undefined&&typeof input.instructions!=='string')throw fixedError('INVALID_INSTRUCTIONS');
  if(typeof input.instructions==='string'&&input.instructions.length){
    let leading=0;const parts=[input.instructions];
    while(messages[leading]?.role==='system'){
      const content=messages[leading].content;parts.push(typeof content==='string'?content:content.map(p=>p.text??'').join(''));leading++;
    }
    messages.splice(0,leading,{role:'system',content:[{type:'text',text:parts.filter(Boolean).join('\n\n')}],timestamp:0});
  }
  return convertResponsesMessages(model,{messages,tools:[]},allowedToolCallProviders,{supportsMidConvoSystemMessages:true,supportsAdditionalTools:false,supportsToolSearch:false,toolOptions:{supportsStrictMode:false}});
}
function selfTest(){
  const image='AQIDBA==';const imageUrl=`data:image/png;base64,${image}`;
  const native=[
    {type:'developer',content:'Instruction fixture.'},
    {role:'user',content:[{type:'input_text',text:'original user text'},{type:'input_image',image_url:imageUrl,detail:'low'}]},
    {type:'reasoning',id:'rs_self',summary:[{type:'summary_text',text:'summary fixture'}],encrypted_content:'OPAQUE_SELFTEST_CIPHER'},
    {type:'message',id:'msg_self',role:'assistant',phase:'commentary',status:'completed',metadata:{synthetic:true},content:[{type:'output_text',text:'visible fixture',annotations:[]}]},
    {type:'function_call',id:'fc_self',status:'completed',metadata:{synthetic:true},call_id:'call_self',name:'read',arguments:'{\n "path" : "fixture.txt" }'},
    {type:'function_call_output',call_id:'call_self',output:'fixture output'},
    {type:'function_call',id:'foreign_non_fc_id',status:'completed',call_id:'call_empty',name:'read',arguments:'{"path":"empty.txt"}'},
    {type:'function_call_output',call_id:'call_empty',output:''},
  ];
  const untouched=JSON.stringify(native);const output=normalize({model:'cache-lab-model',instructions:'Instruction fixture.',input:native});
  assert.equal(JSON.stringify(native),untouched);
  assert.deepEqual(output.find(item=>item.type==='reasoning'),native[2]);
  const visible=output.find(item=>item.type==='message'&&item.content?.[0]?.text==='visible fixture');assert.equal(visible?.phase,'commentary');assert.equal(visible?.id,'msg_self');assert.ok(!('metadata'in visible));
  const call=output.find(item=>item.type==='function_call'&&item.call_id==='call_self');assert.equal(call.id,'fc_self');assert.equal(call.arguments,'{"path":"fixture.txt"}');assert.ok(!('status'in call));assert.ok(!('metadata'in call));
  assert.ok(output.some(item=>item.role==='user'&&item.content.some(part=>part.type==='input_image'&&part.image_url===imageUrl)));assert.equal(output.find(item=>item.role==='user').content[0].text,'original user text');
  const empty=output.find(item=>item.type==='function_call_output'&&item.call_id==='call_empty');assert.equal(empty.output,'(no tool output)');assert.equal(output.find(item=>item.type==='function_call'&&item.call_id==='call_empty').id,undefined);
  assert.deepEqual(output.filter(item=>item.type==='function_call_output').map(item=>item.call_id),['call_self','call_empty']);
  assert.equal(output.filter(item=>item.role==='developer').length,1);
  const final=normalize({model:'cache-lab-model',input:[{type:'message',id:'msg_final',role:'assistant',phase:'final_answer',content:[{type:'output_text',text:'first'},{type:'output_text',text:'second'}]}]});assert.equal(final[0].phase,'final_answer');assert.equal(final[0].content[0].text,'firstsecond');
  const toolImage=normalize({model:'cache-lab-model',input:[{type:'function_call',id:'fc_img',call_id:'call_img',name:'image',arguments:'{}'},{type:'function_call_output',call_id:'call_img',output:[{type:'input_text',text:'caption'},{type:'input_image',image_url:imageUrl}]}]});assert.equal(toolImage[1].output[1].image_url,imageUrl);
  const blank=normalize({model:'cache-lab-model',input:[{type:'function_call',id:'fc_blank',call_id:'call_blank',name:'empty',arguments:'{}'},{type:'function_call_output',call_id:'call_blank',output:' \n\t '}]});assert.equal(blank[1].output,'(no tool output)');
  assert.throws(()=>normalize({model:'cache-lab-model',input:[{role:'user',content:[{type:'input_image',image_url:'https://invalid.test/image.png'}]}]}),/UNSUPPORTED_IMAGE_SOURCE/);
  assert.throws(()=>normalize({model:'cache-lab-model',input:[{type:'function_call',call_id:'orphan',name:'read',arguments:'{}'}]}),/UNPAIRED_FUNCTION_CALL/);
  const noIdPhase=normalize({model:'cache-lab-model',input:[{type:'message',role:'assistant',phase:'commentary',content:[{type:'output_text',text:'portable'}]}]});assert.equal(noIdPhase[0].phase,'commentary');assert.ok(noIdPhase[0].id.startsWith('msg_pi_'));
  const emptyUser=normalize({model:'cache-lab-model',input:[{role:'user',content:[]}]});assert.deepEqual(emptyUser,[{role:'user',content:[{type:'input_text',text:''}]}]);
  assert.throws(()=>normalize({model:'cache-lab-model',input:[{role:'user',content:[{type:'input_audio',data:'not-supported'}]}]}),/UNSUPPORTED_CONTENT_PART/);
  console.error(JSON.stringify({self_test:'passed',actualConverter:'pi-ai convertResponsesMessages',input_items:native.length,output_items:output.length,raw_input_logged:false,real_network:false}));
}
async function readStdin(){
  const chunks=[];let length=0;
  for await(const chunk of process.stdin){length+=chunk.length;if(length>MAX_INPUT_BYTES)throw fixedError('INPUT_TOO_LARGE');chunks.push(chunk);}
  return Buffer.concat(chunks).toString('utf8');
}
if(process.argv.includes('--self-test')){
  try{selfTest()}catch{console.error('NORMALIZE_DSH_INPUT:SELF_TEST_FAILED');process.exitCode=2;}
}else{
  try{const normalized=normalize(JSON.parse(await readStdin()));process.stdout.write(JSON.stringify(normalized));}
  catch(error){console.error(error instanceof Error&&error.message.startsWith('NORMALIZE_DSH_INPUT:')?error.message:'NORMALIZE_DSH_INPUT:INVALID_REQUEST');process.exitCode=2;}
}
