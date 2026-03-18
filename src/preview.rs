use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Instant;

use softbuffer::{Context, Surface};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

struct AppState {
    window: Arc<Window>,
    // Surface must be dropped before Context; field order = drop order.
    surface: Surface<Arc<Window>, Arc<Window>>,
    _context: Context<Arc<Window>>,
}

struct PreviewApp {
    renderer: plothole::PlotRenderer,
    state: Option<AppState>,
    start: Option<Instant>,
    width: u32,
    height: u32,
    done: bool, // true once the final complete frame has been rendered
}

impl ApplicationHandler for PreviewApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let window = Arc::new(
            event_loop
                .create_window(
                    Window::default_attributes()
                        .with_title("Plothole Preview")
                        .with_inner_size(winit::dpi::LogicalSize::new(self.width, self.height)),
                )
                .unwrap(),
        );
        let context = Context::new(window.clone()).unwrap();
        let surface = Surface::new(&context, window.clone()).unwrap();
        self.state = Some(AppState { window, surface, _context: context });
        self.start = Some(Instant::now());
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                self.width  = size.width.max(1);
                self.height = size.height.max(1);
            }
            WindowEvent::RedrawRequested => {
                let Some(state) = self.state.as_mut() else { return };
                let Some(start) = self.start else { return };

                let elapsed = start.elapsed().as_secs_f64();
                let t = elapsed.min(self.renderer.duration());
                if elapsed >= self.renderer.duration() {
                    self.done = true;
                }
                let w = self.width;
                let h = self.height;

                let mut pixmap = tiny_skia::Pixmap::new(w, h).unwrap();
                self.renderer.render_frame_native(&mut pixmap.as_mut(), t);

                state
                    .surface
                    .resize(NonZeroU32::new(w).unwrap(), NonZeroU32::new(h).unwrap())
                    .unwrap();

                let mut buffer = state.surface.buffer_mut().unwrap();
                let data = pixmap.data();
                for (i, pixel) in buffer.iter_mut().enumerate() {
                    let base = i * 4;
                    let r = data[base]     as u32;
                    let g = data[base + 1] as u32;
                    let b = data[base + 2] as u32;
                    *pixel = (r << 16) | (g << 8) | b;
                }
                buffer.present().unwrap();
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(state) = self.state.as_ref() {
            if self.start.is_some() {
                if !self.done {
                    state.window.request_redraw();
                }
            }
        }
    }
}

pub fn run(renderer: plothole::PlotRenderer) {
    let event_loop = EventLoop::new().unwrap();
    event_loop.set_control_flow(ControlFlow::Poll);

    let mut app = PreviewApp {
        renderer,
        state: None,
        start: None,
        width: 800,
        height: 500,
        done: false,
    };

    event_loop.run_app(&mut app).unwrap();
}
