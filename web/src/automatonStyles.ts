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
  | 'aurora'
  | 'cyber_orbit'
  | 'starburst'
  | 'heartbeat'
  | 'moire'
  | 'hyperbolic'
  | 'voronoi'
  | 'chladni'
  | 'torus_knot'
  | 'julia_fractal'
  | 'sierpinski'
  | 'magnetic_dipole'
  | 'quasicrystal'
  | 'superformula'
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
  { id: 'aurora', name: '极光磁暴脉冲', tag: 'Aurora Curtain', desc: '高空磁暴极光幕墙与垂直波动细胞束' },
  { id: 'cyber_orbit', name: '量子轨道星环', tag: 'Orbital Rings', desc: '多倾角交叉电子轨道与共振细胞节点' },
  { id: 'starburst', name: '超光速星爆射线', tag: 'Warp Rays', desc: '中心向外高速迸发的星际跃迁射线细胞' },
  { id: 'heartbeat', name: '脉冲心电共振', tag: 'Pulse Wave', desc: '多通道谐振脉冲波峰扫屏细胞阵列' },
  { id: 'lissajous', name: '李萨如细胞轨迹', tag: 'Lissajous Grid', desc: '多频谐波李萨如余辉细胞轨迹' },
  { id: 'scanline', name: '扫描线细胞矩阵', tag: 'Radar + Scan', desc: '横纵强扫描光束与旋转雷达细胞阵' },
  { id: 'wave', name: '波纹干涉细胞', tag: 'Wave Field', desc: '多波源相干驻波等值离散细胞块' },
  { id: 'vortex', name: '旋涡细胞流', tag: 'Vortex Cells', desc: '极坐标强涡旋角动量离散细胞簇' },
  { id: 'moire', name: '莫尔条纹干涉', tag: 'Moiré Beat', desc: '双旋转晶格叠加产生的动态几何莫尔拍频' },
  { id: 'hyperbolic', name: '双曲庞加莱圆盘', tag: 'Poincaré Disk', desc: '非欧几何双曲共形映射与测地线细胞铺砌' },
  { id: 'voronoi', name: '泰森晶界网络', tag: 'Voronoi Edges', desc: '动态多核泰森多边形晶体边界脉冲细胞' },
  { id: 'chladni', name: '克拉尼声学驻波', tag: 'Chladni Nodal', desc: '二维弹性板高阶谐波共振节点几何图形' },
  { id: 'torus_knot', name: '三维环面扭结', tag: 'Torus Knot 3D', desc: '(3,5) 环面扭结三维投影与景深呼吸细胞' },
  { id: 'julia_fractal', name: '动态朱利亚分形', tag: 'Julia Set', desc: '复平面迭代逃逸等势线实时分形细胞阵' },
  { id: 'sierpinski', name: '谢尔宾斯基分形', tag: 'Bitwise Fractal', desc: '位运算自相似分形地毯与递归脉冲网格' },
  { id: 'magnetic_dipole', name: '偶极磁场磁感线', tag: 'Dipole Flux', desc: '双磁极磁通量线与沿线流动的量子细胞' },
  { id: 'quasicrystal', name: '五重彭罗斯准晶', tag: '5-Fold Penrose', desc: '五维平面波叠加的非周期准晶体对称图案' },
  { id: 'superformula', name: '超级公式几何星花', tag: 'Gielis Formula', desc: '广义超椭圆参数方程演化的星形几何细胞' },
  { id: 'cellular', name: '经典元胞矩阵', tag: '256-Color CA', desc: '后端实时 256 色离散元胞方块阵' },
];

const THINKING_CYCLE: AnimationStyleId[] = [
  'spiral', 'dna_helix', 'cyber_orbit', 'lissajous', 'aurora',
  'hyperbolic', 'torus_knot', 'chladni', 'quasicrystal', 'superformula',
];
const TOOLING_CYCLE: AnimationStyleId[] = [
  'scanline', 'matrix_rain', 'wave', 'plasma_lattice', 'heartbeat',
  'moire', 'voronoi', 'sierpinski', 'magnetic_dipole',
];
const SWARM_CYCLE: AnimationStyleId[] = [
  'shockwave', 'vortex', 'fractal_diamond', 'starburst', 'kaleidoscope',
  'julia_fractal', 'quasicrystal', 'superformula', 'torus_knot', 'voronoi',
];

