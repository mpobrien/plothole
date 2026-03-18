//! Iosevka skeleton font loader.
//!
//! Reads the `skeleton.json` produced by `tools/extract-skeleton.mjs` and
//! exposes it with the same `text_to_paths` interface as `TtfFont`.
//!
//! Coordinate system: Iosevka stores coordinates in font units (UPM=1000,
//! y-up, baseline at 0).  `text_to_paths` converts them to cursor-offset
//! coordinates (y-down) matching the TTF pipeline:
//!   out_x = cursor_x + x * scale
//!   out_y = line_y   - y * scale   (y-flip; baseline at line_y)

use std::collections::HashMap;

use serde::Deserialize;

use crate::font::{Path, Vec2d};

// ── JSON types ─────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct Meta {
    upm:          f64,
    cell_advance: f64,
    ascender:     f64,
}

#[derive(Deserialize)]
struct GlyphData {
    #[allow(dead_code)]
    advance: f64, // kept for JSON compatibility; actual advance comes from meta.cell_advance
    strokes: Vec<Vec<Point>>,
}

#[derive(Deserialize)]
struct Point {
    x: f64,
    y: f64,
}

#[derive(Deserialize)]
struct SkeletonFile {
    meta:   Meta,
    glyphs: HashMap<String, GlyphData>,
}

// ── Public API ─────────────────────────────────────────────────────────────

pub struct IosevkaFont {
    skeleton: SkeletonFile,
}

impl IosevkaFont {
    pub fn from_file(path: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let data = std::fs::read_to_string(path)?;
        let skeleton: SkeletonFile = serde_json::from_str(&data)?;
        Ok(Self { skeleton })
    }

    pub fn from_json(json: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let skeleton: SkeletonFile = serde_json::from_str(json)?;
        Ok(Self { skeleton })
    }

    /// Returns one `Vec<Path<f64>>` per character in cursor-offset coordinates.
    ///
    /// `em_size` is the desired output size in the same units as downstream
    /// coordinates (one em = cap-height, scaled from UPM).
    pub fn text_to_paths(&self, text: &str, em_size: f64) -> Vec<Vec<Path<f64>>> {
        let upm          = self.skeleton.meta.upm;
        let cell_advance = self.skeleton.meta.cell_advance;
        let scale        = em_size / upm;
        let advance      = cell_advance * scale; // same for every glyph — true monospace
        let line_height  = self.skeleton.meta.ascender * scale * 1.2;

        let mut result   = Vec::new();
        let mut cursor_x = 0.0f64;
        let mut line_y   = 0.0f64;

        for ch in text.chars() {
            if ch == '\n' {
                cursor_x = 0.0;
                line_y  += line_height;
                continue;
            }

            let key = ch.to_string();
            let Some(glyph) = self.skeleton.glyphs.get(&key) else {
                // Unknown glyph: emit an empty slot but still advance the cursor.
                result.push(vec![]);
                cursor_x += advance;
                continue;
            };

            let paths: Vec<Path<f64>> = glyph.strokes.iter()
                .filter(|s| s.len() >= 2)
                .map(|stroke| {
                    Path::new(stroke.iter().map(|p| Vec2d {
                        x: cursor_x + p.x * scale,
                        y: line_y   - p.y * scale,
                    }).collect())
                })
                .collect();

            result.push(paths);
            cursor_x += advance; // fixed monospace advance, not glyph.advance
        }

        result
    }
}
