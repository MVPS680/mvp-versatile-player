//! The one shader pass that draws the picture when the sliders are not neutral.
//!
//! # Why it samples egui's own texture
//!
//! The frame is already on the GPU as [`egui::TextureHandle`] `"mvp-frame"`, and
//! `egui_glow` will hand out the underlying `glow::Texture` for a texture id. So this
//! pass binds *that* texture: no second upload, no second copy of the pixels, and
//! nothing to keep in step with the upload path. When the sliders are neutral and the
//! enhancement is off, the untouched `Painter::image` path runs instead and this
//! module is not involved at all — see [`crate::picture::uniforms`].
//!
//! # One pass, no intermediate buffer
//!
//! Everything happens in one fragment shader reading the frame: white balance,
//! brightness and contrast, gamma, saturation, an unsharp mask, and a light
//! edge-aware smoothing. A second pass would mean a second full-size buffer, which at
//! 4K is 33 MB of traffic per frame — the cost this project already paid once with a
//! CPU tone map, and the reason the material in `surface.rs` is opaque.
//!
//! # Failure is not fatal
//!
//! A driver that will not compile the shader, or a frame texture that is not there
//! yet, leaves the picture untouched rather than black: [`PicturePass::failed`] is
//! what the caller checks before installing a callback at all.

use std::sync::Arc;

use eframe::egui_glow::glow::{self, HasContext as _};
use eframe::egui_glow::Painter;
use egui::epaint::PaintCallbackInfo;

use crate::picture::PictureUniforms;

/// A frame the picture pass rendered off screen, on its way to a file.
///
/// This is the only place the adjusted picture exists as bytes: on screen it is a
/// shader, and the frame the upload path holds is the one from *before* it. See
/// [`PicturePass::render_offscreen`].
#[derive(Clone)]
pub struct RenderedSnapshot {
    /// Pixels across.
    pub width: u32,
    /// Pixels down.
    pub height: u32,
    /// Row-major RGBA, **top row first** — the readback is flipped on the way out.
    pub rgba: Vec<u8>,
}

/// What one offscreen render sends back: the file it is for, and the pixels.
///
/// The pixels are `None` when the render could not be done at all, which the app
/// reports rather than swallowing — a key press that silently does nothing is worse
/// than a message, and the raw frame is one setting away.
pub type SnapshotResult = (std::path::PathBuf, Option<RenderedSnapshot>);

/// The sending end of that hand-off.
///
/// A channel rather than a return value because the render happens inside egui's
/// paint pass, where the app is not reachable; the file is written on the frame
/// after the pixels arrive.
pub type SnapshotSender = crossbeam_channel::Sender<SnapshotResult>;

/// The offscreen render a snapshot is waiting for.
///
/// Owned rather than borrowed for the same reason [`Adjusted`] is: it travels into a
/// paint callback, which egui may run after the function that built it has returned.
#[derive(Clone)]
pub struct SnapshotJob {
    /// The file the pixels belong to.
    pub path: std::path::PathBuf,
    /// Where the render sends them.
    pub sender: SnapshotSender,
}

/// Where each uniform lives, looked up once after the program links.
#[derive(Default)]
struct Locations {
    screen: Option<glow::UniformLocation>,
    frame: Option<glow::UniformLocation>,
    bias: Option<glow::UniformLocation>,
    gamma: Option<glow::UniformLocation>,
    saturation: Option<glow::UniformLocation>,
    sharpness: Option<glow::UniformLocation>,
    enhance: Option<glow::UniformLocation>,
    levels: Option<glow::UniformLocation>,
    texel: Option<glow::UniformLocation>,
}

/// The picture pass: one program, one quad, no state of its own between frames.
pub struct PicturePass {
    program: Option<glow::Program>,
    vao: Option<glow::VertexArray>,
    vbo: Option<glow::Buffer>,
    locations: Locations,
    /// `true` when the program could not be built. The renderer then keeps the
    /// untouched path for the rest of the session, which is the only sane answer to a
    /// driver that refuses the shader.
    pub failed: bool,
}

