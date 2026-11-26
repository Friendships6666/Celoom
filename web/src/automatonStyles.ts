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
  { id: 'kaleidoscope', name: '万花筒对称细胞', tag: '12-Fold Cells', desc: '12 阶镜像对称玫瑰线激活的离散细胞方块矩阵' },
  { id: 'spiral', name: '螺旋线细胞阵', tag: 'Spiral Cells', desc: '多臂阿基米德螺旋与黄金螺线点亮的离散细胞块' },
  { id: 'lissajous', name: '李萨如细胞轨迹', tag: 'Lissajous Grid', desc: '谐波参数轨迹投射到离散网格的余辉细胞块' },
  { id: 'scanline', name: '扫描线细胞矩阵', tag: 'Scanline Cells', desc: '横纵扫描线与旋转雷达扫过的离散脉冲细胞块' },
  { id: 'wave', name: '波纹干涉细胞', tag: 'Wave Cells', desc: '双波源相干驻波等值区点亮的离散细胞方块' },
  { id: 'vortex', name: '旋涡细胞流', tag: 'Vortex Cells', desc: '极坐标涡旋角动量场驱动的离散细胞簇' },
  { id: 'cellular', name: '经典细胞矩阵', tag: '256-Color CA', desc: '后端实时 256 色离散元胞方块阵（无平滑模糊）' },
  { id: 'off', name: '关闭动画', tag: 'Minimal', desc: '始终保持纯净静态背景' },
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

function cellColor(theme: 'light' | 'dark', mode: AutomatonMode, phase: number, alpha: number): string {
  const hueBase =
    mode === 'swarm' ? 295 : mode === 'tooling' ? 162 : mode === 'thinking' ? 202 : theme === 'dark' ? 192 : 345;
  const hue = ((hueBase + phase * 85) % 360 + 360) % 360;
  const sat = theme === 'dark' ? 90 : 74;
  const light = theme === 'dark' ? 60 : 45;
  return `hsla(${hue.toFixed(0)}, ${sat}%, ${light}%, ${Math.max(0, Math.min(1, alpha)).toFixed(3)})`;
}

