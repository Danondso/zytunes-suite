use ratatui::style::Color;

use super::theme::AccentAnim;

// Precomputed sine table for 40 steps over [0, 2*pi].
// Values mapped to [0.0, 1.0] via (sin(x) + 1) / 2.
const SINE_TABLE: [f32; 40] = [
    0.500, 0.578, 0.655, 0.727, 0.794, 0.854, 0.905, 0.946, 0.976, 0.994, 1.000, 0.994, 0.976,
    0.946, 0.905, 0.854, 0.794, 0.727, 0.655, 0.578, 0.500, 0.422, 0.345, 0.273, 0.206, 0.146,
    0.095, 0.054, 0.024, 0.006, 0.000, 0.006, 0.024, 0.054, 0.095, 0.146, 0.206, 0.273, 0.345,
    0.422,
];

/// Returns a color that pulses between dim (40% brightness) and full brightness.
/// `period` is in frames (e.g. 40 frames = 2 seconds at 20fps).
pub fn pulse_color(base: Color, frame: usize, period: usize) -> Color {
    if let Color::Rgb(r, g, b) = base {
        let period = period.max(1);
        let idx = (frame % period) * 40 / period;
        let t = SINE_TABLE[idx.min(39)];
        // Oscillate brightness between 0.7 and 1.0 (subtle glow, no white flash)
        let m = 0.7 + 0.3 * t;
        Color::Rgb(
            (r as f32 * m) as u8,
            (g as f32 * m) as u8,
            (b as f32 * m) as u8,
        )
    } else {
        base
    }
}

/// Rotate the hue of an RGB color. Non-RGB colors pass through unchanged.
fn hue_cycle(base: Color, frame: usize, period: usize) -> Color {
    if let Color::Rgb(r, g, b) = base {
        let (r, g, b) = (r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0);
        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        let delta = max - min;
        let l = (max + min) / 2.0;
        if delta < 0.001 {
            return base; // achromatic, nothing to rotate
        }
        let s = if l < 0.5 {
            delta / (max + min)
        } else {
            delta / (2.0 - max - min)
        };
        let h = if (max - r).abs() < 0.001 {
            ((g - b) / delta).rem_euclid(6.0) * 60.0
        } else if (max - g).abs() < 0.001 {
            ((b - r) / delta + 2.0) * 60.0
        } else {
            ((r - g) / delta + 4.0) * 60.0
        };
        let period = period.max(1);
        let new_h = (h + (frame % period) as f32 * 360.0 / period as f32) % 360.0;
        let (ro, go, bo) = hsl_to_rgb(new_h, s, l);
        Color::Rgb(ro, go, bo)
    } else {
        base
    }
}

/// Interpolate between two RGB colors using a sine wave.
fn color_shift(base: Color, target: Color, frame: usize, period: usize) -> Color {
    if let (Color::Rgb(r1, g1, b1), Color::Rgb(r2, g2, b2)) = (base, target) {
        let period = period.max(1);
        let idx = (frame % period) * 40 / period;
        let t = SINE_TABLE[idx.min(39)];
        Color::Rgb(lerp_u8(r1, r2, t), lerp_u8(g1, g2, t), lerp_u8(b1, b2, t))
    } else {
        base
    }
}

