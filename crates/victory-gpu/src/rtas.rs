//! The title's own calls into the console's ray-tracing structure builder
//! (`XboxBvhBuilder.dll`), seen from the layer: the native builder's front
//! end (`C:\Users\Corgan\victory-mcp\DXR-PLAN.md`, plan #2).
//!
//! The builder is a DLL the title ships. Its exports are D3D12's own entry
//! points as members of an `XboxBvhBuilder` object: the title hands it the
//! very `D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_DESC` a PC title hands
//! a command list (destination, inputs: type, flags, count, geometry
//! descriptions or the address of the instances; source; scratch), and the
//! DLL records its 34 compute programs on the list. So the layer can know
//! every build as the title meant it, instead of decoding the programs'
//! arguments.
//!
//! `VICTORY_BVH_HOOK=1` puts `victory-hle`'s wrappers in front of the
//! exports (`victory_hle::modules::bvh`). For now they report here and call
//! the DLL as before ([`Calls`]): what the title asks, how often, and of what
//! size, for the native builder that will answer the builds itself.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

/// Whether the wrappers are in front of the builder's exports
/// (`VICTORY_BVH_HOOK=1`).
pub fn on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_BVH_HOOK").is_some_and(|v| v == "1"))
}

/// D3D12_RAYTRACING_ACCELERATION_STRUCTURE_TYPE_TOP_LEVEL.
pub const TOP: u32 = 0;
/// D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BUILD_FLAG_PERFORM_UPDATE.
pub const UPDATE: u32 = 0x20;
/// D3D12_ELEMENTS_LAYOUT_ARRAY_OF_POINTERS.
pub const POINTERS: u32 = 1;
/// Bytes of a `D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_DESC`.
pub const DESC: usize = 48;
/// Bytes of a `D3D12_RAYTRACING_GEOMETRY_DESC`.
pub const GEOMETRY: usize = 56;
/// Bytes of a `D3D12_RAYTRACING_INSTANCE_DESC`.
pub const INSTANCE: usize = 64;

/// A `D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_DESC` as the title wrote it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Build {
    pub dest: u64,
    pub kind: u32,
    pub flags: u32,
    pub count: u32,
    pub layout: u32,
    /// The instances' address (a top level), or the geometry descriptions'
    /// (an array, or an array of pointers to them).
    pub descs: u64,
    pub source: u64,
    pub scratch: u64,
}

impl Build {
    pub fn parse(b: &[u8; DESC]) -> Build {
        let q = |at: usize| u64::from_le_bytes(b[at..at + 8].try_into().unwrap());
        let d = |at: usize| u32::from_le_bytes(b[at..at + 4].try_into().unwrap());
        Build {
            dest: q(0),
            kind: d(8),
            flags: d(12),
            count: d(16),
            layout: d(20),
            descs: q(24),
            source: q(32),
            scratch: q(40),
        }
    }
}

/// One `D3D12_RAYTRACING_GEOMETRY_DESC`: its type (0 triangles, 1 boxes),
/// flags, and for triangles the description `crate::records` reads (the
/// same 48 bytes the builder's programs are given).
pub fn geometry(b: &[u8; GEOMETRY]) -> (u32, u32, Option<crate::records::Geometry>) {
    let kind = u32::from_le_bytes(b[0..4].try_into().unwrap());
    let flags = u32::from_le_bytes(b[4..8].try_into().unwrap());
    let triangles = (kind == 0).then(|| crate::records::Geometry::parse(b[8..56].try_into().unwrap())).flatten();
    (kind, flags, triangles)
}

/// One `D3D12_RAYTRACING_INSTANCE_DESC`: transform, id, mask, flags, the
/// bottom level's address.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Instance {
    pub transform: [f32; 12],
    pub id: u32,
    pub mask: u8,
    pub hit_group: u32,
    pub flags: u8,
    pub bottom: u64,
}

impl Instance {
    pub fn parse(b: &[u8; INSTANCE]) -> Instance {
        let d = |at: usize| u32::from_le_bytes(b[at..at + 4].try_into().unwrap());
        Instance {
            transform: std::array::from_fn(|k| f32::from_bits(d(4 * k))),
            id: d(48) & 0xff_ffff,
            mask: (d(48) >> 24) as u8,
            hit_group: d(52) & 0xff_ffff,
            flags: (d(52) >> 24) as u8,
            bottom: u64::from_le_bytes(b[56..64].try_into().unwrap()),
        }
    }
}

/// Reads title memory (the CPU's view; what is not there reads as nothing).
#[cfg(windows)]
fn read<const N: usize>(address: u64) -> Option<[u8; N]> {
    let mut b = [0u8; N];
    (address != 0 && crate::host::read_there(address as *const u8, &mut b) == N).then_some(b)
}

#[cfg(not(windows))]
fn read<const N: usize>(_: u64) -> Option<[u8; N]> {
    None
}

/// What the title asked since the last report.
#[derive(Default)]
struct Calls {
    since: Option<std::time::Instant>,
    frame: u64,
    /// Builds of bottom levels, of top levels; of them updates (refits).
    bottoms: u64,
    tops: u64,
    updates: u64,
    geometries: u64,
    triangles: u64,
    boxes: u64,
    instances: u64,
    /// Of the geometries: packed indices, packed vertices (the console's
    /// formats, `crate::records`), with a transform.
    packed_indices: u64,
    packed_vertices: u64,
    transformed: u64,
    postbuilds: u64,
    batched: u64,
    batched_descs: u64,
    rehydrations: u64,
    rehydrated: u64,
    copies: [u64; 4],
    prebuilds: u64,
    /// Destinations seen (all of the run).
    dests: std::collections::HashSet<u64>,
    /// Lines of detail written, by what they are of.
    detailed: std::collections::HashMap<&'static str, u32>,
}

static CALLS: Mutex<Option<Calls>> = Mutex::new(None);

/// Lines of detail written, of each call; the rest are counted.
const DETAIL: u32 = 120;

fn with(f: impl FnOnce(&mut Calls)) {
    let mut guard = CALLS.lock().unwrap_or_else(|e| e.into_inner());
    let c = guard.get_or_insert_with(Calls::default);
    f(c);
    c.report();
}

impl Calls {
    fn report(&mut self) {
        let now = std::time::Instant::now();
        let frame = crate::frame::current();
        let Some(since) = self.since else {
            (self.since, self.frame) = (Some(now), frame);
            return;
        };
        if now.duration_since(since).as_secs_f64() < 10.0 {
            return;
        }
        let frames = frame.wrapping_sub(self.frame).max(1) as f64;
        tracing::info!(
            "the title's ray-tracing builds (VICTORY_BVH_HOOK), per frame over {frames:.0} frames: {:.1} bottom levels ({:.1} geometries, {:.0} triangles, {:.1} box geometries; {:.1} packed indices, {:.1} packed vertices, {:.1} with a transform), \
             {:.2} top levels ({:.0} instances), {:.1} updates, {:.1} post-build infos; {:.2} batched calls ({:.1} descriptions), {:.2} rehydrations ({:.1} structures); \
             copies {:.2} clone, {:.2} compact, {:.2} other; {:.1} prebuild infos; {} destinations in all",
            self.bottoms as f64 / frames,
            self.geometries as f64 / frames,
            self.triangles as f64 / frames,
            self.boxes as f64 / frames,
            self.packed_indices as f64 / frames,
            self.packed_vertices as f64 / frames,
            self.transformed as f64 / frames,
            self.tops as f64 / frames,
            self.instances as f64 / frames,
            self.updates as f64 / frames,
            self.postbuilds as f64 / frames,
            self.batched as f64 / frames,
            self.batched_descs as f64 / frames,
            self.rehydrations as f64 / frames,
            self.rehydrated as f64 / frames,
            self.copies[0] as f64 / frames,
            self.copies[1] as f64 / frames,
            (self.copies[2] + self.copies[3]) as f64 / frames,
            self.prebuilds as f64 / frames,
            self.dests.len(),
        );
        #[cfg(feature = "diagnostics")]
        last::dump_when_due(frame);
        let (dests, detailed) = (std::mem::take(&mut self.dests), std::mem::take(&mut self.detailed));
        *self = Calls { since: Some(now), frame, dests, detailed, ..Calls::default() };
    }

    /// One build: counted, and the first ones written out.
    fn build(&mut self, b: &Build, what: &'static str, list: *const std::ffi::c_void) {
        self.dests.insert(b.dest);
        #[cfg(feature = "diagnostics")]
        last::note(b, list);
        let _ = list;
        if b.flags & UPDATE != 0 {
            self.updates += 1;
        }
        let detail = self.detail(if b.kind == TOP { "top level" } else { what });
        let mut text = if detail { format!("; destination {}", held(b.dest)) } else { String::new() };
        if b.kind == TOP {
            self.tops += 1;
            self.instances += u64::from(b.count);
            if detail {
                // (The instances are in title memory, by address.)
                let first = read::<INSTANCE>(b.descs).map(|i| Instance::parse(&i));
                let last = (b.count > 1).then(|| read::<INSTANCE>(b.descs + INSTANCE as u64 * u64::from(b.count - 1)).map(|i| Instance::parse(&i))).flatten();
                text += &format!("; instances {}; first instance {first:?}; last {last:?}", held(b.descs));
                if let Some(i) = first {
                    text += &format!("; its bottom level {}", held(i.bottom));
                }
            }
        } else {
            self.bottoms += 1;
            self.geometries += u64::from(b.count);
            for n in 0..u64::from(b.count.min(4096)) {
                let at = if b.layout == POINTERS {
                    match read::<8>(b.descs + 8 * n) {
                        Some(p) => u64::from_le_bytes(p),
                        None => break,
                    }
                } else {
                    b.descs + GEOMETRY as u64 * n
                };
                let Some(g) = read::<GEOMETRY>(at) else { break };
                let (kind, flags, triangles) = geometry(&g);
                match triangles {
                    Some(t) => {
                        self.triangles += u64::from(t.triangles());
                        self.packed_indices += u64::from(t.index_format == crate::records::PACKED_INDICES);
                        self.packed_vertices += u64::from(t.vertex_format == crate::records::PACKED_VERTICES);
                        self.transformed += u64::from(t.transform != 0);
                    }
                    None => self.boxes += u64::from(kind == 1),
                }
                if detail && n < 3 {
                    text += &format!("; geometry {n}: kind {kind} flags {flags:#x} {triangles:?}");
                    if let Some(t) = triangles {
                        // (Written by the CPU, or by the card: a vertex buffer
                        // the title's programs make is not in CPU memory.)
                        text += &format!(" vertices {}", held(t.vertex_buffer));
                        if t.vertex_format == crate::records::VERTICES_FLOAT3 {
                            let v = read::<12>(t.vertex_buffer).map(|b| std::array::from_fn::<f32, 3, _>(|k| f32::from_le_bytes(b[4 * k..4 * k + 4].try_into().unwrap())));
                            text += &format!(" first vertex {v:?}");
                        }
                        if t.index_buffer != 0 {
                            text += &format!(" indices {}", held(t.index_buffer));
                        }
                    }
                }
            }
        }
        if detail {
            tracing::info!("{what}: {b:x?}{text}");
        }
    }

