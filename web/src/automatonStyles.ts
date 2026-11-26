export type AutomatonMode = 'idle' | 'thinking' | 'tooling' | 'swarm';

export type AnimationStyleId =
  | 'kaleidoscope'
  | 'spiral'
  | 'lissajous'
  | 'scanline'
  | 'wave'
  | 'vortex'
  | 'dna_helix'
  | 'matrix_rain'
  | 'plasma_lattice'
  | 'shockwave'
  | 'fractal_diamond'
  | 'cellular';

export interface LiveWorkload {
  mode: AutomatonMode;
  activeSessions: number;
  activeTools: number;
  activeSubagents: number;
  toolKinds: number;
  energy: number;
}

export const ANIMATION_STYLES: ReadonlyArray<{
  id: AnimationStyleId;
  name: string;
  tag: string;
  desc: string;
}> = [
  { id: 'kaleidoscope', name: '万花筒对称细胞', tag: '12-Fold Cells', desc: '12 阶镜像对称玫瑰线高能细胞矩阵' },
  { id: 'spiral', name: '双生螺旋星云', tag: 'Spiral Cells', desc: '多臂阿基米德螺旋与黄金螺线细胞阵' },
  { id: 'dna_helix', name: '双螺旋基因链', tag: '3D Helix Grid', desc: '三维投影双螺旋碱基对离散细胞块' },
  { id: 'matrix_rain', name: '赛博数字瀑布', tag: 'Matrix Cells', desc: '高速下落余辉脉冲细胞柱列' },
  { id: 'plasma_lattice', name: '量子等离子晶格', tag: 'Plasma Lattice', desc: '多频正弦叠加高饱和等离子细胞阵' },
  { id: 'shockwave', name: '超新星冲击波', tag: 'Shockwave', desc: '同心爆发冲击波环与径向高能射线' },
  { id: 'fractal_diamond', name: '分形菱形曼陀罗', tag: 'XOR Fractal', desc: '曼哈顿与切比雪夫分形对称细胞图腾' },
  { id: 'lissajous', name: '李萨如细胞轨迹', tag: 'Lissajous Grid', desc: '多频谐波李萨如余辉细胞轨迹' },
  { id: 'scanline', name: '扫描线细胞矩阵', tag: 'Radar + Scan', desc: '横纵强扫描光束与旋转雷达细胞阵' },
  { id: 'wave', name: '波纹干涉细胞', tag: 'Wave Field', desc: '多波源相干驻波等值离散细胞块' },
  { id: 'vortex', name: '旋涡细胞流', tag: 'Vortex Cells', desc: '极坐标强涡旋角动量离散细胞簇' },
  { id: 'cellular', name: '经典元胞矩阵', tag: '256-Color CA', desc: '后端实时 256 色离散元胞方块阵' },
];

const THINKING_CYCLE: AnimationStyleId[] = ['spiral', 'dna_helix', 'lissajous', 'kaleidoscope'];
const TOOLING_CYCLE: AnimationStyleId[] = ['scanline', 'matrix_rain', 'wave', 'plasma_lattice'];
const SWARM_CYCLE: AnimationStyleId[] = ['shockwave', 'vortex', 'fractal_diamond', 'kaleidoscope', 'plasma_lattice'];

/** Dynamically chooses a preset from live mode & time (switching ~every 8s). */
export function selectDynamicPreset(workload: LiveWorkload, timeSec: number): AnimationStyleId {
  if (workload.mode === 'idle') return 'kaleidoscope';
  const pool =
    workload.mode === 'swarm' || workload.activeSubagents > 0
      ? SWARM_CYCLE
      : workload.mode === 'tooling' || workload.activeTools > 0
        ? TOOLING_CYCLE
        : THINKING_CYCLE;
  const idx = Math.floor(timeSec / 8) % pool.length;
  return pool[idx];
}

