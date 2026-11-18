import { useEffect, useMemo, useRef, useState } from 'react';
import { Download, Play, RefreshCw, Square } from 'lucide-react';
import { api, errorMessage } from './api';
import { Alert, Field, Modal, Spinner } from './ui';
import { sameSelection, type Selection } from './types';
import { cacheLabel, headerReportedModel, instructionProbes, parsePricing, qualityActive, qualityGroups, qualityLatency, qualityPrice, qualityStatus, qualityNumber, qualityTps, resultConclusion, isNetworkResult, qualityPlanCounts, type InstructionProbeId, type QualityCheckInput, type QualityGroup, type QualityPlan, type QualityProbeResult, type QualityRun } from './qualityChecks';
import { NetworkDetails, NetworkSummary } from './QualityNetwork';

function RunResult({ result, run }: { result: QualityProbeResult; run: QualityRun }) {
  const o = result.observation ?? {}; const usage = o.usage ?? {};
  if (isNetworkResult(result)) return <tr className="quality-network-row"><th scope="row"><strong>{result.label || result.id}</strong><small>{result.variant} · 第 {result.repeat} 次</small></th>
    <td>—<small>网络探测</small></td><td>—<small>响应头 {qualityLatency(o.header_latency_ms)}</small></td>
    <td>{qualityLatency(o.total_ms)}</td><td>—</td><td><span>HTTP {qualityNumber(o.http_status ?? o.network?.http?.status)}</span><small>network · {o.terminal_status || '未知'}</small></td>
    <td><span>{o.network?.target?.host || '目标未返回'}</span><small>{o.network?.path === 'configured_proxy' ? '代理路径' : o.network?.path === 'direct' ? '直接路径' : '路径未知'}</small></td>
    <td>—<small>网络任务</small></td><td>0 费用<small>网络探测</small></td>
    <td><span>{resultConclusion(result)}</span><details><summary>网络 / CDN / 路由详情</summary><NetworkDetails network={o.network} />{o.error && <p>{o.error.kind}: {o.error.message}</p>}</details></td>
  </tr>;
  const headerModel = headerReportedModel(o.headers);
  return <tr><th scope="row"><strong>{result.label || result.id}</strong><small>{result.variant} · 第 {result.repeat} 次</small></th>
    <td><span>{run.source?.model || '未返回'}</span><strong className={result.analysis?.model_match === false ? 'danger-text' : ''}>{o.response_model || headerModel || '未返回'}</strong>{headerModel && o.response_model && headerModel !== o.response_model && <small>响应头：{headerModel}</small>}</td>
    <td>{qualityLatency(o.first_token_ms)}<small>响应头 {qualityLatency(o.header_latency_ms)}<br />首事件 {qualityLatency(o.first_event_ms)}{o.first_reasoning_ms != null && <><br />首推理 {qualityLatency(o.first_reasoning_ms)}</>}</small></td>
    <td>{qualityLatency(o.total_ms)}{result.analysis?.output_tokens_per_sec != null && <small>{qualityTps(result.analysis.output_tokens_per_sec)}</small>}</td><td>{cacheLabel(usage)}</td>
    <td><span>HTTP {qualityNumber(o.http_status)}</span><small>{o.protocol || '未返回'}<br />{o.terminal_status || '未返回终态'}</small></td>
    <td><span>{o.service_tier || '未返回 tier'}</span><small>请求 API {run.source?.api || 'responses'}<br />上游自报 API {o.reported_api || '未返回'}<br />上游自报提供方 {o.reported_provider || '未返回'}<br />{run.source?.endpoint || '未返回端点'}</small><details><summary>服务端标识</summary><pre>{JSON.stringify(o.headers ?? {}, null, 2)}</pre><small>推理模式 {o.reasoning_context || '未返回'}<br />响应 ID {o.response_id || '未返回'}<br />系统指纹 {o.system_fingerprint || '未返回'}<br />{o.request_started_at || '未返回时间'}</small></details></td>
    <td><span>输入 {qualityNumber(usage.input_tokens)}</span><small>输出 {qualityNumber(usage.output_tokens)}</small><details><summary>完整数值用量</summary><pre>{o.usage == null ? '未返回 usage' : JSON.stringify(o.usage, null, 2)}</pre></details></td>
    <td>{qualityPrice(result.cost, run.request?.pricing)}{result.cost?.notes?.map((note, index) => <small key={index}>{note}</small>)}</td>
    <td><span>{resultConclusion(result)}</span><details><summary>答案 / 工具 / 拒绝详情</summary><p>答案匹配：{result.analysis?.answer_correct === true ? '是' : result.analysis?.answer_correct === false ? '否' : '未返回'} · 工具参数：{result.analysis?.tool_valid === true ? '有效' : result.analysis?.tool_valid === false ? '无效' : '未返回'}</p><pre>{o.text_sample || '未返回文本样本'}</pre>{o.reasoning_sample && <><strong>推理摘要样本</strong><pre>{o.reasoning_sample}</pre></>}{o.refusal_sample && <><strong>拒绝样本</strong><pre>{o.refusal_sample}</pre></>}{o.output != null && <><strong>终态输出（未执行）</strong><pre>{JSON.stringify(o.output, null, 2)}</pre></>}{o.error && <p>{o.error.kind}: {o.error.message}</p>}<small>请求摘要 {o.body_sha256 || '未返回'}<br />响应摘要 {o.response_sha256 || '未返回'}</small></details></td>
  </tr>;
}

