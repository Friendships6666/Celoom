import { useEffect, useRef } from 'react';

interface AutomatonFrame {
  cols: number;
  rows: number;
  tick: number;
  mode: 'idle' | 'thinking' | 'tooling' | 'swarm';
  active_sessions: number;
  active_tools: number;
  active_subagents: number;
  alive_cells: number;
  energy: number;
  cells: string;
}

interface Palette {
  grid: string;
  cell1: string;
  cell2: string;
  cell3: string;
  glow1: string;
  glow2: string;
  glow3: string;
  trail: [string, string, string, string];
}

const DARK_PALETTE: Palette = {
  grid: 'rgba(86, 223, 223, 0.055)',
  cell1: 'rgba(86, 223, 223, 0.42)',
  cell2: 'rgba(56, 189, 248, 0.58)',
  cell3: 'rgba(192, 132, 252, 0.64)',
  glow1: 'rgba(86, 223, 223, 0.24)',
  glow2: 'rgba(56, 189, 248, 0.36)',
  glow3: 'rgba(192, 132, 252, 0.42)',
  trail: [
    'rgba(86, 223, 223, 0.20)',
    'rgba(86, 223, 223, 0.12)',
    'rgba(86, 223, 223, 0.065)',
    'rgba(86, 223, 223, 0.025)',
  ],
};

const LIGHT_PALETTE: Palette = {
  grid: 'rgba(136, 39, 63, 0.055)',
  cell1: 'rgba(136, 39, 63, 0.26)',
  cell2: 'rgba(196, 69, 105, 0.38)',
  cell3: 'rgba(184, 123, 40, 0.40)',
  glow1: 'rgba(136, 39, 63, 0.16)',
  glow2: 'rgba(196, 69, 105, 0.24)',
  glow3: 'rgba(184, 123, 40, 0.26)',
  trail: [
    'rgba(136, 39, 63, 0.13)',
    'rgba(136, 39, 63, 0.08)',
    'rgba(136, 39, 63, 0.045)',
    'rgba(136, 39, 63, 0.02)',
  ],
};

export function AutomatonBg({ theme, authenticated }: { theme: 'light' | 'dark'; authenticated: boolean }) {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const frameRef = useRef<AutomatonFrame | null>(null);
  const themeRef = useRef(theme);
  themeRef.current = theme;

  useEffect(() => {
    if (!authenticated) {
      frameRef.current = null;
      return;
    }
    const source = new EventSource('/api/automaton/stream', { withCredentials: true });
    source.addEventListener('frame', raw => {
      try {
        const parsed = JSON.parse((raw as MessageEvent<string>).data) as AutomatonFrame;
        if (parsed && typeof parsed.cols === 'number' && typeof parsed.rows === 'number' && typeof parsed.cells === 'string') {
          frameRef.current = parsed;
        }
      } catch {
        /* Ignore malformed frame. */
      }
    });
    return () => {
      source.close();
    };
  }, [authenticated]);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const ctx = canvas.getContext('2d');
    if (!ctx) return;

    const reducedMotion = window.matchMedia?.('(prefers-reduced-motion: reduce)').matches ?? false;
    let rafId = 0;
    let width = 0;
    let height = 0;
    let dpr = 1;

    const resize = () => {
      dpr = Math.min(window.devicePixelRatio || 1, 2);
      width = window.innerWidth;
      height = window.innerHeight;
      canvas.width = Math.max(1, Math.floor(width * dpr));
      canvas.height = Math.max(1, Math.floor(height * dpr));
    };
    resize();
    window.addEventListener('resize', resize);

    const draw = (now: number) => {
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
      ctx.clearRect(0, 0, width, height);

      const palette = themeRef.current === 'dark' ? DARK_PALETTE : LIGHT_PALETTE;
      const frame = frameRef.current;
      const cols = frame?.cols ?? 44;
      const rows = frame?.rows ?? 26;
      const cellW = width / cols;
      const cellH = height / rows;
      const energy = frame?.energy ?? 0.14;
      const mode = frame?.mode ?? 'idle';
      const pulse = reducedMotion ? 1 : 0.88 + 0.12 * Math.sin(now * (mode === 'swarm' ? 0.006 : mode === 'tooling' ? 0.0045 : 0.002));

      // Subtle dynamic grid mesh whose intensity responds to AI activity.
      ctx.strokeStyle = palette.grid;
      ctx.lineWidth = mode === 'swarm' || mode === 'tooling' ? 0.85 : 0.6;
      ctx.globalAlpha = Math.min(1, 0.55 + energy * 0.75);
      ctx.beginPath();
      for (let x = 1; x < cols; x++) {
        const px = Math.round(x * cellW) + 0.5;
        ctx.moveTo(px, 0);
        ctx.lineTo(px, height);
      }
      for (let y = 1; y < rows; y++) {
        const py = Math.round(y * cellH) + 0.5;
        ctx.moveTo(0, py);
        ctx.lineTo(width, py);
      }
      ctx.stroke();
      ctx.globalAlpha = 1;

      if (frame && frame.cells.length === cols * rows) {
        const padX = Math.max(1.2, cellW * 0.16);
        const padY = Math.max(1.2, cellH * 0.16);
        const radius = Math.min(cellW - padX * 2, cellH - padY * 2) * 0.28;

        for (let y = 0; y < rows; y++) {
          const rowOffset = y * cols;
          for (let x = 0; x < cols; x++) {
            const code = frame.cells.charCodeAt(rowOffset + x) - 48;
            if (code <= 0 || code > 7) continue;

            const px = x * cellW + padX;
            const py = y * cellH + padY;
            const cw = Math.max(2, cellW - padX * 2);
            const ch = Math.max(2, cellH - padY * 2);

            if (code >= 4) {
              ctx.fillStyle = palette.trail[code - 4];
              ctx.fillRect(px, py, cw, ch);
              continue;
            }

            const isTool = code === 2;
            const isSub = code === 3;
            const fill = isSub ? palette.cell3 : isTool ? palette.cell2 : palette.cell1;
            const glow = isSub ? palette.glow3 : isTool ? palette.glow2 : palette.glow1;

            if (!reducedMotion && (isTool || isSub || mode !== 'idle')) {
              ctx.shadowColor = glow;
              ctx.shadowBlur = isSub ? 14 * pulse : isTool ? 10 * pulse : 5;
            } else {
              ctx.shadowBlur = 0;
            }

            ctx.fillStyle = fill;
            ctx.beginPath();
            const scale = isSub ? 0.94 + 0.08 * pulse : isTool ? 0.92 + 0.06 * pulse : 0.9;
            const sw = cw * scale;
            const sh = ch * scale;
            const sx = px + (cw - sw) * 0.5;
            const sy = py + (ch - sh) * 0.5;
            if (typeof ctx.roundRect === 'function') {
              ctx.roundRect(sx, sy, sw, sh, Math.max(1.5, radius));
            } else {
              ctx.rect(sx, sy, sw, sh);
            }
            ctx.fill();
          }
        }
        ctx.shadowBlur = 0;
      }

      if (!reducedMotion) {
        rafId = window.requestAnimationFrame(draw);
      }
    };

    rafId = window.requestAnimationFrame(draw);
    return () => {
      window.removeEventListener('resize', resize);
      if (rafId) window.cancelAnimationFrame(rafId);
    };
  }, []);

  return <canvas ref={canvasRef} className="automaton-bg" aria-hidden="true" />;
}
