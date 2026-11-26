export type AutomatonMode = 'idle' | 'thinking' | 'tooling' | 'swarm';

export type AnimationStyleId =
  | 'kaleidoscope'
  | 'spiral'
  | 'lissajous'
  | 'scanline'
  | 'wave'
  | 'vortex'
  | 'cellular'
  | 'off';

export interface AnimationSettings {
  style: AnimationStyleId;
  speed: number;
  intensity: number;
  density: number;
  scanlines: boolean;
  previewMode: 'live' | AutomatonMode;
}

export const DEFAULT_ANIM_SETTINGS: AnimationSettings = {
  style: 'kaleidoscope',
  speed: 1.0,
  intensity: 1.0,
  density: 1.0,
  scanlines: true,
  previewMode: 'live',
};

const STORAGE_KEY = 'celoom-anim-settings';
export const ANIM_SETTINGS_EVENT = 'celoom:anim-settings';

export const ANIMATION_STYLES: ReadonlyArray<{
  id: AnimationStyleId;
  name: string;
  tag: string;
  desc: string;
}> = [
  { id: 'kaleidoscope', name: '万花筒对称', tag: '12-Fold Mandala', desc: '12 阶旋转镜像对称玫瑰曲线与发光细胞节点' },
  { id: 'spiral', name: '黄金螺旋星云', tag: 'Fibonacci 137.5°', desc: '斐波那契黄金角叶序细胞阵列与对数螺线' },
  { id: 'lissajous', name: '李萨如交响', tag: 'Harmonic Curves', desc: '多频相位锁定李萨如曲线与繁花规轨迹' },
  { id: 'scanline', name: '赛博扫描线', tag: 'Radar + Scanline', desc: '双轴扫描光束、雷达扇区与细胞矩阵脉冲' },
  { id: 'wave', name: '波纹干涉场', tag: 'Wave Interference', desc: '多波源相干驻波等值环与量子化细胞阵列' },
  { id: 'vortex', name: '流场旋涡', tag: 'Curl Streamlines', desc: '极坐标涡旋流线与拖尾发光细胞粒子' },
  { id: 'cellular', name: '经典细胞矩阵', tag: '256-Color CA', desc: '后端实时 256 色元胞自动机状态场' },
  { id: 'off', name: '关闭动画', tag: 'Minimal', desc: '纯净静态背景，不绘制动态效果' },
];

export function loadAnimSettings(): AnimationSettings {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return { ...DEFAULT_ANIM_SETTINGS };
    const parsed = JSON.parse(raw) as Partial<AnimationSettings>;
    const validStyle = ANIMATION_STYLES.some(s => s.id === parsed.style)
      ? (parsed.style as AnimationStyleId)
      : DEFAULT_ANIM_SETTINGS.style;
    const clamp = (v: unknown, min: number, max: number, fallback: number) =>
      typeof v === 'number' && Number.isFinite(v) ? Math.min(max, Math.max(min, v)) : fallback;
    const validPreview = ['live', 'idle', 'thinking', 'tooling', 'swarm'].includes(String(parsed.previewMode))
      ? (parsed.previewMode as AnimationSettings['previewMode'])
      : 'live';
    return {
      style: validStyle,
      speed: clamp(parsed.speed, 0.2, 3.0, DEFAULT_ANIM_SETTINGS.speed),
      intensity: clamp(parsed.intensity, 0.2, 1.8, DEFAULT_ANIM_SETTINGS.intensity),
      density: clamp(parsed.density, 0.5, 2.0, DEFAULT_ANIM_SETTINGS.density),
      scanlines: typeof parsed.scanlines === 'boolean' ? parsed.scanlines : DEFAULT_ANIM_SETTINGS.scanlines,
      previewMode: validPreview,
    };
  } catch {
    return { ...DEFAULT_ANIM_SETTINGS };
  }
}

export function saveAnimSettings(next: AnimationSettings): void {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(next));
  } catch {
    /* Storage may be disabled. */
  }
  window.dispatchEvent(new CustomEvent(ANIM_SETTINGS_EVENT, { detail: next }));
}

function palette(theme: 'light' | 'dark', mode: AutomatonMode, phase: number, alpha: number): string {
  const hueBase =
    mode === 'swarm' ? 295 : mode === 'tooling' ? 165 : mode === 'thinking' ? 205 : theme === 'dark' ? 192 : 345;
  const hue = (hueBase + phase * 80) % 360;
  const sat = theme === 'dark' ? 88 : 72;
  const light = theme === 'dark' ? 62 : 46;
  return `hsla(${hue.toFixed(1)}, ${sat}%, ${light}%, ${Math.max(0, Math.min(1, alpha)).toFixed(3)})`;
}