fn lerp_u8(a: u8, b: u8, t: f32) -> u8 {
    (a as f32 + (b as f32 - a as f32) * t) as u8
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (u8, u8, u8) {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = l - c / 2.0;
    let (r, g, b) = match h as u32 {
        0..=59 => (c, x, 0.0),
        60..=119 => (x, c, 0.0),
        120..=179 => (0.0, c, x),
        180..=239 => (0.0, x, c),
        240..=299 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    (
        ((r + m) * 255.0) as u8,
        ((g + m) * 255.0) as u8,
        ((b + m) * 255.0) as u8,
    )
}

/// Returns the animated accent color for a theme based on its accent animation mode.
pub fn animated_accent(
    base: Color,
    secondary: Color,
    mode: AccentAnim,
    frame: usize,
    period: usize,
) -> Color {
    match mode {
        AccentAnim::None => base,
        AccentAnim::Pulse => pulse_color(base, frame, period),
        AccentAnim::HueCycle => hue_cycle(base, frame, period),
        AccentAnim::ColorShift => color_shift(base, secondary, frame, period),
    }
}

/// Returns the position of a 3-char "shine" highlight sweeping across a bar.
/// Sweeps across the full `total_width`, not just the filled portion.
/// Returns `None` if total_width is 0.
pub fn shine_offset(frame: usize, total_width: usize) -> Option<usize> {
    if total_width == 0 {
        return None;
    }
    // Sweep speed: advance 1 position every 2 frames (~10 chars/sec at 20fps)
    // Sweep across the full bar width so it looks like continuous motion
    let cycle = total_width + 3;
    let pos = (frame / 2) % cycle;
    Some(pos)
}

/// Builds a progress bar string with a sweeping shine highlight.
/// Returns (bar_string, shine_positions) where shine chars use a brighter style.
pub fn progress_bar_with_shine(filled: usize, empty: usize, frame: usize) -> Vec<(char, bool)> {
    let total = filled + empty;
    let shine_pos = shine_offset(frame, total);

    (0..total)
        .map(|i| {
            if i < filled {
                let is_shine = shine_pos.is_some_and(|sp| i >= sp.saturating_sub(1) && i <= sp + 1);
                ('=', is_shine)
            } else {
                (' ', false)
            }
        })
        .collect()
}

/// Returns a slice of `text` revealed character-by-character over time.
/// At 20fps, reveals ~2 chars per frame for a snappy typewriter effect.
pub fn typing_reveal(text: &str, elapsed_frames: usize) -> &str {
    // Reveal 1 char every 2 frames (10 chars/sec at 20fps)
    let chars_to_show = elapsed_frames / 2;
    if chars_to_show >= text.len() {
        return text;
    }
    // Find the byte boundary for the nth character
    let mut boundary = 0;
    for (i, (idx, _)) in text.char_indices().enumerate() {
        if i >= chars_to_show {
            boundary = idx;
            break;
        }
    }
    &text[..boundary]
}

/// Returns two screen lines for the device ASCII art during connection.
/// Each line must be <=12 chars. Frame counter starts at 0 when connection begins.
/// `family` is whatever we last detected (or `None` while still scanning) so
/// an iPod connect never paints "Zune" / "MTPZ Handshake".
pub fn connection_screen_lines(
    frame: usize,
    family: Option<zytunes::device::DeviceFamily>,
) -> (&'static str, &'static str) {
    // Each stage lasts ~1 second (20 frames at 20fps)
    let stage = frame / 20;
    let sub = frame % 20;
    let found_label = family.map(|f| f.label()).unwrap_or("Device");

    match stage {
        0 => {
            // Scanning with animated dots
            let dots = match (sub / 5) % 4 {
                0 => "Scanning",
                1 => "Scanning.",
                2 => "Scanning..",
                _ => "Scanning...",
            };
            (dots, "")
        }
        1 => {
            let dots = match (sub / 5) % 4 {
                0 => "Found",
                1 => "Found.",
                2 => "Found..",
                _ => "Found...",
            };
            (dots, found_label)
        }
        2 => match family {
            Some(zytunes::device::DeviceFamily::Ipod)
            | Some(zytunes::device::DeviceFamily::Gogear) => {
                let dots = match (sub / 5) % 4 {
                    0 => "Opening",
                    1 => "Opening.",
                    2 => "Opening..",
                    _ => "Opening...",
                };
                (dots, "Volume")
            }
            Some(zytunes::device::DeviceFamily::Zune) => {
                let dots = match (sub / 5) % 4 {
                    0 => "MTPZ",
                    1 => "MTPZ.",
                    2 => "MTPZ..",
                    _ => "MTPZ...",
                };
                (dots, "Handshake")
            }
            None => {
                let dots = match (sub / 5) % 4 {
                    0 => "USB",
                    1 => "USB.",
                    2 => "USB..",
                    _ => "USB...",
                };
                (dots, "Connect")
            }
        },
        _ => {
            let dots = match (sub / 5) % 4 {
                0 => "Ready",
                1 => "Ready.",
                2 => "Ready..",
                _ => "Ready...",
            };
            (dots, "Connecting")
        }
    }
}

/// Theme-specific player skin for the Now Playing panel.
pub struct PlayerSkin {
    pub play: &'static str,
    pub pause: &'static str,
    pub next: &'static str,
    pub prev: &'static str,
    pub bar_filled: char,
    pub bar_empty: char,
    /// Returns ASCII art lines for the player. `playing` and `frame` drive animations.
    pub art_fn: fn(playing: bool, frame: usize) -> Vec<&'static str>,
}

// All art: exactly 5 lines, each exactly 14 chars wide.
// Consistent width prevents centering jitter.

// --- iTunes 2004: spinning CD ---
fn art_itunes(_playing: bool, frame: usize) -> Vec<&'static str> {
    let p = (frame / 4) % 4;
    match p {
        0 => vec![
            " .--=====--. ",
            " | .---=-. | ",
            " | |  O  | | ",
            " | `---=-' | ",
            " `--=====--' ",
        ],
        1 => vec![
            " .--=====--. ",
            " | .--+--. | ",
            " | |  O  | | ",
            " | `--+--' | ",
            " `--=====--' ",
        ],
        2 => vec![
            " .--=====--. ",
            " | .--*--. | ",
            " | |  O  | | ",
            " | `--*--' | ",
            " `--=====--' ",
        ],
        _ => vec![
            " .--=====--. ",
            " | .--~--. | ",
            " | |  O  | | ",
            " | `--~--' | ",
            " `--=====--' ",
        ],
    }
}
pub static SKIN_ITUNES: PlayerSkin = PlayerSkin {
    play: "▶",
    pause: "❚❚",
    next: "▷▷",
    prev: "◁◁",
    bar_filled: '━',
    bar_empty: '─',
    art_fn: art_itunes,
};

// --- Gruvbox Dark: bouncing EQ bars ---
fn art_gruvbox_dark(_playing: bool, frame: usize) -> Vec<&'static str> {
    let p = (frame / 3) % 4;
    match p {
        0 => vec![
            " █       █   ",
            " █ █   █ █   ",
            " █ █ █ █ █   ",
            " █ █ █ █ █   ",
            " █ █ █ █ █   ",
        ],
        1 => vec![
            "   █   █     ",
            " █ █   █ █   ",
            " █ █   █ █   ",
            " █ █ █ █ █   ",
            " █ █ █ █ █   ",
        ],
        2 => vec![
            "     █       ",
            "   █ █ █     ",
            " █ █ █ █ █   ",
            " █ █ █ █ █   ",
            " █ █ █ █ █   ",
        ],
        _ => vec![
            "   █   █     ",
            " █ █ █ █ █   ",
            " █ █ █ █ █   ",
            " █ █ █ █ █   ",
            " █ █ █ █ █   ",
        ],
    }
}
pub static SKIN_GRUVBOX_DARK: PlayerSkin = PlayerSkin {
    play: "►",
    pause: "▪",
    next: "▸▸",
    prev: "◂◂",
    bar_filled: '●',
    bar_empty: '○',
    art_fn: art_gruvbox_dark,
};