function slotHash(slot: number): number {
  let x = (slot + 1) * 0x9e3779b1;
  x ^= x >>> 16;
  x = Math.imul(x, 0x85ebca6b);
  x ^= x >>> 13;
  return x >>> 0;
}

/**
 * Dynamically picks 1, 2, or 3 presets to blend based on live workload:
 * - When background tools or subagents are active: 30% 3-way blend, 50% 2-way blend, 20% single.
 * - Otherwise (pure thinking): single rotating preset.
 */
export function selectDynamicPresets(workload: LiveWorkload, timeSec: number): AnimationStyleId[] {
  if (workload.mode === 'idle') return ['kaleidoscope'];
  const slot = Math.floor(timeSec / 8);
  const hasParallelWork =
    workload.activeSubagents > 0 ||
    workload.activeTools > 0 ||
    workload.mode === 'tooling' ||
    workload.mode === 'swarm';

  const pool =
    workload.mode === 'swarm' || workload.activeSubagents > 0
      ? SWARM_CYCLE
      : hasParallelWork
        ? TOOLING_CYCLE
        : THINKING_CYCLE;

  const h = slotHash(slot);
  const primary = pool[slot % pool.length];
  if (!hasParallelWork) {
    return [primary];
  }

  const roll = h % 100;
  const count = roll < 30 ? 3 : roll < 80 ? 2 : 1;
  if (count === 1) return [primary];

  const allPool: AnimationStyleId[] = [...SWARM_CYCLE, ...TOOLING_CYCLE, ...THINKING_CYCLE];
  const chosen: AnimationStyleId[] = [primary];
  for (let attempt = 1; chosen.length < count && attempt < 12; attempt++) {
    const candidate = allPool[(h + attempt * 7) % allPool.length];
    if (!chosen.includes(candidate)) {
      chosen.push(candidate);
    }
  }
  return chosen;
}

/** Computes workload-driven intensity & speed (+50% overall speed boost) with fixed density. */
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
    speed: Math.min(5.1, (modeBaseSpeed + load * 0.42) * 1.5),
    intensity: Math.min(1.9, modeBaseIntensity + load * 0.25),
  };
}

function cellColor(theme: 'light' | 'dark', mode: AutomatonMode, phase: number, alpha: number): string {
  const hueBase =
    mode === 'swarm' ? 295 : mode === 'tooling' ? 160 : mode === 'thinking' ? 200 : 192;
  const hue = ((hueBase + phase * 95) % 360 + 360) % 360;
  const sat = theme === 'dark' ? 95 : 80;
  const light = theme === 'dark' ? 62 : 46;
  return `hsla(${hue.toFixed(0)}, ${sat}%, ${light}%, ${Math.max(0, Math.min(1, alpha)).toFixed(3)})`;
}

