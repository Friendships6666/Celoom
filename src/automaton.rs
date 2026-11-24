//! 5-color RGB-blending excitable cellular wave automaton driven by live AI tool & subagent state.
//!
//! Each tool category injects its own color (Red, Orange, Blue, Green, Purple) and
//! distinct geometric motion pattern (radial bursts, diamond crystals, scanning beams,
//! interference rings, orbiting spirals). When wavefronts collide, their RGB channels
//! blend naturally across neighbors while decaying monotonically so the field never freezes.

use serde::Serialize;

pub const KIND_SHELL: u8 = 1 << 0; // Red: bash, job
pub const KIND_EDIT: u8 = 1 << 1; // Orange: write, edit
pub const KIND_READ: u8 = 1 << 2; // Blue: read, glob, grep, read_image
pub const KIND_WEB: u8 = 1 << 3; // Green: web_search, web_fetch, dag
pub const KIND_AGENT: u8 = 1 << 4; // Purple: subagent, computer, ask_user, goal

const COLOR_RED: [u8; 3] = [245, 62, 82];
const COLOR_ORANGE: [u8; 3] = [255, 148, 38];
const COLOR_BLUE: [u8; 3] = [56, 152, 255];
const COLOR_GREEN: [u8; 3] = [34, 216, 138];
const COLOR_PURPLE: [u8; 3] = [188, 86, 255];
const PALETTE_5: [[u8; 3]; 5] = [
    COLOR_RED,
    COLOR_ORANGE,
    COLOR_BLUE,
    COLOR_GREEN,
    COLOR_PURPLE,
];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AutomatonMetrics {
    pub active_sessions: usize,
    pub active_tools: usize,
    pub active_subagents: usize,
    pub tool_kinds: u8,
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
    pub tool_kinds: u8,
    pub alive_cells: usize,
    pub energy: f32,
    /// Compact hex string of length `cols * rows * 6` (`rrggbb` per cell).
    pub cells: String,
}

pub struct Automaton {
    cols: usize,
    rows: usize,
    tick: u64,
    seed: u64,
    grid: Vec<[u8; 3]>,
    scratch: Vec<[u8; 3]>,
}

