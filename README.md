# gpupaper

Render a GLSL fragment shader as a Wayland wallpaper.

## Requirements

- A Wayland compositor supporting `zwlr_layer_shell_v1` (Sway, Hyprland, Niri, etc.)
- EGL and OpenGL ES 3.0

## Build

With Nix:

```sh
nix build
./result/bin/gpupaper shader.glsl
```

Or run directly:

```sh
nix run . -- shader.glsl
```

With Cargo, inside the dev shell:

```sh
nix develop -c cargo build --release
./target/release/gpupaper shader.glsl
```

## Usage

```text
gpupaper [-o OUTPUT] [-s SPEED] SHADER
```

- `SHADER` — fragment shader path, or `-` to read stdin
- `-o, --output NAME` — only render on the named output
- `-s, --speed N` — time multiplier (default `1.0`, `0` freezes time)

Examples:

```sh
gpupaper wallpaper.glsl
gpupaper -o eDP-1 wallpaper.glsl
gpupaper --speed 0.5 wallpaper.glsl
cat wallpaper.glsl | gpupaper -
```

## Shader inputs

Plain shaders without a `#version` directive are compiled as GLSL ES 3.00
with:

```glsl
uniform vec2 u_resolution;
uniform float u_time;
layout(location = 0) out vec4 frag_color;
```

Shadertoy shaders using `mainImage(out vec4, in vec2)` are detected
automatically and receive:

```glsl
uniform vec3  iResolution;
uniform float iTime;
uniform float iTimeDelta;
uniform float iFrameRate;
uniform int   iFrame;
uniform vec4  iMouse;   // always zero
uniform vec4  iDate;    // year, month, day, seconds since local midnight
```

Texture channels (`iChannel0..3`) are not supported.

Example:

```glsl
void mainImage(out vec4 fragColor, in vec2 fragCoord) {
    vec2 uv = fragCoord / iResolution.xy;
    fragColor = vec4(uv, 0.5 + 0.5 * sin(iTime), 1.0);
}
```

## GPU selection

The app uses the default EGL device. For Mesa, select a discrete GPU with:

```sh
DRI_PRIME=1 gpupaper shader.glsl
```

NVIDIA users can use PRIME render offload or their distribution's `prime-run`
wrapper.
