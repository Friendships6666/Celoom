import { describe, expect, it } from 'vitest';
import { cacheLabel, cacheReading, instructionProbes, parsePricing, qualityActive, qualityLatency, qualityPrice, qualityNumber, resultConclusion, type QualityProbeResult } from '../src/qualityChecks';

describe('quality checks preserve missing data and explicit price semantics', () => {
  it('does not turn missing usage into zero or 0% cached', () => {
    expect(cacheLabel(null)).toBe('未返回'); expect(cacheReading({ input_tokens: 100 })).toEqual({ input: 100, cached: null, percent: null });
    expect(cacheLabel({ input_tokens: 100, input_tokens_details: { cached_tokens: 0 } })).toContain('0.00%');
    expect(cacheLabel({ input_tokens: 100, input_tokens_details: { cached_tokens: 75 } })).toContain('75.00%');
    expect(cacheReading({ input_tokens: 0, input_tokens_details: { cached_tokens: 0 } }).percent).toBeNull();
  });
  it('formats real latency including zero without inventing TTFT', () => {
    expect(qualityLatency(null)).toBe('未返回'); expect(qualityLatency(0)).toBe('0 ms'); expect(qualityLatency(125)).toBe('125 ms');
    expect(qualityNumber(undefined)).toBe('未返回'); expect(qualityNumber(NaN)).toBe('未返回');
  });
  it('blank rates are unknown while an explicit zero rate remains valid', () => {
    expect(parsePricing('CNY', '', '', '')).toBeNull();
    expect(parsePricing('usd', '0', '', '3.5')).toEqual({ currency: 'USD', input_per_million: 0, cached_input_per_million: null, output_per_million: 3.5 });
    expect(qualityPrice({ estimated_cost: 0, complete: true }, null)).toBe('未配置费率');
    expect(qualityPrice({ estimated_cost: 0, complete: true }, parsePricing('USD', '0', '0', '0'))).toBe('USD 0.00');
    expect(qualityPrice({ estimated_cost: null }, parsePricing('CNY', '1', '', '2'))).toBe('无法完整估算');
    expect(qualityPrice({ estimated_cost: 0.01, complete: false }, parsePricing('CNY', '1', '', '2'))).toContain('部分');
  });
  it.each([['CNY', '-1', '', ''], ['CNY', 'NaN', '', ''], ['CNY', '1000000001', '', ''], ['USD<script>', '1', '', '']])('rejects invalid manual pricing %j', (currency, input, cached, output) => {
    expect(() => parsePricing(currency, input, cached, output)).toThrow();
  });
  it('contains all nine exact independent instruction probes', () => {
    expect(instructionProbes).toHaveLength(9); expect(new Set(instructionProbes.map(p => p.id)).size).toBe(9);
    expect(instructionProbes.find(p => p.id === 'child_role')?.text).toBe('You are already the spawned child agent, not the parent.');
    expect(instructionProbes.find(p => p.id === 'final_report')?.text).toBe('Your final answer must summarize the result, evidence, changed files and remaining limitations for your parent.');
    expect(instructionProbes.every(p => p.meaning.length > 0)).toBe(true);
  });
  it('reports only observable conclusions and distinguishes instruction conflicts', () => {
    const result = { status: 'ok', variant: 'instruction_final_report', observation: {}, analysis: { model_match: false, answer_correct: false } } as QualityProbeResult;
    expect(resultConclusion(result)).toContain('自报不一致'); expect(resultConclusion(result)).toContain('指令可能冲突');
    expect(resultConclusion(result)).not.toMatch(/偷换|审查|底模真实/);
    expect(resultConclusion({ ...result, variant: 'control', analysis: { explicit_refusal: true } })).toContain('拒绝信号');
  });
  it('polls only genuinely active run statuses', () => {
    expect(qualityActive('queued')).toBe(true); expect(qualityActive('running')).toBe(true);
    for (const status of ['failed', 'completed', 'cancelled', 'interrupted', undefined]) expect(qualityActive(status)).toBe(false);
  });
});
