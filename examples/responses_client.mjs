// npm install openai; configure BRIDGE_BASE_URL, BRIDGE_API_KEY, BRIDGE_MODEL.
import OpenAI from 'openai';
const client = new OpenAI({baseURL:process.env.BRIDGE_BASE_URL ?? 'http://127.0.0.1:4317/v1',apiKey:process.env.BRIDGE_API_KEY});
const model = process.env.BRIDGE_MODEL;
const tools=[{type:'function',name:'client_time',description:'Read UTC time on this client',parameters:{type:'object',properties:{},additionalProperties:false},strict:false}];
let input=[{role:'user',content:'Call client_time and tell me the current UTC time.'}];
for(let turn=0;turn<8;turn++) {
  const response=await client.responses.create({model,input,tools,store:false});
  if(response.status!=='completed') throw new Error(`Response ${response.status}`);
  input.push(...response.output);
  const calls=response.output.filter(item=>item.type==='function_call');
  if(!calls.length) {console.log(response.output_text);break;}
  for(const call of calls) {
    if(call.name!=='client_time' || Object.keys(JSON.parse(call.arguments)).length) throw new Error('Unexpected tool call');
    input.push({type:'function_call_output',call_id:call.call_id,output:new Date().toISOString()});
  }
  if(turn===7) throw new Error('Tool round-trip limit reached');
}