    /// Whether another line of detail of `what` is written (and counted).
    fn detail(&mut self, what: &'static str) -> bool {
        let n = self.detailed.entry(what).or_default();
        *n += 1;
        *n <= DETAIL
    }
}

/// `BuildRaytracingAccelerationStructure`: `desc` the title's description,
/// `posts` post-build info descriptions after it.
pub fn build(list: *const std::ffi::c_void, desc: *const u8, posts: u32) {
    let Some(b) = read::<DESC>(desc as u64) else { return };
    with(|c| {
        c.postbuilds += u64::from(posts);
        c.build(&Build::parse(&b), "build", list);
    });
}

/// Bytes of a `D3D12XBOX_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_BATCHED_DESC2`
/// (run 20261127-180216's bytes): the build description, then the number
/// of post-build info descriptions (dword 12) and their address (13-14),
/// then 16 bytes of zeros.
pub const BATCHED: usize = 80;

/// `BuildRaytracingAccelerationStructureBatched2X`: `count` builds.
pub fn batched(list: *const std::ffi::c_void, count: u32, descs: *const u8) {
    with(|c| {
        c.batched += 1;
        c.batched_descs += u64::from(count);
        for n in 0..u64::from(count.min(4096)) {
            let at = descs as u64 + BATCHED as u64 * n;
            let Some(d) = read::<BATCHED>(at) else { break };
            let b = Build::parse(d[..DESC].try_into().unwrap());
            let posts = u32::from_le_bytes(d[48..52].try_into().unwrap());
            c.postbuilds += u64::from(posts);
            if posts > 0 && c.detailed.get("post-build").copied().unwrap_or(0) < DETAIL {
                *c.detailed.entry("post-build").or_default() += 1;
                let post = u64::from_le_bytes(d[56..64].try_into().unwrap());
                let info = read::<16>(post).map(|p| (u64::from_le_bytes(p[0..8].try_into().unwrap()), u32::from_le_bytes(p[8..12].try_into().unwrap())));
                let at = info.map_or(String::new(), |(a, _)| held(a));
                tracing::info!(posts, "batched build {n}: post-build info (destination, type) {info:x?} {at}; tail {:x?}", &d[64..80]);
            }
            c.build(&b, "batched build", list);
        }
    });
}

/// Where a title address is held: in a title buffer (its start and length),
/// and whether the CPU's copy of its first bytes is all zeros.
#[cfg(windows)]
fn held(address: u64) -> String {
    let buffer = crate::shadow::in_buffer(address, 1);
    let zeros = read::<64>(address).map(|b| b.iter().all(|&x| x == 0));
    format!("in buffer {buffer:x?}, CPU's first 64 bytes zero {zeros:?}")
}

#[cfg(not(windows))]
fn held(_: u64) -> String {
    String::new()
}

/// The `Rehydrate...X` calls: `count` build descriptions (the static world's
/// packed structures, unpacked in place).
pub fn rehydrate(count: u32, descs: *const u8, what: &'static str) {
    with(|c| {
        c.rehydrations += 1;
        c.rehydrated += u64::from(count);
        for n in 0..u64::from(count.min(65536)) {
            let Some(b) = read::<DESC>(descs as u64 + DESC as u64 * n) else { break };
            let b = Build::parse(&b);
            // (Counted as builds too: their geometry is what a native builder makes.)
            c.build(&b, what, std::ptr::null());
        }
    });
}

/// `CopyRaytracingAccelerationStructure`.
pub fn copy(dest: u64, source: u64, mode: u32) {
    with(|c| {
        c.copies[(mode as usize).min(3)] += 1;
        c.dests.insert(dest);
        if c.detail("copy") {
            tracing::info!(dest = format_args!("{dest:#x}"), source = format_args!("{source:#x}"), mode, "copy");
        }
    });
}

/// `GetRaytracingAccelerationStructurePrebuildInfo`.
pub fn prebuild() {
    with(|c| c.prebuilds += 1);
}

/// How much of the builder the layer does itself (`VICTORY_BVH_NATIVE`):
/// `static` the static world's unpacking ([`rehydrate_native`]; the rest is
/// still the DLL's), `all` every call (not yet).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Native {
    Off,
    Static,
    /// And the top levels ([`top_native`]) and the bottom levels the title
    /// builds once of meshes the CPU holds ([`once_native`]). The ones whose
    /// vertices only the card holds (skinned meshes, built and updated every
    /// frame) are not built yet: their instances are left out.
    All,
}

/// [`Native`] of this run.
pub fn native() -> Native {
    static N: std::sync::OnceLock<Native> = std::sync::OnceLock::new();
    *N.get_or_init(|| match std::env::var("VICTORY_BVH_NATIVE").as_deref() {
        Ok("static") => Native::Static,
        Ok("all") => Native::All,
        _ => Native::Off,
    })
}

/// Title memory read once a span ([`crate::records::Geometry::spans`]), the
/// decoding's small reads served from it: one kernel read a buffer, not one
/// a vertex.
#[cfg(windows)]
#[derive(Default)]
struct Spans {
    pieces: Vec<(u64, Vec<u8>)>,
}

#[cfg(windows)]
impl Spans {
    fn take(&mut self, start: u64, len: u64) {
        if len == 0 || len > 64 << 20 || self.pieces.iter().any(|(s, b)| *s <= start && start + len <= s + b.len() as u64) {
            return;
        }
        let mut bytes = vec![0u8; len as usize];
        if crate::host::read_there(start as *const u8, &mut bytes) == bytes.len() {
            self.pieces.push((start, bytes));
        }
    }
}

#[cfg(windows)]
impl crate::records::Read for Spans {
    fn read(&mut self, address: u64, bytes: &mut [u8]) -> bool {
        let n = bytes.len() as u64;
        for (start, piece) in &self.pieces {
            if *start <= address && address + n <= start + piece.len() as u64 {
                let at = (address - start) as usize;
                bytes.copy_from_slice(&piece[at..at + bytes.len()]);
                return true;
            }
        }
        crate::host::read_there(address as *const u8, bytes) == bytes.len()
    }
}

/// A static structure's header (64 bytes, `fork-ray-tracing.md`): version 6,
/// marks in the high half (0x8000 bottom level, 0x100 still packed).
pub const PACKED: u32 = 0x0100_0000;

/// How the static world's element nodes place a geometry's points
/// (LeafRehydration's constants, as the DLL works them out from the header;
/// 300 of 300 dispatches of capture run 20261127-173215): scaled by a power
/// of two per axis (header dword 15, bytes 0-2, as a float's exponent);
/// with byte 3's bit 0x40, less the bounds' centre on each axis whose
/// extreme, scaled, passes 65,536. No rotation in any.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Space {
    pub scale: [f32; 3],
    pub origin: [f32; 3],
}

impl Space {
    pub fn of(header: &[u32; 16]) -> Space {
        let scale: [f32; 3] = std::array::from_fn(|k| f32::from_bits((header[15] >> (8 * k) & 0xff) << 23));
        let lo: [f32; 3] = std::array::from_fn(|k| f32::from_bits(header[4 + k]));
        let hi: [f32; 3] = std::array::from_fn(|k| f32::from_bits(header[7 + k]));
        let centred = header[15] >> 24 & 0x40 != 0;
        let origin = std::array::from_fn(|k| {
            if centred && lo[k].abs().max(hi[k].abs()) * scale[k] > 65_536.0 { (lo[k] + hi[k]) * 0.5 } else { 0.0 }
        });
        Space { scale, origin }
    }

    pub fn place(&self, p: [f32; 3]) -> [f32; 3] {
        std::array::from_fn(|k| (p[k] - self.origin[k]) * self.scale[k])
    }
}

/// One element node as LeafRehydration writes it (dumps of runs 171218 and
/// 173215, every flags value): triangle 0's corners at the node's vertices
/// the low nibble of the flags names (`victory_shader::bvh::triangle_vertices`:
/// the one neither barycentric weighs, then I's, then J's, of vertices 0 1 2),
/// the pair's second triangle by the high nibble (of vertices 1 3 2); vertex
/// 3 of a single triangle NaN; dword 12 geometry << 8 | kind, 13 the
/// primitive, 14 the second (all ones for none), 15 the two id bytes.
pub fn element(e: &crate::records::Entry, first: [[f32; 3]; 3], second: Option<[[f32; 3]; 3]>) -> [u32; 16] {
    let mut w = [0u32; 16];
    let nan = f32::NAN.to_bits();
    for d in &mut w[9..12] {
        *d = nan;
    }
    let mut put = |map: [usize; 3], id: u32, t: [[f32; 3]; 3]| {
        let (i, j) = ((id & 3) as usize, (id >> 2 & 3) as usize);
        let f = (3usize.wrapping_sub(i).wrapping_sub(j)) & 3;
        for (n, slot) in [f, i, j].into_iter().enumerate() {
            let v = map[slot.min(2)];
            for c in 0..3 {
                w[3 * v + c] = t[n][c].to_bits();
            }
        }
    };
    let (low, high) = (u32::from(e.flags & 0xf), u32::from(e.flags >> 4));
    put([0, 1, 2], low, first);
    if let Some(t) = second {
        put([1, 3, 2], high, t);
    }
    w[12] = e.geometry << 8 | u32::from(e.kind);
    w[13] = e.primitive;
    w[14] = e.second.unwrap_or(0xffff_ffff);
    w[15] = low | if second.is_some() { high << 8 } else { 0 };
    w
}

