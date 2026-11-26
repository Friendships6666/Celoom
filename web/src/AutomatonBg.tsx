import { useEffect, useRef } from 'react';
import {
  ANIM_SETTINGS_EVENT,
  loadAnimSettings,
  renderVectorOverlay,
  type AnimationSettings,
  type AutomatonMode,
} from './automatonStyles';

interface AutomatonFrame {
  cols: number;
  rows: number;
  tick: number;
  mode: AutomatonMode;
  active_sessions: number;
  active_tools: number;
  active_subagents: number;
  tool_kinds?: number;
  alive_cells: number;
  energy: number;
  /** Hex-encoded `rrggbb` per cell (6 chars per cell). */
  cells: string;
}

function packRgba(r: number, g: number, b: number, a: number): number {
  return ((a & 255) << 24) | ((b & 255) << 16) | ((g & 255) << 8) | (r & 255);
}

function hexByte(s: string, offset: number): number {
  const hi = s.charCodeAt(offset);
  const lo = s.charCodeAt(offset + 1);
  const hn = hi >= 48 && hi <= 57 ? hi - 48 : hi >= 97 && hi <= 102 ? hi - 87 : hi >= 65 && hi <= 70 ? hi - 55 : 0;
  const ln = lo >= 48 && lo <= 57 ? lo - 48 : lo >= 97 && lo <= 102 ? lo - 87 : lo >= 65 && lo <= 70 ? lo - 55 : 0;
  return (hn << 4) | ln;
}

export function AutomatonBg({ theme, authenticated }: { theme: 'light' | 'dark'; authenticated: boolean }) {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const frameRef = useRef<AutomatonFrame | null>(null);
  const decodedRef = useRef<Uint8Array>(new Uint8Array(0));
  const displayRef = useRef<Float32Array>(new Float32Array(0));
  const themeRef = useRef(theme);
  const settingsRef = useRef<AnimationSettings>(loadAnimSettings());
  themeRef.current = theme;

  useEffect(() => {
    const sync = (ev?: Event) => {
      const detail = (ev as CustomEvent<AnimationSettings> | undefined)?.detail;
      settingsRef.current = detail ?? loadAnimSettings();
    };
    window.addEventListener(ANIM_SETTINGS_EVENT, sync);
    window.addEventListener('storage', sync);
    return () => {
      window.removeEventListener(ANIM_SETTINGS_EVENT, sync);
      window.removeEventListener('storage', sync);
    };
  }, []);

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
        if (parsed.cells.length !== total * 6) return;
        const needed = total * 3;
        let buf = decodedRef.current;
        if (buf.length !== needed) {
          buf = new Uint8Array(needed);
          decodedRef.current = buf;
        }
        const cells = parsed.cells;
        for (let i = 0; i < needed; i++) {
          buf[i] = hexByte(cells, i * 2);
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
    const ctx = canvas.getContext('2d', { alpha: true });
    if (!ctx) return;

    const offCanvas = document.createElement('canvas');
    const offCtx = offCanvas.getContext('2d');
    if (!offCtx) return;

    let imgData: ImageData | null = null;
    let rgba32: Uint32Array | null = null;
    const reducedMotion = window.matchMedia?.('(prefers-reduced-motion: reduce)').matches ?? false;
    let rafId = 0;
    let lastDraw = 0;

    const resize = () => {
      const aspect = window.innerHeight > 0 ? window.innerWidth / window.innerHeight : 16 / 9;
      const w = Math.min(window.innerWidth, 1200);
      const h = Math.max(1, Math.round(w / Math.max(0.25, aspect)));
      canvas.width = Math.max(1, w);
      canvas.height = Math.max(1, h);
    };
    resize();
    window.addEventListener('resize', resize);

    const draw = (now: number) => {
      if (document.hidden) {
        rafId = 0;
        return;
      }
      const settings = settingsRef.current;
      const frame = frameRef.current;
      const mode: AutomatonMode = settings.previewMode !== 'live' ? settings.previewMode : (frame?.mode ?? 'idle');
      const minInterval = settings.style === 'off' ? 250 : mode === 'idle' ? 32 : 24;
      if (!reducedMotion && now - lastDraw < minInterval) {
        rafId = window.requestAnimationFrame(draw);
        return;
      }
      lastDraw = now;

      ctx.clearRect(0, 0, canvas.width, canvas.height);

      if (settings.style !== 'off') {
        const cols = frame?.cols ?? 28;
        const rows = frame?.rows ?? 16;
        const total = cols * rows;
        const needed = total * 3;

        if (offCanvas.width !== cols || offCanvas.height !== rows || !imgData || !rgba32) {
          offCanvas.width = cols;
          offCanvas.height = rows;
          imgData = offCtx.createImageData(cols, rows);
          rgba32 = new Uint32Array(imgData.data.buffer);
        }

        if (displayRef.current.length !== needed) {
          displayRef.current = new Float32Array(needed);
        }
        const target = decodedRef.current;
        const disp = displayRef.current;
        const lerpRate = mode === 'idle' ? 0.24 : 0.38;
        if (target.length === needed) {
          for (let i = 0; i < needed; i++) {
            disp[i] += (target[i] - disp[i]) * lerpRate;
          }
        }

        const isDark = themeRef.current === 'dark';
        const styleDim = settings.style === 'cellular' ? 1.0 : 0.65;
        const maxAlpha = (isDark
          ? (mode === 'idle' ? 0.42 : mode === 'thinking' ? 0.54 : 0.62)
          : (mode === 'idle' ? 0.22 : mode === 'thinking' ? 0.28 : 0.34)) * styleDim * settings.intensity;

        for (let i = 0; i < total; i++) {
          const base = i * 3;
          const r = Math.min(255, Math.max(0, Math.round(disp[base])));
          const g = Math.min(255, Math.max(0, Math.round(disp[base + 1])));
          const b = Math.min(255, Math.max(0, Math.round(disp[base + 2])));
          const peak = r > g ? (r > b ? r : b) : (g > b ? g : b);
          if (peak <= 4) {
            rgba32[i] = 0;
          } else {
            const t = peak / 255;
            const alpha = Math.min(255, Math.round((0.03 + Math.pow(t, 0.82) * maxAlpha) * 255));
            rgba32[i] = packRgba(r, g, b, alpha);
          }
        }

        offCtx.putImageData(imgData, 0, 0);
        ctx.imageSmoothingEnabled = true;
        ctx.imageSmoothingQuality = 'high';
        ctx.drawImage(offCanvas, 0, 0, canvas.width, canvas.height);

        const energy = settings.previewMode !== 'live'
          ? (mode === 'swarm' ? 1.0 : mode === 'tooling' ? 0.8 : mode === 'thinking' ? 0.55 : 0.25)
          : (frame?.energy ?? 0.25);
        renderVectorOverlay(ctx, canvas.width, canvas.height, now * 0.001, mode, energy, themeRef.current, settings);
      }

      if (!reducedMotion) {
        rafId = window.requestAnimationFrame(draw);
      }
    };

    const onVisibility = () => {
      if (!document.hidden && !reducedMotion && !rafId) {
        rafId = window.requestAnimationFrame(draw);
      }
    };
    document.addEventListener('visibilitychange', onVisibility);
    rafId = window.requestAnimationFrame(draw);
    return () => {
      window.removeEventListener('resize', resize);
      document.removeEventListener('visibilitychange', onVisibility);
      if (rafId) window.cancelAnimationFrame(rafId);
    };
  }, []);

  return <canvas ref={canvasRef} className="automaton-bg" aria-hidden="true" />;
}

