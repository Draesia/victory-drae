//! A structure in the console's node format ([`crate::bvh`]) as a flat list
//! of what it holds: its triangles, each with the node pointer that names it
//! (the pointer the title's own traversal would hand the instruction for that
//! triangle), and the instance nodes of a top level, which the instruction
//! does not read and the title's program follows itself.
//!
//! The first step of a host acceleration structure (DXR) built from the
//! title's: a bottom level's triangles become its geometry, a top level's
//! instances its instances, and the pointers are the side table that maps
//! what the host's ray query finds back to what the title's program expects
//! (the triangle node it tests with the instruction, the instance node it
//! reads). With that, a title traversal can be guided to the hit the host
//! found, its own triangle test still deciding the hit distance and the
//! barycentrics bit for bit.
//!
//! Exactness rests on one property, which the tests check against a full
//! traversal by the instruction's own reference: the closest hit over the
//! flat list is the closest hit of the structure. The box tests can only
//! leave out triangles the ray misses or reaches later than one it hits:
//! a box the instruction does not enter holds no triangle the ray hits
//! inside its extent, and the triangle test ignores the extent.

use victory_shader::bvh::{self, NONE, Ray};

/// One triangle of a structure.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Triangle {
    /// The node pointer that names it: the triangle node's pointer with the
    /// triangle's number (0-3) as its kind.
    pub pointer: u64,
    /// Its vertices as the instruction takes them: the one neither
    /// barycentric weighs, then I's, then J's ([`bvh::triangle_vertices`]).
    pub vertices: [[f32; 3]; 3],
}

/// What a structure holds.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Flat {
    pub triangles: Vec<Triangle>,
    /// Pointers of instance nodes (kind 6).
    pub instances: Vec<u64>,
    /// Pointers of procedural nodes (kind 7): no geometry the host can be
    /// given; a structure with any needs the title's own traversal.
    pub procedural: Vec<u64>,
    /// Box nodes walked.
    pub boxes: usize,
}

/// Why a structure could not be walked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// A node the walk came to could not be read (not there, or out of the
    /// structure).
    Unreadable(u64),
    /// More nodes than the limit: a malformed structure (a cycle, or a
    /// pointer into other memory).
    TooMany(usize),
}

/// Walks the structure from `root`, reading each box or triangle node with
/// `read` (its first 28 dwords, as [`bvh::intersect`] takes them). Every
/// child of every box node is followed, whatever a ray would do; a pointer
/// met twice is walked once. At most `limit` nodes.
pub fn flatten(root: u64, mut read: impl FnMut(u64) -> Option<[u32; 28]>, limit: usize) -> Result<Flat, Error> {
    let mut flat = Flat::default();
    let mut seen = std::collections::HashSet::new();
    let mut stack = vec![root];
    while let Some(pointer) = stack.pop() {
        if !seen.insert(pointer) {
            continue;
        }
        if seen.len() > limit {
            return Err(Error::TooMany(limit));
        }
        match bvh::kind(pointer) {
            4 | 5 => {
                let words = read(pointer).ok_or(Error::Unreadable(pointer))?;
                flat.boxes += 1;
                // (Pushed last first: the walk takes the children in order.)
                for &child in bvh::children(&words).iter().rev() {
                    if child != NONE {
                        stack.push(u64::from(child));
                    }
                }
            }
            k @ 0..=3 => {
                let words = read(pointer).ok_or(Error::Unreadable(pointer))?;
                flat.triangles.push(Triangle {
                    pointer,
                    vertices: bvh::triangle_vertices(&words, k as u32),
                });
            }
            6 => flat.instances.push(pointer),
            _ => flat.procedural.push(pointer),
        }
    }
    Ok(flat)
}

/// The triangles of a bottom level read from its element nodes, without its
/// box nodes: header dword 13 is the elements' offset from the header (64
/// bytes each), dword 3 their number. Each element is a triangle node whose
/// dword 15 has an id byte per triangle it holds (bits 1:0 I's vertex, 3:2
/// J's; 0 for a triangle that is not there), the bytes of the vertices it
/// does not use holding the title's own data (primitive numbers).
///
/// Run 20261126-140839's dump: the box nodes of most dynamic bottom levels
/// are not a tree (children 0, bounds of 64 m cubes and infinities: built
/// over, or not built yet), so a walk finds one or two triangles of
/// hundreds, and the instruction misses the rest; the element nodes are
/// whole. A host structure built from the elements has every triangle, the
/// pointer of each (`(offset >> 3) | k`) as the title's walk would name it.
pub fn elements(header: &[u32; 16], mut read: impl FnMut(u64) -> Option<[u32; 28]>) -> Result<Flat, Error> {
    let (first, count) = (u64::from(header[13]), u64::from(header[3]));
    let mut flat = Flat::default();
    for n in 0..count {
        let offset = first + 64 * n;
        let pointer = offset >> 3;
        let words = read(pointer).ok_or(Error::Unreadable(pointer))?;
        for k in 0..4u32 {
            if words[15] >> (8 * k) & 0xff == 0 {
                continue;
            }
            flat.triangles.push(Triangle {
                pointer: pointer | u64::from(k),
                vertices: bvh::triangle_vertices(&words, k),
            });
        }
    }
    Ok(flat)
}

