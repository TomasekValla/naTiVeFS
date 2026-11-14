//! TVFS colours (dark and light) with a 256-colour fallback for terminals without truecolor.

use ratatui::style::Color;

type Rgb = (u8, u8, u8);

#[derive(Debug, Clone)]
pub struct Theme {
    pub light: bool,
    truecolor: bool,
    pub bg: Color,
    pub text: Color,
    pub dim: Color,
    pub accent: Color,
    pub accent2: Color,
    pub good: Color,
    pub warn: Color,
    pub bad: Color,
    pub border: Color,
    pub sel_bg: Color,
    grad: (Rgb, Rgb),
}

pub fn detect_truecolor() -> bool {
    let env_has = |k: &str, v: &[&str]| std::env::var(k).is_ok_and(|x| v.iter().any(|v| x.to_ascii_lowercase().contains(v)));
    cfg!(windows)
        || env_has("COLORTERM", &["truecolor", "24bit"])
        || std::env::var_os("WT_SESSION").is_some()
        || env_has("TERM_PROGRAM", &["vscode", "iterm", "wezterm", "ghostty"])
        || env_has("TERM", &["direct", "kitty", "alacritty"])
}

/// Nearest xdg 256-colour index (colour cube or grey ramp).
pub fn to_256(c: Rgb) -> u8 {
    let (r, g, b) = (c.0 as i32, c.1 as i32, c.2 as i32);
    let cube = |v: i32| -> i32 {
        if v < 48 {
            0
        } else if v < 115 {
            1
        } else {
            (v - 35) / 40
        }
    };
    let level = |i: i32| if i == 0 { 0 } else { 55 + 40 * i };
    let (ri, gi, bi) = (cube(r), cube(g), cube(b));
    let cube_rgb = (level(ri), level(gi), level(bi));
    let cube_idx = 16 + 36 * ri + 6 * gi + bi;
    let avg = (r + g + b) / 3;
    let gi2 = ((avg - 8) / 10).clamp(0, 23);
    let grey = 8 + 10 * gi2;
    let dist = |a: (i32, i32, i32)| (a.0 - r).pow(2) + (a.1 - g).pow(2) + (a.2 - b).pow(2);
    if dist((grey, grey, grey)) < dist(cube_rgb) {
        (232 + gi2) as u8
    } else {
        cube_idx as u8
    }
}

impl Theme {
    pub fn new(light: bool, truecolor: bool) -> Self {
        let c = |rgb: Rgb| if truecolor { Color::Rgb(rgb.0, rgb.1, rgb.2) } else { Color::Indexed(to_256(rgb)) };
        if light {
            Self {
                light,
                truecolor,
                bg: c((0xee, 0xf6, 0xf5)),
                text: c((0x12, 0x12, 0x1a)),
                dim: c((0x52, 0x58, 0x70)),
                accent: c((0x7c, 0x4d, 0xff)),
                accent2: c((0x00, 0xa8, 0x86)),
                good: c((0x00, 0x8a, 0x6e)),
                warn: c((0xb0, 0x7a, 0x00)),
                bad: c((0xe0, 0x52, 0x4a)),
                border: c((0xb0, 0xc4, 0xc2)),
                sel_bg: c((0xd6, 0xe6, 0xe4)),
                grad: ((0x00, 0xa8, 0x86), (0x7c, 0x4d, 0xff)),
            }
        } else {
            Self {
                light,
                truecolor,
                bg: c((0x03, 0x07, 0x0a)),
                text: c((0xff, 0xff, 0xff)),
                dim: c((0x9a, 0xa6, 0xbd)),
                accent: c((0xb0, 0x83, 0xff)),
                accent2: c((0x2f, 0xff, 0xc4)),
                good: c((0x2f, 0xff, 0xc4)),
                warn: c((0xff, 0xd8, 0x66)),
                bad: c((0xff, 0x6b, 0x6b)),
                border: c((0x26, 0x30, 0x42)),
                sel_bg: c((0x14, 0x1c, 0x28)),
                grad: ((0x00, 0xe6, 0xbd), (0x84, 0x55, 0xff)),
            }
        }
    }

    /// Point of the TVFS gradient, `t` in 0..=1.
    pub fn grad(&self, t: f32) -> Color {
        self.grad_shade(t, 1.0)
    }

    /// The gradient at `t`, mixed towards the background by `1 - k` (k = 1: full colour). Used for the logo's shadow.
    pub fn grad_shade(&self, t: f32, k: f32) -> Color {
        let t = t.clamp(0.0, 1.0);
        let k = k.clamp(0.0, 1.0);
        let bg: Rgb = if self.light { (0xee, 0xf6, 0xf5) } else { (0x03, 0x07, 0x0a) };
        let mix = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
        let rgb = (mix(self.grad.0 .0, self.grad.1 .0), mix(self.grad.0 .1, self.grad.1 .1), mix(self.grad.0 .2, self.grad.1 .2));
        let toward = |c: u8, b: u8| (b as f32 + (c as f32 - b as f32) * k).round() as u8;
        let rgb = (toward(rgb.0, bg.0), toward(rgb.1, bg.1), toward(rgb.2, bg.2));
        if self.truecolor {
            Color::Rgb(rgb.0, rgb.1, rgb.2)
        } else {
            Color::Indexed(to_256(rgb))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_maps_known_colours() {
        assert_eq!(to_256((0, 0, 0)), 16);
        assert_eq!(to_256((255, 255, 255)), 231);
        let g = to_256((0x9a, 0xa6, 0xbd));
        assert!(g >= 16);
    }

    #[test]
    fn gradient_ends_match_the_palette() {
        let t = Theme::new(false, true);
        assert_eq!(t.grad(0.0), Color::Rgb(0x00, 0xe6, 0xbd));
        assert_eq!(t.grad(1.0), Color::Rgb(0x84, 0x55, 0xff));
        assert_eq!(t.bg, Color::Rgb(3, 7, 10));
    }
}