// --- Gruvbox Light: same EQ bars ---
fn art_gruvbox_light(_p: bool, f: usize) -> Vec<&'static str> {
    art_gruvbox_dark(_p, f)
}
pub static SKIN_GRUVBOX_LIGHT: PlayerSkin = PlayerSkin {
    play: "►",
    pause: "■",
    next: "▸▸",
    prev: "◂◂",
    bar_filled: '◆',
    bar_empty: '◇',
    art_fn: art_gruvbox_light,
};

// --- Everforest Dark: waveform ---
fn art_everforest_dark(_playing: bool, frame: usize) -> Vec<&'static str> {
    let p = (frame / 3) % 4;
    match p {
        0 => vec![
            "              ",
            " ~   ~   ~   ~",
            "  ~ ~ ~ ~ ~ ~ ",
            "   ~   ~   ~  ",
            "              ",
        ],
        1 => vec![
            "              ",
            "~ ~   ~   ~   ",
            " ~ ~ ~ ~ ~ ~  ",
            "    ~   ~   ~ ",
            "              ",
        ],
        2 => vec![
            "              ",
            "  ~ ~   ~   ~ ",
            " ~ ~ ~ ~ ~ ~  ",
            "~   ~   ~   ~ ",
            "              ",
        ],
        _ => vec![
            "              ",
            "   ~ ~   ~   ~",
            "  ~ ~ ~ ~ ~ ~ ",
            " ~   ~   ~    ",
            "              ",
        ],
    }
}
pub static SKIN_EVERFOREST_DARK: PlayerSkin = PlayerSkin {
    play: "▶",
    pause: "||",
    next: "▷▷",
    prev: "◁◁",
    bar_filled: '▓',
    bar_empty: '░',
    art_fn: art_everforest_dark,
};

