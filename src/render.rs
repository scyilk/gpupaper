use std::ffi::c_void;
use std::ptr;

use glow::HasContext;
use khronos_egl as egl;
use wayland_client::protocol::wl_surface;
use wayland_client::Proxy;
use wayland_egl::WlEglSurface;

const VERTEX_SOURCE: &str = r#"#version 300 es
void main() {
    vec2 p = vec2(float((gl_VertexID << 1) & 2), float(gl_VertexID & 2));
    gl_Position = vec4(p * 2.0 - 1.0, 0.0, 1.0);
}
"#;

const FRAGMENT_PRELUDE: &str = r#"#version 300 es
precision highp float;
precision highp int;
uniform vec2 u_resolution;
uniform float u_time;
layout(location = 0) out vec4 frag_color;
"#;

const SHADERTOY_DECLARATIONS: &str = r#"precision highp float;
precision highp int;
uniform vec3 iResolution;
uniform float iTime;
uniform float iTimeDelta;
uniform float iFrameRate;
uniform int iFrame;
uniform vec4 iMouse;
uniform vec4 iDate;
layout(location = 0) out vec4 frag_color;
"#;

const SHADERTOY_MAIN: &str = r#"
void main() {
    vec4 color = vec4(0.0, 0.0, 0.0, 1.0);
    mainImage(color, gl_FragCoord.xy);
    frag_color = color;
}
"#;

pub struct Frame {
    pub width: i32,
    pub height: i32,
    pub time: f32,
    pub time_delta: f32,
    pub frame_rate: f32,
    pub frame: u32,
    pub date: [f32; 4],
}

pub struct Renderer {
    egl: egl::Instance<egl::Static>,
    display: egl::Display,
    config: egl::Config,
    context: egl::Context,
    gl: Option<glow::Context>,
    fragment_source: String,
    program: Option<Program>,
    vertex_array: Option<glow::VertexArray>,
}

struct Program {
    program: glow::Program,
    u_resolution: Option<glow::UniformLocation>,
    u_time: Option<glow::UniformLocation>,
    i_resolution: Option<glow::UniformLocation>,
    i_time: Option<glow::UniformLocation>,
    i_time_delta: Option<glow::UniformLocation>,
    i_frame_rate: Option<glow::UniformLocation>,
    i_frame: Option<glow::UniformLocation>,
    i_mouse: Option<glow::UniformLocation>,
    i_date: Option<glow::UniformLocation>,
}

impl Renderer {
    pub fn new(display_ptr: *mut c_void, fragment_source: String) -> Result<Self, String> {
        let egl: egl::Instance<egl::Static> = egl::Instance::new(egl::Static);

        let display = unsafe { egl.get_display(display_ptr) }
            .ok_or_else(|| "EGL: could not get Wayland display".to_string())?;

        egl.initialize(display)
            .map_err(|e| format!("EGL: initialize failed: {e}"))?;

        egl.bind_api(egl::OPENGL_ES_API)
            .map_err(|e| format!("EGL: could not bind OpenGL ES: {e}"))?;

        let config_attributes = [
            egl::SURFACE_TYPE,
            egl::WINDOW_BIT,
            egl::RENDERABLE_TYPE,
            egl::OPENGL_ES3_BIT,
            egl::RED_SIZE,
            8,
            egl::GREEN_SIZE,
            8,
            egl::BLUE_SIZE,
            8,
            egl::ALPHA_SIZE,
            8,
            egl::NONE,
        ];

        let config = egl
            .choose_first_config(display, &config_attributes)
            .map_err(|e| format!("EGL: could not choose a config: {e}"))?
            .ok_or_else(|| "EGL: no suitable config found".to_string())?;

        let context = egl
            .create_context(
                display,
                config,
                None,
                &[egl::CONTEXT_CLIENT_VERSION, 3, egl::NONE],
            )
            .map_err(|e| format!("EGL: could not create an OpenGL ES 3 context: {e}"))?;

        let _ = egl.swap_interval(display, 0);

        Ok(Self {
            egl,
            display,
            config,
            context,
            gl: None,
            fragment_source: prepare_source(&fragment_source),
            program: None,
            vertex_array: None,
        })
    }

    pub fn create_surface(
        &self,
        surface: &wl_surface::WlSurface,
        width: i32,
        height: i32,
    ) -> Result<(WlEglSurface, egl::Surface), String> {
        let window = WlEglSurface::new(surface.id(), width, height)
            .map_err(|e| format!("could not create EGL window: {e}"))?;

        let egl_surface = unsafe {
            self.egl.create_window_surface(
                self.display,
                self.config,
                window.ptr() as egl::NativeWindowType,
                None,
            )
        }
        .map_err(|e| format!("EGL: could not create window surface: {e}"))?;

        Ok((window, egl_surface))
    }

    pub fn destroy_surface(&self, surface: egl::Surface) {
        let _ = self.egl.make_current(self.display, None, None, None);
        let _ = self.egl.destroy_surface(self.display, surface);
    }