export function renderDiscreteCellPattern(
  ctx: CanvasRenderingContext2D,
  w: number,
  h: number,
  tSec: number,
  styles: AnimationStyleId[],
  workload: LiveWorkload,
  theme: 'light' | 'dark',
  backendRgb?: Uint8Array,
  backendCols?: number,
  backendRows?: number,
): void {
  if (workload.mode === 'idle' || styles.length === 0) {
    return;
  }

  const { speed, intensity } = computeWorkloadDynamics(workload);
  const t = tSec * speed;
  const alphaMax = (theme === 'dark' ? 0.66 : 0.4) * intensity;

  // Fixed grid density (pitch=14px, block=11px) to prevent lag regardless of blend count or workload.
  const pitch = 14;
  const block = 11;
  const cols = Math.ceil(w / pitch);
  const rows = Math.ceil(h / pitch);
  const cx = w * 0.5;
  const cy = h * 0.5;
  const maxR = Math.hypot(cx, cy);

  if (styles.includes('cellular') && backendRgb && backendCols && backendRows && backendRgb.length === backendCols * backendRows * 3) {
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
    if (styles.length === 1) return;
  }

  let lissajousGrid: Float32Array | null = null;
  let lissajousPhase: Float32Array | null = null;
  if (styles.includes('lissajous')) {
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
  const s1x = cx + Math.cos(t * 0.7) * (w * 0.23);
  const s1y = cy + Math.sin(t * 0.55) * (h * 0.21);
  const s2x = cx - Math.cos(t * 0.7) * (w * 0.23);
  const s2y = cy - Math.sin(t * 0.55) * (h * 0.21);

  // Precompute Voronoi seeds & 3D Torus Knot grid if active.
  let voronoiSeeds: Array<[number, number]> | null = null;
  if (styles.includes('voronoi')) {
    voronoiSeeds = [];
    for (let i = 0; i < 10; i++) {
      const a = i * 2.39996 + t * (0.45 + (i % 3) * 0.18);
      const rad = 0.18 + 0.26 * Math.sin(t * 0.6 + i);
      voronoiSeeds.push([cx + Math.cos(a) * w * rad, cy + Math.sin(a * 1.15) * h * rad]);
    }
  }

  let torusGrid: Float32Array | null = null;
  let torusPhase: Float32Array | null = null;
  if (styles.includes('torus_knot')) {
    torusGrid = new Float32Array(cols * rows);
    torusPhase = new Float32Array(cols * rows);
    const p = 3;
    const q = 5;
    const samples = 260;
    const rotY = t * 0.7;
    const rotX = t * 0.45;
    const cY = Math.cos(rotY);
    const sY = Math.sin(rotY);
    const cX = Math.cos(rotX);
    const sX = Math.sin(rotX);
    const scale = Math.min(w, h) * 0.14;
    for (let s = 0; s < samples; s++) {
      const phi = (s / samples) * Math.PI * 2;
      const rr = 2.0 + Math.cos(q * phi);
      const x0 = rr * Math.cos(p * phi);
      const y0 = rr * Math.sin(p * phi);
      const z0 = -Math.sin(q * phi);
      const x1 = x0 * cY - z0 * sY;
      const z1 = x0 * sY + z0 * cY;
      const y2 = y0 * cX - z1 * sX;
      const z2 = y0 * sX + z1 * cX;
      const persp = 1 / (1 + z2 * 0.12);
      const sx = cx + x1 * scale * persp;
      const sy = cy + y2 * scale * persp;
      const gc = Math.floor(sx / pitch);
      const gr = Math.floor(sy / pitch);
      for (let dr = -1; dr <= 1; dr++) {
        for (let dc = -1; dc <= 1; dc++) {
          const nc = gc + dc;
          const nr = gr + dr;
          if (nc >= 0 && nc < cols && nr >= 0 && nr < rows) {
            const d = Math.abs(dc) + Math.abs(dr);
            const val = (d === 0 ? 1.0 : 0.45) * Math.min(1, Math.max(0.35, persp));
            const idx = nr * cols + nc;
            if (val > torusGrid[idx]) {
              torusGrid[idx] = val;
              torusPhase[idx] = s / samples + z2 * 0.1;
            }
          }
        }
      }
    }
  }

  for (let r = 0; r < rows; r++) {
    const py = r * pitch;
    const dy = py + block * 0.5 - cy;
    const scanDistY = Math.abs(py - scanRowY);

    for (let c = 0; c < cols; c++) {
      const px = c * pitch;
      const dx = px + block * 0.5 - cx;
      const dist = Math.hypot(dx, dy);
      const normR = dist / Math.max(1, maxR * 0.7);
      const ang = Math.atan2(dy, dx);

      let maxVal = 0;
      let sumVal = 0;
      let domPhase = 0;

      for (let sIdx = 0; sIdx < styles.length; sIdx++) {
        const style = styles[sIdx];
        if (style === 'cellular') continue;
        let val = 0;
        let phase = sIdx * 0.33;

        if (style === 'kaleidoscope') {
          const sector = (Math.PI * 2) / 12;
          const symAng = Math.abs(((ang % sector) + sector) % sector - sector * 0.5);
          const rose = Math.cos(symAng * 6 + t * 1.15) * Math.sin(normR * 11 - t * 2.1);
          const ring = Math.cos(normR * 15 - t * 1.5 + Math.cos(symAng * 12) * 1.6);
          const raw = (rose + ring) * 0.5;
          if (raw > 0.2) {
            val = (raw - 0.2) / 0.8;
            phase += normR * 0.9 + symAng * 2.5 + t * 0.12;
          }
        } else if (style === 'spiral') {
          const s1 = Math.sin(normR * 20 - 5 * ang - t * 2.7);
          const s2 = Math.sin(normR * 12 + 3 * ang + t * 1.4);
          const raw = s1 * 0.72 + s2 * 0.28;
          if (raw > 0.24 && normR < 1.18) {
            val = ((raw - 0.24) / 0.76) * (1 - normR * 0.28);
            phase += normR + (ang / Math.PI) * 0.5 + t * 0.09;
          }
        } else if (style === 'dna_helix') {
          const nx = (px / Math.max(1, w)) * Math.PI * 4 + t * 1.8;
          const y1 = cy + Math.sin(nx) * (h * 0.32);
          const y2 = cy + Math.sin(nx + Math.PI) * (h * 0.32);
          const d1 = Math.abs(py - y1);
          const d2 = Math.abs(py - y2);
          if (d1 < 22) {
            val = 1 - d1 / 22;
            phase += nx * 0.15;
          } else if (d2 < 22) {
            val = 1 - d2 / 22;
            phase += nx * 0.15 + 0.5;
          } else if (c % 3 === 0 && py > Math.min(y1, y2) && py < Math.max(y1, y2)) {
            val = 0.52;
            phase += nx * 0.15 + 0.25;
          }
        } else if (style === 'matrix_rain') {
          const colSeed = ((c * 73 + 19) % 31) / 31;
          const dropSpeed = 12 + colSeed * 18;
          const headRow = (t * dropSpeed + colSeed * rows * 2) % (rows + 14);
          const trail = headRow - r;
          if (trail >= 0 && trail < 14) {
            val = 1 - trail / 14;
            phase += trail < 1.5 ? 0.05 : 0.35 + colSeed * 0.4;
          }
        } else if (style === 'plasma_lattice') {
          const vx = px * 0.018;
          const vy = py * 0.018;
          const p1 = Math.sin(vx + t * 1.5);
          const p2 = Math.sin(vy - t * 1.3);
          const p3 = Math.sin((vx + vy) * 0.8 + t * 1.9);
          const p4 = Math.sin(dist * 0.024 - t * 2.2);
          const plasma = (p1 + p2 + p3 + p4) * 0.25;
          if (plasma > 0.16) {
            val = (plasma - 0.16) / 0.84;
            phase += plasma * 1.4 + t * 0.12;
          }
        } else if (style === 'shockwave') {
          const pulse = (t * 0.65) % 1.25;
          const ringDist = Math.abs(normR - pulse);
          const ring2Dist = Math.abs(normR - ((pulse + 0.62) % 1.25));
          const rays = Math.pow(Math.max(0, Math.cos(ang * 8 - t * 1.2)), 4);
          const ringGlow = Math.max(
            ringDist < 0.12 ? 1 - ringDist / 0.12 : 0,
            ring2Dist < 0.1 ? (1 - ring2Dist / 0.1) * 0.8 : 0,
          );
          val = Math.min(1, ringGlow + rays * 0.45 * (1 - normR * 0.5));
          phase += normR + t * 0.15;
        } else if (style === 'fractal_diamond') {
          const manhattan = Math.abs(dx) + Math.abs(dy);
          const cheby = Math.max(Math.abs(dx), Math.abs(dy));
          const band1 = Math.sin(manhattan * 0.032 - t * 2.4);
          const band2 = Math.cos(cheby * 0.042 + t * 1.8);
          const xorBit = ((Math.abs(c - Math.floor(cols / 2)) ^ Math.abs(r - Math.floor(rows / 2))) % 5) === 0 ? 0.35 : 0;
          const raw = (band1 + band2) * 0.42 + xorBit;
          if (raw > 0.24) {
            val = Math.min(1, (raw - 0.24) / 0.76);
            phase += manhattan * 0.002 + t * 0.1;
          }
        } else if (style === 'aurora') {
          const curtain = Math.sin(px * 0.015 + t * 1.2 + Math.sin(py * 0.012 - t * 0.9));
          const verticalFlow = Math.cos(py * 0.02 - t * 2.1 + Math.sin(px * 0.025) * 1.6);
          const raw = curtain * 0.55 + verticalFlow * 0.45;
          if (raw > 0.18) {
            val = (raw - 0.18) / 0.82;
            phase += px * 0.0012 + py * 0.0008 + t * 0.1;
          }
        } else if (style === 'cyber_orbit') {
          for (let orb = 0; orb < 3; orb++) {
            const tilt = orb * 1.05 + t * 0.35 * (orb % 2 === 0 ? 1 : -1);
            const ca = Math.cos(tilt);
            const sa = Math.sin(tilt);
            const rx = dx * ca - dy * sa;
            const ry = (dx * sa + dy * ca) * 2.1;
            const dOrb = Math.abs(Math.hypot(rx, ry) - maxR * (0.28 + orb * 0.16));
            if (dOrb < 18) {
              const g = 1 - dOrb / 18;
              if (g > val) {
                val = g;
                phase += orb * 0.28 + ang * 0.2 + t * 0.1;
              }
            }
          }
        } else if (style === 'starburst') {
          const rayWave = Math.pow(Math.max(0, Math.cos(ang * 12 + Math.sin(normR * 4 - t * 2.0))), 3);
          const streak = Math.sin(normR * 22 - t * 4.2);
          const raw = rayWave * (0.55 + 0.45 * streak);
          if (raw > 0.22) {
            val = Math.min(1, (raw - 0.22) / 0.78);
            phase += normR * 0.9 + t * 0.14;
          }
        } else if (style === 'heartbeat') {
          for (let ch = -1; ch <= 1; ch++) {
            const base = cy + ch * (h * 0.24);
            const phaseX = (px * 0.025 - t * 2.6 + ch * 1.7) % (Math.PI * 2);
            const spike = Math.exp(-Math.pow((phaseX - 1.5) * 3.0, 2)) * (h * 0.22) - Math.exp(-Math.pow((phaseX - 2.1) * 4.5, 2)) * (h * 0.12);
            const waveY = base + Math.sin(px * 0.02 - t * 2.0 + ch) * (h * 0.04) - spike;
            const d = Math.abs(py - waveY);
            if (d < 20) {
              const g = 1 - d / 20;
              if (g > val) {
                val = g;
                phase += (ch + 1) * 0.25 + px * 0.001 + t * 0.1;
              }
            }
          }
        } else if (style === 'lissajous' && lissajousGrid && lissajousPhase) {
          const idx = r * cols + c;
          val = lissajousGrid[idx];
          phase += lissajousPhase[idx] + t * 0.1;
        } else if (style === 'scanline') {
          const angDiff = Math.abs(Math.atan2(Math.sin(ang - radarAng), Math.cos(ang - radarAng)));
          const radarGlow = angDiff < 0.3 ? 1 - angDiff / 0.3 : 0;
          const hGlow = scanDistY < 50 ? 1 - scanDistY / 50 : 0;
          const vDist = Math.abs(px - scanColX);
          const vGlow = vDist < 42 ? (1 - vDist / 42) * 0.78 : 0;
          const pulse = (c + r) % 2 === 0 ? 1 : 0.52;
          val = Math.max(radarGlow, hGlow, vGlow) * pulse;
          phase += normR + t * 0.08;
        } else if (style === 'wave') {
          const d1 = Math.hypot(px - s1x, py - s1y) * 0.058;
          const d2 = Math.hypot(px - s2x, py - s2y) * 0.058;
          const amp = (Math.sin(d1 - t * 3.1) + Math.sin(d2 - t * 3.1)) * 0.5;
          if (amp > 0.22) {
            val = (amp - 0.22) / 0.78;
            phase += amp * 0.8 + t * 0.09;
          }
        } else if (style === 'vortex') {
          const swirl = ang + 4.2 * Math.log(normR + 0.14) - t * 1.75;
          const band = Math.sin(swirl * 5) * Math.cos(normR * 10 - t * 1.6);
          if (band > 0.2 && normR < 1.18) {
            val = ((band - 0.2) / 0.8) * Math.min(1, normR * 2.4 + 0.22);
            phase += normR + t * 0.14;
          }
        } else if (style === 'moire') {
          const a1 = t * 0.32;
          const a2 = -t * 0.27 + 0.25;
          const u1 = dx * Math.cos(a1) - dy * Math.sin(a1);
          const v1 = dx * Math.sin(a1) + dy * Math.cos(a1);
          const u2 = dx * Math.cos(a2) - dy * Math.sin(a2);
          const v2 = dx * Math.sin(a2) + dy * Math.cos(a2);
          const g1 = Math.cos(u1 * 0.095) * Math.cos(v1 * 0.095);
          const g2 = Math.cos(u2 * 0.102) * Math.cos(v2 * 0.102);
          const beat = (g1 + g2) * 0.5;
          if (beat > 0.24) {
            val = (beat - 0.24) / 0.76;
            phase += normR * 0.7 + beat * 0.5 + t * 0.1;
          }
        } else if (style === 'hyperbolic') {
          const hr = Math.min(0.96, dist / Math.max(1, Math.min(cx, cy) * 0.96));
          if (hr < 0.96) {
            const hypDist = Math.log((1 + hr) / (1 - hr));
            const pattern = Math.cos(hypDist * 4.5 - t * 2.2) * Math.cos(ang * 7 + hypDist * 1.5 + t * 0.8);
            if (pattern > 0.18) {
              val = ((pattern - 0.18) / 0.82) * (1 - hr * 0.3);
              phase += hypDist * 0.22 + t * 0.12;
            }
          }
        } else if (style === 'voronoi' && voronoiSeeds) {
          let d1 = 1e9;
          let d2 = 1e9;
          for (let i = 0; i < voronoiSeeds.length; i++) {
            const sd = Math.hypot(px - voronoiSeeds[i][0], py - voronoiSeeds[i][1]);
            if (sd < d1) {
              d2 = d1;
              d1 = sd;
            } else if (sd < d2) {
              d2 = sd;
            }
          }
          const edge = d2 - d1;
          if (edge < 24) {
            val = 1 - edge / 24;
            phase += d1 * 0.004 + t * 0.1;
          }
        } else if (style === 'chladni') {
          const nx = (px / Math.max(1, w)) * 2 - 1;
          const ny = (py / Math.max(1, h)) * 2 - 1;
          const n = 3 + Math.floor((Math.sin(t * 0.35) + 1) * 1.5);
          const m = 4 + Math.floor((Math.cos(t * 0.28) + 1) * 1.5);
          const nodal = Math.abs(
            Math.cos(n * Math.PI * nx) * Math.cos(m * Math.PI * ny) -
            Math.cos(m * Math.PI * nx) * Math.cos(n * Math.PI * ny),
          );
          if (nodal < 0.28) {
            val = 1 - nodal / 0.28;
            phase += (nx * nx + ny * ny) * 0.5 + t * 0.1;
          }
        } else if (style === 'torus_knot' && torusGrid && torusPhase) {
          const idx = r * cols + c;
          val = torusGrid[idx];
          phase += torusPhase[idx] + t * 0.1;
        } else if (style === 'julia_fractal') {
          let zx = (dx / Math.max(1, Math.min(w, h) * 0.42)) * 1.35;
          let zy = (dy / Math.max(1, Math.min(w, h) * 0.42)) * 1.35;
          const jcx = -0.74 + 0.14 * Math.cos(t * 0.65);
          const jcy = 0.22 + 0.14 * Math.sin(t * 0.52);
          let iter = 0;
          for (; iter < 7; iter++) {
            const x2 = zx * zx - zy * zy + jcx;
            const y2 = 2 * zx * zy + jcy;
            zx = x2;
            zy = y2;
            if (zx * zx + zy * zy > 6) break;
          }
          if (iter >= 3 && iter < 7) {
            val = (iter - 2) / 5;
            phase += iter * 0.18 + t * 0.1;
          }
        } else if (style === 'sierpinski') {
          const sc = (c + Math.floor(t * 4)) & 63;
          const sr = (r + Math.floor(t * 2)) & 63;
          if ((sc & sr) === 0) {
            const wave = 0.5 + 0.5 * Math.sin(normR * 10 - t * 2.5);
            val = 0.35 + wave * 0.65;
            phase += (sc + sr) * 0.012 + t * 0.08;
          }
        } else if (style === 'magnetic_dipole') {
          const p1x = cx - w * 0.16;
          const p2x = cx + w * 0.16;
          const d1 = Math.max(12, Math.hypot(px - p1x, py - cy));
          const d2 = Math.max(12, Math.hypot(px - p2x, py - cy));
          const a1 = Math.atan2(py - cy, px - p1x);
          const a2 = Math.atan2(py - cy, px - p2x);
          const flux = Math.sin((a1 - a2) * 6 + t * 2.2) * Math.cos((d1 - d2) * 0.035 - t * 1.8);
          if (flux > 0.25) {
            val = (flux - 0.25) / 0.75;
            phase += (a1 - a2) * 0.3 + t * 0.1;
          }
        } else if (style === 'quasicrystal') {
          const qx = dx * 0.045;
          const qy = dy * 0.045;
          let sum = 0;
          for (let k = 0; k < 5; k++) {
            const ka = (k * Math.PI * 2) / 5;
            sum += Math.cos(qx * Math.cos(ka) + qy * Math.sin(ka) + t * 1.4);
          }
          const qv = sum / 5;
          if (qv > 0.16) {
            val = (qv - 0.16) / 0.84;
            phase += qv + normR * 0.5 + t * 0.1;
          }
        } else if (style === 'superformula') {
          const m = 6 + Math.round((Math.sin(t * 0.4) + 1) * 3);
          const n1 = 0.55;
          const n2 = 1.6 + 0.8 * Math.cos(t * 0.7);
          const n3 = 1.6 + 0.8 * Math.sin(t * 0.7);
          const term1 = Math.pow(Math.abs(Math.cos((m * ang) / 4)), n2);
          const term2 = Math.pow(Math.abs(Math.sin((m * ang) / 4)), n3);
          const superR = Math.pow(term1 + term2, -1 / n1) * 0.55;
          const diff = Math.abs(normR - superR * (0.65 + 0.35 * Math.sin(t * 1.5)));
          const shell = Math.abs((normR % 0.32) - superR * 0.22);
          const d = Math.min(diff, shell);
          if (d < 0.1) {
            val = 1 - d / 0.1;
            phase += normR + ang * 0.2 + t * 0.12;
          }
        }

        if (val > 0.04) {
          sumVal += val;
          if (val > maxVal) {
            maxVal = val;
            domPhase = phase;
          }
        }
      }

      let combined = styles.length > 1 ? Math.min(1, maxVal + (sumVal - maxVal) * 0.52) : maxVal;
      if (combined > 0.05 && scanDistY < 48) {
        combined = Math.min(1, combined * (1 + (1 - scanDistY / 48) * 0.78));
      }

      if (combined > 0.05) {
        ctx.fillStyle = cellColor(theme, workload.mode, domPhase, combined * alphaMax);
        ctx.fillRect(px, py, block, block);
      }
    }
  }
}
