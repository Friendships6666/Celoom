export type AlertKind = 'completed' | 'question' | 'network' | 'stopped';

const MUTE_KEY = 'celoom-sound-muted';
let audioCtx: AudioContext | null = null;
let unlocked = false;

export function isSoundMuted(): boolean {
  try {
    return localStorage.getItem(MUTE_KEY) === '1';
  } catch {
    return false;
  }
}

export function setSoundMuted(muted: boolean): void {
  try {
    localStorage.setItem(MUTE_KEY, muted ? '1' : '0');
  } catch {
    /* Storage may be disabled. */
  }
}

function getContext(): AudioContext | null {
  if (typeof window === 'undefined') return null;
  const Ctor = window.AudioContext || (window as unknown as { webkitAudioContext?: typeof AudioContext }).webkitAudioContext;
  if (!Ctor) return null;
  if (!audioCtx) audioCtx = new Ctor();
  return audioCtx;
}

export function installAudioUnlock(): () => void {
  if (typeof window === 'undefined') return () => {};
  const unlock = () => {
    const ctx = getContext();
    if (!ctx) return;
    if (ctx.state === 'suspended') {
      void ctx.resume().then(() => { unlocked = true; }).catch(() => {});
    } else {
      unlocked = true;
    }
  };
  window.addEventListener('pointerdown', unlock, { passive: true });
  window.addEventListener('keydown', unlock, { passive: true });
  return () => {
    window.removeEventListener('pointerdown', unlock);
    window.removeEventListener('keydown', unlock);
  };
}

function playTone(ctx: AudioContext, freq: number, startOffset: number, duration: number, type: OscillatorType, peakGain: number) {
  const osc = ctx.createOscillator();
  const gain = ctx.createGain();
  const t0 = ctx.currentTime + startOffset;
  osc.type = type;
  osc.frequency.setValueAtTime(freq, t0);
  gain.gain.setValueAtTime(0.0001, t0);
  gain.gain.exponentialRampToValueAtTime(peakGain, t0 + 0.015);
  gain.gain.exponentialRampToValueAtTime(0.0001, t0 + duration);
  osc.connect(gain);
  gain.connect(ctx.destination);
  osc.start(t0);
  osc.stop(t0 + duration + 0.02);
}

export function playAlertSound(kind: AlertKind): void {
  if (isSoundMuted()) return;
  const ctx = getContext();
  if (!ctx) return;
  if (ctx.state === 'suspended') {
    void ctx.resume().then(() => {
      if (ctx.state === 'running' || unlocked) triggerNotes(ctx, kind);
    }).catch(() => {});
    return;
  }
  triggerNotes(ctx, kind);
}

function triggerNotes(ctx: AudioContext, kind: AlertKind) {
  switch (kind) {
    case 'completed':
      // Pleasant ascending triad (C5 - E5 - G5)
      playTone(ctx, 523.25, 0.0, 0.14, 'sine', 0.11);
      playTone(ctx, 659.25, 0.09, 0.14, 'sine', 0.11);
      playTone(ctx, 783.99, 0.18, 0.22, 'sine', 0.12);
      break;
    case 'question':
      // Attentive double ping (A4 - D5)
      playTone(ctx, 440.0, 0.0, 0.14, 'triangle', 0.12);
      playTone(ctx, 587.33, 0.14, 0.22, 'triangle', 0.13);
      break;
    case 'network':
      // Network warning (E4 - C4)
      playTone(ctx, 329.63, 0.0, 0.16, 'triangle', 0.12);
      playTone(ctx, 261.63, 0.15, 0.24, 'triangle', 0.12);
      break;
    case 'stopped':
      // Stopped / failed alert (D4 - A3)
      playTone(ctx, 293.66, 0.0, 0.16, 'sine', 0.13);
      playTone(ctx, 220.0, 0.15, 0.25, 'sine', 0.13);
      break;
  }
}
