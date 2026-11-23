import { qualityLatency, qualityNumber, record, numeric, networkStatus, routeHopCount, safeRedirectLocation, safeNetworkHeaders, type GatewayProbe, type IpGeoSource, type IpReport, type NetworkObservation, type RelayAnalysis } from './qualityChecks';

const textList = (value: unknown) => Array.isArray(value) ? value.filter((item): item is string => typeof item === 'string') : [];
function Notes({ value }: { value: unknown }) { const notes = textList(value); return notes.length ? <ul className="network-notes">{notes.map((note, index) => <li key={index}>{note}</li>)}</ul> : null; }

function countryFlag(code?: string | null) {
  if (!code || !/^[A-Za-z]{2}$/.test(code)) return '';
  return String.fromCodePoint(...code.toUpperCase().split('').map(c => 127397 + c.charCodeAt(0)));
}

function boolBadge(val: boolean | null | undefined, yesText: string, noText: string, warnOnYes = true) {
  if (val === true) return <span className={`ip-badge ${warnOnYes ? 'warn' : 'ok'}`}>{yesText}</span>;
  if (val === false) return <span className={`ip-badge ${warnOnYes ? 'ok' : 'neutral'}`}>{noText}</span>;
  return <span className="ip-badge neutral">未知</span>;
}

function GeoMiniMap({ primary, sources }: { primary?: IpGeoSource | null; sources: IpGeoSource[] }) {
  const valid = sources.filter(s => typeof s.lat === 'number' && typeof s.lon === 'number');
  if (!valid.length) return <p className="field-hint">暂无经纬度定位数据。</p>;
  const project = (lat: number, lon: number) => ({
    x: ((lon + 180) / 360) * 320,
    y: ((90 - lat) / 180) * 140,
  });
  const pLat = primary?.lat ?? valid[0].lat!;
  const pLon = primary?.lon ?? valid[0].lon!;
  const pPt = project(pLat, pLon);
  return (
    <div className="ip-map-wrap">
      <div className="ip-map-coord">{pLat.toFixed(2)}, {pLon.toFixed(2)}</div>
      <svg className="ip-map-svg" viewBox="0 0 320 140" role="img" aria-label="IP 地理位置定位图">
        <rect x="0" y="0" width="320" height="140" rx="8" fill="var(--surface)" />
        {[35, 70, 105].map(y => <line key={`h-${y}`} x1="0" y1={y} x2="320" y2={y} stroke="var(--line)" strokeDasharray="2 3" />)}
        {[80, 160, 240].map(x => <line key={`v-${x}`} x1={x} y1="0" x2={x} y2="140" stroke="var(--line)" strokeDasharray="2 3" />)}
        {valid.slice(1).map((s, idx) => {
          const pt = project(s.lat!, s.lon!);
          return <circle key={idx} cx={pt.x} cy={pt.y} r="3.5" fill="#3b82f6" opacity="0.8"><title>{s.source}: {s.city || s.country}</title></circle>;
        })}
        <circle cx={pPt.x} cy={pPt.y} r="10" fill="var(--accent)" opacity="0.18" />
        <circle cx={pPt.x} cy={pPt.y} r="4.5" fill="var(--accent)" stroke="#fff" strokeWidth="1.5" />
      </svg>
      <div className="ip-map-legend">
        <span><i className="dot-primary" />主定位 · {pLat.toFixed(2)}, {pLon.toFixed(2)}</span>
        {valid.length > 1 && <span><i className="dot-secondary" />其他来源 {valid.length - 1} 个</span>}
      </div>
    </div>
  );
}

