import geminiLogo from './assets/logos/gemini.svg';
import deepseekLogo from './assets/logos/deepseek.svg';
import glmLogo from './assets/logos/glm.svg';
import gptLogo from './assets/logos/gpt.svg';
import claudeLogo from './assets/logos/claude.svg';
import qwenLogo from './assets/logos/qwen.svg';
import grokLogo from './assets/logos/grok.svg';

export type ModelBrand = 'gemini' | 'deepseek' | 'glm' | 'gpt' | 'claude' | 'qwen' | 'grok';

const BRAND_LOGOS: Record<ModelBrand, string> = {
  gemini: geminiLogo,
  deepseek: deepseekLogo,
  glm: glmLogo,
  gpt: gptLogo,
  claude: claudeLogo,
  qwen: qwenLogo,
  grok: grokLogo,
};

export function detectModelBrand(...names: (string | null | undefined)[]): ModelBrand | null {
  const combined = names.filter((n): n is string => typeof n === 'string' && n.trim().length > 0).join(' ').toLowerCase();
  if (!combined) return null;
  if (/\bgemini\b|gemini[-_/0-9]/i.test(combined)) return 'gemini';
  if (/\bdeepseek\b|deepseek[-_/0-9]/i.test(combined)) return 'deepseek';
  if (/\b(chat)?glm\b|glm[-_/0-9]/i.test(combined)) return 'glm';
  if (/\b(gpt|chatgpt)\b|gpt[-_/0-9]|\bo[134](-mini|-pro)?\b/i.test(combined)) return 'gpt';
  if (/\bclaude\b|claude[-_/0-9]/i.test(combined)) return 'claude';
  if (/\bqwen\b|qwen[-_/0-9]/i.test(combined)) return 'qwen';
  if (/\bgrok\b|grok[-_/0-9]/i.test(combined)) return 'grok';
  return null;
}

export function ModelAvatar({ names, size = 16 }: { names: (string | null | undefined)[]; size?: number }) {
  const brand = detectModelBrand(...names);
  if (!brand) return null;
  return (
    <span className={`assistant-avatar brand-${brand}`} aria-hidden="true">
      <img src={BRAND_LOGOS[brand]} alt="" width={size} height={size} draggable={false} />
    </span>
  );
}