/// Header dword 11 of a bottom level the layer built itself from the
/// title's geometry ([`once`]): made once, every triangle in it (the console
/// builder leaves the dword 0).
pub const ONCE: u32 = 0x3145_434e;
/// Such a structure's layout: the header, a root box node with no children
/// at 64 (16-bit boxes, 64 bytes: what the title's own walk finds before the
/// host's structure is entered: nothing), the element nodes from 128.
pub const ONCE_ROOT: u32 = 64;
pub const ONCE_ELEMENTS: u32 = 128;
/// The most elements of one (the host's builds take no more,
/// `memory.rs` `MOST_ELEMENTS`).
pub const ONCE_MOST: usize = 1 << 16;

/// Why [`once`] built nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotOnce {
    /// No triangles.
    Empty,
    /// More elements than [`ONCE_MOST`].
    TooMany,
    /// An index or a vertex could not be read or decoded.
    Unreadable,
    /// Every vertex of a geometry reads as zeros: the card writes them
    /// (skinned meshes), the CPU does not hold them.
    NotWritten,
}

/// The flags of an element that holds triangles `a` and `b` (their vertex
/// numbers) as a pair, if they share an edge with opposite directions:
/// the low nibble names `a`'s corners so that the shared edge lies on the
/// node's vertices 1 and 2, the high nibble `b`'s (see [`element`]).
pub fn pair(a: [u32; 3], b: [u32; 3]) -> Option<u8> {
    let of_a = [(9u8, (a[1], a[2]), a[0]), (4, (a[2], a[0]), a[1]), (2, (a[0], a[1]), a[2])];
    let of_b = [(9u8, (b[0], b[2]), b[1]), (4, (b[1], b[0]), b[2]), (2, (b[2], b[1]), b[0])];
    for (low, edge, apex) in of_a {
        for (high, other, far) in of_b {
            if edge == other && apex != far && edge.0 != edge.1 {
                return Some(low | high << 4);
            }
        }
    }
    None
}

/// A bottom level of the console's kind made from the title's geometry on
/// the CPU (`VICTORY_BVH_NATIVE=all`, the builds the title makes once of
/// meshes the CPU holds): the header (version 6, a bottom level, marked
/// [`ONCE`], its space the geometry's own), an empty root, and an element
/// node for each triangle or pair of neighbours that share an edge
/// ([`pair`]; dump 5: the console's own dynamic bottom levels hold the same
/// element nodes). `geometries` with their flags (bit 0 opaque);
/// `generation` goes in dword 2, so a structure built again at an address
/// differs from the one before.
pub fn once(
    geometries: &[(crate::records::Geometry, u32)],
    generation: u32,
    memory: &mut impl crate::records::Read,
) -> Result<Vec<u8>, NotOnce> {
    let mut nodes: Vec<[u32; 16]> = Vec::new();
    let (mut lo, mut hi) = ([f32::MAX; 3], [-f32::MAX; 3]);
    for (number, (g, flags)) in geometries.iter().enumerate() {
        let count = g.triangles();
        let mut written = count == 0 || g.vertex_format != crate::records::VERTICES_FLOAT3;
        // (Each vertex decoded once: a mesh's triangles share them, and the
        // builds of a streaming scene are a million triangles in ten
        // seconds, run 20261127-202020.)
        let matrix = g.matrix(memory).map_err(|_| NotOnce::Unreadable)?;
        let mut placed: Vec<Option<[f32; 3]>> = vec![None; g.vertex_count.min(1 << 22) as usize];
        let mut get = |p: u32| -> Option<([u32; 3], [[f32; 3]; 3])> {
            let ix = g.indices(p, memory).ok()?;
            let mut t = [[0.0; 3]; 3];
            for (k, &i) in ix.iter().enumerate() {
                t[k] = match placed.get(i as usize).copied().flatten() {
                    Some(v) => v,
                    None => {
                        let v = crate::records::apply(&matrix, g.position(i, memory).ok()?);
                        if let Some(slot) = placed.get_mut(i as usize) {
                            *slot = Some(v);
                        }
                        v
                    }
                };
            }
            Some((ix, t))
        };
        let mut next = if count > 0 { Some(get(0).ok_or(NotOnce::Unreadable)?) } else { None };
        let mut p = 0u32;
        while let Some((ix, t)) = next.take() {
            let after = if p + 1 < count { Some(get(p + 1).ok_or(NotOnce::Unreadable)?) } else { None };
            let paired = after.and_then(|(jx, u)| pair(ix, jx).map(|f| (f, u)));
            let opaque = (*flags & 1) as u8;
            let entry = crate::records::Entry {
                geometry: number as u32,
                kind: opaque | if paired.is_some() { 0x80 } else { 0 },
                primitive: p,
                second: paired.map(|_| p + 1),
                flags: paired.map_or(9, |(f, _)| f),
            };
            for v in t.iter().chain(paired.iter().flat_map(|(_, u)| u.iter())) {
                if !v.iter().all(|c| c.is_finite()) {
                    return Err(NotOnce::Unreadable);
                }
                written |= v.iter().any(|c| *c != 0.0);
                for k in 0..3 {
                    lo[k] = lo[k].min(v[k]);
                    hi[k] = hi[k].max(v[k]);
                }
            }
            nodes.push(element(&entry, t, paired.map(|(_, u)| u)));
            if nodes.len() > ONCE_MOST {
                return Err(NotOnce::TooMany);
            }
            if paired.is_some() {
                p += 2;
                next = if p < count { Some(get(p).ok_or(NotOnce::Unreadable)?) } else { None };
            } else {
                p += 1;
                next = after;
            }
        }
        if !written {
            return Err(NotOnce::NotWritten);
        }
    }
    if nodes.is_empty() {
        return Err(NotOnce::Empty);
    }
    let mut out = own_structure(ONCE, generation, lo, hi, nodes.len() as u32);
    for (i, w) in nodes.iter().enumerate() {
        for (k, d) in w.iter().enumerate() {
            let at = ONCE_ELEMENTS as usize + 64 * i + 4 * k;
            out[at..at + 4].copy_from_slice(&d.to_le_bytes());
        }
    }
    Ok(out)
}

/// A structure of the layer's own laid out (see [`ONCE_ROOT`]): its header
/// with `mark` in dword 11 and its empty root; the element nodes zeros.
pub fn own_structure(mark: u32, generation: u32, lo: [f32; 3], hi: [f32; 3], count: u32) -> Vec<u8> {
    let size = ONCE_ELEMENTS as usize + 64 * count as usize;
    let mut out = vec![0u8; size];
    let header = [
        0x8004_0006,
        (size / 64 - 1) as u32,
        generation,
        count,
        lo[0].to_bits(),
        lo[1].to_bits(),
        lo[2].to_bits(),
        hi[0].to_bits(),
        hi[1].to_bits(),
        hi[2].to_bits(),
        ONCE_ROOT,
        mark,
        ONCE_ROOT >> 3 | 4,
        ONCE_ELEMENTS,
        0x40,
        0x007f_7f7f,
    ];
    for (k, v) in header.into_iter().enumerate() {
        out[4 * k..4 * k + 4].copy_from_slice(&v.to_le_bytes());
    }
    for k in 0..4 {
        let at = ONCE_ROOT as usize + 4 * k;
        out[at..at + 4].copy_from_slice(&victory_shader::bvh::NONE.to_le_bytes());
    }
    out
}

/// Header dword 11 of a bottom level whose element nodes the card writes
/// from the title's vertices ([`plan`]): a mesh the title skins on the card
/// and builds again or updates as it moves.
pub const CARD: u32 = 0x3144_5243;

/// Whether a header is one of those.
pub fn made_on_card(h: &[u32; 16]) -> bool {
    h[0] == 0x8004_0006 && h[11] == CARD && h[13] == ONCE_ELEMENTS && h[3] > 0 && h[1] == h[3] + 1
}

/// The element nodes of float-vertex `geometries` as a plan for the card
/// (`memory.rs` `GATHER_CS`): eight dwords an element, the byte offsets
/// from title address `base` of the vertices that go to the node's four
/// places (all ones: none, the place reads NaN) and the node's dwords 12
/// to 15; elements and their order as [`once`] makes them. With it the
/// largest offset. The vertices themselves are not read: the card holds
/// them.
pub fn plan(
    geometries: &[(crate::records::Geometry, u32)],
    base: u64,
    memory: &mut impl crate::records::Read,
) -> Result<(Vec<u32>, u32), NotOnce> {
    let mut words: Vec<u32> = Vec::new();
    let mut most = 0u32;
    for (number, (g, flags)) in geometries.iter().enumerate() {
        let stride = u64::from(g.stride);
        let from = g.vertex_buffer.checked_sub(base).ok_or(NotOnce::Unreadable)?;
        if g.vertex_format != crate::records::VERTICES_FLOAT3 || stride < 12 || from + stride * u64::from(g.vertex_count) > u64::from(u32::MAX / 2) {
            return Err(NotOnce::Unreadable);
        }
        let count = g.triangles();
        let mut get = |p: u32| -> Result<[u32; 3], NotOnce> {
            let ix = g.indices(p, memory).map_err(|_| NotOnce::Unreadable)?;
            if ix.iter().any(|&i| i >= g.vertex_count) {
                return Err(NotOnce::Unreadable);
            }
            Ok(ix)
        };
        let mut p = 0u32;
        let mut next = if count > 0 { Some(get(0)?) } else { None };
        while let Some(ix) = next.take() {
            let after = if p + 1 < count { Some(get(p + 1)?) } else { None };
            let paired = after.and_then(|jx| pair(ix, jx).map(|f| (f, jx)));
            let entry = crate::records::Entry {
                geometry: number as u32,
                kind: (*flags & 1) as u8 | if paired.is_some() { 0x80 } else { 0 },
                primitive: p,
                second: paired.map(|_| p + 1),
                flags: paired.map_or(9, |(f, _)| f),
            };
            // Which corner lands in which place: the node made of corners
            // numbered 1 to 6 in their first coordinate.
            let corner = |n: u32| [f32::from_bits(n), 0.0, 0.0];
            let node = element(&entry, [corner(1), corner(2), corner(3)], paired.map(|_| [corner(4), corner(5), corner(6)]));
            for place in 0..4 {
                let offset = match node[3 * place] {
                    n @ 1..=3 => Some(ix[n as usize - 1]),
                    n @ 4..=6 => paired.map(|(_, jx)| jx[n as usize - 4]),
                    _ => None,
                }
                .map(|i| (from + stride * u64::from(i)) as u32);
                most = most.max(offset.unwrap_or(0));
                words.push(offset.unwrap_or(u32::MAX));
            }
            words.extend_from_slice(&node[12..16]);
            if words.len() / 8 > ONCE_MOST {
                return Err(NotOnce::TooMany);
            }
            if paired.is_some() {
                p += 2;
                next = if p < count { Some(get(p)?) } else { None };
            } else {
                p += 1;
                next = after;
            }
        }
    }
    if words.is_empty() {
        return Err(NotOnce::Empty);
    }
    Ok((words, most))
}

