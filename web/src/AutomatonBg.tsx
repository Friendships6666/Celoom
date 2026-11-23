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
  /** Hex-encoded `u8` states (`00`..`ff`), 2 chars per cell. */
  cells: string;
}

function build256Palette(theme: 'light' | 'dark'): string[] {
  const lut = new Array<string>(256);
  lut[0] = 'transparent';
  for (let i = 1; i < 256; i++) {
    const t = i / 255;
    if (theme === 'dark') {
      // 256-color dark bioluminescent spectrum: deep teal -> cyan -> sky -> indigo -> violet -> magenta
      const hue = (172 + t * 142) % 360;
      const sat = 72 + Math.sin(t * Math.PI) * 22;
      const light = 46 + t * 24;
      const alpha = 0.08 + Math.pow(t, 0.82) * 0.62;
      lut[i] = `hsla(${hue.toFixed(1)}, ${sat.toFixed(0)}%, ${light.toFixed(0)}%, ${alpha.toFixed(3)})`;
    } else {
      // 256-color light botanical bloom spectrum: blush rose -> crimson -> coral -> amber -> plum
      const hue = (342 + t * 88) % 360;
      const sat = 58 + Math.sin(t * Math.PI) * 24;
      const light = 64 - t * 22;
      const alpha = 0.06 + Math.pow(t, 0.85) * 0.42;
      lut[i] = `hsla(${hue.toFixed(1)}, ${sat.toFixed(0)}%, ${light.toFixed(0)}%, ${alpha.toFixed(3)})`;
    }
  }
  return lut;
}

const DARK_LUT = build256Palette('dark');
const LIGHT_LUT = build256Palette('light');

function createGlowSprite(color: string): HTMLCanvasElement {
  const off = document.createElement('canvas');
  off.width = 64;
  off.height = 64;
  const octx = off.getContext('2d');
  if (octx) {
    const grad = octx.createRadialGradient(32, 32, 2, 32, 32, 32);
    grad.addColorStop(0, color);
    grad.addColorStop(0.45, color.replace(/[\d.]+\)$/, '0.22)'));
    grad.addColorStop(1, 'rgba(0,0,0,0)');
    octx.fillStyle = grad;
    octx.fillRect(0, 0, 64, 64);
  }
  return off;
}

