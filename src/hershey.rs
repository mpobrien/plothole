use std::collections::HashMap;
use std::sync::OnceLock;

static FONTS_DATA: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/fonts.bin"));
static FONTS: OnceLock<HashMap<String, crate::font::Font>> = OnceLock::new();

pub fn fonts() -> &'static HashMap<String, crate::font::Font> {
    FONTS.get_or_init(|| {
        bincode::deserialize(FONTS_DATA).expect("Failed to deserialize font data")
    })
}