/// Whether a header is one of [`once`]'s.
pub fn made_once(h: &[u32; 16]) -> bool {
    h[0] == 0x8004_0006 && h[11] == ONCE && h[13] == ONCE_ELEMENTS && h[3] > 0 && h[1] == h[3] + 1
}

/// A native top level's layout in the title's destination: the header, a
/// root box node with no children at 64 (what the title's own walk finds
/// before the host's structure is entered: nothing), the instance records
/// (128 bytes each) from 256.
pub const TOP_ROOT: u32 = 64;
pub const TOP_RECORDS: u32 = 256;
/// The most instances of a native top level (the host reads a top level's
/// first 512 KiB back, `memory.rs`).
pub const TOP_MOST: u32 = 4000;

/// The inverse of an affine 3x4 matrix (rows), if it has one.
pub fn invert(m: &[f32; 12]) -> Option<[f32; 12]> {
    let [a, b, c, tx, d, e, f, ty, g, h, i, tz] = *m;
    let det = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g);
    if !det.is_finite() || det.abs() < 1e-20 {
        return None;
    }
    let r = [
        (e * i - f * h) / det,
        (c * h - b * i) / det,
        (b * f - c * e) / det,
        (f * g - d * i) / det,
        (a * i - c * g) / det,
        (c * d - a * f) / det,
        (d * h - e * g) / det,
        (b * g - a * h) / det,
        (a * e - b * d) / det,
    ];
    let t = [-(r[0] * tx + r[1] * ty + r[2] * tz), -(r[3] * tx + r[4] * ty + r[5] * tz), -(r[6] * tx + r[7] * ty + r[8] * tz)];
    Some([r[0], r[1], r[2], t[0], r[3], r[4], r[5], t[1], r[6], r[7], r[8], t[2]])
}

/// One instance record of a top level, as the console's builder writes it
/// (dump of run 20261127-184716, 1,034 records against the title's instance
/// descriptions): dwords 0-11 the description's transform, 12 its id and
/// mask, 13 its hit group and flags, 14 the bottom level's root pointer
/// (its header's dword 12; bit 30 for a static one), 15 its address in
/// 256-byte units, 16-27 the world-to-node transform (the translation, then
/// the 3x3 rows; the inverse of the instance's, in the bottom level's node
/// space), 28 its number in the descriptions, 30 the bytes 7f 7f 7f.
/// `None` for a transform with no inverse.
pub fn instance_record(desc: &[u32; 16], number: u32, bottom: &[u32; 16]) -> Option<[u32; 32]> {
    let t: [f32; 12] = std::array::from_fn(|k| f32::from_bits(desc[k]));
    let inverse = invert(&t)?;
    let space = Space::of(bottom);
    let mut w = [0u32; 32];
    w[..14].copy_from_slice(&desc[..14]);
    let statics = bottom[0] >> 16 & 2 != 0;
    w[14] = bottom[12] | u32::from(statics) << 30;
    w[15] = ((u64::from(desc[14]) | u64::from(desc[15]) << 32) >> 8) as u32;
    for r in 0..3 {
        w[16 + r] = ((inverse[4 * r + 3] - space.origin[r]) * space.scale[r]).to_bits();
        for c in 0..3 {
            w[19 + 3 * r + c] = (inverse[4 * r + c] * space.scale[r]).to_bits();
        }
    }
    w[28] = number;
    w[30] = 0x007f_7f7f;
    Some(w)
}

/// A bottom level's header as the walks need it: version 6, a bottom
/// level, unpacked, with elements.
pub fn walkable_bottom(h: &[u32; 16]) -> bool {
    h[0] & 0xffff == 6 && h[0] >> 31 == 1 && h[0] & PACKED == 0 && h[3] > 0
}

/// A top level built by the layer (`VICTORY_BVH_NATIVE=all`), in the
/// console's format, on the CPU: the header, an empty root, and a record for
/// each instance whose bottom level the CPU holds unpacked (the static ones
/// the layer unpacked; a dynamic one's is not built yet, and its record is
/// left empty: the host's structure leaves the instance out). The title
/// writes its instance descriptions after it records the build, so they are
/// read when `list` is submitted. The host's own structures are then built
/// from these records as from the console's (`memory.rs`).
#[cfg(windows)]
pub fn top_native(list: *const std::ffi::c_void, b: &Build) {
    let b = *b;
    match crate::list::host_list(list.cast()) {
        Some(host) => host.defer_host(move || write_top(&b)),
        None => write_top(&b),
    }
}

#[cfg(windows)]
fn write_top(b: &Build) {
    let count = b.count.min(TOP_MOST) as usize;
    let mut descs = vec![0u8; INSTANCE * count];
    let readable = count == 0 || crate::host::read_there(b.descs as *const u8, &mut descs) == descs.len();
    let mut out = vec![0u8; TOP_RECORDS as usize + 128 * count];
    let put = |out: &mut [u8], at: usize, v: u32| out[at..at + 4].copy_from_slice(&v.to_le_bytes());
    let (least, most) = ((-f32::MAX).to_bits(), f32::MAX.to_bits());
    let header = [
        6,
        (out.len() / 64 - 1) as u32,
        0,
        if readable { count as u32 } else { 0 },
        least,
        least,
        least,
        most,
        most,
        most,
        TOP_ROOT,
        0,
        TOP_ROOT >> 3 | 5,
        TOP_RECORDS,
        0x40,
        0x007f_7f7f,
    ];
    for (k, v) in header.into_iter().enumerate() {
        put(&mut out, 4 * k, v);
    }
    for k in 0..4 {
        put(&mut out, TOP_ROOT as usize + 4 * k, victory_shader::bvh::NONE);
    }
    let mut headers: std::collections::HashMap<u64, Option<[u32; 16]>> = std::collections::HashMap::new();
    let (mut with, mut without) = (0u64, 0u64);
    if readable {
        for k in 0..count {
            let d: [u32; 16] = std::array::from_fn(|n| u32::from_le_bytes(descs[64 * k + 4 * n..64 * k + 4 * n + 4].try_into().unwrap()));
            let bottom = u64::from(d[14]) | u64::from(d[15]) << 32;
            let h = *headers.entry(bottom).or_insert_with(|| {
                read::<64>(bottom)
                    .map(|h| std::array::from_fn(|n| u32::from_le_bytes(h[4 * n..4 * n + 4].try_into().unwrap())))
                    .filter(walkable_bottom)
            });
            let Some(record) = h.and_then(|h| instance_record(&d, k as u32, &h)) else {
                without += 1;
                continue;
            };
            with += 1;
            for (n, v) in record.into_iter().enumerate() {
                put(&mut out, TOP_RECORDS as usize + 128 * k + 4 * n, v);
            }
        }
    }
    crate::host::copy_mapped(b.dest as *mut u8, out.as_ptr(), out.len(), true);
    crate::memory::title_wrote(b.dest, out.len() as u64);
    let mut guard = UNPACKED.lock().unwrap_or_else(|e| e.into_inner());
    let u = guard.get_or_insert_with(Unpacked::default);
    u.tops += 1;
    u.instances[0] += with;
    u.instances[1] += without;
    u.unreadable_tops += u64::from(!readable);
    u.line();
}

/// The builds of a batched call the layer does not do itself, as the DLL's
/// descriptions (80 bytes each); the top levels among them done
/// ([`top_native`]).
#[cfg(windows)]
pub fn batched_native(list: *const std::ffi::c_void, count: u32, descs: *const u8) -> Vec<u8> {
    let mut rest = Vec::new();
    for n in 0..u64::from(count.min(4096)) {
        let Some(d) = read::<BATCHED>(descs as u64 + BATCHED as u64 * n) else { break };
        let b = Build::parse(d[..DESC].try_into().unwrap());
        let posts = u32::from_le_bytes(d[48..52].try_into().unwrap());
        let post = u64::from_le_bytes(d[56..64].try_into().unwrap());
        if b.kind == TOP {
            top_native(list, &b);
        } else if !once_native(&b, posts, post) && !card_native(list, &b, posts, post) {
            rest.extend_from_slice(&d);
        }
    }
    rest
}

/// What the builder said a build's result takes at most, by the build's
/// geometry count and triangles (the least said): what the title gave the
/// destination ([`prebuilt`], [`once_native`]).
#[cfg(windows)]
static SIZES: Mutex<Option<std::collections::HashMap<(u32, u64), u64>>> = Mutex::new(None);

/// The geometry descriptions of a bottom level's inputs (their count,
/// layout and address), read with what they name; `None` if one is not
/// triangles the layer decodes.
#[cfg(windows)]
fn geometries(count: u32, layout: u32, descs: u64, spans: Option<&mut Spans>) -> Option<Vec<(crate::records::Geometry, u32)>> {
    let mut out = Vec::with_capacity(count as usize);
    let mut spans = spans;
    for n in 0..u64::from(count.min(4096)) {
        let at = if layout == POINTERS { read::<8>(descs + 8 * n).map_or(0, u64::from_le_bytes) } else { descs + GEOMETRY as u64 * n };
        let (_, flags, g) = geometry(&read::<GEOMETRY>(at)?);
        let g = g?;
        if !matches!(g.vertex_format, crate::records::PACKED_VERTICES | crate::records::VERTICES_FLOAT3) {
            return None;
        }
        if let Some(spans) = spans.as_deref_mut() {
            for (start, len) in g.spans(spans) {
                spans.take(start, len);
            }
        }
        out.push((g, flags));
    }
    (out.len() == count as usize).then_some(out)
}

