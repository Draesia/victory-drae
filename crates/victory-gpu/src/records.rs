//! The triangle geometry the title hands the console's structure builder
//! (`XboxBvhBuilder.dll`), decoded on the host: the first part of a native
//! builder (`C:\Users\Corgan\victory-mcp\DXR-PLAN.md`, plan #2).
//!
//! A static bottom level ships packed. The builder's LeafRehydration program
//! (5e68767498c57eb6) unpacks its leaves: each packed entry names a geometry
//! and one or two of its triangles, and the program reads those triangles
//! from the title's own index and vertex buffers through a list of 48-byte
//! records, one a geometry. The records are the title's geometry
//! descriptions in D3D12's own layout (`D3D12_RAYTRACING_GEOMETRY_TRIANGLES_DESC`):
//!
//! | byte | what |
//! |---|---|
//! | 0 | transform (3x4 floats, row-major, as D3D12's `Transform3x4`), an address or 0 |
//! | 8 | index format |
//! | 12 | vertex format |
//! | 16 | index count |
//! | 20 | vertex count |
//! | 24 | index buffer |
//! | 32 | vertex buffer |
//! | 40 | vertex stride (bits 0-14); bit 15: the packed index stream's winding is reversed |
//!
//! Besides D3D12's formats the records use two of the console's, decoded
//! here from the program's listing (capture run 20261127-164943: every one
//! of 626 records of 300 dispatches is index format 53, vertex format 16):
//! - index format [`PACKED_INDICES`]: a header of three dwords `h0 h1 h2` at
//!   the index buffer; triangle `p`'s dword `D` at `+t0 + 4p`, `t0 = (h0 &
//!   0x1ffff) * 16 + 12`; its group `g = D >> 21` has two dwords `Q0 Q1` at
//!   `+g * 16 + 12`, a base `((Q0 << 2) & 0x10000) | (Q1 & 0xffff)`; the
//!   three indices are 16-bit words at `+t0 + h1 * 4 + 2 * (base + f)`, `f`
//!   the 7-bit fields of `D` at bits 0, 7 and 14, reversed with bit 15 of
//!   the stride.
//! - vertex format [`PACKED_VERTICES`]: 8 bytes a vertex, `a = d0 >> 8` (24
//!   bits), `b = (d0 & 0xff) | (d1 & 0xffff) << 8` (24 bits), `c = d1 >> 16`
//!   (16 bits), each times a scale plus an offset kept before the vertices:
//!   the offsets and an axis word at the vertex buffer -32, the scales at
//!   -16. The axis word orders the three: 0 `(c, b, a)`, 1 `(b, c, a)`,
//!   anything else `(a, b, c)`.
//!
//! What the program makes of an entry (an element node, 64 bytes, in the
//! structure's own space: [`Leaf::node_space`]) is what the title's trace
//! programs read after a hit; [`Leaf`] holds the dispatch's constants that
//! place the triangles there.

/// Bytes of a record.
pub const RECORD: usize = 48;
/// DXGI_FORMAT_UNKNOWN as an index format: no index buffer, triangle `p`
/// is vertices `3p`, `3p + 1`, `3p + 2`.
pub const NO_INDICES: u32 = 0;
/// DXGI_FORMAT_R32_UINT.
pub const INDICES_32: u32 = 42;
/// DXGI_FORMAT_R16_UINT.
pub const INDICES_16: u32 = 57;
/// The console's packed index stream (see the module).
pub const PACKED_INDICES: u32 = 53;
/// DXGI_FORMAT_R32G32B32_FLOAT.
pub const VERTICES_FLOAT3: u32 = 6;
/// The console's 24/24/16-bit vertices (see the module).
pub const PACKED_VERTICES: u32 = 16;

/// One record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Geometry {
    pub transform: u64,
    pub index_format: u32,
    pub vertex_format: u32,
    pub index_count: u32,
    pub vertex_count: u32,
    pub index_buffer: u64,
    pub vertex_buffer: u64,
    pub stride: u32,
    /// Bit 15 of the stride: the packed index stream's triangles reversed.
    pub reversed: bool,
}

/// Why a triangle could not be decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Why {
    /// Memory the decoding needed could not be read.
    Unreadable(u64),
    /// A format this decoder does not know.
    IndexFormat(u32),
    VertexFormat(u32),
    /// A triangle or vertex past the record's counts.
    OutOfRange,
}

/// Reads `bytes.len()` bytes of title memory at an address into `bytes`;
/// false if they are not all there.
pub trait Read {
    fn read(&mut self, address: u64, bytes: &mut [u8]) -> bool;
}

