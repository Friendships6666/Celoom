import { useEffect, useRef } from 'react';

type AutomatonMode = 'idle' | 'thinking' | 'tooling' | 'swarm';

interface AutomatonFrame {
  cols: number;
  rows: number;
  tick: number;
  mode: AutomatonMode;
  active_sessions: number;
  active_tools: number;
  active_subagents: number;
  alive_cells: number;
  energy: number;
  /** Hex-encoded `u8` states (`00`..`ff`), 2 chars per cell. */
  cells: string;
}

interface Rgb {
  r: number;
  g: number;
  b: number;
}

interface ModePalette {
  low: Rgb;
  mid: Rgb;
  high: Rgb;
}

const DARK_PALETTES: Record<AutomatonMode, ModePalette> = {
  idle: { low: { r: 10, g: 52, b: 68 }, mid: { r: 18, g: 138, b: 148 }, high: { r: 72, g: 224, b: 220 } },
  thinking: { low: { r: 34, g: 28, b: 92 }, mid: { r: 99, g: 102, b: 241 }, high: { r: 168, g: 130, b: 255 } },
  tooling: { low: { r: 58, g: 38, b: 12 }, mid: { r: 217, g: 119, b: 6 }, high: { r: 52, g: 211, b: 153 } },
  swarm: { low: { r: 72, g: 18, b: 62 }, mid: { r: 236, g: 72, b: 153 }, high: { r: 34, g: 211, b: 238 } },
};

const LIGHT_PALETTES: Record<AutomatonMode, ModePalette> = {
  idle: { low: { r: 196, g: 226, b: 228 }, mid: { r: 20, g: 136, b: 144 }, high: { r: 13, g: 110, b: 122 } },
  thinking: { low: { r: 214, g: 212, b: 246 }, mid: { r: 99, g: 102, b: 241 }, high: { r: 109, g: 40, b: 217 } },
  tooling: { low: { r: 246, g: 222, b: 188 }, mid: { r: 217, g: 119, b: 6 }, high: { r: 16, g: 148, b: 108 } },
  swarm: { low: { r: 246, g: 204, b: 228 }, mid: { r: 219, g: 39, b: 119 }, high: { r: 8, g: 145, b: 178 } },
};

function packRgba(r: number, g: number, b: number, a: number): number {
  // Little-endian ABGR in Uint32Array -> RGBA bytes in ImageData.
  return ((a & 255) << 24) | ((b & 255) << 16) | ((g & 255) << 8) | (r & 255);
}

function buildPaletteLut(palette: ModePalette, maxAlpha: number): Uint32Array {
  const lut = new Uint32Array(256);
  lut[0] = 0;
  for (let i = 1; i < 256; i++) {
    const t = i / 255;
    const c = t < 0.5
      ? {
          r: palette.low.r + (palette.mid.r - palette.low.r) * (t * 2),
          g: palette.low.g + (palette.mid.g - palette.low.g) * (t * 2),
          b: palette.low.b + (palette.mid.b - palette.low.b) * (t * 2),
        }
      : {
          r: palette.mid.r + (palette.high.r - palette.mid.r) * ((t - 0.5) * 2),
          g: palette.mid.g + (palette.high.g - palette.mid.g) * ((t - 0.5) * 2),
          b: palette.mid.b + (palette.high.b - palette.mid.b) * ((t - 0.5) * 2),
        };
    const alpha = Math.round((0.04 + Math.pow(t, 0.85) * maxAlpha) * 255);
    lut[i] = packRgba(Math.round(c.r), Math.round(c.g), Math.round(c.b), alpha);
  }
  return lut;
}

const DARK_LUTS: Record<AutomatonMode, Uint32Array> = {
  idle: buildPaletteLut(DARK_PALETTES.idle, 0.42),
  thinking: buildPaletteLut(DARK_PALETTES.thinking, 0.54),
  tooling: buildPaletteLut(DARK_PALETTES.tooling, 0.58),
  swarm: buildPaletteLut(DARK_PALETTES.swarm, 0.62),
};

const LIGHT_LUTS: Record<AutomatonMode, Uint32Array> = {
  idle: buildPaletteLut(LIGHT_PALETTES.idle, 0.24),
  thinking: buildPaletteLut(LIGHT_PALETTES.thinking, 0.30),
  tooling: buildPaletteLut(LIGHT_PALETTES.tooling, 0.34),
  swarm: buildPaletteLut(LIGHT_PALETTES.swarm, 0.36),
};

function hexNibble(code: number): number {
  if (code >= 48 && code <= 57) return code - 48;
  if (code >= 97 && code <= 102) return code - 87;
  if (code >= 65 && code <= 70) return code - 55;
  return 0;
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
      // Cap backing buffer at 960x540 so GPU bilinear upscaling stays fluid with minimal fillrate.
      const aspect = window.innerHeight > 0 ? window.innerWidth / window.innerHeight : 16 / 9;
      const w = Math.min(window.innerWidth, 960);
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
      const frame = frameRef.current;
      const mode: AutomatonMode = frame?.mode ?? 'idle';
      const minInterval = mode === 'idle' ? 45 : 32;
      if (!reducedMotion && now - lastDraw < minInterval) {
        rafId = window.requestAnimationFrame(draw);
        return;
      }
      lastDraw = now;

      const cols = frame?.cols ?? 28;
      const rows = frame?.rows ?? 16;
      const total = cols * rows;

      if (offCanvas.width !== cols || offCanvas.height !== rows || !imgData || !rgba32) {
        offCanvas.width = cols;
        offCanvas.height = rows;
        imgData = offCtx.createImageData(cols, rows);
        rgba32 = new Uint32Array(imgData.data.buffer);
      }

      if (displayRef.current.length !== total) {
        displayRef.current = new Float32Array(total);
      }
      const target = decodedRef.current;
      const disp = displayRef.current;
      const lerpRate = mode === 'idle' ? 0.22 : 0.36;
      if (target.length === total) {
        for (let i = 0; i < total; i++) {
          disp[i] += (target[i] - disp[i]) * lerpRate;
        }
      }

      const lut = (themeRef.current === 'dark' ? DARK_LUTS : LIGHT_LUTS)[mode];
      for (let i = 0; i < total; i++) {
        const idx = Math.min(255, Math.max(0, Math.round(disp[i])));
        rgba32[i] = idx <= 3 ? 0 : lut[idx];
      }

      offCtx.putImageData(imgData, 0, 0);
      ctx.clearRect(0, 0, canvas.width, canvas.height);
      ctx.imageSmoothingEnabled = true;
      ctx.imageSmoothingQuality = 'high';
      ctx.drawImage(offCanvas, 0, 0, canvas.width, canvas.height);

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
