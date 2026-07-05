use std::{
    collections::HashMap,
    error::Error,
    ffi::{CString, c_void},
    fmt,
    mem::size_of,
    ptr,
};

use gl::types::{GLchar, GLenum, GLint, GLsizei, GLuint};

pub const LIQUID_GLASS_VERTEX_SHADER: &str = r#"#version 330 core

layout (location = 0) in vec2 pos;
layout (location = 1) in vec2 texCoords;

uniform mat4 u_Proj;
uniform mat4 u_ModelView;
uniform vec2 uMidPoint;
uniform vec2 uQuadNDC2ScreenNDCScale;

out vec2 v_TexCoord;
out vec2 vMidPointNDC;
out vec2 vLocalOffsetNDC;

void main() {
    gl_Position = u_Proj * u_ModelView * vec4(pos, 0.0, 1.0);
    v_TexCoord = texCoords;
    vMidPointNDC = uMidPoint;
    vLocalOffsetNDC = vec2(texCoords.x * 2.0 - 1.0, 1.0 - texCoords.y * 2.0) * uQuadNDC2ScreenNDCScale;
}
"#;

pub const LIQUID_GLASS_FRAGMENT_SHADER: &str = r#"#version 330 core

uniform sampler2D uBlurTex;

uniform float uPowerFactor;
uniform float uNoise;
uniform float uRefractionPower;
uniform float uGlowWeight;
uniform float uGlowBias;
uniform float uGlowEdge0;
uniform float uGlowEdge1;

in vec2 v_TexCoord;
in vec2 vMidPointNDC;
in vec2 vLocalOffsetNDC;

out vec4 color;

const float M_E = 2.718281828459045;
const vec2 CENTER = vec2(0.5);

float rand(vec2 co) {
    return fract(sin(dot(co, vec2(12.9898, 78.233))) * 43758.5453);
}

float f(float x) {
    return 1.0 - 2.3 * pow(5.2 * M_E, -6.9 * x - 0.7);
}

float sdSuperellipse(vec2 p, float n, float r) {
    vec2 absP = abs(p);
    float numerator = pow(absP.x, n) + pow(absP.y, n) - pow(r, n);
    float denominator = n * sqrt(pow(absP.x, 2.0 * n - 2.0) + pow(absP.y, 2.0 * n - 2.0)) + 0.00001;
    return numerator / denominator;
}

float glow(vec2 uv) {
    vec2 glowUV = uv * 2.0 - 1.0;
    return sin(atan(glowUV.y, glowUV.x) - 0.5);
}

vec2 toScreenUV(vec2 ndc) {
    return ndc * 0.5 + vec2(0.5);
}

void main() {
    vec2 localUV = v_TexCoord;
    vec2 p = (localUV - CENTER) * 2.0;

    float d = sdSuperellipse(p, max(uPowerFactor, 1.0), 1.0);
    float edge = 1.0 - smoothstep(-0.003, 0.003, d);

    if (edge <= 0.0) {
        discard;
    }

    float dist = max(-d, 0.0);
    float refraction = pow(f(dist), max(uRefractionPower, 0.0));
    vec2 targetNDC = vMidPointNDC + vLocalOffsetNDC * refraction;
    vec2 sampleUV = clamp(toScreenUV(targetNDC), 0.003, 0.997);

    vec4 glassColor = texture(uBlurTex, sampleUV);

    float noise = (rand(gl_FragCoord.xy * 1e-3) - 0.5) * max(uNoise, 0.0);
    glassColor.rgb += vec3(noise);
    glassColor.rgb *= edge;

    float glowValue = glow(localUV);
    float glowMask = smoothstep(uGlowEdge0, uGlowEdge1, dist);
    float glowStrength = glowValue * uGlowWeight * glowMask + 1.0 + uGlowBias;

    glassColor.rgb *= glowStrength;

    color = vec4(glassColor.rgb, edge);
}
"#;

pub const GAUSSIAN_BLUR_VERTEX_SHADER: &str = r#"#version 330 core

layout (location = 0) in vec2 pos;
layout (location = 1) in vec2 texCoords;

uniform mat4 u_Proj;
uniform mat4 u_ModelView;
uniform vec2 uMidPoint;
uniform vec2 uQuadNDC2ScreenNDCScale;

out vec2 v_TexCoord;
out vec2 vMidPoint;
out vec2 vScreenScale;

void main() {
    gl_Position = u_Proj * u_ModelView * vec4(pos, 0.0, 1.0);
    v_TexCoord = texCoords;
    vMidPoint = uMidPoint * 0.5 + 0.5;
    vScreenScale = uQuadNDC2ScreenNDCScale;
}
"#;

pub const GAUSSIAN_BLUR_FRAGMENT_SHADER: &str = r#"#version 330 core

uniform sampler2D uTexture;

uniform vec2 uResolution;
uniform vec2 uBoxPixelSize;
uniform float uRadius;
uniform float uBlurRadius;
uniform float uSigma;
uniform float uOpacity;
uniform vec4 uTint;

in vec2 v_TexCoord;
in vec2 vMidPoint;
in vec2 vScreenScale;

out vec4 color;

const vec2 CENTER = vec2(0.5);
const int MAX_BLUR_RADIUS = 12;

float sdRoundedRect(vec2 p, vec2 halfSize, float radius) {
    vec2 q = abs(p) - halfSize + vec2(radius);
    return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - radius;
}

vec2 screenUv(vec2 localUV) {
    vec2 p = (localUV - CENTER) * 2.0;
    return vMidPoint + vec2(p.x, -p.y) * vScreenScale * 0.5;
}