/// The closest hit over a flat list: the triangle and the instruction's
/// result for it (distance numerator, denominator, I and J numerators), or
/// `None`. Ties go to the first in the list.
pub fn closest(flat: &Flat, ray: &Ray) -> Option<(Triangle, [f32; 4])> {
    let mut best: Option<(Triangle, [f32; 4], f32)> = None;
    for t in &flat.triangles {
        let r = bvh::triangle(&t.vertices, ray);
        let distance = r[0] / r[1];
        if !(distance.is_finite() && distance >= 0.0 && distance <= ray.extent) {
            continue;
        }
        if best.as_ref().is_none_or(|b| distance < b.2) {
            best = Some((*t, r, distance));
        }
    }
    best.map(|(t, r, _)| (t, r))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// A 32-bit box node.
    fn box32(children: [u32; 4], boxes: [[f32; 6]; 4]) -> [u32; 28] {
        let mut w = [0u32; 28];
        w[..4].copy_from_slice(&children);
        for (b, bounds) in boxes.iter().enumerate() {
            for (k, v) in bounds.iter().enumerate() {
                w[4 + 6 * b + k] = v.to_bits();
            }
        }
        w
    }

    /// A triangle node with vertices `v` (up to five) and one id byte per
    /// triangle: I weighs the node's vertex 1 of the triangle, J vertex 2.
    fn tri_node(v: &[[f32; 3]]) -> [u32; 28] {
        let mut w = [0u32; 28];
        for (n, p) in v.iter().enumerate() {
            for c in 0..3 {
                w[3 * n + c] = p[c].to_bits();
            }
        }
        // (i = 1, j = 2 for each of the four triangles)
        w[15] = 0x0909_0909;
        w
    }

    fn empty() -> [f32; 6] {
        let (up, down) = (f32::INFINITY, f32::NEG_INFINITY);
        [up, up, up, down, down, down]
    }

    fn bounds(points: &[[f32; 3]]) -> [f32; 6] {
        let mut b = empty();
        for p in points {
            for a in 0..3 {
                b[a] = b[a].min(p[a]);
                b[3 + a] = b[3 + a].max(p[a]);
            }
        }
        b
    }

    /// A small structure: a root box with two triangle nodes and a box that
    /// holds a third; pointers are byte offsets / 8 with the kind in the low
    /// three bits.
    fn structure(seed: u32) -> (u64, HashMap<u64, [u32; 28]>) {
        let mut rng = seed.wrapping_mul(2654435761) | 1;
        let mut next = || {
            rng ^= rng << 13;
            rng ^= rng >> 17;
            rng ^= rng << 5;
            (rng % 2000) as f32 / 100.0 - 10.0
        };
        let mut nodes = HashMap::new();
        let mut verts = |n: usize| (0..n).map(|_| [next(), next(), next()]).collect::<Vec<_>>();
        let a = verts(5);
        let b = verts(4);
        let c = verts(3);
        // slots: 1 root (box32), 2 triangle node A, 3 triangle node B, 4 inner box, 5 triangle node C
        let (pa, pb, pc) = (slot_ptr(2), slot_ptr(3), slot_ptr(5));
        nodes.insert(pa, tri_node(&a));
        nodes.insert(pb, tri_node(&b));
        nodes.insert(pc, tri_node(&c));
        // triangles of A: 0, 1, 2, 3; of B: 0, 1; of C: 0
        let tris_a: Vec<[[f32; 3]; 3]> = (0..4).map(|k| bvh::triangle_vertices(&nodes[&pa], k)).collect();
        let tris_b: Vec<[[f32; 3]; 3]> = (0..2).map(|k| bvh::triangle_vertices(&nodes[&pb], k)).collect();
        let tri_c = bvh::triangle_vertices(&nodes[&pc], 0);
        let all = |ts: &[[[f32; 3]; 3]]| bounds(&ts.iter().flatten().copied().collect::<Vec<_>>());
        let inner = slot_ptr(4) | 5;
        nodes.insert(inner, box32([pc as u32, NONE, NONE, NONE], [all(&[tri_c]), empty(), empty(), empty()]));
        let root = slot_ptr(1) | 5;
        let children_a = [pa as u32, (pa | 1) as u32, (pa | 2) as u32, (pa | 3) as u32];
        nodes.insert(slot_ptr(6) | 5, box32(children_a, [all(&tris_a[0..1]), all(&tris_a[1..2]), all(&tris_a[2..3]), all(&tris_a[3..4])]));
        nodes.insert(
            root,
            box32(
                [(slot_ptr(6) | 5) as u32, pb as u32, (pb | 1) as u32, inner as u32],
                [all(&tris_a), all(&tris_b[0..1]), all(&tris_b[1..2]), all(&[tri_c])],
            ),
        );
        (root, nodes)
    }

    fn slot_ptr(slot: u64) -> u64 {
        // A node at byte offset slot * 128: its pointer is the offset / 8.
        (slot * 128 / 64) << 3
    }

    /// The title's way: a stack, the instruction at every node, boxes in the
    /// order it returns them, the closest triangle hit kept.
    fn traverse(root: u64, nodes: &HashMap<u64, [u32; 28]>, ray: &Ray) -> Option<(u64, [f32; 4])> {
        let mut best: Option<(u64, [f32; 4], f32)> = None;
        let mut stack = vec![root];
        while let Some(p) = stack.pop() {
            let words = nodes[&(p & !7 | if bvh::kind(p) <= 3 { 0 } else { bvh::kind(p) as u64 })];
            let r = bvh::intersect(&words, bvh::kind(p), ray, true);
            if bvh::kind(p) >= 4 {
                for &c in r.iter().rev() {
                    if c != NONE {
                        stack.push(u64::from(c));
                    }
                }
            } else {
                let f = r.map(f32::from_bits);
                let d = f[0] / f[1];
                if d.is_finite() && d >= 0.0 && d <= ray.extent && best.as_ref().is_none_or(|b| d < b.2) {
                    best = Some((p, f, d));
                }
            }
        }
        best.map(|(p, f, _)| (p, f))
    }

    #[test]
    fn a_flat_list_holds_every_triangle_once_with_its_pointer() {
        let (root, nodes) = structure(7);
        let read = |p: u64| nodes.get(&(p & !7 | if bvh::kind(p) <= 3 { 0 } else { bvh::kind(p) as u64 })).copied();
        let flat = flatten(root, read, 100).unwrap();
        assert_eq!(flat.triangles.len(), 7);
        assert_eq!(flat.boxes, 3);
        let mut pointers: Vec<u64> = flat.triangles.iter().map(|t| t.pointer).collect();
        pointers.sort_unstable();
        pointers.dedup();
        assert_eq!(pointers.len(), 7);
    }

    #[test]
    fn the_closest_hit_of_the_flat_list_is_the_traversals() {
        let mut hits = 0;
        for seed in 1..40u32 {
            let (root, nodes) = structure(seed);
            let read = |p: u64| nodes.get(&(p & !7 | if bvh::kind(p) <= 3 { 0 } else { bvh::kind(p) as u64 })).copied();
            let flat = flatten(root, read, 100).unwrap();
            let mut s = seed.wrapping_mul(747796405) | 1;
            let mut next = || {
                s ^= s << 13;
                s ^= s >> 17;
                s ^= s << 5;
                (s % 2000) as f32 / 100.0 - 10.0
            };
            for _ in 0..300 {
                let origin = [next() * 2.0, next() * 2.0, next() * 2.0];
                let target = [next(), next(), next()];
                let dir = [target[0] - origin[0], target[1] - origin[1], target[2] - origin[2]];
                let ray = Ray::new(origin, dir, 1.0e6);
                let want = traverse(root, &nodes, &ray);
                let got = closest(&flat, &ray).map(|(t, r)| (t.pointer, r));
                match (want, got) {
                    (None, None) => {}
                    (Some(w), Some(g)) => {
                        hits += 1;
                        // The same distance; the same triangle unless two
                        // lie at exactly that distance.
                        assert_eq!(w.1[0] / w.1[1], g.1[0] / g.1[1], "seed {seed}");
                        if w.0 == g.0 {
                            assert_eq!(w.1.map(f32::to_bits), g.1.map(f32::to_bits));
                        }
                    }
                    (w, g) => panic!("seed {seed}: traversal {w:?}, flat {g:?}"),
                }
            }
        }
        assert!(hits > 100, "only {hits} hits: the test's rays miss");
    }

    #[test]
    fn the_elements_give_every_triangle_with_its_pointer() {
        let mut nodes: HashMap<u64, [u32; 28]> = HashMap::new();
        // Header: 2 elements from offset 0x80.
        let mut h = [0u32; 16];
        h[3] = 2;
        h[13] = 0x80;
        let mut a = tri_node(&[[0.0; 3], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [1.0, 1.0, 0.0]]);
        a[15] = 0x0404; // two triangles
        let mut b = tri_node(&[[0.0; 3], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]]);
        b[15] = 0x04; // one
        nodes.insert(0x80 >> 3, a);
        nodes.insert(0xc0 >> 3, b);
        let flat = elements(&h, |p| nodes.get(&p).copied()).unwrap();
        let pointers: Vec<u64> = flat.triangles.iter().map(|t| t.pointer).collect();
        assert_eq!(pointers, vec![0x10, 0x11, 0x18]);
        assert_eq!(flat.triangles[0].vertices, bvh::triangle_vertices(&a, 0));
        assert_eq!(flat.triangles[1].vertices, bvh::triangle_vertices(&a, 1));
    }

    #[test]
    fn a_cycle_is_cut_by_the_limit_or_the_seen_set() {
        let mut nodes: HashMap<u64, [u32; 28]> = HashMap::new();
        let root = slot_ptr(1) | 5;
        nodes.insert(root, box32([root as u32, NONE, NONE, NONE], [[0.0; 6], empty(), empty(), empty()]));
        let read = |p: u64| nodes.get(&p).copied();
        let flat = flatten(root, read, 10).unwrap();
        assert_eq!(flat.boxes, 1);
        assert!(flatten(slot_ptr(9) | 4, |_| None, 10).is_err());
    }
}

/// Offline: the structures of a dump of title memory by address
/// (`VICTORY_MEMORY_DUMP`: `pages.txt` and `arena-<n>.bin`), each walked
/// flat. `cargo test -p victory-gpu --lib dumped -- --ignored --nocapture`
/// with `VICTORY_BVH_DUMP=<that folder>`.
#[cfg(test)]
pub(crate) mod dumped {
    use super::*;
    use std::collections::{BTreeSet, HashMap};

    const PAGE: u64 = 1 << 16;
    const ARENA: u64 = 64 << 20;

    pub(crate) struct Memory {
        pages: HashMap<u64, (usize, u64)>,
        arenas: Vec<Vec<u8>>,
        /// Packed (static) structures the runtime saw: address, bytes.
        packed: std::collections::BTreeMap<u64, u64>,
    }

    impl Memory {
        pub(crate) fn dword(&self, address: u64) -> Option<u32> {
            let (arena, at) = *self.pages.get(&(address / PAGE))?;
            let at = (at + address % PAGE) as usize;
            let bytes = self.arenas.get(arena)?.get(at..at + 4)?;
            Some(u32::from_le_bytes(bytes.try_into().ok()?))
        }
        pub(crate) fn words<const N: usize>(&self, address: u64) -> Option<[u32; N]> {
            let mut w = [0u32; N];
            for (i, d) in w.iter_mut().enumerate() {
                *d = self.dword(address + 4 * i as u64)?;
            }
            Some(w)
        }
    }

    pub(crate) fn load(folder: &std::path::Path) -> (Memory, Vec<u64>) {
        let text = std::fs::read_to_string(folder.join("pages.txt")).expect("pages.txt");
        let mut pages = HashMap::new();
        let mut walked = BTreeSet::new();
        let mut packed = std::collections::BTreeMap::new();
        let hex = |s: &str| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok();
        for line in text.lines() {
            let f: Vec<&str> = line.split_whitespace().collect();
            match f.as_slice() {
                ["walked", a] => {
                    walked.extend(hex(a));
                }
                ["packed", a, len, _] => {
                    if let (Some(a), Ok(len)) = (hex(a), len.parse::<u64>()) {
                        packed.insert(a, len);
                    }
                }
                [page, arena, at] if page.starts_with("0x") => {
                    if let (Some(page), Ok(arena), Some(at)) = (hex(page), arena.parse::<usize>(), hex(at)) {
                        pages.insert(page / PAGE, (arena, at));
                    }
                }
                _ => {}
            }
        }
        let mut arenas = Vec::new();
        for n in 0.. {
            match std::fs::read(folder.join(format!("arena-{n}.bin"))) {
                Ok(b) => {
                    assert!(b.len() as u64 <= ARENA);
                    arenas.push(b);
                }
                Err(_) => break,
            }
        }
        (Memory { pages, arenas, packed }, walked.into_iter().collect())
    }

    /// One structure at `base` (its 64-byte header there): flattened from
    /// the header's root pointer (dword 12), nodes at `base + (pointer >> 3)
    /// << 6`.
    fn walk(memory: &Memory, base: u64) -> Result<(Flat, [u32; 16]), String> {
        let header: [u32; 16] = memory.words(base).ok_or("header not there")?;
        let root = u64::from(header[12]);
        let read = |p: u64| memory.words::<28>(base + ((p >> 3) << 6));
        flatten(root, read, 4 << 20).map(|f| (f, header)).map_err(|e| format!("{e:?}"))
    }