impl Automaton {
    pub fn new() -> Self {
        let (cols, rows) = (28, 16);
        let mut me = Self {
            cols,
            rows,
            tick: 0,
            seed: 0x9E37_79B9_7F4A_7C15,
            grid: vec![[0, 0, 0]; cols * rows],
            scratch: vec![[0, 0, 0]; cols * rows],
        };
        me.seed_radial(cols / 3, rows / 2, COLOR_BLUE, 2);
        me.seed_radial((cols * 2) / 3, rows / 2, COLOR_PURPLE, 2);
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
        let kinds = metrics.tool_kinds;

        // Tool-specific dynamic motion & color patterns:
        if (kinds & KIND_SHELL) != 0 && self.tick % 2 == 0 {
            // Red: crisp radial shockwave bursts at random points.
            let rx = (self.next_rand() as usize) % cols;
            let ry = (self.next_rand() as usize) % rows;
            self.seed_radial(rx, ry, COLOR_RED, 2);
        }
        if (kinds & KIND_EDIT) != 0 && self.tick % 2 == 1 {
            // Orange: constructive diamond crystal ripples sweeping across center.
            let angle = self.tick as f64 * 0.34;
            let cx = ((cols as f64 * 0.5) + angle.cos() * (cols as f64 * 0.28)) as usize;
            let cy = ((rows as f64 * 0.5) + angle.sin() * (rows as f64 * 0.24)) as usize;
            self.seed_diamond(cx % cols, cy % rows, COLOR_ORANGE, 2);
        }
        if (kinds & KIND_READ) != 0 {
            // Blue: fast diagonal scanning sweep beam.
            let sx = (self.tick as usize * 3) % cols;
            let sy = (self.tick as usize) % rows;
            self.seed_radial(sx, sy, COLOR_BLUE, 1);
            self.seed_radial((cols - 1 - sx) % cols, (rows - 1 - sy) % rows, COLOR_BLUE, 1);
        }
        if (kinds & KIND_WEB) != 0 {
            // Green: twin counter-rotating interference rings (web/DAG pipelines).
            let a1 = self.tick as f64 * 0.26;
            let a2 = -a1;
            let x1 = ((cols as f64 * 0.5) + a1.cos() * (cols as f64 * 0.32)) as usize;
            let y1 = ((rows as f64 * 0.5) + a1.sin() * (rows as f64 * 0.32)) as usize;
            let x2 = ((cols as f64 * 0.5) + a2.cos() * (cols as f64 * 0.22)) as usize;
            let y2 = ((rows as f64 * 0.5) + a2.sin() * (rows as f64 * 0.22)) as usize;
            self.seed_radial(x1 % cols, y1 % rows, COLOR_GREEN, 2);
            self.seed_radial(x2 % cols, y2 % rows, COLOR_GREEN, 1);
        }
        if (kinds & KIND_AGENT) != 0 || metrics.active_subagents > 0 {
            // Purple: multi-cluster orbiting spiral blooms for subagents/autonomy.
            let clusters = metrics.active_subagents.clamp(1, 5);
            for k in 0..clusters {
                let angle = (k as f64 / clusters as f64) * std::f64::consts::TAU
                    + (self.tick as f64 * 0.29);
                let cx = ((cols as f64 * 0.5) + angle.cos() * (cols as f64 * 0.30)) as usize;
                let cy = ((rows as f64 * 0.5) + angle.sin() * (rows as f64 * 0.30)) as usize;
                self.seed_radial(cx % cols, cy % rows, COLOR_PURPLE, 2);
            }
        }

        if kinds == 0 && metrics.active_subagents == 0 {
            if metrics.active_sessions > 0 {
                // Thinking: twin harmonic waves mixing Blue + Purple + subtle Green.
                if self.tick % 3 == 0 {
                    let a = self.tick as f64 * 0.20;
                    let x1 = ((cols as f64 * 0.5) + a.cos() * (cols as f64 * 0.26)) as usize;
                    let y1 = ((rows as f64 * 0.5) + a.sin() * (rows as f64 * 0.24)) as usize;
                    let c = PALETTE_5[((self.tick / 6) as usize % 3) + 2]; // Blue/Green/Purple
                    self.seed_radial(x1 % cols, y1 % rows, c, 1);
                }
            } else if self.tick % 5 == 0 {
                // Idle: calm multi-color pacemakers rotating slowly through Red/Orange/Blue/Green/Purple.
                let a = self.tick as f64 * 0.13;
                let cx = ((cols as f64 * 0.5) + a.cos() * (cols as f64 * 0.28)) as usize;
                let cy = ((rows as f64 * 0.5) + a.sin() * (rows as f64 * 0.28)) as usize;
                let c = PALETTE_5[((self.tick / 10) as usize) % PALETTE_5.len()];
                self.seed_radial(cx % cols, cy % rows, c, 1);
            }
        }

        let decay: u8 = match mode {
            "swarm" => 14,
            "tooling" => 16,
            "thinking" => 18,
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
                let cur_max = cur[0].max(cur[1]).max(cur[2]);

                let next = if cur_max > 0 {
                    // Decay proportionally across RGB channels so hue stays intact as it fades.
                    let nr = cur[0].saturating_sub(decay);
                    let ng = cur[1].saturating_sub(decay);
                    let nb = cur[2].saturating_sub(decay);
                    [nr, ng, nb]
                } else {
                    let neighbors = [
                        self.grid[y_up * cols + x],
                        self.grid[y_dn * cols + x],
                        self.grid[row_off + x_lt],
                        self.grid[row_off + x_rt],
                    ];
                    let mut excited = 0u8;
                    let mut sum_r = 0u16;
                    let mut sum_g = 0u16;
                    let mut sum_b = 0u16;
                    for n in neighbors {
                        let n_max = n[0].max(n[1]).max(n[2]);
                        if n_max >= 145 {
                            excited += 1;
                            sum_r += n[0] as u16;
                            sum_g += n[1] as u16;
                            sum_b += n[2] as u16;
                        }
                    }
                    if excited >= threshold {
                        // Blend excited neighbor colors together with slight wave attenuation.
                        let div = excited as u16;
                        [
                            ((sum_r / div) * 235 / 255) as u8,
                            ((sum_g / div) * 235 / 255) as u8,
                            ((sum_b / div) * 235 / 255) as u8,
                        ]
                    } else {
                        [0, 0, 0]
                    }
                };

                if next[0] > 0 || next[1] > 0 || next[2] > 0 {
                    alive_cells += 1;
                }
                self.scratch[idx] = next;
            }
        }