function BgpTopologySvg({ bgp }: { bgp: NonNullable<IpReport['bgp']> }) {
  const upstreams = bgp.upstreams ?? [];
  if (!bgp.asn) return null;
  const height = Math.max(120, upstreams.length * 46 + 24);
  const srcY = height / 2;
  return (
    <svg className="ip-bgp-svg" viewBox={`0 0 560 ${height}`} role="img" aria-label="BGP 上游路由拓扑图">
      <g>
        <rect x="12" y={srcY - 22} width="150" height="44" rx="8" className="bgp-node-src" />
        <text x="22" y={srcY - 4} className="bgp-asn">AS{bgp.asn}</text>
        <text x="22" y={srcY + 12} className="bgp-org">{(bgp.as_name || '源自治系统').slice(0, 20)}</text>
      </g>
      {upstreams.length === 0 ? (
        <text x="200" y={srcY + 4} className="bgp-muted">暂未观测到上游邻居路径</text>
      ) : upstreams.map((up, idx) => {
        const y = 26 + idx * 44;
        const strokeW = Math.max(1.2, Math.min(4.5, (up.share_pct ?? 5) / 12 + 1));
        return (
          <g key={up.asn}>
            <path d={`M 162 ${srcY} C 215 ${srcY}, 225 ${y}, 275 ${y}`} fill="none" stroke={up.is_tier1 ? '#3b82f6' : 'var(--muted)'} strokeWidth={strokeW} opacity="0.65" />
            {up.share_pct != null && <text x="210" y={(srcY + y) / 2 - 4} className="bgp-pct">{up.share_pct}%</text>}
            <rect x="275" y={y - 18} width="265" height="36" rx="7" className={up.is_tier1 ? 'bgp-node-tier1' : 'bgp-node-up'} />
            <text x="286" y={y - 2} className="bgp-asn">AS{up.asn}{up.is_tier1 ? ' · Tier 1' : ''}</text>
            <text x="286" y={y + 12} className="bgp-org">{(up.name || '未知上游组织').slice(0, 32)}</text>
          </g>
        );
      })}
    </svg>
  );
}

