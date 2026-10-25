mod render;

use std::collections::HashMap;
use std::ffi::c_void;
use std::fs;
use std::io::{self, Read};
use std::process::ExitCode;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use render::{Frame, Renderer};
use wayland_client::protocol::{wl_callback, wl_compositor, wl_output, wl_registry, wl_surface};
use wayland_client::{delegate_noop, Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::{Layer, ZwlrLayerShellV1},
    zwlr_layer_surface_v1::{Anchor, KeyboardInteractivity, ZwlrLayerSurfaceV1},
};

const VERSION: &str = env!("CARGO_PKG_VERSION");

struct Cli {
    shader: String,
    output: Option<String>,
    speed: f32,
}

fn main() -> ExitCode {
    let cli = match parse_args() {
        Ok(Some(cli)) => cli,
        Ok(None) => return ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("gpupaper: {message}");
            eprintln!("Try 'gpupaper --help' for more information.");
            return ExitCode::from(2);
        }
    };

    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("gpupaper: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<(), String> {
    let fragment_source = read_shader(&cli.shader)?;

    let connection = Connection::connect_to_env()
        .map_err(|e| format!("could not connect to the Wayland compositor: {e}"))?;
    let mut event_queue = connection.new_event_queue();
    let qh = event_queue.handle();

    let display = connection.display();
    let display_ptr = display.id().as_ptr() as *mut c_void;
    let renderer = Renderer::new(display_ptr, fragment_source)?;

    let mut app = App {
        compositor: None,
        layer_shell: None,
        outputs: HashMap::new(),
        renderer,
        filter: cli.output,
        speed: cli.speed,
        start: Instant::now(),
        running: true,
        error: None,
    };

    display.get_registry(&qh, ());

    event_queue
        .roundtrip(&mut app)
        .map_err(|e| format!("Wayland roundtrip failed: {e}"))?;

    let (_, compositor_version) = app
        .compositor
        .as_ref()
        .ok_or_else(|| "compositor does not advertise wl_compositor".to_string())?;
    if *compositor_version < 3 {
        return Err("compositor does not support wl_compositor version 3 (buffer scale)".into());
    }
    if app.layer_shell.is_none() {
        return Err("compositor does not support zwlr_layer_shell_v1".into());
    }

    while app.running {
        event_queue
            .blocking_dispatch(&mut app)
            .map_err(|e| format!("Wayland dispatch failed: {e}"))?;
    }

    match app.error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

fn parse_args() -> Result<Option<Cli>, String> {
    let mut args = std::env::args().skip(1);
    let mut shader = None;
    let mut output = None;
    let mut speed = 1.0;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print_usage();
                return Ok(None);
            }
            "-V" | "--version" => {
                println!("gpupaper {VERSION}");
                return Ok(None);
            }
            "-o" | "--output" => {
                let value = args
                    .next()
                    .ok_or_else(|| format!("{arg} requires an output name"))?;
                output = Some(value);
            }
            "-s" | "--speed" => {
                let value = args
                    .next()
                    .ok_or_else(|| format!("{arg} requires a number"))?;
                speed = parse_speed(&value)?;
            }
            "--" => {
                for value in args.by_ref() {
                    if shader.replace(value).is_some() {
                        return Err("only one shader may be given".into());
                    }
                }
            }
            _ if arg.starts_with("--output=") => {
                output = Some(arg["--output=".len()..].to_string());
            }
            _ if arg.starts_with("--speed=") => {
                speed = parse_speed(&arg["--speed=".len()..])?;
            }
            _ if arg.starts_with('-') && arg != "-" => {
                return Err(format!("unknown option '{arg}'"));
            }
            _ => {
                if shader.replace(arg).is_some() {
                    return Err("only one shader may be given".into());
                }
            }
        }
    }

    let shader = shader.ok_or_else(|| "missing shader argument".to_string())?;
    Ok(Some(Cli {
        shader,
        output,
        speed,
    }))
}

