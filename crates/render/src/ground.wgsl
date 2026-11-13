struct Uniforms { view_proj: mat4x4<f32>, bounds: vec4<f32>, phase: vec4<f32> };
@group(0) @binding(0) var<uniform> u: Uniforms;
struct Vertex { @builtin(position) clip: vec4<f32>, @location(0) plane: vec2<f32> };
@vertex fn vs(@builtin(vertex_index) i: u32) -> Vertex {
    let corners = array<vec2<f32>, 6>(vec2(0.,0.), vec2(1.,0.), vec2(0.,1.), vec2(0.,1.), vec2(1.,0.), vec2(1.,1.));
    let p = mix(u.bounds.xy, u.bounds.zw, corners[i]);
    var out: Vertex;
    out.clip = u.view_proj * vec4(p.x, u.phase.z, p.y, 1.);
    out.plane = p;
    return out;
}
@fragment fn fs(v: Vertex) -> @location(0) vec4<f32> {
    let p = (v.plane - u.phase.xy) / 20.;
    let aa = max(fwidth(p), vec2(0.0001));
    let cell = abs(fract(p) - 0.5) / aa;
    let grid = 1. - smoothstep(0.5, 1.5, min(cell.x, cell.y));
    let stud_distance = abs(length(fract(p + 0.5) - 0.5) - 0.30);
    let stud = 1. - smoothstep(0.015, 0.015 + max(aa.x, aa.y), stud_distance);
    let detail = clamp(1. - max(aa.x, aa.y) * 2., 0., 1.);
    return vec4(0.25, 0.33, 0.29, 0.10 + detail * max(grid * 0.24, stud * 0.14));
}
