import { describe, expect, it } from 'vitest';
import { detectModelBrand } from '../src/ModelAvatar';

describe('detectModelBrand', () => {
  it('returns null by default when no known model keyword matches', () => {
    expect(detectModelBrand()).toBeNull();
    expect(detectModelBrand('')).toBeNull();
    expect(detectModelBrand(null, 'custom-local-model')).toBeNull();
  });

  it('matches brand logo from model name words', () => {
    expect(detectModelBrand('gemini-2.5-pro')).toBe('gemini');
    expect(detectModelBrand('DeepSeek-V3')).toBe('deepseek');
    expect(detectModelBrand('glm-4.5-air')).toBe('glm');
    expect(detectModelBrand('gpt-5.1-codex')).toBe('gpt');
    expect(detectModelBrand('claude-sonnet-4')).toBe('claude');
    expect(detectModelBrand('qwen2.5-72b')).toBe('qwen');
    expect(detectModelBrand('grok-3')).toBe('grok');
  });
});