    /// The bottom levels the instances of the walked top levels name
    /// (dword 15 of an instance node, in 256-byte units).
    pub(super) fn blas_of_dump(memory: &Memory, walked: &[u64]) -> BTreeSet<u64> {
        let mut out = BTreeSet::new();
        for &base in walked {
            let Ok((flat, _)) = walk(memory, base) else { continue };
            for &i in &flat.instances {
                if let Some(w) = memory.words::<16>(base + ((i >> 3) << 6)) {
                    out.insert(u64::from(w[15]) << 8);
                }
            }
        }
        out
    }

    #[test]
    #[ignore]
    fn the_dumped_structures_walk_flat() {
        let Some(folder) = std::env::var_os("VICTORY_BVH_DUMP") else {
            eprintln!("VICTORY_BVH_DUMP not set");
            return;
        };
        let (memory, walked) = load(std::path::Path::new(&folder));
        eprintln!("{} pages, {} arenas, {} structures walked by trace programs", memory.pages.len(), memory.arenas.len(), walked.len());
        let mut blas: BTreeSet<u64> = BTreeSet::new();
        let mut roots: HashMap<u64, u64> = HashMap::new();
        for &base in &walked {
            match walk(&memory, base) {
                Ok((flat, h)) => {
                    eprintln!(
                        "{base:#x}: header {:08x} {:08x} .. root {:08x}; {} boxes, {} triangles, {} instances, {} procedural",
                        h[0], h[1], h[12], flat.boxes, flat.triangles.len(), flat.instances.len(), flat.procedural.len()
                    );
                    let mut shown_static = 0;
                    let (mut statics, mut static_packed, mut static_header) = (0, 0, 0);
                    for (n, &i) in flat.instances.iter().enumerate() {
                        let Some(w) = memory.words::<32>(base + ((i >> 3) << 6)) else { continue };
                        if w[14] & (1 << 30) != 0 {
                            statics += 1;
                            let a = u64::from(w[15]) << 8;
                            static_packed += usize::from(memory.packed.contains_key(&a));
                            static_header += usize::from(memory.words::<16>(a).is_some_and(|h| h[0] & 0xffff == 6));
                            if shown_static < 4 {
                                shown_static += 1;
                                let near = memory.packed.range(..=a).next_back().map(|(p, l)| format!("{p:#x}+{l}")).unwrap_or_default();
                                let h: [u32; 16] = memory.words(a).unwrap_or_default();
                                eprintln!(
                                    "  static instance {i:#x}: {:08x?}; dword 15 << 8 = {a:#x}, header there {:08x} {:08x} root {:08x}; packed below it: {near}",
                                    &w[12..20], h[0], h[1], h[12]
                                );
                            }
                        }
                        if n < 3 {
                            eprintln!("  instance {i:#x}: {:08x?}", &w[..20]);
                        }
                        // Dwords 0-11 a matrix, 12 id and mask, 13 flags, 14
                        // the root pointer (bit 30: a static structure), 15 the
                        // bottom level's address in 256-byte units (run
                        // 20261126-140839's dump).
                        blas.insert(u64::from(w[15]) << 8);
                        roots.entry(u64::from(w[15]) << 8).or_insert(u64::from(w[14] & !(1 << 30)));
                    }
                    eprintln!("  {statics} static instances (root pointer bit 30): {static_packed} at a packed structure's address, {static_header} at a version-6 header");
                    // The top level's own element list: instance records of
                    // 128 bytes from header dword 13, dword 3 of them.
                    let (first, count) = (u64::from(h[13]), u64::from(h[3]));
                    let (mut good, mut readable, mut inverse) = (0, 0, 0);
                    for k in 0..count {
                        let Some(w) = memory.words::<32>(base + first + 128 * k) else { continue };
                        readable += 1;
                        let a = u64::from(w[15]) << 8;
                        let named = memory.words::<16>(a).is_some_and(|b| b[0] & 0xffff == 6 && b[0] >> 31 == 1);
                        good += usize::from(named);
                        if named {
                            // Dwords 0-11 and 16-27: two 3x4 row-major
                            // matrices; one the other's inverse?
                            let m = |o: usize| -> [[f32; 4]; 3] {
                                std::array::from_fn(|r| std::array::from_fn(|c| f32::from_bits(w[o + 4 * r + c])))
                            };
                            let (a0, b0) = (m(0), m(16));
                            let mut worst = 0f32;
                            for r in 0..3 {
                                for c in 0..4 {
                                    let mut v = if c == 3 { a0[r][3] } else { 0.0 };
                                    for k in 0..3 {
                                        v += a0[r][k] * b0[k][c];
                                    }
                                    let want = if c == r { 1.0 } else { 0.0 };
                                    worst = worst.max((v - want).abs());
                                }
                            }
                            if worst < 1.0e-3 {
                                inverse += 1;
                            } else if good <= 3 {
                                eprintln!("  instance {k} record: {:08x?}", &w[..32]);
                            }
                        }
                    }
                    eprintln!("  {inverse} of {good} instances: dwords 0-11 times dwords 16-27 is the identity");
                    eprintln!(
                        "  its element list: header {:08x?}; {count} instances from {first:#x}, {readable} readable, {good} naming a version-6 bottom level",
                        &h[..16]
                    );
                }
                Err(e) => eprintln!("{base:#x}: {e}"),
            }
        }
        let (mut ok, mut tris, mut bad, mut proc_, mut absent, mut same_root) = (0, 0usize, 0, 0usize, 0, 0);
        let (mut by_elements, mut element_tris, mut element_bad, mut finite) = (0, 0usize, 0, 0usize);
        let mut classes: std::collections::BTreeMap<String, (usize, usize, usize, usize)> = std::collections::BTreeMap::new();
        for &b in &blas {
            if memory.dword(b).is_none() {
                absent += 1;
                continue;
            }
            if let Some(h) = memory.words::<16>(b) {
                let read = |p: u64| memory.words::<28>(b + ((p >> 3) << 6));
                let class = if h[0] & 0xffff != 6 { "not version 6".to_owned() } else { format!("marks {:04x}", h[0] >> 16) };
                let e = classes.entry(class).or_insert((0usize, 0usize, 0usize, 0usize));
                e.0 += 1;
                match elements(&h, read) {
                    Ok(flat) => {
                        by_elements += 1;
                        element_tris += flat.triangles.len();
                        let good = flat.triangles.iter().filter(|t| t.vertices.iter().flatten().all(|v| v.is_finite() && v.abs() < 1.0e6)).count();
                        finite += good;
                        e.1 += flat.triangles.len();
                        e.2 += good;
                    }
                    Err(_) => {
                        element_bad += 1;
                        e.3 += 1;
                    }
                }
            }
            match walk(&memory, b) {
                Ok((flat, h)) => {
                    ok += 1;
                    if ok <= 5 {
                        eprintln!(
                            "bottom level {b:#x}: header {:08x} nodes {:#x} elements {} root {:08x}: {} boxes, {} triangles",
                            h[0], h[1], h[3], h[12], flat.boxes, flat.triangles.len()
                        );
                    }
                    tris += flat.triangles.len();
                    proc_ += flat.procedural.len();
                    same_root += usize::from(roots.get(&b) == Some(&u64::from(h[12])));
                }
                Err(e) => {
                    bad += 1;
                    let h: [u32; 16] = memory.words(b).unwrap_or_default();
                    let missing = e
                        .strip_prefix("Unreadable(")
                        .and_then(|r| r.trim_end_matches(')').parse::<u64>().ok())
                        .map(|p| {
                            let at = b + ((p >> 3) << 6);
                            format!("node {p:#x} kind {} at {at:#x}, its page {}", p & 7, if memory.dword(at).is_some() { "there" } else { "not dumped" })
                        })
                        .unwrap_or_default();
                    eprintln!(
                        "bottom level {b:#x}: {e}; header version/marks {:08x} nodes {:#x} elements {} root {:08x} (instances' {:08x}); {missing}",
                        h[0],
                        h[1],
                        h[3],
                        h[12],
                        roots.get(&b).copied().unwrap_or(0)
                    );
                }
            }
        }
        for (class, (n, t, good, bad)) in &classes {
            eprintln!("  {class}: {n} bottom levels, {t} triangles, {good} with sane vertices, {bad} unreadable");
        }
        eprintln!(
            "from their element nodes: {by_elements} bottom levels read ({element_bad} not), {element_tris} triangles, {finite} of them with finite vertices within 1e6"
        );
        eprintln!(
            "bottom levels named by instances: {} ({absent} not in the dump; {ok} walked, {bad} not; {tris} triangles, {proc_} procedural; \
             {same_root} whose header root is the instances' root pointer)",
            blas.len()
        );
    }
}

/// On the card: a host (DXR) structure built from a dumped bottom level's
/// element triangles, traced with ray queries, against [`closest`] over the
/// same triangles. `VICTORY_BVH_DUMP=<dump folder>`; `cargo test -p
/// victory-gpu --lib dxr -- --ignored --nocapture`. Needs the Windows SDK's
/// `dxc.exe` (shader model 6.5) and a card with ray tracing tier 1.1.
#[cfg(all(test, windows))]
mod dxr {
    use super::dumped::{blas_of_dump, load};
    use super::*;
    use windows::Win32::Graphics::Direct3D::D3D_FEATURE_LEVEL_12_0;
    use windows::Win32::Graphics::Direct3D12::*;
    use windows::Win32::Graphics::Dxgi::Common::*;
    use windows::Win32::Graphics::Dxgi::*;
    use windows::Win32::System::Threading::{CreateEventW, INFINITE, WaitForSingleObject};
    use windows::core::Interface;

    const DXC: &str = r"C:\Program Files (x86)\Windows Kits\10\bin\10.0.26100.0\x64\dxc.exe";
    const RAYS: usize = 8192;

