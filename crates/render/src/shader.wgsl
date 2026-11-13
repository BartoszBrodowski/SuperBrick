// Parts, studs, edge lines and conditional lines, all instanced from one atlas.

struct Globals {
    view_proj: mat4x4<f32>,
    eye: vec4<f32>,
    light: vec4<f32>,
    // x, y: viewport size in pixels; z: line depth pull (NDC); w: unused
    viewport: vec4<f32>,
};

struct Pal {
    main: vec4<f32>,
    edge: vec4<f32>,
};

@group(0) @binding(0) var<uniform> g: Globals;
@group(0) @binding(1) var<storage, read> palette: array<Pal>;

struct Inst {
    @location(4) c0: vec3<f32>,
    @location(5) c1: vec3<f32>,
    @location(6) c2: vec3<f32>,
    @location(7) t: vec3<f32>,
    @location(8) color: u32,
    @location(10) id: u32,
    @location(11) flags: u32,
};

const SELECT_TINT: vec4<f32> = vec4<f32>(1.0, 0.55, 0.08, 1.0);
// FLAG_FADE (128): context parts pushed toward the background, opaque and unhatched.
const FADE_TO: vec3<f32> = vec3<f32>(0.86, 0.88, 0.91);

fn hidden(i: Inst) -> bool {
    return (i.flags & 2u) != 0u;
}

fn tinted(c: vec4<f32>, i: Inst) -> vec4<f32> {
    // FLAG_ERROR (256): solid red, visible through hatched blockers.
    if ((i.flags & 256u) != 0u) { return vec4<f32>(mix(c.rgb, vec3<f32>(0.95, 0.1, 0.08), 0.8), 1.0); }
    if ((i.flags & 16u) != 0u) { return vec4<f32>(1.0, 0.65, 0.06, 1.0); }
    if ((i.flags & 8u) != 0u) { return vec4<f32>(0.95, 0.12, 0.1, 1.0); }
    if ((i.flags & 4u) != 0u) { return vec4<f32>(0.1, 0.45, 1.0, 1.0); }
    if ((i.flags & 32u) != 0u) { return vec4<f32>(0.6,0.65,0.72,1.0); }
    if ((i.flags & 1u) != 0u) {
        return vec4<f32>(mix(c.rgb, SELECT_TINT.rgb, 0.5), max(c.a, 0.85));
    }
    if ((i.flags & 128u) != 0u) {
        return vec4<f32>(mix(c.rgb, FADE_TO, 0.72), 1.0);
    }
    return c;
}

fn world(i: Inst, p: vec3<f32>) -> vec3<f32> {
    return i.c0 * p.x + i.c1 * p.y + i.c2 * p.z + i.t;
}

fn resolve_color(code: u32, slot: u32) -> vec4<f32> {
    if (code == 0xFFFFFFFFu) {
        return palette[slot].main;
    }
    if (code == 0xFFFFFFFEu) {
        return palette[slot].edge;
    }
    if ((code & 0x80000000u) != 0u) {
        return palette[code & 0x7FFFFFFFu].edge;
    }
    return palette[code].main;
}

struct TriOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) wpos: vec3<f32>,
    @location(1) color: vec4<f32>,
    @location(2) @interpolate(flat) flags: u32,
};

@vertex
fn vs_tri(@location(0) pos: vec3<f32>, @location(1) code: u32, inst: Inst) -> TriOut {
    let w = world(inst, pos);
    var o: TriOut;
    o.clip = g.view_proj * vec4<f32>(w, 1.0);
    if (hidden(inst)) {
        o.clip = vec4<f32>(0.0, 0.0, -2.0, 1.0);
    }
    o.wpos = w;
    o.color = tinted(resolve_color(code, inst.color), inst);
    o.flags = inst.flags;
    if ((inst.flags & 28u) != 0u) { o.clip.z -= 0.00002 * o.clip.w; }
    return o;
}

