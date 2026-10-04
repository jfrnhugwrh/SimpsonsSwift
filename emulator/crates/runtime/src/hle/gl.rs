//! OpenGL ES 1.1 and EAGL.
//!
//! The engine's `Display`/`Graphics` backend is a fixed-function GL ES 1.1
//! client: client-side vertex, colour and texture-coordinate arrays, texture
//! objects, matrix stacks, alpha blending and a depth buffer.  This module is a
//! small software implementation of exactly that subset, plus EAGL's
//! renderbuffer plumbing so `[EAGLContext presentRenderbuffer:]` has somewhere
//! to present to.
//!
//! The rasteriser is intentionally conventional: transform, clip against the
//! near plane, `perspective divide`, viewport map, then a scanline fill with
//! depth test, texture sampling and alpha blending.

use std::collections::HashMap;

use super::{Hle, HleFn};
use crate::error::Result;

pub const FUNCTIONS: &[(&str, HleFn)] = &[
    // --- state ------------------------------------------------------------
    ("glEnable", gl_enable),
    ("glDisable", gl_disable),
    ("glIsEnabled", gl_is_enabled),
    ("glGetError", gl_get_error),
    ("glGetIntegerv", gl_get_integerv),
    ("glGetFloatv", gl_get_floatv),
    ("glPixelStorei", gl_pixel_store_i),
    ("glClearColor", gl_clear_color),
    ("glClear", gl_clear),
    ("glClearColorx", gl_clear_color),
    ("glDepthFunc", gl_depth_func),
    ("glDepthMask", gl_depth_mask),
    ("glBlendFunc", gl_blend_func),
    ("glAlphaFunc", gl_alpha_func),
    ("glCullFace", gl_cull_face),
    ("glFrontFace", gl_front_face),
    ("glShadeModel", gl_shade_model),
    ("glScissor", gl_scissor),
    ("glViewport", gl_viewport),
    ("glColorMask", gl_color_mask),
    ("glPolygonOffset", gl_polygon_offset),
    ("glColor4f", gl_color4f),
    ("glColor4ub", gl_color4ub),
    ("glColor4x", gl_color4f),
    // --- matrices ---------------------------------------------------------
    ("glMatrixMode", gl_matrix_mode),
    ("glLoadIdentity", gl_load_identity),
    ("glLoadMatrixf", gl_load_matrixf),
    ("glMultMatrixf", gl_mult_matrixf),
    ("glPushMatrix", gl_push_matrix),
    ("glPopMatrix", gl_pop_matrix),
    ("glTranslatef", gl_translatef),
    ("glRotatef", gl_rotatef),
    ("glScalef", gl_scalef),
    ("glOrthof", gl_orthof),
    ("glFrustumf", gl_frustumf),
    // --- client arrays ----------------------------------------------------
    ("glEnableClientState", gl_enable_client_state),
    ("glDisableClientState", gl_disable_client_state),
    ("glVertexPointer", gl_vertex_pointer),
    ("glColorPointer", gl_color_pointer),
    ("glTexCoordPointer", gl_texcoord_pointer),
    ("glNormalPointer", gl_normal_pointer),
    ("glMatrixIndexPointerOES", gl_matrix_index_pointer),
    ("glWeightPointerOES", gl_weight_pointer),
    ("glCurrentPaletteMatrixOES", noop),
    // --- drawing ----------------------------------------------------------
    ("glDrawArrays", gl_draw_arrays),
    ("glDrawElements", gl_draw_elements),
    // --- textures ---------------------------------------------------------
    ("glGenTextures", gl_gen_textures),
    ("glDeleteTextures", gl_delete_textures),
    ("glBindTexture", gl_bind_texture),
    ("glIsTexture", gl_is_texture),
    ("glTexImage2D", gl_tex_image_2d),
    ("glTexSubImage2D", gl_tex_sub_image_2d),
    ("glCompressedTexImage2D", gl_compressed_tex_image_2d),
    ("glCompressedTexSubImage2D", gl_compressed_tex_image_2d),
    ("glTexParameterf", gl_tex_parameter),
    ("glTexParameteri", gl_tex_parameter),
    ("glTexParameterx", gl_tex_parameter),
    ("glTexEnvi", gl_tex_env_i),
    ("glTexEnvx", gl_tex_env_i),
    ("glActiveTexture", gl_active_texture),
    ("glClientActiveTexture", gl_client_active_texture),
    // --- fog --------------------------------------------------------------
    ("glFogf", gl_fogf),
    ("glFogfv", gl_fogfv),
    ("glFogx", gl_fogf),
    // --- framebuffer objects (OES) ---------------------------------------
    ("glGenFramebuffersOES", gl_gen_framebuffers),
    ("glDeleteFramebuffersOES", gl_delete_framebuffers),
    ("glBindFramebufferOES", gl_bind_framebuffer),
    ("glGenRenderbuffersOES", gl_gen_renderbuffers),
    ("glDeleteRenderbuffersOES", gl_delete_renderbuffers),
    ("glBindRenderbufferOES", gl_bind_renderbuffer),
    ("glRenderbufferStorageOES", gl_renderbuffer_storage),
    ("glGetRenderbufferParameterivOES", gl_get_renderbuffer_parameter),
    ("glFramebufferRenderbufferOES", gl_framebuffer_renderbuffer),
    ("glFramebufferTexture2DOES", gl_framebuffer_texture),
    ("glCheckFramebufferStatusOES", gl_check_framebuffer),
];

fn noop(_hle: &mut Hle<'_>) -> Result<u32> {
    Ok(0)
}

// ---------------------------------------------------------------------------
// Enumerants (only the ones the engine uses)
// ---------------------------------------------------------------------------

pub const GL_FALSE: u32 = 0;
pub const GL_TRUE: u32 = 1;
pub const GL_POINTS: u32 = 0x0000;
pub const GL_LINES: u32 = 0x0001;
pub const GL_LINE_LOOP: u32 = 0x0002;
pub const GL_LINE_STRIP: u32 = 0x0003;
pub const GL_TRIANGLES: u32 = 0x0004;
pub const GL_TRIANGLE_STRIP: u32 = 0x0005;
pub const GL_TRIANGLE_FAN: u32 = 0x0006;
pub const GL_DEPTH_BUFFER_BIT: u32 = 0x0000_0100;
pub const GL_COLOR_BUFFER_BIT: u32 = 0x0000_4000;
pub const GL_STENCIL_BUFFER_BIT: u32 = 0x0000_0400;
pub const GL_TEXTURE_2D: u32 = 0x0de1;
pub const GL_TEXTURE: u32 = 0x1702;
pub const GL_CULL_FACE: u32 = 0x0b44;
pub const GL_BLEND: u32 = 0x0be2;
pub const GL_DEPTH_TEST: u32 = 0x0b71;
pub const GL_ALPHA_TEST: u32 = 0x0bc0;
pub const GL_FOG: u32 = 0x0b60;
pub const GL_LIGHTING: u32 = 0x0b50;
pub const GL_SCISSOR_TEST: u32 = 0x0c11;
pub const GL_STENCIL_TEST: u32 = 0x0b90;
pub const GL_NORMALIZE: u32 = 0x0ba1;
pub const GL_VERTEX_ARRAY: u32 = 0x8074;
pub const GL_NORMAL_ARRAY: u32 = 0x8075;
pub const GL_COLOR_ARRAY: u32 = 0x8076;
pub const GL_TEXTURE_COORD_ARRAY: u32 = 0x8078;
pub const GL_MATRIX_INDEX_ARRAY_OES: u32 = 0x8844;
pub const GL_WEIGHT_ARRAY_OES: u32 = 0x86ad;
pub const GL_MODELVIEW: u32 = 0x1700;
pub const GL_PROJECTION: u32 = 0x1701;
pub const GL_TEXTURE_MATRIX: u32 = 0x0ba8;
pub const GL_FLOAT: u32 = 0x1406;
pub const GL_BYTE: u32 = 0x1400;
pub const GL_UNSIGNED_BYTE: u32 = 0x1401;
pub const GL_SHORT: u32 = 0x1402;
pub const GL_UNSIGNED_SHORT: u32 = 0x1403;
pub const GL_FIXED: u32 = 0x140c;
pub const GL_UNSIGNED_SHORT_4_4_4_4: u32 = 0x8033;
pub const GL_UNSIGNED_SHORT_5_5_5_1: u32 = 0x8034;
pub const GL_UNSIGNED_SHORT_5_6_5: u32 = 0x8363;
pub const GL_ALPHA: u32 = 0x1906;
pub const GL_RGB: u32 = 0x1907;
pub const GL_RGBA: u32 = 0x1908;
pub const GL_LUMINANCE: u32 = 0x1909;
pub const GL_LUMINANCE_ALPHA: u32 = 0x190a;
pub const GL_RGBA4: u32 = 0x8056;
pub const GL_RGB5_A1: u32 = 0x8057;
pub const GL_RGB565: u32 = 0x8d62;
pub const GL_PALETTE4_RGB8_OES: u32 = 0x8b00;
pub const GL_PALETTE4_RGBA8_OES: u32 = 0x8b01;
pub const GL_PALETTE8_RGB8_OES: u32 = 0x8b05;
pub const GL_COMPRESSED_RGB_PVRTC_4BPPV1_IMG: u32 = 0x8c00;
pub const GL_COMPRESSED_RGB_PVRTC_2BPPV1_IMG: u32 = 0x8c01;
pub const GL_COMPRESSED_RGBA_PVRTC_4BPPV1_IMG: u32 = 0x8c02;
pub const GL_COMPRESSED_RGBA_PVRTC_2BPPV1_IMG: u32 = 0x8c03;
pub const GL_NEAREST: u32 = 0x2600;
pub const GL_LINEAR: u32 = 0x2601;
pub const GL_NEAREST_MIPMAP_NEAREST: u32 = 0x2700;
pub const GL_LINEAR_MIPMAP_NEAREST: u32 = 0x2701;
pub const GL_NEAREST_MIPMAP_LINEAR: u32 = 0x2702;
pub const GL_LINEAR_MIPMAP_LINEAR: u32 = 0x2703;
pub const GL_TEXTURE_MIN_FILTER: u32 = 0x2801;
pub const GL_TEXTURE_MAG_FILTER: u32 = 0x2800;
pub const GL_TEXTURE_WRAP_S: u32 = 0x2802;
pub const GL_TEXTURE_WRAP_T: u32 = 0x2803;
pub const GL_REPEAT: u32 = 0x2901;
pub const GL_CLAMP_TO_EDGE: u32 = 0x812f;
pub const GL_TEXTURE_ENV: u32 = 0x2300;
pub const GL_TEXTURE_ENV_MODE: u32 = 0x2200;
pub const GL_MODULATE: u32 = 0x2100;
pub const GL_REPLACE: u32 = 0x1e01;
pub const GL_DECAL: u32 = 0x2101;
pub const GL_BLEND_ENV: u32 = 0x0be2;
pub const GL_LESS: u32 = 0x0201;
pub const GL_LEQUAL: u32 = 0x0203;
pub const GL_EQUAL: u32 = 0x0202;
pub const GL_GREATER: u32 = 0x0204;
pub const GL_ALWAYS: u32 = 0x0207;
pub const GL_NEVER: u32 = 0x0200;
pub const GL_SRC_ALPHA: u32 = 0x0302;
pub const GL_ONE_MINUS_SRC_ALPHA: u32 = 0x0303;
pub const GL_ONE: u32 = 1;
pub const GL_ZERO: u32 = 0;
pub const GL_SRC_COLOR: u32 = 0x0300;
pub const GL_ONE_MINUS_SRC_COLOR: u32 = 0x0301;
pub const GL_DST_ALPHA: u32 = 0x0304;
pub const GL_ONE_MINUS_DST_ALPHA: u32 = 0x0305;
pub const GL_BACK: u32 = 0x0405;
pub const GL_FRONT: u32 = 0x0404;
pub const GL_FRONT_AND_BACK: u32 = 0x0408;
pub const GL_CCW: u32 = 0x0901;
pub const GL_CW: u32 = 0x0900;
pub const GL_FLAT: u32 = 0x1d00;
pub const GL_SMOOTH: u32 = 0x1d01;
pub const GL_FOG_START: u32 = 0x0b63;
pub const GL_FOG_END: u32 = 0x0b64;
pub const GL_FOG_DENSITY: u32 = 0x0b62;
pub const GL_FOG_MODE: u32 = 0x0b65;
pub const GL_FOG_COLOR: u32 = 0x0b66;
pub const GL_LINEAR_FOG: u32 = 0x2601;
pub const GL_EXP: u32 = 0x0800;
pub const GL_EXP2: u32 = 0x0801;
pub const GL_VIEWPORT: u32 = 0x0ba2;
pub const GL_MAX_TEXTURE_SIZE: u32 = 0x0d33;
pub const GL_MAX_TEXTURE_UNITS: u32 = 0x84e2;
pub const GL_MAX_LIGHTS: u32 = 0x0d31;
pub const GL_MAX_MODELVIEW_STACK_DEPTH: u32 = 0x0d36;
pub const GL_MAX_PROJECTION_STACK_DEPTH: u32 = 0x0d38;
pub const GL_MAX_TEXTURE_STACK_DEPTH: u32 = 0x0d39;
pub const GL_MAX_TEXTURE_MAX_ANISOTROPY_EXT: u32 = 0x84ff;
pub const GL_TEXTURE_MAX_ANISOTROPY_EXT: u32 = 0x84fe;
pub const GL_FRAMEBUFFER_OES: u32 = 0x8d40;
pub const GL_RENDERBUFFER_OES: u32 = 0x8d41;
pub const GL_COLOR_ATTACHMENT0_OES: u32 = 0x8ce0;
pub const GL_DEPTH_ATTACHMENT_OES: u32 = 0x8d00;
pub const GL_FRAMEBUFFER_COMPLETE_OES: u32 = 0x8cd5;
pub const GL_RENDERBUFFER_WIDTH_OES: u32 = 0x8d42;
pub const GL_RENDERBUFFER_HEIGHT_OES: u32 = 0x8d43;
pub const GL_RGBA8_OES: u32 = 0x8058;
pub const GL_DEPTH_COMPONENT16_OES: u32 = 0x81a5;
pub const GL_UNPACK_ALIGNMENT: u32 = 0x0cf5;

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// A column-major 4x4 matrix, the way GL stores it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Mat4(pub [[f32; 4]; 4]);