impl<F: FnMut(u64, &mut [u8]) -> bool> Read for F {
    fn read(&mut self, address: u64, bytes: &mut [u8]) -> bool {
        self(address, bytes)
    }
}

fn dwords<const N: usize>(memory: &mut impl Read, address: u64) -> Result<[u32; N], Why> {
    let mut bytes = vec![0u8; 4 * N];
    if !memory.read(address, &mut bytes) {
        return Err(Why::Unreadable(address));
    }
    Ok(std::array::from_fn(|i| u32::from_le_bytes(bytes[4 * i..4 * i + 4].try_into().unwrap())))
}

fn half(memory: &mut impl Read, address: u64) -> Result<u32, Why> {
    let mut b = [0u8; 2];
    if !memory.read(address, &mut b) {
        return Err(Why::Unreadable(address));
    }
    Ok(u32::from(u16::from_le_bytes(b)))
}

impl Geometry {
    /// A record; `None` for an empty one (no formats, no buffers: past the
    /// list's end).
    pub fn parse(b: &[u8; RECORD]) -> Option<Geometry> {
        let q = |at: usize| u64::from_le_bytes(b[at..at + 8].try_into().unwrap());
        let d = |at: usize| u32::from_le_bytes(b[at..at + 4].try_into().unwrap());
        let g = Geometry {
            transform: q(0),
            index_format: d(8),
            vertex_format: d(12),
            index_count: d(16),
            vertex_count: d(20),
            index_buffer: q(24),
            vertex_buffer: q(32),
            stride: d(40) & 0x7fff,
            reversed: d(40) >> 15 & 1 == 1,
        };
        (g.vertex_format != 0 || g.vertex_buffer != 0).then_some(g)
    }

    /// Its triangles.
    pub fn triangles(&self) -> u32 {
        if self.index_format == NO_INDICES {
            self.vertex_count / 3
        } else {
            self.index_count / 3
        }
    }

    /// Triangle `p`'s three vertex numbers, in the order the program takes
    /// them.
    pub fn indices(&self, p: u32, memory: &mut impl Read) -> Result<[u32; 3], Why> {
        if p >= self.triangles() {
            return Err(Why::OutOfRange);
        }
        let ib = self.index_buffer;
        let p64 = u64::from(p);
        let ix = match self.index_format {
            NO_INDICES => [3 * p, 3 * p + 1, 3 * p + 2],
            INDICES_32 => dwords::<3>(memory, ib + 12 * p64)?,
            INDICES_16 => {
                let at = ib + 6 * p64;
                [half(memory, at)?, half(memory, at + 2)?, half(memory, at + 4)?]
            }
            PACKED_INDICES => {
                let [h0, h1, _] = dwords::<3>(memory, ib)?;
                let t0 = u64::from(h0 & 0x1ffff) * 16 + 12;
                let [d] = dwords::<1>(memory, ib + t0 + 4 * p64)?;
                let g = u64::from(d >> 21 & 0x7ff);
                let [q0, q1] = dwords::<2>(memory, ib + g * 16 + 12)?;
                let base = u64::from((q0 << 2 & 0x10000) | (q1 & 0xffff));
                let pool = ib + t0 + u64::from(h1) * 4 + 2 * base;
                let mut ix = [0u32; 3];
                for (k, shift) in [0, 7, 14].into_iter().enumerate() {
                    ix[k] = half(memory, pool + 2 * u64::from(d >> shift & 0x7f))?;
                }
                if self.reversed {
                    ix.reverse();
                }
                ix
            }
            other => return Err(Why::IndexFormat(other)),
        };
        Ok(ix)
    }

    /// Vertex `i` in the geometry's own space (before its transform).
    pub fn position(&self, i: u32, memory: &mut impl Read) -> Result<[f32; 3], Why> {
        if i >= self.vertex_count {
            return Err(Why::OutOfRange);
        }
        let at = self.vertex_buffer + u64::from(self.stride) * u64::from(i);
        match self.vertex_format {
            VERTICES_FLOAT3 => Ok(dwords::<3>(memory, at)?.map(f32::from_bits)),
            PACKED_VERTICES => {
                let [d0, d1] = dwords::<2>(memory, at)?;
                let head = dwords::<8>(memory, self.vertex_buffer.wrapping_sub(32))?;
                let (offset, axes, scale) = ([head[0], head[1], head[2]].map(f32::from_bits), head[3], [head[4], head[5], head[6]].map(f32::from_bits));
                let stored = [d0 >> 8, (d0 & 0xff) | (d1 & 0xffff) << 8, d1 >> 16];
                let [a, b, c]: [f32; 3] = std::array::from_fn(|k| (stored[k] as f32).mul_add(scale[k], offset[k]));
                Ok(match axes {
                    0 => [c, b, a],
                    1 => [b, c, a],
                    _ => [a, b, c],
                })
            }
            other => Err(Why::VertexFormat(other)),
        }
    }