fn parse_speed(value: &str) -> Result<f32, String> {
    let speed: f32 = value
        .parse()
        .map_err(|_| format!("invalid speed '{value}'"))?;

    if !speed.is_finite() || speed < 0.0 {
        return Err("speed must be a finite non-negative number".into());
    }

    Ok(speed)
}

fn print_usage() {
    println!(
        "Usage: gpupaper [-o OUTPUT] [-s SPEED] SHADER\n\
         \n\
         Render a GLSL fragment shader as a Wayland wallpaper.\n\
         \n\
         Arguments:\n\
         \x20 SHADER             Path to a fragment shader, or '-' to read stdin\n\
         \n\
         Options:\n\
         \x20 -o, --output NAME  Only render on the named output\n\
         \x20 -s, --speed N      Time multiplier (default: 1.0; 0 freezes time)\n\
         \x20 -h, --help         Show this help\n\
         \x20 -V, --version      Show the version\n\
         \n\
         If the shader has no '#version' directive, it is compiled as GLSL ES 3.00\n\
         with these declarations added:\n\
         \n\
         \x20 uniform vec2 u_resolution;\n\
         \x20 uniform float u_time;\n\
         \x20 layout(location = 0) out vec4 frag_color;\n\
         \n\
         Shadertoy shaders using mainImage() are detected automatically and get\n\
         iResolution, iTime, iTimeDelta, iFrameRate, iFrame, iMouse, and iDate.\n\
         Texture channels (iChannel0..3) are not supported; iMouse is always zero.\n\
         iDate uses real local time and is not affected by --speed.\n"
    );
}

fn read_shader(path: &str) -> Result<String, String> {
    let source = if path == "-" {
        let mut source = String::new();
        io::stdin()
            .read_to_string(&mut source)
            .map_err(|e| format!("could not read stdin: {e}"))?;
        source
    } else {
        fs::read_to_string(path).map_err(|e| format!("could not read '{path}': {e}"))?
    };

    if source.trim().is_empty() {
        return Err("shader is empty".into());
    }

    Ok(source)
}

fn local_date() -> [f32; 4] {
    let since_epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let epoch = since_epoch.as_secs() as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };

    if unsafe { libc::localtime_r(&epoch, &mut tm) }.is_null() {
        return [1970.0, 1.0, 1.0, 0.0];
    }

    let seconds = (tm.tm_hour * 3600 + tm.tm_min * 60 + tm.tm_sec) as f64
        + f64::from(since_epoch.subsec_nanos()) / 1_000_000_000.0;

    [
        (tm.tm_year + 1900) as f32,
        (tm.tm_mon + 1) as f32,
        tm.tm_mday as f32,
        seconds as f32,
    ]
}

struct App {
    compositor: Option<(wl_compositor::WlCompositor, u32)>,
    layer_shell: Option<(ZwlrLayerShellV1, u32)>,
    outputs: HashMap<u32, Output>,
    renderer: Renderer,
    filter: Option<String>,
    speed: f32,
    start: Instant,
    running: bool,
    error: Option<String>,
}

struct Output {
    proxy: wl_output::WlOutput,
    name: Option<String>,
    scale: i32,
    done: bool,
    layer: Option<LayerSurface>,
}

struct LayerSurface {
    surface: wl_surface::WlSurface,
    layer_surface: ZwlrLayerSurfaceV1,
    window: Option<wayland_egl::WlEglSurface>,
    egl_surface: Option<khronos_egl::Surface>,
    width: i32,
    height: i32,
    configured: bool,
    frame_pending: bool,
    frame: u32,
    last_wall_time: Option<f32>,
}