// --- Everforest Light: same waveform ---
fn art_everforest_light(_p: bool, f: usize) -> Vec<&'static str> {
    art_everforest_dark(_p, f)
}
pub static SKIN_EVERFOREST_LIGHT: PlayerSkin = PlayerSkin {
    play: "▶",
    pause: "||",
    next: "▷▷",
    prev: "◁◁",
    bar_filled: '▓',
    bar_empty: '░',
    art_fn: art_everforest_light,
};

// --- Tokyo Night: turntable with tone arm ---
fn art_tokyo_night(_playing: bool, frame: usize) -> Vec<&'static str> {
    let p = (frame / 3) % 4;
    match p {
        0 => vec![
            " .----------.",
            " | .---.  O |",
            " | | + | /  |",
            " | `---'    |",
            " `----------'",
        ],
        1 => vec![
            " .----------.",
            " | .---. O  |",
            " | | + |/   |",
            " | `---'    |",
            " `----------'",
        ],
        2 => vec![
            " .----------.",
            " | .---.O   |",
            " | | + /    |",
            " | `---'    |",
            " `----------'",
        ],
        _ => vec![
            " .----------.",
            " | .---. O  |",
            " | | + |\\   |",
            " | `---'    |",
            " `----------'",
        ],
    }
}
pub static SKIN_TOKYO_NIGHT: PlayerSkin = PlayerSkin {
    play: "▶",
    pause: "||",
    next: "▷▷",
    prev: "◁◁",
    bar_filled: '█',
    bar_empty: '░',
    art_fn: art_tokyo_night,
};

// --- IBM Mainframe: cassette reels ---
fn art_ibm(_playing: bool, frame: usize) -> Vec<&'static str> {
    let p = (frame / 3) % 4;
    match p {
        0 => vec![
            " +-----------+",
            " | /--\\ /--\\ |",
            " | |  | |  | |",
            " | \\--/ \\--/ |",
            " +===+======+=",
        ],
        1 => vec![
            " +-----------+",
            " | /--\\ /--\\ |",
            " | |- | | -| |",
            " | \\--/ \\--/ |",
            " +=+========+=",
        ],
        2 => vec![
            " +-----------+",
            " | /--\\ /--\\ |",
            " | |  | |  | |",
            " | \\--/ \\--/ |",
            " +=+=======+=+",
        ],
        _ => vec![
            " +-----------+",
            " | /--\\ /--\\ |",
            " | | -| |- | |",
            " | \\--/ \\--/ |",
            " +==+======+=+",
        ],
    }
}
pub static SKIN_IBM: PlayerSkin = PlayerSkin {
    play: "►",
    pause: "[]",
    next: ">>",
    prev: "<<",
    bar_filled: '▓',
    bar_empty: '░',
    art_fn: art_ibm,
};