    /// Its transform (row-major 3x4), the identity without one.
    pub fn matrix(&self, memory: &mut impl Read) -> Result<[f32; 12], Why> {
        if self.transform == 0 {
            return Ok([1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0]);
        }
        Ok(dwords::<12>(memory, self.transform)?.map(f32::from_bits))
    }

    /// Triangle `p`'s vertices, transformed.
    pub fn triangle(&self, p: u32, memory: &mut impl Read) -> Result<[[f32; 3]; 3], Why> {
        let m = self.matrix(memory)?;
        let ix = self.indices(p, memory)?;
        let mut out = [[0.0; 3]; 3];
        for (k, &i) in ix.iter().enumerate() {
            out[k] = apply(&m, self.position(i, memory)?);
        }
        Ok(out)
    }
}

/// Title addresses are below this (40 bits), and the title's own memory above
/// 4 GB.
const TITLE: std::ops::Range<u64> = 1 << 32..1 << 40;

impl Geometry {
    /// Whether it looks like a geometry description at all (a list's end, or
    /// memory that is not one, does not): known index formats, sane counts and
    /// strides, buffers in title memory.
    pub fn plausible(&self) -> bool {
        let indexed = self.index_format != NO_INDICES;
        matches!(self.index_format, NO_INDICES | INDICES_32 | INDICES_16 | PACKED_INDICES)
            && (1..256).contains(&self.vertex_format)
            && (1..=256).contains(&self.stride)
            && self.vertex_count <= 1 << 24
            && self.index_count <= 3 << 24
            && TITLE.contains(&self.vertex_buffer)
            && (!indexed || TITLE.contains(&self.index_buffer))
            && (self.transform == 0 || TITLE.contains(&self.transform))
    }

    /// The title memory the builder's programs read for it (address, bytes):
    /// its transform, its index stream (a packed stream's length from its
    /// header: the group table and triangle dwords, then a pool of at most
    /// three 16-bit indices a triangle; 619 streams of a capture all end
    /// within it), its vertices (with a packed format's scale and offset
    /// before them).
    pub fn spans(&self, memory: &mut impl Read) -> Vec<(u64, u64)> {
        let mut out = Vec::with_capacity(3);
        if self.transform != 0 {
            out.push((self.transform, 48));
        }
        let tris = u64::from(self.triangles());
        match self.index_format {
            INDICES_32 => out.push((self.index_buffer, 12 * tris)),
            INDICES_16 => out.push((self.index_buffer, 6 * tris)),
            PACKED_INDICES => {
                let start = match dwords::<3>(memory, self.index_buffer) {
                    Ok([h0, h1, _]) => u64::from(h0 & 0x1ffff) * 16 + 12 + 4 * u64::from(h1).max(tris),
                    // (The header is the stream's first bytes: it at least.)
                    Err(_) => 12,
                };
                out.push((self.index_buffer, (start + 6 * tris).min(MOST_SPAN)));
            }
            _ => {}
        }
        let vertices = u64::from(self.stride) * u64::from(self.vertex_count);
        if self.vertex_format == PACKED_VERTICES {
            out.push((self.vertex_buffer - 32, 32 + vertices));
        } else {
            out.push((self.vertex_buffer, vertices));
        }
        out.retain(|&(_, len)| len > 0);
        out
    }
}

/// The most one span is taken for (a malformed header).
const MOST_SPAN: u64 = 64 << 20;

/// The records of a list in title memory at `address`, as many as look like
/// records ([`Geometry::plausible`]), up to `most`.
pub fn read_records(address: u64, most: usize, memory: &mut impl Read) -> Vec<Geometry> {
    let mut out = Vec::new();
    let mut b = [0u8; RECORD];
    for n in 0..most as u64 {
        if !memory.read(address + RECORD as u64 * n, &mut b) {
            break;
        }
        match Geometry::parse(&b) {
            Some(g) if g.plausible() => out.push(g),
            _ => break,
        }
    }
    out
}

/// A row-major 3x4 matrix times a point.
pub fn apply(m: &[f32; 12], p: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|r| m[4 * r] * p[0] + m[4 * r + 1] * p[1] + m[4 * r + 2] * p[2] + m[4 * r + 3])
}