/// Everything a paint callback needs to draw one adjusted frame.
///
/// Owned rather than borrowed because a callback has to be `'static`: it is handed to
/// egui, which may run it after the function that built it has returned.
#[derive(Clone)]
pub struct Adjusted {
    /// The program, shared with the app rather than rebuilt per frame.
    pub pass: Arc<PicturePass>,
    /// What the sliders and the analysis decided, already smoothed.
    pub uniforms: PictureUniforms,
    /// Size of the frame texture in pixels, which is what the sharpening radius is
    /// measured against.
    pub frame_size: [f32; 2],
}

/// Bytes per vertex: two floats of position, two of texture coordinate.
const VERTEX_STRIDE: i32 = 16;

impl PicturePass {
    /// Build the program and the quad.
    ///
    /// Never fails: an error is logged and [`PicturePass::failed`] is set, because the
    /// player has to keep showing the picture either way.
    pub fn new(gl: Arc<glow::Context>) -> Self {
        let mut pass = Self {
            program: None,
            vao: None,
            vbo: None,
            locations: Locations::default(),
            failed: false,
        };
        if let Err(error) = pass.build(&gl) {
            log::error!("画面调节着色器不可用，已回退到原始渲染路径: {error}");
            pass.failed = true;
        }
        pass
    }

    /// Compile, link, look up the uniforms and build the quad.
    fn build(&mut self, gl: &glow::Context) -> Result<(), String> {
        unsafe {
            let vertex = compile(gl, glow::VERTEX_SHADER, VERTEX_SRC)?;
            let fragment = compile(gl, glow::FRAGMENT_SHADER, FRAGMENT_SRC)?;
            let program = gl.create_program().map_err(|error| error.to_string())?;
            gl.attach_shader(program, vertex);
            gl.attach_shader(program, fragment);
            gl.link_program(program);
            let linked = gl.get_program_link_status(program);
            gl.detach_shader(program, vertex);
            gl.detach_shader(program, fragment);
            gl.delete_shader(vertex);
            gl.delete_shader(fragment);
            if !linked {
                let log = gl.get_program_info_log(program);
                gl.delete_program(program);
                return Err(format!("链接失败: {log}"));
            }

            let uniform = |name: &str| gl.get_uniform_location(program, name);
            self.locations = Locations {
                screen: uniform("u_screen"),
                frame: uniform("u_frame"),
                bias: uniform("u_bias"),
                gamma: uniform("u_gamma"),
                saturation: uniform("u_saturation"),
                sharpness: uniform("u_sharpness"),
                enhance: uniform("u_enhance"),
                levels: uniform("u_levels"),
                texel: uniform("u_texel"),
            };

            let vao = gl.create_vertex_array().map_err(|error| error.to_string())?;
            let vbo = gl.create_buffer().map_err(|error| error.to_string())?;
            gl.bind_vertex_array(Some(vao));
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
            gl.enable_vertex_attrib_array(0);
            gl.vertex_attrib_pointer_f32(0, 2, glow::FLOAT, false, VERTEX_STRIDE, 0);
            gl.enable_vertex_attrib_array(1);
            gl.vertex_attrib_pointer_f32(1, 2, glow::FLOAT, false, VERTEX_STRIDE, 8);
            gl.bind_vertex_array(None);
            gl.bind_buffer(glow::ARRAY_BUFFER, None);

            self.program = Some(program);
            self.vao = Some(vao);
            self.vbo = Some(vbo);
        }
        Ok(())
    }
}

/// Compile one shader stage, keeping the driver's own message when it fails.
fn compile(gl: &glow::Context, kind: u32, source: &str) -> Result<glow::Shader, String> {
    unsafe {
        let shader = gl.create_shader(kind).map_err(|error| error.to_string())?;
        gl.shader_source(shader, source);
        gl.compile_shader(shader);
        if gl.get_shader_compile_status(shader) {
            Ok(shader)
        } else {
            let log = gl.get_shader_info_log(shader);
            gl.delete_shader(shader);
            Err(format!("编译失败: {log}"))
        }
    }
}