    const TRACE: &str = r#"
#define RS "SRV(t0), SRV(t1), UAV(u0)"
RaytracingAccelerationStructure scene : register(t0);
struct R { float4 o; float4 d; };
StructuredBuffer<R> rays : register(t1);
RWStructuredBuffer<float4> hits : register(u0);
[RootSignature(RS)]
[numthreads(64, 1, 1)]
void main(uint3 id : SV_DispatchThreadID) {
    R r = rays[id.x];
    RayDesc d;
    d.Origin = r.o.xyz;
    d.Direction = r.d.xyz;
    d.TMin = 0.0;
    d.TMax = r.o.w;
    RayQuery<RAY_FLAG_FORCE_OPAQUE> q;
    q.TraceRayInline(scene, RAY_FLAG_NONE, 0xff, d);
    q.Proceed();
    if (q.CommittedStatus() == COMMITTED_TRIANGLE_HIT) {
        float2 b = q.CommittedTriangleBarycentrics();
        hits[id.x] = float4(asfloat(q.CommittedPrimitiveIndex()), q.CommittedRayT(), b.x, b.y);
    } else {
        hits[id.x] = float4(asfloat(0xffffffffu), -1.0, 0.0, 0.0);
    }
}
"#;

    fn buffer(device: &ID3D12Device, size: u64, heap: D3D12_HEAP_TYPE, state: D3D12_RESOURCE_STATES, uav: bool) -> ID3D12Resource {
        let props = if heap == D3D12_HEAP_TYPE_CUSTOM {
            D3D12_HEAP_PROPERTIES {
                Type: D3D12_HEAP_TYPE_CUSTOM,
                CPUPageProperty: D3D12_CPU_PAGE_PROPERTY_WRITE_BACK,
                MemoryPoolPreference: D3D12_MEMORY_POOL_L0,
                ..Default::default()
            }
        } else {
            D3D12_HEAP_PROPERTIES { Type: heap, ..Default::default() }
        };
        let desc = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
            Width: size.max(256),
            Height: 1,
            DepthOrArraySize: 1,
            MipLevels: 1,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
            Flags: if uav { D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS } else { D3D12_RESOURCE_FLAG_NONE },
            ..Default::default()
        };
        let mut out: Option<ID3D12Resource> = None;
        // SAFETY: a plain buffer.
        unsafe { device.CreateCommittedResource(&props, D3D12_HEAP_FLAG_NONE, &desc, state, None, &mut out) }.expect("buffer");
        out.expect("buffer")
    }

    fn mapped(b: &ID3D12Resource) -> *mut u8 {
        let mut data = std::ptr::null_mut();
        // SAFETY: a CPU-visible buffer, mapped whole.
        unsafe { b.Map(0, None, Some(&mut data)) }.expect("map");
        data.cast()
    }

    #[test]
    #[ignore]
    fn ray_queries_find_the_closest_element_triangle() {
        let Some(folder) = std::env::var_os("VICTORY_BVH_DUMP") else {
            eprintln!("VICTORY_BVH_DUMP not set");
            return;
        };
        if !std::path::Path::new(DXC).exists() {
            eprintln!("no {DXC}");
            return;
        }
        // The dumped bottom level with the most sane element triangles.
        let (memory, walked) = load(std::path::Path::new(&folder));
        let mut best: Option<(u64, Flat)> = None;
        for b in blas_of_dump(&memory, &walked) {
            let Some(h) = memory.words::<16>(b) else { continue };
            if h[0] & 0xffff != 6 || h[0] >> 16 != 0x8002 {
                continue;
            }
            let read = |p: u64| memory.words::<28>(b + ((p >> 3) << 6));
            let Ok(mut flat) = elements(&h, read) else { continue };
            flat.triangles.retain(|t| t.vertices.iter().flatten().all(|v| v.is_finite() && v.abs() < 1.0e6));
            if best.as_ref().is_none_or(|(_, f)| flat.triangles.len() > f.triangles.len()) {
                best = Some((b, flat));
            }
        }
        let (base, flat) = best.expect("no clean bottom level in the dump");
        let n = flat.triangles.len();
        eprintln!("bottom level {base:#x}: {n} triangles");
        // Its bounds, and rays from around it at points inside it.
        let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
        for v in flat.triangles.iter().flat_map(|t| t.vertices) {
            for a in 0..3 {
                lo[a] = lo[a].min(v[a]);
                hi[a] = hi[a].max(v[a]);
            }
        }
        let mut s = 0x9e37_79b9u32;
        let mut next = || {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            (s >> 8) as f32 / (1u32 << 24) as f32
        };
        let mut rays = Vec::with_capacity(RAYS);
        for _ in 0..RAYS {
            let inside: [f32; 3] = std::array::from_fn(|a| lo[a] + (hi[a] - lo[a]) * next());
            let origin: [f32; 3] = std::array::from_fn(|a| lo[a] - (hi[a] - lo[a]) * 0.5 + (hi[a] - lo[a]) * 2.0 * next());
            let dir: [f32; 3] = std::array::from_fn(|a| inside[a] - origin[a]);
            rays.push((origin, dir));
        }

        // SAFETY: a test's own device, buffers and lists, used in order and
        // waited for.
        unsafe {
            let factory: IDXGIFactory4 = CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0)).unwrap();
            let adapter: IDXGIAdapter = match crate::host::fast_adapter() {
                Some(fast) => fast.cast().unwrap(),
                None => factory.EnumAdapters(0).unwrap(),
            };
            let mut device: Option<ID3D12Device> = None;
            D3D12CreateDevice(&adapter, D3D_FEATURE_LEVEL_12_0, &mut device).unwrap();
            let device = device.unwrap();
            let device5: ID3D12Device5 = device.cast().expect("ID3D12Device5");
            let mut options5 = D3D12_FEATURE_DATA_D3D12_OPTIONS5::default();
            device
                .CheckFeatureSupport(
                    D3D12_FEATURE_D3D12_OPTIONS5,
                    std::ptr::from_mut(&mut options5).cast(),
                    std::mem::size_of_val(&options5) as u32,
                )
                .unwrap();
            if options5.RaytracingTier.0 < D3D12_RAYTRACING_TIER_1_1.0 {
                eprintln!("ray tracing tier {:?}: no ray queries", options5.RaytracingTier);
                return;
            }
            let queue: ID3D12CommandQueue = device
                .CreateCommandQueue(&D3D12_COMMAND_QUEUE_DESC { Type: D3D12_COMMAND_LIST_TYPE_DIRECT, ..Default::default() })
                .unwrap();
            let allocator: ID3D12CommandAllocator = device.CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT).unwrap();
            let list: ID3D12GraphicsCommandList4 = device
                .CreateCommandList::<_, _, ID3D12GraphicsCommandList>(0, D3D12_COMMAND_LIST_TYPE_DIRECT, &allocator, None)
                .unwrap()
                .cast()
                .unwrap();

            // Vertices, three a triangle, in the PC's memory the card reads.
            let vertices = buffer(&device, (n * 36) as u64, D3D12_HEAP_TYPE_CUSTOM, D3D12_RESOURCE_STATE_COMMON, false);
            let v = mapped(&vertices).cast::<f32>();
            for (i, t) in flat.triangles.iter().enumerate() {
                for (k, p) in t.vertices.iter().enumerate() {
                    for c in 0..3 {
                        *v.add(i * 9 + k * 3 + c) = p[c];
                    }
                }
            }
            let geometry = D3D12_RAYTRACING_GEOMETRY_DESC {
                Type: D3D12_RAYTRACING_GEOMETRY_TYPE_TRIANGLES,
                Flags: D3D12_RAYTRACING_GEOMETRY_FLAG_OPAQUE,
                Anonymous: D3D12_RAYTRACING_GEOMETRY_DESC_0 {
                    Triangles: D3D12_RAYTRACING_GEOMETRY_TRIANGLES_DESC {
                        Transform3x4: 0,
                        IndexFormat: DXGI_FORMAT_UNKNOWN,
                        VertexFormat: DXGI_FORMAT_R32G32B32_FLOAT,
                        IndexCount: 0,
                        VertexCount: (n * 3) as u32,
                        IndexBuffer: 0,
                        VertexBuffer: D3D12_GPU_VIRTUAL_ADDRESS_AND_STRIDE { StartAddress: vertices.GetGPUVirtualAddress(), StrideInBytes: 12 },
                    },
                },
            };
            let bottom_inputs = D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS {
                Type: D3D12_RAYTRACING_ACCELERATION_STRUCTURE_TYPE_BOTTOM_LEVEL,
                Flags: D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BUILD_FLAG_PREFER_FAST_TRACE,
                NumDescs: 1,
                DescsLayout: D3D12_ELEMENTS_LAYOUT_ARRAY,
                Anonymous: D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS_0 { pGeometryDescs: &geometry },
            };
            let mut bottom_info = D3D12_RAYTRACING_ACCELERATION_STRUCTURE_PREBUILD_INFO::default();
            device5.GetRaytracingAccelerationStructurePrebuildInfo(&bottom_inputs, &mut bottom_info);
            let rtas = D3D12_RESOURCE_STATE_RAYTRACING_ACCELERATION_STRUCTURE;
            let bottom = buffer(&device, bottom_info.ResultDataMaxSizeInBytes, D3D12_HEAP_TYPE_DEFAULT, rtas, true);
            // One instance of it, unmoved.
            let instances = buffer(&device, 64, D3D12_HEAP_TYPE_CUSTOM, D3D12_RESOURCE_STATE_COMMON, false);
            let instance = D3D12_RAYTRACING_INSTANCE_DESC {
                Transform: [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                _bitfield1: 0xff << 24,
                _bitfield2: 0,
                AccelerationStructure: bottom.GetGPUVirtualAddress(),
            };
            std::ptr::write(mapped(&instances).cast(), instance);
            let top_inputs = D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS {
                Type: D3D12_RAYTRACING_ACCELERATION_STRUCTURE_TYPE_TOP_LEVEL,
                Flags: D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BUILD_FLAG_PREFER_FAST_TRACE,
                NumDescs: 1,
                DescsLayout: D3D12_ELEMENTS_LAYOUT_ARRAY,
                Anonymous: D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS_0 { InstanceDescs: instances.GetGPUVirtualAddress() },
            };
            let mut top_info = D3D12_RAYTRACING_ACCELERATION_STRUCTURE_PREBUILD_INFO::default();
            device5.GetRaytracingAccelerationStructurePrebuildInfo(&top_inputs, &mut top_info);
            let top = buffer(&device, top_info.ResultDataMaxSizeInBytes, D3D12_HEAP_TYPE_DEFAULT, rtas, true);
            let scratch_size = bottom_info.ScratchDataSizeInBytes.max(top_info.ScratchDataSizeInBytes);
            let scratch = buffer(&device, scratch_size, D3D12_HEAP_TYPE_DEFAULT, D3D12_RESOURCE_STATE_UNORDERED_ACCESS, true);
            list.BuildRaytracingAccelerationStructure(
                &D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_DESC {
                    DestAccelerationStructureData: bottom.GetGPUVirtualAddress(),
                    Inputs: bottom_inputs,
                    SourceAccelerationStructureData: 0,
                    ScratchAccelerationStructureData: scratch.GetGPUVirtualAddress(),
                },
                None,
            );
            let uav = D3D12_RESOURCE_BARRIER {
                Type: D3D12_RESOURCE_BARRIER_TYPE_UAV,
                Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
                Anonymous: D3D12_RESOURCE_BARRIER_0 {
                    UAV: std::mem::ManuallyDrop::new(D3D12_RESOURCE_UAV_BARRIER { pResource: std::mem::ManuallyDrop::new(None) }),
                },
            };
            list.ResourceBarrier(std::slice::from_ref(&uav));
            list.BuildRaytracingAccelerationStructure(
                &D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_DESC {
                    DestAccelerationStructureData: top.GetGPUVirtualAddress(),
                    Inputs: top_inputs,
                    SourceAccelerationStructureData: 0,
                    ScratchAccelerationStructureData: scratch.GetGPUVirtualAddress(),
                },
                None,
            );
            list.ResourceBarrier(std::slice::from_ref(&uav));

            // The rays and the results.
            let ray_buffer = buffer(&device, (RAYS * 32) as u64, D3D12_HEAP_TYPE_CUSTOM, D3D12_RESOURCE_STATE_COMMON, false);
            let r = mapped(&ray_buffer).cast::<f32>();
            for (i, (o, d)) in rays.iter().enumerate() {
                let w = [o[0], o[1], o[2], 1.0e6, d[0], d[1], d[2], 0.0];
                std::ptr::copy_nonoverlapping(w.as_ptr(), r.add(i * 8), 8);
            }
            let results = buffer(&device, (RAYS * 16) as u64, D3D12_HEAP_TYPE_CUSTOM, D3D12_RESOURCE_STATE_UNORDERED_ACCESS, true);

            // The trace program.
            let dir = std::env::temp_dir().join(format!("victory-dxr-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("trace.hlsl"), TRACE).unwrap();
            let status = std::process::Command::new(DXC)
                .args(["-T", "cs_6_5", "-E", "main", "-Fo"])
                .arg(dir.join("trace.dxil"))
                .arg(dir.join("trace.hlsl"))
                .status()
                .unwrap();
            assert!(status.success(), "dxc");
            let code = std::fs::read(dir.join("trace.dxil")).unwrap();
            let root: ID3D12RootSignature = device.CreateRootSignature(0, &code).unwrap();
            let pso: ID3D12PipelineState = device
                .CreateComputePipelineState(&D3D12_COMPUTE_PIPELINE_STATE_DESC {
                    pRootSignature: std::mem::ManuallyDrop::new(Some(root.clone())),
                    CS: D3D12_SHADER_BYTECODE { pShaderBytecode: code.as_ptr().cast(), BytecodeLength: code.len() },
                    ..Default::default()
                })
                .unwrap();
            list.SetComputeRootSignature(&root);
            list.SetPipelineState(&pso);
            list.SetComputeRootShaderResourceView(0, top.GetGPUVirtualAddress());
            list.SetComputeRootShaderResourceView(1, ray_buffer.GetGPUVirtualAddress());
            list.SetComputeRootUnorderedAccessView(2, results.GetGPUVirtualAddress());
            list.Dispatch((RAYS / 64) as u32, 1, 1);
            list.Close().unwrap();
            queue.ExecuteCommandLists(&[Some(list.cast().unwrap())]);
            let fence: ID3D12Fence = device.CreateFence(0, D3D12_FENCE_FLAG_NONE).unwrap();
            let event = CreateEventW(None, false, false, None).unwrap();
            queue.Signal(&fence, 1).unwrap();
            fence.SetEventOnCompletion(1, event).unwrap();
            WaitForSingleObject(event, INFINITE);
            device.GetDeviceRemovedReason().unwrap();

            // Against the instruction's own triangle test over the same list.
            let got = std::slice::from_raw_parts(mapped(&results).cast::<[f32; 4]>(), RAYS);
            let (mut hits, mut agree, mut same_triangle, mut close_t, mut misses_both) = (0, 0, 0, 0, 0);
            let mut disagree = Vec::new();
            for (i, (o, d)) in rays.iter().enumerate() {
                let ray = bvh::Ray::new(*o, *d, 1.0e6);
                let want = closest(&flat, &ray);
                let g = got[i];
                let g_prim = g[0].to_bits();
                match (want, g_prim) {
                    (None, u32::MAX) => {
                        agree += 1;
                        misses_both += 1;
                    }
                    (Some((t, r)), p) if p != u32::MAX => {
                        hits += 1;
                        agree += 1;
                        let index = flat.triangles.iter().position(|x| x.pointer == t.pointer).unwrap() as u32;
                        same_triangle += usize::from(index == p);
                        let want_t = r[0] / r[1];
                        if (want_t - g[1]).abs() <= 1.0e-4 * want_t.abs().max(1.0) {
                            close_t += 1;
                        } else if disagree.len() < 5 {
                            disagree.push(format!("ray {i}: t {want_t} (triangle {index}) on the card {} (triangle {p})", g[1]));
                        }
                    }
                    (w, p) => {
                        if disagree.len() < 5 {
                            disagree.push(format!("ray {i}: instruction {:?}, card primitive {p:#x} t {}", w.map(|(_, r)| r[0] / r[1]), g[1]));
                        }
                    }
                }
            }
            eprintln!(
                "{RAYS} rays: {agree} agree on hit or miss ({hits} hits, {misses_both} misses); of the hits {same_triangle} the same triangle, {close_t} the same distance (1e-4)"
            );
            for d in &disagree {
                eprintln!("  {d}");
            }
            let _ = std::fs::remove_dir_all(&dir);
            assert!(hits > RAYS / 10, "too few hits to judge: {hits}");
            assert!(agree * 1000 >= RAYS * 995, "hit or miss differs for {} of {RAYS} rays", RAYS - agree);
            assert!(close_t * 1000 >= hits * 995, "distance differs for {} of {hits} hits", hits - close_t);
        }
    }

    /// A test's device, list and the steps of a trace.
    struct Gpu {
        device: ID3D12Device,
        device5: ID3D12Device5,
        queue: ID3D12CommandQueue,
        allocator: ID3D12CommandAllocator,
        list: ID3D12GraphicsCommandList4,
        fence: ID3D12Fence,
        done: u64,
        kept: Vec<ID3D12Resource>,
    }

    impl Gpu {
        /// None without ray queries.
        fn new() -> Option<Gpu> {
            // SAFETY: a test's own device and objects.
            unsafe {
                let factory: IDXGIFactory4 = CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0)).ok()?;
                let adapter: IDXGIAdapter = match crate::host::fast_adapter() {
                    Some(fast) => fast.cast().ok()?,
                    None => factory.EnumAdapters(0).ok()?,
                };
                let mut device: Option<ID3D12Device> = None;
                D3D12CreateDevice(&adapter, D3D_FEATURE_LEVEL_12_0, &mut device).ok()?;
                let device = device?;
                let mut options5 = D3D12_FEATURE_DATA_D3D12_OPTIONS5::default();
                device
                    .CheckFeatureSupport(D3D12_FEATURE_D3D12_OPTIONS5, std::ptr::from_mut(&mut options5).cast(), std::mem::size_of_val(&options5) as u32)
                    .ok()?;
                if options5.RaytracingTier.0 < D3D12_RAYTRACING_TIER_1_1.0 {
                    return None;
                }
                let queue: ID3D12CommandQueue =
                    device.CreateCommandQueue(&D3D12_COMMAND_QUEUE_DESC { Type: D3D12_COMMAND_LIST_TYPE_DIRECT, ..Default::default() }).ok()?;
                let allocator: ID3D12CommandAllocator = device.CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT).ok()?;
                let list: ID3D12GraphicsCommandList4 = device
                    .CreateCommandList::<_, _, ID3D12GraphicsCommandList>(0, D3D12_COMMAND_LIST_TYPE_DIRECT, &allocator, None)
                    .ok()?
                    .cast()
                    .ok()?;
                let fence: ID3D12Fence = device.CreateFence(0, D3D12_FENCE_FLAG_NONE).ok()?;
                Some(Gpu { device5: device.cast().ok()?, device, queue, allocator, list, fence, done: 0, kept: Vec::new() })
            }
        }

        fn run(&mut self) {
            // SAFETY: our list, run and waited for.
            unsafe {
                self.list.Close().unwrap();
                self.queue.ExecuteCommandLists(&[Some(self.list.cast().unwrap())]);
                self.done += 1;
                self.queue.Signal(&self.fence, self.done).unwrap();
                let event = CreateEventW(None, false, false, None).unwrap();
                self.fence.SetEventOnCompletion(self.done, event).unwrap();
                WaitForSingleObject(event, INFINITE);
                self.device.GetDeviceRemovedReason().unwrap();
                self.allocator.Reset().unwrap();
                self.list.Reset(&self.allocator, None).unwrap();
                self.kept.clear();
            }
        }

        /// Records the build of a structure from `inputs`; its buffer.
        fn build(&mut self, inputs: &D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS) -> ID3D12Resource {
            // SAFETY: inputs the caller keeps alive until `run`.
            unsafe {
                let mut info = D3D12_RAYTRACING_ACCELERATION_STRUCTURE_PREBUILD_INFO::default();
                self.device5.GetRaytracingAccelerationStructurePrebuildInfo(inputs, &mut info);
                let out = buffer(&self.device, info.ResultDataMaxSizeInBytes, D3D12_HEAP_TYPE_DEFAULT, D3D12_RESOURCE_STATE_RAYTRACING_ACCELERATION_STRUCTURE, true);
                let scratch = buffer(&self.device, info.ScratchDataSizeInBytes, D3D12_HEAP_TYPE_DEFAULT, D3D12_RESOURCE_STATE_UNORDERED_ACCESS, true);
                self.list.BuildRaytracingAccelerationStructure(
                    &D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_DESC {
                        DestAccelerationStructureData: out.GetGPUVirtualAddress(),
                        Inputs: *inputs,
                        SourceAccelerationStructureData: 0,
                        ScratchAccelerationStructureData: scratch.GetGPUVirtualAddress(),
                    },
                    None,
                );
                self.kept.push(scratch);
                out
            }
        }

        /// A bottom level of `triangles`, built when the list runs.
        fn bottom(&mut self, triangles: &[Triangle]) -> ID3D12Resource {
            let vertices = buffer(&self.device, (triangles.len() * 36) as u64, D3D12_HEAP_TYPE_CUSTOM, D3D12_RESOURCE_STATE_COMMON, false);
            let v = mapped(&vertices).cast::<f32>();
            for (i, t) in triangles.iter().enumerate() {
                for (k, p) in t.vertices.iter().enumerate() {
                    for c in 0..3 {
                        // SAFETY: inside the mapping just made.
                        unsafe { *v.add(i * 9 + k * 3 + c) = p[c] };
                    }
                }
            }
            // SAFETY: the vertex buffer lives until `run` (kept).
            let address = unsafe { vertices.GetGPUVirtualAddress() };
            let geometry = D3D12_RAYTRACING_GEOMETRY_DESC {
                Type: D3D12_RAYTRACING_GEOMETRY_TYPE_TRIANGLES,
                Flags: D3D12_RAYTRACING_GEOMETRY_FLAG_OPAQUE,
                Anonymous: D3D12_RAYTRACING_GEOMETRY_DESC_0 {
                    Triangles: D3D12_RAYTRACING_GEOMETRY_TRIANGLES_DESC {
                        Transform3x4: 0,
                        IndexFormat: DXGI_FORMAT_UNKNOWN,
                        VertexFormat: DXGI_FORMAT_R32G32B32_FLOAT,
                        IndexCount: 0,
                        VertexCount: (triangles.len() * 3) as u32,
                        IndexBuffer: 0,
                        VertexBuffer: D3D12_GPU_VIRTUAL_ADDRESS_AND_STRIDE { StartAddress: address, StrideInBytes: 12 },
                    },
                },
            };
            let inputs = D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS {
                Type: D3D12_RAYTRACING_ACCELERATION_STRUCTURE_TYPE_BOTTOM_LEVEL,
                Flags: D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BUILD_FLAG_PREFER_FAST_TRACE,
                NumDescs: 1,
                DescsLayout: D3D12_ELEMENTS_LAYOUT_ARRAY,
                Anonymous: D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS_0 { pGeometryDescs: &geometry },
            };
            let out = self.build(&inputs);
            self.kept.push(vertices);
            out
        }

        /// A top level of `instances`, built after the bottom levels.
        fn top(&mut self, instances: &[D3D12_RAYTRACING_INSTANCE_DESC]) -> ID3D12Resource {
            let descs = buffer(&self.device, (instances.len() * 64) as u64, D3D12_HEAP_TYPE_CUSTOM, D3D12_RESOURCE_STATE_COMMON, false);
            // SAFETY: inside the mapping just made.
            unsafe { std::ptr::copy_nonoverlapping(instances.as_ptr(), mapped(&descs).cast(), instances.len()) };
            let barrier = D3D12_RESOURCE_BARRIER {
                Type: D3D12_RESOURCE_BARRIER_TYPE_UAV,
                Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
                Anonymous: D3D12_RESOURCE_BARRIER_0 {
                    UAV: std::mem::ManuallyDrop::new(D3D12_RESOURCE_UAV_BARRIER { pResource: std::mem::ManuallyDrop::new(None) }),
                },
            };
            // SAFETY: our open list; the descriptions live until `run` (kept).
            let out = unsafe {
                self.list.ResourceBarrier(std::slice::from_ref(&barrier));
                let inputs = D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS {
                    Type: D3D12_RAYTRACING_ACCELERATION_STRUCTURE_TYPE_TOP_LEVEL,
                    Flags: D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BUILD_FLAG_PREFER_FAST_TRACE,
                    NumDescs: instances.len() as u32,
                    DescsLayout: D3D12_ELEMENTS_LAYOUT_ARRAY,
                    Anonymous: D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS_0 { InstanceDescs: descs.GetGPUVirtualAddress() },
                };
                let out = self.build(&inputs);
                self.list.ResourceBarrier(std::slice::from_ref(&barrier));
                out
            };
            self.kept.push(descs);
            out
        }

        /// Each ray (origin, direction) traced in `top`: (instance index,
        /// primitive index, distance), the indices `u32::MAX` for a miss.
        fn trace(&mut self, top: &ID3D12Resource, rays: &[([f32; 3], [f32; 3])]) -> Vec<(u32, u32, f32)> {
            let count = rays.len().next_multiple_of(64);
            let ray_buffer = buffer(&self.device, (count * 32) as u64, D3D12_HEAP_TYPE_CUSTOM, D3D12_RESOURCE_STATE_COMMON, false);
            let r = mapped(&ray_buffer).cast::<f32>();
            for (i, (o, d)) in rays.iter().enumerate() {
                let w = [o[0], o[1], o[2], 1.0e30, d[0], d[1], d[2], 0.0];
                // SAFETY: inside the mapping just made.
                unsafe { std::ptr::copy_nonoverlapping(w.as_ptr(), r.add(i * 8), 8) };
            }
            let results = buffer(&self.device, (count * 16) as u64, D3D12_HEAP_TYPE_CUSTOM, D3D12_RESOURCE_STATE_UNORDERED_ACCESS, true);
            let dir = std::env::temp_dir().join(format!("victory-dxr-scene-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("trace.hlsl"), TRACE_SCENE).unwrap();
            let status = std::process::Command::new(DXC)
                .args(["-T", "cs_6_5", "-E", "main", "-Fo"])
                .arg(dir.join("trace.dxil"))
                .arg(dir.join("trace.hlsl"))
                .status()
                .unwrap();
            assert!(status.success(), "dxc");
            let code = std::fs::read(dir.join("trace.dxil")).unwrap();
            let _ = std::fs::remove_dir_all(&dir);
            // SAFETY: our device and open list; the buffers live until `run`.
            unsafe {
                let root: ID3D12RootSignature = self.device.CreateRootSignature(0, &code).unwrap();
                let pso: ID3D12PipelineState = self
                    .device
                    .CreateComputePipelineState(&D3D12_COMPUTE_PIPELINE_STATE_DESC {
                        pRootSignature: std::mem::ManuallyDrop::new(Some(root.clone())),
                        CS: D3D12_SHADER_BYTECODE { pShaderBytecode: code.as_ptr().cast(), BytecodeLength: code.len() },
                        ..Default::default()
                    })
                    .unwrap();
                self.list.SetComputeRootSignature(&root);
                self.list.SetPipelineState(&pso);
                self.list.SetComputeRootShaderResourceView(0, top.GetGPUVirtualAddress());
                self.list.SetComputeRootShaderResourceView(1, ray_buffer.GetGPUVirtualAddress());
                self.list.SetComputeRootUnorderedAccessView(2, results.GetGPUVirtualAddress());
                self.list.Dispatch((count / 64) as u32, 1, 1);
                self.kept.push(ray_buffer);
                self.run();
                let got = std::slice::from_raw_parts(mapped(&results).cast::<[u32; 4]>(), rays.len());
                got.iter().map(|g| (g[0], g[1], f32::from_bits(g[2]))).collect()
            }
        }
    }

    const TRACE_SCENE: &str = r#"
#define RS "SRV(t0), SRV(t1), UAV(u0)"
RaytracingAccelerationStructure scene : register(t0);
struct R { float4 o; float4 d; };
StructuredBuffer<R> rays : register(t1);
RWStructuredBuffer<uint4> hits : register(u0);
[RootSignature(RS)]
[numthreads(64, 1, 1)]
void main(uint3 id : SV_DispatchThreadID) {
    R r = rays[id.x];
    RayDesc d;
    d.Origin = r.o.xyz;
    d.Direction = r.d.xyz;
    d.TMin = 0.0;
    d.TMax = r.o.w;
    RayQuery<RAY_FLAG_FORCE_OPAQUE> q;
    q.TraceRayInline(scene, RAY_FLAG_NONE, 0xff, d);
    q.Proceed();
    if (q.CommittedStatus() == COMMITTED_TRIANGLE_HIT) {
        hits[id.x] = uint4(q.CommittedInstanceIndex(), q.CommittedPrimitiveIndex(), asuint(q.CommittedRayT()), 0);
    } else {
        hits[id.x] = uint4(0xffffffffu, 0xffffffffu, asuint(-1.0), 0);
    }
}
"#;

    /// `m` (a 3x4 row-major affine) applied to point `p` (`w` 1) or
    /// direction (`w` 0).
    fn apply(m: &[[f32; 4]; 3], p: [f32; 3], w: f32) -> [f32; 3] {
        std::array::from_fn(|r| m[r][0] * p[0] + m[r][1] * p[1] + m[r][2] * p[2] + m[r][3] * w)
    }

    /// The inverse of a 3x4 affine.
    fn invert(m: &[[f32; 4]; 3]) -> Option<[[f32; 4]; 3]> {
        let a = |r: usize, c: usize| f64::from(m[r][c]);
        let det = a(0, 0) * (a(1, 1) * a(2, 2) - a(1, 2) * a(2, 1)) - a(0, 1) * (a(1, 0) * a(2, 2) - a(1, 2) * a(2, 0))
            + a(0, 2) * (a(1, 0) * a(2, 1) - a(1, 1) * a(2, 0));
        if det.abs() < 1.0e-12 || !det.is_finite() {
            return None;
        }
        let mut inv = [[0f64; 4]; 3];
        for r in 0..3 {
            for c in 0..3 {
                let (r1, r2, c1, c2) = ((c + 1) % 3, (c + 2) % 3, (r + 1) % 3, (r + 2) % 3);
                inv[r][c] = (a(r1, c1) * a(r2, c2) - a(r1, c2) * a(r2, c1)) / det;
            }
        }
        for r in 0..3 {
            inv[r][3] = -(inv[r][0] * a(0, 3) + inv[r][1] * a(1, 3) + inv[r][2] * a(2, 3));
        }
        Some(inv.map(|row| row.map(|v| v as f32)))
    }

    #[test]
    fn affine_inverse_undoes_the_matrix() {
        let m = [[0.0, -2.0, 0.0, 5.0], [1.5, 0.0, 0.0, -3.0], [0.0, 0.0, 0.5, 7.0]];
        let inv = invert(&m).unwrap();
        let p = [1.0, 2.0, 3.0];
        let back = apply(&inv, apply(&m, p, 1.0), 1.0);
        for a in 0..3 {
            assert!((back[a] - p[a]).abs() < 1.0e-5);
        }
    }

    /// The whole dumped top level as a host structure: each instance's
    /// bottom level from its element triangles, its matrix (dwords 0-11,
    /// world to object as the title's walk uses it) inverted for the host's
    /// object-to-world transform. Against the instruction's triangle test
    /// over every instance whose world box the ray enters.
    #[test]
    #[ignore]
    fn ray_queries_find_the_closest_triangle_of_the_scene() {
        let Some(folder) = std::env::var_os("VICTORY_BVH_DUMP") else {
            eprintln!("VICTORY_BVH_DUMP not set");
            return;
        };
        if !std::path::Path::new(DXC).exists() {
            return;
        }
        let (memory, walked) = load(std::path::Path::new(&folder));
        let Some(mut gpu) = Gpu::new() else {
            eprintln!("no ray queries");
            return;
        };
        // The top level with the most instances.
        let mut best: Option<(u64, [u32; 16])> = None;
        for &b in &walked {
            if let Some(h) = memory.words::<16>(b)
                && h[0] & 0xffff == 6
                && h[0] >> 31 == 0
                && best.as_ref().is_none_or(|(_, bh)| h[3] > bh[3])
            {
                best = Some((b, h));
            }
        }
        let (top_base, th) = best.expect("no top level");
        // Bottom levels by address, from their elements.
        let mut bottoms: std::collections::BTreeMap<u64, Flat> = std::collections::BTreeMap::new();
        struct Placed {
            blas: u64,
            wto: [[f32; 4]; 3],
            otw: [[f32; 4]; 3],
            lo: [f32; 3],
            hi: [f32; 3],
        }
        let mut placed: Vec<Placed> = Vec::new();
        for k in 0..u64::from(th[3]) {
            let Some(w) = memory.words::<32>(top_base + u64::from(th[13]) + 128 * k) else { continue };
            let blas = u64::from(w[15]) << 8;
            let Some(bh) = memory.words::<16>(blas) else { continue };
            if bh[0] & 0xffff != 6 || bh[0] >> 31 != 1 || bh[0] >> 16 & 0x100 != 0 {
                continue;
            }
            if !bottoms.contains_key(&blas) {
                let read = |p: u64| memory.words::<28>(blas + ((p >> 3) << 6));
                let Ok(mut flat) = elements(&bh, read) else { continue };
                flat.triangles.retain(|t| t.vertices.iter().flatten().all(|v| v.is_finite() && v.abs() < 1.0e6));
                if flat.triangles.is_empty() {
                    continue;
                }
                bottoms.insert(blas, flat);
            }
            let wto: [[f32; 4]; 3] = std::array::from_fn(|r| std::array::from_fn(|c| f32::from_bits(w[4 * r + c])));
            let Some(otw) = invert(&wto) else { continue };
            // World box from the object-space vertices' box.
            let (mut olo, mut ohi) = ([f32::MAX; 3], [f32::MIN; 3]);
            for v in bottoms[&blas].triangles.iter().flat_map(|t| t.vertices) {
                for a in 0..3 {
                    olo[a] = olo[a].min(v[a]);
                    ohi[a] = ohi[a].max(v[a]);
                }
            }
            let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
            for corner in 0..8 {
                let p = [if corner & 1 == 0 { olo[0] } else { ohi[0] }, if corner & 2 == 0 { olo[1] } else { ohi[1] }, if corner & 4 == 0 { olo[2] } else { ohi[2] }];
                let q = apply(&otw, p, 1.0);
                for a in 0..3 {
                    lo[a] = lo[a].min(q[a]);
                    hi[a] = hi[a].max(q[a]);
                }
            }
            placed.push(Placed { blas, wto, otw, lo, hi });
        }
        let triangles: usize = bottoms.values().map(|f| f.triangles.len()).sum();
        eprintln!("top level {top_base:#x}: {} instances placed of {}, {} bottom levels, {triangles} triangles", placed.len(), th[3], bottoms.len());
        assert!(!placed.is_empty());
        // Host structures.
        let built: std::collections::BTreeMap<u64, ID3D12Resource> = bottoms.iter().map(|(&a, f)| (a, gpu.bottom(&f.triangles))).collect();
        let descs: Vec<D3D12_RAYTRACING_INSTANCE_DESC> = placed
            .iter()
            .enumerate()
            .map(|(i, p)| D3D12_RAYTRACING_INSTANCE_DESC {
                Transform: std::array::from_fn(|n| p.otw[n / 4][n % 4]),
                _bitfield1: (i as u32 & 0xff_ffff) | 0xff << 24,
                _bitfield2: 0,
                // SAFETY: a live buffer.
                AccelerationStructure: unsafe { built[&p.blas].GetGPUVirtualAddress() },
            })
            .collect();
        let top = gpu.top(&descs);
        gpu.run();
        // Rays from one instance's box toward another's.
        let mut s = 0x2545_f491u32;
        let mut next = || {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            s
        };
        let mut rays = Vec::new();
        for _ in 0..2048 {
            let a = &placed[next() as usize % placed.len()];
            let b = &placed[next() as usize % placed.len()];
            let mut f = || (next() >> 8) as f32 / (1u32 << 24) as f32;
            let from: [f32; 3] = std::array::from_fn(|k| a.lo[k] + (a.hi[k] - a.lo[k]) * f());
            let to: [f32; 3] = std::array::from_fn(|k| b.lo[k] + (b.hi[k] - b.lo[k]) * f());
            let d = [to[0] - from[0], to[1] - from[1], to[2] - from[2]];
            if d.iter().all(|x| x.abs() > 1.0e-3) {
                rays.push((from, d));
            }
        }
        let got = gpu.trace(&top, &rays);
        // The reference: every instance whose world box the ray enters.
        let (mut agree, mut hits, mut same, mut close) = (0, 0, 0, 0);
        let mut shown = 0;
        for (i, (o, d)) in rays.iter().enumerate() {
            let mut want: Option<(usize, u32, f32)> = None;
            for (n, p) in placed.iter().enumerate() {
                let (mut t0, mut t1) = (0f32, f32::MAX);
                for k in 0..3 {
                    let inv = 1.0 / d[k];
                    let (a, b) = ((p.lo[k] - o[k]) * inv, (p.hi[k] - o[k]) * inv);
                    t0 = t0.max(a.min(b));
                    t1 = t1.min(a.max(b));
                }
                if t0 > t1 * 1.0001 + 1.0e-4 {
                    continue;
                }
                let ray = bvh::Ray::new(apply(&p.wto, *o, 1.0), apply(&p.wto, *d, 0.0), 1.0e30);
                if let Some((t, r)) = closest(&bottoms[&p.blas], &ray) {
                    let dist = r[0] / r[1];
                    if want.is_none_or(|w| dist < w.2) {
                        let index = bottoms[&p.blas].triangles.iter().position(|x| x.pointer == t.pointer).unwrap() as u32;
                        want = Some((n, index, dist));
                    }
                }
            }
            let g = got[i];
            match want {
                None if g.0 == u32::MAX => agree += 1,
                Some(w) if g.0 != u32::MAX => {
                    agree += 1;
                    hits += 1;
                    same += usize::from(w.0 as u32 == g.0 && w.1 == g.1);
                    if (w.2 - g.2).abs() <= 1.0e-3 * w.2.abs().max(1.0) {
                        close += 1;
                    } else if shown < 5 {
                        shown += 1;
                        eprintln!("  ray {i}: reference instance {} triangle {} t {}; card instance {} triangle {} t {}", w.0, w.1, w.2, g.0, g.1, g.2);
                    }
                }
                w => {
                    if shown < 5 {
                        shown += 1;
                        eprintln!("  ray {i}: reference {w:?}; card {g:?}");
                    }
                }
            }
        }
        eprintln!("{} rays: {agree} agree on hit or miss ({hits} hits); {same} same instance and triangle, {close} same distance (1e-3)", rays.len());
        assert!(hits > rays.len() / 10, "too few hits: {hits}");
        assert!(agree * 100 >= rays.len() * 99);
        assert!(close * 100 >= hits * 99);
    }

    /// Compiled with the SDK's dxc (shader model 6.5): the DXIL.
    fn compile(source: &str) -> Vec<u8> {
        let dir = std::env::temp_dir().join(format!("victory-dxr-cs-{}-{}", std::process::id(), source.len()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("cs.hlsl"), source).unwrap();
        let status = std::process::Command::new(DXC)
            .args(["-T", "cs_6_5", "-E", "main", "-Fo"])
            .arg(dir.join("cs.dxil"))
            .arg(dir.join("cs.hlsl"))
            .status()
            .unwrap();
        assert!(status.success(), "dxc");
        let code = std::fs::read(dir.join("cs.dxil")).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        code
    }

    /// The pass that turns a bottom level's element nodes into a vertex
    /// buffer for the host's build: four triangles an element, three
    /// vertices each in the instruction's order (the one neither barycentric
    /// weighs, then I's, then J's), all NaN where the element holds no such
    /// triangle (an inactive primitive to the host). So primitive `p` is
    /// triangle `p % 4` of element `p / 4`, and its title pointer is
    /// `(elements + 64 * (p / 4)) >> 3 | p % 4`: no table needed.
    pub(super) const ELEMENTS_CS: &str = r#"
#define RS "RootConstants(num32BitConstants=2, b0), SRV(t0), UAV(u0)"
cbuffer C : register(b0) { uint first; uint count; };
ByteAddressBuffer nodes : register(t0);
RWByteAddressBuffer vertices : register(u0);
static const uint MAP[4] = { 0x210u, 0x231u, 0x432u, 0x042u };
[RootSignature(RS)]
[numthreads(64, 1, 1)]
void main(uint3 id : SV_DispatchThreadID) {
    uint e = id.x;
    if (e >= count) return;
    uint at = first + 64u * e;
    uint w[16];
    [unroll] for (uint n = 0u; n < 16u; ++n) w[n] = nodes.Load(at + 4u * n);
    [unroll] for (uint k = 0u; k < 4u; ++k) {
        uint tri = (e * 4u + k) * 36u;
        uint byte_ = (w[15] >> (8u * k)) & 0xffu;
        if (byte_ == 0u) {
            [unroll] for (uint d = 0u; d < 9u; ++d) vertices.Store(tri + 4u * d, 0x7fc00000u);
            continue;
        }
        uint i = byte_ & 3u;
        uint j = (byte_ >> 2u) & 3u;
        uint order[3] = { (3u - i - j) & 3u, i, j };
        [unroll] for (uint v = 0u; v < 3u; ++v) {
            uint vertex = (MAP[k] >> (4u * order[v])) & 0xfu;
            [unroll] for (uint c = 0u; c < 3u; ++c) {
                vertices.Store(tri + 12u * v + 4u * c, w[3u * vertex + c]);
            }
        }
    }
}
"#;

    /// The extraction pass on the card gives the vertices of
    /// [`super::elements`], bit for bit, for every clean bottom level of the
    /// dump, and NaN for the triangles an element does not hold.
    #[test]
    #[ignore]
    fn the_element_pass_on_the_card_gives_the_triangles() {
        let Some(folder) = std::env::var_os("VICTORY_BVH_DUMP") else {
            eprintln!("VICTORY_BVH_DUMP not set");
            return;
        };
        if !std::path::Path::new(DXC).exists() {
            return;
        }
        let (memory, walked) = load(std::path::Path::new(&folder));
        let Some(mut gpu) = Gpu::new() else {
            eprintln!("no ray queries");
            return;
        };
        let code = compile(ELEMENTS_CS);
        let (mut levels, mut checked, mut inactive) = (0, 0usize, 0usize);
        for b in blas_of_dump(&memory, &walked) {
            let Some(h) = memory.words::<16>(b) else { continue };
            if h[0] & 0xffff != 6 || h[0] >> 31 != 1 || h[0] >> 16 & 0x100 != 0 {
                continue;
            }
            let (first, count) = (u64::from(h[13]), u64::from(h[3]));
            if count == 0 || count > 1 << 20 {
                continue;
            }
            // The element nodes' bytes, as the card would read them.
            let len = 64 * count;
            let Some(words) = (0..len / 4).map(|n| memory.dword(b + first + 4 * n)).collect::<Option<Vec<u32>>>() else { continue };
            let read = |p: u64| memory.words::<28>(b + ((p >> 3) << 6));
            let Ok(flat) = elements(&h, read) else { continue };
            let nodes = buffer(&gpu.device, len, D3D12_HEAP_TYPE_CUSTOM, D3D12_RESOURCE_STATE_COMMON, false);
            // SAFETY: inside the mapping.
            unsafe { std::ptr::copy_nonoverlapping(words.as_ptr(), mapped(&nodes).cast(), words.len()) };
            let out_len = count * 4 * 36;
            let out = buffer(&gpu.device, out_len, D3D12_HEAP_TYPE_CUSTOM, D3D12_RESOURCE_STATE_UNORDERED_ACCESS, true);
            // SAFETY: our device and list; buffers live through the run.
            let got: Vec<u32> = unsafe {
                let root: ID3D12RootSignature = gpu.device.CreateRootSignature(0, &code).unwrap();
                let pso: ID3D12PipelineState = gpu
                    .device
                    .CreateComputePipelineState(&D3D12_COMPUTE_PIPELINE_STATE_DESC {
                        pRootSignature: std::mem::ManuallyDrop::new(Some(root.clone())),
                        CS: D3D12_SHADER_BYTECODE { pShaderBytecode: code.as_ptr().cast(), BytecodeLength: code.len() },
                        ..Default::default()
                    })
                    .unwrap();
                gpu.list.SetComputeRootSignature(&root);
                gpu.list.SetPipelineState(&pso);
                gpu.list.SetComputeRoot32BitConstants(0, 2, [0u32, count as u32].as_ptr().cast(), 0);
                gpu.list.SetComputeRootShaderResourceView(1, nodes.GetGPUVirtualAddress());
                gpu.list.SetComputeRootUnorderedAccessView(2, out.GetGPUVirtualAddress());
                gpu.list.Dispatch(count.div_ceil(64) as u32, 1, 1);
                gpu.run();
                std::slice::from_raw_parts(mapped(&out).cast::<u32>(), (out_len / 4) as usize).to_vec()
            };
            levels += 1;
            // Every triangle of the CPU's list where its pointer says.
            for t in &flat.triangles {
                let e = ((t.pointer & !7) << 3) - first;
                let p = (e / 64 * 4 + (t.pointer & 7)) as usize;
                for (v, vertex) in t.vertices.iter().enumerate() {
                    for c in 0..3 {
                        assert_eq!(got[p * 9 + v * 3 + c], vertex[c].to_bits(), "{b:#x} primitive {p}");
                    }
                }
                checked += 1;
            }
            // And NaN where the CPU's list has none.
            let held: std::collections::HashSet<usize> =
                flat.triangles.iter().map(|t| ((((t.pointer & !7) << 3) - first) / 64 * 4 + (t.pointer & 7)) as usize).collect();
            for p in 0..(count * 4) as usize {
                if !held.contains(&p) {
                    assert!(f32::from_bits(got[p * 9]).is_nan(), "{b:#x} primitive {p} should be inactive");
                    inactive += 1;
                }
            }
        }
        eprintln!("{levels} bottom levels: {checked} triangles the same bit for bit, {inactive} inactive slots NaN");
        assert!(levels > 10 && checked > 10_000);
    }
}
