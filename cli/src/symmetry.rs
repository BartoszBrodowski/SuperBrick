//! `sb-dev symmetry MODEL [--axis x|y|z] [--plane P] [--min N] [--json]`: is the model's solid
//! geometry mirror-symmetric about a plane?
//!
//! Every part's solid (physics::Shape: hull body plus studs) is sampled on a 1-LDU grid at
//! half-integer points; pockets not reachable from outside are filled, so how a volume is split
//! into parts (a 1x2 beside a 1x1 versus a 1x3) and hidden insides do not matter. The filled
//! occupancy is compared with its mirror image; differences thinner than about 2 LDU (rounding at
//! sloped or tilted faces) are eroded away and the rest is grouped into connected regions, each
//! reported with the parts that make it up.

use std::{collections::BTreeMap, path::Path, sync::Arc};

use glam::DVec3;
use ldraw::{Document, Library};
use physics::{Body, Shape};
use rayon::prelude::*;

use crate::{flag, opt, Res};

pub fn cmd_symmetry(args: &[String]) -> Res<bool> {
    let valued = ["--axis", "--plane", "--min", "--probe"];
    let path = Path::new(
        args.iter()
            .enumerate()
            .find(|(i, a)| !a.starts_with("--") && (*i == 0 || !valued.contains(&args[i - 1].as_str())))
            .map(|(_, a)| a.as_str())
            .ok_or("give a model file")?,
    );
    let axis = match opt(args, "--axis").unwrap_or("z") {
        "x" => 0,
        "y" => 1,
        "z" => 2,
        a => return Err(format!("--axis must be x, y or z, not {a}").into()),
    };
    let inside = flag(args, "--inside");
    let probe: usize = opt(args, "--probe").map(str::parse).transpose()?.unwrap_or(2);
    let min: usize = opt(args, "--min").map(str::parse).transpose()?.unwrap_or(8);
    let lib = Library::open_default()?;
    let doc = Document::load(path)?;
    let flat = doc.flatten(path.parent());
    let mut shapes: BTreeMap<String, Option<Arc<Shape>>> = BTreeMap::new();
    let mut bodies = Vec::new();
    let mut skipped = Vec::new();
    for (i, p) in flat.iter().enumerate() {
        let sh = shapes
            .entry(p.name.clone())
            .or_insert_with(|| Shape::build(&lib, &p.name).ok().map(Arc::new))
            .clone();
        match sh {
            Some(sh) => bodies.push((i, Body::new(sh, p.rot, p.pos))),
            None => skipped.push(p.name.clone()),
        }
    }
    if bodies.is_empty() {
        return Err("no solid parts".into());
    }
    let (mut lo, mut hi) = (DVec3::splat(f64::INFINITY), DVec3::splat(f64::NEG_INFINITY));
    for (_, b) in &bodies {
        lo = lo.min(b.flo);
        hi = hi.max(b.fhi);
    }
    // The plane: given, or the middle of the model's extent along the axis, rounded to 0.5 LDU so
    // half-integer grid points mirror onto grid points.
    let plane: f64 = match opt(args, "--plane") {
        Some(s) => s.parse()?,
        None => ((lo[axis] + hi[axis]) / 2.0 * 2.0).round() / 2.0,
    };
    // Grid symmetric about the plane, one empty voxel of margin all round.
    let margin = probe as f64 + 2.0;
    let half = (plane - lo[axis]).max(hi[axis] - plane).ceil() + margin;
    // Along the axis the grid is centred on the plane, so cell i mirrors onto cell n-1-i.
    let mut g0 = (lo - margin).floor();
    let mut g1 = (hi + margin).ceil();
    g0[axis] = plane - half;
    g1[axis] = plane + half;
    let n = [
        (g1.x - g0.x) as usize,
        (g1.y - g0.y) as usize,
        (g1.z - g0.z) as usize,
    ];
    let idx = |x: usize, y: usize, z: usize| (x * n[1] + y) * n[2] + z;
    let centre = |x: usize, y: usize, z: usize| {
        g0 + DVec3::new(x as f64 + 0.5, y as f64 + 0.5, z as f64 + 0.5)
    };
    // Owner = index into `bodies` + 1 (0 = empty).
    let owners: Vec<Vec<(usize, u32)>> = bodies
        .par_iter()
        .enumerate()
        .map(|(bi, (_, b))| {
            let a = ((b.flo - g0).floor().max(DVec3::ZERO)).as_uvec3();
            let e = (b.fhi - g0).ceil();
            let mut out = Vec::new();
            for x in a.x as usize..(e.x as usize).min(n[0]) {
                for y in a.y as usize..(e.y as usize).min(n[1]) {
                    for z in a.z as usize..(e.z as usize).min(n[2]) {
                        let c = centre(x, y, z);
                        if if inside { filled(b, c) } else { b.occ(c, true, 0.0) } {
                            out.push((idx(x, y, z), bi as u32 + 1));
                        }
                    }
                }
            }
            out
        })
        .collect();
    let total = n[0] * n[1] * n[2];
    let mut owner = vec![0u32; total];
    for list in owners {
        for (i, o) in list {
            if owner[i] == 0 {
                owner[i] = o;
            }
        }
    }
    let neighbours = |(x, y, z): (usize, usize, usize)| {
        let mut v = Vec::with_capacity(6);
        if x > 0 { v.push((x - 1, y, z)); }
        if y > 0 { v.push((x, y - 1, z)); }
        if z > 0 { v.push((x, y, z - 1)); }
        if x + 1 < n[0] { v.push((x + 1, y, z)); }
        if y + 1 < n[1] { v.push((x, y + 1, z)); }
        if z + 1 < n[2] { v.push((x, y, z + 1)); }
        v
    };
    // The outside: empty space a cube probe of side 2*probe+1 LDU can reach from the grid's
    // border, swept out to everything the probe touches. Gaps narrower than the probe (stud
    // clearances, seams between parts) do not let it into the model's hollow inside.
    let empty: Vec<bool> = owner.iter().map(|&o| o == 0).collect();
    let roomy = morph(&empty, n, probe, true);
    let mut reach = vec![false; total];
    let mut stack = Vec::new();
    for x in 0..n[0] {
        for y in 0..n[1] {
            for z in 0..n[2] {
                let border = x == 0 || y == 0 || z == 0 || x + 1 == n[0] || y + 1 == n[1] || z + 1 == n[2];
                if border && roomy[idx(x, y, z)] {
                    reach[idx(x, y, z)] = true;
                    stack.push((x, y, z));
                }
            }
        }
    }
    while let Some(c) = stack.pop() {
        for q in neighbours(c) {
            let i = idx(q.0, q.1, q.2);
            if !reach[i] && roomy[i] {
                reach[i] = true;
                stack.push(q);
            }
        }
    }
    let swept = morph(&reach, n, probe, false);
    let outside: Vec<bool> = (0..total).map(|i| swept[i] && empty[i]).collect();
    // --inside compares every part's whole (filled) volume, so the inside structure counts too.
    let solid = |i: usize| if inside { owner[i] != 0 } else { !outside[i] };
    let mirror = |(x, y, z): (usize, usize, usize)| {
        let mut c = [x, y, z];
        c[axis] = n[axis] - 1 - c[axis];
        (c[0], c[1], c[2])
    };
    // Solid here, empty in the mirror image.
    let mut diff = vec![false; total];
    for x in 0..n[0] {
        for y in 0..n[1] {
            for z in 0..n[2] {
                let m = mirror((x, y, z));
                diff[idx(x, y, z)] = solid(idx(x, y, z)) && !solid(idx(m.0, m.1, m.2));
            }
        }
    }
    // Erode once (6-neighbourhood) to drop rounding slivers, then grow back inside `diff`.
    let core: Vec<bool> = (0..total)
        .into_par_iter()
        .map(|i| {
            if !diff[i] {
                return false;
            }
            let (x, r) = (i / (n[1] * n[2]), i % (n[1] * n[2]));
            let (y, z) = (r / n[2], r % n[2]);
            let nb = neighbours((x, y, z));
            nb.len() == 6 && nb.iter().all(|q| diff[idx(q.0, q.1, q.2)])
        })
        .collect();
    let mut seen = vec![false; total];
    let mut regions = Vec::new();
    for start in 0..total {
        if !core[start] || seen[start] {
            continue;
        }
        // Connected component of the eroded core, then its diff voxels within one step.
        let mut comp = Vec::new();
        let mut st = vec![start];
        seen[start] = true;
        while let Some(i) = st.pop() {
            comp.push(i);
            let (x, r) = (i / (n[1] * n[2]), i % (n[1] * n[2]));
            let (y, z) = (r / n[2], r % n[2]);
            for q in neighbours((x, y, z)) {
                let j = idx(q.0, q.1, q.2);
                if core[j] && !seen[j] {
                    seen[j] = true;
                    st.push(j);
                }
            }
        }
        let mut cells = comp.clone();
        for &i in &comp {
            let (x, r) = (i / (n[1] * n[2]), i % (n[1] * n[2]));
            let (y, z) = (r / n[2], r % n[2]);
            for q in neighbours((x, y, z)) {
                let j = idx(q.0, q.1, q.2);
                if diff[j] && !core[j] && !seen[j] {
                    seen[j] = true;
                    cells.push(j);
                }
            }
        }
        if cells.len() < min {
            continue;
        }
        let (mut blo, mut bhi, mut sum) = (DVec3::splat(f64::INFINITY), DVec3::splat(f64::NEG_INFINITY), DVec3::ZERO);
        let mut by: BTreeMap<u32, usize> = BTreeMap::new();
        let mut filled = 0;
        for &i in &cells {
            let (x, r) = (i / (n[1] * n[2]), i % (n[1] * n[2]));
            let (y, z) = (r / n[2], r % n[2]);
            let c = centre(x, y, z);
            blo = blo.min(c - 0.5);
            bhi = bhi.max(c + 0.5);
            sum += c;
            match owner[i] {
                0 => filled += 1,
                o => *by.entry(o).or_default() += 1,
            }
        }
        regions.push((cells.len(), blo, bhi, sum / cells.len() as f64, by, filled));
    }
    regions.sort_by(|a, b| b.0.cmp(&a.0));
    let json = flag(args, "--json");
    let axis_name = ["x", "y", "z"][axis];
    let side = |c: DVec3| if c[axis] < plane { "-" } else { "+" };
    let part = |o: u32| &flat[bodies[o as usize - 1].0];
    if json {
        let out: Vec<_> = regions
            .iter()
            .map(|(vol, blo, bhi, c, by, filled)| {
                serde_json::json!({
                    "volume": vol, "box": [blo.to_array(), bhi.to_array()], "centre": c.to_array(),
                    "side": side(*c), "enclosed_fill": filled,
                    "parts": by.iter().map(|(o, k)| {
                        let p = part(*o);
                        serde_json::json!({"name": p.name, "color": p.color.0, "pos": p.pos.to_array(),
                            "rot": p.rot.transpose().to_cols_array(), "path": p.path, "voxels": k})
                    }).collect::<Vec<_>>(),
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "axis": axis_name, "plane": plane, "parts": flat.len(),
                "skipped": skipped, "regions": out
            }))?
        );
    } else {
        println!(
            "{} parts, mirror plane {}={plane}; {} asymmetric region(s) of {min}+ LDU³{}",
            flat.len(),
            ["x", "y", "z"][axis],
            regions.len(),
            if skipped.is_empty() { String::new() } else { format!(" (no solid for {skipped:?})") }
        );
        for (vol, blo, bhi, c, by, filled) in &regions {
            println!(
                "\n{vol} LDU³ solid only on the {} side, box [{:.0},{:.0},{:.0}]..[{:.0},{:.0},{:.0}]{}",
                side(*c), blo.x, blo.y, blo.z, bhi.x, bhi.y, bhi.z,
                if *filled > 0 { format!(", {filled} enclosed") } else { String::new() }
            );
            let mut v: Vec<_> = by.iter().collect();
            v.sort_by(|a, b| b.1.cmp(a.1));
            for (o, k) in v {
                let p = part(*o);
                println!(
                    "  {k:>6}  {} c{} at [{:.1},{:.1},{:.1}]{}",
                    p.name, p.color.0, p.pos.x, p.pos.y, p.pos.z,
                    if p.path.is_empty() { String::new() } else { format!(" in {}", p.path) }
                );
            }
        }
    }
    Ok(regions.is_empty())
}