/// Vertex stage: one quad, in *physical pixels* with the origin at the top left,
/// flipped into clip space. egui hands a callback its rect in points and the
/// framebuffer counts from the bottom, so both conversions happen here.
const VERTEX_SRC: &str = r#"#version 330 core
layout (location = 0) in vec2 a_pos;
layout (location = 1) in vec2 a_uv;

uniform vec2 u_screen;

out vec2 v_uv;

void main() {
    vec2 ndc = vec2(
        a_pos.x / u_screen.x * 2.0 - 1.0,
        1.0 - a_pos.y / u_screen.y * 2.0
    );
    v_uv = a_uv;
    gl_Position = vec4(ndc, 0.0, 1.0);
}
"#;

/// Fragment stage: the whole chain, in the order the eye expects it.
///
/// Linear light in, sRGB out. At neutral settings the two `pow` calls cancel — but
/// the renderer does not rely on that: neutral means this shader is not installed.
const FRAGMENT_SRC: &str = r#"#version 330 core
in vec2 v_uv;
out vec4 f_color;

uniform sampler2D u_frame;
uniform vec4 u_bias;      // brightness, contrast, temperature, tint
uniform float u_gamma;
uniform float u_saturation;
uniform float u_sharpness;
uniform vec4 u_enhance;   // auto levels, colour boost, denoise, deblock
uniform vec2 u_levels;    // black point, white point
uniform vec2 u_texel;     // 1 / frame resolution

const vec3 LUMA = vec3(0.2126, 0.7152, 0.0722);

vec3 to_linear(vec3 c) { return pow(max(c, 0.0), vec3(2.2)); }
vec3 to_srgb(vec3 c) { return pow(max(c, 0.0), vec3(1.0 / 2.2)); }
vec3 sample_linear(vec2 uv) { return to_linear(texture(u_frame, clamp(uv, 0.0, 1.0)).rgb); }

void main() {
    vec2 uv = clamp(v_uv, 0.0, 1.0);
    vec3 c = sample_linear(uv);

    // White balance as a gain, never as an offset: an offset lifts the black point,
    // and then the letterbox bars change colour with the slider.
    c.r *= 1.0 + u_bias.z;
    c.b *= 1.0 - u_bias.z;
    c.g *= 1.0 + u_bias.w;

    // Auto levels: stretch between the frame's own black and white points, then lift
    // the shadows with it, because a frame that needs the stretch is usually dark.
    if (u_enhance.x > 0.001) {
        c = (c - u_levels.x) / max(u_levels.y - u_levels.x, 0.05);
        c = pow(max(c, 0.0), vec3(1.0 - 0.35 * u_enhance.x));
    }

    // Brightness and contrast around mid grey rather than around 0.5: a scene that is
    // dark on purpose has to stay dark when contrast goes up.
    c = (c - 0.18) * (1.0 + u_bias.y) + 0.18 + u_bias.x;

    c = pow(max(c, 0.0), vec3(1.0 / max(u_gamma, 0.05)));

    // Saturation keeps luminance, so turning colour down does not change exposure.
    float luma = dot(c, LUMA);
    c = mix(vec3(luma), c, max(1.0 + u_saturation + u_enhance.y, 0.0));

    // Sharpening: an unsharp mask that backs off where there is no edge, because what
    // it would amplify there is compression noise.
    if (u_sharpness > 0.001) {
        vec3 blur = 0.25 * (
            sample_linear(uv + vec2(u_texel.x, 0.0)) +
            sample_linear(uv - vec2(u_texel.x, 0.0)) +
            sample_linear(uv + vec2(0.0, u_texel.y)) +
            sample_linear(uv - vec2(0.0, u_texel.y))
        );
        float edge = length(c - blur);
        c += (c - blur) * u_sharpness * (edge > 0.02 ? 1.0 : 0.35);
    }

    // Light, edge-aware smoothing: neighbours that differ a lot are left alone, which
    // is what keeps it off edges and off faces. Denoise and deblock share it — one
    // pass cannot tell a block edge from a real edge without more than a 3x3
    // neighbourhood knows.
    float smoothing = max(u_enhance.z, u_enhance.w);
    if (smoothing > 0.001) {
        vec3 sum = c;
        float weight = 1.0;
        for (int i = 0; i < 4; i++) {
            vec2 offset = i == 0 ? vec2(u_texel.x, 0.0)
                        : i == 1 ? vec2(-u_texel.x, 0.0)
                        : i == 2 ? vec2(0.0, u_texel.y)
                                 : vec2(0.0, -u_texel.y);
            vec3 neighbour = sample_linear(uv + offset);
            float similarity = 1.0 / (1.0 + 40.0 * abs(dot(neighbour, LUMA) - luma));
            sum += neighbour * similarity;
            weight += similarity;
        }
        c = mix(c, sum / weight, clamp(smoothing, 0.0, 1.0) * 0.6);
    }

    f_color = vec4(to_srgb(clamp(c, 0.0, 1.0)), 1.0);
}
"#;