@fragment
fn fs_tri(v: TriOut) -> @location(0) vec4<f32> {
    // Screen-space hatch makes previews see-through without changing the opaque atlas
    // into a second, unsorted transparency pipeline. Base geometry remains untouched.
    if ((v.flags & 60u) != 0u && ((u32(v.clip.x) + u32(v.clip.y)) % 6u) < 3u) { discard; }
    // Flat normal from screen derivatives; LDraw winding is not trusted, so face it to the viewer.
    var n = normalize(cross(dpdx(v.wpos), dpdy(v.wpos)));
    let to_eye = normalize(g.eye.xyz - v.wpos);
    if (dot(n, to_eye) < 0.0) {
        n = -n;
    }
    let l = normalize(g.light.xyz);
    let diff = max(dot(n, l), 0.0);
    let head = max(dot(n, to_eye), 0.0);
    let h = normalize(l + to_eye);
    let spec = pow(max(dot(n, h), 0.0), 48.0) * 0.18;
    let c = v.color.rgb * (0.28 + 0.52 * diff + 0.30 * head) + vec3<f32>(spec);
    return vec4<f32>(c, v.color.a);
}

struct LineOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec4<f32>,
};

fn pulled(c: vec4<f32>) -> vec4<f32> {
    var o = c;
    o.z = o.z - g.viewport.z * o.w;
    return o;
}

@vertex
fn vs_line(@location(0) pos: vec3<f32>, @location(1) code: u32, inst: Inst) -> LineOut {
    var o: LineOut;
    o.clip = pulled(g.view_proj * vec4<f32>(world(inst, pos), 1.0));
    if (hidden(inst)) {
        o.clip = vec4<f32>(0.0, 0.0, -2.0, 1.0);
    }
    o.color = resolve_color(code, inst.color);
    if ((inst.flags & 1u) != 0u) {
        o.color = SELECT_TINT;
    }
    if ((inst.flags & 128u) != 0u) {
        o.color = vec4<f32>(mix(o.color.rgb, FADE_TO, 0.6), o.color.a);
    }
    return o;
}

@vertex
fn vs_cond(
    @location(0) pos: vec3<f32>,
    @location(1) code: u32,
    @location(2) other: vec3<f32>,
    @location(3) c1: vec3<f32>,
    @location(9) c2: vec3<f32>,
    inst: Inst,
) -> LineOut {
    let here = g.view_proj * vec4<f32>(world(inst, pos), 1.0);
    // Both vertices of the segment must reach the same verdict, or the hidden one's off-screen
    // position turns the segment into a stray line toward the screen centre. So run the side test
    // in a fixed endpoint order (lexicographic on local coordinates), with bit-identical inputs.
    var e0 = pos;
    var e1 = other;
    if (pos.x > other.x || (pos.x == other.x && (pos.y > other.y || (pos.y == other.y && pos.z > other.z)))) {
        e0 = other;
        e1 = pos;
    }
    let a = g.view_proj * vec4<f32>(world(inst, e0), 1.0);
    let b = g.view_proj * vec4<f32>(world(inst, e1), 1.0);
    let p = g.view_proj * vec4<f32>(world(inst, c1), 1.0);
    let q = g.view_proj * vec4<f32>(world(inst, c2), 1.0);
    let A = a.xy / a.w;
    let d = b.xy / b.w - A;
    let P = p.xy / p.w - A;
    let Q = q.xy / q.w - A;
    let s1 = d.x * P.y - d.y * P.x;
    let s2 = d.x * Q.y - d.y * Q.x;
    var o: LineOut;
    // Drawn only when both control points fall on the same side of the line on screen.
    if (s1 * s2 < 0.0 || hidden(inst)) {
        o.clip = vec4<f32>(0.0, 0.0, -2.0, 1.0);
    } else {
        o.clip = pulled(here);
    }
    o.color = resolve_color(code, inst.color);
    if ((inst.flags & 128u) != 0u) {
        o.color = vec4<f32>(mix(o.color.rgb, FADE_TO, 0.6), o.color.a);
    }
    return o;
}

@fragment
fn fs_line(v: LineOut) -> @location(0) vec4<f32> {
    return v.color;
}

// ID pass for picking: part index + 1 into an R32Uint target (0 = background).
struct PickOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) @interpolate(flat) id: u32,
};

@vertex
fn vs_pick(@location(0) pos: vec3<f32>, @location(1) code: u32, inst: Inst) -> PickOut {
    var o: PickOut;
    o.clip = g.view_proj * vec4<f32>(world(inst, pos), 1.0);
    if (hidden(inst) || (inst.flags & 28u) != 0u || ((inst.flags & 32u)!=0u && (inst.flags & 64u)==0u)) {
        o.clip = vec4<f32>(0.0, 0.0, -2.0, 1.0);
    }
    o.id = inst.id + 1u;
    return o;
}

@fragment
fn fs_pick(v: PickOut) -> @location(0) u32 {
    return v.id;
}