/// `GetRaytracingAccelerationStructurePrebuildInfo`, answered: `inputs` the
/// title's (24 bytes: type, flags, count, layout, descriptions), `info` the
/// builder's answer (its first qword the result's most bytes).
#[cfg(windows)]
pub fn prebuilt(inputs: *const u8, info: *const u8) {
    if native() != Native::All {
        return;
    }
    let (Some(i), Some(most)) = (read::<24>(inputs as u64), read::<8>(info as u64).map(u64::from_le_bytes)) else { return };
    let d = |at: usize| u32::from_le_bytes(i[at..at + 4].try_into().unwrap());
    let (kind, flags, count, layout) = (d(0), d(4), d(8), d(12));
    if kind == TOP || flags & UPDATE != 0 || most == 0 {
        return;
    }
    let descs = u64::from_le_bytes(i[16..24].try_into().unwrap());
    let Some(g) = geometries(count, layout, descs, None) else { return };
    let triangles: u64 = g.iter().map(|g| u64::from(g.0.triangles())).sum();
    let mut sizes = SIZES.lock().unwrap_or_else(|e| e.into_inner());
    let least = sizes.get_or_insert_with(Default::default).entry((count, triangles)).or_insert(most);
    *least = (*least).min(most);
}

/// A bottom level the title builds once, of geometry the CPU holds, built
/// by the layer ([`once`]) into the title's destination on the CPU, and its
/// post-build sizes (`posts` descriptions at `post`: a destination and a
/// type; the compacted size, 0, and the current size, 3) written: the
/// structure's bytes, which a compacting copy then copies whole
/// ([`copy_native`]). `false`: not one of those (an update, vertices only
/// the card holds, no room said): left to the builder.
#[cfg(windows)]
fn once_native(b: &Build, posts: u32, post: u64) -> bool {
    static GENERATION: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);
    let started = std::time::Instant::now();
    let mut why = None;
    let mut built = None;
    if b.flags & UPDATE != 0 {
        why = Some(0);
    } else {
        let mut spans = Spans::default();
        match geometries(b.count, b.layout, b.descs, Some(&mut spans)) {
            None => why = Some(1),
            Some(g) => {
                let triangles: u64 = g.iter().map(|g| u64::from(g.0.triangles())).sum();
                let room = SIZES.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|s| s.get(&(b.count, triangles)).copied());
                let generation = GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                match (room, once(&g, generation, &mut spans)) {
                    (_, Err(NotOnce::NotWritten)) => why = Some(2),
                    (_, Err(NotOnce::TooMany)) => why = Some(3),
                    (_, Err(_)) => why = Some(1),
                    (None, Ok(_)) => why = Some(4),
                    (Some(room), Ok(bytes)) if bytes.len() as u64 > room => why = Some(5),
                    (Some(_), Ok(bytes)) => built = Some((bytes, triangles)),
                }
            }
        }
    }
    let mut guard = UNPACKED.lock().unwrap_or_else(|e| e.into_inner());
    let u = guard.get_or_insert_with(Unpacked::default);
    match (&built, why) {
        (Some((bytes, triangles)), _) => {
            crate::host::copy_mapped(b.dest as *mut u8, bytes.as_ptr(), bytes.len(), true);
            crate::memory::title_wrote(b.dest, bytes.len() as u64);
            u.once[3] += sizes_written(posts, post, bytes.len() as u64);
            u.once[0] += 1;
            u.once[1] += (bytes.len() as u64 - u64::from(ONCE_ELEMENTS)) / 64;
            u.once[2] += triangles;
        }
        (None, Some(why)) => u.not_once[why] += 1,
        (None, None) => {}
    }
    u.once_micros += started.elapsed().as_micros() as u64;
    u.line();
    built.is_some()
}

/// A plan made for a destination ([`plan`]) in a buffer the card reads:
/// what it was made of (each geometry's indices, counts and place among the
/// vertices), so an update of the same mesh takes it again.
#[cfg(windows)]
struct Planned {
    of: Vec<[u64; 4]>,
    buffer: windows::Win32::Graphics::Direct3D12::ID3D12Resource,
    count: u32,
    most: u32,
    used: std::sync::atomic::AtomicU64,
}

#[cfg(windows)]
static PLANS: Mutex<Option<std::collections::HashMap<u64, std::sync::Arc<Planned>>>> = Mutex::new(None);

/// The post-build sizes a build asks for (`posts` descriptions at `post`:
/// the compacted size, 0, and the current size, 3) written as `size`; how
/// many.
#[cfg(windows)]
fn sizes_written(posts: u32, post: u64, size: u64) -> u64 {
    let mut written = 0;
    for n in 0..u64::from(posts.min(8)) {
        let Some(p) = read::<16>(post + 16 * n) else { break };
        let (to, kind) = (u64::from_le_bytes(p[0..8].try_into().unwrap()), u32::from_le_bytes(p[8..12].try_into().unwrap()));
        if to != 0 && matches!(kind, 0 | 3) {
            crate::host::copy_mapped(to as *mut u8, size.to_le_bytes().as_ptr(), 8, true);
            written += 1;
        }
    }
    written
}

/// A bottom level of a mesh whose vertices only the card holds (the title
/// skins it there; float vertices in one title buffer), built or updated by
/// the layer: the header and the empty root on the CPU, once (they do not
/// change with the vertices), and the element nodes by a pass on the card
/// at this place of the title's list, from a plan of which vertex goes
/// where ([`plan`], kept for the destination: an update has its build's
/// triangles). `false`: not one of those, left to the builder.
#[cfg(windows)]
fn card_native(list: *const std::ffi::c_void, b: &Build, posts: u32, post: u64) -> bool {
    let started = std::time::Instant::now();
    let done = card_build(list, b, posts, post);
    let mut guard = UNPACKED.lock().unwrap_or_else(|e| e.into_inner());
    let u = guard.get_or_insert_with(Unpacked::default);
    match done {
        Ok((elements, planned)) => {
            u.card[0] += 1;
            u.card[1] += u64::from(elements);
            u.card[2] += u64::from(planned);
        }
        Err(why) => u.not_card[why] += 1,
    }
    u.card_micros += started.elapsed().as_micros() as u64;
    u.line();
    done.is_ok()
}

/// [`card_native`]: the elements and whether a plan was made now, or why
/// not (a place in `Unpacked::not_card`).
#[cfg(windows)]
fn card_build(list: *const std::ffi::c_void, b: &Build, posts: u32, post: u64) -> Result<(u32, bool), usize> {
    let g = geometries(b.count, b.layout, b.descs, None).ok_or(0usize)?;
    if g.is_empty() || g.iter().any(|g| g.0.vertex_format != crate::records::VERTICES_FLOAT3) {
        return Err(0);
    }
    // The vertices: all in one title buffer's copy.
    let base = g.iter().map(|g| g.0.vertex_buffer).min().unwrap_or(0);
    let end = g.iter().map(|g| g.0.vertex_buffer + u64::from(g.0.stride) * u64::from(g.0.vertex_count)).max().unwrap_or(0);
    let (start, len) = crate::shadow::in_buffer(base, 1).ok_or(1usize)?;
    if end > start + len {
        return Err(1);
    }
    let vertices = crate::shadow::find(base, 1).ok_or(1usize)?;
    let triangles: u64 = g.iter().map(|g| u64::from(g.0.triangles())).sum();
    let room = SIZES.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|s| s.get(&(b.count, triangles)).copied()).ok_or(2usize)?;
    // The plan: the destination's, if it was made of this.
    let of: Vec<[u64; 4]> = g
        .iter()
        .map(|(g, flags)| {
            [
                g.index_buffer,
                u64::from(g.index_count) | u64::from(g.index_format) << 32 | u64::from(*flags) << 48 | u64::from(g.reversed) << 56,
                g.vertex_buffer - base,
                u64::from(g.vertex_count) | u64::from(g.stride) << 32,
            ]
        })
        .collect();
    let frame = crate::frame::current();
    let kept = PLANS.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|p| p.get(&b.dest).cloned()).filter(|p| p.of == of);
    let (planned, made) = match kept {
        Some(p) => (p, false),
        None => {
            let mut spans = Spans::default();
            for (g, _) in &g {
                let vertices = g.vertex_buffer..g.vertex_buffer + u64::from(g.stride) * u64::from(g.vertex_count);
                for (start, len) in g.spans(&mut spans) {
                    // (Not the vertices: the CPU does not hold them.)
                    if start >= vertices.end || start + len <= vertices.start {
                        spans.take(start, len);
                    }
                }
            }
            let (words, most) = plan(&g, base, &mut spans).map_err(|why| if why == NotOnce::TooMany { 3usize } else { 4 })?;
            let (buffer, data) = crate::memory::shared_buffer(4 * words.len() as u64).ok_or(5usize)?;
            // SAFETY: the new buffer's mapping, at least that long.
            unsafe { std::ptr::copy_nonoverlapping(words.as_ptr(), data as *mut u32, words.len()) };
            let p = std::sync::Arc::new(Planned {
                of,
                buffer,
                count: (words.len() / 8) as u32,
                most,
                used: std::sync::atomic::AtomicU64::new(frame),
            });
            let mut plans = PLANS.lock().unwrap_or_else(|e| e.into_inner());
            let plans = plans.get_or_insert_with(Default::default);
            if plans.len() >= 4096 {
                plans.retain(|_, p| p.used.load(std::sync::atomic::Ordering::Relaxed) + 600 > frame);
            }
            plans.insert(b.dest, p.clone());
            (p, true)
        }
    };
    planned.used.store(frame, std::sync::atomic::Ordering::Relaxed);
    let whole = own_structure(CARD, 0, [-f32::MAX; 3], [f32::MAX; 3], planned.count);
    if whole.len() as u64 > room {
        return Err(6);
    }
    // The header and the root, unless they are there: written again they
    // would go up again with their page, over the page's nodes.
    if read::<128>(b.dest).is_none_or(|there| there[..] != whole[..128]) {
        crate::host::copy_mapped(b.dest as *mut u8, whole.as_ptr(), 128, true);
    }
    crate::memory::title_wrote(b.dest, whole.len() as u64);
    sizes_written(posts, post, whole.len() as u64);
    let gather = crate::memory::Gather {
        to: b.dest + u64::from(ONCE_ELEMENTS),
        count: planned.count,
        plan: planned.buffer.clone(),
        vertices,
        base,
        most: planned.most,
    };
    crate::list::work_here(list.cast(), move |host| {
        crate::memory::card_elements(host, &gather);
    });
    Ok((planned.count, made))
}