impl LayerSurface {
    fn configure(&mut self, renderer: &Renderer, scale: i32) -> Result<(), String> {
        let width = self.width * scale;
        let height = self.height * scale;
        if width <= 0 || height <= 0 {
            return Ok(());
        }

        if let Some(window) = self.window.as_ref() {
            window.resize(width, height, 0, 0);
        } else {
            let (window, surface) = renderer.create_surface(&self.surface, width, height)?;
            self.window = Some(window);
            self.egl_surface = Some(surface);
        }

        self.surface.set_buffer_scale(scale);
        Ok(())
    }
}

impl App {
    fn maybe_create_layers(&mut self, qh: &QueueHandle<Self>) {
        let Some((compositor, _)) = self.compositor.clone() else {
            return;
        };
        let Some((shell, _)) = self.layer_shell.clone() else {
            return;
        };
        let filter = self.filter.clone();

        for (&id, output) in self.outputs.iter_mut() {
            if !output.done || output.layer.is_some() {
                continue;
            }
            if let Some(filter) = filter.as_deref() {
                if output.name.as_deref() != Some(filter) {
                    continue;
                }
            }

            let surface = compositor.create_surface(qh, ());
            let layer_surface = shell.get_layer_surface(
                &surface,
                Some(&output.proxy),
                Layer::Background,
                "gpupaper".to_string(),
                qh,
                id,
            );

            layer_surface.set_anchor(Anchor::all());
            layer_surface.set_exclusive_zone(-1);
            layer_surface.set_size(0, 0);
            layer_surface.set_keyboard_interactivity(KeyboardInteractivity::None);
            surface.commit();

            output.layer = Some(LayerSurface {
                surface,
                layer_surface,
                window: None,
                egl_surface: None,
                width: 0,
                height: 0,
                configured: false,
                frame_pending: false,
                frame: 0,
                last_wall_time: None,
            });
        }
    }

    fn draw_output(&mut self, id: u32, qh: &QueueHandle<Self>) {
        let start = self.start;
        let speed = self.speed;
        let renderer = &mut self.renderer;

        let Some(output) = self.outputs.get_mut(&id) else {
            return;
        };
        let Some(layer) = output.layer.as_mut() else {
            return;
        };
        if !layer.configured {
            return;
        }

        let scale = output.scale.max(1);
        let width = layer.width * scale;
        let height = layer.height * scale;
        let Some(egl_surface) = layer.egl_surface else {
            return;
        };
        if width <= 0 || height <= 0 {
            return;
        }

        if !layer.frame_pending {
            let _ = layer.surface.frame(qh, id);
            layer.frame_pending = true;
        }

        let wall_time = start.elapsed().as_secs_f32();
        let wall_delta = layer
            .last_wall_time
            .map_or(1.0 / 60.0, |last| (wall_time - last).max(0.0));
        layer.last_wall_time = Some(wall_time);

        let time = wall_time * speed;
        let time_delta = wall_delta * speed;
        let frame_rate = if wall_delta > 0.0 {
            (1.0 / wall_delta).clamp(1.0, 1000.0)
        } else {
            60.0
        };
        let frame = Frame {
            width,
            height,
            time,
            time_delta,
            frame_rate,
            frame: layer.frame,
            date: local_date(),
        };
        layer.frame = layer.frame.wrapping_add(1);

        if let Err(e) = renderer.draw(egl_surface, frame) {
            self.error = Some(e);
            self.running = false;
        }
    }

    fn take_layer(&mut self, id: u32) -> Option<LayerSurface> {
        self.outputs.get_mut(&id)?.layer.take()
    }

    fn destroy_layer(&mut self, layer: &mut LayerSurface) {
        if let Some(surface) = layer.egl_surface.take() {
            self.renderer.destroy_surface(surface);
        }
        layer.window.take();
        layer.layer_surface.destroy();
        layer.surface.destroy();
    }

