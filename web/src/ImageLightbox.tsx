import { useState } from 'react';
import { ChevronLeft, ChevronRight } from 'lucide-react';
import type { ImageInput } from './types';
import { imageMimes } from './messageInput';
import { Modal } from './ui';

// Only inline raster attachments. No remote URL, HTML/SVG, fetch or new tab.
export function attachmentUrl(image: ImageInput) {
  return image && imageMimes.includes(image.mime) && typeof image.data === 'string' && image.data && image.data.length % 4 === 0 && /^[A-Za-z0-9+/]*={0,2}$/.test(image.data)
    ? `data:${image.mime};base64,${image.data}` : undefined;
}
export function ImageLightbox({ images, initialIndex, onClose }: { images: ImageInput[]; initialIndex: number; onClose: () => void }) {
  const [index, setIndex] = useState(initialIndex);
  const move = (step: number) => setIndex(current => (current + step + images.length) % images.length);
  return <div onKeyDown={event => {
    if (event.nativeEvent.isComposing || event.keyCode === 229) return;
    if (images.length > 1 && (event.key === 'ArrowLeft' || event.key === 'ArrowRight')) {
      event.preventDefault(); event.stopPropagation(); move(event.key === 'ArrowLeft' ? -1 : 1);
    }
  }}><Modal title="图片预览" subtitle="仅查看本条消息的图片附件" onClose={onClose} className="image-lightbox">
    <div className="lightbox-stage"><img src={attachmentUrl(images[index])} alt={`预览图片 ${index + 1}`} /></div>
    <footer className="lightbox-toolbar"><span className="lightbox-shortcut">Esc 关闭</span><div className="lightbox-navigation">
      <button type="button" className="icon-button" aria-label="上一张图片" disabled={images.length < 2} onClick={() => move(-1)}><ChevronLeft size={20} /></button>
      <span className="lightbox-count" role="status" aria-live="polite">{index + 1} / {images.length}</span>
      <button type="button" className="icon-button" aria-label="下一张图片" disabled={images.length < 2} onClick={() => move(1)}><ChevronRight size={20} /></button>
    </div><span className="lightbox-shortcut">← / → 切换</span></footer>
  </Modal></div>;
}