export function IpReportCard({ report }: { report: IpReport | null | undefined }) {
  if (!report || !report.ip) return null;
  if (report.bogon) {
    return (
      <div className="ip-report">
        <div className="ip-report-head">
          <strong>远端 IP 深度档案 · <code>{report.ip}</code></strong>
          <span className="ip-badge warn">内网 / Bogon</span>
        </div>
        <Notes value={report.notes} />
      </div>
    );
  }
  const sec = report.security ?? {};
  const geo = report.geo ?? {};
  const sources = geo.sources ?? [];
  const bgp = report.bgp;
  const dnsbl = report.dnsbl;
  const ports = report.open_ports ?? [];
  const domains = report.associated_domains ?? [];
  const safeIp = encodeURIComponent(report.ip);

  return (
    <div className="ip-report">
      <div className="ip-report-head">
        <div>
          <strong>远端 IP 深度档案 · <code>{report.ip}</code></strong>
          <small>{report.version}{bgp?.prefix ? ` · ${bgp.prefix}` : ''}{report.isp ? ` · ${report.isp}` : ''}</small>
        </div>
        <div className="ip-report-badges">
          {sec.rpki_status && <span className={`ip-badge ${sec.rpki_status === 'valid' ? 'ok' : sec.rpki_status === 'invalid' ? 'danger' : 'neutral'}`}>RPKI: {sec.rpki_status}</span>}
          {sec.risk_level && <span className={`ip-badge ${sec.risk_level.includes('未发现') ? 'ok' : 'warn'}`}>{sec.risk_level}</span>}
        </div>
      </div>

      <div className="ip-report-grid">
        <div className="ip-card">
          <h6>基础属性</h6>
          <div className="ip-kv"><span>Bogon / 广播</span>{boolBadge(report.bogon, '是', '否 (公网可达)')}</div>
          <div className="ip-kv"><span>反向 DNS (PTR)</span><code>{report.reverse_dns || '—'}</code></div>
          <div className="ip-kv"><span>常见端口探测</span>
            <div className="ip-port-list">
              {ports.length ? ports.map(p => <span key={p.port} className={`ip-port ${p.open ? 'open' : ''}`}>{p.port}</span>) : <small>未探测</small>}
            </div>
          </div>
        </div>

        <div className="ip-card">
          <h6>威胁与信誉</h6>
          <div className="ip-kv"><span>风险标记</span><b>{sec.risk_level || '未知'}</b></div>
          <div className="ip-kv"><span>滥用等级</span><b>{sec.abuse_level || '未知'}</b></div>
          <div className="ip-kv"><span>RPKI 状态</span><b>{sec.rpki_status || '未知'}</b></div>
          <div className="ip-kv"><span>DNSBL 黑名单</span>
            <b>{dnsbl?.checked ? (dnsbl.listed ? `命中 ${dnsbl.hits?.join(', ')}` : `纯净 (0/${dnsbl.providers_checked})`) : (dnsbl?.note || '未检查')}</b>
          </div>
        </div>

        <div className="ip-card">
          <h6>网络类型标记</h6>
          <div className="ip-kv"><span>代理 / VPN</span>{boolBadge(sec.proxy, '检测到', '未检测到')}</div>
          <div className="ip-kv"><span>数据中心 / IDC</span>{boolBadge(sec.hosting, 'IDC 托管', '非 IDC', false)}</div>
          <div className="ip-kv"><span>移动蜂窝网络</span>{boolBadge(sec.mobile, '移动网络', '非移动网络', false)}</div>
          <div className="ip-kv"><span>所属组织</span><b>{report.org || '未知'}</b></div>
        </div>
      </div>

      <div className="ip-report-two">
        <div className="ip-card">
          <h6>地图定位</h6>
          <GeoMiniMap primary={geo.primary} sources={sources} />
        </div>
        <div className="ip-card">
          <h6>地理位置（多源对比）</h6>
          {sources.length ? (
            <ul className="ip-geo-list">
              {sources.map((s, idx) => (
                <li key={idx}>
                  <span className="ip-geo-src">{s.source}</span>
                  <span>{countryFlag(s.country_code)} {[s.country, s.region, s.city].filter(Boolean).join(' / ') || '未知'}</span>
                  {s.lat != null && s.lon != null && <small>{s.lat.toFixed(3)}, {s.lon.toFixed(3)}</small>}
                </li>
              ))}
            </ul>
          ) : <p className="field-hint">多源地理库暂不可用。</p>}
          {geo.max_offset_km != null && <small className="field-hint">多源最大距离偏移 {geo.max_offset_km} km 内</small>}
        </div>
      </div>

      {bgp && bgp.asn && (
        <div className="ip-card">
          <h6>BGP 路由拓扑 · AS{bgp.asn} {bgp.prefix ? `(${bgp.prefix})` : ''} {bgp.observed_paths ? `· ${bgp.observed_paths} 条观测路径` : ''}</h6>
          <BgpTopologySvg bgp={bgp} />
          <small className="field-hint">连线粗细 = 上游路径占比；蓝框 = Tier 1 骨干网。</small>
        </div>
      )}

      {(domains.length > 0 || (bgp?.downstreams?.length ?? 0) > 0 || (bgp?.announced_prefixes?.length ?? 0) > 0) && (
        <div className="ip-report-two">
          {domains.length > 0 && (
            <div className="ip-card">
              <h6>关联域名</h6>
              <div className="ip-chip-list">{domains.map(d => <code key={d}>{d}</code>)}</div>
            </div>
          )}
          {(bgp?.downstreams?.length ?? 0) > 0 && (
            <div className="ip-card">
              <h6>同 AS 下游 / 客户（AS{bgp?.asn}）</h6>
              <div className="ip-chip-list">{bgp!.downstreams!.map(d => <span key={d.asn} className="ip-chip">AS{d.asn}{d.name ? ` · ${d.name.slice(0, 18)}` : ''}</span>)}</div>
            </div>
          )}
          {(bgp?.announced_prefixes?.length ?? 0) > 0 && (
            <div className="ip-card">
              <h6>同 AS 活跃公告网段</h6>
              <div className="ip-chip-list">{bgp!.announced_prefixes!.map(p => <code key={p}>{p}</code>)}</div>
            </div>
          )}
        </div>
      )}

      <div className="ip-tools-bar">
        <span>交叉验证：</span>
        <a href={`https://bgp.tools/ip/${safeIp}`} target="_blank" rel="noopener noreferrer">BGP.tools</a>
        <a href={`https://www.shodan.io/host/${safeIp}`} target="_blank" rel="noopener noreferrer">Shodan</a>
        <a href={`https://www.abuseipdb.com/check/${safeIp}`} target="_blank" rel="noopener noreferrer">AbuseIPDB</a>
        <a href={`https://www.virustotal.com/gui/ip-address/${safeIp}`} target="_blank" rel="noopener noreferrer">VirusTotal</a>
      </div>
    </div>
  );
}
export function GatewayProbeCard({ probe }: { probe: GatewayProbe | null | undefined }) {
  if (!probe || !probe.origin) return null;
  const hints = probe.platform_hints ?? [];
  const apiStatus = probe.api_status;
  const models = probe.v1_models;
  const root = probe.root;
  const health = probe.health;
  return (
    <div className="ip-report">
      <div className="ip-report-head">
        <div>
          <strong>后端网关 / API 匿名探测 · <code>{probe.origin}</code></strong>
          <small>仅发送匿名 GET 请求，未携带任何 API Key</small>
        </div>
        <div className="ip-report-badges">
          {hints.length ? hints.map(h => <span key={h} className="ip-badge ok">{h}</span>) : <span className="ip-badge neutral">通用反向代理 / 未暴露特征</span>}
        </div>
      </div>
      <div className="ip-report-two">
        <div className="ip-card">
          <h6>NewAPI / OneAPI 状态接口 (<code>/api/status</code>)</h6>
          {apiStatus ? (
            <>
              <div className="ip-kv"><span>HTTP 状态</span><b>{qualityNumber(apiStatus.http_status)}</b></div>
              <div className="ip-kv"><span>系统名称</span><b>{apiStatus.system_name || '未返回'}</b></div>
              <div className="ip-kv"><span>网关版本</span><code>{apiStatus.version || '未返回'}</code></div>
              <div className="ip-kv"><span>Server</span><code>{apiStatus.server || '未返回'}</code></div>
            </>
          ) : <p className="field-hint">未响应或超时。</p>}
        </div>
        <div className="ip-card">
          <h6>模型列表接口 (<code>/v1/models</code>) &amp; 根路径</h6>
          {models ? (
            <>
              <div className="ip-kv"><span>/v1/models 状态</span><b>HTTP {qualityNumber(models.http_status)}</b></div>
              <div className="ip-kv"><span>错误类型指纹</span><code>{models.error_type || '未返回'}</code></div>
              {models.public_models_count != null && <div className="ip-kv"><span>公开模型数</span><b>{qualityNumber(models.public_models_count)}</b></div>}
              {models.error_message && <small className="field-hint">{models.error_message}</small>}
            </>
          ) : <p className="field-hint">/v1/models 未响应。</p>}
          {root && <div className="ip-kv"><span>首页 (<code>/</code>)</span><b>HTTP {qualityNumber(root.http_status)}{root.title ? ` · ${root.title}` : ''}{root.server ? ` (${root.server})` : ''}</b></div>}
          {health && <div className="ip-kv"><span>健康检查 (<code>/health</code>)</span><b>HTTP {qualityNumber(health.http_status)}{health.snippet ? ` · ${health.snippet}` : ''}</b></div>}
        </div>
      </div>
    </div>
  );
}

