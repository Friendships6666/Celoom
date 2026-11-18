import { describe, expect, it } from 'vitest';
import { sanitizeConfig } from '../src/api';
import { selectSubagentModel, validateSubagentModels } from '../src/subagentModels';
import type { AppConfig, SubagentModel } from '../src/types';

const model = { id: 'model-a', name: 'Shared model', model: 'api-model', supports_images: true, max_output_tokens: 4096, context_window: 32000, reasoning_efforts: ['low', 'high'], reasoning_effort: 'low', strict_tools: true, reasoning_replay: false, send_store: true };
const selection = { site_id: 'site-a', key_id: 'key-a', model_id: 'model-a' };
const preset = (reasoning_effort: string | null = null, id = 'preset'): SubagentModel => ({ id, name: `Preset ${id}`, selection: { ...selection }, reasoning_effort });
const config = (): AppConfig => ({ revision: 4, network: { proxy_mode: 'direct', proxy_url: '', no_proxy: '', connect_timeout_secs: 10, headers_timeout_secs: 120, idle_timeout_secs: 300, request_timeout_secs: 1800, max_attempts: 3 }, sites: [{ id: 'site-a', name: 'Site A', base_url: 'https://a.example.test/v1', keys: [{ id: 'key-a', name: 'Key A', has_secret: true, models: [structuredClone(model)] }, { id: 'key-b', name: 'Key B', has_secret: true, models: [{ ...structuredClone(model), id: 'model-b' }] }] }, { id: 'site-b', name: 'Site B', base_url: 'https://b.example.test/v1', keys: [{ id: 'key-c', name: 'Key C', has_secret: true, models: [{ ...structuredClone(model), id: 'model-c', reasoning_efforts: ['medium'] }] }] }], subagent_models: [preset()] });

describe('subagent model pool configuration', () => {
  it('accepts typed old optional configs and defaults missing pool without mutation', () => {
    const { subagent_models: _pool, ...old } = config();
    const compatible: AppConfig = old;
    expect(validateSubagentModels(compatible)).toBe('');
    expect(sanitizeConfig(compatible).subagent_models).toEqual([]);
    expect(compatible.subagent_models).toBeUndefined();
  });
  it('round trips null, empty and named efforts for aliases of the same model', () => {
    const original = { ...config(), extra: { keep: true }, subagent_models: [preset(null, 'default'), preset('', 'omit'), preset('high', 'deep')] };
    const result = sanitizeConfig(original);
    expect(validateSubagentModels(result)).toBe('');
    expect(result.subagent_models).toEqual(original.subagent_models);
    expect(result).toHaveProperty('extra.keep', true);
    expect(JSON.parse(JSON.stringify(result)).subagent_models.map((entry: SubagentModel) => entry.reasoning_effort)).toEqual([null, '', 'high']);
  });
  it('keeps only reference fields in the pool and removes returned secrets', () => {
    const original = config();
    original.sites[0].keys[0].secret = 'returned-key-fixture';
    Object.assign(original.subagent_models![0], { secret: 'pool-secret-fixture', api_key: 'pool-key-fixture' });
    Object.assign(original.subagent_models![0].selection, { secret: 'selection-secret-fixture' });
    const result = sanitizeConfig(original);
    expect(result.subagent_models).toEqual([preset()]);
    expect(JSON.stringify(result)).not.toContain('secret-fixture');
    expect(result.sites[0].keys[0].secret).toBeUndefined();
    expect(original.sites[0].keys[0].secret).toBe('returned-key-fixture');
  });
  it('accepts routes across sites and keys while rejecting mismatched combinations', () => {
    const draft = config();
    for (const selection of [{ site_id: 'site-a', key_id: 'key-b', model_id: 'model-b' }, { site_id: 'site-b', key_id: 'key-c', model_id: 'model-c' }]) {
      draft.subagent_models![0].selection = selection;
      expect(validateSubagentModels(draft)).toBe('');
    }
    draft.subagent_models![0].selection.key_id = 'key-a';
    expect(validateSubagentModels(draft)).toContain('引用的站点、Key 或模型已被删除');
  });
  it.each(['site', 'key', 'model'])('reports deleted %s references and retains the preset', target => {
    const draft = config();
    if (target === 'site') draft.sites.shift();
    if (target === 'key') draft.sites[0].keys.shift();
    if (target === 'model') draft.sites[0].keys[0].models = [];
    const before = structuredClone(draft.subagent_models);
    expect(validateSubagentModels(draft)).toContain('子代理模型池「Preset preset」引用');
    expect(validateSubagentModels(draft)).toContain('「子代理」标签');
    expect(draft.subagent_models).toEqual(before);
    expect(sanitizeConfig(draft).subagent_models).toEqual(before);
  });
  it('requires a saved or newly entered key secret', () => {
    const draft = config(); draft.sites[0].keys[0].has_secret = false;
    expect(validateSubagentModels(draft)).toContain('尚未配置密钥');
    draft.sites[0].keys[0].secret = 'new-fixture-key';
    expect(validateSubagentModels(draft)).toBe('');
  });
  it('rejects stale named efforts without deleting or changing them', () => {
    const draft = config(); draft.subagent_models![0].reasoning_effort = 'medium';
    expect(validateSubagentModels(draft)).toContain('「medium」不受当前模型支持');
    expect(draft.subagent_models![0].reasoning_effort).toBe('medium');
  });
  it('resets effort on any route change, including another key, and preserves unchanged routing', () => {
    const entry = preset('high');
    expect(selectSubagentModel(entry, { ...selection })).toEqual(entry);
    for (const change of [{ ...selection, site_id: 'site-b' }, { ...selection, key_id: 'key-b' }, { ...selection, model_id: 'model-b' }]) expect(selectSubagentModel(entry, change)).toEqual({ ...entry, selection: change, reasoning_effort: null });
    expect(entry.reasoning_effort).toBe('high');
  });
  it('requires nonempty names, unique IDs across all config objects, and at most 32 entries', () => {
    for (const id of ['site-a', 'key-a', 'model-a', 'preset']) {
      const draft = config(); draft.subagent_models!.push(preset(null, id));
      expect(validateSubagentModels(draft)).toContain('ID');
    }
    const draft = config(); draft.subagent_models![0].name = ' ';
    expect(validateSubagentModels(draft)).toContain('需要填写名称');
    draft.subagent_models = Array.from({ length: 33 }, (_, i) => preset(null, `pool-${i}`));
    expect(validateSubagentModels(draft)).toContain('最多允许 32');
    draft.subagent_models.pop(); expect(validateSubagentModels(draft)).toBe('');
  });
});