/// The records of a list (up to the first empty one).
pub fn records(bytes: &[u8]) -> Vec<Geometry> {
    bytes
        .chunks_exact(RECORD)
        .map_while(|c| Geometry::parse(c.try_into().unwrap()))
        .collect()
}

/// One packed leaf entry: a geometry's one or two triangles (a pair shares
/// an edge).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    /// Its record's number.
    pub geometry: u32,
    /// A byte kept beside the geometry number (bit 7 with a pair).
    pub kind: u8,
    pub primitive: u32,
    /// The pair's other triangle (0xffffff in the entry: none).
    pub second: Option<u32>,
    /// How the pair's vertices are shared (bits the program sorts the
    /// element node's corners by).
    pub flags: u8,
}

/// Entries kept per dispatch group (each group's at `+group << 17`, room for
/// its element nodes, 64 bytes an entry).
pub const GROUP: u32 = 2048;

/// The `n` entries of a group's packed block, as the program lays them out:
/// geometry numbers (3 bytes each), the kind bytes, the first primitives (3
/// bytes), the second (3 bytes), the flag bytes.
pub fn entries(block: &[u8], n: usize) -> Option<Vec<Entry>> {
    if block.len() < 11 * n {
        return None;
    }
    let u24 = |at: usize| u32::from(block[at]) | u32::from(block[at + 1]) << 8 | u32::from(block[at + 2]) << 16;
    Some(
        (0..n)
            .map(|i| {
                let second = u24(7 * n + 3 * i);
                Entry {
                    geometry: u24(3 * i),
                    kind: block[3 * n + i],
                    primitive: u24(4 * n + 3 * i),
                    second: (second != 0xff_ffff).then_some(second),
                    flags: block[10 * n + i],
                }
            })
            .collect(),
    )
}

/// LeafRehydration's 19 root constants: where its entries are and how it
/// places triangles in the structure's space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Leaf {
    /// Subtracted after the rotation.
    pub origin: [f32; 3],
    /// The first group's entries, bytes from the leaf view.
    pub first: u32,
    /// Times after the origin (only when `scaled`).
    pub scale: [f32; 3],
    pub scaled: bool,
    /// The rotation's rows.
    pub rows: [[f32; 3]; 3],
    /// Entries of the dispatch, all groups.
    pub count: u32,
}

impl Leaf {
    pub fn parse(c: &[u32]) -> Option<Leaf> {
        if c.len() < 19 {
            return None;
        }
        let f = |k: usize| f32::from_bits(c[k]);
        Some(Leaf {
            origin: [f(0), f(1), f(2)],
            first: c[3],
            scale: [f(4), f(5), f(6)],
            scaled: c[7] != 0,
            rows: [[f(8), f(9), f(10)], [f(12), f(13), f(14)], [f(16), f(17), f(18)]],
            count: c[11],
        })
    }

    /// The groups: each one's byte offset from the leaf view and its entries.
    pub fn groups(&self) -> Vec<(u64, usize)> {
        (0..self.count.div_ceil(GROUP))
            .map(|g| (u64::from(self.first) + (u64::from(g) << 17), (self.count - g * GROUP).min(GROUP) as usize))
            .collect()
    }