void main() {
    vec2 localUV = v_TexCoord;
    vec2 pixelP = (localUV - CENTER) * uBoxPixelSize;
    vec2 halfSize = uBoxPixelSize * 0.5;
    float radius = clamp(uRadius, 0.0, min(halfSize.x, halfSize.y));

    float d = sdRoundedRect(pixelP, halfSize, radius);
    float aa = max(fwidth(d) * 1.5, 0.75);
    float mask = 1.0 - smoothstep(0.0, aa, d);

    if (mask <= 0.0) {
        discard;
    }

    vec2 baseUV = clamp(screenUv(localUV), 0.003, 0.997);
    vec2 texel = 1.0 / max(uResolution, vec2(1.0));
    float blurRadius = clamp(uBlurRadius, 0.0, float(MAX_BLUR_RADIUS));
    float sigma = max(uSigma, 0.001);
    float twoSigmaSq = 2.0 * sigma * sigma;

    vec4 blurred = vec4(0.0);
    float totalWeight = 0.0;

    for (int x = -MAX_BLUR_RADIUS; x <= MAX_BLUR_RADIUS; x++) {
        for (int y = -MAX_BLUR_RADIUS; y <= MAX_BLUR_RADIUS; y++) {
            vec2 offset = vec2(float(x), float(y));
            if (abs(offset.x) > blurRadius || abs(offset.y) > blurRadius) {
                continue;
            }

            float weight = exp(-dot(offset, offset) / twoSigmaSq);
            vec2 sampleUV = clamp(baseUV + offset * texel, 0.003, 0.997);
            blurred += texture(uTexture, sampleUV) * weight;
            totalWeight += weight;
        }
    }

    blurred /= max(totalWeight, 0.0001);
    blurred.rgb = mix(blurred.rgb, uTint.rgb, clamp(uTint.a, 0.0, 1.0));
    color = vec4(blurred.rgb, mask * clamp(uOpacity, 0.0, 1.0));
}
"#;

pub const TEXT_VERTEX_SHADER: &str = r#"#version 330 core

layout (location = 0) in vec4 pos;
layout (location = 1) in vec2 texCoords;
layout (location = 2) in vec4 color;

uniform mat4 u_Proj;
uniform mat4 u_ModelView;

out vec2 v_TexCoord;
out vec4 v_Color;

void main() {
    gl_Position = u_Proj * u_ModelView * pos;

    v_TexCoord = texCoords;
    v_Color = color;
}
"#;

pub const TEXT_FRAGMENT_SHADER: &str = r#"#version 330 core

out vec4 color;

uniform sampler2D u_Texture;

in vec2 v_TexCoord;
in vec4 v_Color;

void main() {
    color = vec4(1.0, 1.0, 1.0, texture(u_Texture, v_TexCoord).r) * v_Color;
}
"#;

pub const MAX_SHADER_BLUR_RADIUS: f32 = 12.0;

pub type Render2DResult<T> = Result<T, Render2DError>;

#[derive(Debug)]
pub enum Render2DError {
    NulInShaderSource { label: String },
    NulInUniformName { name: String },
    ShaderCompile { label: String, log: String },
    ProgramLink { label: String, log: String },
    InvalidMeshBuild(&'static str),
}

impl fmt::Display for Render2DError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NulInShaderSource { label } => {
                write!(
                    f,
                    "shader source for '{label}' contains an interior NUL byte"
                )
            }
            Self::NulInUniformName { name } => {
                write!(f, "uniform name '{name}' contains an interior NUL byte")
            }
            Self::ShaderCompile { label, log } => {
                write!(f, "failed to compile shader '{label}': {log}")
            }
            Self::ProgramLink { label, log } => {
                write!(f, "failed to link shader program '{label}': {log}")
            }
            Self::InvalidMeshBuild(message) => f.write_str(message),
        }
    }
}