/// `CopyRaytracingAccelerationStructure` of a structure the layer built
/// ([`once`]): its bytes copied on the CPU (it holds no addresses: a
/// compacting copy and a clone are the same). `false`: not one of those.
#[cfg(windows)]
pub fn copy_native(list: *const std::ffi::c_void, dest: u64, source: u64) -> bool {
    if native() != Native::All || dest == 0 || dest == source {
        return false;
    }
    let Some(h) = read::<64>(source) else { return false };
    let header: [u32; 16] = std::array::from_fn(|k| u32::from_le_bytes(h[4 * k..4 * k + 4].try_into().unwrap()));
    if made_on_card(&header) {
        // The header and the root on the CPU; the nodes, which only the
        // card holds, by a copy there at this place of the list.
        let Some(head) = read::<128>(source) else { return false };
        let nodes = 64 * u64::from(header[3]);
        crate::host::copy_mapped(dest as *mut u8, head.as_ptr(), head.len(), true);
        crate::memory::title_wrote(dest, 128 + nodes);
        crate::list::work_here(list.cast(), move |host| {
            crate::memory::card_copy(host, dest + u64::from(ONCE_ELEMENTS), source + u64::from(ONCE_ELEMENTS), nodes);
        });
        let mut guard = UNPACKED.lock().unwrap_or_else(|e| e.into_inner());
        let u = guard.get_or_insert_with(Unpacked::default);
        u.card[3] += 1;
        u.line();
        return true;
    }
    if !made_once(&header) {
        return false;
    }
    let mut bytes = vec![0u8; (header[1] as usize + 1) * 64];
    if crate::host::read_there(source as *const u8, &mut bytes) != bytes.len() {
        return false;
    }
    crate::host::copy_mapped(dest as *mut u8, bytes.as_ptr(), bytes.len(), true);
    crate::memory::title_wrote(dest, bytes.len() as u64);
    let mut guard = UNPACKED.lock().unwrap_or_else(|e| e.into_inner());
    let u = guard.get_or_insert_with(Unpacked::default);
    u.once[4] += 1;
    u.line();
    true
}

/// What the native unpacking did since the last line.
#[derive(Default)]
struct Unpacked {
    /// Top levels written, their instances with a record and without, top
    /// levels whose descriptions could not be read.
    tops: u64,
    instances: [u64; 2],
    unreadable_tops: u64,
    /// Bottom levels built once by the layer ([`once_native`]): structures,
    /// elements, triangles, post-build sizes written, copies.
    once: [u64; 5],
    /// Builds left to the builder: updates, geometry not decoded, vertices
    /// only the card holds, too many elements, no size known for the
    /// destination, more than its size.
    not_once: [u64; 6],
    once_micros: u64,
    /// Bottom levels whose nodes the card writes ([`card_native`]): builds
    /// and updates, their elements, plans made, copies.
    card: [u64; 4],
    /// Of the builds left by [`once_native`], those left to the builder:
    /// geometry not float triangles, vertices not in one title buffer, no
    /// size known for the destination, too many elements, indices not
    /// read, no buffer for the plan, more than the destination's size.
    not_card: [u64; 7],
    card_micros: u64,
    since: Option<std::time::Instant>,
    structures: u64,
    nodes: u64,
    triangles: u64,
    missing: u64,
    skipped: u64,
    micros: u64,
}

static UNPACKED: Mutex<Option<Unpacked>> = Mutex::new(None);

/// The `Rehydrate...X` calls done by the layer (`VICTORY_BVH_NATIVE=static`):
/// each packed structure unpacked in place on the CPU, as the DLL's four
/// programs would on the card: the header's packed mark cleared, the element
/// nodes written from the packed entries and the title's own geometry
/// (`crate::records`), the root an empty box (the title's own walk of a
/// structure the host has not built finds nothing, where the packed boxes
/// were). The box tree is not made: the host's structures answer the walks.
#[cfg(windows)]
pub fn rehydrate_native(count: u32, descs: *const u8) -> i32 {
    let started = std::time::Instant::now();
    let mut done = Unpacked::default();
    for n in 0..u64::from(count.min(65536)) {
        let Some(b) = read::<DESC>(descs as u64 + DESC as u64 * n) else { break };
        unpack(&Build::parse(&b), &mut done);
    }
    let mut guard = UNPACKED.lock().unwrap_or_else(|e| e.into_inner());
    let u = guard.get_or_insert_with(Unpacked::default);
    u.structures += done.structures;
    u.nodes += done.nodes;
    u.triangles += done.triangles;
    u.missing += done.missing;
    u.skipped += done.skipped;
    u.micros += started.elapsed().as_micros() as u64;
    u.line();
    0
}

impl Unpacked {
    /// Every ten seconds, what was done.
    fn line(&mut self) {
        let now = std::time::Instant::now();
        if self.since.is_some_and(|s| now.duration_since(s).as_secs_f64() < 10.0) {
            return;
        }
        tracing::info!(
            structures = self.structures,
            nodes = self.nodes,
            triangles = self.triangles,
            unreadable = self.missing,
            skipped = self.skipped,
            ms = self.micros / 1000,
            tops = self.tops,
            instances = self.instances[0],
            instances_without_a_bottom_level = self.instances[1],
            unreadable_tops = self.unreadable_tops,
            built_once = self.once[0],
            once_elements = self.once[1],
            once_triangles = self.once[2],
            once_sizes_written = self.once[3],
            once_copies = self.once[4],
            once_ms = self.once_micros / 1000,
            left_updates = self.not_once[0],
            left_not_decoded = self.not_once[1],
            left_card_vertices = self.not_once[2],
            left_too_many = self.not_once[3],
            left_no_size = self.not_once[4],
            left_too_big = self.not_once[5],
            card_builds = self.card[0],
            card_elements = self.card[1],
            card_plans = self.card[2],
            card_copies = self.card[3],
            card_ms = self.card_micros / 1000,
            card_left_not_float = self.not_card[0],
            card_left_not_one_buffer = self.not_card[1],
            card_left_no_size = self.not_card[2],
            card_left_too_many = self.not_card[3],
            card_left_indices = self.not_card[4],
            card_left_no_plan_buffer = self.not_card[5],
            card_left_too_big = self.not_card[6],
            "VICTORY_BVH_NATIVE: since the last line, static structures unpacked by the layer, top levels written by it, bottom levels built once by it (the left_ counts: not those), bottom levels whose nodes the card writes for it (card_left_: not those either, left to the builder)"
        );
        *self = Unpacked { since: Some(now), ..Unpacked::default() };
    }
}

#[cfg(windows)]
fn unpack(b: &Build, done: &mut Unpacked) {
    let Some(h) = read::<64>(b.dest) else {
        done.skipped += 1;
        return;
    };
    let header: [u32; 16] = std::array::from_fn(|k| u32::from_le_bytes(h[4 * k..4 * k + 4].try_into().unwrap()));
    if header[0] & 0xffff != 6 || header[0] & PACKED == 0 || b.kind == TOP {
        done.skipped += 1;
        return;
    }
    let (count, first) = (header[3] as usize, u64::from(header[13]));
    let size = (u64::from(header[1]) + 1) * 64;
    if count == 0 || first + 64 * count as u64 > size || size > 256 << 20 {
        done.skipped += 1;
        return;
    }
    // The geometry (the title's descriptions, on the CPU) and what it names.
    let mut spans = Spans::default();
    let mut geometries = Vec::with_capacity(b.count as usize);
    for n in 0..u64::from(b.count.min(4096)) {
        let at = if b.layout == POINTERS {
            read::<8>(b.descs + 8 * n).map_or(0, u64::from_le_bytes)
        } else {
            b.descs + GEOMETRY as u64 * n
        };
        let g = read::<GEOMETRY>(at).and_then(|g| geometry(&g).2);
        if let Some(g) = g {
            for (start, len) in g.spans(&mut spans) {
                spans.take(start, len);
            }
        }
        geometries.push(g);
    }
    // The entries of every group first: the nodes are written over them.
    let mut entries = Vec::with_capacity(count);
    for g in 0..count.div_ceil(crate::records::GROUP as usize) {
        let n = (count - g * crate::records::GROUP as usize).min(crate::records::GROUP as usize);
        let mut block = vec![0u8; 11 * n];
        let at = b.dest + first + ((g as u64) << 17);
        if crate::host::read_there(at as *const u8, &mut block) != block.len() {
            done.skipped += 1;
            return;
        }
        entries.extend(crate::records::entries(&block, n).unwrap_or_default());
    }
    let space = Space::of(&header);
    let mut nodes = vec![0u8; 64 * count];
    for (i, e) in entries.iter().enumerate() {
        let triangle = |p: u32, spans: &mut Spans| -> Option<[[f32; 3]; 3]> {
            let g = geometries.get(e.geometry as usize).copied().flatten()?;
            g.triangle(p, spans).ok().map(|t| t.map(|v| space.place(v)))
        };
        let (a, second) = (triangle(e.primitive, &mut spans), e.second.map(|p| triangle(p, &mut spans)));
        let nan = [[f32::NAN; 3]; 3];
        if a.is_none() || second.is_some_and(|s| s.is_none()) {
            done.missing += 1;
        }
        let w = element(e, a.unwrap_or(nan), second.map(|s| s.unwrap_or(nan)));
        for (k, d) in w.iter().enumerate() {
            nodes[64 * i + 4 * k..64 * i + 4 * k + 4].copy_from_slice(&d.to_le_bytes());
        }
        done.triangles += 1 + u64::from(e.second.is_some());
    }
    // The root: an empty box (children none), where the packed boxes were.
    let root = b.dest + ((u64::from(header[12]) >> 3) << 6);
    let mut empty = [0u8; 128];
    for k in 0..4 {
        empty[4 * k..4 * k + 4].copy_from_slice(&victory_shader::bvh::NONE.to_le_bytes());
    }
    let header0 = (header[0] & !PACKED).to_le_bytes();
    crate::host::copy_mapped((b.dest + first) as *mut u8, nodes.as_ptr(), nodes.len(), true);
    if root + 128 <= b.dest + size && !(b.dest + first..b.dest + first + nodes.len() as u64).contains(&root) {
        crate::host::copy_mapped(root as *mut u8, empty.as_ptr(), empty.len(), true);
    }
    crate::host::copy_mapped(b.dest as *mut u8, header0.as_ptr(), 4, true);
    crate::memory::title_wrote(b.dest, size);
    done.structures += 1;
    done.nodes += count as u64;
}