// --- Windows 95: bouncing note ---
fn art_win95(_playing: bool, frame: usize) -> Vec<&'static str> {
    let p = (frame / 5) % 4;
    match p {
        0 => vec![
            " +-----------+",
            " | d         |",
            " |           |",
            " |           |",
            " +-----------+",
        ],
        1 => vec![
            " +-----------+",
            " |           |",
            " |     d     |",
            " |           |",
            " +-----------+",
        ],
        2 => vec![
            " +-----------+",
            " |           |",
            " |           |",
            " |        d  |",
            " +-----------+",
        ],
        _ => vec![
            " +-----------+",
            " |           |",
            " |  d        |",
            " |           |",
            " +-----------+",
        ],
    }
}
pub static SKIN_WIN95: PlayerSkin = PlayerSkin {
    play: "|>",
    pause: "||",
    next: ">>|",
    prev: "|<<",
    bar_filled: '█',
    bar_empty: '░',
    art_fn: art_win95,
};

// --- System 7: pulsing speaker ---
fn art_system7(_playing: bool, frame: usize) -> Vec<&'static str> {
    let p = (frame / 4) % 3;
    match p {
        0 => vec![
            "              ",
            "   |\\         ",
            "   | \\  )     ",
            "   | /        ",
            "   |/         ",
        ],
        1 => vec![
            "              ",
            "   |\\         ",
            "   | \\ ) )    ",
            "   | /        ",
            "   |/         ",
        ],
        _ => vec![
            "              ",
            "   |\\         ",
            "   | \\ ) ) )  ",
            "   | /        ",
            "   |/         ",
        ],
    }
}
pub static SKIN_SYSTEM7: PlayerSkin = PlayerSkin {
    play: "▶",
    pause: "■",
    next: "▷▷",
    prev: "◁◁",
    bar_filled: '█',
    bar_empty: '·',
    art_fn: art_system7,
};

// --- BIOS: spinning pipe ---
fn art_bios(_playing: bool, frame: usize) -> Vec<&'static str> {
    let p = (frame / 4) % 4;
    match p {
        0 => vec![
            "              ",
            "  PLAYING...  ",
            "              ",
            "      |       ",
            "              ",
        ],
        1 => vec![
            "              ",
            "  PLAYING...  ",
            "              ",
            "      /       ",
            "              ",
        ],
        2 => vec![
            "              ",
            "  PLAYING...  ",
            "              ",
            "      -       ",
            "              ",
        ],
        _ => vec![
            "              ",
            "  PLAYING...  ",
            "              ",
            "      \\       ",
            "              ",
        ],
    }
}
pub static SKIN_BIOS: PlayerSkin = PlayerSkin {
    play: "|>",
    pause: "||",
    next: ">>|",
    prev: "|<<",
    bar_filled: '=',
    bar_empty: '-',
    art_fn: art_bios,
};

// --- Red Sands: moon phases ---
fn art_red_sands(_playing: bool, frame: usize) -> Vec<&'static str> {
    let p = (frame / 10) % 8;
    match p {
        0 => vec![
            "   .----.     ",
            "  /      \\    ",
            " |        |   ",
            "  \\      /    ",
            "   `----'     ",
        ],
        1 => vec![
            "   .----.     ",
            "  / |    \\    ",
            " |  |     |   ",
            "  \\ |    /    ",
            "   `----'     ",
        ],
        2 => vec![
            "   .----.     ",
            "  /###   \\    ",
            " |###     |   ",
            "  \\###   /    ",
            "   `----'     ",
        ],
        3 => vec![
            "   .----.     ",
            "  /#####.\\    ",
            " |######.|   ",
            "  \\#####./    ",
            "   `----'     ",
        ],
        4 => vec![
            "   .----.     ",
            "  /######\\    ",
            " |########|   ",
            "  \\######/    ",
            "   `----'     ",
        ],
        5 => vec![
            "   .----.     ",
            "  /.#####\\    ",
            " |.######|   ",
            "  \\.#####/    ",
            "   `----'     ",
        ],
        6 => vec![
            "   .----.     ",
            "  /   ###\\    ",
            " |     ###|   ",
            "  \\   ###/    ",
            "   `----'     ",
        ],
        _ => vec![
            "   .----.     ",
            "  /    | \\    ",
            " |     |  |   ",
            "  \\    | /    ",
            "   `----'     ",
        ],
    }
}
pub static SKIN_RED_SANDS: PlayerSkin = PlayerSkin {
    play: "▸",
    pause: "◾",
    next: "▸▸",
    prev: "◂◂",
    bar_filled: '▬',
    bar_empty: '·',
    art_fn: art_red_sands,
};