export function AutomatonBg({ theme, authenticated }: { theme: 'light' | 'dark'; authenticated: boolean }) {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const frameRef = useRef<AutomatonFrame | null>(null);
  const decodedRef = useRef<Uint8Array>(new Uint8Array(0));
  const displayRef = useRef<Float32Array>(new Float32Array(0));
  const themeRef = useRef(theme);
  themeRef.current = theme;

  useEffect(() => {
    if (!authenticated) {
      frameRef.current = null;
      decodedRef.current = new Uint8Array(0);
      return;
    }
    const source = new EventSource('/api/automaton/stream', { withCredentials: true });
    source.addEventListener('frame', raw => {
      try {
        const parsed = JSON.parse((raw as MessageEvent<string>).data) as AutomatonFrame;
        if (!parsed || typeof parsed.cols !== 'number' || typeof parsed.rows !== 'number' || typeof parsed.cells !== 'string') {
          return;
        }
        const total = parsed.cols * parsed.rows;
        if (parsed.cells.length !== total * 2) return;
        const buf = new Uint8Array(total);
        for (let i = 0; i < total; i++) {
          buf[i] = parseInt(parsed.cells.slice(i * 2, i * 2 + 2), 16) || 0;
        }
        frameRef.current = parsed;
        decodedRef.current = buf;
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
    const darkGlow = createGlowSprite('rgba(86, 223, 223, 0.55)');
    const lightGlow = createGlowSprite('rgba(168, 48, 82, 0.35)');
    let rafId = 0;
    let width = 0;
    let height = 0;
    let dpr = 1;
    let lastDraw = 0;

    const resize = () => {
      dpr = Math.min(window.devicePixelRatio || 1, 1.5);
      width = window.innerWidth;
      height = window.innerHeight;
      canvas.width = Math.max(1, Math.floor(width * dpr));
      canvas.height = Math.max(1, Math.floor(height * dpr));
    };
    resize();
    window.addEventListener('resize', resize);

    const draw = (now: number) => {
      const frame = frameRef.current;
      const mode = frame?.mode ?? 'idle';
      const minInterval = mode === 'idle' ? 50 : 28; // ~20fps idle, ~35fps active for low CPU
      if (!reducedMotion && now - lastDraw < minInterval) {
        rafId = window.requestAnimationFrame(draw);
        return;
      }
      lastDraw = now;

      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
      ctx.clearRect(0, 0, width, height);

      const isDark = themeRef.current === 'dark';
      const lut = isDark ? DARK_LUT : LIGHT_LUT;
      const glowSprite = isDark ? darkGlow : lightGlow;
      const cols = frame?.cols ?? 28;
      const rows = frame?.rows ?? 16;
      const total = cols * rows;
      const cellW = width / cols;
      const cellH = height / rows;
      const energy = frame?.energy ?? 0.08;

      if (displayRef.current.length !== total) {
        displayRef.current = new Float32Array(total);
      }
      const target = decodedRef.current;
      const disp = displayRef.current;
      const lerpRate = mode === 'idle' ? 0.14 : 0.28;
      if (target.length === total) {
        for (let i = 0; i < total; i++) {
          disp[i] += (target[i] - disp[i]) * lerpRate;
        }
      }

      // Subtle breathing grid lines scaled by AI activity.
      ctx.strokeStyle = isDark ? 'rgba(86, 223, 223, 0.045)' : 'rgba(136, 39, 63, 0.045)';
      ctx.lineWidth = mode === 'swarm' || mode === 'tooling' ? 0.9 : 0.6;
      ctx.globalAlpha = Math.min(1, 0.4 + energy * 0.8);
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

      const pulse = reducedMotion ? 1 : 0.9 + 0.1 * Math.sin(now * (mode === 'idle' ? 0.0018 : 0.0045));
      const padX = Math.max(1.5, cellW * 0.14);
      const padY = Math.max(1.5, cellH * 0.14);
      const baseW = Math.max(2, cellW - padX * 2);
      const baseH = Math.max(2, cellH - padY * 2);
      const radius = Math.min(baseW, baseH) * 0.34;

      for (let y = 0; y < rows; y++) {
        const rowOffset = y * cols;
        for (let x = 0; x < cols; x++) {
          const val = disp[rowOffset + x];
          const idx = Math.min(255, Math.round(val));
          if (idx <= 2) continue;

          const px = x * cellW + padX;
          const py = y * cellH + padY;
          const t = idx / 255;

          // Pre-rendered radial halo for energetic cells (no expensive shadowBlur).
          if (idx >= 120 && !reducedMotion) {
            const haloSize = Math.max(cellW, cellH) * (1.7 + t * 0.9) * pulse;
            const cx = x * cellW + cellW * 0.5;
            const cy = y * cellH + cellH * 0.5;
            ctx.globalAlpha = (t - 0.35) * 0.65;
            ctx.drawImage(glowSprite, cx - haloSize * 0.5, cy - haloSize * 0.5, haloSize, haloSize);
            ctx.globalAlpha = 1;
          }

          const scale = 0.62 + t * 0.38 * pulse;
          const sw = baseW * scale;
          const sh = baseH * scale;
          const sx = px + (baseW - sw) * 0.5;
          const sy = py + (baseH - sh) * 0.5;

          ctx.fillStyle = lut[idx];
          ctx.beginPath();
          if (typeof ctx.roundRect === 'function') {
            ctx.roundRect(sx, sy, sw, sh, Math.max(2, radius * scale));
          } else {
            ctx.rect(sx, sy, sw, sh);
          }
          ctx.fill();
        }
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
