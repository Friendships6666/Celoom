import type { ImageInput, MessageInput } from './types';

export const imageMimes = ['image/png', 'image/jpeg', 'image/webp', 'image/gif'];
export const MAX_IMAGES = 8;
export const MAX_IMAGE_BYTES = 10 * 1024 * 1024;
export const MAX_MESSAGE_BYTES = 24 * 1024 * 1024;
export const MAX_PASTE_SOURCES = 128;
export function validateMessage(input: MessageInput) {
  const images = input.images ?? [];
  if (!input.text.trim() && !images.length) throw new Error('请输入文字或添加图片。');
  if (images.length > MAX_IMAGES) throw new Error('每条消息最多 8 张图片。');
  for (const image of images) {
    if (!imageMimes.includes(image.mime) || !image.data || !/^[A-Za-z0-9+/]*={0,2}$/.test(image.data) || image.data.length % 4 !== 0) throw new Error('图片格式无效，请重新选择 PNG、JPEG、WebP 或 GIF。');
    const bytes = image.data.length / 4 * 3 - (image.data.endsWith('==') ? 2 : image.data.endsWith('=') ? 1 : 0);
    if (bytes > MAX_IMAGE_BYTES) throw new Error('每张图片限 10 MiB。');
  }
  if (new TextEncoder().encode(JSON.stringify(input)).byteLength > MAX_MESSAGE_BYTES) throw new Error('文字和图片编码后的请求总量限 24 MiB，请减少附件。');
}
// Snapshot both clipboard channels before the paste event returns. Neither File
// identity nor metadata reliably identifies their overlap; compare actual bytes.
export function pastedFiles(data: DataTransfer): File[] {
  const sources: File[] = [];
  for (const item of data.items ?? []) {
    if (item.kind !== 'file') continue;
    const file = item.getAsFile();
    if (file) sources.push(file);
    if (sources.length > MAX_PASTE_SOURCES) return sources;
  }
  for (const file of data.files ?? []) {
    sources.push(file);
    if (sources.length > MAX_PASTE_SOURCES) break;
  }
  return sources;
}
// Only this paste batch is deduplicated. Uploads and previous drafts never enter
// this set. Base64 is an exact byte encoding, not a metadata or perceptual hash.
export async function pastedImages(files: File[], available = MAX_IMAGES): Promise<ImageInput[]> {
  if (files.length > MAX_PASTE_SOURCES) throw new Error('剪贴板图片来源过多，请分批粘贴或使用上传。');
  const seen = new Set<string>();
  const batch: ImageInput[] = [];
  let encodedBytes = 0;
  // Sequential reads bound temporary memory even when both clipboard channels
  // wrap many large images. Reject on the first excess *unique* attachment.
  for (const file of files) {
    const image = await readImage(file);
    if (seen.has(image.data)) continue;
    seen.add(image.data);
    if (batch.length >= available) throw new Error('每条消息最多 8 张图片。');
    encodedBytes += image.data.length;
    if (encodedBytes > MAX_MESSAGE_BYTES) throw new Error('文字和图片编码后的请求总量限 24 MiB，请减少附件。');
    batch.push(image);
  }
  return batch;
}
export function readImage(file: File): Promise<ImageInput> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve({ mime: file.type, data: String(reader.result).split(',')[1] ?? '' });
    reader.onerror = () => reject(new Error('图片读取失败，可能暂时离线或文件不可用，请重新选择。'));
    reader.onabort = () => reject(new Error('图片读取已取消，请重新选择。'));
    try { reader.readAsDataURL(file); }
    catch { reject(new Error('图片读取失败，文件不可用，请重新选择。')); }
  });
}