// --- Newport Lights: cigarette out of pack, then horizontal with smoke ---
fn art_newport(_playing: bool, frame: usize) -> Vec<&'static str> {
    let p = (frame / 8) % 8;
    match p {
        0 => vec![
            "  /NEWPORT/|  ",
            " /       / |  ",
            "/________/ |  ",
            "|       | /   ",
            "|_______|/    ",
        ],
        1 => vec![
            "    []        ",
            "  /N||PORT/|  ",
            " /       / |  ",
            "/________/ |  ",
            "|_______|/    ",
        ],
        2 => vec![
            "    []        ",
            "    ||        ",
            "  /N||PORT/|  ",
            " /________/|  ",
            " |_______|/   ",
        ],
        3 => vec![
            "    []        ",
            "    ||        ",
            "    ||        ",
            "  /NEWPORT/|  ",
            "  |_______|/  ",
        ],
        4 => vec![
            "              ",
            "              ",
            "   _______    ",
            " ()_______))))",
            "              ",
        ],
        5 => vec![
            "              ",
            "         (    ",
            "   ______     ",
            " ()______)))  ",
            "              ",
        ],
        6 => vec![
            "        (     ",
            "       )      ",
            "   _____      ",
            " ()_____)))   ",
            "              ",
        ],
        _ => vec![
            "       )      ",
            "      (       ",
            "   ____       ",
            " ()____))     ",
            "              ",
        ],
    }
}
pub static SKIN_NEWPORT: PlayerSkin = PlayerSkin {
    play: "▶",
    pause: "||",
    next: "▷▷",
    prev: "◁◁",
    bar_filled: '█',
    bar_empty: '░',
    art_fn: art_newport,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pulse_color_stays_in_range() {
        let base = Color::Rgb(200, 150, 100);
        for frame in 0..100 {
            if let Color::Rgb(r, g, b) = pulse_color(base, frame, 40) {
                assert!(r <= 200, "r={} exceeded base at frame {}", r, frame);
                assert!(g <= 150, "g={} exceeded base at frame {}", g, frame);
                assert!(b <= 100, "b={} exceeded base at frame {}", b, frame);
            } else {
                panic!("Expected Rgb color");
            }
        }
    }

    #[test]
    fn pulse_color_is_periodic() {
        let base = Color::Rgb(200, 150, 100);
        let c0 = pulse_color(base, 0, 40);
        let c40 = pulse_color(base, 40, 40);
        assert_eq!(c0, c40);
    }

    #[test]
    fn pulse_color_non_rgb_passthrough() {
        let c = pulse_color(Color::White, 5, 40);
        assert_eq!(c, Color::White);
    }

    #[test]
    fn typing_reveal_progression() {
        let text = "zytunes";
        assert_eq!(typing_reveal(text, 0), "");
        assert_eq!(typing_reveal(text, 2), "z");
        assert_eq!(typing_reveal(text, 4), "zy");
    }

    #[test]
    fn typing_reveal_complete() {
        let text = "zytunes";
        assert_eq!(typing_reveal(text, 100), "zytunes");
    }

    #[test]
    fn shine_offset_wraps() {
        let bar_width = 10;
        let mut positions = Vec::new();
        for frame in 0..100 {
            if let Some(pos) = shine_offset(frame, bar_width) {
                positions.push(pos);
            }
        }
        // Should wrap around — we should see values < bar_width appear more than once
        assert!(positions.len() > bar_width);
    }

    #[test]
    fn shine_offset_zero_width() {
        assert_eq!(shine_offset(5, 0), None);
    }

    #[test]
    fn connection_screen_lines_fit_width() {
        use zytunes::device::DeviceFamily;
        for family in [
            None,
            Some(DeviceFamily::Zune),
            Some(DeviceFamily::Ipod),
            Some(DeviceFamily::Gogear),
        ] {
            for frame in 0..100 {
                let (line1, line2) = connection_screen_lines(frame, family);
                assert!(
                    line1.len() <= 12,
                    "line1 '{}' exceeds 12 chars at frame {} family {:?}",
                    line1,
                    frame,
                    family
                );
                assert!(
                    line2.len() <= 12,
                    "line2 '{}' exceeds 12 chars at frame {} family {:?}",
                    line2,
                    frame,
                    family
                );
            }
        }
    }

    #[test]
    fn connection_screen_stages_progress() {
        use zytunes::device::DeviceFamily;
        let (l1_0, _) = connection_screen_lines(0, None);
        let (_, l2_20) = connection_screen_lines(20, Some(DeviceFamily::Ipod));
        let (l1_40_zune, _) = connection_screen_lines(40, Some(DeviceFamily::Zune));
        let (l1_40_ipod, _) = connection_screen_lines(40, Some(DeviceFamily::Ipod));
        assert!(l1_0.starts_with("Scanning"));
        assert_eq!(l2_20, "iPod");
        assert!(l1_40_zune.starts_with("MTPZ"));
        assert!(l1_40_ipod.starts_with("Opening"));
    }

    #[test]
    fn progress_bar_correct_length() {
        let bar = progress_bar_with_shine(10, 5, 0);
        assert_eq!(bar.len(), 15);
        // First 10 should be filled ('='), last 5 should be empty (' ')
        for (i, &(ch, _)) in bar.iter().enumerate() {
            if i < 10 {
                assert_eq!(ch, '=');
            } else {
                assert_eq!(ch, ' ');
            }
        }
    }

    #[test]
    fn skin_animation_frames() {
        // Verify at least a couple of skins actually animate (produce distinct frames).
        let tokyo = &SKIN_TOKYO_NIGHT;
        let frames: Vec<_> = (0..20).map(|f| (tokyo.art_fn)(true, f)).collect();
        let unique: std::collections::HashSet<String> =
            frames.iter().map(|f| format!("{:?}", f)).collect();
        assert!(unique.len() >= 2, "Tokyo Night should have animated frames");

        let ibm = &SKIN_IBM;
        let frames: Vec<_> = (0..20).map(|f| (ibm.art_fn)(true, f)).collect();
        let unique: std::collections::HashSet<String> =
            frames.iter().map(|f| format!("{:?}", f)).collect();
        assert!(unique.len() >= 2, "IBM should have animated frames");
    }

    #[test]
    fn animated_accent_none_returns_base() {
        let base = Color::Rgb(100, 150, 200);
        let sec = Color::Rgb(200, 100, 50);
        assert_eq!(animated_accent(base, sec, AccentAnim::None, 10, 40), base);
    }

    #[test]
    fn animated_accent_pulse_matches_pulse_color() {
        let base = Color::Rgb(100, 150, 200);
        let sec = Color::Rgb(200, 100, 50);
        for frame in 0..40 {
            assert_eq!(
                animated_accent(base, sec, AccentAnim::Pulse, frame, 40),
                pulse_color(base, frame, 40),
            );
        }
    }

    #[test]
    fn hue_cycle_produces_different_colors() {
        let base = Color::Rgb(200, 50, 50);
        let colors: std::collections::HashSet<_> = (0..40)
            .map(|f| format!("{:?}", hue_cycle(base, f, 40)))
            .collect();
        assert!(colors.len() > 5, "Hue cycle should produce varied colors");
    }

    #[test]
    fn hue_cycle_achromatic_passthrough() {
        let grey = Color::Rgb(128, 128, 128);
        assert_eq!(hue_cycle(grey, 10, 40), grey);
    }

    #[test]
    fn color_shift_endpoints() {
        let a = Color::Rgb(100, 0, 0);
        let b = Color::Rgb(0, 100, 0);
        // At frame 0 (t=0.5 in sine table), should be a mix
        let mid = color_shift(a, b, 0, 40);
        assert_ne!(mid, a);
        assert_ne!(mid, b);
    }

    #[test]
    fn color_shift_non_rgb_passthrough() {
        assert_eq!(
            color_shift(Color::White, Color::Rgb(0, 0, 0), 5, 40),
            Color::White
        );
    }
}
