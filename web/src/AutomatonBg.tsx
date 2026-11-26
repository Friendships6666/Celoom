import { useEffect, useRef } from 'react';
import {
  computeWorkloadDynamics,
  renderDiscreteCellPattern,
  selectDynamicPresets,
  type AnimationStyleId,
  type AutomatonMode,
  type LiveWorkload,
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

function hexByte(s: string, offset: number): number {
  const hi = s.charCodeAt(offset);
  const lo = s.charCodeAt(offset + 1);
  const hn = hi >= 48 && hi <= 57 ? hi - 48 : hi >= 97 && hi <= 102 ? hi - 87 : hi >= 65 && hi <= 70 ? hi - 55 : 0;
  const ln = lo >= 48 && lo <= 57 ? lo - 48 : lo >= 97 && lo <= 102 ? lo - 87 : lo >= 65 && lo <= 70 ? lo - 55 : 0;
  return (hn << 4) | ln;
}

const PRESET_LOCK_MS = 60_000;

export function AutomatonBg({ theme, authenticated }: { theme: 'light' | 'dark'; authenticated: boolean }) {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const frameRef = useRef<AutomatonFrame | null>(null);
  const decodedRef = useRef<Uint8Array>(new Uint8Array(0));
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

    const reducedMotion = window.matchMedia?.('(prefers-reduced-motion: reduce)').matches ?? false;
    let rafId = 0;
    let lastDraw = 0;
    let prevNow = performance.now();

    // Monotonic integrated time & smoothed dynamics prevent backward jumps / flickering.
    let animTime = 0;
    let smoothSpeed = 1.9;
    let smoothIntensity = 0;
    let lockedStyles: AnimationStyleId[] = [];
    let lockedUntil = 0;
    let lastNonIdleMode: AutomatonMode = 'thinking';

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
      const frame = frameRef.current;
      const workload: LiveWorkload = {
        mode: frame?.mode ?? 'idle',
        activeSessions: frame?.active_sessions ?? 0,
        activeTools: frame?.active_tools ?? 0,
        activeSubagents: frame?.active_subagents ?? 0,
        toolKinds: frame?.tool_kinds ?? 0,
        energy: frame?.energy ?? 0,
      };
      const minInterval = workload.mode === 'idle' && smoothIntensity <= 0.02 ? 120 : 16;
      if (!reducedMotion && now - lastDraw < minInterval) {
        rafId = window.requestAnimationFrame(draw);
        return;
      }
      const dt = Math.min(0.1, Math.max(0.001, (now - prevNow) * 0.001));
      prevNow = now;
      lastDraw = now;

      const targetDyn = computeWorkloadDynamics(workload);
      smoothSpeed += (targetDyn.speed - smoothSpeed) * 0.05;
      smoothIntensity += (targetDyn.intensity - smoothIntensity) * (workload.mode === 'idle' ? 0.08 : 0.12);
      animTime += dt * smoothSpeed;

      if (workload.mode !== 'idle') {
        lastNonIdleMode = workload.mode;
        if (lockedStyles.length === 0 || now >= lockedUntil) {
          lockedStyles = selectDynamicPresets(workload, now * 0.001);
          lockedUntil = now + PRESET_LOCK_MS;
        }
      }

      ctx.clearRect(0, 0, canvas.width, canvas.height);

      if (smoothIntensity > 0.02 && lockedStyles.length > 0) {
        const renderWorkload: LiveWorkload =
          workload.mode === 'idle' ? { ...workload, mode: lastNonIdleMode } : workload;
        renderDiscreteCellPattern(
          ctx,
          canvas.width,
          canvas.height,
          animTime,
          lockedStyles,
          renderWorkload,
          themeRef.current,
          decodedRef.current,
          frame?.cols,
          frame?.rows,
          smoothIntensity,
        );
      }

      if (!reducedMotion) {
        rafId = window.requestAnimationFrame(draw);
      }
    };

    const onVisibility = () => {
      if (!document.hidden && !reducedMotion && !rafId) {
        prevNow = performance.now();
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

export function AutomatonPreview({
  style,
  workload,
  theme,
}: {
  style: AnimationStyleId | 'auto';
  workload: LiveWorkload;
  theme: 'light' | 'dark';
}) {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const propsRef = useRef({ style, workload, theme });
  propsRef.current = { style, workload, theme };

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const ctx = canvas.getContext('2d', { alpha: true });
    if (!ctx) return;
    let rafId = 0;
    let prevNow = performance.now();
    let animTime = 0;
    let smoothSpeed = 1.9;
    let smoothIntensity = 1.2;

    const draw = (now: number) => {
      const { style: sel, workload: wl, theme: th } = propsRef.current;
      const dt = Math.min(0.1, Math.max(0.001, (now - prevNow) * 0.001));
      prevNow = now;
      const dyn = computeWorkloadDynamics(wl);
      smoothSpeed += (dyn.speed - smoothSpeed) * 0.08;
      smoothIntensity += (dyn.intensity - smoothIntensity) * 0.1;
      animTime += dt * smoothSpeed;

      const activeStyles = sel === 'auto' ? selectDynamicPresets(wl, now * 0.001) : [sel];
      ctx.clearRect(0, 0, canvas.width, canvas.height);

      if (wl.mode !== 'idle' && smoothIntensity > 0.02) {
        if (activeStyles.includes('cellular')) {
          const cols = 28;
          const rows = 14;
          const fake = new Uint8Array(cols * rows * 3);
          for (let r = 0; r < rows; r++) {
            for (let c = 0; c < cols; c++) {
              const v = Math.max(0, Math.sin(c * 0.55 + animTime * 1.4) * Math.cos(r * 0.6 - animTime * 1.1));
              const i = (r * cols + c) * 3;
              fake[i] = Math.round(50 * v);
              fake[i + 1] = Math.round(220 * v);
              fake[i + 2] = Math.round(255 * v);
            }
          }
          renderDiscreteCellPattern(ctx, canvas.width, canvas.height, animTime, activeStyles, wl, th, fake, cols, rows, smoothIntensity);
        } else {
          renderDiscreteCellPattern(ctx, canvas.width, canvas.height, animTime, activeStyles, wl, th, undefined, undefined, undefined, smoothIntensity);
        }
      }
      rafId = window.requestAnimationFrame(draw);
    };
    rafId = window.requestAnimationFrame(draw);
    return () => window.cancelAnimationFrame(rafId);
  }, []);

  return <canvas ref={canvasRef} width={640} height={240} className="anim-preview-canvas" />;
}
