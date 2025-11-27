use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::env;
use std::fs::File;
use std::io::Write;

#[derive(Serialize, Deserialize)]
struct Vec2d {
    x: i32,
    y: i32,
}

#[derive(Serialize, Deserialize)]
struct Path {
    points: Vec<Vec2d>,
}

#[derive(Serialize, Deserialize)]
struct Glyph {
    left: i32,
    right: i32,
    paths: Vec<Path>,
}

fn main() {
    println!("cargo:rerun-if-changed=fonts.json");

    let out_dir = env::var("OUT_DIR").unwrap();
    let dest_path = std::path::Path::new(&out_dir).join("fonts.bin");

    // Read and parse the JSON file
    let fonts_json = std::fs::read_to_string("fonts.json").unwrap();
    let fonts: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&fonts_json).unwrap();

    let mut font_map: HashMap<String, Vec<Glyph>> = HashMap::new();

    // Convert JSON to our types
    for (font_name, font_data) in fonts.iter() {
        let glyphs_json = font_data.as_array().unwrap();
        let mut glyphs = Vec::new();

        for glyph_json in glyphs_json {
            let glyph_arr = glyph_json.as_array().unwrap();
            let left = glyph_arr[0].as_i64().unwrap() as i32;
            let right = glyph_arr[1].as_i64().unwrap() as i32;
            let paths_json = glyph_arr[2].as_array().unwrap();

            let mut paths = Vec::new();
            for path_json in paths_json {
                let points_json = path_json.as_array().unwrap();
                let mut points = Vec::new();

                for point_json in points_json {
                    let point_arr = point_json.as_array().unwrap();
                    let x = point_arr[0].as_i64().unwrap() as i32;
                    let y = point_arr[1].as_i64().unwrap() as i32;
                    points.push(Vec2d { x, y });
                }

                paths.push(Path { points });
            }

            glyphs.push(Glyph { left, right, paths });
        }

        font_map.insert(font_name.clone(), glyphs);
    }

    // Serialize to binary
    let encoded = bincode::serialize(&font_map).unwrap();
    let mut file = File::create(&dest_path).unwrap();
    file.write_all(&encoded).unwrap();

    println!("Generated binary font data: {} bytes", encoded.len());
}