impl Error for Render2DError {}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub fn is_empty(self) -> bool {
        self.width <= 0.0 || self.height <= 0.0
    }

    pub fn center(self) -> (f32, f32) {
        (self.x + self.width * 0.5, self.y + self.height * 0.5)
    }

    pub fn contains(self, x: f32, y: f32) -> bool {
        x >= self.x && x <= self.x + self.width && y >= self.y && y <= self.y + self.height
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Color {
    pub const TRANSPARENT: Self = Self {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 0.0,
    };

    pub const WHITE: Self = Self {
        r: 1.0,
        g: 1.0,
        b: 1.0,
        a: 1.0,
    };

    pub const fn rgba(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self { r, g, b, a }
    }

    pub fn from_rgba8(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self {
            r: r as f32 / 255.0,
            g: g as f32 / 255.0,
            b: b as f32 / 255.0,
            a: a as f32 / 255.0,
        }
    }

    pub fn clamped(self) -> Self {
        Self {
            r: self.r.clamp(0.0, 1.0),
            g: self.g.clamp(0.0, 1.0),
            b: self.b.clamp(0.0, 1.0),
            a: self.a.clamp(0.0, 1.0),
        }
    }

    pub fn lerp(start: Self, end: Self, progress: f32) -> Self {
        let progress = progress.clamp(0.0, 1.0);
        Self {
            r: lerp(start.r, end.r, progress),
            g: lerp(start.g, end.g, progress),
            b: lerp(start.b, end.b, progress),
            a: lerp(start.a, end.a, progress),
        }
    }

    pub fn transition(start: Self, end: Self, progress: f32, smooth: bool) -> Self {
        if smooth {
            Self::lerp(start, end, progress)
        } else if progress >= 0.95 {
            end
        } else {
            start
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Mat4 {
    values: [f32; 16],
}

impl Mat4 {
    pub const IDENTITY: Self = Self {
        values: [
            1.0, 0.0, 0.0, 0.0, //
            0.0, 1.0, 0.0, 0.0, //
            0.0, 0.0, 1.0, 0.0, //
            0.0, 0.0, 0.0, 1.0,
        ],
    };

    pub const fn from_column_major(values: [f32; 16]) -> Self {
        Self { values }
    }

    pub fn orthographic_top_left(width: f32, height: f32) -> Self {
        let width = width.max(1.0);
        let height = height.max(1.0);

        Self {
            values: [
                2.0 / width,
                0.0,
                0.0,
                0.0,
                0.0,
                -2.0 / height,
                0.0,
                0.0,
                0.0,
                0.0,
                -1.0,
                0.0,
                -1.0,
                1.0,
                0.0,
                1.0,
            ],
        }
    }

    pub fn as_ptr(&self) -> *const f32 {
        self.values.as_ptr()
    }

    pub const fn as_slice(&self) -> &[f32; 16] {
        &self.values
    }
}

impl Default for Mat4 {
    fn default() -> Self {
        Self::IDENTITY
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Transform2D {
    pub projection: Mat4,
    pub model_view: Mat4,
}

impl Transform2D {
    pub const IDENTITY: Self = Self {
        projection: Mat4::IDENTITY,
        model_view: Mat4::IDENTITY,
    };

    pub fn top_left_pixels(width: f32, height: f32) -> Self {
        Self {
            projection: Mat4::orthographic_top_left(width, height),
            model_view: Mat4::IDENTITY,
        }
    }
}

impl Default for Transform2D {
    fn default() -> Self {
        Self::IDENTITY
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScreenMetrics {
    pub physical_width: f32,
    pub physical_height: f32,
    pub gui_scale: f32,
}

impl ScreenMetrics {
    pub fn new(physical_width: f32, physical_height: f32, gui_scale: f32) -> Self {
        Self {
            physical_width: physical_width.max(1.0),
            physical_height: physical_height.max(1.0),
            gui_scale: gui_scale.max(0.0001),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GaussianBlurOptions {
    pub corner_radius: f32,
    pub blur_radius: f32,
    pub tint: Option<Color>,
    pub opacity: f32,
}

impl Default for GaussianBlurOptions {
    fn default() -> Self {
        Self {
            corner_radius: 30.0,
            blur_radius: 30.0,
            tint: None,
            opacity: 1.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LiquidGlassOptions {
    pub power: f32,
    pub noise: f32,
    pub refraction_power: f32,
    pub radius: f32,
    pub glow_weight: f32,
    pub glow_bias: f32,
    pub glow_edge0: f32,
    pub glow_edge1: f32,
}

impl Default for LiquidGlassOptions {
    fn default() -> Self {
        Self {
            power: 3.0,
            noise: 0.04,
            refraction_power: 1.0,
            radius: 4.0,
            glow_weight: 0.3,
            glow_bias: 0.0,
            glow_edge0: 0.06,
            glow_edge1: 0.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoundedCorners {
    pub top_left: bool,
    pub top_right: bool,
    pub bottom_right: bool,
    pub bottom_left: bool,
}

impl RoundedCorners {
    pub const ALL: Self = Self {
        top_left: true,
        top_right: true,
        bottom_right: true,
        bottom_left: true,
    };

    pub const NONE: Self = Self {
        top_left: false,
        top_right: false,
        bottom_right: false,
        bottom_left: false,
    };

    pub fn all(self) -> bool {
        self.top_left && self.top_right && self.bottom_right && self.bottom_left
    }
}

pub fn load_gl_with<F>(mut loadfn: F)
where
    F: FnMut(&'static str) -> *const c_void,
{
    gl::load_with(|symbol| loadfn(symbol));
}

pub fn setup_render_state() {
    unsafe {
        gl::Enable(gl::BLEND);
        gl::BlendFunc(gl::SRC_ALPHA, gl::ONE_MINUS_SRC_ALPHA);
        gl::Disable(gl::DEPTH_TEST);
        gl::Disable(gl::CULL_FACE);
    }
}

pub fn bind_texture(texture_id: GLuint, slot: u32) {
    unsafe {
        gl::ActiveTexture(gl::TEXTURE0 + slot);
        gl::BindTexture(gl::TEXTURE_2D, texture_id);
    }
}

pub fn reset_texture_slot() {
    unsafe {
        gl::ActiveTexture(gl::TEXTURE0);
    }
}

pub fn default_pixel_store() {
    unsafe {
        gl::PixelStorei(gl::UNPACK_SWAP_BYTES, 0);
        gl::PixelStorei(gl::UNPACK_LSB_FIRST, 0);
        gl::PixelStorei(gl::UNPACK_ROW_LENGTH, 0);
        gl::PixelStorei(gl::UNPACK_IMAGE_HEIGHT, 0);
        gl::PixelStorei(gl::UNPACK_SKIP_ROWS, 0);
        gl::PixelStorei(gl::UNPACK_SKIP_PIXELS, 0);
        gl::PixelStorei(gl::UNPACK_SKIP_IMAGES, 0);
        gl::PixelStorei(gl::UNPACK_ALIGNMENT, 4);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinShader {
    Text,
    LiquidGlass,
    GaussianBlur,
}

impl BuiltinShader {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::LiquidGlass => "liquid_glass",
            Self::GaussianBlur => "gaussian_blur",
        }
    }

    pub const fn sources(self) -> (&'static str, &'static str) {
        match self {
            Self::Text => (TEXT_VERTEX_SHADER, TEXT_FRAGMENT_SHADER),
            Self::LiquidGlass => (LIQUID_GLASS_VERTEX_SHADER, LIQUID_GLASS_FRAGMENT_SHADER),
            Self::GaussianBlur => (GAUSSIAN_BLUR_VERTEX_SHADER, GAUSSIAN_BLUR_FRAGMENT_SHADER),
        }
    }
}

#[derive(Debug)]
pub struct ShaderProgram {
    id: GLuint,
    uniform_locations: HashMap<String, GLint>,
}

impl ShaderProgram {
    pub fn from_builtin(shader: BuiltinShader) -> Render2DResult<Self> {
        let (vertex, fragment) = shader.sources();
        Self::from_source(shader.label(), vertex, fragment)
    }

    pub fn from_source(
        label: impl Into<String>,
        vertex_source: &str,
        fragment_source: &str,
    ) -> Render2DResult<Self> {
        let label = label.into();
        let vertex = compile_shader(gl::VERTEX_SHADER, vertex_source, &format!("{label}.vert"))?;
        let fragment = compile_shader(
            gl::FRAGMENT_SHADER,
            fragment_source,
            &format!("{label}.frag"),
        )?;

        let program = unsafe { gl::CreateProgram() };
        unsafe {
            gl::AttachShader(program, vertex);
            gl::AttachShader(program, fragment);
            gl::LinkProgram(program);
        }

        let mut status = 0;
        unsafe {
            gl::GetProgramiv(program, gl::LINK_STATUS, &mut status);
        }

        unsafe {
            gl::DeleteShader(vertex);
            gl::DeleteShader(fragment);
        }

        if status == 0 {
            let log = program_info_log(program);
            unsafe {
                gl::DeleteProgram(program);
            }
            return Err(Render2DError::ProgramLink { label, log });
        }

        Ok(Self {
            id: program,
            uniform_locations: HashMap::new(),
        })
    }

    pub fn id(&self) -> GLuint {
        self.id
    }

    pub fn bind(&self) {
        unsafe {
            gl::UseProgram(self.id);
        }
    }

    pub fn set_bool(&mut self, name: &str, value: bool) -> Render2DResult<()> {
        self.set_i32(name, i32::from(value))
    }

    pub fn set_i32(&mut self, name: &str, value: i32) -> Render2DResult<()> {
        let location = self.uniform_location(name)?;
        if location >= 0 {
            unsafe {
                gl::Uniform1i(location, value);
            }
        }
        Ok(())
    }

    pub fn set_f32(&mut self, name: &str, value: f32) -> Render2DResult<()> {
        let location = self.uniform_location(name)?;
        if location >= 0 {
            unsafe {
                gl::Uniform1f(location, value);
            }
        }
        Ok(())
    }

    pub fn set_vec2(&mut self, name: &str, x: f32, y: f32) -> Render2DResult<()> {
        let location = self.uniform_location(name)?;
        if location >= 0 {
            unsafe {
                gl::Uniform2f(location, x, y);
            }
        }
        Ok(())
    }

    pub fn set_vec3(&mut self, name: &str, x: f32, y: f32, z: f32) -> Render2DResult<()> {
        let location = self.uniform_location(name)?;
        if location >= 0 {
            unsafe {
                gl::Uniform3f(location, x, y, z);
            }
        }
        Ok(())
    }

    pub fn set_vec4(&mut self, name: &str, x: f32, y: f32, z: f32, w: f32) -> Render2DResult<()> {
        let location = self.uniform_location(name)?;
        if location >= 0 {
            unsafe {
                gl::Uniform4f(location, x, y, z, w);
            }
        }
        Ok(())
    }

    pub fn set_color(&mut self, name: &str, color: Color) -> Render2DResult<()> {
        let color = color.clamped();
        self.set_vec4(name, color.r, color.g, color.b, color.a)
    }

    pub fn set_mat4(&mut self, name: &str, matrix: &Mat4) -> Render2DResult<()> {
        let location = self.uniform_location(name)?;
        if location >= 0 {
            unsafe {
                gl::UniformMatrix4fv(location, 1, gl::FALSE, matrix.as_ptr());
            }
        }
        Ok(())
    }

    pub fn set_defaults(&mut self, transform: Transform2D) -> Render2DResult<()> {
        self.set_mat4("u_Proj", &transform.projection)?;
        self.set_mat4("u_ModelView", &transform.model_view)
    }

    fn uniform_location(&mut self, name: &str) -> Render2DResult<GLint> {
        if let Some(location) = self.uniform_locations.get(name) {
            return Ok(*location);
        }

        let c_name = CString::new(name).map_err(|_| Render2DError::NulInUniformName {
            name: name.to_owned(),
        })?;
        let location = unsafe { gl::GetUniformLocation(self.id, c_name.as_ptr()) };
        self.uniform_locations.insert(name.to_owned(), location);
        Ok(location)
    }
}

impl Drop for ShaderProgram {
    fn drop(&mut self) {
        if self.id != 0 {
            unsafe {
                gl::DeleteProgram(self.id);
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrawMode {
    Lines,
    Triangles,
}

impl DrawMode {
    const fn gl_mode(self) -> GLenum {
        match self {
            Self::Lines => gl::LINES,
            Self::Triangles => gl::TRIANGLES,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attrib {
    Float,
    Vec2,
    Vec3,
    Color,
}

impl Attrib {
    const fn components(self) -> usize {
        match self {
            Self::Float => 1,
            Self::Vec2 => 2,
            Self::Vec3 => 3,
            Self::Color => 4,
        }
    }
}

#[derive(Debug)]
pub struct Mesh {
    draw_mode: DrawMode,
    vao: GLuint,
    vbo: GLuint,
    ibo: GLuint,
    stride_components: usize,
    vertices: Vec<f32>,
    indices: Vec<u32>,
    vertex_count: u32,
    building: bool,
    uploaded_index_count: usize,
}

impl Mesh {
    pub fn new(draw_mode: DrawMode, attributes: &[Attrib]) -> Self {
        assert!(
            !attributes.is_empty(),
            "mesh requires at least one vertex attribute"
        );

        let stride_components = attributes
            .iter()
            .map(|attrib| attrib.components())
            .sum::<usize>();
        let stride_bytes = (stride_components * size_of::<f32>()) as GLsizei;

        let mut vao = 0;
        let mut vbo = 0;
        let mut ibo = 0;

        unsafe {
            gl::GenVertexArrays(1, &mut vao);
            gl::GenBuffers(1, &mut vbo);
            gl::GenBuffers(1, &mut ibo);

            gl::BindVertexArray(vao);
            gl::BindBuffer(gl::ARRAY_BUFFER, vbo);
            gl::BindBuffer(gl::ELEMENT_ARRAY_BUFFER, ibo);

            let mut offset = 0_usize;
            for (index, attribute) in attributes.iter().enumerate() {
                gl::EnableVertexAttribArray(index as GLuint);
                gl::VertexAttribPointer(
                    index as GLuint,
                    attribute.components() as GLint,
                    gl::FLOAT,
                    gl::FALSE,
                    stride_bytes,
                    offset as *const c_void,
                );
                offset += attribute.components() * size_of::<f32>();
            }

            gl::BindVertexArray(0);
            gl::BindBuffer(gl::ARRAY_BUFFER, 0);
            gl::BindBuffer(gl::ELEMENT_ARRAY_BUFFER, 0);
        }

        Self {
            draw_mode,
            vao,
            vbo,
            ibo,
            stride_components,
            vertices: Vec::with_capacity(256 * stride_components),
            indices: Vec::with_capacity(512),
            vertex_count: 0,
            building: false,
            uploaded_index_count: 0,
        }
    }

    pub fn begin(&mut self) {
        self.vertices.clear();
        self.indices.clear();
        self.vertex_count = 0;
        self.uploaded_index_count = 0;
        self.building = true;
    }

    pub fn vec2(&mut self, x: f32, y: f32) -> &mut Self {
        self.vertices.extend_from_slice(&[x, y]);
        self
    }

    pub fn vec3(&mut self, x: f32, y: f32, z: f32) -> &mut Self {
        self.vertices.extend_from_slice(&[x, y, z]);
        self
    }

    pub fn color(&mut self, color: Color) -> &mut Self {
        let color = color.clamped();
        self.vertices
            .extend_from_slice(&[color.r, color.g, color.b, color.a]);
        self
    }

    pub fn next(&mut self) -> Render2DResult<u32> {
        let next_vertex_count = self.vertex_count as usize + 1;
        let expected_components = next_vertex_count * self.stride_components;
        if self.vertices.len() != expected_components {
            return Err(Render2DError::InvalidMeshBuild(
                "vertex attributes do not match the mesh vertex layout",
            ));
        }

        let index = self.vertex_count;
        self.vertex_count += 1;
        Ok(index)
    }

    pub fn line(&mut self, i1: u32, i2: u32) {
        self.indices.extend_from_slice(&[i1, i2]);
    }

    pub fn triangle(&mut self, i1: u32, i2: u32, i3: u32) {
        self.indices.extend_from_slice(&[i1, i2, i3]);
    }

    pub fn quad(&mut self, i1: u32, i2: u32, i3: u32, i4: u32) {
        self.indices.extend_from_slice(&[i1, i2, i3, i3, i4, i1]);
    }

    pub fn end(&mut self) -> Render2DResult<()> {
        if !self.building {
            return Err(Render2DError::InvalidMeshBuild(
                "Mesh::end called while the mesh is not building",
            ));
        }

        self.upload();
        self.building = false;
        Ok(())
    }

    pub fn render(
        &mut self,
        shader: &mut ShaderProgram,
        transform: Transform2D,
    ) -> Render2DResult<()> {
        if self.building {
            self.end()?;
        }

        if self.uploaded_index_count == 0 {
            return Ok(());
        }

        shader.bind();
        shader.set_defaults(transform)?;

        unsafe {
            gl::BindVertexArray(self.vao);
            gl::DrawElements(
                self.draw_mode.gl_mode(),
                self.uploaded_index_count as GLsizei,
                gl::UNSIGNED_INT,
                ptr::null(),
            );
            gl::BindVertexArray(0);
        }

        Ok(())
    }

    fn upload(&mut self) {
        self.uploaded_index_count = self.indices.len();
        if self.vertices.is_empty() || self.indices.is_empty() {
            return;
        }

        unsafe {
            gl::BindVertexArray(self.vao);

            gl::BindBuffer(gl::ARRAY_BUFFER, self.vbo);
            gl::BufferData(
                gl::ARRAY_BUFFER,
                (self.vertices.len() * size_of::<f32>()) as isize,
                self.vertices.as_ptr().cast(),
                gl::DYNAMIC_DRAW,
            );

            gl::BindBuffer(gl::ELEMENT_ARRAY_BUFFER, self.ibo);
            gl::BufferData(
                gl::ELEMENT_ARRAY_BUFFER,
                (self.indices.len() * size_of::<u32>()) as isize,
                self.indices.as_ptr().cast(),
                gl::DYNAMIC_DRAW,
            );

            gl::BindVertexArray(0);
            gl::BindBuffer(gl::ARRAY_BUFFER, 0);
        }
    }
}

impl Drop for Mesh {
    fn drop(&mut self) {
        unsafe {
            if self.ibo != 0 {
                gl::DeleteBuffers(1, &self.ibo);
            }
            if self.vbo != 0 {
                gl::DeleteBuffers(1, &self.vbo);
            }
            if self.vao != 0 {
                gl::DeleteVertexArrays(1, &self.vao);
            }
        }
    }
}

#[derive(Debug)]
pub struct Render2D {
    gaussian_blur_shader: ShaderProgram,
    liquid_glass_shader: ShaderProgram,
    gaussian_blur_mesh: Mesh,
    liquid_glass_mesh: Mesh,
    gaussian_blur_screen_texture: Option<ScreenTexture>,
}

impl Render2D {
    pub fn new() -> Render2DResult<Self> {
        Ok(Self {
            gaussian_blur_shader: ShaderProgram::from_builtin(BuiltinShader::GaussianBlur)?,
            liquid_glass_shader: ShaderProgram::from_builtin(BuiltinShader::LiquidGlass)?,
            gaussian_blur_mesh: Mesh::new(DrawMode::Triangles, &[Attrib::Vec2, Attrib::Vec2]),
            liquid_glass_mesh: Mesh::new(DrawMode::Triangles, &[Attrib::Vec2, Attrib::Vec2]),
            gaussian_blur_screen_texture: None,
        })
    }

    pub fn gaussian_blur_shader(&self) -> &ShaderProgram {
        &self.gaussian_blur_shader
    }

    pub fn gaussian_blur_shader_mut(&mut self) -> &mut ShaderProgram {
        &mut self.gaussian_blur_shader
    }

    pub fn liquid_glass_shader(&self) -> &ShaderProgram {
        &self.liquid_glass_shader
    }

    pub fn liquid_glass_shader_mut(&mut self) -> &mut ShaderProgram {
        &mut self.liquid_glass_shader
    }

    pub fn copy_current_screen_to_gaussian_blur_texture(
        &mut self,
        metrics: ScreenMetrics,
    ) -> GLuint {
        let width = metrics.physical_width.round().max(1.0) as i32;
        let height = metrics.physical_height.round().max(1.0) as i32;
        let texture_id = self.ensure_gaussian_blur_screen_texture(width, height);

        let previous_active_texture = get_integer(gl::ACTIVE_TEXTURE);
        unsafe {
            gl::ActiveTexture(gl::TEXTURE0);
        }
        let previous_texture = get_integer(gl::TEXTURE_BINDING_2D);
        let previous_read_framebuffer = get_integer(gl::READ_FRAMEBUFFER_BINDING);
        let previous_read_buffer = get_integer(gl::READ_BUFFER);

        unsafe {
            gl::BindTexture(gl::TEXTURE_2D, texture_id);
            let current_draw_framebuffer = get_integer(gl::DRAW_FRAMEBUFFER_BINDING);
            gl::BindFramebuffer(gl::READ_FRAMEBUFFER, current_draw_framebuffer as GLuint);
            gl::ReadBuffer(if current_draw_framebuffer == 0 {
                gl::BACK
            } else {
                gl::COLOR_ATTACHMENT0
            });
            gl::CopyTexSubImage2D(gl::TEXTURE_2D, 0, 0, 0, 0, 0, width, height);

            gl::BindFramebuffer(gl::READ_FRAMEBUFFER, previous_read_framebuffer as GLuint);
            gl::ReadBuffer(previous_read_buffer as GLenum);
            gl::BindTexture(gl::TEXTURE_2D, previous_texture as GLuint);
            gl::ActiveTexture(previous_active_texture as GLenum);
        }

        texture_id
    }

    pub fn bind_gaussian_blur(
        &mut self,
        metrics: ScreenMetrics,
        rect: Rect,
        texture_id: GLuint,
        options: GaussianBlurOptions,
    ) -> Render2DResult<()> {
        let screen_width = metrics.physical_width.max(1.0);
        let screen_height = metrics.physical_height.max(1.0);
        let pixel_width = (rect.width * metrics.gui_scale).max(1.0);
        let pixel_height = (rect.height * metrics.gui_scale).max(1.0);
        let pixel_corner_radius = (options.corner_radius * metrics.gui_scale)
            .max(0.0)
            .min(pixel_width.min(pixel_height) * 0.5);
        let pixel_blur_radius = (options.blur_radius * metrics.gui_scale)
            .max(0.0)
            .min(MAX_SHADER_BLUR_RADIUS);
        let (center_x, center_y) = rect.center();
        let center_x = center_x * metrics.gui_scale;
        let center_y = center_y * metrics.gui_scale;
        let tint = options.tint.unwrap_or(Color::TRANSPARENT).clamped();

        self.gaussian_blur_shader.bind();
        bind_texture(texture_id, 0);
        self.gaussian_blur_shader.set_i32("uTexture", 0)?;
        self.gaussian_blur_shader
            .set_vec2("uResolution", screen_width, screen_height)?;
        self.gaussian_blur_shader
            .set_vec2("uBoxPixelSize", pixel_width, pixel_height)?;
        self.gaussian_blur_shader.set_vec2(
            "uMidPoint",
            center_x / screen_width * 2.0 - 1.0,
            1.0 - center_y / screen_height * 2.0,
        )?;
        self.gaussian_blur_shader.set_vec2(
            "uQuadNDC2ScreenNDCScale",
            pixel_width / screen_width,
            pixel_height / screen_height,
        )?;
        self.gaussian_blur_shader
            .set_f32("uRadius", pixel_corner_radius)?;
        self.gaussian_blur_shader
            .set_f32("uBlurRadius", pixel_blur_radius)?;
        self.gaussian_blur_shader
            .set_f32("uSigma", (pixel_blur_radius * 0.5).max(0.5))?;
        self.gaussian_blur_shader
            .set_f32("uOpacity", options.opacity.clamp(0.0, 1.0))?;
        self.gaussian_blur_shader.set_color("uTint", tint)?;

        Ok(())
    }

    pub fn draw_gaussian_blur(
        &mut self,
        metrics: ScreenMetrics,
        transform: Transform2D,
        rect: Rect,
        texture_id: GLuint,
        options: GaussianBlurOptions,
    ) -> Render2DResult<bool> {
        if rect.is_empty() || texture_id == 0 {
            return Ok(false);
        }

        setup_render_state();
        self.bind_gaussian_blur(metrics, rect, texture_id, options)?;
        self.render_gaussian_blur_quad(transform, rect)?;
        bind_texture(0, 0);
        reset_texture_slot();
        Ok(true)
    }

    pub fn draw_gaussian_blur_from_current_screen(
        &mut self,
        metrics: ScreenMetrics,
        transform: Transform2D,
        rect: Rect,
        options: GaussianBlurOptions,
    ) -> Render2DResult<bool> {
        if rect.is_empty() {
            return Ok(false);
        }

        let texture_id = self.copy_current_screen_to_gaussian_blur_texture(metrics);
        self.draw_gaussian_blur(metrics, transform, rect, texture_id, options)
    }

    pub fn draw_gaussian_blur_corners(
        &mut self,
        metrics: ScreenMetrics,
        transform: Transform2D,
        rect: Rect,
        texture_id: GLuint,
        options: GaussianBlurOptions,
        corners: RoundedCorners,
    ) -> Render2DResult<bool> {
        if rect.is_empty() || texture_id == 0 {
            return Ok(false);
        }

        if corners.all() {
            return self.draw_gaussian_blur(metrics, transform, rect, texture_id, options);
        }

        setup_render_state();
        let square_mask_options = GaussianBlurOptions {
            corner_radius: 0.0,
            ..options
        };
        self.bind_gaussian_blur(metrics, rect, texture_id, square_mask_options)?;
        self.render_gaussian_blur_corners_quad(transform, rect, options.corner_radius, corners)?;
        bind_texture(0, 0);
        reset_texture_slot();
        Ok(true)
    }

    pub fn bind_liquid_glass(
        &mut self,
        metrics: ScreenMetrics,
        rect: Rect,
        blur_texture_id: GLuint,
        options: LiquidGlassOptions,
    ) -> Render2DResult<()> {
        let screen_width = metrics.physical_width.max(1.0);
        let screen_height = metrics.physical_height.max(1.0);
        let pixel_width = (rect.width * metrics.gui_scale).max(1.0);
        let pixel_height = (rect.height * metrics.gui_scale).max(1.0);
        let (center_x, center_y) = rect.center();
        let center_x = center_x * metrics.gui_scale;
        let center_y = center_y * metrics.gui_scale;

        self.liquid_glass_shader.bind();
        bind_texture(blur_texture_id, 0);
        self.liquid_glass_shader.set_i32("uBlurTex", 0)?;
        self.liquid_glass_shader.set_vec2(
            "uMidPoint",
            center_x / screen_width * 2.0 - 1.0,
            1.0 - center_y / screen_height * 2.0,
        )?;
        self.liquid_glass_shader.set_vec2(
            "uQuadNDC2ScreenNDCScale",
            pixel_width / screen_width,
            pixel_height / screen_height,
        )?;
        self.liquid_glass_shader
            .set_f32("uPowerFactor", options.power.max(1.0))?;
        self.liquid_glass_shader
            .set_f32("uNoise", options.noise.max(0.0))?;
        self.liquid_glass_shader
            .set_f32("uRefractionPower", options.refraction_power.max(0.0))?;
        self.liquid_glass_shader
            .set_f32("uGlowWeight", options.glow_weight)?;
        self.liquid_glass_shader
            .set_f32("uGlowBias", options.glow_bias)?;
        self.liquid_glass_shader
            .set_f32("uGlowEdge0", options.glow_edge0.clamp(0.0, 1.0))?;
        self.liquid_glass_shader
            .set_f32("uGlowEdge1", options.glow_edge1.clamp(0.0, 1.0))?;

        Ok(())
    }

    pub fn draw_liquid_glass(
        &mut self,
        metrics: ScreenMetrics,
        transform: Transform2D,
        rect: Rect,
        blur_texture_id: GLuint,
        options: LiquidGlassOptions,
    ) -> Render2DResult<bool> {
        if rect.is_empty() || blur_texture_id == 0 {
            return Ok(false);
        }

        setup_render_state();
        self.bind_liquid_glass(metrics, rect, blur_texture_id, options)?;
        self.render_liquid_glass_quad(transform, rect)?;
        bind_texture(0, 0);
        reset_texture_slot();
        Ok(true)
    }

    pub fn render_gaussian_blur_quad(
        &mut self,
        transform: Transform2D,
        rect: Rect,
    ) -> Render2DResult<()> {
        let Self {
            gaussian_blur_shader,
            gaussian_blur_mesh,
            ..
        } = self;

        build_textured_quad(gaussian_blur_mesh, rect)?;
        gaussian_blur_mesh.render(gaussian_blur_shader, transform)
    }

    pub fn render_gaussian_blur_corners_quad(
        &mut self,
        transform: Transform2D,
        rect: Rect,
        radius: f32,
        corners: RoundedCorners,
    ) -> Render2DResult<()> {
        let Self {
            gaussian_blur_shader,
            gaussian_blur_mesh,
            ..
        } = self;

        build_textured_rounded_corner_quad(gaussian_blur_mesh, rect, radius, corners)?;
        gaussian_blur_mesh.render(gaussian_blur_shader, transform)
    }

    pub fn render_liquid_glass_quad(
        &mut self,
        transform: Transform2D,
        rect: Rect,
    ) -> Render2DResult<()> {
        let Self {
            liquid_glass_shader,
            liquid_glass_mesh,
            ..
        } = self;

        build_textured_quad(liquid_glass_mesh, rect)?;
        liquid_glass_mesh.render(liquid_glass_shader, transform)
    }

    fn ensure_gaussian_blur_screen_texture(&mut self, width: i32, height: i32) -> GLuint {
        if self.gaussian_blur_screen_texture.is_none() {
            let mut id = 0;
            unsafe {
                gl::GenTextures(1, &mut id);
            }
            self.gaussian_blur_screen_texture = Some(ScreenTexture {
                id,
                width: 0,
                height: 0,
            });
        }

        let texture = self
            .gaussian_blur_screen_texture
            .as_mut()
            .expect("texture exists");
        unsafe {
            gl::BindTexture(gl::TEXTURE_2D, texture.id);
        }

        if texture.width != width || texture.height != height {
            default_pixel_store();
            unsafe {
                gl::TexParameteri(
                    gl::TEXTURE_2D,
                    gl::TEXTURE_WRAP_S,
                    gl::CLAMP_TO_EDGE as GLint,
                );
                gl::TexParameteri(
                    gl::TEXTURE_2D,
                    gl::TEXTURE_WRAP_T,
                    gl::CLAMP_TO_EDGE as GLint,
                );
                gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MIN_FILTER, gl::LINEAR as GLint);
                gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MAG_FILTER, gl::LINEAR as GLint);
                gl::TexImage2D(
                    gl::TEXTURE_2D,
                    0,
                    gl::RGBA8 as GLint,
                    width,
                    height,
                    0,
                    gl::RGBA,
                    gl::UNSIGNED_BYTE,
                    ptr::null(),
                );
            }
            texture.width = width;
            texture.height = height;
        }

        texture.id
    }
}

#[derive(Debug)]
struct ScreenTexture {
    id: GLuint,
    width: i32,
    height: i32,
}

impl Drop for ScreenTexture {
    fn drop(&mut self) {
        if self.id != 0 {
            unsafe {
                gl::DeleteTextures(1, &self.id);
            }
        }
    }
}

pub fn interpolate(old_value: f64, new_value: f64, interpolation_value: f64) -> f64 {
    old_value + (new_value - old_value) * interpolation_value
}

pub fn is_hovered(mouse_x: f64, mouse_y: f64, x: f64, y: f64, width: f64, height: f64) -> bool {
    mouse_x >= x && mouse_x <= x + width && mouse_y >= y && mouse_y <= y + height
}

pub fn lerp(from: f32, to: f32, progress: f32) -> f32 {
    from + (to - from) * progress
}

fn build_textured_quad(mesh: &mut Mesh, rect: Rect) -> Render2DResult<()> {
    mesh.begin();
    let bottom_left = mesh
        .vec2(rect.x, rect.y + rect.height)
        .vec2(0.0, 1.0)
        .next()?;
    let bottom_right = mesh
        .vec2(rect.x + rect.width, rect.y + rect.height)
        .vec2(1.0, 1.0)
        .next()?;
    let top_right = mesh
        .vec2(rect.x + rect.width, rect.y)
        .vec2(1.0, 0.0)
        .next()?;
    let top_left = mesh.vec2(rect.x, rect.y).vec2(0.0, 0.0).next()?;
    mesh.quad(bottom_left, bottom_right, top_right, top_left);
    mesh.end()
}

fn build_textured_rounded_corner_quad(
    mesh: &mut Mesh,
    rect: Rect,
    radius: f32,
    corners: RoundedCorners,
) -> Render2DResult<()> {
    let safe_radius = radius.max(0.0).min(rect.width.min(rect.height) * 0.5);
    let top_left_radius = if corners.top_left { safe_radius } else { 0.0 };
    let top_right_radius = if corners.top_right { safe_radius } else { 0.0 };
    let bottom_right_radius = if corners.bottom_right {
        safe_radius
    } else {
        0.0
    };
    let bottom_left_radius = if corners.bottom_left {
        safe_radius
    } else {
        0.0
    };

    mesh.begin();
    let center = put_textured_vertex(
        mesh,
        rect,
        rect.x + rect.width * 0.5,
        rect.y + rect.height * 0.5,
    )?;
    let first = put_textured_corner_vertex(
        mesh,
        rect,
        bottom_right_radius,
        0.0,
        rect.x + rect.width - bottom_right_radius,
        rect.y + rect.height - bottom_right_radius,
        rect.x + rect.width,
        rect.y + rect.height,
    )?;
    let mut previous = first;

    previous = append_textured_corner(
        mesh,
        center,
        previous,
        rect,
        bottom_right_radius,
        0.0,
        90.0,
        false,
        rect.x + rect.width - bottom_right_radius,
        rect.y + rect.height - bottom_right_radius,
        rect.x + rect.width,
        rect.y + rect.height,
    )?;
    previous = append_textured_corner(
        mesh,
        center,
        previous,
        rect,
        top_right_radius,
        90.0,
        180.0,
        true,
        rect.x + rect.width - top_right_radius,
        rect.y + top_right_radius,
        rect.x + rect.width,
        rect.y,
    )?;
    previous = append_textured_corner(
        mesh,
        center,
        previous,
        rect,
        top_left_radius,
        180.0,
        270.0,
        true,
        rect.x + top_left_radius,
        rect.y + top_left_radius,
        rect.x,
        rect.y,
    )?;
    previous = append_textured_corner(
        mesh,
        center,
        previous,
        rect,
        bottom_left_radius,
        270.0,
        360.0,
        true,
        rect.x + bottom_left_radius,
        rect.y + rect.height - bottom_left_radius,
        rect.x,
        rect.y + rect.height,
    )?;

    mesh.triangle(center, previous, first);
    mesh.end()
}

#[allow(clippy::too_many_arguments)]
fn append_textured_corner(
    mesh: &mut Mesh,
    center: u32,
    mut previous: u32,
    rect: Rect,
    radius: f32,
    start_deg: f32,
    end_deg: f32,
    include_start: bool,
    center_x: f32,
    center_y: f32,
    fallback_x: f32,
    fallback_y: f32,
) -> Render2DResult<u32> {
    let step = 90.0 / 28.0;
    if radius <= 0.0 {
        if !include_start {
            return Ok(previous);
        }

        let current = put_textured_vertex(mesh, rect, fallback_x, fallback_y)?;
        mesh.triangle(center, previous, current);
        return Ok(current);
    }

    let mut degrees = if include_start {
        start_deg
    } else {
        start_deg + step
    };

    while degrees <= end_deg + 0.001 {
        let current = put_textured_corner_vertex(
            mesh, rect, radius, degrees, center_x, center_y, fallback_x, fallback_y,
        )?;
        mesh.triangle(center, previous, current);
        previous = current;
        degrees += step;
    }

    Ok(previous)
}

#[allow(clippy::too_many_arguments)]
fn put_textured_corner_vertex(
    mesh: &mut Mesh,
    rect: Rect,
    radius: f32,
    degrees: f32,
    center_x: f32,
    center_y: f32,
    fallback_x: f32,
    fallback_y: f32,
) -> Render2DResult<u32> {
    if radius <= 0.0 {
        return put_textured_vertex(mesh, rect, fallback_x, fallback_y);
    }

    let radians = degrees.to_radians();
    put_textured_vertex(
        mesh,
        rect,
        center_x + radians.sin() * radius,
        center_y + radians.cos() * radius,
    )
}

fn put_textured_vertex(mesh: &mut Mesh, rect: Rect, px: f32, py: f32) -> Render2DResult<u32> {
    let u = (px - rect.x) / rect.width;
    let v = (py - rect.y) / rect.height;
    mesh.vec2(px, py).vec2(u, v).next()
}

fn compile_shader(kind: GLenum, source: &str, label: &str) -> Render2DResult<GLuint> {
    let c_source = CString::new(source).map_err(|_| Render2DError::NulInShaderSource {
        label: label.to_owned(),
    })?;
    let shader = unsafe { gl::CreateShader(kind) };

    unsafe {
        gl::ShaderSource(shader, 1, &c_source.as_ptr(), ptr::null());
        gl::CompileShader(shader);
    }

    let mut status = 0;
    unsafe {
        gl::GetShaderiv(shader, gl::COMPILE_STATUS, &mut status);
    }

    if status == 0 {
        let log = shader_info_log(shader);
        unsafe {
            gl::DeleteShader(shader);
        }
        return Err(Render2DError::ShaderCompile {
            label: label.to_owned(),
            log,
        });
    }

    Ok(shader)
}

fn shader_info_log(shader: GLuint) -> String {
    let mut length = 0;
    unsafe {
        gl::GetShaderiv(shader, gl::INFO_LOG_LENGTH, &mut length);
    }
    read_gl_log(length, |buffer, capacity, written| unsafe {
        gl::GetShaderInfoLog(shader, capacity, written, buffer);
    })
}

fn program_info_log(program: GLuint) -> String {
    let mut length = 0;
    unsafe {
        gl::GetProgramiv(program, gl::INFO_LOG_LENGTH, &mut length);
    }
    read_gl_log(length, |buffer, capacity, written| unsafe {
        gl::GetProgramInfoLog(program, capacity, written, buffer);
    })
}

fn read_gl_log<F>(length: GLint, read: F) -> String
where
    F: FnOnce(*mut GLchar, GLsizei, *mut GLsizei),
{
    if length <= 1 {
        return "no info log returned".to_owned();
    }

    let mut buffer = vec![0_u8; length as usize];
    let mut written = 0;
    read(buffer.as_mut_ptr().cast::<GLchar>(), length, &mut written);

    let written = usize::try_from(written).unwrap_or(buffer.len());
    let end = written.min(buffer.len());
    String::from_utf8_lossy(&buffer[..end])
        .trim_end_matches('\0')
        .to_owned()
}

fn get_integer(name: GLenum) -> GLint {
    let mut value = 0;
    unsafe {
        gl::GetIntegerv(name, &mut value);
    }
    value
}
