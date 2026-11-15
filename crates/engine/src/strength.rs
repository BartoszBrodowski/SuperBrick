//! Sturdiness: how many connections hold two regions together at their weakest point.
//!
//! The build is a graph whose nodes are pieces and whose edge weights are the number of
//! connections between two pieces (stud engagements, pins, ...). The strength between two
//! regions is the minimum cut separating them (max-flow, Dinic); the weakest point of one
//! connected group is its global minimum cut (Stoer–Wagner).
use std::collections::VecDeque;

/// A cut: total weight, the edges crossing it, and which nodes are on the first side.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Cut {
    pub value: f64,
    pub edges: Vec<(usize, usize, f64)>,
    pub side: Vec<bool>,
}

struct Flow {
    head: Vec<usize>,
    to: Vec<usize>,
    cap: Vec<f64>,
    next: Vec<usize>,
    level: Vec<i32>,
    it: Vec<usize>,
}

const NIL: usize = usize::MAX;

impl Flow {
    fn new(n: usize) -> Flow {
        Flow { head: vec![NIL; n], to: Vec::new(), cap: Vec::new(), next: Vec::new(), level: vec![0; n], it: vec![0; n] }
    }
    fn edge(&mut self, a: usize, b: usize, c: f64, back: f64) {
        for (u, v, w) in [(a, b, c), (b, a, back)] {
            self.to.push(v);
            self.cap.push(w);
            self.next.push(self.head[u]);
            self.head[u] = self.to.len() - 1;
        }
    }
    fn bfs(&mut self, s: usize, t: usize) -> bool {
        self.level.iter_mut().for_each(|l| *l = -1);
        self.level[s] = 0;
        let mut q = VecDeque::from([s]);
        while let Some(u) = q.pop_front() {
            let mut e = self.head[u];
            while e != NIL {
                let v = self.to[e];
                if self.cap[e] > 1e-9 && self.level[v] < 0 {
                    self.level[v] = self.level[u] + 1;
                    q.push_back(v);
                }
                e = self.next[e];
            }
        }
        self.level[t] >= 0
    }
    fn dfs(&mut self, u: usize, t: usize, f: f64) -> f64 {
        if u == t {
            return f;
        }
        while self.it[u] != NIL {
            let e = self.it[u];
            let v = self.to[e];
            if self.cap[e] > 1e-9 && self.level[v] == self.level[u] + 1 {
                let d = self.dfs(v, t, f.min(self.cap[e]));
                if d > 1e-9 {
                    self.cap[e] -= d;
                    self.cap[e ^ 1] += d;
                    return d;
                }
            }
            self.it[u] = self.next[e];
        }
        0.0
    }
    fn max_flow(&mut self, s: usize, t: usize) -> f64 {
        let mut total = 0.0;
        while self.bfs(s, t) {
            self.it.clone_from(&self.head);
            loop {
                let f = self.dfs(s, t, f64::INFINITY);
                if f <= 1e-9 {
                    break;
                }
                total += f;
            }
        }
        total
    }
}

/// Minimum cut separating every node in `a` from every node in `b`. Nodes in both are ignored.
/// `edges` are undirected `(u, v, weight)`.
pub fn min_cut(n: usize, edges: &[(usize, usize, f64)], a: &[usize], b: &[usize]) -> Cut {
    let (s, t) = (n, n + 1);
    let mut g = Flow::new(n + 2);
    for &(u, v, w) in edges {
        if u < n && v < n && u != v && w > 0.0 {
            g.edge(u, v, w, w);
        }
    }
    let in_b: Vec<bool> = {
        let mut m = vec![false; n];
        b.iter().filter(|&&x| x < n).for_each(|&x| m[x] = true);
        m
    };
    for &x in a.iter().filter(|&&x| x < n && !in_b[x]) {
        g.edge(s, x, f64::INFINITY, 0.0);
    }
    for &x in b.iter().filter(|&&x| x < n) {
        if !a.contains(&x) {
            g.edge(x, t, f64::INFINITY, 0.0);
        }
    }
    let value = g.max_flow(s, t);
    // Source side: reachable from s in the residual graph.
    let mut side = vec![false; n + 2];
    let mut q = VecDeque::from([s]);
    side[s] = true;
    while let Some(u) = q.pop_front() {
        let mut e = g.head[u];
        while e != NIL {
            let v = g.to[e];
            if g.cap[e] > 1e-9 && !side[v] {
                side[v] = true;
                q.push_back(v);
            }
            e = g.next[e];
        }
    }
    side.truncate(n);
    let cut_edges = edges
        .iter()
        .copied()
        .filter(|&(u, v, w)| u < n && v < n && w > 0.0 && side[u] != side[v])
        .collect();
    Cut { value, edges: cut_edges, side }
}