        std::mem::swap(&mut self.grid, &mut self.scratch);

        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut cells = String::with_capacity(cols * rows * 6);
        for &[r, g, b] in &self.grid {
            for c in [r, g, b] {
                cells.push(HEX[(c >> 4) as usize] as char);
                cells.push(HEX[(c & 0x0f) as usize] as char);
            }
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
            tool_kinds: kinds,
            alive_cells,
            energy,
            cells,
        }
    }

    fn resize(&mut self, new_cols: usize, new_rows: usize) {
        let mut next = vec![[0, 0, 0]; new_cols * new_rows];
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
        self.scratch = vec![[0, 0, 0]; new_cols * new_rows];
    }

    fn blend_cell(&mut self, x: usize, y: usize, color: [u8; 3]) {
        let idx = y * self.cols + x;
        let cur = self.grid[idx];
        // Blend with existing color if active, or take stronger channel mix.
        if cur[0] == 0 && cur[1] == 0 && cur[2] == 0 {
            self.grid[idx] = color;
        } else {
            self.grid[idx] = [
                ((cur[0] as u16 + color[0] as u16) / 2).max(cur[0].max(color[0]) as u16 * 3 / 4) as u8,
                ((cur[1] as u16 + color[1] as u16) / 2).max(cur[1].max(color[1]) as u16 * 3 / 4) as u8,
                ((cur[2] as u16 + color[2] as u16) / 2).max(cur[2].max(color[2]) as u16 * 3 / 4) as u8,
            ];
        }
    }

    fn seed_radial(&mut self, cx: usize, cy: usize, color: [u8; 3], radius: usize) {
        let cols = self.cols;
        let rows = self.rows;
        for dy in 0..=radius {
            for dx in 0..=radius {
                if dx + dy > radius {
                    continue;
                }
                let atten = 255u16.saturating_sub(((dx + dy) * 32) as u16);
                let c = [
                    ((color[0] as u16 * atten) / 255) as u8,
                    ((color[1] as u16 * atten) / 255) as u8,
                    ((color[2] as u16 * atten) / 255) as u8,
                ];
                for (sx, sy) in [
                    ((cx + dx) % cols, (cy + dy) % rows),
                    ((cx + cols - (dx % cols)) % cols, (cy + dy) % rows),
                    ((cx + dx) % cols, (cy + rows - (dy % rows)) % rows),
                    ((cx + cols - (dx % cols)) % cols, (cy + rows - (dy % rows)) % rows),
                ] {
                    self.blend_cell(sx, sy, c);
                }
            }
        }
    }

    fn seed_diamond(&mut self, cx: usize, cy: usize, color: [u8; 3], radius: usize) {
        let cols = self.cols;
        let rows = self.rows;
        for d in 0..=radius {
            for (sx, sy) in [
                ((cx + d) % cols, cy),
                ((cx + cols - (d % cols)) % cols, cy),
                (cx, (cy + d) % rows),
                (cx, (cy + rows - (d % rows)) % rows),
            ] {
                self.blend_cell(sx, sy, color);
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
        assert_eq!(idle.cells.len(), idle.cols * idle.rows * 6);
        assert!(idle.cells.bytes().all(|b| b.is_ascii_hexdigit()));

        let tooling = automaton.step(AutomatonMetrics {
            active_sessions: 1,
            active_tools: 3,
            active_subagents: 0,
            tool_kinds: KIND_SHELL | KIND_WEB,
        });
        assert_eq!(tooling.mode, "tooling");
        assert!(tooling.cols > idle.cols);
        assert_eq!(tooling.cells.len(), tooling.cols * tooling.rows * 6);

        let swarm = automaton.step(AutomatonMetrics {
            active_sessions: 3,
            active_tools: 2,
            active_subagents: 2,
            tool_kinds: KIND_AGENT | KIND_READ,
        });
        assert_eq!(swarm.mode, "swarm");
        assert!(swarm.cols > tooling.cols);
        assert_eq!(swarm.cells.len(), swarm.cols * swarm.rows * 6);
    }
}
