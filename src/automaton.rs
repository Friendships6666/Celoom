//! High-performance multi-state cellular automaton driven by live AI work state.
//!
//! Cell states (`u8` in `0..=7`):
//! - `0`: dead / empty
//! - `1`: living cellular bloom
//! - `2`: active tool energy cell
//! - `3`: active subagent swarm cell
//! - `4..=7`: decaying bioluminescent trails (`4` -> `5` -> `6` -> `7` -> `0`)

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
    /// Compact ASCII string of length `cols * rows`, each byte in `'0'..='7'`.
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
        let (cols, rows) = (44, 26);
        let mut me = Self {
            cols,
            rows,
            tick: 0,
            seed: 0x9E37_79B9_7F4A_7C15,
            grid: vec![0; cols * rows],
            scratch: vec![0; cols * rows],
        };
        me.seed_bloom(cols / 2, rows / 2, 1, 5);
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

        // Inject energy proportional to live AI activity so the automaton
        // continuously responds to tools, subagents, and thinking state.
        if metrics.active_subagents > 0 {
            let clusters = metrics.active_subagents.min(8);
            for k in 0..clusters {
                if self.tick.wrapping_add(k as u64) % 3 == 0 {
                    let angle = (k as f64 / clusters as f64) * std::f64::consts::TAU
                        + (self.tick as f64 * 0.09);
                    let cx = ((cols as f64 * 0.5) + angle.cos() * (cols as f64 * 0.28)) as usize;
                    let cy = ((rows as f64 * 0.5) + angle.sin() * (rows as f64 * 0.28)) as usize;
                    self.seed_bloom(cx % cols, cy % rows, 3, 2);
                }
            }
        }
        if metrics.active_tools > 0 {
            let bursts = metrics.active_tools.min(6);
            for b in 0..bursts {
                if self.tick.wrapping_add(b as u64) % 2 == 0 {
                    let rx = (self.next_rand() as usize) % cols;
                    let ry = (self.next_rand() as usize) % rows;
                    self.seed_bloom(rx, ry, 2, 2);
                }
            }
        } else if metrics.active_sessions > 0 {
            if self.tick % 4 == 0 {
                let cx = cols / 2 + ((self.next_rand() as usize) % 7).saturating_sub(3);
                let cy = rows / 2 + ((self.next_rand() as usize) % 5).saturating_sub(2);
                self.seed_bloom(cx % cols, cy % rows, 1, 2);
            }
        } else if self.tick % 10 == 0 {
            let rx = (self.next_rand() as usize) % cols;
            let ry = (self.next_rand() as usize) % rows;
            self.seed_bloom(rx, ry, 1, 2);
        }

        let mut alive_cells = 0usize;
        let active_boost = metrics.active_tools + metrics.active_subagents * 2;
        for y in 0..rows {
            let y_up = if y == 0 { rows - 1 } else { y - 1 };
            let y_dn = if y + 1 == rows { 0 } else { y + 1 };
            for x in 0..cols {
                let x_lt = if x == 0 { cols - 1 } else { x - 1 };
                let x_rt = if x + 1 == cols { 0 } else { x + 1 };

                let idx = y * cols + x;
                let cur = self.grid[idx];

                let neighbors = [
                    self.grid[y_up * cols + x_lt],
                    self.grid[y_up * cols + x],
                    self.grid[y_up * cols + x_rt],
                    self.grid[y * cols + x_lt],
                    self.grid[y * cols + x_rt],
                    self.grid[y_dn * cols + x_lt],
                    self.grid[y_dn * cols + x],
                    self.grid[y_dn * cols + x_rt],
                ];

                let mut live_count = 0u8;
                let mut tool_count = 0u8;
                let mut sub_count = 0u8;
                for &n in &neighbors {
                    if matches!(n, 1..=3) {
                        live_count += 1;
                        if n == 2 {
                            tool_count += 1;
                        } else if n == 3 {
                            sub_count += 1;
                        }
                    }
                }

                let next = match cur {
                    1..=3 => {
                        if live_count == 2 || live_count == 3 || (active_boost > 0 && live_count == 4) {
                            if sub_count > 0 && metrics.active_subagents > 0 {
                                3
                            } else if tool_count > 0 && metrics.active_tools > 0 {
                                2
                            } else {
                                cur
                            }
                        } else {
                            4
                        }
                    }
                    0 | 4..=7 => {
                        // HighLife-inspired birth (3 or 6 neighbors) keeps intricate gliders/blooms alive.
                        if live_count == 3 || (live_count == 6 && active_boost > 0) {
                            if sub_count >= tool_count && sub_count > 0 && metrics.active_subagents > 0 {
                                3
                            } else if tool_count > 0 && metrics.active_tools > 0 {
                                2
                            } else {
                                1
                            }
                        } else if cur >= 4 && cur < 7 {
                            cur + 1
                        } else {
                            0
                        }
                    }
                    _ => 0,
                };

                if matches!(next, 1..=3) {
                    alive_cells += 1;
                }
                self.scratch[idx] = next;
            }
        }

        std::mem::swap(&mut self.grid, &mut self.scratch);

        // Ensure the background never goes completely sterile when idle.
        if alive_cells < (cols * rows) / 40 {
            let rx = (self.next_rand() as usize) % cols;
            let ry = (self.next_rand() as usize) % rows;
            self.seed_bloom(rx, ry, 1, 3);
        }

        let mut cells = String::with_capacity(cols * rows);
        for &cell in &self.grid {
            cells.push((b'0' + cell.min(7)) as char);
        }

        let density = alive_cells as f32 / (cols * rows).max(1) as f32;
        let activity = ((metrics.active_sessions as f32 * 0.2)
            + (metrics.active_tools as f32 * 0.25)
            + (metrics.active_subagents as f32 * 0.35))
            .min(1.0);
        let energy = (density * 1.4 + activity * 0.65).clamp(0.08, 1.0);

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

    fn seed_bloom(&mut self, cx: usize, cy: usize, state: u8, radius: usize) {
        let cols = self.cols;
        let rows = self.rows;
        for dy in 0..=radius {
            for dx in 0..=radius {
                if dx + dy > radius + 1 {
                    continue;
                }
                if self.next_rand() & 1 == 0 || (dx == 0 && dy == 0) {
                    for (sx, sy) in [
                        ((cx + dx) % cols, (cy + dy) % rows),
                        ((cx + cols - (dx % cols)) % cols, (cy + dy) % rows),
                        ((cx + dx) % cols, (cy + rows - (dy % rows)) % rows),
                        ((cx + cols - (dx % cols)) % cols, (cy + rows - (dy % rows)) % rows),
                    ] {
                        self.grid[sy * cols + sx] = state;
                    }
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
        let extra = (metrics.active_subagents.min(6)) * 2;
        (72 + extra, 42 + extra / 2, "swarm")
    } else if metrics.active_tools > 0 {
        let extra = (metrics.active_tools.min(6)) * 2;
        (60 + extra, 35 + extra / 2, "tooling")
    } else if metrics.active_sessions > 0 {
        (54, 31, "thinking")
    } else {
        (44, 26, "idle")
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
        assert_eq!(idle.cols, 44);
        assert_eq!(idle.rows, 26);
        assert_eq!(idle.cells.len(), idle.cols * idle.rows);
        assert!(idle.cells.bytes().all(|b| (b'0'..=b'7').contains(&b)));

        let tooling = automaton.step(AutomatonMetrics {
            active_sessions: 1,
            active_tools: 3,
            active_subagents: 0,
        });
        assert_eq!(tooling.mode, "tooling");
        assert!(tooling.cols > idle.cols);
        assert_eq!(tooling.cells.len(), tooling.cols * tooling.rows);

        let swarm = automaton.step(AutomatonMetrics {
            active_sessions: 3,
            active_tools: 2,
            active_subagents: 2,
        });
        assert_eq!(swarm.mode, "swarm");
        assert!(swarm.cols > tooling.cols);
        assert_eq!(swarm.cells.len(), swarm.cols * swarm.rows);
    }
}