/// Is a world point inside the part's filled volume: its visual hull from the three axis
/// silhouettes (so underside cavities, tube interiors and other blind pockets count as solid,
/// while through-holes and open profiles seen along an axis stay empty), plus its studs.
fn filled(b: &Body, p: DVec3) -> bool {
    let sh = &b.shape;
    let l = b.r.transpose() * (p - b.t);
    let f = |v: f64, lo: i64| (v - lo as f64).floor();
    let (ix, iy, iz) = (f(l.x, sh.lo[0]), f(l.y, sh.lo[1]), f(l.z, sh.lo[2]));
    let [nx, ny, nz] = sh.n;
    if ix >= 0.0 && iy >= 0.0 && iz >= 0.0 && (ix as usize) < nx && (iy as usize) < ny && (iz as usize) < nz {
        let (a, b2, c) = (ix as usize, iy as usize, iz as usize);
        if !sh.hy.0[c * nx + a].is_nan() && !sh.hx.0[b2 * nz + c].is_nan() && !sh.hz.0[b2 * nx + a].is_nan() {
            return true;
        }
    }
    sh.inside(l, true, 0.0)
}

/// Separable cube filter of half-width `k` over a `n`-shaped grid (x-major, z fastest):
/// `all` = erosion (every cell in the window set; cells past the edge count as set),
/// otherwise dilation (any cell set).
fn morph(v: &[bool], n: [usize; 3], k: usize, all: bool) -> Vec<bool> {
    let mut cur = v.to_vec();
    let stride = [n[1] * n[2], n[2], 1];
    for a in 0..3 {
        let (b, c) = ((a + 1) % 3, (a + 2) % 3);
        let mut next = vec![false; cur.len()];
        let mut pre = vec![0usize; n[a] + 1];
        for j in 0..n[b] {
            for l in 0..n[c] {
                let base = j * stride[b] + l * stride[c];
                for i in 0..n[a] {
                    pre[i + 1] = pre[i] + cur[base + i * stride[a]] as usize;
                }
                for i in 0..n[a] {
                    let lo = i.saturating_sub(k);
                    let hi = (i + k + 1).min(n[a]);
                    let set = pre[hi] - pre[lo];
                    next[base + i * stride[a]] = if all { set == hi - lo } else { set > 0 };
                }
            }
        }
        cur = next;
    }
    cur
}