    fn remove_output(&mut self, id: u32) {
        if let Some(mut output) = self.outputs.remove(&id) {
            if let Some(mut layer) = output.layer.take() {
                self.destroy_layer(&mut layer);
            }
        }
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for App {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_registry::Event::Global {
                name,
                interface,
                version,
            } => {
                match interface.as_str() {
                    "wl_compositor" => {
                        let version = version.min(4);
                        let compositor = registry.bind::<wl_compositor::WlCompositor, _, _>(
                            name,
                            version,
                            qh,
                            (),
                        );
                        state.compositor = Some((compositor, version));
                    }
                    "zwlr_layer_shell_v1" => {
                        let version = version.min(4);
                        let shell = registry.bind::<ZwlrLayerShellV1, _, _>(name, version, qh, ());
                        state.layer_shell = Some((shell, version));
                    }
                    "wl_output" => {
                        let version = version.min(4);
                        let proxy =
                            registry.bind::<wl_output::WlOutput, _, _>(name, version, qh, name);
                        state.outputs.insert(
                            name,
                            Output {
                                proxy,
                                name: None,
                                scale: 1,
                                done: false,
                                layer: None,
                            },
                        );
                    }
                    _ => {}
                }

                state.maybe_create_layers(qh);
            }
            wl_registry::Event::GlobalRemove { name } => {
                state.remove_output(name);
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_output::WlOutput, u32> for App {
    fn event(
        state: &mut Self,
        _: &wl_output::WlOutput,
        event: wl_output::Event,
        id: &u32,
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        let id = *id;
        let mut redraw = false;
        let mut error = None;

        {
            let Some(output) = state.outputs.get_mut(&id) else {
                return;
            };

            match event {
                wl_output::Event::Name { name } => output.name = Some(name),
                wl_output::Event::Scale { factor } => {
                    if factor > 0 {
                        output.scale = factor;
                    }
                    if let Some(layer) = output.layer.as_mut() {
                        if layer.configured {
                            if let Err(e) = layer.configure(&state.renderer, output.scale.max(1)) {
                                error = Some(e);
                            } else {
                                redraw = true;
                            }
                        }
                    }
                }
                wl_output::Event::Done => output.done = true,
                _ => {}
            }
        }

        if let Some(message) = error {
            state.error = Some(message);
            state.running = false;
            return;
        }

        state.maybe_create_layers(qh);
        if redraw {
            state.draw_output(id, qh);
        }
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, u32> for App {
    fn event(
        state: &mut Self,
        _: &ZwlrLayerSurfaceV1,
        event: wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::Event,
        id: &u32,
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::Event;

        let id = *id;
        match event {
            Event::Configure {
                serial,
                width,
                height,
            } => {
                let mut error = None;
                {
                    let Some(output) = state.outputs.get_mut(&id) else {
                        return;
                    };
                    let Some(layer) = output.layer.as_mut() else {
                        return;
                    };

                    layer.layer_surface.ack_configure(serial);
                    layer.width = width as i32;
                    layer.height = height as i32;
                    layer.configured = true;

                    if let Err(e) = layer.configure(&state.renderer, output.scale.max(1)) {
                        error = Some(e);
                    }
                }

                if let Some(message) = error {
                    state.error = Some(message);
                    state.running = false;
                    return;
                }

                state.draw_output(id, qh);
            }
            Event::Closed => {
                if let Some(mut layer) = state.take_layer(id) {
                    state.destroy_layer(&mut layer);
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_callback::WlCallback, u32> for App {
    fn event(
        state: &mut Self,
        _: &wl_callback::WlCallback,
        event: wl_callback::Event,
        id: &u32,
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_callback::Event::Done { .. } = event {
            let id = *id;
            if let Some(output) = state.outputs.get_mut(&id) {
                if let Some(layer) = output.layer.as_mut() {
                    layer.frame_pending = false;
                }
            }
            state.draw_output(id, qh);
        }
    }
}

delegate_noop!(App: ignore wl_compositor::WlCompositor);
delegate_noop!(App: ignore wl_surface::WlSurface);
delegate_noop!(App: ignore ZwlrLayerShellV1);
