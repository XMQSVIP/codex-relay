import { useToast } from './Toast';
import { useState } from 'react';
import { ChevronDown } from 'lucide-react';
import { api } from './api';
import type { AccountPolicy, NativeFeature, Operations } from './api';

const time = (value: number | null | undefined) => value == null ? '暂无记录' : new Date(value).toLocaleString();
const supported = (value: boolean | null | undefined) => value == null ? '未知' : value ? '原生支持' : '原生不支持';

export function CapabilityStatus({policy,token,revision,disabled,onChange}:{policy:AccountPolicy;token:string;revision:number;disabled:boolean;onChange:()=>void}) {
  const [pending,setPending]=useState(false); const notify = useToast();
  const caps=policy.capabilities; const health=policy.health;
  const toggle=async(feature:NativeFeature,enabled:boolean)=>{
    setPending(true);
    const disabledFeatures = new Set(policy.disabledFeatures ?? []);
    if (enabled) disabledFeatures.delete(feature); else disabledFeatures.add(feature);
    try {await api(token,`/accounts/${policy.id}/features`,{revision,disabledFeatures:[...disabledFeatures]});notify(enabled?'已恢复自动接入。':'已关闭该能力。');onChange();}
    catch(e){notify(e instanceof Error?e.message:'设置失败', 'error');onChange();}finally{setPending(false);}
  };
  const verify=async(feature:NativeFeature)=>{
    setPending(true);
    try{await api(token,`/accounts/${policy.id}/features/verify`,{feature});notify('能力诊断已通过，接入开关保持不变。');onChange();}
    catch(e){notify(e instanceof Error?e.message:'诊断未通过', 'error');onChange();}finally{setPending(false);}
  };
  return <div className="runtime-details">
    <p>模型目录：{caps&&!caps.stale?'已读取':'待刷新'} · 最近成功请求：{time(health?.lastVerifiedAt)}</p>
    <p role="status">{health?.state==='cooling'?`冷却中，${time(health.retryAt)} 后尝试恢复`:health?.state==='halfOpen'?'等待一次请求验证恢复':health?'未触发上游冷却':'暂无上游健康记录'}{health?.reason?` · ${health.reason}`:''}{health?.sharedUpstream?' · 同提供方共享冷却':''}</p>
    <details><summary>能力与原生版本</summary><p>目录：{caps?`${caps.models.length} 个模型 · ${caps.stale?'待刷新，接入不受影响':'已刷新'} · ${time(caps.checkedAt)}`:'尚未读取，仍可直接发起请求'}{caps?.errorCode?` · ${caps.errorCode}`:''}</p><p>搜索：{supported(caps?.webSearch)}；生图：{supported(caps?.imageGeneration)}。目录用于选路参考，实际能力由原生程序和模型决定。</p><p className="helper">安装指纹：{caps?.installation?.slice(0,16)??'未知'}；可直接使用 Key 已授权的模型。</p>
      <p>客户端 function、custom、namespace 和 MCP 工具默认可用，无需验证。</p>
      <p className="helper">以下能力默认自动接入；取消勾选后，使用该能力的请求将被此账户拒绝。目录过期、重启或更换模型不会自动关闭开关。</p>
      <div className="capability-options">{([['imageInput','内嵌图片输入'],['webSearch','搜索'],['imageGeneration','生图']] as const).map(([feature,label])=><div key={feature}><label className="checkbox-label"><input type="checkbox" checked={!policy.disabledFeatures?.includes(feature)} disabled={disabled||pending||!!policy.active} onChange={e=>void toggle(feature,e.target.checked)}/>{label}</label><button className="button secondary" aria-label={`${label}运行诊断`} disabled={disabled||pending||!!policy.active} onClick={()=>void verify(feature)}>{pending?'处理中…':'运行诊断（可选）'}</button>{caps?.verificationErrors?.[feature]?<span className="field-error">上次诊断：{caps.verificationErrors[feature]}</span>:null}</div>)}</div>
      <p className="helper">诊断仅用于排障，会发送真实模型请求并消耗用量，不改变接入开关。搜索与生图诊断可能需要约两分钟。</p>

    </details>
  </div>;
}
export function StorageStatus({operations}:{operations:Operations}) {
  const s=operations.responseBindings;
  if (!s) return null;
  return <details className="storage-status"><summary><span>续聊账户绑定</span><span>{'error' in s ? '读取失败' : `${s.count} 个指纹`}</span><ChevronDown size={14} aria-hidden="true" /></summary>
    {'error' in s ? <p className="field-error">暂时无法读取账户绑定状态。</p> : <p className="helper">绑定元数据已用 {(s.bytes/1048576).toFixed(1)} / {(s.maxBytes/1048576).toFixed(0)} MiB，有效续聊后保留 {Math.round(s.ttlSeconds/86400)} 天，重启后仍可识别原账户。</p>}
    <p className="helper">仅保存续聊所需的账户绑定指纹，聊天正文不写入本地数据库。临时上下文随连接结束释放，聊天记录由客户端管理。</p>
  </details>;
}