/// `VICTORY_BVH_HOOK_DUMP=<frames>` with `VICTORY_MEMORY_DUMP=<folder>`
/// (diagnostics): that many frames after the first build, the last build of
/// each destination is written to the dump folder (`builds.txt`: each top
/// level's instance descriptions, each bottom level's geometry) and the
/// arenas dumped (`go`), so the console's own structures can be compared
/// with what they were built from.
#[cfg(feature = "diagnostics")]
mod last {
    use super::*;
    use std::collections::HashMap;
    use std::fmt::Write as _;

    struct Last {
        first: Option<u64>,
        done: bool,
        builds: HashMap<u64, (u64, Build, String)>,
    }

    static LAST: Mutex<Option<Last>> = Mutex::new(None);

    fn due() -> Option<u64> {
        static DUE: std::sync::OnceLock<Option<u64>> = std::sync::OnceLock::new();
        *DUE.get_or_init(|| std::env::var("VICTORY_BVH_HOOK_DUMP").ok().and_then(|v| v.parse().ok()))
    }

    pub(super) fn note(b: &Build, list: *const std::ffi::c_void) {
        if due().is_none() {
            return;
        }
        // A top level's instances as they are when its list is submitted:
        // the title writes them after it records the build (those read at
        // the call matched none of the console's instance records).
        #[cfg(windows)]
        if b.kind == TOP
            && let Some(host) = crate::list::host_list(list.cast())
        {
            let b = *b;
            host.defer_host(move || note_now(&b));
            return;
        }
        note_now(b);
    }

    fn note_now(b: &Build) {
        let frame = crate::frame::current();
        let mut guard = LAST.lock().unwrap_or_else(|e| e.into_inner());
        let l = guard.get_or_insert_with(|| Last { first: None, done: false, builds: HashMap::new() });
        if l.done {
            return;
        }
        l.first.get_or_insert(frame);
        let mut text = String::new();
        if b.kind == TOP {
            for k in 0..u64::from(b.count.min(8192)) {
                let Some(i) = read::<INSTANCE>(b.descs + INSTANCE as u64 * k) else { break };
                let words: Vec<String> = i.chunks_exact(4).map(|c| format!("{:08x}", u32::from_le_bytes(c.try_into().unwrap()))).collect();
                let _ = writeln!(text, "  instance {k} {}", words.join(" "));
            }
        } else {
            for n in 0..u64::from(b.count.min(4096)) {
                let at = if b.layout == POINTERS { read::<8>(b.descs + 8 * n).map_or(0, u64::from_le_bytes) } else { b.descs + GEOMETRY as u64 * n };
                let Some(g) = read::<GEOMETRY>(at) else { break };
                let _ = writeln!(text, "  geometry {n} {:?}", geometry(&g));
            }
        }
        l.builds.insert(b.dest, (frame, *b, text));
    }

    pub(super) fn dump_when_due(frame: u64) {
        let (Some(due), Some(folder)) = (due(), std::env::var_os("VICTORY_MEMORY_DUMP")) else { return };
        let mut guard = LAST.lock().unwrap_or_else(|e| e.into_inner());
        let Some(l) = guard.as_mut() else { return };
        if l.done || l.first.is_none_or(|f| frame < f + due) {
            return;
        }
        l.done = true;
        let folder = std::path::PathBuf::from(folder);
        let mut out = format!("frame {frame}\n");
        let mut all: Vec<_> = l.builds.iter().collect();
        all.sort_by_key(|(d, _)| **d);
        for (dest, (at, b, text)) in all {
            let _ = writeln!(out, "build {dest:#x} frame {at} {b:x?}");
            out += text;
        }
        let _ = std::fs::create_dir_all(&folder);
        let _ = std::fs::write(folder.join("builds.txt"), out);
        let _ = std::fs::write(folder.join("go"), "go");
        tracing::info!(builds = l.builds.len(), "VICTORY_BVH_HOOK_DUMP: the last builds written out, the arenas asked for");
    }
}