export function RelaySummary({ value }: { value: unknown }) {
  const relay = record(value) as RelayAnalysis;
  if (!Object.keys(relay).length) return null;
  const platforms = relay.gateway_platforms ?? [];
  const signals = relay.nesting_signals ?? [];
  const pool = relay.pool_signals ?? {};
  const risk = relay.nesting_risk || '样本不足';
  const riskCls = risk === '高' ? 'danger' : risk === '中' ? 'warn' : risk === '未见明显套娃' ? 'ok' : 'neutral';
  return (
    <section className="network-run-summary" aria-label="中转站套娃与号池分析">
      <div className="ip-report-head">
        <h4>中转站套娃与号池特征分析</h4>
        <div className="ip-report-badges">
          <span className={`ip-badge ${riskCls}`}>套娃嫌疑：{risk}</span>
          {platforms.map(p => <span key={p} className="ip-badge ok">{p}</span>)}
        </div>
      </div>
      {signals.length > 0 ? (
        <ul className="network-notes">{signals.map((s, idx) => <li key={idx}>{s}</li>)}</ul>
      ) : (
        <p className="field-hint">本次未观测到明显的多级套娃或响应指纹漂移信号。</p>
      )}
      <div className="network-stage-grid">
        <span>system_fingerprint 数 <b>{qualityNumber(pool.distinct_system_fingerprints?.length)}</b></span>
        <span>service_tier 种类 <b>{pool.distinct_service_tiers?.length ? pool.distinct_service_tiers.join(' / ') : '未返回'}</b></span>
        <span>response_id 前缀 <b>{pool.response_id_prefixes?.length ? pool.response_id_prefixes.join(' / ') : '未返回'}</b></span>
        <span>限流余量跳升 <b>{pool.ratelimit_non_monotonic ? '是（疑似多 Key 号池）' : '否 / 未返回'}</b></span>
        <span>首 token 范围 <b>{pool.ttft_min_ms != null && pool.ttft_max_ms != null ? `${qualityLatency(pool.ttft_min_ms)} ~ ${qualityLatency(pool.ttft_max_ms)}` : '未返回'}</b></span>
      </div>
    </section>
  );
}