export function renderVectorOverlay(
  ctx: CanvasRenderingContext2D,
  w: number,
  h: number,
  tSec: number,
  mode: AutomatonMode,
  energy: number,
  theme: 'light' | 'dark',
  settings: AnimationSettings,
): void {
  const style = settings.style;
  if (style === 'off' || style === 'cellular') {
    if (style === 'cellular' && settings.scanlines) {
      drawScanlines(ctx, w, h, tSec, mode, theme, settings.intensity * 0.6);
    }
    return;
  }

  const modeBoost = mode === 'swarm' ? 1.55 : mode === 'tooling' ? 1.35 : mode === 'thinking' ? 1.2 : 1.0;
  const t = tSec * settings.speed * modeBoost;
  const alphaBase = (theme === 'dark' ? 0.42 : 0.24) * settings.intensity * (0.85 + Math.min(0.6, energy * 0.4));
  const cx = w * 0.5;
  const cy = h * 0.5;
  const radius = Math.min(w, h) * 0.44;

  ctx.save();
  ctx.globalCompositeOperation = theme === 'dark' ? 'lighter' : 'source-over';

  if (style === 'kaleidoscope') {
    const folds = Math.max(6, Math.round(12 * settings.density));
    const rings = Math.max(3, Math.round(5 * settings.density));
    for (let rIdx = 1; rIdx <= rings; rIdx++) {
      const rr = (radius * rIdx) / rings;
      const k = 2 + (rIdx % 3);
      ctx.beginPath();
      const steps = 140;
      for (let i = 0; i <= steps; i++) {
        const theta = (i / steps) * Math.PI * 2;
        const mod = 0.72 + 0.28 * Math.cos(k * theta * 2 + t * (rIdx % 2 === 0 ? 0.9 : -0.7));
        const px = cx + Math.cos(theta + t * 0.15) * rr * mod;
        const py = cy + Math.sin(theta + t * 0.15) * rr * mod;
        if (i === 0) ctx.moveTo(px, py);
        else ctx.lineTo(px, py);
      }
      ctx.closePath();
      ctx.strokeStyle = palette(theme, mode, rIdx * 0.22 + t * 0.08, alphaBase * 0.55);
      ctx.lineWidth = 1.2;
      ctx.stroke();
    }
    for (let f = 0; f < folds; f++) {
      const baseAngle = (f / folds) * Math.PI * 2 + t * 0.18;
      for (let s = 1; s <= 4; s++) {
        const frac = s / 4;
        const wave = Math.sin(t * 1.1 + s * 0.9) * 0.22;
        const dist = radius * frac * (0.88 + 0.12 * Math.cos(t * 0.8 + f));
        const ang = baseAngle + wave;
        const x = cx + Math.cos(ang) * dist;
        const y = cy + Math.sin(ang) * dist;
        const cellR = (2.2 + 2.5 * Math.abs(Math.sin(t + s))) * (0.7 + 0.3 * settings.density);
        ctx.beginPath();
        ctx.arc(x, y, cellR, 0, Math.PI * 2);
        ctx.fillStyle = palette(theme, mode, frac + t * 0.1, alphaBase * 0.95);
        ctx.fill();
      }
    }
  } else if (style === 'spiral') {
    const count = Math.round(140 * settings.density);
    const golden = Math.PI * (3 - Math.sqrt(5));
    const arms = 5;
    for (let a = 0; a < arms; a++) {
      ctx.beginPath();
      const armOffset = (a / arms) * Math.PI * 2 + t * 0.25;
      for (let i = 0; i <= 90; i++) {
        const u = i / 90;
        const ang = armOffset + u * Math.PI * 4.2;
        const r = radius * 1.15 * u;
        const px = cx + Math.cos(ang) * r;
        const py = cy + Math.sin(ang) * r;
        if (i === 0) ctx.moveTo(px, py);
        else ctx.lineTo(px, py);
      }
      ctx.strokeStyle = palette(theme, mode, a * 0.18 + t * 0.06, alphaBase * 0.45);
      ctx.lineWidth = 1.1;
      ctx.stroke();
    }
    for (let i = 1; i <= count; i++) {
      const frac = i / count;
      const r = radius * 1.12 * Math.sqrt(frac);
      const theta = i * golden + t * 0.22;
      const px = cx + Math.cos(theta) * r;
      const py = cy + Math.sin(theta) * r;
      const pulse = 0.65 + 0.35 * Math.sin(t * 2.1 - frac * 8);
      ctx.beginPath();
      ctx.arc(px, py, (1.6 + 2.4 * frac) * pulse, 0, Math.PI * 2);
      ctx.fillStyle = palette(theme, mode, frac * 1.3 + t * 0.07, alphaBase * pulse);
      ctx.fill();
    }
  } else if (style === 'lissajous') {
    const curves = Math.max(2, Math.round(3 * settings.density));
    const rx = w * 0.4;
    const ry = h * 0.4;
    for (let c = 0; c < curves; c++) {
      const fx = 3 + c;
      const fy = 2 + ((c + 1) % 3);
      const delta = t * (0.45 + c * 0.14) + c * 1.1;
      ctx.beginPath();
      const steps = 260;
      for (let i = 0; i <= steps; i++) {
        const u = (i / steps) * Math.PI * 2;
        const x = cx + Math.sin(fx * u + delta) * rx * (0.86 + 0.08 * Math.sin(t + c));
        const y = cy + Math.sin(fy * u) * ry * (0.86 + 0.08 * Math.cos(t + c));
        if (i === 0) ctx.moveTo(x, y);
        else ctx.lineTo(x, y);
      }
      ctx.strokeStyle = palette(theme, mode, c * 0.28 + t * 0.06, alphaBase * 0.65);
      ctx.lineWidth = 1.4;
      ctx.stroke();

      const beads = Math.round(14 * settings.density);
      for (let b = 0; b < beads; b++) {
        const u = ((b / beads) + t * 0.08) * Math.PI * 2;
        const x = cx + Math.sin(fx * u + delta) * rx * 0.86;
        const y = cy + Math.sin(fy * u) * ry * 0.86;
        ctx.beginPath();
        ctx.arc(x, y, 2.8, 0, Math.PI * 2);
        ctx.fillStyle = palette(theme, mode, b / beads + c * 0.2, alphaBase);
        ctx.fill();
      }
    }
  } else if (style === 'scanline') {
    // Sweeping radar + horizontal cyber scan beam + cellular grid intersections
    const grid = Math.max(28, Math.round(52 / settings.density));
    const sweepY = ((t * 95) % (h + 120)) - 60;
    const sweepAng = (t * 0.85) % (Math.PI * 2);

    ctx.strokeStyle = palette(theme, mode, 0.1, alphaBase * 0.22);
    ctx.lineWidth = 1;
    for (let x = (w % grid) * 0.5; x < w; x += grid) {
      ctx.beginPath();
      ctx.moveTo(x, 0);
      ctx.lineTo(x, h);
      ctx.stroke();
    }
    for (let y = (h % grid) * 0.5; y < h; y += grid) {
      ctx.beginPath();
      ctx.moveTo(0, y);
      ctx.lineTo(w, y);
      ctx.stroke();
    }

    for (let x = (w % grid) * 0.5; x < w; x += grid) {
      for (let y = (h % grid) * 0.5; y < h; y += grid) {
        const distY = Math.abs(y - sweepY);
        if (distY < 90) {
          const glow = 1 - distY / 90;
          ctx.beginPath();
          ctx.arc(x, y, 1.5 + glow * 3.2, 0, Math.PI * 2);
          ctx.fillStyle = palette(theme, mode, (x + y) / 500, alphaBase * glow);
          ctx.fill();
        }
      }
    }

    const grad = ctx.createLinearGradient(0, sweepY - 50, 0, sweepY + 12);
    grad.addColorStop(0, palette(theme, mode, 0.0, 0));
    grad.addColorStop(0.85, palette(theme, mode, 0.15, alphaBase * 0.75));
    grad.addColorStop(1, palette(theme, mode, 0.3, 0));
    ctx.fillStyle = grad;
    ctx.fillRect(0, sweepY - 50, w, 62);

    // Radar sweep line from center
    ctx.beginPath();
    ctx.moveTo(cx, cy);
    ctx.lineTo(cx + Math.cos(sweepAng) * radius * 1.25, cy + Math.sin(sweepAng) * radius * 1.25);
    ctx.strokeStyle = palette(theme, mode, 0.35, alphaBase * 0.85);
    ctx.lineWidth = 1.6;
    ctx.stroke();
  } else if (style === 'wave') {
    const s1x = cx + Math.cos(t * 0.45) * w * 0.18;
    const s1y = cy + Math.sin(t * 0.35) * h * 0.16;
    const s2x = cx - Math.cos(t * 0.45) * w * 0.18;
    const s2y = cy - Math.sin(t * 0.35) * h * 0.16;
    const rings = Math.round(9 * settings.density);
    for (let i = 0; i < rings; i++) {
      const r = ((i / rings) * radius * 1.35 + (t * 48) % (radius * 1.35 / rings));
      const fade = 1 - r / (radius * 1.45);
      if (fade <= 0) continue;
      for (const [sx, sy, phase] of [[s1x, s1y, 0], [s2x, s2y, 0.35]] as const) {
        ctx.beginPath();
        ctx.arc(sx, sy, r, 0, Math.PI * 2);
        ctx.strokeStyle = palette(theme, mode, phase + i * 0.08, alphaBase * fade * 0.65);
        ctx.lineWidth = 1.2;
        ctx.stroke();
      }
    }
    const cols = Math.round(18 * settings.density);
    const rows = Math.round(11 * settings.density);
    for (let r = 0; r < rows; r++) {
      for (let c = 0; c < cols; c++) {
        const px = ((c + 0.5) / cols) * w;
        const py = ((r + 0.5) / rows) * h;
        const d1 = Math.hypot(px - s1x, py - s1y) * 0.045;
        const d2 = Math.hypot(px - s2x, py - s2y) * 0.045;
        const amp = (Math.sin(d1 - t * 2.4) + Math.sin(d2 - t * 2.4)) * 0.5;
        if (amp > 0.25) {
          ctx.beginPath();
          ctx.arc(px, py, 1.5 + amp * 3.2, 0, Math.PI * 2);
          ctx.fillStyle = palette(theme, mode, amp * 0.5 + t * 0.05, alphaBase * amp);
          ctx.fill();
        }
      }
    }
  } else if (style === 'vortex') {
    const streams = Math.round(56 * settings.density);
    for (let i = 0; i < streams; i++) {
      const frac = i / streams;
      const baseR = radius * (0.18 + 0.95 * ((frac * 1.7) % 1));
      const ang0 = frac * Math.PI * 10 + t * (0.4 + 0.3 * (1 - baseR / radius));
      ctx.beginPath();
      for (let seg = 0; seg <= 10; seg++) {
        const s = seg / 10;
        const a = ang0 - s * 0.55;
        const r = baseR * (1 + 0.08 * Math.sin(a * 3 + t));
        const px = cx + Math.cos(a) * r;
        const py = cy + Math.sin(a) * r;
        if (seg === 0) ctx.moveTo(px, py);
        else ctx.lineTo(px, py);
      }
      ctx.strokeStyle = palette(theme, mode, frac + t * 0.08, alphaBase * 0.72);
      ctx.lineWidth = 1.6;
      ctx.stroke();

      const headX = cx + Math.cos(ang0) * baseR * (1 + 0.08 * Math.sin(ang0 * 3 + t));
      const headY = cy + Math.sin(ang0) * baseR * (1 + 0.08 * Math.sin(ang0 * 3 + t));
      ctx.beginPath();
      ctx.arc(headX, headY, 2.3, 0, Math.PI * 2);
      ctx.fillStyle = palette(theme, mode, frac + 0.25, alphaBase * 0.95);
      ctx.fill();
    }
  }

  if (settings.scanlines) {
    drawScanlines(ctx, w, h, tSec, mode, theme, settings.intensity);
  }
  ctx.restore();
}

function drawScanlines(
  ctx: CanvasRenderingContext2D,
  w: number,
  h: number,
  tSec: number,
  mode: AutomatonMode,
  theme: 'light' | 'dark',
  intensity: number,
): void {
  const lineAlpha = (theme === 'dark' ? 0.09 : 0.04) * Math.min(1.5, intensity);
  ctx.fillStyle = theme === 'dark' ? `rgba(0, 220, 255, ${lineAlpha})` : `rgba(120, 20, 50, ${lineAlpha})`;
  for (let y = 0; y < h; y += 4) {
    ctx.fillRect(0, y, w, 1);
  }
  const beamY = ((tSec * 65) % (h + 80)) - 40;
  const beam = ctx.createLinearGradient(0, beamY - 24, 0, beamY + 24);
  beam.addColorStop(0, palette(theme, mode, 0, 0));
  beam.addColorStop(0.5, palette(theme, mode, 0.2, 0.14 * intensity));
  beam.addColorStop(1, palette(theme, mode, 0, 0));
  ctx.fillStyle = beam;
  ctx.fillRect(0, beamY - 24, w, 48);
}