/// Said once: the wrappers could not find the DLL's own export.
pub fn missing(name: &str) {
    static SAID: AtomicBool = AtomicBool::new(false);
    if !SAID.swap(true, Ordering::Relaxed) {
        tracing::error!(name, "VICTORY_BVH_HOOK: the structure builder's own export is not known; the call is dropped");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neighbours_pair_on_their_shared_edge() {
        // (a1, a2) is b's (b1, b0) backwards-run edge: low 9, high 4.
        assert_eq!(pair([0, 1, 2], [2, 1, 3]), Some(0x49));
        // (a0, a1) is b's (b1, b0): low 2, high 4.
        assert_eq!(pair([0, 1, 2], [1, 0, 3]), Some(0x42));
        // The same direction on both (the windings disagree): no pair.
        assert_eq!(pair([0, 1, 2], [1, 2, 3]), None);
        // No shared edge; and the same triangle twice.
        assert_eq!(pair([0, 1, 2], [3, 4, 5]), None);
        assert_eq!(pair([0, 1, 2], [2, 1, 0]), None);
    }

    /// A quad of two triangles and a third apart, as float vertices with
    /// 16-bit indices.
    fn mesh() -> (crate::records::Geometry, impl FnMut(u64, &mut [u8]) -> bool, Vec<[f32; 3]>) {
        const VB: u64 = 0x1_0000_1000;
        const IB: u64 = 0x1_0000_2000;
        let points: Vec<[f32; 3]> =
            vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [1.0, 1.0, 0.5], [5.0, 5.0, 5.0], [6.0, 5.0, 5.0], [5.0, 6.0, 5.0]];
        let indices: [u16; 9] = [0, 1, 2, 2, 1, 3, 4, 5, 6];
        let vb: Vec<u8> = points.iter().flatten().flat_map(|f| f.to_bits().to_le_bytes()).collect();
        let ib: Vec<u8> = indices.iter().flat_map(|i| i.to_le_bytes()).collect();
        let g = crate::records::Geometry {
            transform: 0,
            index_format: crate::records::INDICES_16,
            vertex_format: crate::records::VERTICES_FLOAT3,
            index_count: 9,
            vertex_count: 7,
            index_buffer: IB,
            vertex_buffer: VB,
            stride: 12,
            reversed: false,
        };
        let memory = move |address: u64, bytes: &mut [u8]| {
            for (base, data) in [(VB, &vb), (IB, &ib)] {
                if address >= base && address + bytes.len() as u64 <= base + data.len() as u64 {
                    let at = (address - base) as usize;
                    bytes.copy_from_slice(&data[at..at + bytes.len()]);
                    return true;
                }
            }
            false
        };
        (g, memory, points)
    }

    #[test]
    fn a_mesh_built_once_is_a_walkable_bottom_level_of_its_triangles() {
        let (g, mut memory, points) = mesh();
        let bytes = once(&[(g, 1)], 7, &mut memory).unwrap();
        let word = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
        let header: [u32; 16] = std::array::from_fn(|k| word(4 * k));
        assert!(walkable_bottom(&header) && made_once(&header));
        assert_eq!((header[2], header[3], header[12], header[13]), (7, 2, 12, 128));
        assert_eq!(bytes.len(), 128 + 2 * 64);
        assert_eq!((header[1] as usize + 1) * 64, bytes.len());
        // Its space is the geometry's own, its bounds the points'.
        let space = Space::of(&header);
        assert_eq!((space.scale, space.origin), ([1.0; 3], [0.0; 3]));
        assert_eq!([4, 5, 6, 7, 8, 9].map(|k| f32::from_bits(header[k])), [0.0, 0.0, 0.0, 6.0, 6.0, 5.0]);
        // The root: a box with no child.
        assert!((0..4).all(|k| word(64 + 4 * k) == victory_shader::bvh::NONE));
        // The pair, then the single one, as the node test reads them.
        let node = |n: usize| -> [u32; 28] { std::array::from_fn(|k| if k < 16 { word(128 + 64 * n + 4 * k) } else { 0 }) };
        let (first, second) = (node(0), node(1));
        assert_eq!((first[12], first[13], first[14]), (0x81, 0, 1));
        assert_eq!(victory_shader::bvh::triangle_vertices(&first, 0), [points[0], points[1], points[2]]);
        assert_eq!(victory_shader::bvh::triangle_vertices(&first, 1), [points[2], points[1], points[3]]);
        assert_eq!((second[12], second[13], second[14], second[15]), (0x01, 2, 0xffff_ffff, 9));
        assert_eq!(victory_shader::bvh::triangle_vertices(&second, 0), [points[4], points[5], points[6]]);
        assert!(f32::from_bits(second[9]).is_nan());
    }

    #[test]
    fn a_plan_names_each_nodes_vertices_for_the_card() {
        let (g, mut memory, _) = mesh();
        // (The vertex memory starts 24 bytes before this mesh's.)
        let (words, most) = plan(&[(g, 1)], g.vertex_buffer - 24, &mut memory).unwrap();
        assert_eq!(words.len(), 16);
        assert_eq!(words[..8], [24, 36, 48, 60, 0x81, 0, 1, 0x409]);
        assert_eq!(words[8..], [72, 84, 96, u32::MAX, 0x01, 2, 0xffff_ffff, 9]);
        assert_eq!(most, 96);
        // The same elements as the CPU's build of the mesh.
        let bytes = once(&[(g, 1)], 1, &mut memory).unwrap();
        for e in 0..2 {
            for k in 0..4 {
                let at = 128 + 64 * e + 48 + 4 * k;
                assert_eq!(u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()), words[8 * e + 4 + k]);
            }
        }
        // An index past the vertices is not planned.
        let mut short = g;
        short.vertex_count = 6;
        assert_eq!(plan(&[(short, 1)], g.vertex_buffer, &mut memory), Err(NotOnce::Unreadable));
        let header: [u32; 16] = {
            let h = own_structure(CARD, 0, [-f32::MAX; 3], [f32::MAX; 3], 2);
            std::array::from_fn(|k| u32::from_le_bytes(h[4 * k..4 * k + 4].try_into().unwrap()))
        };
        assert!(made_on_card(&header) && walkable_bottom(&header) && !made_once(&header));
    }

    #[test]
    fn vertices_the_cpu_does_not_hold_are_not_built_from() {
        let (g, _, _) = mesh();
        // (The card writes a skinned mesh's vertices: the CPU reads zeros.)
        let mut zeros = |address: u64, bytes: &mut [u8]| {
            if address >= 0x1_0000_2000 {
                let indices: [u16; 9] = [0, 1, 2, 2, 1, 3, 4, 5, 6];
                let ib: Vec<u8> = indices.iter().flat_map(|i| i.to_le_bytes()).collect();
                let at = (address - 0x1_0000_2000) as usize;
                bytes.copy_from_slice(&ib[at..at + bytes.len()]);
            } else {
                bytes.fill(0);
            }
            true
        };
        assert_eq!(once(&[(g, 1)], 1, &mut zeros), Err(NotOnce::NotWritten));
        let mut nothing = |_: u64, _: &mut [u8]| false;
        assert_eq!(once(&[(g, 1)], 1, &mut nothing), Err(NotOnce::Unreadable));
        assert_eq!(once(&[], 1, &mut nothing), Err(NotOnce::Empty));
    }

    #[test]
    fn a_build_description_reads_in_d3d12s_layout() {
        let mut b = [0u8; DESC];
        b[0..8].copy_from_slice(&0x9_6362_4000u64.to_le_bytes());
        b[8..12].copy_from_slice(&1u32.to_le_bytes());
        b[12..16].copy_from_slice(&0x24u32.to_le_bytes());
        b[16..20].copy_from_slice(&3u32.to_le_bytes());
        b[24..32].copy_from_slice(&0x7ff0_1234_5678u64.to_le_bytes());
        b[40..48].copy_from_slice(&0xa_e6c0_e9e0u64.to_le_bytes());
        let p = Build::parse(&b);
        assert_eq!((p.dest, p.kind, p.flags, p.count, p.layout), (0x9_6362_4000, 1, 0x24, 3, 0));
        assert_eq!((p.descs, p.source, p.scratch), (0x7ff0_1234_5678, 0, 0xa_e6c0_e9e0));
    }

    #[test]
    fn an_instance_reads_in_d3d12s_layout() {
        let mut b = [0u8; INSTANCE];
        b[0..4].copy_from_slice(&1.0f32.to_bits().to_le_bytes());
        b[48..52].copy_from_slice(&(0xff00_0007u32).to_le_bytes());
        b[52..56].copy_from_slice(&(0x0400_0002u32).to_le_bytes());
        b[56..64].copy_from_slice(&0x9_6295_fc00u64.to_le_bytes());
        let i = Instance::parse(&b);
        assert_eq!((i.transform[0], i.id, i.mask, i.hit_group, i.flags, i.bottom), (1.0, 7, 0xff, 2, 4, 0x9_6295_fc00));
    }

    #[test]
    fn an_instance_record_is_the_consoles() {
        let f = f32::to_bits;
        // A record of dump 5 (run 20261127-184716): a dynamic bottom level
        // (no node space), its transform and what the console wrote after it.
        let t = [13.8575f32, -6.6865, 0.0, -35.1808, 6.6865, 13.8575, 0.0, -189.2898, 0.0, 0.0, 15.3864, 8.3946];
        let mut desc = [0u32; 16];
        for (d, v) in desc.iter_mut().zip(t) {
            *d = f(v);
        }
        [desc[12], desc[13], desc[14], desc[15]] = [0x4005_5c00, 0x0100_0005, 0x68a9_1000, 0x9];
        let mut bottom = [0u32; 16];
        [bottom[0], bottom[3], bottom[12], bottom[15]] = [0x8000_0006, 5, 0x301, 0x007f_7f7f];
        let w = instance_record(&desc, 1380, &bottom).unwrap();
        assert_eq!(&w[..12], &desc[..12]);
        assert_eq!([w[12], w[13], w[14], w[15]], [0x4005_5c00, 0x0100_0005, 0x301, 0x0968_a910]);
        let shown = [7.4056f32, 10.0864, -0.5456, 0.0585, 0.0282, 0.0, -0.0282, 0.0585, 0.0, 0.0, 0.0, 0.065];
        for (k, v) in shown.iter().enumerate() {
            assert!((f32::from_bits(w[16 + k]) - v).abs() < 2e-4, "{k}: {}", f32::from_bits(w[16 + k]));
        }
        assert_eq!([w[28], w[30]], [1380, 0x007f_7f7f]);
        // A static one: its root pointer marked, the transform in its node space.
        [bottom[0], bottom[12], bottom[15]] = [0x8002_0006, 0x134, 0x808e_8f8e];
        let w = instance_record(&desc, 0, &bottom).unwrap();
        assert_eq!(w[14], 0x4000_0134);
        assert!((f32::from_bits(w[16]) - 7.4056 * 32768.0).abs() < 8.0);
        assert!((f32::from_bits(w[19]) - 0.0585 * 32768.0).abs() < 2.0);
        assert!((f32::from_bits(w[27]) - 0.065 * 32768.0).abs() < 2.0);
        assert!(walkable_bottom(&bottom));
        bottom[0] |= PACKED;
        assert!(!walkable_bottom(&bottom));
        // No inverse: no record.
        assert!(instance_record(&[0u32; 16], 0, &bottom).is_none());
    }

    #[test]
    fn a_header_gives_the_element_nodes_space() {
        let f = f32::to_bits;
        let mut h = [0u32; 16];
        // Bounds (-0.2656, 0.0005, -0.6265) to (0.7832, 0.6626, 0.5981),
        // exponents 2^16 2^17 2^16, centred: y passes 65,536 scaled.
        [h[4], h[5], h[6], h[7], h[8], h[9]] = [f(-0.2656), f(0.0005), f(-0.6265), f(0.7832), f(0.6626), f(0.5981)];
        h[15] = 0xc08f_908f;
        let s = Space::of(&h);
        assert_eq!(s.scale, [65536.0, 131072.0, 65536.0]);
        assert_eq!(s.origin[0], 0.0);
        assert!((s.origin[1] - 0.33155).abs() < 1e-4);
        assert_eq!(s.origin[2], 0.0);
        h[15] = 0x808f_908f;
        assert_eq!(Space::of(&h).origin, [0.0; 3]);
        assert_eq!(Space::of(&h).place([1.0, 0.5, -1.0]), [65536.0, 65536.0, -65536.0]);
    }

    #[test]
    fn an_element_node_holds_the_pair_as_the_program_writes_it() {
        use crate::records::Entry;
        let (a0, a1, a2, b1) = ([1.0, 0.0, 0.0], [2.0, 0.0, 0.0], [3.0, 0.0, 0.0], [4.0, 0.0, 0.0]);
        // Flags 148 (dump: node a1, a2/b0, a0/b2, b1; id bytes 0x904): the
        // pair shares a2 = b0 and a0 = b2.
        let e = Entry { geometry: 3, kind: 0x81, primitive: 7, second: Some(8), flags: 148 };
        let w = element(&e, [a0, a1, a2], Some([a2, b1, a0]));
        let v = |k: usize| [f32::from_bits(w[3 * k]), f32::from_bits(w[3 * k + 1]), f32::from_bits(w[3 * k + 2])];
        assert_eq!([v(0), v(1), v(2), v(3)], [a1, a2, a0, b1]);
        assert_eq!(&w[12..], &[3 << 8 | 0x81, 7, 8, 0x904]);
        // And the walk's own reading of it gives the triangles back.
        let mut words = [0u32; 28];
        words[..16].copy_from_slice(&w);
        let t0 = victory_shader::bvh::triangle_vertices(&words, 0);
        let t1 = victory_shader::bvh::triangle_vertices(&words, 1);
        assert_eq!(t0, [a0, a1, a2]);
        assert_eq!(t1, [a2, b1, a0]);
        // A single triangle (flags 9): vertices 0 1 2, vertex 3 NaN, no second.
        let e = Entry { geometry: 0, kind: 1, primitive: 5, second: None, flags: 9 };
        let w = element(&e, [a0, a1, a2], None);
        assert_eq!(&w[..9], &[a0, a1, a2].concat().iter().map(|x: &f32| x.to_bits()).collect::<Vec<_>>()[..]);
        assert!(f32::from_bits(w[9]).is_nan());
        assert_eq!(&w[12..], &[1, 5, 0xffff_ffff, 0x9]);
    }

    #[test]
    fn a_geometry_description_carries_the_records_layout() {
        let mut b = [0u8; GEOMETRY];
        b[4..8].copy_from_slice(&1u32.to_le_bytes());
        b[16..20].copy_from_slice(&crate::records::PACKED_INDICES.to_le_bytes());
        b[20..24].copy_from_slice(&crate::records::PACKED_VERTICES.to_le_bytes());
        b[24..28].copy_from_slice(&300u32.to_le_bytes());
        b[40..48].copy_from_slice(&0x6_0000_0100u64.to_le_bytes());
        b[48..52].copy_from_slice(&8u32.to_le_bytes());
        let (kind, flags, t) = geometry(&b);
        let t = t.unwrap();
        assert_eq!((kind, flags, t.index_count, t.vertex_buffer, t.stride), (0, 1, 300, 0x6_0000_0100, 8));
    }
}