export function NetworkDetails({ network }: { network: NetworkObservation | null | undefined }) {
  if (!network) return <p className="field-hint">未返回网络观测。</p>;
  const dns = network.dns; const connection = network.connection; const http = network.http; const route = network.route;
  const redirect = safeRedirectLocation(http?.redirect_location);
  const address = network.target ? `${network.target.host || '未知主机'}${network.target.port ? `:${network.target.port}` : ''}` : '未知目标';
  const proxy = network.path === 'configured_proxy';
  const tls = record(connection?.tls);
  return <div className="network-details">
    <div className="network-target"><strong>{address}</strong><span>{network.path === 'direct' ? '直接路径' : proxy ? '代理路径' : '路径未知'}</span><p>{network.policy || '未返回连接策略'}</p>{proxy && <small>经由已配置的代理路径，DNS / TCP / TLS 阶段未知。</small>}</div>
    {dns && <section><h5>DNS</h5><div className="network-stage-grid"><span>状态 <b>{networkStatus(dns.status)}</b></span><span>解析耗时 <b>{qualityLatency(dns.elapsed_ms)}</b></span></div><div className="network-addresses">{textList(dns.addresses).length ? textList(dns.addresses).map((ip, index) => <code key={index}>{ip}</code>) : <small>IP 地址未知或未解析</small>}</div><Notes value={dns.notes} /></section>}
    {connection && <section><h5>连接阶段</h5><div className="network-stage-grid"><span>状态 <b>{networkStatus(connection.status)}</b></span><span>DNS <b>{qualityLatency(connection.dns_ms)}</b></span><span>TCP <b>{qualityLatency(connection.tcp_ms)}</b></span><span>TLS <b>{qualityLatency(connection.tls_ms)}</b></span><span>HTTP 响应头 <b>{qualityLatency(connection.http_header_ms)}</b></span><span>远端 IP <b>{connection.remote_ip || '未知'}</b></span><span>HTTP 版本 <b>{connection.http_version || '未返回'}</b></span></div>{Object.keys(tls).length > 0 && <details><summary>TLS / 证书实际观测</summary><div className="network-stage-grid"><span>校验 <b>{tls.verified === true ? '通过' : networkStatus(typeof tls.status === 'string' ? tls.status : null)}</b></span><span>Subject <b>{typeof tls.subject === 'string' ? tls.subject : '未返回'}</b></span><span>Issuer <b>{typeof tls.issuer === 'string' ? tls.issuer : '未返回'}</b></span><span>有效期至 <b>{typeof tls.expires_at === 'string' ? tls.expires_at : '未返回'}</b></span><span>SHA-256 <b>{typeof tls.certificate_sha256 === 'string' ? tls.certificate_sha256.slice(0, 20) + '…' : '未返回'}</b></span></div><pre>{JSON.stringify(tls, null, 2)}</pre><small>仅展示已返回证书字段。</small></details>}<Notes value={connection.notes} /></section>}
    {http && <section><h5>HTTP HEAD</h5><div className="network-stage-grid"><span>状态 <b>{qualityNumber(http.status)}</b></span><span>HTTP 版本 <b>{http.version || '未返回'}</b></span></div><small>匿名 HEAD 请求。</small>{redirect && <p className="network-redirect">重定向位置：<code>{redirect}</code></p>}<details><summary>HTTP 响应头</summary><pre>{JSON.stringify(safeNetworkHeaders(http.headers), null, 2)}</pre></details></section>}
    {network.cdn && <section><h5>CDN / 代理线索</h5><small>基于 HTTP 响应头识别。</small>{network.cdn.candidates?.length ? <ul className="network-cdn-list">{network.cdn.candidates.map((candidate, index) => <li key={index}><strong>{candidate.name || '未命名线索'}</strong><Notes value={candidate.evidence} /></li>)}</ul> : <p className="field-hint">未检测到 CDN 特征头。</p>}<Notes value={network.cdn.notes} /></section>}
    {route && <section><h5>路由跳数</h5><div className="network-stage-grid"><span>状态 <b>{networkStatus(route.status)}</b></span><span>工具 <b>{route.tool || '未运行'}</b></span><span>确认到达目标 <b>{route.reached_target === true ? '是' : '否'}</b></span><span>总跳数 <b>{routeHopCount(route)}</b></span><span>最多探测 <b>{numeric(route.max_hops) === null ? '未知' : `${qualityNumber(route.max_hops)} 跳`}</b></span></div><Notes value={route.notes} />{route.hops?.length ? <div className="network-hop-scroll"><table className="network-hop-table"><caption>观测跳点（* 为超时跳）</caption><thead><tr><th scope="col">TTL</th><th scope="col">IP</th><th scope="col">RTT</th><th scope="col">状态</th></tr></thead><tbody>{route.hops.map((hop, index) => <tr key={index}><td>{qualityNumber(hop.ttl)}</td><td><code>{hop.ip || '*'}</code></td><td>{hop.rtt_ms == null ? '*' : qualityLatency(hop.rtt_ms)}</td><td>{networkStatus(hop.status)}</td></tr>)}</tbody></table></div> : <small>暂无可见跳点。</small>}</section>}
    {network.ip_report && <section><IpReportCard report={network.ip_report} /></section>}
    {network.gateway_probe && <section><GatewayProbeCard probe={network.gateway_probe} /></section>}
  </div>;
}
export function NetworkSummary({ value }: { value: unknown }) {
  const summary = record(value); if (!Object.keys(summary).length) return null;
  const head = record(summary.head_ms);
  const candidates = Array.isArray(summary.cdn_candidates) ? summary.cdn_candidates : [];
  const ipReport = summary.ip_report && typeof summary.ip_report === 'object' ? summary.ip_report as IpReport : null;
  const gatewayProbe = summary.gateway_probe && typeof summary.gateway_probe === 'object' ? summary.gateway_probe as GatewayProbe : null;
  return <section className="network-run-summary" aria-label="网络观测汇总"><h4>网络观测汇总</h4><div className="network-stage-grid"><span>观测任务 <b>{qualityNumber(summary.samples)}</b></span><span>HTTP 响应样本 <b>{qualityNumber(summary.successful_http_samples)}</b></span><span>HEAD 最小 <b>{qualityLatency(head.min)}</b></span><span>HEAD 最大 <b>{qualityLatency(head.max)}</b></span><span>HEAD 中位数 <b>{qualityLatency(head.median)}</b></span><span>HEAD 平均 <b>{qualityLatency(head.mean)}</b></span><span>HEAD 标准差 <b>{qualityLatency(head.stddev)}</b></span><span>路由状态 <b>{networkStatus(typeof summary.route_status === 'string' ? summary.route_status : null)}</b></span></div><p className="field-hint">HEAD 延迟为本次 HTTP 样本统计。</p>{candidates.length > 0 && <p className="network-summary-cdn">CDN 线索：{candidates.map(item => typeof item === 'string' ? item : typeof record(item).name === 'string' ? String(record(item).name) : '未命名线索').join('、')}</p>}<Notes value={summary.notes} /><IpReportCard report={ipReport} /><GatewayProbeCard probe={gatewayProbe} /></section>;
}