/** Computes workload-driven intensity & speed while keeping cell grid density fixed. */
export function computeWorkloadDynamics(workload: LiveWorkload): { speed: number; intensity: number } {
  if (workload.mode === 'idle') {
    return { speed: 1.0, intensity: 0.0 };
  }
  const load =
    workload.activeSubagents * 0.55 +
    workload.activeTools * 0.32 +
    workload.activeSessions * 0.18 +
    workload.toolKinds * 0.14 +
    workload.energy * 0.45;
  const modeBaseSpeed =
    workload.mode === 'swarm' ? 1.85 : workload.mode === 'tooling' ? 1.55 : 1.3;
  const modeBaseIntensity =
    workload.mode === 'swarm' ? 1.55 : workload.mode === 'tooling' ? 1.38 : 1.22;
  return {
    speed: Math.min(3.4, modeBaseSpeed + load * 0.42),
    intensity: Math.min(1.85, modeBaseIntensity + load * 0.25),
  };
}

function cellColor(theme: 'light' | 'dark', mode: AutomatonMode, phase: number, alpha: number): string {
  const hueBase =
    mode === 'swarm' ? 295 : mode === 'tooling' ? 160 : mode === 'thinking' ? 200 : 192;
  const hue = ((hueBase + phase * 95) % 360 + 360) % 360;
  const sat = theme === 'dark' ? 94 : 78;
  const light = theme === 'dark' ? 62 : 46;
  return `hsla(${hue.toFixed(0)}, ${sat}%, ${light}%, ${Math.max(0, Math.min(1, alpha)).toFixed(3)})`;
}

