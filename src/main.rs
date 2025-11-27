pub mod font;
mod hershey;
use num_traits::{Num, Signed, Zero};
use std::collections::HashMap;
use std::ops::Add;
use std::ops::Mul;
use std::ops::Neg;
use std::ops::Sub;

use std::{fs::File, ops::Deref};

use font::{Path, Vec2d};
use piet::{
    Color, RenderContext,
    kurbo::{Line, Rect, Size},
};
use piet_common::Device;
use piet_svg::RenderContext as SvgRenderContext;

use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Render text using a given font
    RenderText {
        #[arg(short, long)]
        text: String,

        #[arg(short, long)]
        font_name: String,
    },
}

fn main() {
    let cli = Cli::parse();
    match cli.command {
        Commands::RenderText { text, font_name } => {
            render_text(&text, &font_name);
        }
    }
}

fn render_text(text: &str, font_name: &str) {
    let font = hershey::get_by_name(font_name).expect("unknown font name");
    let drawing = Drawing::new(text_to_paths(text, &font));
    let bounds = drawing.bounding_box();
    let size = bounds.size();

    // Create an SVG render context with the given size
    let mut rc = SvgRenderContext::new(Size::new(size.x, size.y));
    render(&mut rc, &drawing);
    rc.finish().unwrap();
    println!("{}", rc.display());
    let out = File::create("out.svg").unwrap();
    rc.write(out).unwrap();
}

fn render(rc: &mut impl RenderContext, drawing: &Drawing<f64>) {
    let bb = drawing.bounding_box();
    let offset = bb.offset();

    rc.clear(None, Color::WHITE);
    for path in &drawing.paths {
        let path = match path {
            PenPath::PenUp(_) => {
                continue;
            }
            PenPath::PenDown(path) => path,
        };
        for segment in path.points().iter().zip(path.points().iter().skip(1)) {
            let (start, end) = segment;
            rc.stroke(
                Line::new((start + &offset).tuple(), (end + &offset).tuple()),
                &Color::BLACK,
                1.0,
            );
        }
    }

    rc.finish().unwrap();
    // rctx.stroke(shape, brush, width);
}

fn do_stuff(rc: &mut impl RenderContext) {
    rc.clear(None, Color::WHITE);
    rc.stroke(Line::new((10.0, 10.0), (100.0, 50.0)), &Color::BLUE, 1.0);
    rc.finish().unwrap();
    // rctx.stroke(shape, brush, width);
}

// Returns a set of paths that will render a string of text
// using the given font.
fn text_to_paths<'a>(input: &str, ft: &'a font::Font) -> Vec<Path<f64>> {
    let spacing = 0;
    let mut x = 0;
    let mut out = vec![];
    for ch in input.chars() {
        let index = (ch as usize) - 32;
        if index > ft.len() {
            x = x + spacing;
        }
        let glyph = &ft[index];

        for glyph_path in &glyph.paths {
            let mut new_path: Path<f64> = Path::empty();
            for point in glyph_path.points() {
                new_path.push(Vec2d {
                    x: (x as f64) + (point.x as f64) - (glyph.left as f64),
                    y: point.y as f64,
                });
            }
            out.push(new_path);
        }
        x = x + glyph.right - glyph.left + spacing
    }
    out
}

enum PenPath<T>
where
    T: Num
        + Signed
        + Zero
        + Neg<Output = T>
        + Sub<Output = T>
        + Mul<Output = T>
        + Add<Output = T>
        + PartialOrd
        + Copy,
{
    PenUp(Path<T>),
    PenDown(Path<T>),
}

impl<T> Deref for PenPath<T>
where
    T: Num
        + Signed
        + Zero
        + Neg<Output = T>
        + Sub<Output = T>
        + Mul<Output = T>
        + Add<Output = T>
        + PartialOrd
        + Copy,
{
    type Target = Path<T>;

    fn deref(&self) -> &Self::Target {
        match self {
            PenPath::PenUp(p) | PenPath::PenDown(p) => p,
        }
    }
}

struct Drawing<T>
where
    T: Num
        + Signed
        + Zero
        + Neg<Output = T>
        + Sub<Output = T>
        + Mul<Output = T>
        + Add<Output = T>
        + PartialOrd
        + Copy,
{
    paths: Vec<PenPath<T>>,
}

impl<T> Drawing<T>
where
    T: Num
        + Signed
        + Zero
        + Neg<Output = T>
        + Sub<Output = T>
        + Mul<Output = T>
        + Add<Output = T>
        + PartialOrd
        + Copy,
{
    fn new(paths: Vec<Path<T>>) -> Self {
        let mut prev_position = Vec2d {
            x: T::zero(),
            y: T::zero(),
        };
        let mut out_paths = vec![];
        for path in paths {
            if path.points().is_empty() {
                continue;
            }
            // Move to the start of the next path (pen up).
            out_paths.push(PenPath::PenUp(Path::new(vec![
                prev_position.clone(),
                path.points()[0].clone(),
            ])));
            // Update final position so the next path knows where to move from.
            prev_position = path.points().last().unwrap().clone();
            // Draw the path.
            out_paths.push(PenPath::PenDown(path));
        }
        assert!(!out_paths.is_empty());
        Self { paths: out_paths }
    }

    fn bounding_box(&self) -> BoundingBox<T> {
        let first_point = self
            .paths
            .first()
            .expect("paths are empty")
            .points()
            .first()
            .expect("path has no points");
        let mut bounding_box = BoundingBox::new(&first_point);
        for path in self.paths.iter() {
            for point in path.points() {
                bounding_box.update(point)
            }
        }
        bounding_box
    }
}

#[derive(Clone, Debug)]
struct BoundingBox<T>
where
    T: Num
        + Signed
        + Zero
        + Neg<Output = T>
        + Sub<Output = T>
        + Mul<Output = T>
        + Add<Output = T>
        + PartialOrd
        + Copy,
{
    /// smallest value on X axis
    left: T,
    /// largest value on X axis
    right: T,

    /// smallest value on Y axis
    top: T,
    /// largest value on Y axis
    bottom: T,
}

impl<T> BoundingBox<T>
where
    T: Num
        + Signed
        + Zero
        + Neg<Output = T>
        + Sub<Output = T>
        + Mul<Output = T>
        + Add<Output = T>
        + PartialOrd
        + Copy,
{
    pub fn new(defaults: &Vec2d<T>) -> Self {
        Self {
            left: defaults.x,
            right: defaults.x,
            top: defaults.y,
            bottom: defaults.y,
        }
    }

    // Returns the width (x) and height (y) of the box as a Vec2d
    pub fn size(&self) -> Vec2d<T> {
        Vec2d {
            x: self.right - self.left,
            y: self.bottom - self.top,
        }
    }

    // Returns a vector Vec2d which, when added to the top-left corner,
    // translates it to the origin (0,0).
    pub fn offset(&self) -> Vec2d<T> {
        Vec2d {
            x: -self.left,
            y: -self.top,
        }
    }

    pub fn update(&mut self, point: &Vec2d<T>) {
        if point.x < self.left {
            self.left = point.x;
        }
        if point.x > self.right {
            self.right = point.x;
        }
        if point.y < self.top {
            self.top = point.y;
        }
        if point.y > self.bottom {
            self.bottom = point.y;
        }
    }
}