export function AutomatonPreview({ settings, theme }: { settings: AnimationSettings; theme: 'light' | 'dark' }) {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const settingsRef = useRef(settings);
  const themeRef = useRef(theme);
  settingsRef.current = settings;
  themeRef.current = theme;

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const ctx = canvas.getContext('2d', { alpha: true });
    if (!ctx) return;
    let rafId = 0;
    const draw = (now: number) => {
      const s = settingsRef.current;
      const mode: AutomatonMode = s.previewMode === 'live' ? 'thinking' : s.previewMode;
      const energy = mode === 'swarm' ? 1.0 : mode === 'tooling' ? 0.8 : mode === 'thinking' ? 0.55 : 0.25;
      ctx.clearRect(0, 0, canvas.width, canvas.height);
      if (s.style === 'cellular') {
        // Synthetic preview grid when backend cellular mode is selected.
        const cols = 28;
        const rows = 14;
        const cw = canvas.width / cols;
        const ch = canvas.height / rows;
        for (let r = 0; r < rows; r++) {
          for (let c = 0; c < cols; c++) {
            const v = 0.5 + 0.5 * Math.sin(c * 0.45 + now * 0.002 * s.speed) * Math.cos(r * 0.5 - now * 0.0016 * s.speed);
            const hue = (185 + (c * 7 + r * 11 + now * 0.02)) % 360;
            ctx.fillStyle = `hsla(${hue}, 82%, 58%, ${(v * 0.5 * s.intensity).toFixed(3)})`;
            ctx.fillRect(c * cw, r * ch, cw + 0.5, ch + 0.5);
          }
        }
      }
      renderVectorOverlay(ctx, canvas.width, canvas.height, now * 0.001, mode, energy, themeRef.current, s);
      rafId = window.requestAnimationFrame(draw);
    };
    rafId = window.requestAnimationFrame(draw);
    return () => window.cancelAnimationFrame(rafId);
  }, []);

  return <canvas ref={canvasRef} width={640} height={240} className="anim-preview-canvas" />;
}