export function QualityChecks({ selection, modelName, onClose }: { selection: Selection; modelName: string; onClose: () => void }) {
  const [groups, setGroups] = useState<QualityGroup[]>(['routing', 'cache', 'protocol']);
  const [instructions, setInstructions] = useState<InstructionProbeId[]>(['ignore_previous', 'child_role', 'independent_task']);
  const [extended, setExtended] = useState(false); const [large, setLarge] = useState(false); const [networkTrace, setNetworkTrace] = useState(false);
  const [currency, setCurrency] = useState('CNY'); const [inputRate, setInputRate] = useState(''); const [cachedRate, setCachedRate] = useState(''); const [outputRate, setOutputRate] = useState('');
  const [plan, setPlan] = useState<QualityPlan | null>(null); const [plannedKey, setPlannedKey] = useState('');
  const [runs, setRuns] = useState<QualityRun[]>([]); const [selectedId, setSelectedId] = useState('');
  const [listLoading, setListLoading] = useState(true); const [planning, setPlanning] = useState(true); const [action, setAction] = useState<'start' | 'cancel' | 'export' | null>(null);
  const [error, setError] = useState(''); const [planError, setPlanError] = useState(''); const [listError, setListError] = useState(''); const [pollError, setPollError] = useState('');
  const [refresh, setRefresh] = useState(0); const [planRefresh, setPlanRefresh] = useState(0); const [pollRefresh, setPollRefresh] = useState(0);
  const alive = useRef(true); const actionLock = useRef(false);
  const startIdentity = useRef<{ key: string; id: string } | null>(null);
  useEffect(() => { alive.current = true; return () => { alive.current = false; }; }, []);
  const proposal = useMemo(() => {
    try {
      if (!groups.length) throw new Error('请至少选择一组检查。');
      const pricing = parsePricing(currency, inputRate, cachedRate, outputRate);
      const input: QualityCheckInput = { selection, groups, instruction_probes: instructions, extended_cache: groups.includes('cache') && extended, network_trace: groups.includes('network') && networkTrace, context_size: groups.includes('cache') && large ? 'large' : 'small', pricing };
      return { input, issue: '', key: JSON.stringify(input) };
    } catch (reason) { return { input: null, issue: errorMessage(reason), key: '' }; }
  }, [selection, groups, instructions, extended, large, networkTrace, currency, inputRate, cachedRate, outputRate]);
  const run = runs.find(item => item.id === selectedId); const active = runs.some(item => qualityActive(item.status));
  const acceptRun = (next: QualityRun) => {
    if (!alive.current || !sameSelection(next.selection, selection)) return;
    setRuns(current => {
      const previous = current.find(item => item.id === next.id);
      if (previous && (previous.updated_at > next.updated_at || (!qualityActive(previous.status) && qualityActive(next.status)))) return current;
      return [next, ...current.filter(item => item.id !== next.id)].sort((a, b) => b.created_at.localeCompare(a.created_at));
    });
  };
  useEffect(() => {
    const controller = new AbortController(); setListLoading(true); setListError('');
    void api.qualityRuns(selection, controller.signal).then(result => {
      if (controller.signal.aborted) return;
      const matching = result.filter(item => sameSelection(item.selection, selection));
      setRuns(current => {
        const merged = new Map(matching.map(item => [item.id, item]));
        for (const item of current) { const incoming = merged.get(item.id); if (!incoming || incoming.updated_at <= item.updated_at || (!qualityActive(item.status) && qualityActive(incoming.status))) merged.set(item.id, item); }
        return [...merged.values()].sort((a, b) => b.created_at.localeCompare(a.created_at));
      });
      setSelectedId(id => id || matching.find(item => qualityActive(item.status))?.id || matching[0]?.id || '');
    }).catch(reason => { if (!controller.signal.aborted) setListError(errorMessage(reason)); }).finally(() => { if (!controller.signal.aborted) setListLoading(false); });
    return () => controller.abort();
  }, [selection.site_id, selection.key_id, selection.model_id, refresh]);
  useEffect(() => {
    const controller = new AbortController(); setPlanning(true); setPlanError('');
    if (!proposal.input) { setPlanning(false); setPlan(null); setPlannedKey(''); return; }
    const timer = setTimeout(() => {
      void api.qualityPlan(proposal.input!, controller.signal).then(result => {
        if (controller.signal.aborted) return; setPlan(result); setPlannedKey(proposal.key);
      }).catch(reason => { if (!controller.signal.aborted) { setPlanError(errorMessage(reason)); setPlannedKey(''); } }).finally(() => { if (!controller.signal.aborted) setPlanning(false); });
    }, 200);
    return () => { clearTimeout(timer); controller.abort(); };
  }, [proposal.key, planRefresh]);
  useEffect(() => {
    if (!run || !qualityActive(run.status)) return;
    const controller = new AbortController(); let timer: ReturnType<typeof setTimeout>;
    const poll = async () => {
      try { const next = await api.qualityRun(run.id, controller.signal); if (controller.signal.aborted) return; setPollError(''); acceptRun(next); if (qualityActive(next.status)) timer = setTimeout(poll, 1000); }
      catch (reason) { if (!controller.signal.aborted) setPollError(errorMessage(reason)); }
    };
    timer = setTimeout(() => void poll(), 1000);
    return () => { clearTimeout(timer); controller.abort(); };
    // Status changes stop polling; individual partial results do not spawn a
    // second timer. Reopening reads persisted runs and never posts a paid run.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [run?.id, run?.status, pollRefresh]);
  async function start() {
    if (actionLock.current || !proposal.input || planning || plannedKey !== proposal.key || !plan || plan.request_count > 40 || active) return;
    actionLock.current = true; setAction('start'); setError('');
    if (!startIdentity.current || startIdentity.current.key !== proposal.key) startIdentity.current = { key: proposal.key, id: crypto.randomUUID() };
    const client_request_id = startIdentity.current.id;
    try { const next = await api.startQuality({ ...proposal.input, client_request_id }); acceptRun(next); startIdentity.current = null; if (alive.current) setSelectedId(next.id); }
    catch (reason) { if (alive.current) { setError(`${errorMessage(reason)} 已保留本次请求 ID；先刷新记录确认，同计划手动重试将复用该 ID。`); setRefresh(value => value + 1); } }
    finally { actionLock.current = false; if (alive.current) setAction(null); }
  }
  async function cancel() {
    if (!run || actionLock.current) return; actionLock.current = true; setAction('cancel'); setError('');
    try { acceptRun(await api.cancelQuality(run.id)); } catch (reason) { if (alive.current) setError(errorMessage(reason)); }
    finally { actionLock.current = false; if (alive.current) setAction(null); }
  }
  async function download() {
    if (!run || actionLock.current) return; actionLock.current = true; setAction('export'); setError('');
    try {
      const result = await api.exportQuality(run.id); if (!alive.current) return;
      const url = URL.createObjectURL(new Blob([JSON.stringify(result, null, 2)], { type: 'application/json' }));
      const anchor = document.createElement('a'); anchor.href = url; anchor.download = `model-quality-${run.id.replace(/[^A-Za-z0-9_-]/g, '')}.json`; anchor.click(); setTimeout(() => URL.revokeObjectURL(url), 0);
    } catch (reason) { if (alive.current) setError(errorMessage(reason)); }
    finally { actionLock.current = false; if (alive.current) setAction(null); }
  }
  const totals = run?.summary?.totals as Record<string, unknown> | undefined;
  const findings = Array.isArray(run?.summary?.findings) ? run.summary.findings as { code?: string; severity?: string; message?: string; probe_ids?: string[] }[] : [];
  const currentPlan = plannedKey === proposal.key ? plan : null;
  const counts = currentPlan ? qualityPlanCounts(currentPlan) : null;
  const runCounts = run?.plan ? qualityPlanCounts(run.plan) : null;
  const overBudget = (currentPlan?.request_count ?? 0) > 40;
  return <Modal title="模型质量检查" subtitle={`${modelName} · 仅当前站点 / Key / 模型`} onClose={onClose} className="quality-modal">
    <div className="quality-scroll">
      <div className="quality-intro"><strong>检查计划</strong><p>确认计划后点击开始执行探针。</p></div>
      <fieldset className="quality-options" disabled={action === 'start'}><legend>本次计划</legend>
        <div className="quality-group-grid">{qualityGroups.map(group => <label className="quality-check-option" key={group.id}><input type="checkbox" checked={groups.includes(group.id)} onChange={e => { if (group.id === 'network' && !e.target.checked) setNetworkTrace(false); setGroups(current => e.target.checked ? qualityGroups.map(item => item.id).filter(id => id === group.id || current.includes(id)) : current.filter(id => id !== group.id)); }} /><span><strong>{group.label}</strong><small>{group.hint}</small></span></label>)}</div>
        <details className="quality-instructions" open={groups.includes('routing')}><summary>指令成对探针 · {instructions.length} 项</summary><p className="field-hint">共同任务：Compute 17*19. Reply only with 323. 每条指令与相同控制任务对照，并重复两次。</p><div>{instructionProbes.map(probe => <label key={probe.id} className="quality-phrase"><input type="checkbox" disabled={!groups.includes('routing')} checked={instructions.includes(probe.id)} onChange={e => setInstructions(current => e.target.checked ? instructionProbes.map(item => item.id).filter(id => id === probe.id || current.includes(id)) : current.filter(id => id !== probe.id))} /><span><code>{probe.text}</code><small>{probe.meaning}</small>{probe.id === 'final_report' && <small>该总结要求可能与“仅回复 323”冲突。</small>}</span></label>)}</div></details>
        <div className="quality-extra-options"><label><input type="checkbox" disabled={!groups.includes('cache')} checked={extended} onChange={e => setExtended(e.target.checked)} />扩展缓存对照（增加 8 次请求）</label><label className={large ? 'danger-text' : ''}><input type="checkbox" disabled={!groups.includes('cache')} checked={large} onChange={e => setLarge(e.target.checked)} />大型上下文 · 更高费用（缓存语料最高 256 KiB）</label></div>
        <div className="quality-network-option"><label><input type="checkbox" disabled={!groups.includes('network')} checked={networkTrace} onChange={e => setNetworkTrace(e.target.checked)} />路由跳数（直接路径，ICMP 受限时未知）</label><p className="field-hint">增加 1 项路由跳数探测：直连路径最多 30 跳、总计 12 秒（traceroute / tracepath / mtr）；走代理时跳过。</p></div>
        <details className="quality-pricing"><summary>可选费率 · {proposal.input?.pricing ? '使用手动费率估算' : '未配置费率'}</summary><p className="field-hint">按每百万 token 填写费率以估算费用，留空为未配置。</p><div className="form-grid"><Field label="计价币种"><input value={currency} maxLength={12} onChange={e => setCurrency(e.target.value)} placeholder="CNY / USD" /></Field><Field label="未缓存输入 / 百万 token"><input type="number" min="0" max="1000000000" step="any" value={inputRate} onChange={e => setInputRate(e.target.value)} placeholder="未配置" /></Field><Field label="缓存输入 / 百万 token"><input type="number" min="0" max="1000000000" step="any" value={cachedRate} onChange={e => setCachedRate(e.target.value)} placeholder="未配置" /></Field><Field label="输出 / 百万 token"><input type="number" min="0" max="1000000000" step="any" value={outputRate} onChange={e => setOutputRate(e.target.value)} placeholder="未配置" /></Field></div></details>
      </fieldset>
      {proposal.issue && <Alert>{proposal.issue}</Alert>}
      {planError && <Alert>{planError}<button className="text-button" onClick={() => setPlanRefresh(value => value + 1)}>重试计划</button></Alert>}
      <section className="quality-plan" aria-label="检查计划">{planning ? <Spinner label="生成计划中" /> : currentPlan && <>
        <div className="quality-plan-facts"><span>请求 <b>{currentPlan.request_count} 次</b></span><span>输入估算 <b>约 {qualityNumber(currentPlan.estimated_input_tokens_upper)} token</b></span><span>输入数据 <b>{qualityNumber(currentPlan.estimated_input_bytes_total)} bytes</b></span><span>输出上限 <b>每次 {currentPlan.max_output_tokens_per_request} / 总 {currentPlan.max_output_tokens_total} token</b></span></div>
        {counts && <p className="quality-plan-counts">模型请求 {counts.provider} 次 · 网络任务 {counts.network} 项 · 网络 HTTP 最多 {counts.maxHttp} 次{counts.network > 0 && '（连接探测 1 + HEAD 重复 3）'} · 总任务上限 40</p>}
        {overBudget && <Alert>本计划超过 40 项任务上限，请减少检查项目。</Alert>}
        <p className="field-hint">输入为合成请求估算，输出为请求上限；实际用量以端点返回为准。{currentPlan.context_size === 'large' && <strong className="danger-text"> 大型上下文已启用，费用将显著提高。</strong>}</p>
        <p className="quality-source">Responses API · {currentPlan.source?.site_name} · {currentPlan.source?.model}<br /><code>{currentPlan.source?.endpoint || '未返回端点'}</code> · 配置 r{currentPlan.source?.config_revision ?? '未返回'}</p>
        <details><summary>查看全部 {currentPlan.probes?.length ?? 0} 个探针与限制</summary><ol className="quality-probe-plan">{currentPlan.probes?.map((probe, index) => <li key={`${probe.id}-${index}`}><strong>{probe.label}</strong><span>{probe.group} / {probe.variant} · 第 {probe.repeat} 次 · {probe.input_bytes} bytes · 输出≤{probe.max_output_tokens}</span></li>)}</ol></details>{currentPlan.notes?.map((note, index) => <p className="field-hint" key={index}>{note}</p>)}
      </>}</section>
      {error && <Alert onClose={() => setError('')}>{error}</Alert>}
      <div className="quality-start"><small>{active ? '当前模型已有进行中的检查。' : counts?.provider === 0 ? '本次仅执行网络探测（0 模型请求）。' : '点击开始执行本次检查计划。'}</small><button className="button primary" disabled={!currentPlan || overBudget || planning || !!proposal.issue || active || action !== null || listLoading || !!listError} onClick={() => void start()}>{action === 'start' ? <Spinner label="启动中" /> : <><Play size={15} />开始检查 · {currentPlan?.request_count ?? '…'} 次请求</>}</button></div>
      <section className="quality-history" aria-label="质量检查记录"><div className="section-heading"><h3>检查记录</h3><button className="text-button" onClick={() => setRefresh(value => value + 1)} disabled={listLoading}><RefreshCw size={14} />刷新记录</button></div>
        {listLoading && <Spinner label="读取记录" />}{listError && <Alert>{listError}<button className="text-button" onClick={() => setRefresh(value => value + 1)}>重试记录</button></Alert>}
        {runs.length > 0 ? <div className="quality-run-select"><Field label="查看检查"><select value={selectedId} onChange={e => { setSelectedId(e.target.value); setPollError(''); }}>{runs.map(item => <option key={item.id} value={item.id}>{new Date(item.created_at).toLocaleString()} · {qualityStatus[item.status] || item.status} · {item.completed_probes}/{item.plan?.request_count ?? '?'}</option>)}</select></Field></div> : !listLoading && !listError && <p className="inline-empty">暂无检查记录。</p>}
        {run && <><div className="quality-progress"><div><strong>{qualityStatus[run.status] || run.status} · {run.completed_probes} / {run.plan?.request_count ?? '?'} 次</strong><progress aria-label="检查进度" value={run.completed_probes} max={run.plan?.request_count || 1} /></div><div className="button-row">{qualityActive(run.status) && <button className="button secondary danger-text" disabled={action !== null} onClick={() => void cancel()}>{action === 'cancel' ? <Spinner label="取消中" /> : <><Square size={14} />取消本次检查</>}</button>}<button className="button secondary" disabled={action !== null} onClick={() => void download()}><Download size={14} />导出 JSON</button></div></div>
          {run.error && <Alert>{run.error}</Alert>}{pollError && <Alert>{pollError}<button className="text-button" onClick={() => setPollRefresh(value => value + 1)}>重试进度读取</button></Alert>}
          {runCounts?.provider === 0 ? <div className="quality-summary"><span>仅网络探测（0 模型请求）</span><span>模型费用 0</span></div> : totals && <div className="quality-summary"><span>已完成 {qualityNumber(totals.requests_completed)}</span>{(run.summary?.quality as Record<string, unknown> | undefined)?.total_model_probes != null && <span>通过 {qualityNumber((run.summary?.quality as Record<string, unknown>)?.passed_probes)} / {qualityNumber((run.summary?.quality as Record<string, unknown>)?.total_model_probes)}</span>}{(run.summary?.latency as Record<string, unknown> | undefined)?.median_output_tps != null && <span>中位生成速度 {qualityTps((run.summary?.latency as Record<string, unknown>)?.median_output_tps)}</span>}<span>输入 {qualityNumber(totals.input_tokens)}</span><span>缓存 {qualityNumber(totals.cached_tokens)}</span><span>输出 {qualityNumber(totals.output_tokens)}</span><span>估算费用 {qualityPrice({ currency: typeof totals.currency === 'string' ? totals.currency : undefined, estimated_cost: typeof totals.estimated_cost === 'number' ? totals.estimated_cost : null, complete: totals.usage_complete === true }, run.request?.pricing)}</span><small>{totals.usage_complete === true ? '用量完整' : '部分用量未返回'}</small></div>}
          <NetworkSummary value={run.summary?.network} />
          {findings.map((finding, index) => <div className={`quality-finding ${finding.severity || 'info'}`} key={`${finding.code}-${index}`}><strong>{finding.message || finding.code}</strong>{finding.probe_ids?.length ? <small>关联探针：{finding.probe_ids.join('、')}</small> : null}</div>)}
          {run.results?.length > 0 ? <div className="quality-table-scroll" tabIndex={0} aria-label="探针结果表，可横向滚动"><table className="quality-table"><thead><tr>{['探针 / 对照', '请求模型 / 返回自报', '首 token 延迟', '总延迟', '缓存 token / 输入 / 比例', 'HTTP / 协议 / 终态', '来源 / 服务端', '真实用量', '费率估算', '答案 / 工具 / 拒绝结论'].map(label => <th key={label} scope="col">{label}</th>)}</tr></thead><tbody>{run.results.map((result, index) => <RunResult key={`${result.id}-${index}`} result={result} run={run} />)}</tbody></table></div> : <p className="field-hint">尚无探针结果。</p>}
        </>}
      </section>
    </div><footer className="settings-footer"><small>诊断探针</small><button className="button ghost" onClick={onClose}>返回模型设置</button></footer>
  </Modal>;
}