export function renderDiscreteCellPattern(
  ctx: CanvasRenderingContext2D,
  w: number,
  h: number,
  tSec: number,
  style: AnimationStyleId,
  workload: LiveWorkload,
  theme: 'light' | 'dark',
  backendRgb?: Uint8Array,
  backendCols?: number,
  backendRows?: number,
): void {
  if (workload.mode === 'idle') {
    return;
  }

  const { speed, intensity } = computeWorkloadDynamics(workload);
  const t = tSec * speed;
  const alphaMax = (theme === 'dark' ? 0.64 : 0.38) * intensity;

  // Fixed grid density (pitch=14px, block=11px) to prevent lag regardless of workload.
  const pitch = 14;
  const block = 11;
  const cols = Math.ceil(w / pitch);
  const rows = Math.ceil(h / pitch);
  const cx = w * 0.5;
  const cy = h * 0.5;
  const maxR = Math.hypot(cx, cy);

  if (style === 'cellular') {
    if (backendRgb && backendCols && backendRows && backendRgb.length === backendCols * backendRows * 3) {
      const cellW = w / backendCols;
      const cellH = h / backendRows;
      const gx = Math.max(1, Math.round(cellW * 0.14));
      const gy = Math.max(1, Math.round(cellH * 0.14));
      const scanY = (t * 115) % h;
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
          let a = 0.1 + Math.pow(peak / 255, 0.8) * alphaMax;
          const dist = Math.abs(py - scanY);
          if (dist < 52) a = Math.min(0.96, a * (1 + (1 - dist / 52) * 0.95));
          ctx.fillStyle = `rgba(${red}, ${green}, ${blue}, ${Math.min(1, a).toFixed(3)})`;
          ctx.fillRect(px, py, Math.max(2, Math.floor(cellW) - gx), Math.max(2, Math.floor(cellH) - gy));
        }
      }
    }
    return;
  }

  let lissajousGrid: Float32Array | null = null;
  let lissajousPhase: Float32Array | null = null;
  if (style === 'lissajous') {
    lissajousGrid = new Float32Array(cols * rows);
    lissajousPhase = new Float32Array(cols * rows);
    const curves = 3;
    const samples = 240;
    for (let curve = 0; curve < curves; curve++) {
      const fx = 3 + curve;
      const fy = 2 + ((curve + 1) % 3);
      const delta = t * (0.65 + curve * 0.2) + curve * 1.15;
      for (let s = 0; s < samples; s++) {
        const u = (s / samples) * Math.PI * 2;
        const x = cx + Math.sin(fx * u + delta) * (w * 0.43);
        const y = cy + Math.sin(fy * u) * (h * 0.42);
        const gc = Math.floor(x / pitch);
        const gr = Math.floor(y / pitch);
        if (gc >= 0 && gc < cols && gr >= 0 && gr < rows) {
          const i = gr * cols + gc;
          const pulse = 0.6 + 0.4 * Math.sin(u * 5 - t * 3.5);
          if (pulse > lissajousGrid[i]) {
            lissajousGrid[i] = pulse;
            lissajousPhase[i] = s / samples + curve * 0.3;
          }
        }
      }
    }
  }

  const scanRowY = ((t * 145) % (h + 80)) - 40;
  const scanColX = ((t * 185) % (w + 80)) - 40;
  const radarAng = ((t * 1.45) % (Math.PI * 2)) - Math.PI;

  for (let r = 0; r < rows; r++) {
    const py = r * pitch;
    const dy = py + block * 0.5 - cy;
    const scanDistY = Math.abs(py - scanRowY);

    for (let c = 0; c < cols; c++) {
      const px = c * pitch;
      const dx = px + block * 0.5 - cx;
      let val = 0;
      let phase = 0;

      if (style === 'kaleidoscope') {
        const dist = Math.hypot(dx, dy);
        const normR = dist / Math.max(1, maxR * 0.68);
        const ang = Math.atan2(dy, dx);
        const sector = (Math.PI * 2) / 12;
        const symAng = Math.abs(((ang % sector) + sector) % sector - sector * 0.5);
        const rose = Math.cos(symAng * 6 + t * 1.15) * Math.sin(normR * 11 - t * 2.1);
        const ring = Math.cos(normR * 15 - t * 1.5 + Math.cos(symAng * 12) * 1.6);
        const raw = (rose + ring) * 0.5;
        if (raw > 0.2) {
          val = (raw - 0.2) / 0.8;
          phase = normR * 0.9 + symAng * 2.5 + t * 0.12;
        }
      } else if (style === 'spiral') {
        const dist = Math.hypot(dx, dy);
        const normR = dist / Math.max(1, maxR * 0.72);
        const ang = Math.atan2(dy, dx);
        const s1 = Math.sin(normR * 20 - 5 * ang - t * 2.7);
        const s2 = Math.sin(normR * 12 + 3 * ang + t * 1.4);
        const raw = s1 * 0.72 + s2 * 0.28;
        if (raw > 0.24 && normR < 1.18) {
          val = ((raw - 0.24) / 0.76) * (1 - normR * 0.28);
          phase = normR + (ang / Math.PI) * 0.5 + t * 0.09;
        }
      } else if (style === 'dna_helix') {
        const nx = (px / Math.max(1, w)) * Math.PI * 4 + t * 1.8;
        const y1 = cy + Math.sin(nx) * (h * 0.32);
        const y2 = cy + Math.sin(nx + Math.PI) * (h * 0.32);
        const d1 = Math.abs(py - y1);
        const d2 = Math.abs(py - y2);
        if (d1 < 22) {
          val = 1 - d1 / 22;
          phase = nx * 0.15;
        } else if (d2 < 22) {
          val = 1 - d2 / 22;
          phase = nx * 0.15 + 0.5;
        } else if (c % 3 === 0 && py > Math.min(y1, y2) && py < Math.max(y1, y2)) {
          val = 0.52;
          phase = nx * 0.15 + 0.25;
        }
      } else if (style === 'matrix_rain') {
        const colSeed = ((c * 73 + 19) % 31) / 31;
        const dropSpeed = 12 + colSeed * 18;
        const headRow = (t * dropSpeed + colSeed * rows * 2) % (rows + 14);
        const trail = headRow - r;
        if (trail >= 0 && trail < 14) {
          val = 1 - trail / 14;
          phase = trail < 1.5 ? 0.05 : 0.35 + colSeed * 0.4;
        }
      } else if (style === 'plasma_lattice') {
        const vx = px * 0.018;
        const vy = py * 0.018;
        const p1 = Math.sin(vx + t * 1.5);
        const p2 = Math.sin(vy - t * 1.3);
        const p3 = Math.sin((vx + vy) * 0.8 + t * 1.9);
        const p4 = Math.sin(Math.hypot(dx, dy) * 0.024 - t * 2.2);
        const plasma = (p1 + p2 + p3 + p4) * 0.25;
        if (plasma > 0.16) {
          val = (plasma - 0.16) / 0.84;
          phase = plasma * 1.4 + t * 0.12;
        }
      } else if (style === 'shockwave') {
        const dist = Math.hypot(dx, dy);
        const normR = dist / Math.max(1, maxR * 0.72);
        const pulse = (t * 0.65) % 1.25;
        const ringDist = Math.abs(normR - pulse);
        const ring2Dist = Math.abs(normR - ((pulse + 0.62) % 1.25));
        const ang = Math.atan2(dy, dx);
        const rays = Math.pow(Math.max(0, Math.cos(ang * 8 - t * 1.2)), 4);
        const ringGlow = Math.max(
          ringDist < 0.12 ? 1 - ringDist / 0.12 : 0,
          ring2Dist < 0.1 ? (1 - ring2Dist / 0.1) * 0.8 : 0,
        );
        val = Math.min(1, ringGlow + rays * 0.45 * (1 - normR * 0.5));
        phase = normR + t * 0.15;
      } else if (style === 'fractal_diamond') {
        const manhattan = Math.abs(dx) + Math.abs(dy);
        const cheby = Math.max(Math.abs(dx), Math.abs(dy));
        const band1 = Math.sin(manhattan * 0.032 - t * 2.4);
        const band2 = Math.cos(cheby * 0.042 + t * 1.8);
        const xorBit = ((Math.abs(c - Math.floor(cols / 2)) ^ Math.abs(r - Math.floor(rows / 2))) % 5) === 0 ? 0.35 : 0;
        const raw = (band1 + band2) * 0.42 + xorBit;
        if (raw > 0.24) {
          val = Math.min(1, (raw - 0.24) / 0.76);
          phase = manhattan * 0.002 + t * 0.1;
        }
      } else if (style === 'lissajous' && lissajousGrid && lissajousPhase) {
        const idx = r * cols + c;
        val = lissajousGrid[idx];
        phase = lissajousPhase[idx] + t * 0.1;
      } else if (style === 'scanline') {
        const dist = Math.hypot(dx, dy);
        const ang = Math.atan2(dy, dx);
        const angDiff = Math.abs(Math.atan2(Math.sin(ang - radarAng), Math.cos(ang - radarAng)));
        const radarGlow = angDiff < 0.3 ? 1 - angDiff / 0.3 : 0;
        const hGlow = scanDistY < 50 ? 1 - scanDistY / 50 : 0;
        const vDist = Math.abs(px - scanColX);
        const vGlow = vDist < 42 ? (1 - vDist / 42) * 0.78 : 0;
        const pulse = (c + r) % 2 === 0 ? 1 : 0.52;
        val = Math.max(radarGlow, hGlow, vGlow) * pulse;
        phase = dist / Math.max(1, maxR) + t * 0.08;
      } else if (style === 'wave') {
        const s1x = cx + Math.cos(t * 0.7) * (w * 0.23);
        const s1y = cy + Math.sin(t * 0.55) * (h * 0.21);
        const s2x = cx - Math.cos(t * 0.7) * (w * 0.23);
        const s2y = cy - Math.sin(t * 0.55) * (h * 0.21);
        const d1 = Math.hypot(px - s1x, py - s1y) * 0.058;
        const d2 = Math.hypot(px - s2x, py - s2y) * 0.058;
        const amp = (Math.sin(d1 - t * 3.1) + Math.sin(d2 - t * 3.1)) * 0.5;
        if (amp > 0.22) {
          val = (amp - 0.22) / 0.78;
          phase = amp * 0.8 + t * 0.09;
        }
      } else if (style === 'vortex') {
        const dist = Math.hypot(dx, dy);
        const normR = dist / Math.max(1, maxR * 0.68);
        const ang = Math.atan2(dy, dx);
        const swirl = ang + 4.2 * Math.log(normR + 0.14) - t * 1.75;
        const band = Math.sin(swirl * 5) * Math.cos(normR * 10 - t * 1.6);
        if (band > 0.2 && normR < 1.18) {
          val = ((band - 0.2) / 0.8) * Math.min(1, normR * 2.4 + 0.22);
          phase = normR + t * 0.14;
        }
      }

      if (val > 0.05 && scanDistY < 48) {
        val = Math.min(1, val * (1 + (1 - scanDistY / 48) * 0.78));
      }

      if (val > 0.05) {
        ctx.fillStyle = cellColor(theme, workload.mode, phase, val * alphaMax);
        ctx.fillRect(px, py, block, block);
      }
    }
  }
}