    /// A point of a geometry (transformed) in the structure's space, as the
    /// program computes it: rotated, less the origin, scaled.
    pub fn node_space(&self, p: [f32; 3]) -> [f32; 3] {
        std::array::from_fn(|r| {
            let row = self.rows[r];
            let v = row[0] * p[0] + row[1] * p[1] + row[2] * p[2] - self.origin[r];
            if self.scaled { v * self.scale[r] } else { v }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Title memory of a test: bytes by address.
    #[derive(Default)]
    struct Mem(HashMap<u64, u8>);

    impl Mem {
        fn put(&mut self, at: u64, bytes: &[u8]) {
            for (i, b) in bytes.iter().enumerate() {
                self.0.insert(at + i as u64, *b);
            }
        }
        fn dw(&mut self, at: u64, d: &[u32]) {
            let b: Vec<u8> = d.iter().flat_map(|x| x.to_le_bytes()).collect();
            self.put(at, &b);
        }
    }

    impl Read for Mem {
        fn read(&mut self, address: u64, bytes: &mut [u8]) -> bool {
            for (i, b) in bytes.iter_mut().enumerate() {
                match self.0.get(&(address + i as u64)) {
                    Some(x) => *b = *x,
                    None => return false,
                }
            }
            true
        }
    }

    fn record(g: &Geometry) -> [u8; RECORD] {
        let mut b = [0u8; RECORD];
        b[0..8].copy_from_slice(&g.transform.to_le_bytes());
        b[8..12].copy_from_slice(&g.index_format.to_le_bytes());
        b[12..16].copy_from_slice(&g.vertex_format.to_le_bytes());
        b[16..20].copy_from_slice(&g.index_count.to_le_bytes());
        b[20..24].copy_from_slice(&g.vertex_count.to_le_bytes());
        b[24..32].copy_from_slice(&g.index_buffer.to_le_bytes());
        b[32..40].copy_from_slice(&g.vertex_buffer.to_le_bytes());
        b[40..44].copy_from_slice(&(g.stride | u32::from(g.reversed) << 15).to_le_bytes());
        b
    }

    /// Packs triangles the way the console's index format 53 holds them:
    /// one group per 40 triangles, each group's indices in the pool.
    fn pack_indices(m: &mut Mem, ib: u64, tris: &[[u16; 3]]) {
        let groups = tris.len().div_ceil(40);
        // The group table right after the header; the triangle dwords after
        // it, then the pool.
        let h0 = (12 + 16 * groups as u32).div_ceil(16); // t0 = h0 * 16 + 12
        let t0 = h0 * 16 + 12;
        let h1 = tris.len() as u32; // pool at t0 + 4 * h1
        m.dw(ib, &[h0, h1, 0]);
        let mut pool: Vec<u16> = Vec::new();
        for (g, chunk) in tris.chunks(40).enumerate() {
            let base = pool.len() as u32;
            assert!(base < 1 << 17);
            m.dw(ib + 12 + 16 * g as u64, &[(base >> 16 & 1) << 14, base & 0xffff]);
            for (k, t) in chunk.iter().enumerate() {
                let p = g * 40 + k;
                let at = pool.len() as u32 - base;
                pool.extend_from_slice(t);
                let d = (at) | (at + 1) << 7 | (at + 2) << 14 | (g as u32) << 21;
                m.dw(ib + u64::from(t0) + 4 * p as u64, &[d]);
            }
        }
        let bytes: Vec<u8> = pool.iter().flat_map(|x| x.to_le_bytes()).collect();
        m.put(ib + u64::from(t0) + 4 * u64::from(h1), &bytes);
    }

    #[test]
    fn a_record_reads_back_in_d3d12s_layout() {
        let g = Geometry {
            transform: 0x9_19e9_cdb0,
            index_format: PACKED_INDICES,
            vertex_format: PACKED_VERTICES,
            index_count: 336,
            vertex_count: 227,
            index_buffer: 0x6_6fee_d760,
            vertex_buffer: 0x6_6fed_a460,
            stride: 8,
            reversed: true,
        };
        assert_eq!(Geometry::parse(&record(&g)), Some(g));
        assert_eq!(g.triangles(), 112);
        let mut list = record(&g).to_vec();
        list.extend_from_slice(&[0u8; RECORD]);
        list.extend_from_slice(&record(&g));
        assert_eq!(records(&list), vec![g]);
    }

    #[test]
    fn the_packed_index_stream_gives_back_each_triangle() {
        let mut m = Mem::default();
        let ib = 0x5000_0000;
        let tris: Vec<[u16; 3]> = (0..100u16).map(|p| [p, p + 1, 300 - p]).collect();
        pack_indices(&mut m, ib, &tris);
        let mut g = Geometry {
            transform: 0,
            index_format: PACKED_INDICES,
            vertex_format: PACKED_VERTICES,
            index_count: 300,
            vertex_count: 301,
            index_buffer: ib,
            vertex_buffer: 0,
            stride: 8,
            reversed: false,
        };
        for (p, t) in tris.iter().enumerate() {
            assert_eq!(g.indices(p as u32, &mut m), Ok(t.map(u32::from)), "triangle {p}");
        }
        g.reversed = true;
        assert_eq!(g.indices(7, &mut m), Ok([293, 8, 7]));
        assert_eq!(g.indices(100, &mut m), Err(Why::OutOfRange));
    }

    #[test]
    fn plain_index_formats() {
        let mut m = Mem::default();
        m.dw(0x100, &[4, 5, 6, 7, 8, 9]);
        m.put(0x200, &[1, 0, 2, 0, 3, 0, 9, 0, 8, 0, 7, 0]);
        let mut g = Geometry {
            transform: 0,
            index_format: INDICES_32,
            vertex_format: VERTICES_FLOAT3,
            index_count: 6,
            vertex_count: 12,
            index_buffer: 0x100,
            vertex_buffer: 0,
            stride: 12,
            reversed: true,
        };
        // (The reversal is the packed stream's only.)
        assert_eq!(g.indices(1, &mut m), Ok([7, 8, 9]));
        g.index_format = INDICES_16;
        g.index_buffer = 0x200;
        assert_eq!(g.indices(1, &mut m), Ok([9, 8, 7]));
        g.index_format = NO_INDICES;
        assert_eq!(g.indices(3, &mut m), Ok([9, 10, 11]));
        g.index_format = 99;
        assert_eq!(g.indices(0, &mut m), Err(Why::IndexFormat(99)));
    }

    #[test]
    fn packed_vertices_take_their_scale_offset_and_axis_word() {
        let mut m = Mem::default();
        let vb = 0x7000_0040;
        let f = f32::to_bits;
        // a = 0x123456, b = 0xabcdef, c = 0x4321.
        let (d0, d1) = (0x123456 << 8 | 0xef, 0x4321 << 16 | 0xabcd);
        m.dw(vb, &[d0, d1]);
        let g = |axes: u32, m: &mut Mem| {
            m.dw(vb - 32, &[f(1.0), f(2.0), f(3.0), axes, f(0.5), f(0.25), f(0.125), 0]);
            Geometry {
                transform: 0,
                index_format: NO_INDICES,
                vertex_format: PACKED_VERTICES,
                index_count: 0,
                vertex_count: 3,
                index_buffer: 0,
                vertex_buffer: vb,
                stride: 8,
                reversed: false,
            }
            .position(0, m)
        };
        let a = 0x123456 as f32 * 0.5 + 1.0;
        let b = 0xabcdef as f32 * 0.25 + 2.0;
        let c = 0x4321 as f32 * 0.125 + 3.0;
        assert_eq!(g(2, &mut m), Ok([a, b, c]));
        assert_eq!(g(0, &mut m), Ok([c, b, a]));
        assert_eq!(g(1, &mut m), Ok([b, c, a]));
    }

    #[test]
    fn a_transformed_float_triangle() {
        let mut m = Mem::default();
        let f = f32::to_bits;
        m.dw(0x300, &[f(1.0), f(2.0), f(3.0), f(4.0), f(5.0), f(6.0), f(7.0), f(8.0), f(9.0)]);
        // Swap x and y, then move by (10, 20, 30).
        m.dw(0x400, &[f(0.0), f(1.0), f(0.0), f(10.0), f(1.0), f(0.0), f(0.0), f(20.0), f(0.0), f(0.0), f(1.0), f(30.0)]);
        let g = Geometry {
            transform: 0x400,
            index_format: NO_INDICES,
            vertex_format: VERTICES_FLOAT3,
            index_count: 0,
            vertex_count: 3,
            index_buffer: 0,
            vertex_buffer: 0x300,
            stride: 12,
            reversed: false,
        };
        assert_eq!(g.triangle(0, &mut m), Ok([[12.0, 21.0, 33.0], [15.0, 24.0, 36.0], [18.0, 27.0, 39.0]]));
    }

    #[test]
    fn a_records_spans_are_what_the_program_reads() {
        let mut m = Mem::default();
        let ib = 0x5_0000_0000;
        let tris: Vec<[u16; 3]> = (0..100u16).map(|p| [p, p + 1, p + 2]).collect();
        pack_indices(&mut m, ib, &tris);
        let g = Geometry {
            transform: 0x9_0000_0040,
            index_format: PACKED_INDICES,
            vertex_format: PACKED_VERTICES,
            index_count: 300,
            vertex_count: 102,
            index_buffer: ib,
            vertex_buffer: 0x6_0000_0100,
            stride: 8,
            reversed: false,
        };
        assert!(g.plausible());
        let spans = g.spans(&mut m);
        assert_eq!(spans[0], (0x9_0000_0040, 48));
        assert_eq!(spans[2], (0x6_0000_00e0, 32 + 8 * 102));
        // Every byte the decoding reads lies in the index span.
        let (start, len) = spans[1];
        struct Within<'a>(&'a mut Mem, u64, u64);
        impl Read for Within<'_> {
            fn read(&mut self, address: u64, bytes: &mut [u8]) -> bool {
                let outside = address < self.1 || address + bytes.len() as u64 > self.1 + self.2;
                !(outside && address < 0x6_0000_0000) && self.0.read(address, bytes)
            }
        }
        let mut within = Within(&mut m, start, len);
        for p in 0..100 {
            assert_eq!(g.indices(p, &mut within), Ok(tris[p as usize].map(u32::from)));
        }
        // Not records: a list's end, other memory.
        assert!(!Geometry { index_format: 7, ..g }.plausible());
        assert!(!Geometry { vertex_buffer: 0x1000, ..g }.plausible());
        assert!(!Geometry { stride: 0, ..g }.plausible());
        let mut list = record(&g).to_vec();
        list.extend_from_slice(&record(&Geometry { index_format: 7, ..g }));
        list.extend_from_slice(&record(&g));
        m.put(0x7_0000_0000, &list);
        assert_eq!(read_records(0x7_0000_0000, 64, &mut m), vec![g]);
    }

    #[test]
    fn entries_are_laid_out_field_by_field() {
        let n = 3;
        let mut b = vec![0u8; 11 * n];
        let put24 = |b: &mut Vec<u8>, at: usize, v: u32| b[at..at + 3].copy_from_slice(&v.to_le_bytes()[..3]);
        for i in 0..n {
            put24(&mut b, 3 * i, 10 + i as u32);
            b[3 * n + i] = 0x81;
            put24(&mut b, 4 * n + 3 * i, 2 * i as u32);
            put24(&mut b, 7 * n + 3 * i, if i == 1 { 0xff_ffff } else { 2 * i as u32 + 1 });
            b[10 * n + i] = 9 + i as u8;
        }
        let e = entries(&b, n).unwrap();
        assert_eq!(e[1], Entry { geometry: 11, kind: 0x81, primitive: 2, second: None, flags: 10 });
        assert_eq!(e[2].second, Some(5));
        assert!(entries(&b[..20], n).is_none());
    }

    #[test]
    fn the_leaf_constants_place_a_point() {
        // A dispatch of the capture: origin 0, scale (8192, 262144, 32768),
        // the identity, 86 entries at 0x180.
        let c = [
            0, 0x3fa7_85ba, 0, 0x180, 0x4600_0000, 0x4880_0000, 0x4700_0000, 1, 0x3f80_0000, 0, 0, 0x56, 0, 0x3f80_0000, 0, 0, 0, 0,
            0x3f80_0000,
        ];
        let l = Leaf::parse(&c).unwrap();
        assert_eq!(l.count, 86);
        assert_eq!(l.groups(), vec![(0x180, 86)]);
        let y0 = f32::from_bits(0x3fa7_85ba);
        assert_eq!(l.node_space([1.0, 2.0, 0.5]), [8192.0, (2.0 - y0) * 262144.0, 16384.0]);
        let big = Leaf { count: 5000, ..l };
        assert_eq!(big.groups(), vec![(0x180, 2048), (0x180 + (1 << 17), 2048), (0x180 + (2 << 17), 904)]);
    }

    /// Offline, step 2 of plan #2: the console's element nodes against the
    /// records' triangles. `VICTORY_REHYDRATE_CAPTURE=<capture folder>`
    /// (`dispatches.txt`, `windows.txt`, `windows.bin` of a run) and
    /// `VICTORY_BVH_DUMP=<that run's VICTORY_MEMORY_DUMP folder>`;
    /// `cargo test -p victory-gpu --lib records -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn the_rehydrated_elements_are_the_records_triangles() {
        let (Some(capture), Some(dump)) = (std::env::var_os("VICTORY_REHYDRATE_CAPTURE"), std::env::var_os("VICTORY_BVH_DUMP")) else {
            eprintln!("VICTORY_REHYDRATE_CAPTURE or VICTORY_BVH_DUMP not set");
            return;
        };
        let capture = std::path::Path::new(&capture);
        let mut windows = Windows::load(capture);
        let (arenas, _) = crate::bvh_flat::dumped::load(std::path::Path::new(&dump));
        let text = std::fs::read_to_string(capture.join("dispatches.txt")).expect("dispatches.txt");
        let hex = |s: &str| u64::from_str_radix(s.trim_start_matches("0x"), 16).unwrap_or(0);
        let (mut nodes, mut matched, mut missing, mut undecoded, mut dispatches) = (0usize, 0usize, 0usize, 0usize, 0usize);
        // Dispatches whose decoded triangles all matched, some, none.
        let (mut whole, mut partly, mut none, mut unread) = (0usize, 0usize, 0usize, 0usize);
        let mut shown = 0;
        for line in text.lines().filter(|l| l.starts_with("LeafRehydration")) {
            let f: Vec<&str> = line.split_whitespace().collect();
            let list = |name: &str| -> Vec<u64> {
                f.iter().position(|w| *w == name).map(|i| f[i + 1].split(',').map(hex).collect()).unwrap_or_default()
            };
            let (views, constants) = (list("views"), list("constants"));
            let constants: Vec<u32> = constants.iter().map(|&c| c as u32).collect();
            let (Some(leaf), [view, _, records_at, ..]) = (Leaf::parse(&constants), views.as_slice()) else { continue };
            let mut list = vec![0u8; RECORD * 64];
            if !windows.read(*records_at, &mut list) {
                continue;
            }
            let geometries = records(&list);
            dispatches += 1;
            let (mut here_matched, mut here_missing) = (0usize, 0usize);
            for (offset, n) in leaf.groups() {
                let mut block = vec![0u8; 11 * n];
                if !windows.read(view + offset, &mut block) {
                    continue;
                }
                let Some(entries) = entries(&block, n) else { continue };
                for (i, e) in entries.iter().enumerate() {
                    let node = view + offset + 64 * i as u64;
                    let Some(words) = arenas.words::<28>(node) else { continue };
                    let Some(g) = geometries.get(e.geometry as usize) else {
                        undecoded += 1;
                        continue;
                    };
                    let mut ours = Vec::new();
                    for p in std::iter::once(e.primitive).chain(e.second) {
                        match g.triangle(p, &mut windows) {
                            Ok(t) => ours.push(t.map(|v| leaf.node_space(v))),
                            Err(_) => undecoded += 1,
                        }
                    }
                    if ours.is_empty() {
                        continue;
                    }
                    for k in 0..4u32 {
                        if words[15] >> (8 * k) & 0xff == 0 {
                            continue;
                        }
                        nodes += 1;
                        let theirs = victory_shader::bvh::triangle_vertices(&words, k);
                        if ours.iter().any(|t| same_triangle(t, &theirs)) {
                            matched += 1;
                            here_matched += 1;
                        } else {
                            missing += 1;
                            here_missing += 1;
                            if here_matched > 0 && shown < 8 {
                                shown += 1;
                                eprintln!("node {node:#x} triangle {k}: theirs {theirs:?}; ours {ours:?} (entry {e:?})");
                            }
                        }
                    }
                }
            }
            match (here_matched, here_missing) {
                (0, 0) => unread += 1,
                (_, 0) => whole += 1,
                (0, _) => none += 1,
                _ => partly += 1,
            }
        }
        eprintln!(
            "{dispatches} dispatches: {nodes} triangles of element nodes with decoded entries, {matched} the records' own, {missing} not, {undecoded} triangles not decoded (memory not captured)"
        );
        eprintln!("dispatches: {whole} every triangle matched, {partly} some, {none} none, {unread} nothing decoded");
        assert!(nodes > 0, "no element nodes in the dump at the capture's addresses");
        assert_eq!(missing, 0);
    }