export function renderDiscreteCellPattern(
  ctx: CanvasRenderingContext2D,
  w: number,
  h: number,
  tSec: number,
  mode: AutomatonMode,
  energy: number,
  theme: 'light' | 'dark',
  settings: AnimationSettings,
  backendRgb?: Uint8Array,
  backendCols?: number,
  backendRows?: number,
): void {
  if (settings.style === 'off' || mode === 'idle') {
    return;
  }

  const modeBoost = mode === 'swarm' ? 1.5 : mode === 'tooling' ? 1.3 : 1.1;
  const t = tSec * settings.speed * modeBoost;
  const alphaMax = (theme === 'dark' ? 0.58 : 0.34) * settings.intensity * (0.85 + Math.min(0.5, energy * 0.4));

  // Crisp discrete cell pitch & block size with visible gap between blocks.
  const pitch = Math.max(8, Math.round(16 / settings.density));
  const gap = pitch >= 12 ? 3 : 2;
  const block = Math.max(4, pitch - gap);
  const cols = Math.ceil(w / pitch);
  const rows = Math.ceil(h / pitch);
  const cx = w * 0.5;
  const cy = h * 0.5;
  const maxR = Math.hypot(cx, cy);

  if (settings.style === 'cellular') {
    if (backendRgb && backendCols && backendRows && backendRgb.length === backendCols * backendRows * 3) {
      const cellW = w / backendCols;
      const cellH = h / backendRows;
      const gx = Math.max(1, Math.round(cellW * 0.14));
      const gy = Math.max(1, Math.round(cellH * 0.14));
      const scanY = settings.scanlines ? ((t * 90) % h) : -999;
      for (let r = 0; r < backendRows; r++) {
        for (let c = 0; c < backendCols; c++) {
          const idx = (r * backendCols + c) * 3;
          const red = backendRgb[idx];
          const green = backendRgb[idx + 1];
          const blue = backendRgb[idx + 2];
          const peak = Math.max(red, green, blue);
          if (peak <= 8) continue;
          const px = Math.round(c * cellW);
          const py = Math.round(r * cellH);
          let a = (0.08 + Math.pow(peak / 255, 0.85) * alphaMax);
          if (settings.scanlines) {
            const dist = Math.abs(py - scanY);
            if (dist < 48) a = Math.min(0.95, a * (1 + (1 - dist / 48) * 0.9));
          }
          ctx.fillStyle = `rgba(${red}, ${green}, ${blue}, ${Math.min(1, a).toFixed(3)})`;
          ctx.fillRect(px, py, Math.max(2, Math.floor(cellW) - gx), Math.max(2, Math.floor(cellH) - gy));
        }
      }
    }
    return;
  }

  // Precompute Lissajous discrete cell map if needed.
  let lissajousGrid: Float32Array | null = null;
  let lissajousPhase: Float32Array | null = null;
  if (settings.style === 'lissajous') {
    lissajousGrid = new Float32Array(cols * rows);
    lissajousPhase = new Float32Array(cols * rows);
    const curves = 2;
    const samples = 220;
    for (let curve = 0; curve < curves; curve++) {
      const fx = 3 + curve;
      const fy = 2 + curve;
      const delta = t * (0.55 + curve * 0.18) + curve * 1.2;
      for (let s = 0; s < samples; s++) {
        const u = (s / samples) * Math.PI * 2;
        const x = cx + Math.sin(fx * u + delta) * (w * 0.42);
        const y = cy + Math.sin(fy * u) * (h * 0.41);
        const gc = Math.floor(x / pitch);
        const gr = Math.floor(y / pitch);
        if (gc >= 0 && gc < cols && gr >= 0 && gr < rows) {
          const i = gr * cols + gc;
          const pulse = 0.55 + 0.45 * Math.sin(u * 4 - t * 3.0);
          if (pulse > lissajousGrid[i]) {
            lissajousGrid[i] = pulse;
            lissajousPhase[i] = s / samples + curve * 0.35;
          }
        }
      }
    }
  }

  const scanRowY = ((t * 110) % (h + 80)) - 40;
  const scanColX = ((t * 145) % (w + 80)) - 40;
  const radarAng = ((t * 1.1) % (Math.PI * 2)) - Math.PI;

  for (let r = 0; r < rows; r++) {
    const py = r * pitch;
    const dy = py + block * 0.5 - cy;
    const scanDistY = Math.abs(py - scanRowY);

    for (let c = 0; c < cols; c++) {
      const px = c * pitch;
      const dx = px + block * 0.5 - cx;
      let val = 0;
      let phase = 0;

      if (settings.style === 'kaleidoscope') {
        const dist = Math.hypot(dx, dy);
        const normR = dist / Math.max(1, maxR * 0.68);
        const ang = Math.atan2(dy, dx);
        const sector = (Math.PI * 2) / 12;
        const symAng = Math.abs(((ang % sector) + sector) % sector - sector * 0.5);
        const rose = Math.cos(symAng * 6 + t * 0.9) * Math.sin(normR * 10 - t * 1.6);
        const ring = Math.cos(normR * 14 - t * 1.2 + Math.cos(symAng * 12) * 1.4);
        const raw = (rose + ring) * 0.5;
        if (raw > 0.28) {
          val = (raw - 0.28) / 0.72;
          phase = normR * 0.8 + symAng * 2.2 + t * 0.08;
        }
      } else if (settings.style === 'spiral') {
        const dist = Math.hypot(dx, dy);
        const normR = dist / Math.max(1, maxR * 0.72);
        const ang = Math.atan2(dy, dx);
        const arms = 4;
        const s1 = Math.sin(normR * 18 - arms * ang - t * 2.2);
        const s2 = Math.sin(normR * 10 + 3 * ang + t * 1.1);
        const raw = s1 * 0.7 + s2 * 0.3;
        if (raw > 0.34 && normR < 1.15) {
          val = ((raw - 0.34) / 0.66) * (1 - normR * 0.35);
          phase = normR * 0.9 + (ang / Math.PI) * 0.5 + t * 0.06;
        }
      } else if (settings.style === 'lissajous' && lissajousGrid && lissajousPhase) {
        const idx = r * cols + c;
        val = lissajousGrid[idx];
        phase = lissajousPhase[idx] + t * 0.08;
      } else if (settings.style === 'scanline') {
        const dist = Math.hypot(dx, dy);
        const ang = Math.atan2(dy, dx);
        const angDiff = Math.abs(Math.atan2(Math.sin(ang - radarAng), Math.cos(ang - radarAng)));
        const radarGlow = angDiff < 0.26 ? 1 - angDiff / 0.26 : 0;
        const hGlow = scanDistY < 44 ? 1 - scanDistY / 44 : 0;
        const vDist = Math.abs(px - scanColX);
        const vGlow = vDist < 36 ? (1 - vDist / 36) * 0.7 : 0;
        const pulse = ((c + r) % 3 === 0 || (c % 4 === 0 && r % 2 === 0)) ? 1 : 0.45;
        val = Math.max(radarGlow, hGlow, vGlow) * pulse;
        phase = dist / Math.max(1, maxR) + t * 0.05;
      } else if (settings.style === 'wave') {
        const s1x = cx + Math.cos(t * 0.6) * (w * 0.22);
        const s1y = cy + Math.sin(t * 0.45) * (h * 0.2);
        const s2x = cx - Math.cos(t * 0.6) * (w * 0.22);
        const s2y = cy - Math.sin(t * 0.45) * (h * 0.2);
        const d1 = Math.hypot(px - s1x, py - s1y) * 0.055;
        const d2 = Math.hypot(px - s2x, py - s2y) * 0.055;
        const amp = (Math.sin(d1 - t * 2.8) + Math.sin(d2 - t * 2.8)) * 0.5;
        if (amp > 0.3) {
          val = (amp - 0.3) / 0.7;
          phase = amp * 0.7 + t * 0.06;
        }
      } else if (settings.style === 'vortex') {
        const dist = Math.hypot(dx, dy);
        const normR = dist / Math.max(1, maxR * 0.68);
        const ang = Math.atan2(dy, dx);
        const swirl = ang + 3.8 * Math.log(normR + 0.15) - t * 1.4;
        const band = Math.sin(swirl * 5) * Math.cos(normR * 9 - t * 1.3);
        if (band > 0.28 && normR < 1.15) {
          val = ((band - 0.28) / 0.72) * Math.min(1, normR * 2.2 + 0.2);
          phase = normR + t * 0.1;
        }
      }

      if (settings.scanlines && val > 0.05 && scanDistY < 46) {
        val = Math.min(1, val * (1 + (1 - scanDistY / 46) * 0.75));
      }

      if (val > 0.06) {
        ctx.fillStyle = cellColor(theme, mode, phase, val * alphaMax);
        ctx.fillRect(px, py, block, block);
      }
    }
  }
}
