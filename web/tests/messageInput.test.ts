import { File } from 'node:buffer';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { MAX_IMAGES, MAX_MESSAGE_BYTES, MAX_PASTE_SOURCES, pastedFiles, pastedImages, readImage } from '../src/messageInput';
import { attachmentUrl } from '../src/ImageLightbox';

// Synthetic bytes only; no system clipboard, user files or DOM dependency.
class FixtureReader {
  result: string | null = null;
  onload?: () => void;
  onerror?: () => void;
  onabort?: () => void;
  readAsDataURL(file: Blob) {
    void file.arrayBuffer().then(bytes => {
      this.result = `data:${file.type};base64,${Buffer.from(bytes).toString('base64')}`;
      this.onload?.();
    });
  }
}
const file = (bytes: string, name = 'fixture.png', lastModified = 1) => new File([bytes], name, { type: 'image/png', lastModified }) as unknown as globalThis.File;
const transfer = (items: globalThis.File[], files: globalThis.File[]) => ({ items: items.map(value => ({ kind: 'file', getAsFile: () => value })), files }) as unknown as DataTransfer;
beforeEach(() => vi.stubGlobal('FileReader', FixtureReader));
afterEach(() => vi.unstubAllGlobals());

describe('one-paste actual-byte deduplication', () => {
  it('collapses identical bytes from different items/files names, wrappers and modification times', async () => {
    const otherMimeWrapper = new File(['same pixels'], 'other-type.jpg', { type: 'image/jpeg', lastModified: 42 }) as unknown as globalThis.File;
    const sources = pastedFiles(transfer([file('same pixels', 'item.png', 2)], [file('same pixels', 'another-name.png', 999), otherMimeWrapper]));
    expect(sources).toHaveLength(3);
    expect(await pastedImages(sources)).toEqual([{ mime: 'image/png', data: Buffer.from('same pixels').toString('base64') }]);
  });
  it('preserves different bytes with completely identical metadata', async () => {
    const a = file('pixel red'); const b = file('pixel tan');
    expect(a.size).toBe(b.size); expect(a.lastModified).toBe(b.lastModified); expect(a.name).toBe(b.name);
    expect(await pastedImages(pastedFiles(transfer([a], [b])))).toHaveLength(2);
  });
  it('collapses two same-image wrappers in one items channel, but not two deliberate paste events', async () => {
    const sources = pastedFiles(transfer([file('same'), file('same', 'other.png', 3)], []));
    expect(await pastedImages(sources)).toHaveLength(1);
    expect([...(await pastedImages(sources)), ...(await pastedImages(sources))]).toHaveLength(2);
  });
  it('does not deduplicate normal upload reads', async () => {
    const a = file('same');
    expect(await Promise.all([a, file('same')].map(readImage))).toHaveLength(2);
  });
  it('counts the eight-image limit after duplicate wrappers and against available draft slots', async () => {
    const eight = Array.from({ length: MAX_IMAGES }, (_, n) => file(`pixel-${n}`));
    expect(await pastedImages(pastedFiles(transfer(eight, eight.map((_, n) => file(`pixel-${n}`, `renamed-${n}.png`, n + 20)))))).toHaveLength(8);
    expect(await pastedImages([file('same'), file('same')], 1)).toHaveLength(1);
    await expect(pastedImages([...eight, file('ninth')])).rejects.toThrow('8');
    await expect(pastedImages([file('one'), file('two')], 1)).rejects.toThrow('8');
  });
  it('caps pathological raw clipboard sources before reads with a separate honest error', async () => {
    const wrapper = file('same');
    const sources = pastedFiles(transfer(Array(MAX_PASTE_SOURCES + 20).fill(wrapper), [wrapper]));
    expect(sources).toHaveLength(MAX_PASTE_SOURCES + 1);
    const read = vi.spyOn(FixtureReader.prototype, 'readAsDataURL');
    await expect(pastedImages(sources)).rejects.toThrow('剪贴板图片来源过多');
    expect(read).not.toHaveBeenCalled(); read.mockRestore();
    expect(await pastedImages(Array(MAX_PASTE_SOURCES).fill(wrapper))).toHaveLength(1);
  });
  it('bounds unique encoded contents early without charging duplicate wrappers twice', async () => {
    let n = 0;
    vi.stubGlobal('FileReader', class extends FixtureReader { readAsDataURL() { this.result = `data:image/png;base64,${(n++ < 2 ? 'A' : 'B').repeat(MAX_MESSAGE_BYTES / 2 + 4)}`; queueMicrotask(() => this.onload?.()); } });
    await expect(pastedImages([file('one'), file('same-wrapper')])).resolves.toHaveLength(1);
    n = 0;
    await expect(pastedImages([file('one'), file('same-wrapper'), file('two')])).rejects.toThrow('24 MiB');
  });
  it('clearly reports read errors and aborts without returning a partial batch', async () => {
    vi.stubGlobal('FileReader', class extends FixtureReader { readAsDataURL() { queueMicrotask(() => this.onerror?.()); } });
    await expect(pastedImages([file('fixture')])).rejects.toThrow('图片读取失败');
    vi.stubGlobal('FileReader', class extends FixtureReader { readAsDataURL() { queueMicrotask(() => this.onabort?.()); } });
    await expect(pastedImages([file('fixture')])).rejects.toThrow('图片读取已取消');
    vi.stubGlobal('FileReader', class extends FixtureReader { readAsDataURL() { throw new Error('raw filesystem details must not leak'); } });
    await expect(pastedImages([file('fixture')])).rejects.toThrow('图片读取失败');
  });
});

describe('inline attachment preview allowlist', () => {
  it('allows only base64 raster attachments, never remote URLs, SVG or HTML', () => {
    for (const mime of ['image/png', 'image/jpeg', 'image/webp', 'image/gif']) expect(attachmentUrl({ mime, data: 'YWJj' })).toBe(`data:${mime};base64,YWJj`);
    for (const mime of ['image/svg+xml', 'text/html', 'text/plain']) expect(attachmentUrl({ mime, data: 'YWJj' })).toBeUndefined();
    for (const data of ['https://example.test/image.png', '<script>', 'not base64', '']) expect(attachmentUrl({ mime: 'image/png', data })).toBeUndefined();
  });
});
