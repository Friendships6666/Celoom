import { modelOptions, sameSelection, type AppConfig, type Selection, type SubagentModel } from './types';

export const poolLimit = 32;
export function selectSubagentModel(entry: SubagentModel, selection: Selection): SubagentModel {
  return { ...entry, selection, reasoning_effort: sameSelection(entry.selection, selection) ? entry.reasoning_effort : null };
}

// Validation reports references without mutating the user's draft or deleting presets.
export function validateSubagentModels(config: AppConfig): string {
  const pool = config.subagent_models ?? [];
  const issue = (name: string, message: string) => `子代理模型池「${name}」${message}，请前往「子代理」标签修复。`;
  if (pool.length > poolLimit) return `子代理模型池最多允许 ${poolLimit} 个预设，请前往「子代理」标签调整。`;
  const ids = new Set(config.sites.flatMap(site => [site.id, ...site.keys.flatMap(key => [key.id, ...key.models.map(model => model.id)])]));
  const options = modelOptions(config);
  for (const [index, entry] of pool.entries()) {
    const name = entry.name.trim() || `预设 ${index + 1}`;
    if (!entry.id.trim() || ids.has(entry.id)) return issue(name, '的 ID 为空或与现有配置重复');
    ids.add(entry.id);
    if (!entry.name.trim()) return issue(name, '需要填写名称');
    const option = options.find(option => sameSelection(option.selection, entry.selection));
    if (!option) return issue(name, '引用的站点、Key 或模型已被删除');
    if (!option.key.has_secret && !option.key.secret?.trim()) return issue(name, '引用的 Key 尚未配置密钥');
    if (entry.reasoning_effort !== null && entry.reasoning_effort !== '' && !option.model.reasoning_efforts.includes(entry.reasoning_effort)) return issue(name, `的思考等级「${entry.reasoning_effort}」不受当前模型支持`);
  }
  return '';
}
