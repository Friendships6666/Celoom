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
      // Rich 256-color bioluminescent spectrum sweeping 320 deg:
      // emerald -> cyan -> sky -> indigo -> violet -> magenta -> coral-amber
      const hue = (150 + t * 290) % 360;
      const sat = 76 + Math.sin(t * Math.PI * 2) * 16;
      const light = 42 + t * 28;
      const alpha = 0.08 + Math.pow(t, 0.78) * 0.62;
      lut[i] = `hsla(${hue.toFixed(1)}, ${sat.toFixed(0)}%, ${light.toFixed(0)}%, ${alpha.toFixed(3)})`;
    } else {
      // Rich 256-color botanical prism sweeping 300 deg:
      // rose -> crimson -> coral -> amber -> sage-teal -> cobalt -> plum
      const hue = (335 + t * 285) % 360;
      const sat = 62 + Math.sin(t * Math.PI * 2) * 18;
      const light = 66 - t * 22;
      const alpha = 0.06 + Math.pow(t, 0.8) * 0.42;
      lut[i] = `hsla(${hue.toFixed(1)}, ${sat.toFixed(0)}%, ${light.toFixed(0)}%, ${alpha.toFixed(3)})`;
    }
  }
  return lut;
}

const DARK_LUT = build256Palette('dark');
const LIGHT_LUT = build256Palette('light');

function hexNibble(code: number): number {
  if (code >= 48 && code <= 57) return code - 48;
  if (code >= 97 && code <= 102) return code - 87;
  if (code >= 65 && code <= 70) return code - 55;
  return 0;
}

function createGlowSprite(color: string): HTMLCanvasElement {
  const off = document.createElement('canvas');
  off.width = 48;
  off.height = 48;
  const octx = off.getContext('2d');
  if (octx) {
    const grad = octx.createRadialGradient(24, 24, 2, 24, 24, 24);
    grad.addColorStop(0, color);
    grad.addColorStop(0.5, color.replace(/[\d.]+\)$/, '0.18)'));
    grad.addColorStop(1, 'rgba(0,0,0,0)');
    octx.fillStyle = grad;
    octx.fillRect(0, 0, 48, 48);
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
        let buf = decodedRef.current;
        if (buf.length !== total) {
          buf = new Uint8Array(total);
          decodedRef.current = buf;
        }
        const cells = parsed.cells;
        for (let i = 0; i < total; i++) {
          buf[i] = (hexNibble(cells.charCodeAt(i * 2)) << 4) | hexNibble(cells.charCodeAt(i * 2 + 1));
        }
        frameRef.current = parsed;
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
    const darkGlow = createGlowSprite('rgba(86, 223, 223, 0.50)');
    const lightGlow = createGlowSprite('rgba(168, 48, 82, 0.32)');
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
      const minInterval = mode === 'idle' ? 40 : 28; // ~25fps idle, ~35fps active
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
      const energy = frame?.energy ?? 0.1;

      if (displayRef.current.length !== total) {
        displayRef.current = new Float32Array(total);
      }
      const target = decodedRef.current;
      const disp = displayRef.current;
      const lerpRate = mode === 'idle' ? 0.20 : 0.34;
      if (target.length === total) {
        for (let i = 0; i < total; i++) {
          disp[i] += (target[i] - disp[i]) * lerpRate;
        }
      }

      // Subtle rhythmic grid mesh.
      ctx.strokeStyle = isDark ? 'rgba(86, 223, 223, 0.045)' : 'rgba(136, 39, 63, 0.045)';
      ctx.lineWidth = mode === 'swarm' || mode === 'tooling' ? 0.85 : 0.6;
      ctx.globalAlpha = Math.min(1, 0.38 + energy * 0.72);
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

      const phaseShift = reducedMotion ? 0 : Math.floor((now * (mode === 'idle' ? 0.015 : 0.04)) % 255);
      const padX = Math.max(1.5, cellW * 0.16);
      const padY = Math.max(1.5, cellH * 0.16);
      const baseW = Math.max(2, cellW - padX * 2);
      const baseH = Math.max(2, cellH - padY * 2);

      for (let y = 0; y < rows; y++) {
        const rowOffset = y * cols;
        for (let x = 0; x < cols; x++) {
          const val = disp[rowOffset + x];
          const rawIdx = Math.min(255, Math.round(val));
          if (rawIdx <= 3) continue;

          const t = rawIdx / 255;
          // Subtle spatial-temporal harmonic color cycling across all 256 colors.
          const colorIdx = 1 + ((rawIdx + phaseShift + ((x * 5 + y * 9) & 63)) % 255);
          const wavePulse = reducedMotion ? 1 : 0.86 + 0.18 * Math.sin(now * 0.005 + x * 0.45 + y * 0.35);

          const px = x * cellW + padX;
          const py = y * cellH + padY;

          if (rawIdx >= 150 && !reducedMotion) {
            const haloSize = Math.max(cellW, cellH) * (1.5 + t * 0.9) * wavePulse;
            const cx = x * cellW + cellW * 0.5;
            const cy = y * cellH + cellH * 0.5;
            ctx.globalAlpha = (t - 0.45) * 0.6;
            ctx.drawImage(glowSprite, cx - haloSize * 0.5, cy - haloSize * 0.5, haloSize, haloSize);
            ctx.globalAlpha = 1;
          }

          const scale = (0.52 + t * 0.46) * wavePulse;
          const sw = baseW * scale;
          const sh = baseH * scale;
          const sx = px + (baseW - sw) * 0.5;
          const sy = py + (baseH - sh) * 0.5;

          ctx.fillStyle = lut[colorIdx];
          ctx.fillRect(sx, sy, sw, sh);
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