/// Global minimum cut of the subgraph induced by `nodes` (Stoer–Wagner, O(k³)).
/// `None` for fewer than two nodes. A disconnected subgraph has a cut of 0.
pub fn weakest_cut(n: usize, edges: &[(usize, usize, f64)], nodes: &[usize]) -> Option<Cut> {
    let k = nodes.len();
    if k < 2 {
        return None;
    }
    let mut local = vec![NIL; n];
    for (i, &x) in nodes.iter().enumerate() {
        if x < n {
            local[x] = i;
        }
    }
    let mut w = vec![vec![0.0f64; k]; k];
    for &(u, v, c) in edges {
        if u < n && v < n && local[u] != NIL && local[v] != NIL && u != v {
            w[local[u]][local[v]] += c;
            w[local[v]][local[u]] += c;
        }
    }
    // members[i]: original local nodes merged into super-node i.
    let mut members: Vec<Vec<usize>> = (0..k).map(|i| vec![i]).collect();
    let mut alive: Vec<usize> = (0..k).collect();
    let mut best = f64::INFINITY;
    let mut best_set: Vec<usize> = Vec::new();
    while alive.len() > 1 {
        let mut added = vec![false; k];
        let mut weight = vec![0.0f64; k];
        let mut prev = NIL;
        let mut last = NIL;
        for _ in 0..alive.len() {
            let mut sel = NIL;
            for &v in &alive {
                if !added[v] && (sel == NIL || weight[v] > weight[sel]) {
                    sel = v;
                }
            }
            added[sel] = true;
            prev = last;
            last = sel;
            for &v in &alive {
                if !added[v] {
                    weight[v] += w[sel][v];
                }
            }
        }
        if weight[last] < best {
            best = weight[last];
            best_set = members[last].clone();
        }
        // Merge `last` into `prev`.
        let moved = std::mem::take(&mut members[last]);
        members[prev].extend(moved);
        for &v in &alive {
            w[prev][v] += w[last][v];
            w[v][prev] = w[prev][v];
        }
        w[prev][prev] = 0.0;
        alive.retain(|&v| v != last);
    }
    let mut side = vec![false; n];
    for i in best_set {
        side[nodes[i]] = true;
    }
    let in_nodes = |x: usize| x < n && local[x] != NIL;
    let cut_edges = edges
        .iter()
        .copied()
        .filter(|&(u, v, c)| in_nodes(u) && in_nodes(v) && c > 0.0 && side[u] != side[v])
        .collect();
    Some(Cut { value: best, edges: cut_edges, side })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two 2x4 towers joined by a single 1-stud bridge: the bridge is the weak point.
    fn dumbbell() -> (usize, Vec<(usize, usize, f64)>) {
        // 0-1-2 strongly linked, 3-4-5 strongly linked, 2-3 by one stud.
        let e = vec![(0, 1, 8.0), (1, 2, 8.0), (0, 2, 4.0), (3, 4, 8.0), (4, 5, 8.0), (3, 5, 4.0), (2, 3, 1.0)];
        (6, e)
    }

    #[test]
    fn min_cut_between_regions() {
        let (n, e) = dumbbell();
        let c = min_cut(n, &e, &[0], &[5]);
        assert_eq!(c.value, 1.0);
        assert_eq!(c.edges, vec![(2, 3, 1.0)]);
        assert!(c.side[0] && c.side[2] && !c.side[3]);
        let c = min_cut(n, &e, &[0], &[1]);
        assert_eq!(c.value, 12.0);
    }

    #[test]
    fn weakest_point_of_a_group() {
        let (n, e) = dumbbell();
        let c = weakest_cut(n, &e, &[0, 1, 2, 3, 4, 5]).unwrap();
        assert_eq!(c.value, 1.0);
        assert_eq!(c.edges, vec![(2, 3, 1.0)]);
        // A disconnected set has a zero cut.
        let c = weakest_cut(n, &e, &[0, 5]).unwrap();
        assert_eq!(c.value, 0.0);
        assert!(weakest_cut(n, &e, &[0]).is_none());
    }
}

/// Connection graph of a checker report: weight = connections between the two pieces (studs
/// engaged; 1 for a single other link). Nodes are flat-part indices.
pub fn edges(report: &crate::checker::Report) -> Vec<(usize, usize, f64)> {
    report.edges().map(|(a, b, n)| (a.min(b), a.max(b), n.max(1) as f64)).collect()
}
