import { describe, expect, it } from 'vitest';
import { isNetworkResult, networkConclusion, networkStatus, qualityPlanCounts, resultConclusion, routeHopCount, safeRedirectLocation, safeNetworkHeaders, type QualityPlan, type QualityProbeResult } from '../src/qualityChecks';

const result = (observation: QualityProbeResult['observation'] = {}, status = 'ok'): QualityProbeResult => ({ id: 'network', label: 'Network fixture', group: 'network', variant: 'http_head', repeat: 1, status, observation });

describe('network quality observations keep model semantics separate', () => {
  it('recognizes network group, protocol and nested observation independently', () => {
    expect(isNetworkResult(result())).toBe(true);
    expect(isNetworkResult({ ...result({ protocol: 'network' }), group: 'legacy' })).toBe(true);
    expect(isNetworkResult({ ...result({ network: { kind: 'connection' } }), group: 'legacy' })).toBe(true);
    expect(isNetworkResult({ ...result(), group: 'routing' })).toBe(false);
  });
  it.each([200, 301, 401, 404, 405, 503])('HTTP %s means an observed response, not a model failure', status => {
    const r = { ...result({ network: { kind: 'http_head', http: { status } } }), analysis: { model_match: false, answer_correct: false } };
    expect(resultConclusion(r)).toContain(`HTTP ${status} 已响应`);
    expect(resultConclusion(r)).not.toMatch(/自报不一致|答案未匹配|未返回判断/);
  });
  it('does not invent connectivity or stage latency when observations are missing', () => {
    expect(resultConclusion(result())).toContain('已记录网络观测');
    expect(networkStatus(null)).toBe('未知');
    expect(networkStatus('unavailable')).toBe('无法检查');
  });
  it('distinguishes missing tools, policy skips, cancellation and actual errors', () => {
    expect(networkConclusion(result({ error: { kind: 'unavailable' } }))).toContain('当前环境无法检查');
    expect(networkConclusion(result({ network: { route: { status: 'skipped' } } }))).toContain('已跳过直连探测');
    expect(networkConclusion(result({}, 'cancelled'))).toContain('已取消');
    expect(networkConclusion(result({ error: { kind: 'transport', message: '连接超时' } }, 'error'))).toBe('连接超时');
  });
  it('only reports total hops after a complete route that reached the target', () => {
    expect(routeHopCount({ status: 'ok', reached_target: true, hop_count: 3 })).toBe('3');
    for (const route of [null, undefined, {}, { status: 'partial', reached_target: true, hop_count: 12 }, { status: 'ok', reached_target: false, hop_count: 12 }, { status: 'ok', reached_target: true, hop_count: null }, { status: 'ok', reached_target: true, hop_count: NaN }]) expect(routeHopCount(route)).toContain('未知');
    expect(networkConclusion(result({ network: { kind: 'route_trace', route: { status: 'partial', hops: [{ ttl: 1, ip: null, rtt_ms: null, status: '*' }] } } }))).toContain('总跳数未知');
  });
  it('preserves explicit plan task and HTTP caps including zero provider requests', () => {
    const plan = { request_count: 5, provider_request_count: 0, network_request_count: 5, max_network_http_requests: 4, probes: [] } as unknown as QualityPlan;
    expect(qualityPlanCounts(plan)).toEqual({ provider: 0, network: 5, maxHttp: 4 });
    expect(qualityPlanCounts({ ...plan, request_count: 14, provider_request_count: 14, network_request_count: 0, max_network_http_requests: 0 })).toEqual({ provider: 14, network: 0, maxHttp: 0 });
  });
  it('supports old plans without introducing network tasks', () => {
    expect(qualityPlanCounts({ request_count: 14, probes: [] } as unknown as QualityPlan)).toEqual({ provider: 14, network: 0, maxHttp: 0 });
    expect(qualityPlanCounts({ request_count: 18, probes: Array.from({ length: 4 }, () => ({ group: 'network' })) } as unknown as QualityPlan)).toEqual({ provider: 14, network: 4, maxHttp: 4 });
  });
});

describe('redirect display excludes secrets and executable locations', () => {
  it.each([
    ['https://user:password@fixture.test/path?token=private#secret', 'https://fixture.test/path'],
    ['/v1/responses?token=private#secret', '/v1/responses'],
    ['https://[::1]:8443/v1?key=fixture', 'https://[::1]:8443/v1'],
  ])('strips query, credentials and fragment from %s', (input, expected) => expect(safeRedirectLocation(input)).toBe(expected));
  it.each(['javascript:alert(1)', 'data:text/plain,private', '//user:password@host/path', 'not a URL', '', null, undefined])('rejects unsafe or missing location %s', input => expect(safeRedirectLocation(input)).toBeNull());
});


it('only displays safe network header clues and redacts Location credentials and queries', () => {
  expect(safeNetworkHeaders({ Server: 'cloudflare', 'CF-Ray': 'fixture-ray', Via: 'proxy\r\nheader', Location: 'https://user:password@fixture.test/v1?secret=fixture#fragment', 'Set-Cookie': 'sensitive-cookie', Authorization: 'sensitive-key', 'Proxy-Authorization': 'sensitive-proxy', 'x-arbitrary-secret': 'sensitive-other', 'x-cache': 1 })).toEqual({ server: 'cloudflare', 'cf-ray': 'fixture-ray', via: 'proxy  header', location: 'https://fixture.test/v1' });
  expect(safeNetworkHeaders(null)).toEqual({});
});