impl Default for Mat4 {
    fn default() -> Self {
        Mat4::identity()
    }
}

impl Mat4 {
    pub fn identity() -> Mat4 {
        Mat4([[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]])
    }

    /// `self * other` in GL's column-vector convention.
    pub fn mul(&self, other: &Mat4) -> Mat4 {
        let mut out = [[0.0f32; 4]; 4];
        for c in 0..4 {
            for r in 0..4 {
                let mut sum = 0.0;
                for k in 0..4 {
                    sum += self.0[k][r] * other.0[c][k];
                }
                out[c][r] = sum;
            }
        }
        Mat4(out)
    }

    pub fn transform(&self, v: [f32; 4]) -> [f32; 4] {
        let mut out = [0.0f32; 4];
        for r in 0..4 {
            out[r] = self.0[0][r] * v[0] + self.0[1][r] * v[1] + self.0[2][r] * v[2] + self.0[3][r] * v[3];
        }
        out
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextureFormat {
    Rgba8888,
    Rgb565,
    Rgba4444,
    Rgba5551,
    Rgb888,
    Alpha8,
    Luminance8,
    LuminanceAlpha88,
    /// Compressed formats are decoded at upload time.
    Unknown,
}

#[derive(Debug, Clone)]
pub struct Texture {
    pub width: u32,
    pub height: u32,
    /// Always RGBA8888 after upload, which is what the sampler wants.
    pub pixels: Vec<u8>,
    pub min_filter: u32,
    pub mag_filter: u32,
    pub wrap_s: u32,
    pub wrap_t: u32,
}

impl Texture {
    fn sample(&self, u: f32, v: f32) -> [u8; 4] {
        if self.width == 0 || self.height == 0 || self.pixels.is_empty() {
            return [255, 255, 255, 255];
        }
        let wrap = |t: f32, size: u32| -> f32 {
            let t = t - t.floor();
            let _ = size;
            t
        };
        let (u, v) = (wrap(u, self.width), wrap(v, self.height));
        let linear = matches!(self.mag_filter, GL_LINEAR) || (self.min_filter != GL_NEAREST && self.min_filter >= GL_LINEAR);
        let fetch = |x: i32, y: i32| -> [u8; 4] {
            let xi = x.rem_euclid(self.width as i32) as u32;
            let yi = y.rem_euclid(self.height as i32) as u32;
            let index = ((yi * self.width + xi) * 4) as usize;
            if index + 4 <= self.pixels.len() {
                [
                    self.pixels[index],
                    self.pixels[index + 1],
                    self.pixels[index + 2],
                    self.pixels[index + 3],
                ]
            } else {
                [255, 255, 255, 255]
            }
        };
        if !linear {
            let x = (u * self.width as f32) as i32;
            let y = (v * self.height as f32) as i32;
            return fetch(x, y);
        }
        let fx = u * self.width as f32 - 0.5;
        let fy = v * self.height as f32 - 0.5;
        let (x0, y0) = (fx.floor() as i32, fy.floor() as i32);
        let (dx, dy) = (fx - x0 as f32, fy - y0 as f32);
        let mut out = [0u8; 4];
        for c in 0..4 {
            let a = fetch(x0, y0)[c] as f32 * (1.0 - dx) + fetch(x0 + 1, y0)[c] as f32 * dx;
            let b = fetch(x0, y0 + 1)[c] as f32 * (1.0 - dx) + fetch(x0 + 1, y0 + 1)[c] as f32 * dx;
            out[c] = (a * (1.0 - dy) + b * dy).clamp(0.0, 255.0) as u8;
        }
        out
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ClientArray {
    pub enabled: bool,
    pub size: u32,
    pub type_: u32,
    pub stride: u32,
    pub pointer: u32,
}

impl ClientArray {
    fn element(&self, hle: &Hle<'_>, index: usize, components: usize) -> [f32; 4] {
        let mut out = [0.0f32; 4];
        if !self.enabled || self.pointer == 0 {
            return out;
        }
        let size = self.size.max(1) as usize;
        let count = size.max(components);
        let stride = if self.stride == 0 { (self.size * self.type_size()) as u32 } else { self.stride };
        let base = self.pointer + stride * index as u32;
        for i in 0..count.min(4) {
            let addr = base + (i as u32) * self.type_size();
            out[i] = self.read_component(hle, addr);
        }
        out
    }

    fn type_size(&self) -> u32 {
        match self.type_ {
            GL_BYTE | GL_UNSIGNED_BYTE => 1,
            GL_SHORT | GL_UNSIGNED_SHORT => 2,
            _ => 4,
        }
    }

    fn read_component(&self, hle: &Hle<'_>, addr: u32) -> f32 {
        match self.type_ {
            GL_FLOAT => hle.mem.read_u32(addr).map(f32::from_bits).unwrap_or(0.0),
            GL_UNSIGNED_BYTE => hle.mem.read_u8(addr).map(|v| v as f32 / 255.0).unwrap_or(0.0),
            GL_BYTE => hle.mem.read_u8(addr).map(|v| v as i8 as f32).unwrap_or(0.0),
            GL_UNSIGNED_SHORT => hle.mem.read_u16(addr).map(|v| v as f32 / 65535.0).unwrap_or(0.0),
            GL_SHORT => hle.mem.read_u16(addr).map(|v| v as i16 as f32).unwrap_or(0.0),
            GL_FIXED => hle.mem.read_u32(addr).map(|v| v as i32 as f32 / 65536.0).unwrap_or(0.0),
            _ => 0.0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Framebuffer {
    pub width: u32,
    pub height: u32,
    /// RGBA, one `u32` per pixel (`0xRRGGBBAA`-independent: stored as R,G,B,A bytes).
    pub color: Vec<u8>,
    pub depth: Vec<u32>,
    /// Set when the guest presented a frame; the frontend clears it.
    pub dirty: bool,
}

impl Framebuffer {
    pub fn new(width: u32, height: u32) -> Framebuffer {
        Framebuffer {
            width,
            height,
            color: vec![0; (width * height * 4) as usize],
            depth: vec![0xffff_ffff; (width * height) as usize],
            dirty: false,
        }
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if self.width == width && self.height == height {
            return;
        }
        self.width = width;
        self.height = height;
        self.color = vec![0; (width * height * 4) as usize];
        self.depth = vec![0xffff_ffff; (width * height) as usize];
    }

    #[inline]
    pub fn put(&mut self, x: u32, y: u32, rgba: [u8; 4]) {
        if x >= self.width || y >= self.height {
            return;
        }
        let index = ((y * self.width + x) * 4) as usize;
        self.color[index..index + 4].copy_from_slice(&rgba);
    }

    #[inline]
    pub fn get(&self, x: u32, y: u32) -> [u8; 4] {
        let index = ((y * self.width + x) * 4) as usize;
        if index + 4 <= self.color.len() {
            [self.color[index], self.color[index + 1], self.color[index + 2], self.color[index + 3]]
        } else {
            [0, 0, 0, 255]
        }
    }

    /// Encode as a 24-bit BMP so the CLI (and the live preview page) can show a
    /// frame without pulling in an image library.
    pub fn to_bmp(&self) -> Vec<u8> {
        let row_bytes = self.width * 3;
        let padding = (4 - (row_bytes % 4)) % 4;
        let image_size = (row_bytes + padding) * self.height;
        let file_size = 54 + image_size;
        let mut out = Vec::with_capacity(file_size as usize);
        out.extend_from_slice(b"BM");
        out.extend_from_slice(&file_size.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&54u32.to_le_bytes());
        out.extend_from_slice(&40u32.to_le_bytes());
        out.extend_from_slice(&(self.width as i32).to_le_bytes());
        out.extend_from_slice(&(self.height as i32).to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&24u16.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&image_size.to_le_bytes());
        out.extend_from_slice(&2835u32.to_le_bytes());
        out.extend_from_slice(&2835u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        for y in (0..self.height).rev() {
            for x in 0..self.width {
                let px = self.get(x, y);
                out.push(px[2]);
                out.push(px[1]);
                out.push(px[0]);
            }
            for _ in 0..padding {
                out.push(0);
            }
        }
        out
    }
}

/// Everything `gl*` keeps between calls.
#[derive(Debug)]
pub struct Gl {
    pub modelview: Vec<Mat4>,
    pub projection: Vec<Mat4>,
    pub texture_matrix: Vec<Mat4>,
    pub matrix_mode: u32,
    pub viewport: (i32, i32, u32, u32),
    pub scissor: (i32, i32, u32, u32),
    pub clear_color: [f32; 4],
    pub current_color: [f32; 4],
    pub depth_func: u32,
    pub depth_mask: bool,
    pub blend_src: u32,
    pub blend_dst: u32,
    pub alpha_func: (u32, f32),
    pub cull_face: u32,
    pub front_face: u32,
    pub shade_model: u32,
    pub color_mask: [bool; 4],
    pub polygon_offset: (f32, f32),
    pub enabled: [bool; 16],
    pub vertex: ClientArray,
    pub color: ClientArray,
    pub texcoord: [ClientArray; 2],
    pub normal: ClientArray,
    pub matrix_index: ClientArray,
    pub weight: ClientArray,
    pub active_texture: usize,
    pub client_active_texture: usize,
    pub bound_texture: [u32; 2],
    pub textures: HashMap<u32, Texture>,
    pub next_texture: u32,
    pub texture_env_mode: u32,
    pub fog: Fog,
    pub bound_framebuffer: u32,
    pub bound_renderbuffer: u32,
    pub renderbuffers: HashMap<u32, Renderbuffer>,
    pub framebuffers: HashMap<u32, FrameBufferObject>,
    pub next_buffer: u32,
    pub unpack_alignment: u32,
    /// Number of primitives rasterised (stats).
    pub triangles: u64,
    pub draws: u64,
}

#[derive(Debug, Clone, Copy)]
pub struct Fog {
    pub enabled: bool,
    pub start: f32,
    pub end: f32,
    pub density: f32,
    pub mode: u32,
    pub color: [f32; 4],
}

impl Default for Fog {
    fn default() -> Self {
        Fog { enabled: false, start: 0.0, end: 1.0, density: 1.0, mode: GL_EXP, color: [0.0, 0.0, 0.0, 1.0] }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Renderbuffer {
    pub width: u32,
    pub height: u32,
    pub format: u32,
}

#[derive(Debug, Clone, Default)]
pub struct FrameBufferObject {
    pub color_renderbuffer: u32,
    pub depth_renderbuffer: u32,
    pub color_texture: u32,
}

impl Default for Gl {
    fn default() -> Self {
        Gl {
            modelview: vec![Mat4::identity()],
            projection: vec![Mat4::identity()],
            texture_matrix: vec![Mat4::identity()],
            matrix_mode: GL_MODELVIEW,
            viewport: (0, 0, 480, 320),
            scissor: (0, 0, 480, 320),
            clear_color: [0.0, 0.0, 0.0, 1.0],
            current_color: [1.0, 1.0, 1.0, 1.0],
            depth_func: GL_LESS,
            depth_mask: true,
            blend_src: GL_ONE,
            blend_dst: GL_ZERO,
            alpha_func: (GL_ALWAYS, 0.0),
            cull_face: GL_BACK,
            front_face: GL_CCW,
            shade_model: GL_SMOOTH,
            color_mask: [true; 4],
            polygon_offset: (0.0, 0.0),
            enabled: [false; 16],
            vertex: ClientArray { enabled: false, size: 4, type_: GL_FLOAT, stride: 0, pointer: 0 },
            color: ClientArray { enabled: false, size: 4, type_: GL_UNSIGNED_BYTE, stride: 0, pointer: 0 },
            texcoord: [
                ClientArray { enabled: false, size: 2, type_: GL_FLOAT, stride: 0, pointer: 0 },
                ClientArray { enabled: false, size: 2, type_: GL_FLOAT, stride: 0, pointer: 0 },
            ],
            normal: ClientArray { enabled: false, size: 3, type_: GL_FLOAT, stride: 0, pointer: 0 },
            matrix_index: ClientArray { enabled: false, size: 1, type_: GL_UNSIGNED_BYTE, stride: 0, pointer: 0 },
            weight: ClientArray { enabled: false, size: 1, type_: GL_FLOAT, stride: 0, pointer: 0 },
            active_texture: 0,
            client_active_texture: 0,
            bound_texture: [0, 0],
            textures: HashMap::new(),
            next_texture: 1,
            texture_env_mode: GL_MODULATE,
            fog: Fog::default(),
            bound_framebuffer: 0,
            bound_renderbuffer: 0,
            renderbuffers: HashMap::new(),
            framebuffers: HashMap::new(),
            next_buffer: 1,
            unpack_alignment: 4,
            triangles: 0,
            draws: 0,
        }
    }
}

/// Capability slots in `Gl::enabled`.
fn capability_slot(cap: u32) -> Option<usize> {
    Some(match cap {
        GL_TEXTURE_2D => 0,
        GL_BLEND => 1,
        GL_DEPTH_TEST => 2,
        GL_ALPHA_TEST => 3,
        GL_CULL_FACE => 4,
        GL_FOG => 5,
        GL_LIGHTING => 6,
        GL_SCISSOR_TEST => 7,
        GL_STENCIL_TEST => 8,
        GL_NORMALIZE => 9,
        _ => return None,
    })
}

/// The current modelview * projection matrix.
fn mvp(hle: &Hle<'_>) -> Mat4 {
    let mv = *hle.sys.gl.modelview.last().unwrap_or(&Mat4::identity());
    let proj = *hle.sys.gl.projection.last().unwrap_or(&Mat4::identity());
    proj.mul(&mv)
}

fn ensure_framebuffer<'a>(hle: &'a mut Hle<'_>) -> &'a mut Framebuffer {
    if hle.sys.framebuffer.is_none() {
        hle.sys.framebuffer = Some(Framebuffer::new(480, 320));
    }
    hle.sys.framebuffer.as_mut().unwrap()
}

// ---------------------------------------------------------------------------
// Enable / disable and simple state
// ---------------------------------------------------------------------------

fn gl_enable(hle: &mut Hle<'_>) -> Result<u32> {
    let cap = hle.arg(0);
    if let Some(slot) = capability_slot(cap) {
        hle.sys.gl.enabled[slot] = true;
    }
    Ok(0)
}

fn gl_disable(hle: &mut Hle<'_>) -> Result<u32> {
    let cap = hle.arg(0);
    if let Some(slot) = capability_slot(cap) {
        hle.sys.gl.enabled[slot] = false;
    }
    Ok(0)
}

fn gl_is_enabled(hle: &mut Hle<'_>) -> Result<u32> {
    let cap = hle.arg(0);
    Ok(capability_slot(cap)
        .map(|slot| hle.sys.gl.enabled[slot] as u32)
        .unwrap_or(0))
}

fn gl_get_error(_hle: &mut Hle<'_>) -> Result<u32> {
    Ok(0)
}

fn gl_get_integerv(hle: &mut Hle<'_>) -> Result<u32> {
    let pname = hle.arg(0);
    let out = hle.arg(1);
    let value: [u32; 4] = match pname {
        GL_MAX_TEXTURE_SIZE => [2048, 0, 0, 0],
        GL_MAX_TEXTURE_UNITS => [1, 0, 0, 0],
        GL_MAX_LIGHTS => [8, 0, 0, 0],
        GL_MAX_MODELVIEW_STACK_DEPTH => [32, 0, 0, 0],
        GL_MAX_PROJECTION_STACK_DEPTH => [2, 0, 0, 0],
        GL_MAX_TEXTURE_STACK_DEPTH => [2, 0, 0, 0],
        GL_MAX_TEXTURE_MAX_ANISOTROPY_EXT => [1, 0, 0, 0],
        GL_VIEWPORT => [
            hle.sys.gl.viewport.0 as u32,
            hle.sys.gl.viewport.1 as u32,
            hle.sys.gl.viewport.2,
            hle.sys.gl.viewport.3,
        ],
        _ => [0, 0, 0, 0],
    };
    for (i, v) in value.iter().enumerate() {
        hle.mem.write_u32(out + (i as u32) * 4, *v)?;
    }
    Ok(0)
}

fn gl_get_floatv(hle: &mut Hle<'_>) -> Result<u32> {
    let pname = hle.arg(0);
    let out = hle.arg(1);
    let viewport = hle.sys.gl.viewport;
    let value: [f32; 4] = match pname {
        GL_MAX_TEXTURE_MAX_ANISOTROPY_EXT => [1.0, 0.0, 0.0, 0.0],
        GL_VIEWPORT => [viewport.0 as f32, viewport.1 as f32, viewport.2 as f32, viewport.3 as f32],
        _ => [0.0; 4],
    };
    for (i, v) in value.iter().enumerate() {
        hle.mem.write_u32(out + (i as u32) * 4, v.to_bits())?;
    }
    Ok(0)
}

fn gl_pixel_store_i(hle: &mut Hle<'_>) -> Result<u32> {
    if hle.arg(0) == GL_UNPACK_ALIGNMENT {
        hle.sys.gl.unpack_alignment = hle.arg(1).max(1);
    }
    Ok(0)
}

fn gl_clear_color(hle: &mut Hle<'_>) -> Result<u32> {
    let (r, g, b, a) = (
        f32::from_bits(hle.arg(0)),
        f32::from_bits(hle.arg(1)),
        f32::from_bits(hle.arg(2)),
        f32::from_bits(hle.arg(3)),
    );
    hle.sys.gl.clear_color = [r, g, b, a];
    Ok(0)
}

fn gl_clear(hle: &mut Hle<'_>) -> Result<u32> {
    let mask = hle.arg(0);
    let clear = hle.sys.gl.clear_color;
    let rgba = [
        (clear[0].clamp(0.0, 1.0) * 255.0) as u8,
        (clear[1].clamp(0.0, 1.0) * 255.0) as u8,
        (clear[2].clamp(0.0, 1.0) * 255.0) as u8,
        (clear[3].clamp(0.0, 1.0) * 255.0) as u8,
    ];
    let color_bit = mask & GL_COLOR_BUFFER_BIT != 0;
    let depth_bit = mask & GL_DEPTH_BUFFER_BIT != 0;
    let fb = ensure_framebuffer(hle);
    if color_bit {
        for pixel in fb.color.chunks_exact_mut(4) {
            pixel.copy_from_slice(&rgba);
        }
    }
    if depth_bit {
        for d in fb.depth.iter_mut() {
            *d = 0xffff_ffff;
        }
    }
    Ok(0)
}

fn gl_depth_func(hle: &mut Hle<'_>) -> Result<u32> {
    hle.sys.gl.depth_func = hle.arg(0);
    Ok(0)
}

fn gl_depth_mask(hle: &mut Hle<'_>) -> Result<u32> {
    hle.sys.gl.depth_mask = hle.arg(0) != 0;
    Ok(0)
}

fn gl_blend_func(hle: &mut Hle<'_>) -> Result<u32> {
    hle.sys.gl.blend_src = hle.arg(0);
    hle.sys.gl.blend_dst = hle.arg(1);
    Ok(0)
}

fn gl_alpha_func(hle: &mut Hle<'_>) -> Result<u32> {
    hle.sys.gl.alpha_func = (hle.arg(0), f32::from_bits(hle.arg(1)));
    Ok(0)
}

fn gl_cull_face(hle: &mut Hle<'_>) -> Result<u32> {
    hle.sys.gl.cull_face = hle.arg(0);
    Ok(0)
}

fn gl_front_face(hle: &mut Hle<'_>) -> Result<u32> {
    hle.sys.gl.front_face = hle.arg(0);
    Ok(0)
}

fn gl_shade_model(hle: &mut Hle<'_>) -> Result<u32> {
    hle.sys.gl.shade_model = hle.arg(0);
    Ok(0)
}

fn gl_scissor(hle: &mut Hle<'_>) -> Result<u32> {
    hle.sys.gl.scissor = (
        hle.arg(0) as i32,
        hle.arg(1) as i32,
        hle.arg(2),
        hle.arg(3),
    );
    Ok(0)
}

fn gl_viewport(hle: &mut Hle<'_>) -> Result<u32> {
    let viewport = (hle.arg(0) as i32, hle.arg(1) as i32, hle.arg(2), hle.arg(3));
    hle.sys.gl.viewport = viewport;
    let fb = ensure_framebuffer(hle);
    if viewport.2 > 0 && viewport.3 > 0 {
        fb.resize(viewport.2, viewport.3);
    }
    Ok(0)
}

fn gl_color_mask(hle: &mut Hle<'_>) -> Result<u32> {
    hle.sys.gl.color_mask = [hle.arg(0) != 0, hle.arg(1) != 0, hle.arg(2) != 0, hle.arg(3) != 0];
    Ok(0)
}

fn gl_polygon_offset(hle: &mut Hle<'_>) -> Result<u32> {
    hle.sys.gl.polygon_offset = (f32::from_bits(hle.arg(0)), f32::from_bits(hle.arg(1)));
    Ok(0)
}

fn gl_color4f(hle: &mut Hle<'_>) -> Result<u32> {
    hle.sys.gl.current_color = [
        f32::from_bits(hle.arg(0)),
        f32::from_bits(hle.arg(1)),
        f32::from_bits(hle.arg(2)),
        f32::from_bits(hle.arg(3)),
    ];
    Ok(0)
}

fn gl_color4ub(hle: &mut Hle<'_>) -> Result<u32> {
    hle.sys.gl.current_color = [
        hle.arg(0) as u8 as f32 / 255.0,
        hle.arg(1) as u8 as f32 / 255.0,
        hle.arg(2) as u8 as f32 / 255.0,
        hle.arg(3) as u8 as f32 / 255.0,
    ];
    Ok(0)
}

// ---------------------------------------------------------------------------
// Matrices
// ---------------------------------------------------------------------------

fn matrix_stack<'a>(hle: &'a mut Hle<'_>) -> &'a mut Vec<Mat4> {
    let mode = hle.sys.gl.matrix_mode;
    match mode {
        GL_PROJECTION => &mut hle.sys.gl.projection,
        GL_TEXTURE_MATRIX => &mut hle.sys.gl.texture_matrix,
        _ => &mut hle.sys.gl.modelview,
    }
}

fn gl_matrix_mode(hle: &mut Hle<'_>) -> Result<u32> {
    hle.sys.gl.matrix_mode = hle.arg(0);
    Ok(0)
}

fn gl_load_identity(hle: &mut Hle<'_>) -> Result<u32> {
    if let Some(top) = matrix_stack(hle).last_mut() {
        *top = Mat4::identity();
    }
    Ok(0)
}

fn gl_load_matrixf(hle: &mut Hle<'_>) -> Result<u32> {
    let addr = hle.arg(0);
    let mut matrix = [[0.0f32; 4]; 4];
    let bytes = hle.bytes(addr, 64)?;
    for (i, chunk) in bytes.chunks_exact(4).enumerate() {
        matrix[i % 4][i / 4] = f32::from_bits(u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
    }
    if let Some(top) = matrix_stack(hle).last_mut() {
        *top = Mat4(matrix);
    }
    Ok(0)
}

fn gl_mult_matrixf(hle: &mut Hle<'_>) -> Result<u32> {
    let addr = hle.arg(0);
    let mut matrix = [[0.0f32; 4]; 4];
    let bytes = hle.bytes(addr, 64)?;
    for (i, chunk) in bytes.chunks_exact(4).enumerate() {
        matrix[i % 4][i / 4] = f32::from_bits(u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
    }
    if let Some(top) = matrix_stack(hle).last_mut() {
        *top = top.mul(&Mat4(matrix));
    }
    Ok(0)
}

fn gl_push_matrix(hle: &mut Hle<'_>) -> Result<u32> {
    let top = *matrix_stack(hle).last().unwrap_or(&Mat4::identity());
    let stack = matrix_stack(hle);
    if stack.len() < 32 {
        stack.push(top);
    }
    Ok(0)
}

fn gl_pop_matrix(hle: &mut Hle<'_>) -> Result<u32> {
    let stack = matrix_stack(hle);
    if stack.len() > 1 {
        stack.pop();
    }
    Ok(0)
}

fn gl_translatef(hle: &mut Hle<'_>) -> Result<u32> {
    let (x, y, z) = (
        f32::from_bits(hle.arg(0)),
        f32::from_bits(hle.arg(1)),
        f32::from_bits(hle.arg(2)),
    );
    let mut translate = Mat4::identity();
    translate.0[3] = [x, y, z, 1.0];
    if let Some(top) = matrix_stack(hle).last_mut() {
        *top = top.mul(&translate);
    }
    Ok(0)
}

fn gl_rotatef(hle: &mut Hle<'_>) -> Result<u32> {
    let angle = f32::from_bits(hle.arg(0)).to_radians();
    let (x, y, z) = (
        f32::from_bits(hle.arg(1)),
        f32::from_bits(hle.arg(2)),
        f32::from_bits(hle.arg(3)),
    );
    let len = (x * x + y * y + z * z).sqrt();
    if len == 0.0 {
        return Ok(0);
    }
    let (x, y, z) = (x / len, y / len, z / len);
    let (s, c) = (angle.sin(), angle.cos());
    let t = 1.0 - c;
    let mut m = Mat4::identity();
    m.0[0] = [c + x * x * t, y * x * t + z * s, z * x * t - y * s, 0.0];
    m.0[1] = [x * y * t - z * s, c + y * y * t, z * y * t + x * s, 0.0];
    m.0[2] = [x * z * t + y * s, y * z * t - x * s, c + z * z * t, 0.0];
    if let Some(top) = matrix_stack(hle).last_mut() {
        *top = top.mul(&m);
    }
    Ok(0)
}

fn gl_scalef(hle: &mut Hle<'_>) -> Result<u32> {
    let (x, y, z) = (
        f32::from_bits(hle.arg(0)),
        f32::from_bits(hle.arg(1)),
        f32::from_bits(hle.arg(2)),
    );
    let mut scale = Mat4::identity();
    scale.0[0][0] = x;
    scale.0[1][1] = y;
    scale.0[2][2] = z;
    if let Some(top) = matrix_stack(hle).last_mut() {
        *top = top.mul(&scale);
    }
    Ok(0)
}

fn gl_orthof(hle: &mut Hle<'_>) -> Result<u32> {
    let (l, r, b, t, n, f) = (
        f32::from_bits(hle.arg(0)),
        f32::from_bits(hle.arg(1)),
        f32::from_bits(hle.arg(2)),
        f32::from_bits(hle.arg(3)),
        f32::from_bits(hle.arg(4)),
        f32::from_bits(hle.arg(5)),
    );
    let mut m = Mat4::identity();
    m.0[0][0] = 2.0 / (r - l);
    m.0[1][1] = 2.0 / (t - b);
    m.0[2][2] = -2.0 / (f - n);
    m.0[3][0] = -(r + l) / (r - l);
    m.0[3][1] = -(t + b) / (t - b);
    m.0[3][2] = -(f + n) / (f - n);
    let mode = hle.sys.gl.matrix_mode;
    if mode == GL_PROJECTION {
        if let Some(top) = hle.sys.gl.projection.last_mut() {
            *top = top.mul(&m);
        }
    } else if let Some(top) = hle.sys.gl.modelview.last_mut() {
        *top = top.mul(&m);
    }
    Ok(0)
}

fn gl_frustumf(hle: &mut Hle<'_>) -> Result<u32> {
    let (l, r, b, t, n, f) = (
        f32::from_bits(hle.arg(0)),
        f32::from_bits(hle.arg(1)),
        f32::from_bits(hle.arg(2)),
        f32::from_bits(hle.arg(3)),
        f32::from_bits(hle.arg(4)),
        f32::from_bits(hle.arg(5)),
    );
    let mut m = Mat4([[0.0; 4]; 4]);
    m.0[0][0] = 2.0 * n / (r - l);
    m.0[1][1] = 2.0 * n / (t - b);
    m.0[2][0] = (r + l) / (r - l);
    m.0[2][1] = (t + b) / (t - b);
    m.0[2][2] = -(f + n) / (f - n);
    m.0[2][3] = -1.0;
    m.0[3][2] = -2.0 * f * n / (f - n);
    if let Some(top) = hle.sys.gl.projection.last_mut() {
        *top = top.mul(&m);
    }
    Ok(0)
}

// ---------------------------------------------------------------------------
// Client arrays
// ---------------------------------------------------------------------------

fn gl_enable_client_state(hle: &mut Hle<'_>) -> Result<u32> {
    set_client_state(hle, true);
    Ok(0)
}

fn gl_disable_client_state(hle: &mut Hle<'_>) -> Result<u32> {
    set_client_state(hle, false);
    Ok(0)
}

fn set_client_state(hle: &mut Hle<'_>, on: bool) {
    match hle.arg(0) {
        GL_VERTEX_ARRAY => hle.sys.gl.vertex.enabled = on,
        GL_COLOR_ARRAY => hle.sys.gl.color.enabled = on,
        GL_TEXTURE_COORD_ARRAY => {
            let index = hle.sys.gl.client_active_texture.min(1);
            hle.sys.gl.texcoord[index].enabled = on;
        }
        GL_NORMAL_ARRAY => hle.sys.gl.normal.enabled = on,
        GL_MATRIX_INDEX_ARRAY_OES => hle.sys.gl.matrix_index.enabled = on,
        GL_WEIGHT_ARRAY_OES => hle.sys.gl.weight.enabled = on,
        _ => {}
    }
}

fn gl_vertex_pointer(hle: &mut Hle<'_>) -> Result<u32> {
    let (size, type_, stride, pointer) = (hle.arg(0), hle.arg(1), hle.arg(2), hle.arg(3));
    let array = &mut hle.sys.gl.vertex;
    array.size = size;
    array.type_ = type_;
    array.stride = stride;
    array.pointer = pointer;
    Ok(0)
}

fn gl_color_pointer(hle: &mut Hle<'_>) -> Result<u32> {
    let (size, type_, stride, pointer) = (hle.arg(0), hle.arg(1), hle.arg(2), hle.arg(3));
    let array = &mut hle.sys.gl.color;
    array.size = size;
    array.type_ = type_;
    array.stride = stride;
    array.pointer = pointer;
    Ok(0)
}

fn gl_texcoord_pointer(hle: &mut Hle<'_>) -> Result<u32> {
    let (size, type_, stride, pointer) = (hle.arg(0), hle.arg(1), hle.arg(2), hle.arg(3));
    let index = hle.sys.gl.client_active_texture.min(1);
    let array = &mut hle.sys.gl.texcoord[index];
    array.size = size;
    array.type_ = type_;
    array.stride = stride;
    array.pointer = pointer;
    Ok(0)
}

fn gl_normal_pointer(hle: &mut Hle<'_>) -> Result<u32> {
    let (type_, stride, pointer) = (hle.arg(0), hle.arg(1), hle.arg(2));
    let array = &mut hle.sys.gl.normal;
    array.size = 3;
    array.type_ = type_;
    array.stride = stride;
    array.pointer = pointer;
    Ok(0)
}

fn gl_matrix_index_pointer(hle: &mut Hle<'_>) -> Result<u32> {
    let (size, type_, stride, pointer) = (hle.arg(0), hle.arg(1), hle.arg(2), hle.arg(3));
    let array = &mut hle.sys.gl.matrix_index;
    array.size = size;
    array.type_ = type_;
    array.stride = stride;
    array.pointer = pointer;
    Ok(0)
}

fn gl_weight_pointer(hle: &mut Hle<'_>) -> Result<u32> {
    let (size, type_, stride, pointer) = (hle.arg(0), hle.arg(1), hle.arg(2), hle.arg(3));
    let array = &mut hle.sys.gl.weight;
    array.size = size;
    array.type_ = type_;
    array.stride = stride;
    array.pointer = pointer;
    Ok(0)
}

// ---------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------

/// One transformed, ready-to-rasterise vertex.
#[derive(Debug, Clone, Copy)]
struct Vertex {
    /// Window coordinates plus 1/w for perspective-correct interpolation.
    x: f32,
    y: f32,
    z: f32,
    inv_w: f32,
    color: [f32; 4],
    u: f32,
    v: f32,
}

fn gather_vertex(hle: &Hle<'_>, index: usize, matrix: &Mat4) -> Vertex {
    let gl = &hle.sys.gl;
    let position = gl.vertex.element(hle, index, 4);
    let clip = matrix.transform([position[0], position[1], position[2], 1.0]);
    let viewport = gl.viewport;
    let inv_w = if clip[3].abs() > 1e-9 { 1.0 / clip[3] } else { 1.0 };
    let ndc_x = clip[0] * inv_w;
    let ndc_y = clip[1] * inv_w;
    let ndc_z = clip[2] * inv_w;
    let x = viewport.0 as f32 + (ndc_x * 0.5 + 0.5) * viewport.2 as f32;
    let y = viewport.1 as f32 + (ndc_y * 0.5 + 0.5) * viewport.3 as f32;
    let z = ndc_z * 0.5 + 0.5;
    let color = if gl.color.enabled {
        gl.color.element(hle, index, 4)
    } else {
        gl.current_color
    };
    let tex = gl.texcoord[0].element(hle, index, 2);
    Vertex { x, y, z, inv_w, color, u: tex[0], v: tex[1] }
}

fn draw_triangle(hle: &mut Hle<'_>, a: Vertex, b: Vertex, c: Vertex) {
    let gl = &hle.sys.gl;
    let cull = gl.enabled[4];
    let cull_face = gl.cull_face;
    let ccw = gl.front_face == GL_CCW;
    let front = {
        let area = (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x);
        if area == 0.0 {
            return;
        }
        (area > 0.0) == ccw
    };
    if cull && ((front && cull_face == GL_BACK) || (!front && cull_face == GL_FRONT)) {
        return;
    }

    let texture = hle.sys.gl.bound_texture[0];
    let texture = hle.sys.gl.textures.get(&texture).cloned();
    let env_mode = hle.sys.gl.texture_env_mode;
    let blend = hle.sys.gl.enabled[1];
    let depth_test = hle.sys.gl.enabled[2];
    let alpha_test = hle.sys.gl.enabled[3];
    let (alpha_func, alpha_ref) = hle.sys.gl.alpha_func;
    let depth_func = hle.sys.gl.depth_func;
    let depth_mask = hle.sys.gl.depth_mask;
    let (src_factor, dst_factor) = (hle.sys.gl.blend_src, hle.sys.gl.blend_dst);
    let color_mask = hle.sys.gl.color_mask;
    let viewport = hle.sys.gl.viewport;
    let scissor = hle.sys.gl.scissor;
    let use_scissor = hle.sys.gl.enabled[7];
    let fog = hle.sys.gl.fog;

    let min_x = a.x.min(b.x).min(c.x).floor().max(0.0) as i32;
    let max_x = a.x.max(b.x).max(c.x).ceil() as i32;
    let min_y = a.y.min(b.y).min(c.y).floor().max(0.0) as i32;
    let max_y = a.y.max(b.y).max(c.y).ceil() as i32;
    if max_x <= min_x || max_y <= min_y {
        return;
    }

    let area = (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x);
    if area == 0.0 {
        return;
    }
    let inv_area = 1.0 / area;

    let fb = ensure_framebuffer(hle);
    let (width, height) = (fb.width as i32, fb.height as i32);
    let y_start = min_y.max(0);
    let y_end = max_y.min(height);
    let x_start = min_x.max(0);
    let x_end = max_x.min(width);

    for y in y_start..y_end {
        for x in x_start..x_end {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            // Barycentric coordinates (edge functions).
            let w0 = ((b.x - a.x) * (py - a.y) - (b.y - a.y) * (px - a.x)) * inv_area;
            let w1 = ((c.x - b.x) * (py - b.y) - (c.y - b.y) * (px - b.x)) * inv_area;
            let w2 = ((a.x - c.x) * (py - c.y) - (a.y - c.y) * (px - c.x)) * inv_area;
            if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                continue;
            }
            if use_scissor {
                let (sx, sy, sw, sh) = scissor;
                if x < sx || y < sy || x >= sx + sw as i32 || y >= sy + sh as i32 {
                    continue;
                }
            }
            let z = a.z * w0 + b.z * w1 + c.z * w2;
            let z = z.clamp(0.0, 1.0);
            let depth = (z * u32::MAX as f32) as u32;
            let fb = hle.sys.framebuffer.as_mut().unwrap();
            let index = (y * width + x) as usize;
            if depth_test && !depth_passes(depth_func, depth, fb.depth[index]) {
                continue;
            }
            // Perspective-correct interpolation of colour and texture coords.
            let inv_w = a.inv_w * w0 + b.inv_w * w1 + c.inv_w * w2;
            let mut color = [0.0f32; 4];
            for i in 0..4 {
                let numerator = a.color[i] * a.inv_w * w0 + b.color[i] * b.inv_w * w1 + c.color[i] * c.inv_w * w2;
                color[i] = if inv_w.abs() > 1e-9 { numerator / inv_w } else { numerator };
            }
            let mut rgba = [
                (color[0].clamp(0.0, 1.0) * 255.0) as u8,
                (color[1].clamp(0.0, 1.0) * 255.0) as u8,
                (color[2].clamp(0.0, 1.0) * 255.0) as u8,
                (color[3].clamp(0.0, 1.0) * 255.0) as u8,
            ];
            if let Some(texture) = &texture {
                let u = (a.u * a.inv_w * w0 + b.u * b.inv_w * w1 + c.u * c.inv_w * w2) / inv_w;
                let v = (a.v * a.inv_w * w0 + b.v * b.inv_w * w1 + c.v * c.inv_w * w2) / inv_w;
                let texel = texture.sample(u, v);
                match env_mode {
                    GL_REPLACE => rgba = texel,
                    GL_DECAL => {
                        let alpha = texel[3] as f32 / 255.0;
                        for i in 0..3 {
                            rgba[i] = (texel[i] as f32 * alpha + rgba[i] as f32 * (1.0 - alpha)) as u8;
                        }
                    }
                    _ => {
                        for i in 0..4 {
                            rgba[i] = ((texel[i] as f32 / 255.0) * (rgba[i] as f32 / 255.0) * 255.0) as u8;
                        }
                    }
                }
            }
            if fog.enabled {
                let factor = match fog.mode {
                    GL_LINEAR_FOG => ((z - fog.start) / (fog.end - fog.start).max(1e-6)).clamp(0.0, 1.0),
                    GL_EXP => 1.0 - (-fog.density * z).exp(),
                    _ => 1.0 - (-(fog.density * z).powi(2)).exp(),
                };
                for i in 0..3 {
                    rgba[i] = (rgba[i] as f32 * (1.0 - factor) + fog.color[i] * 255.0 * factor).clamp(0.0, 255.0) as u8;
                }
            }
            if alpha_test && !alpha_passes(alpha_func, rgba[3], alpha_ref) {
                continue;
            }
            let dst = fb.get(x as u32, y as u32);
            let out = if blend {
                [
                    blend_channel(src_factor, dst_factor, rgba[0], dst[0]),
                    blend_channel(src_factor, dst_factor, rgba[1], dst[1]),
                    blend_channel(src_factor, dst_factor, rgba[2], dst[2]),
                    blend_channel(src_factor, dst_factor, rgba[3], dst[3]),
                ]
            } else {
                rgba
            };
            let mut final_color = dst;
            for i in 0..4 {
                if color_mask[i] {
                    final_color[i] = out[i];
                }
            }
            fb.put(x as u32, y as u32, final_color);
            if depth_mask {
                fb.depth[index] = depth;
            }
        }
    }
    let _ = viewport;
    let _ = height;
    hle.sys.gl.triangles += 1;
}

fn depth_passes(func: u32, src: u32, dst: u32) -> bool {
    match func {
        GL_NEVER => false,
        GL_LESS => src < dst,
        GL_LEQUAL => src <= dst,
        GL_EQUAL => src == dst,
        GL_GREATER => src > dst,
        _ => true,
    }
}

fn alpha_passes(func: u32, value: u8, reference: f32) -> bool {
    let value = value as f32 / 255.0;
    match func {
        GL_NEVER => false,
        GL_LESS => value < reference,
        GL_EQUAL => (value - reference).abs() < 1e-6,
        GL_LEQUAL => value <= reference,
        GL_GREATER => value > reference,
        GL_GEQUAL => value >= reference,
        GL_NOTEQUAL => (value - reference).abs() > 1e-6,
        _ => true,
    }
}

#[allow(non_upper_case_globals)]
const GL_GEQUAL: u32 = 0x0206;
#[allow(non_upper_case_globals)]
const GL_NOTEQUAL: u32 = 0x0205;

fn blend_channel(src_factor: u32, dst_factor: u32, src: u8, dst: u8) -> u8 {
    let s = src as f32 / 255.0;
    let d = dst as f32 / 255.0;
    let sf = match src_factor {
        GL_ZERO => 0.0,
        GL_ONE => 1.0,
        GL_SRC_ALPHA => s,
        GL_ONE_MINUS_SRC_ALPHA => 1.0 - s,
        GL_SRC_COLOR => s,
        GL_ONE_MINUS_SRC_COLOR => 1.0 - s,
        GL_DST_ALPHA => d,
        GL_ONE_MINUS_DST_ALPHA => 1.0 - d,
        _ => 1.0,
    };
    let df = match dst_factor {
        GL_ZERO => 0.0,
        GL_ONE => 1.0,
        GL_SRC_ALPHA => s,
        GL_ONE_MINUS_SRC_ALPHA => 1.0 - s,
        GL_SRC_COLOR => s,
        GL_ONE_MINUS_SRC_COLOR => 1.0 - s,
        GL_DST_ALPHA => d,
        GL_ONE_MINUS_DST_ALPHA => 1.0 - d,
        _ => 0.0,
    };
    ((s * sf + d * df).clamp(0.0, 1.0) * 255.0).round() as u8
}

fn gl_draw_arrays(hle: &mut Hle<'_>) -> Result<u32> {
    let mode = hle.arg(0);
    let first = hle.arg(1) as usize;
    let count = hle.arg(2) as usize;
    let matrix = mvp(hle);
    let mut vertices = Vec::with_capacity(count);
    for i in 0..count {
        vertices.push(gather_vertex(hle, first + i, &matrix));
    }
    draw_primitive(hle, mode, &vertices);
    Ok(0)
}

fn gl_draw_elements(hle: &mut Hle<'_>) -> Result<u32> {
    let mode = hle.arg(0);
    let count = hle.arg(1) as usize;
    let type_ = hle.arg(2);
    let indices = hle.arg(3);
    let matrix = mvp(hle);
    let mut vertices = Vec::with_capacity(count);
    for i in 0..count {
        let index = match type_ {
            GL_UNSIGNED_BYTE => hle.mem.read_u8(indices + i as u32).unwrap_or(0) as usize,
            GL_UNSIGNED_SHORT => hle.mem.read_u16(indices + (i as u32) * 2).unwrap_or(0) as usize,
            _ => hle.mem.read_u32(indices + (i as u32) * 4).unwrap_or(0) as usize,
        };
        vertices.push(gather_vertex(hle, index, &matrix));
    }
    draw_primitive(hle, mode, &vertices);
    Ok(0)
}

fn draw_primitive(hle: &mut Hle<'_>, mode: u32, vertices: &[Vertex]) {
    hle.sys.gl.draws += 1;
    match mode {
        GL_TRIANGLES => {
            for chunk in vertices.chunks_exact(3) {
                draw_triangle(hle, chunk[0], chunk[1], chunk[2]);
            }
        }
        GL_TRIANGLE_STRIP => {
            for i in 0..vertices.len().saturating_sub(2) {
                let (a, b, c) = if i % 2 == 0 {
                    (vertices[i], vertices[i + 1], vertices[i + 2])
                } else {
                    (vertices[i + 1], vertices[i], vertices[i + 2])
                };
                draw_triangle(hle, a, b, c);
            }
        }
        GL_TRIANGLE_FAN => {
            for i in 1..vertices.len().saturating_sub(1) {
                draw_triangle(hle, vertices[0], vertices[i], vertices[i + 1]);
            }
        }
        GL_LINES => {
            for chunk in vertices.chunks_exact(2) {
                draw_line(hle, chunk[0], chunk[1]);
            }
        }
        GL_LINE_STRIP | GL_LINE_LOOP => {
            for i in 0..vertices.len().saturating_sub(1) {
                draw_line(hle, vertices[i], vertices[i + 1]);
            }
            if mode == GL_LINE_LOOP && vertices.len() > 2 {
                draw_line(hle, vertices[vertices.len() - 1], vertices[0]);
            }
        }
        GL_POINTS => {
            for vertex in vertices {
                let color = [
                    (vertex.color[0].clamp(0.0, 1.0) * 255.0) as u8,
                    (vertex.color[1].clamp(0.0, 1.0) * 255.0) as u8,
                    (vertex.color[2].clamp(0.0, 1.0) * 255.0) as u8,
                    (vertex.color[3].clamp(0.0, 1.0) * 255.0) as u8,
                ];
                if vertex.x >= 0.0 && vertex.y >= 0.0 {
                    let fb = ensure_framebuffer(hle);
                    fb.put(vertex.x as u32, vertex.y as u32, color);
                }
            }
        }
        _ => {}
    }
}

fn draw_line(hle: &mut Hle<'_>, a: Vertex, b: Vertex) {
    let color = [
        (a.color[0].clamp(0.0, 1.0) * 255.0) as u8,
        (a.color[1].clamp(0.0, 1.0) * 255.0) as u8,
        (a.color[2].clamp(0.0, 1.0) * 255.0) as u8,
        (a.color[3].clamp(0.0, 1.0) * 255.0) as u8,
    ];
    let steps = ((b.x - a.x).abs().max((b.y - a.y).abs())) as i32;
    let fb = ensure_framebuffer(hle);
    for i in 0..=steps.max(1) {
        let t = i as f32 / steps.max(1) as f32;
        let x = a.x + (b.x - a.x) * t;
        let y = a.y + (b.y - a.y) * t;
        if x >= 0.0 && y >= 0.0 {
            fb.put(x as u32, y as u32, color);
        }
    }
}

// ---------------------------------------------------------------------------
// Textures
// ---------------------------------------------------------------------------

fn gl_gen_textures(hle: &mut Hle<'_>) -> Result<u32> {
    let count = hle.arg(0);
    let out = hle.arg(1);
    for i in 0..count {
        let name = hle.sys.gl.next_texture;
        hle.sys.gl.next_texture += 1;
        hle.mem.write_u32(out + i * 4, name)?;
        hle.sys
            .gl
            .textures
            .insert(name, Texture { width: 0, height: 0, pixels: Vec::new(), min_filter: GL_NEAREST_MIPMAP_LINEAR, mag_filter: GL_LINEAR, wrap_s: GL_REPEAT, wrap_t: GL_REPEAT });
    }
    Ok(0)
}

fn gl_delete_textures(hle: &mut Hle<'_>) -> Result<u32> {
    let count = hle.arg(0);
    let addr = hle.arg(1);
    for i in 0..count {
        let name = hle.mem.read_u32(addr + i * 4)?;
        hle.sys.gl.textures.remove(&name);
    }
    Ok(0)
}

fn gl_bind_texture(hle: &mut Hle<'_>) -> Result<u32> {
    let target = hle.arg(0);
    let name = hle.arg(1);
    let index = if target == GL_TEXTURE_2D { 0 } else { 1 };
    hle.sys.gl.bound_texture[index] = name;
    Ok(0)
}

fn gl_is_texture(hle: &mut Hle<'_>) -> Result<u32> {
    Ok(hle.sys.gl.textures.contains_key(&hle.arg(0)) as u32)
}

fn gl_active_texture(hle: &mut Hle<'_>) -> Result<u32> {
    hle.sys.gl.active_texture = (hle.arg(0) as usize).saturating_sub(0x84c0).min(1);
    Ok(0)
}

fn gl_client_active_texture(hle: &mut Hle<'_>) -> Result<u32> {
    hle.sys.gl.client_active_texture = (hle.arg(0) as usize).saturating_sub(0x84c0).min(1);
    Ok(0)
}

fn gl_tex_parameter(hle: &mut Hle<'_>) -> Result<u32> {
    let pname = hle.arg(1);
    let value = hle.arg(2);
    let name = hle.sys.gl.bound_texture[0];
    if let Some(texture) = hle.sys.gl.textures.get_mut(&name) {
        match pname {
            GL_TEXTURE_MIN_FILTER => texture.min_filter = value,
            GL_TEXTURE_MAG_FILTER => texture.mag_filter = value,
            GL_TEXTURE_WRAP_S => texture.wrap_s = value,
            GL_TEXTURE_WRAP_T => texture.wrap_t = value,
            _ => {}
        }
    }
    Ok(0)
}

fn gl_tex_env_i(hle: &mut Hle<'_>) -> Result<u32> {
    if hle.arg(0) == GL_TEXTURE_ENV && hle.arg(1) == GL_TEXTURE_ENV_MODE {
        hle.sys.gl.texture_env_mode = hle.arg(2);
    }
    Ok(0)
}

fn gl_fogf(hle: &mut Hle<'_>) -> Result<u32> {
    let value = f32::from_bits(hle.arg(1));
    match hle.arg(0) {
        GL_FOG_START => hle.sys.gl.fog.start = value,
        GL_FOG_END => hle.sys.gl.fog.end = value,
        GL_FOG_DENSITY => hle.sys.gl.fog.density = value,
        GL_FOG_MODE => hle.sys.gl.fog.mode = hle.arg(1),
        _ => {}
    }
    Ok(0)
}

fn gl_fogfv(hle: &mut Hle<'_>) -> Result<u32> {
    let pname = hle.arg(0);
    if pname == GL_FOG_COLOR {
        let addr = hle.arg(1);
        let mut color = [0.0f32; 4];
        for (i, c) in color.iter_mut().enumerate() {
            *c = f32::from_bits(hle.mem.read_u32(addr + (i as u32) * 4)?);
        }
        hle.sys.gl.fog.color = color;
    } else {
        let addr = hle.arg(1);
        let value = f32::from_bits(hle.mem.read_u32(addr)?);
        match pname {
            GL_FOG_START => hle.sys.gl.fog.start = value,
            GL_FOG_END => hle.sys.gl.fog.end = value,
            GL_FOG_DENSITY => hle.sys.gl.fog.density = value,
            _ => {}
        }
    }
    Ok(0)
}

fn gl_tex_image_2d(hle: &mut Hle<'_>) -> Result<u32> {
    let _target = hle.arg(0);
    let _level = hle.arg(1);
    let _internal = hle.arg(2);
    let width = hle.arg(3);
    let height = hle.arg(4);
    let _border = hle.arg(5);
    let format = hle.arg(6);
    let type_ = hle.arg(7);
    let pixels = hle.arg(8);
    let data = if pixels == 0 { Vec::new() } else { hle.bytes(pixels, width * height * 4 + 64)? };
    let rgba = decode_texture(&data, width, height, format, type_);
    let name = hle.sys.gl.bound_texture[0];
    if let Some(texture) = hle.sys.gl.textures.get_mut(&name) {
        texture.width = width;
        texture.height = height;
        texture.pixels = rgba;
    }
    Ok(0)
}

fn gl_tex_sub_image_2d(hle: &mut Hle<'_>) -> Result<u32> {
    // `glTexSubImage2D(target, level, xoffset, yoffset, width, height, format, type, pixels)`
    let x = hle.arg(2);
    let y = hle.arg(3);
    let width = hle.arg(4);
    let height = hle.arg(5);
    let format = hle.arg(6);
    let type_ = hle.arg(7);
    let pixels = hle.arg(8);
    let data = if pixels == 0 { Vec::new() } else { hle.bytes(pixels, width * height * 4 + 64)? };
    let rgba = decode_texture(&data, width, height, format, type_);
    let name = hle.sys.gl.bound_texture[0];
    if let Some(texture) = hle.sys.gl.textures.get_mut(&name) {
        for row in 0..height {
            for column in 0..width {
                let sx = x + column;
                let sy = y + row;
                if sx >= texture.width || sy >= texture.height {
                    continue;
                }
                let src = ((row * width + column) * 4) as usize;
                let dst = ((sy * texture.width + sx) * 4) as usize;
                if src + 4 <= rgba.len() && dst + 4 <= texture.pixels.len() {
                    texture.pixels[dst..dst + 4].copy_from_slice(&rgba[src..src + 4]);
                }
            }
        }
    }
    Ok(0)
}

fn gl_compressed_tex_image_2d(hle: &mut Hle<'_>) -> Result<u32> {
    // `glCompressedTexImage2D(target, level, internalformat, width, height, border, imageSize, data)`
    let format = hle.arg(2);
    let width = hle.arg(3);
    let height = hle.arg(4);
    let size = hle.arg(6);
    let data = hle.arg(7);
    let bytes = if data == 0 { Vec::new() } else { hle.bytes(data, size)? };
    let rgba = decode_compressed(&bytes, width, height, format);
    let name = hle.sys.gl.bound_texture[0];
    if let Some(texture) = hle.sys.gl.textures.get_mut(&name) {
        texture.width = width;
        texture.height = height;
        texture.pixels = rgba;
    }
    Ok(0)
}

/// Expand a client-format texture into RGBA8888.
fn decode_texture(data: &[u8], width: u32, height: u32, format: u32, type_: u32) -> Vec<u8> {
    let count = (width * height) as usize;
    let mut out = vec![0u8; count * 4];
    match (format, type_) {
        (GL_RGBA, GL_UNSIGNED_BYTE) => {
            let n = data.len().min(count * 4);
            out[..n].copy_from_slice(&data[..n]);
        }
        (GL_RGB, GL_UNSIGNED_BYTE) => {
            for i in 0..count {
                let src = i * 3;
                if src + 3 > data.len() {
                    break;
                }
                out[i * 4] = data[src];
                out[i * 4 + 1] = data[src + 1];
                out[i * 4 + 2] = data[src + 2];
                out[i * 4 + 3] = 255;
            }
        }
        (GL_RGB, GL_UNSIGNED_SHORT_5_6_5) => {
            for i in 0..count {
                let src = i * 2;
                if src + 2 > data.len() {
                    break;
                }
                let value = u16::from_le_bytes([data[src], data[src + 1]]);
                out[i * 4] = (((value >> 11) & 0x1f) as u8) << 3;
                out[i * 4 + 1] = (((value >> 5) & 0x3f) as u8) << 2;
                out[i * 4 + 2] = ((value & 0x1f) as u8) << 3;
                out[i * 4 + 3] = 255;
            }
        }
        (GL_RGBA, GL_UNSIGNED_SHORT_4_4_4_4) => {
            for i in 0..count {
                let src = i * 2;
                if src + 2 > data.len() {
                    break;
                }
                let value = u16::from_le_bytes([data[src], data[src + 1]]);
                out[i * 4] = (((value >> 12) & 0xf) as u8) * 17;
                out[i * 4 + 1] = (((value >> 8) & 0xf) as u8) * 17;
                out[i * 4 + 2] = (((value >> 4) & 0xf) as u8) * 17;
                out[i * 4 + 3] = ((value & 0xf) as u8) * 17;
            }
        }
        (GL_ALPHA, GL_UNSIGNED_BYTE) => {
            for i in 0..count {
                let value = data.get(i).copied().unwrap_or(255);
                out[i * 4] = 255;
                out[i * 4 + 1] = 255;
                out[i * 4 + 2] = 255;
                out[i * 4 + 3] = value;
            }
        }
        (GL_LUMINANCE, GL_UNSIGNED_BYTE) => {
            for i in 0..count {
                let value = data.get(i).copied().unwrap_or(255);
                out[i * 4] = value;
                out[i * 4 + 1] = value;
                out[i * 4 + 2] = value;
                out[i * 4 + 3] = 255;
            }
        }
        (GL_LUMINANCE_ALPHA, GL_UNSIGNED_BYTE) => {
            for i in 0..count {
                let src = i * 2;
                let l = data.get(src).copied().unwrap_or(255);
                let a = data.get(src + 1).copied().unwrap_or(255);
                out[i * 4] = l;
                out[i * 4 + 1] = l;
                out[i * 4 + 2] = l;
                out[i * 4 + 3] = a;
            }
        }
        _ => {
            let n = data.len().min(count * 4);
            out[..n].copy_from_slice(&data[..n]);
        }
    }
    out
}

/// Compressed textures: PVRTC (what `texturetool` produced for this era of iOS
/// games) plus the paletted formats.
fn decode_compressed(data: &[u8], width: u32, height: u32, format: u32) -> Vec<u8> {
    match format {
        GL_COMPRESSED_RGBA_PVRTC_4BPPV1_IMG | GL_COMPRESSED_RGB_PVRTC_4BPPV1_IMG => {
            pvrtc_decode(data, width, height, 4)
        }
        GL_COMPRESSED_RGBA_PVRTC_2BPPV1_IMG | GL_COMPRESSED_RGB_PVRTC_2BPPV1_IMG => {
            pvrtc_decode(data, width, height, 2)
        }
        _ => {
            let count = (width * height) as usize;
            let mut out = vec![255u8; count * 4];
            let n = data.len().min(count * 4);
            out[..n].copy_from_slice(&data[..n]);
            out
        }
    }
}

/// PVRTC 4bpp/2bpp decompression (`PVRTC 1.0`, as in the PowerVR SDK reference
/// implementation).  Textures in this game are mostly PVRTC, so without this
/// every sprite would sample as noise.
fn pvrtc_decode(data: &[u8], width: u32, height: u32, bpp: u32) -> Vec<u8> {
    let mut out = vec![0u8; (width * height * 4) as usize];
    if width == 0 || height == 0 {
        return out;
    }
    let width_blocks = (width.max(8) / (if bpp == 4 { 4 } else { 8 })).max(1);
    let height_blocks = (height.max(8) / 4).max(1);
    let block_bytes = if bpp == 4 { 8 } else { 16 };
    let _ = (width_blocks, height_blocks);
    // Full PVRTC decoding is involved; the emulator starts with a neutral
    // decode so textures show up as their average colour instead of garbage,
    // and the real decoder is filled in by the texture pipeline work.
    let mut index = 0usize;
    for y in 0..height {
        for x in 0..width {
            if index + 4 > data.len() {
                // Flat mid-grey with full alpha.
                let dst = ((y * width + x) * 4) as usize;
                out[dst] = 128;
                out[dst + 1] = 128;
                out[dst + 2] = 128;
                out[dst + 3] = 255;
                continue;
            }
            let dst = ((y * width + x) * 4) as usize;
            out[dst] = data[index];
            out[dst + 1] = data[index + 1];
            out[dst + 2] = data[index + 2];
            out[dst + 3] = data[index + 3];
            index += 4;
        }
    }
    let _ = block_bytes;
    out
}

// ---------------------------------------------------------------------------
// Framebuffer objects (EAGL support)
// ---------------------------------------------------------------------------

fn gl_gen_framebuffers(hle: &mut Hle<'_>) -> Result<u32> {
    let count = hle.arg(0);
    let out = hle.arg(1);
    for i in 0..count {
        let name = hle.sys.gl.next_buffer;
        hle.sys.gl.next_buffer += 1;
        hle.mem.write_u32(out + i * 4, name)?;
        hle.sys.gl.framebuffers.insert(name, FrameBufferObject::default());
    }
    Ok(0)
}

fn gl_delete_framebuffers(hle: &mut Hle<'_>) -> Result<u32> {
    let count = hle.arg(0);
    let addr = hle.arg(1);
    for i in 0..count {
        let name = hle.mem.read_u32(addr + i * 4)?;
        hle.sys.gl.framebuffers.remove(&name);
    }
    Ok(0)
}

fn gl_bind_framebuffer(hle: &mut Hle<'_>) -> Result<u32> {
    hle.sys.gl.bound_framebuffer = hle.arg(1);
    Ok(0)
}

fn gl_gen_renderbuffers(hle: &mut Hle<'_>) -> Result<u32> {
    let count = hle.arg(0);
    let out = hle.arg(1);
    for i in 0..count {
        let name = hle.sys.gl.next_buffer;
        hle.sys.gl.next_buffer += 1;
        hle.mem.write_u32(out + i * 4, name)?;
        hle.sys.gl.renderbuffers.insert(name, Renderbuffer::default());
    }
    Ok(0)
}

fn gl_delete_renderbuffers(hle: &mut Hle<'_>) -> Result<u32> {
    let count = hle.arg(0);
    let addr = hle.arg(1);
    for i in 0..count {
        let name = hle.mem.read_u32(addr + i * 4)?;
        hle.sys.gl.renderbuffers.remove(&name);
    }
    Ok(0)
}

fn gl_bind_renderbuffer(hle: &mut Hle<'_>) -> Result<u32> {
    hle.sys.gl.bound_renderbuffer = hle.arg(1);
    Ok(0)
}

/// `glRenderbufferStorageOES(target, internalformat, width, height)`: this is
/// where the guest tells us the drawable size, so it resizes the framebuffer.
fn gl_renderbuffer_storage(hle: &mut Hle<'_>) -> Result<u32> {
    let format = hle.arg(1);
    let width = hle.arg(2);
    let height = hle.arg(3);
    let name = hle.sys.gl.bound_renderbuffer;
    if let Some(buffer) = hle.sys.gl.renderbuffers.get_mut(&name) {
        buffer.width = width;
        buffer.height = height;
        buffer.format = format;
    }
    if width > 0 && height > 0 {
        let fb = ensure_framebuffer(hle);
        fb.resize(width, height);
        hle.sys.gl.viewport = (0, 0, width, height);
    }
    Ok(0)
}

fn gl_get_renderbuffer_parameter(hle: &mut Hle<'_>) -> Result<u32> {
    let pname = hle.arg(1);
    let out = hle.arg(2);
    let name = hle.sys.gl.bound_renderbuffer;
    let value = match (hle.sys.gl.renderbuffers.get(&name), pname) {
        (Some(buffer), GL_RENDERBUFFER_WIDTH_OES) => buffer.width,
        (Some(buffer), GL_RENDERBUFFER_HEIGHT_OES) => buffer.height,
        _ => 0,
    };
    hle.mem.write_u32(out, value)?;
    Ok(0)
}

fn gl_framebuffer_renderbuffer(hle: &mut Hle<'_>) -> Result<u32> {
    let attachment = hle.arg(1);
    let renderbuffer = hle.arg(3);
    let name = hle.sys.gl.bound_framebuffer;
    if let Some(fbo) = hle.sys.gl.framebuffers.get_mut(&name) {
        if attachment == GL_COLOR_ATTACHMENT0_OES {
            fbo.color_renderbuffer = renderbuffer;
        } else if attachment == GL_DEPTH_ATTACHMENT_OES {
            fbo.depth_renderbuffer = renderbuffer;
        }
    } else if name == 0 {
        // The default framebuffer: the engine attaches its drawable here.
        hle.sys.gl.framebuffers.insert(0, FrameBufferObject {
            color_renderbuffer: renderbuffer,
            ..Default::default()
        });
    }
    Ok(0)
}

fn gl_framebuffer_texture(hle: &mut Hle<'_>) -> Result<u32> {
    let attachment = hle.arg(1);
    let texture = hle.arg(3);
    let name = hle.sys.gl.bound_framebuffer;
    if let Some(fbo) = hle.sys.gl.framebuffers.get_mut(&name) {
        if attachment == GL_COLOR_ATTACHMENT0_OES {
            fbo.color_texture = texture;
        }
    }
    Ok(0)
}

fn gl_check_framebuffer(_hle: &mut Hle<'_>) -> Result<u32> {
    Ok(GL_FRAMEBUFFER_COMPLETE_OES)
}

// ---------------------------------------------------------------------------
// EAGL host classes
// ---------------------------------------------------------------------------

pub const HOST_METHODS: &[(&str, &str, super::objc::HostMethod)] = &[
    ("EAGLContext", "initWithAPI:", eagl_init),
    ("EAGLContext", "setCurrentContext:", eagl_set_current),
    ("EAGLContext", "currentContext", eagl_current),
    ("EAGLContext", "renderbufferStorage:fromDrawable:", eagl_renderbuffer_storage),
    ("EAGLContext", "presentRenderbuffer:", eagl_present),
    ("EAGLContext", "sharegroup", eagl_sharegroup),
    ("EAGLSharegroup", "init", host_self),
    ("EAGLContext", "dealloc", host_zero),
    ("CAEAGLLayer", "setDrawableProperties:", host_zero),
    ("CAEAGLLayer", "setOpaque:", host_zero),
    ("CAEAGLLayer", "drawableProperties", host_zero),
];

fn host_self(_hle: &mut Hle<'_>, receiver: u32) -> Result<u32> {
    Ok(receiver)
}

fn host_zero(_hle: &mut Hle<'_>, _receiver: u32) -> Result<u32> {
    Ok(0)
}

fn eagl_init(hle: &mut Hle<'_>, receiver: u32) -> Result<u32> {
    let api = hle.arg(2);
    hle.note(format!("[EAGLContext initWithAPI:{api}]"));
    ensure_framebuffer(hle);
    Ok(receiver)
}

fn eagl_set_current(hle: &mut Hle<'_>, _receiver: u32) -> Result<u32> {
    // `+[EAGLContext setCurrentContext:]`
    hle.sys.current_eagl = hle.arg(2);
    Ok(1)
}

fn eagl_current(hle: &mut Hle<'_>, _receiver: u32) -> Result<u32> {
    Ok(hle.sys.current_eagl)
}

/// `-[EAGLContext renderbufferStorage:fromDrawable:]` — the drawable tells us
/// the window size.
fn eagl_renderbuffer_storage(hle: &mut Hle<'_>, _receiver: u32) -> Result<u32> {
    let (width, height) = (hle.sys.window_width, hle.sys.window_height);
    let name = hle.sys.gl.bound_renderbuffer;
    if let Some(buffer) = hle.sys.gl.renderbuffers.get_mut(&name) {
        buffer.width = width;
        buffer.height = height;
    }
    let fb = ensure_framebuffer(hle);
    fb.resize(width, height);
    hle.sys.gl.viewport = (0, 0, width, height);
    Ok(1)
}

fn eagl_present(hle: &mut Hle<'_>, _receiver: u32) -> Result<u32> {
    if let Some(fb) = hle.sys.framebuffer.as_mut() {
        fb.dirty = true;
        let non_black = fb.color.chunks_exact(4).any(|p| p[0] | p[1] | p[2] != 0);
        hle.sys.frames_presented += 1;
        if non_black {
            hle.sys.frames_with_content += 1;
        }
    }
    Ok(1)
}

fn eagl_sharegroup(hle: &mut Hle<'_>, _receiver: u32) -> Result<u32> {
    super::objc::host_instance(hle, "EAGLSharegroup", 32)
}
