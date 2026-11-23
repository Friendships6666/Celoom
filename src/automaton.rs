//! Excitable 256-color cyclic cellular wave automaton driven by live AI work state.
//!
//! Unlike static Conway life rules that can freeze into 2x2 blocks, every active
//! cell advances monotonically through a multi-hue excitable wave cycle (`0..=255`),
//! producing continuous rhythmic ripples, spirals, and blooms at very low CPU cost.

use serde::Serialize;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AutomatonMetrics {
    pub active_sessions: usize,
    pub active_tools: usize,
    pub active_subagents: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct AutomatonFrame {
    pub cols: usize,
    pub rows: usize,
    pub tick: u64,
    pub mode: &'static str,
    pub active_sessions: usize,
    pub active_tools: usize,
    pub active_subagents: usize,
    pub alive_cells: usize,
    pub energy: f32,
    /// Compact hex string of length `cols * rows * 2` (`00`..`ff`, 256 states).
    pub cells: String,
}

pub struct Automaton {
    cols: usize,
    rows: usize,
    tick: u64,
    seed: u64,
    grid: Vec<u8>,
    scratch: Vec<u8>,
}

impl Automaton {
    pub fn new() -> Self {
        let (cols, rows) = (28, 16);
        let mut me = Self {
            cols,
            rows,
            tick: 0,
            seed: 0x9E37_79B9_7F4A_7C15,
            grid: vec![0; cols * rows],
            scratch: vec![0; cols * rows],
        };
        me.seed_wave(cols / 2, rows / 2, 220, 2);
        me
    }

    pub fn step(&mut self, metrics: AutomatonMetrics) -> AutomatonFrame {
        let (target_cols, target_rows, mode) = target_layout(metrics);
        if target_cols != self.cols || target_rows != self.rows {
            self.resize(target_cols, target_rows);
        }

        self.tick = self.tick.wrapping_add(1);
        let cols = self.cols;
        let rows = self.rows;

        // Rhythmic pacemakers ensure the field never freezes or dies out.
        if metrics.active_subagents > 0 {
            let clusters = metrics.active_subagents.min(6);
            for k in 0..clusters {
                let angle = (k as f64 / clusters as f64) * std::f64::consts::TAU
                    + (self.tick as f64 * 0.28);
                let cx = ((cols as f64 * 0.5) + angle.cos() * (cols as f64 * 0.30)) as usize;
                let cy = ((rows as f64 * 0.5) + angle.sin() * (rows as f64 * 0.30)) as usize;
                let phase = 210u8.saturating_add(((self.tick * 17 + k as u64 * 31) % 45) as u8);
                self.seed_wave(cx % cols, cy % rows, phase, 1);
            }
        }
        if metrics.active_tools > 0 {
            let bursts = metrics.active_tools.min(4);
            for b in 0..bursts {
                if (self.tick + b as u64) % 2 == 0 {
                    let rx = (self.next_rand() as usize) % cols;
                    let ry = (self.next_rand() as usize) % rows;
                    let phase = 200u8.saturating_add(((self.tick * 19 + b as u64 * 29) % 55) as u8);
                    self.seed_wave(rx, ry, phase, 1);
                }
            }
        } else if metrics.active_sessions > 0 {
            if self.tick % 3 == 0 {
                let angle = self.tick as f64 * 0.22;
                let cx = ((cols as f64 * 0.5) + angle.cos() * (cols as f64 * 0.22)) as usize;
                let cy = ((rows as f64 * 0.5) + angle.sin() * (rows as f64 * 0.22)) as usize;
                self.seed_wave(cx % cols, cy % rows, 215, 1);
            }
        } else if self.tick % 6 == 0 {
            // Gentle orbiting idle pacemaker keeps calm rhythmic ripples flowing.
            let angle = self.tick as f64 * 0.14;
            let cx = ((cols as f64 * 0.5) + angle.cos() * (cols as f64 * 0.28)) as usize;
            let cy = ((rows as f64 * 0.5) + angle.sin() * (rows as f64 * 0.28)) as usize;
            self.seed_wave(cx % cols, cy % rows, 196, 1);
        }

        // Cyclic excitable wave transition:
        // - Wavefront band: 170..=255 (triggers resting 0 neighbors into wave)
        // - Refractory/color cascade: decays monotonically every tick so no cell can ever freeze!
        let step_decay: u8 = match mode {
            "swarm" => 13,
            "tooling" => 15,
            "thinking" => 17,
            _ => 21,
        };
        let threshold: u8 = if mode == "idle" { 2 } else { 1 };

        let mut alive_cells = 0usize;
        for y in 0..rows {
            let y_up = if y == 0 { rows - 1 } else { y - 1 };
            let y_dn = if y + 1 == rows { 0 } else { y + 1 };
            let row_off = y * cols;
            for x in 0..cols {
                let x_lt = if x == 0 { cols - 1 } else { x - 1 };
                let x_rt = if x + 1 == cols { 0 } else { x + 1 };

                let idx = row_off + x;
                let cur = self.grid[idx];

                let next = if cur > 0 {
                    // Always advance downward through the 256-color spectrum -> 0 (never freezes).
                    cur.saturating_sub(step_decay)
                } else {
                    let neighbors = [
                        self.grid[y_up * cols + x],
                        self.grid[y_dn * cols + x],
                        self.grid[row_off + x_lt],
                        self.grid[row_off + x_rt],
                    ];
                    let mut excited = 0u8;
                    let mut peak = 0u8;
                    for n in neighbors {
                        if n >= 168 {
                            excited += 1;
                            if n > peak {
                                peak = n;
                            }
                        }
                    }
                    if excited >= threshold {
                        // Shift phase slightly on propagation for rich multi-color wave fronts.
                        let shift = ((x + y + (self.tick as usize)) & 3) as u8 * 7;
                        peak.saturating_sub(8 + shift).max(168)
                    } else {
                        0
                    }
                };

                if next > 0 {
                    alive_cells += 1;
                }
                self.scratch[idx] = next;
            }
        }

        std::mem::swap(&mut self.grid, &mut self.scratch);

        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut cells = String::with_capacity(cols * rows * 2);
        for &c in &self.grid {
            cells.push(HEX[(c >> 4) as usize] as char);
            cells.push(HEX[(c & 0x0f) as usize] as char);
        }

        let density = alive_cells as f32 / (cols * rows).max(1) as f32;
        let activity = ((metrics.active_sessions as f32 * 0.18)
            + (metrics.active_tools as f32 * 0.25)
            + (metrics.active_subagents as f32 * 0.35))
            .min(1.0);
        let energy = (density * 0.85 + activity * 0.6).clamp(0.08, 1.0);

        AutomatonFrame {
            cols,
            rows,
            tick: self.tick,
            mode,
            active_sessions: metrics.active_sessions,
            active_tools: metrics.active_tools,
            active_subagents: metrics.active_subagents,
            alive_cells,
            energy,
            cells,
        }
    }

    fn resize(&mut self, new_cols: usize, new_rows: usize) {
        let mut next = vec![0u8; new_cols * new_rows];
        let copy_cols = self.cols.min(new_cols);
        let copy_rows = self.rows.min(new_rows);
        for y in 0..copy_rows {
            let src_start = y * self.cols;
            let dst_start = y * new_cols;
            next[dst_start..dst_start + copy_cols]
                .copy_from_slice(&self.grid[src_start..src_start + copy_cols]);
        }
        self.cols = new_cols;
        self.rows = new_rows;
        self.grid = next;
        self.scratch = vec![0u8; new_cols * new_rows];
    }

    fn seed_wave(&mut self, cx: usize, cy: usize, state: u8, radius: usize) {
        let cols = self.cols;
        let rows = self.rows;
        for dy in 0..=radius {
            for dx in 0..=radius {
                if dx + dy > radius {
                    continue;
                }
                let val = state.saturating_sub(((dx + dy) * 18) as u8);
                if val == 0 {
                    continue;
                }
                for (sx, sy) in [
                    ((cx + dx) % cols, (cy + dy) % rows),
                    ((cx + cols - (dx % cols)) % cols, (cy + dy) % rows),
                    ((cx + dx) % cols, (cy + rows - (dy % rows)) % rows),
                    ((cx + cols - (dx % cols)) % cols, (cy + rows - (dy % rows)) % rows),
                ] {
                    self.grid[sy * cols + sx] = self.grid[sy * cols + sx].max(val);
                }
            }
        }
    }

    fn next_rand(&mut self) -> u64 {
        let mut x = self.seed;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.seed = x;
        x
    }
}

fn target_layout(metrics: AutomatonMetrics) -> (usize, usize, &'static str) {
    if metrics.active_subagents > 0 {
        let extra = (metrics.active_subagents.min(5)) * 2;
        (40 + extra, 22 + extra / 2, "swarm")
    } else if metrics.active_tools > 0 {
        let extra = (metrics.active_tools.min(5)) * 2;
        (34 + extra, 19 + extra / 2, "tooling")
    } else if metrics.active_sessions > 0 {
        (32, 18, "thinking")
    } else {
        (28, 16, "idle")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_scales_and_encodes_valid_states() {
        let mut automaton = Automaton::new();
        let idle = automaton.step(AutomatonMetrics::default());
        assert_eq!(idle.mode, "idle");
        assert_eq!(idle.cols, 28);
        assert_eq!(idle.rows, 16);
        assert_eq!(idle.cells.len(), idle.cols * idle.rows * 2);
        assert!(idle.cells.bytes().all(|b| b.is_ascii_hexdigit()));

        let tooling = automaton.step(AutomatonMetrics {
            active_sessions: 1,
            active_tools: 3,
            active_subagents: 0,
        });
        assert_eq!(tooling.mode, "tooling");
        assert!(tooling.cols > idle.cols);
        assert_eq!(tooling.cells.len(), tooling.cols * tooling.rows * 2);

        let swarm = automaton.step(AutomatonMetrics {
            active_sessions: 3,
            active_tools: 2,
            active_subagents: 2,
        });
        assert_eq!(swarm.mode, "swarm");
        assert!(swarm.cols > tooling.cols);
        assert_eq!(swarm.cells.len(), swarm.cols * swarm.rows * 2);
    }
}