impl PicturePass {
    /// Draw the frame through the shader.
    ///
    /// Called from inside a paint callback, where egui has already set up its own
    /// pipeline — and `egui_glow` re-runs `prepare_painting` after the callback
    /// returns, so this only has to leave the state it touched somewhere sane rather
    /// than exactly where it found it.
    ///
    /// `rect` and `uv` come from the caller's `image_transformed`, which means rotation
    /// and mirroring arrive already applied. The shader never learns that the picture
    /// is turned, and that is what keeps it in step with the untouched path.
    // Every one of these is a separate thing the frame is drawn with, and grouping them
    // into a struct would only move the list: same as `image_transformed`, which carries
    // the geometry for the other path.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &self,
        painter: &Painter,
        info: &PaintCallbackInfo,
        frame: egui::TextureId,
        rect: egui::Rect,
        uv: [[f32; 2]; 4],
        frame_size: [f32; 2],
        uniforms: &PictureUniforms,
    ) {
        let Some(texture) = painter.texture(frame) else {
            // The frame has not reached egui's texture map yet. Drawing nothing is
            // better than drawing something wrong, and the next frame will have it.
            return;
        };
        let gl = painter.gl();
        let ppp = info.pixels_per_point;
        let screen = [info.screen_size_px[0] as f32, info.screen_size_px[1] as f32];

        let (left, top) = (rect.left() * ppp, rect.top() * ppp);
        let (right, bottom) = (rect.right() * ppp, rect.bottom() * ppp);
        // Where the four corners of the quad go, in the order the vertex builder below
        // expects them.
        let corners = [
            (left, top, uv[0]),
            (right, top, uv[1]),
            (right, bottom, uv[2]),
            (left, bottom, uv[3]),
        ];
        let viewport = info.viewport_in_pixels();
        // SAFETY: the scissor is set inside the player's own rectangle and lifted
        // again below; nothing is created or deleted here.
        unsafe {
            gl.enable(glow::SCISSOR_TEST);
            gl.scissor(
                viewport.left_px,
                viewport.from_bottom_px,
                viewport.width_px,
                viewport.height_px,
            );
            gl.disable(glow::BLEND);
        }
        self.draw_quad(gl, screen, corners, texture, frame_size, uniforms);
        // SAFETY: lifting the two states this function changed, which is what egui
        // expects to find when it draws its own shapes after the callback.
        unsafe {
            gl.disable(glow::SCISSOR_TEST);
            gl.enable(glow::BLEND);
        }
    }

    /// Render the frame into a framebuffer of our own and read the pixels back.
    ///
    /// Only ever called for a snapshot that asked to include the adjustments, and
    /// there is no other way to get them: they are a GPU effect, so the bytes the
    /// upload path holds are the frame from *before* them. The cost is one extra draw
    /// at the frame's own resolution plus one readback of the whole picture, paid on
    /// the frame a snapshot was asked for and on no other.
    ///
    /// The quad is drawn at the frame's resolution with the identity UV, so a snapshot
    /// keeps the shape and the orientation it has always had: none of the display's
    /// zoom, rotation or mirroring is baked in, and the sharpening radius is the same
    /// `1 / frame_size` the screen uses.
    pub fn render_offscreen(
        &self,
        painter: &Painter,
        frame: egui::TextureId,
        frame_size: [f32; 2],
        uniforms: &PictureUniforms,
        viewport_px: [i32; 2],
        job: &SnapshotJob,
    ) {
        let width = frame_size[0].round().max(1.0) as i32;
        let height = frame_size[1].round().max(1.0) as i32;
        let gl = painter.gl();
        let Some(texture) = painter.texture(frame) else {
            let _ = job.sender.send((job.path.clone(), None));
            return;
        };

        // SAFETY: the texture and the framebuffer are created and deleted inside this
        // block, and both the framebuffer binding and the viewport are put back before
        // it ends — egui is mid-frame and draws its own shapes after this callback.
        let pixels = unsafe {
            let previous =
                std::num::NonZeroU32::new(gl.get_parameter_i32(glow::FRAMEBUFFER_BINDING) as u32)
                    .map(glow::NativeFramebuffer);
            let mut pixels = None;
            if let (Ok(target), Ok(fbo)) = (gl.create_texture(), gl.create_framebuffer()) {
                gl.bind_texture(glow::TEXTURE_2D, Some(target));
                gl.tex_image_2d(
                    glow::TEXTURE_2D,
                    0,
                    glow::RGBA8 as i32,
                    width,
                    height,
                    0,
                    glow::RGBA,
                    glow::UNSIGNED_BYTE,
                    glow::PixelUnpackData::Slice(None),
                );
                gl.bind_texture(glow::TEXTURE_2D, None);
                gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
                gl.framebuffer_texture_2d(
                    glow::FRAMEBUFFER,
                    glow::COLOR_ATTACHMENT0,
                    glow::TEXTURE_2D,
                    Some(target),
                    0,
                );
                if gl.check_framebuffer_status(glow::FRAMEBUFFER) == glow::FRAMEBUFFER_COMPLETE {
                    gl.viewport(0, 0, width, height);
                    // The framebuffer is fresh, so its contents are undefined: blending
                    // against them would make the snapshot depend on whatever was in that
                    // memory. Nothing here wants blending anyway — the shader writes opaque
                    // pixels — and the scissor belongs to whatever egui was clipping, not
                    // to a texture of our own.
                    gl.disable(glow::SCISSOR_TEST);
                    gl.disable(glow::BLEND);
                    let corners = [
                        (0.0, 0.0, [0.0, 0.0]),
                        (width as f32, 0.0, [1.0, 0.0]),
                        (width as f32, height as f32, [1.0, 1.0]),
                        (0.0, height as f32, [0.0, 1.0]),
                    ];
                    let drawn = self.draw_quad(
                        gl,
                        [width as f32, height as f32],
                        corners,
                        texture,
                        frame_size,
                        uniforms,
                    );
                    if drawn {
                        let mut bytes = vec![0u8; (width * height * 4) as usize];
                        gl.read_pixels(
                            0,
                            0,
                            width,
                            height,
                            glow::RGBA,
                            glow::UNSIGNED_BYTE,
                            glow::PixelPackData::Slice(Some(&mut bytes)),
                        );
                        pixels = Some(bytes);
                    }
                }
                // Back to what `draw` leaves behind for egui's own shapes: blending on,
                // clipping off.
                gl.enable(glow::BLEND);
                gl.bind_framebuffer(glow::FRAMEBUFFER, previous);
                gl.delete_framebuffer(fbo);
                gl.delete_texture(target);
            }
            gl.viewport(0, 0, viewport_px[0], viewport_px[1]);
            pixels
        };

        let Some(bytes) = pixels else {
            let _ = job.sender.send((job.path.clone(), None));
            return;
        };
        // `glReadPixels` hands back the bottom row first and a PNG wants the top row
        // first, so the rows are reversed here rather than making every later reader of
        // the file wonder which way up it is.
        let row = (width * 4) as usize;
        let mut flip = Vec::with_capacity(bytes.len());
        for chunk in bytes.chunks_exact(row).rev() {
            flip.extend_from_slice(chunk);
        }
        let _ = job.sender.send((
            job.path.clone(),
            Some(RenderedSnapshot {
                width: width as u32,
                height: height as u32,
                rgba: flip,
            }),
        ));
    }

    /// Upload the uniforms and draw the quad.
    ///
    /// Shared by the on-screen draw and the offscreen render a snapshot asks for, so
    /// the two can never be told different numbers. `false` when there is nothing to
    /// draw through, which the two callers answer differently: silently on screen, and
    /// with a message for a snapshot.
    fn draw_quad(
        &self,
        gl: &glow::Context,
        screen: [f32; 2],
        corners: [(f32, f32, [f32; 2]); 4],
        texture: glow::Texture,
        frame_size: [f32; 2],
        uniforms: &PictureUniforms,
    ) -> bool {
        let (Some(program), Some(vao), Some(vbo)) = (self.program, self.vao, self.vbo) else {
            return false;
        };

        // Six vertices rather than four plus an index buffer: two triangles whose
        // corners are in the same order the untouched mesh uses.
        let mut vertices = [0.0f32; 24];
        for (slot, index) in [0usize, 1, 2, 0, 2, 3].iter().enumerate() {
            let (x, y, uv) = corners[*index];
            vertices[slot * 4] = x;
            vertices[slot * 4 + 1] = y;
            vertices[slot * 4 + 2] = uv[0];
            vertices[slot * 4 + 3] = uv[1];
        }

        /// Borrow a uniform location, with the lifetime written out: a closure here
        /// cannot express it and the borrow checker says so.
        fn location(slot: &Option<glow::UniformLocation>) -> Option<&glow::UniformLocation> {
            slot.as_ref()
        }

        // SAFETY: the program, the array and the buffer belong to this pass and are
        // unbound again before the block ends; the buffer data is a local array.
        unsafe {
            gl.use_program(Some(program));
            gl.bind_vertex_array(Some(vao));
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
            gl.buffer_data_u8_slice(
                glow::ARRAY_BUFFER,
                bytemuck::cast_slice(&vertices),
                glow::DYNAMIC_DRAW,
            );
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(texture));

            if let Some(slot) = location(&self.locations.screen) {
                gl.uniform_2_f32(Some(slot), screen[0], screen[1]);
            }
            if let Some(slot) = location(&self.locations.frame) {
                gl.uniform_1_i32(Some(slot), 0);
            }
            if let Some(slot) = location(&self.locations.bias) {
                gl.uniform_4_f32(
                    Some(slot),
                    uniforms.bias[0],
                    uniforms.bias[1],
                    uniforms.bias[2],
                    uniforms.bias[3],
                );
            }
            if let Some(slot) = location(&self.locations.gamma) {
                gl.uniform_1_f32(Some(slot), uniforms.gamma);
            }
            if let Some(slot) = location(&self.locations.saturation) {
                gl.uniform_1_f32(Some(slot), uniforms.saturation);
            }
            if let Some(slot) = location(&self.locations.sharpness) {
                gl.uniform_1_f32(Some(slot), uniforms.sharpness);
            }
            if let Some(slot) = location(&self.locations.enhance) {
                gl.uniform_4_f32(
                    Some(slot),
                    uniforms.enhance[0],
                    uniforms.enhance[1],
                    uniforms.enhance[2],
                    uniforms.enhance[3],
                );
            }
            if let Some(slot) = location(&self.locations.levels) {
                gl.uniform_2_f32(Some(slot), uniforms.levels[0], uniforms.levels[1]);
            }
            if let Some(slot) = location(&self.locations.texel) {
                gl.uniform_2_f32(
                    Some(slot),
                    1.0 / frame_size[0].max(1.0),
                    1.0 / frame_size[1].max(1.0),
                );
            }

            gl.draw_arrays(glow::TRIANGLES, 0, 6);

            gl.bind_vertex_array(None);
            gl.bind_buffer(glow::ARRAY_BUFFER, None);
            gl.bind_texture(glow::TEXTURE_2D, None);
            gl.use_program(None);
        }
        true
    }
}
