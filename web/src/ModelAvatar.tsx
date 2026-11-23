import geminiLogo from './assets/logos/gemini.svg';
import geminiRaw from './assets/logos/gemini.svg?raw';
import deepseekLogo from './assets/logos/deepseek.svg';
import deepseekRaw from './assets/logos/deepseek.svg?raw';
import glmLogo from './assets/logos/glm.svg';
import glmRaw from './assets/logos/glm.svg?raw';
import gptLogo from './assets/logos/gpt.svg';
import gptRaw from './assets/logos/gpt.svg?raw';
import claudeLogo from './assets/logos/claude.svg';
import claudeRaw from './assets/logos/claude.svg?raw';
import qwenLogo from './assets/logos/qwen.svg';
import qwenRaw from './assets/logos/qwen.svg?raw';
import grokLogo from './assets/logos/grok.svg';
import grokRaw from './assets/logos/grok.svg?raw';

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

const BRAND_RAW: Record<ModelBrand, string> = {
  gemini: geminiRaw,
  deepseek: deepseekRaw,
  glm: glmRaw,
  gpt: gptRaw,
  claude: claudeRaw,
  qwen: qwenRaw,
  grok: grokRaw,
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

function extractSvgColors(raw: string): { primary: string; secondary: string; tertiary: string } {
  const matches = [...raw.matchAll(/(?:fill|stop-color)=["'](#[0-9a-fA-F]{6})["']/gi)]
    .map(m => m[1].toLowerCase())
    .filter(hex => hex !== '#000000');
  const unique = [...new Set(matches)];
  if (unique.length >= 3) {
    return { primary: unique[0], secondary: unique[1], tertiary: unique[2] };
  }
  if (unique.length === 2) {
    return { primary: unique[0], secondary: unique[1], tertiary: unique[0] };
  }
  if (unique.length === 1) {
    const c = unique[0];
    return { primary: c, secondary: `color-mix(in srgb, ${c} 55%, #ffffff)`, tertiary: c };
  }
  // Monochrome currentColor logos (e.g., ChatGPT / Grok): crisp white + silver shimmer.
  return { primary: '#ffffff', secondary: '#cbd5e1', tertiary: '#f8fafc' };
}

export function brandAccent(brand: ModelBrand | null): { primary: string; secondary: string; tertiary: string } {
  if (!brand) return { primary: '#9f1d35', secondary: '#06b6d4', tertiary: '#9f1d35' };
  return extractSvgColors(BRAND_RAW[brand]);
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