    /// The same three corners, in any order, within a thousandth of the
    /// larger coordinate (the program's arithmetic is fused differently).
    fn same_triangle(a: &[[f32; 3]; 3], b: &[[f32; 3]; 3]) -> bool {
        let close = |p: &[f32; 3], q: &[f32; 3]| (0..3).all(|k| (p[k] - q[k]).abs() <= 1e-3 * p[k].abs().max(q[k].abs()).max(1.0));
        a.iter().all(|p| b.iter().any(|q| close(p, q))) && b.iter().all(|q| a.iter().any(|p| close(p, q)))
    }

    /// The capture's title memory: `windows.txt` (address, offset, bytes)
    /// over `windows.bin`.
    struct Windows {
        index: std::collections::BTreeMap<u64, (u64, u64)>,
        bin: std::fs::File,
    }

    impl Windows {
        fn load(folder: &std::path::Path) -> Windows {
            let hex = |s: &str| u64::from_str_radix(s.trim_start_matches("0x"), 16).unwrap_or(0);
            let mut index = std::collections::BTreeMap::new();
            for line in std::fs::read_to_string(folder.join("windows.txt")).expect("windows.txt").lines() {
                let f: Vec<&str> = line.split_whitespace().collect();
                if let [a, o, g, ..] = f.as_slice() {
                    index.entry(hex(a)).or_insert((hex(o), hex(g)));
                }
            }
            Windows { index, bin: std::fs::File::open(folder.join("windows.bin")).expect("windows.bin") }
        }
    }

    impl Read for Windows {
        fn read(&mut self, address: u64, bytes: &mut [u8]) -> bool {
            use std::io::{Read as _, Seek, SeekFrom};
            let n = bytes.len() as u64;
            for (&start, &(offset, got)) in self.index.range(address.saturating_sub(1 << 16)..=address).rev() {
                if address + n <= start + got {
                    return self.bin.seek(SeekFrom::Start(offset + address - start)).is_ok() && self.bin.read_exact(bytes).is_ok();
                }
            }
            false
        }
    }
}