    pub fn draw(&mut self, surface: egl::Surface, frame: Frame) -> Result<(), String> {
        self.egl
            .make_current(
                self.display,
                Some(surface),
                Some(surface),
                Some(self.context),
            )
            .map_err(|e| format!("EGL: could not make context current: {e}"))?;

        if self.gl.is_none() {
            let egl = &self.egl;
            let gl = unsafe {
                glow::Context::from_loader_function(|name| {
                    egl.get_proc_address(name)
                        .map_or(ptr::null(), |f| f as *const c_void)
                })
            };
            self.gl = Some(gl);
        }

        self.ensure_program()?;

        let gl = self.gl.as_ref().expect("GL context was just created");
        let program = self.program.as_ref().expect("program was just created");

        unsafe {
            gl.viewport(0, 0, frame.width, frame.height);
            gl.use_program(Some(program.program));

            gl.uniform_2_f32(
                program.u_resolution.as_ref(),
                frame.width as f32,
                frame.height as f32,
            );
            gl.uniform_1_f32(program.u_time.as_ref(), frame.time);

            gl.uniform_3_f32(
                program.i_resolution.as_ref(),
                frame.width as f32,
                frame.height as f32,
                1.0,
            );
            gl.uniform_1_f32(program.i_time.as_ref(), frame.time);
            gl.uniform_1_f32(program.i_time_delta.as_ref(), frame.time_delta);
            gl.uniform_1_f32(program.i_frame_rate.as_ref(), frame.frame_rate);
            gl.uniform_1_i32(program.i_frame.as_ref(), frame.frame as i32);
            gl.uniform_4_f32(program.i_mouse.as_ref(), 0.0, 0.0, 0.0, 0.0);
            gl.uniform_4_f32(
                program.i_date.as_ref(),
                frame.date[0],
                frame.date[1],
                frame.date[2],
                frame.date[3],
            );

            gl.draw_arrays(glow::TRIANGLES, 0, 3);
        }

        self.egl
            .swap_buffers(self.display, surface)
            .map_err(|e| format!("EGL: swap failed: {e}"))
    }

    fn ensure_program(&mut self) -> Result<(), String> {
        if self.program.is_some() {
            return Ok(());
        }

        let gl = self.gl.as_ref().expect("GL context must be current");

        unsafe {
            if self.vertex_array.is_none() {
                let vertex_array = gl
                    .create_vertex_array()
                    .map_err(|e| format!("GL: could not create vertex array: {e}"))?;
                gl.bind_vertex_array(Some(vertex_array));
                self.vertex_array = Some(vertex_array);
            }

            let vertex_shader = compile_shader(gl, glow::VERTEX_SHADER, VERTEX_SOURCE)?;
            let fragment_shader = compile_shader(gl, glow::FRAGMENT_SHADER, &self.fragment_source)?;

            let program = gl
                .create_program()
                .map_err(|e| format!("GL: could not create program: {e}"))?;

            gl.attach_shader(program, vertex_shader);
            gl.attach_shader(program, fragment_shader);
            gl.link_program(program);
            gl.detach_shader(program, vertex_shader);
            gl.detach_shader(program, fragment_shader);
            gl.delete_shader(vertex_shader);
            gl.delete_shader(fragment_shader);

            if !gl.get_program_link_status(program) {
                let log = gl.get_program_info_log(program);
                gl.delete_program(program);
                return Err(format!("shader link failed:\n{log}"));
            }

            self.program = Some(Program {
                program,
                u_resolution: gl.get_uniform_location(program, "u_resolution"),
                u_time: gl.get_uniform_location(program, "u_time"),
                i_resolution: gl.get_uniform_location(program, "iResolution"),
                i_time: gl.get_uniform_location(program, "iTime"),
                i_time_delta: gl.get_uniform_location(program, "iTimeDelta"),
                i_frame_rate: gl.get_uniform_location(program, "iFrameRate"),
                i_frame: gl.get_uniform_location(program, "iFrame"),
                i_mouse: gl.get_uniform_location(program, "iMouse"),
                i_date: gl.get_uniform_location(program, "iDate"),
            });
        }

        Ok(())
    }
}

unsafe fn compile_shader(
    gl: &glow::Context,
    shader_type: u32,
    source: &str,
) -> Result<glow::Shader, String> {
    let shader = gl
        .create_shader(shader_type)
        .map_err(|e| format!("GL: could not create shader: {e}"))?;

    gl.shader_source(shader, source);
    gl.compile_shader(shader);

    if gl.get_shader_compile_status(shader) {
        Ok(shader)
    } else {
        let log = gl.get_shader_info_log(shader);
        gl.delete_shader(shader);
        Err(format!("shader compilation failed:\n{log}"))
    }
}

fn prepare_source(source: &str) -> String {
    if is_shadertoy(source) {
        let mut prepared = if has_version_directive(source) {
            insert_after_version(source, SHADERTOY_DECLARATIONS)
        } else {
            format!("#version 300 es\n{SHADERTOY_DECLARATIONS}\n{source}")
        };
        prepared.push_str(SHADERTOY_MAIN);
        prepared
    } else if has_version_directive(source) {
        source.to_string()
    } else {
        format!("{FRAGMENT_PRELUDE}\n{source}")
    }
}

fn is_shadertoy(source: &str) -> bool {
    source.contains("mainImage")
}

fn has_version_directive(source: &str) -> bool {
    for line in source.lines() {
        let line = line.trim_start();
        if line.is_empty() || line.starts_with("//") {
            continue;
        }
        return line.starts_with("#version");
    }
    false
}

fn insert_after_version(source: &str, declarations: &str) -> String {
    let mut output = String::with_capacity(source.len() + declarations.len());
    let mut inserted = false;

    for line in source.split_inclusive('\n') {
        output.push_str(line);
        if !inserted && line.trim_start().starts_with("#version") {
            if !line.ends_with('\n') {
                output.push('\n');
            }
            output.push_str(declarations);
            inserted = true;
        }
    }

    if !inserted {
        output.push_str(declarations);
    }

    output
}
