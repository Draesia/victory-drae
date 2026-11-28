//! Title memory by address, for translated shaders that read and write
//! through raw 40-bit addresses (`victory_shader::emit::SLOT_MEMORY`: the
//! console's ray-tracing structure builder and the programs that walk its
//! structures; the structures themselves lie in plain title memory, in no
//! buffer object).
//!
//! A 64 KiB page in a title buffer that holds all of it is in that buffer's
//! copy ([`crate::shadow`]), which every other access uses. A page outside
//! buffers has a page of its own in an **arena**: memory of ours with a copy
//! on the card ([`crate::shadow::by_address`]). The streamed structures lie
//! anywhere between other data (6.6 GB of title memory in 4 MiB windows, run
//! light-1; 8 GB in windows of 256 KiB, light-2), so a page is what is kept.
//!
//! This module keeps what a shader needs to find the pages:
//!
//! - the **directory**: per 64 KiB page of title address space the number
//!   of the copy that holds the page (0: none) and the page's byte offset
//!   in it; one for all lists, entries only ever go from empty to filled.
//!   The CPU writes it in memory of its own and shaders read a copy of that
//!   on the card, brought up to date as title memory is (over the bus, every
//!   step of every ray waited for it);
//! - per view heap a run of slots with a raw unordered access view of each
//!   numbered copy (slot 0 of the run: a null view, for pages without a
//!   copy);
//! - the **faults**: the page numbers shaders found empty, read back here
//!   once a frame, so the pages are there from the next frame on. A pointer a
//!   shader takes out of memory cannot be known before it runs.
//!
//! **A page that is not there when a shader touches it is a lost access**:
//! the read gives 0, the store is dropped, and the fault note only helps
//! the next frame. For the structure builder that is the same as a dropped
//! stage, so before one of its dispatches the host makes ready what it can
//! see the dispatch name:
//!
//! - 256 KiB from each root view outside buffers, and where title memory has a
//!   packed ("dehydrated") structure there, all of it: the title ships its
//!   structures packed, and the builder's rehydration stages unpack each in
//!   place once it is streamed in;
//! - 4 MiB from each unordered access view outside buffers (structures built);
//! - the page of each pair of root constants that is a title address and of
//!   each root constant that is one in 256-byte units;
//! - the first page of every structure a list's upload memory names
//!   (a top-level build's instances).
//!
//! **What comes back to title memory**: what the builder stores in title
//! buffers of at most [`BACK`] bytes it names (the sizes the title reads to
//! make a structure smaller). Structures and scratch memory stay on the card
//! (the first whole run of the builder wrote garbage over the title's
//! objects). So an arena's page is the only place a structure is whole, and
//! title memory must not go over it: of a page the title wrote only the
//! dwords that changed since they were last taken go up. A packed structure
//! goes up whole when the builder comes to it, whatever is known of it: the
//! same bytes streamed in again do not differ from those last taken, and the
//! card holds them unpacked (run light-3: a quarter of the structures
//! unpacked a second time, into garbage all over their neighbours).
//!
//! **Stores are bounded**: a structure the builder misreads makes it store
//! anywhere. Pages of title buffers take its stores only once one of its
//! dispatches was given that buffer as an unordered access view (its scratch
//! memory, its results); what else a store reaches is an arena's.
//!
//! A packed structure is gone when title memory has no header of it any more
//! (looked at over [`SWEEP`] frames); the pages it alone has are given back.
//!
//! shortcut: a page held for anything else is kept for the rest of the run.
//! shortcut: pages a list does not name are looked at again in turn, over
//! [`SWEEP`] frames.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use victory_shader::emit::{MEMORY_FAULTS, MEMORY_PAGES, MEMORY_READ_ONLY};
use windows::Win32::Graphics::Direct3D12::*;

use crate::descriptor::host::HostHeap;
use crate::host::{HostList, host};
use crate::shadow::Shadow;

/// Copies that can be numbered (the run of heap slots is one longer).
const COPIES: u32 = (1 << 12) - 1;
/// A directory page.
const PAGE: u64 = 1 << 16;
/// An arena's bytes.
const ARENA: u64 = 64 << 20;
/// What is compared at a time.
const PIECE: usize = 4096;
/// Pages held, at most (3 GiB).
const MOST: usize = 3 << 14;
/// Pages of title memory with small title buffers in them, at most.
const WINDOWS: usize = 512;
/// The biggest packed structure taken for one.
const PACKED: u64 = 64 << 20;
/// For so many frames what a page holds may still be on its way.
const YOUNG: u64 = 4;
/// Free pages there are, at least, when a list is recorded.
const SPARE: usize = 1024;
/// A packed structure not named for so many frames is another one.
const AGAIN: u64 = 8;
/// From a root view; from one that is written (a structure being built).
const VIEW: u64 = 256 << 10;
const BUILT: u64 = 4 << 20;
/// The biggest title buffer whose builder results come back.
const BACK: u64 = 256 << 10;
/// Pages taken from the faults, of one frame and in all, at most: a ray in a
/// structure that is not right goes anywhere, and 2,048 a frame took all the
/// pages there are in the first seconds (run light-4).
const FAULTS: usize = 128;
const FAULTS_ALL: usize = 2048;
/// Frames over which every page is looked at once.
const SWEEP: usize = 128;

/// What a draw or dispatch does with title memory by address.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Use {
    Reads,
    /// Stores by address, as the structure builder; `back`: its results in
    /// small title buffers come back.
    Builds { back: bool },
}

/// A page of title memory in an arena.
struct Held {
    /// Where in the arena's memory, and the arena's place in `copies`.
    at: usize,
    copy: usize,
    /// The write-watch asking after which it was last taken (0: never).
    taken: u64,
    /// The frame it was last looked at in, and the one it came in.
    looked: u64,
    born: u64,
    /// Packed structures that have bytes in it, and whether it is held for
    /// them alone.
    owners: u32,
    theirs: bool,
}

/// A packed structure the builder came to.
struct Packed {
    /// The frame it last did.
    frame: u64,
    len: u64,
    /// Its first two dwords in title memory.
    header: u64,
}

struct Memory {
    /// The CPU's entries (number, offset), and the copy shaders read.
    directory: (*mut u32, Arc<Shadow>),
    faults: (ID3D12Resource, *mut u32, u64),
    /// Copy `n` is `copies[n - 1]`.
    copies: Vec<Arc<Shadow>>,
    /// Places of title buffers' copies among `copies`.
    numbers: HashMap<usize, usize>,
    /// Arenas: place in `copies`, address.
    arenas: Vec<(usize, usize)>,
    /// By title page.
    pages: HashMap<u64, Held>,
    /// Their numbers in the order they came, and where the sweep is.
    order: Vec<u64>,
    sweep: usize,
    /// Windows of title memory itself ([`Memory::map`]).
    windows: usize,
    /// Packed structures the builder came to: the frame it last did.
    packed: HashMap<u64, Packed>,
    /// Their addresses in the order they came, and where the sweep is.
    packed_order: Vec<u64>,
    packed_sweep: usize,
    /// Places of pages that are free: the copy, the address in the arena.
    free: Vec<(usize, usize)>,
    /// Copies of title buffers the builder may store in.
    open: std::collections::HashSet<usize>,
    /// Pages taken from faults.
    faulted: usize,
    /// Per host view heap (by address): the first slot of its run, and how
    /// many copies have their view there.
    heaps: HashMap<usize, (u32, u32)>,
    /// The frame the faults were last read in.
    frame: u64,
    /// Pages asked for this frame that are not the title's memory.
    refused: std::collections::HashSet<u64>,
    stats: Stats,
    /// `VICTORY_DXR=1`: the title's ray-tracing structures read back.
    dxr: dxr::Dxr,
    #[cfg(feature = "diagnostics")]
    dump: dump::Dump,
    #[cfg(feature = "diagnostics")]
    trace: trace::Trace,
}

/// Since the last report.
#[derive(Default)]
struct Stats {
    from: Option<(std::time::Instant, u64)>,
    uses: [u64; 2],
    faults: u64,
    unknown: u64,
    back: u64,
    /// Pages looked at, bytes that changed, packed structures sent.
    looked: u64,
    changed: u64,
    packed: u64,
    packed_bytes: u64,
    gone: u64,
    /// Pages that came for an address in constants, a written view, another
    /// view, a structure named in upload memory.
    came: [u64; 4],
    micros: u64,
}

// SAFETY: the mappings are of memory the struct owns; all access is under
// the mutex.
unsafe impl Send for Memory {}

static MEMORY: Mutex<Option<Memory>> = Mutex::new(None);

/// What a draw binds.
pub(crate) struct Ready {
    pub(crate) directory: u64,
    pub(crate) faults: u64,
    /// The view heap slot of the null view; copy `n`'s view is `n` further.
    pub(crate) base: u32,
    /// Work of ours was recorded on the list (native ray tracing): its
    /// compute root signature and pipeline are to be set again.
    pub(crate) clobbered: bool,
}

impl Memory {
    fn new() -> Option<Memory> {
        let host = host()?;
        let bytes = u64::from(MEMORY_PAGES) * 8;
        // Zeroed, and watched for writes as the title's memory is.
        let entries = crate::upload::reserve(bytes as usize)?;
        let directory = crate::shadow::by_address(entries as u64, bytes)?;
        crate::host::name(&directory.buffer, || "title memory by address: directory".into());
        let (faults, data) = host.shared_buffer(u64::from(MEMORY_FAULTS) * 4)?;
        crate::host::name(&faults, || "title memory by address: faults".into());
        // SAFETY: a fresh mapping of at least that many bytes.
        unsafe { std::ptr::write_bytes(data, 0, MEMORY_FAULTS as usize * 4) };
        // SAFETY: a live buffer.
        let gpu = unsafe { faults.GetGPUVirtualAddress() };
        tracing::info!("title memory by address: a shader reads or writes through raw addresses");
        Some(Memory {
            directory: (entries as *mut u32, directory),
            faults: (faults, data.cast::<u32>(), gpu),
            copies: Vec::new(),
            numbers: HashMap::new(),
            arenas: Vec::new(),
            pages: HashMap::new(),
            order: Vec::new(),
            sweep: 0,
            windows: 0,
            packed: HashMap::new(),
            packed_order: Vec::new(),
            packed_sweep: 0,
            free: Vec::new(),
            open: Default::default(),
            faulted: 0,
            heaps: HashMap::new(),
            frame: u64::MAX,
            refused: Default::default(),
            stats: Stats::default(),
            dxr: Default::default(),
            #[cfg(feature = "diagnostics")]
            dump: Default::default(),
            #[cfg(feature = "diagnostics")]
            trace: Default::default(),
        })
    }

    /// The directory's entry of `page`.
    fn entry(&self, page: u64) -> *mut u32 {
        debug_assert!(page < u64::from(MEMORY_PAGES));
        // SAFETY: inside the directory.
        unsafe { self.directory.0.add(page as usize * 2) }
    }

    /// The place in `copies` of the copy that holds `page`, if one does.
    fn copy_of(&self, page: u64) -> Option<usize> {
        if page >= u64::from(MEMORY_PAGES) {
            return None;
        }
        // SAFETY: an entry of the directory.
        ((unsafe { self.entry(page).read() } & !MEMORY_READ_ONLY) as usize).checked_sub(1)
    }

    /// Whether `page` can be given a place: in the directory, not turned down
    /// this frame, and the title's GPU memory. (A shader's pointer, or a
    /// constant taken for an address, may be anything.)
    fn possible(&mut self, page: u64) -> bool {
        // Not the last entry: addresses past the directory read it, and it
        // stays empty.
        if page < 0x1_0000 || page >= u64::from(MEMORY_PAGES) - 1 || self.refused.contains(&page) {
            return false;
        }
        let at = (page * PAGE) as usize;
        let ours = crate::texture::committed(at as *const u8, PAGE as usize)
            && crate::watch::writes(at, at + PAGE as usize, true).is_some();
        if !ours {
            self.refused.insert(page);
            self.stats.unknown += 1;
        }
        ours
    }

    /// Gives a copy a number, if one is left.
    fn number(&mut self, shadow: Arc<Shadow>) -> Option<usize> {
        let key = Arc::as_ptr(&shadow) as usize;
        if let Some(&n) = self.numbers.get(&key) {
            return Some(n);
        }
        if self.copies.len() >= COPIES as usize {
            static ONCE: std::sync::Once = std::sync::Once::new();
            ONCE.call_once(|| tracing::warn!(COPIES, "title memory by address: no number left for another copy; its pages read zeros"));
            return None;
        }
        // SAFETY: a live buffer.
        let flags = unsafe { shadow.buffer.GetDesc() }.Flags;
        if flags.0 & D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS.0 == 0 {
            return None;
        }
        self.copies.push(shadow);
        let n = self.copies.len() - 1;
        self.numbers.insert(key, n);
        Some(n)
    }

    /// Gives `page` a place, if it has none and the memory is the title's;
    /// whether it has one then.
    fn map(&mut self, page: u64) -> bool {
        if self.copy_of(page).is_some() {
            return true;
        }
        if !self.possible(page) {
            return false;
        }
        let start = page * PAGE;
        if crate::shadow::in_buffer(start, PAGE).is_some() {
            let Some(shadow) = crate::shadow::find(start, PAGE) else {
                return false;
            };
            let fresh = !self.numbers.contains_key(&(Arc::as_ptr(&shadow) as usize));
            let Some(n) = self.number(shadow.clone()) else {
                return false;
            };
            // Every page the copy holds whole, not only the one asked for.
            // shortcut: a page of a title buffer inside this one that has a
            // copy of its own is given this copy too.
            if fresh {
                let (first, len) = shadow.span();
                let mut at = first.next_multiple_of(PAGE);
                while at + PAGE <= first + len && at / PAGE < u64::from(MEMORY_PAGES) - 1 {
                    let entry = self.entry(at / PAGE);
                    // SAFETY: an entry of the directory.
                    if unsafe { entry.read() } == 0 {
                        close(entry, n, at - first);
                    }
                    at += PAGE;
                }
            }
            // SAFETY: as above.
            if unsafe { self.entry(page).read() } == 0 {
                close(self.entry(page), n, start - shadow.span().0);
            }
            return true;
        }
        // Small title buffers in it: what the builder stores in them comes
        // back, so title memory itself.
        if self.windows < WINDOWS && crate::shadow::small_buffer_in(start, PAGE, BACK) {
            if let Some(shadow) = crate::shadow::by_address(start, PAGE)
                && let Some(n) = self.number(shadow)
            {
                self.windows += 1;
                fill(self.entry(page), n, 0);
                return true;
            }
            return false;
        }
        if self.pages.len() >= MOST {
            static ONCE: std::sync::Once = std::sync::Once::new();
            ONCE.call_once(|| tracing::warn!(MOST, "title memory by address: no more pages are taken; others read zeros"));
            return false;
        }
        if let Some((copy, at)) = self.free.pop() {
            self.hold(page, at, copy);
            return true;
        }
        if !self.another() {
            return false;
        }
        let Some((copy, at)) = self.free.pop() else {
            return false;
        };
        self.hold(page, at, copy);
        true
    }

    /// Another arena's pages.
    fn another(&mut self) -> bool {
        let Some(at) = crate::upload::reserve(ARENA as usize) else {
            return false;
        };
        let Some(n) = crate::shadow::by_address(at as u64, ARENA).and_then(|s| self.number(s)) else {
            return false;
        };
        self.arenas.push((n, at));
        // The lowest first.
        self.free.extend((0..(ARENA / PAGE) as usize).rev().map(|i| (n, at + i * PAGE as usize)));
        true
    }

    /// `page` is held at `at` in the arena that is `copy`.
    fn hold(&mut self, page: u64, at: usize, copy: usize) {
        self.pages.insert(page, Held { at, copy, taken: 0, looked: u64::MAX, born: self.frame, owners: 0, theirs: false });
        self.order.push(page);
        fill(self.entry(page), copy, at as u64 - self.copies[copy].span().0);
    }

    /// The packed structure at `address` is gone: the pages it alone had are
    /// given back.
    fn gone(&mut self, address: u64) {
        let Some(packed) = self.packed.remove(&address) else { return };
        for page in address / PAGE..=(address + packed.len - 1) / PAGE {
            let Some(held) = self.pages.get_mut(&page) else { continue };
            held.owners = held.owners.saturating_sub(1);
            if held.owners == 0 && held.theirs {
                self.free.push((held.copy, held.at));
                self.pages.remove(&page);
                // SAFETY: an entry of the directory, under the mutex.
                unsafe { self.entry(page).write_volatile(0) };
            }
        }
        self.stats.gone += 1;
    }

    /// Whether the packed structure at `address` is still in title memory.
    fn there(&self, address: u64) -> bool {
        let mut header = [0u8; 8];
        crate::texture::committed(address as *const u8, 8) && {
            crate::host::copy_mapped(header.as_mut_ptr(), address as *const u8, 8, false);
            self.packed.get(&address).is_some_and(|p| p.header == u64::from_le_bytes(header))
        }
    }

    /// Looks at some of the pages and packed structures again.
    fn look_again(&mut self) {
        for _ in 0..self.order.len().div_ceil(SWEEP) {
            self.sweep = (self.sweep + 1) % self.order.len();
            let page = self.order[self.sweep];
            if self.pages.contains_key(&page) {
                self.bring(page, page + 1);
            } else {
                self.order.swap_remove(self.sweep);
                if self.order.is_empty() {
                    break;
                }
            }
        }
        for _ in 0..self.packed_order.len().div_ceil(SWEEP) {
            self.packed_sweep = (self.packed_sweep + 1) % self.packed_order.len();
            let address = self.packed_order[self.packed_sweep];
            if !self.packed.contains_key(&address) {
                self.packed_order.swap_remove(self.packed_sweep);
                if self.packed_order.is_empty() {
                    break;
                }
            } else if !self.there(address) {
                self.gone(address);
            }
        }
    }

    /// A dispatch of the builder was given `address` to write: if it is in a
    /// title buffer's copy, stores by address reach that from here on.
    fn opens(&mut self, address: u64) {
        let Some(n) = self.map(address / PAGE).then(|| self.copy_of(address / PAGE)).flatten() else {
            return;
        };
        if self.pages.contains_key(&(address / PAGE)) || !self.open.insert(n) {
            return;
        }
        // (Its pages hold what the CPU writes beside the builder's scratch.)
        self.copies[n].keep_stores();
        let (first, len) = self.copies[n].span();
        tracing::info!(
            start = format_args!("{first:#x}"),
            len,
            "title memory by address: the structure builder may store in a title buffer"
        );
        for page in first.div_ceil(PAGE)..(first + len) / PAGE {
            if self.copy_of(page) == Some(n) {
                fill(self.entry(page), n, page * PAGE - first);
            }
        }
    }

    /// [`Memory::map`] of the pages of `[address, address + len)`, up to the
    /// first that is not the title's, and what the title wrote there since
    /// they were taken goes into the arenas.
    fn ready(&mut self, address: u64, len: u64) {
        let first = address / PAGE;
        let mut end = first;
        while end * PAGE < address + len && self.map(end) {
            end += 1;
        }
        self.bring(first, end);
    }

    /// [`Memory::ready`] of what the geometry descriptions at `at` name: the
    /// transforms, index streams and vertices (`crate::records`), read from
    /// title memory as the CPU holds it.
    fn ready_geometry(&mut self, at: u64) {
        const RECORDS: usize = 4096;
        let mut title = |address: u64, bytes: &mut [u8]| {
            crate::texture::committed(address as *const u8, bytes.len()) && {
                crate::host::copy_mapped(bytes.as_mut_ptr(), address as *const u8, bytes.len(), false);
                true
            }
        };
        for g in crate::records::read_records(at, RECORDS, &mut title) {
            for (start, len) in g.spans(&mut title) {
                self.ready(start, len);
            }
        }
    }

    /// Takes the pages `[first, end)` that are in arenas from title memory
    /// where it wrote them.
    fn bring(&mut self, first: u64, end: u64) {
        // Once a frame: the builder names the same pages list after list.
        let frame = self.frame;
        if !(first..end).any(|page| self.pages.get(&page).is_some_and(|h| h.looked != frame)) {
            return;
        }
        let started = std::time::Instant::now();
        let answer = crate::watch::writes((first * PAGE) as usize, (end * PAGE) as usize, true);
        for page in first..end {
            let Some(held) = self.pages.get_mut(&page) else {
                continue;
            };
            if held.looked == frame {
                continue;
            }
            held.looked = frame;
            self.stats.looked += 1;
            let wrote = answer
                .as_ref()
                .map(|(_, w)| w.binary_search_by_key(&(page as usize), |w| w.0).map_or(0, |i| w[i].1));
            let now = answer.as_ref().map_or(1, |a| a.0);
            let stale = held.taken == 0 || wrote.is_none_or(|w| w > held.taken);
            if !stale {
                continue;
            }
            let never = std::mem::replace(&mut held.taken, now.max(1)) == 0;
            let shadow = &self.copies[held.copy];
            let base = held.at - shadow.span().0 as usize;
            let mut title = [0u8; PIECE];
            for piece in (0..PAGE as usize).step_by(PIECE) {
                let from = (page * PAGE) as usize + piece;
                // (Pages that are not there read as zeros.)
                crate::host::copy_mapped(title.as_mut_ptr(), from as *const u8, PIECE, false);
                // SAFETY: a page of an arena of ours, under the mutex.
                let ours = unsafe { std::slice::from_raw_parts_mut((held.at + piece) as *mut u8, PIECE) };
                self.stats.changed += merge(&title, ours, never) as u64 * 4;
            }
            if never {
                shadow.resend(base, base + PAGE as usize);
            }
        }
        self.stats.micros += started.elapsed().as_micros() as u64;
    }

    /// The bytes of a packed structure title memory has at `address` by now,
    /// and its first two dwords.
    fn packed_at(&self, address: u64) -> Option<(u64, u64)> {
        if address & 0xFF != 0 {
            return None;
        }
        let mut header = [0u8; 8];
        crate::host::copy_mapped(header.as_mut_ptr(), address as *const u8, 8, false);
        let header = u64::from_le_bytes(header);
        Some((packed_len(header)?, header))
    }

    /// At the submission of a list whose builder dispatch was given `address`
    /// outside buffers, in the order of its copies: what title memory has
    /// there now goes up. When the dispatch is recorded it may not be there
    /// yet: the title's queue waits for its streaming. (Nothing is asked of
    /// the write watch here: the submission's asking stands.)
    fn land(&mut self, address: u64) {
        let page = address / PAGE;
        if !crate::texture::committed((page * PAGE) as *const u8, PAGE as usize) {
            return;
        }
        let Some((len, header)) = self.packed_at(address) else {
            // Anything else: what came since its page was first taken.
            for page in page..=(address + VIEW - 1) / PAGE {
                if self.pages.get(&page).is_some_and(|h| h.born + YOUNG >= self.frame) {
                    self.take(page, 0, PAGE as usize, false);
                }
            }
            return;
        };
        if let Some(known) = self.packed.get_mut(&address) {
            let soon = known.frame + AGAIN >= self.frame;
            known.frame = self.frame;
            if known.header == header && soon {
                return;
            }
            if known.header != header {
                self.gone(address);
            }
        }
        let last = (address + len - 1) / PAGE;
        if !self.packed.contains_key(&address) {
            self.packed.insert(address, Packed { frame: self.frame, len, header });
            self.packed_order.push(address);
            for page in page..=last {
                let new = self.copy_of(page).is_none();
                if new && !self.place(page) {
                    continue;
                }
                if let Some(held) = self.pages.get_mut(&page) {
                    held.owners += 1;
                    held.theirs |= new;
                }
            }
            self.stats.packed_bytes += len;
        }
        for page in page..=last {
            let from = address.max(page * PAGE) - page * PAGE;
            let to = (address + len).min((page + 1) * PAGE) - page * PAGE;
            self.take(page, from as usize, to as usize, true);
        }
        self.stats.packed += 1;
    }

    /// A place in an arena for a page of committed title memory, at a
    /// submission: only one there is already (the list brings up the arenas
    /// it knew when it was recorded).
    fn place(&mut self, page: u64) -> bool {
        if page < 0x1_0000
            || page >= u64::from(MEMORY_PAGES) - 1
            || !crate::texture::committed((page * PAGE) as *const u8, PAGE as usize)
            || crate::shadow::in_buffer(page * PAGE, PAGE).is_some()
        {
            return false;
        }
        let Some((copy, at)) = self.free.pop() else {
            return false;
        };
        self.hold(page, at, copy);
        crate::watch::wrote(self.entry(page) as usize, 8);
        self.take(page, 0, PAGE as usize, true);
        true
    }

    /// Bytes `from..to` of `page` from title memory into its arena's page, at
    /// a submission; `whole`: sent whether they differ from those last taken
    /// or not.
    fn take(&mut self, page: u64, from: usize, to: usize, whole: bool) {
        let Some(held) = self.pages.get(&page) else { return };
        let shadow = &self.copies[held.copy];
        let base = held.at - shadow.span().0 as usize;
        let mut title = [0u8; PIECE];
        let mut at = from;
        while at < to {
            let n = (PIECE - at % PIECE).min(to - at);
            crate::host::copy_mapped(title.as_mut_ptr(), (page as usize * PAGE as usize + at) as *const u8, n, false);
            // SAFETY: a page of an arena of ours, under the mutex.
            let ours = unsafe { std::slice::from_raw_parts_mut((held.at + at) as *mut u8, n) };
            if *ours != title[..n] {
                ours.copy_from_slice(&title[..n]);
                crate::watch::wrote(held.at + at, n);
            }
            at += n;
        }
        if whole {
            shadow.resend(base + from, base + to);
        }
    }

    /// The pages shaders found empty since the last frame.
    fn take_faults(&mut self) {
        let mut pages = Vec::new();
        for i in 1..MEMORY_FAULTS as usize {
            // SAFETY: inside the faults' mapping; the GPU may be writing,
            // a number missed now is written again next frame.
            let at = unsafe { self.faults.1.add(i) };
            // SAFETY: as above.
            let page = unsafe { at.read_volatile() };
            if page != 0 {
                pages.push(u64::from(page));
                // SAFETY: as above.
                unsafe { at.write_volatile(0) };
            }
        }
        if pages.is_empty() {
            return;
        }
        pages.sort_unstable();
        let mut made = 0;
        for &page in &pages {
            if made < FAULTS && self.faulted < FAULTS_ALL && self.copy_of(page).is_none() && self.map(page) {
                made += 1;
                self.faulted += 1;
                self.bring(page, page + 1);
            }
        }
        self.stats.faults += pages.len() as u64;
        // Each is an access that read 0 or was dropped.
        static SAID: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let said = SAID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if said < 40 || said.is_multiple_of(100) {
            let some: Vec<String> = pages
                .iter()
                .step_by(pages.len().div_ceil(12))
                .map(|&p| format!("{:#x}", p * PAGE))
                .collect();
            tracing::warn!(
                frame = self.frame,
                pages = pages.len(),
                made,
                held = self.pages.len(),
                ?some,
                "title memory by address: shaders touched pages that had no copy (those accesses read 0 or were dropped); those that are the title's are there now"
            );
        }
    }

    /// The structures named in a list's upload memory around `address`
    /// (instances of a top-level build: a structure's address is their last
    /// 8 bytes): the first page of each.
    fn named_in_upload(&mut self, address: u64) {
        let Some((from, len)) = crate::upload::block(address as usize) else {
            return;
        };
        // SAFETY: the block is handed out and stays mapped; the title wrote
        // it before it recorded the dispatch.
        let words = unsafe { std::slice::from_raw_parts(from as *const u64, len / 8) };
        let mut seen = std::collections::HashSet::new();
        for &word in words {
            if word & 0xFF == 0 && (1 << 32..1 << 37).contains(&word) && seen.insert(word / PAGE) && self.map(word / PAGE) {
                let page = word / PAGE;
                self.bring(page, page + 1);
            }
        }
    }

    /// A small title buffer whose results come back, if its pages are
    /// windows of title memory itself (or a copy of the buffer's own).
    fn small(&mut self, list: &HostList, start: u64, len: u64) {
        let mut at = start;
        while at < start + len {
            let page = at / PAGE;
            let n = ((page + 1) * PAGE).min(start + len) - at;
            self.map(page);
            if let Some(k) = self.copy_of(page)
                && !self.pages.contains_key(&page)
            {
                self.copies[k].refresh(list, at, n);
                #[cfg(feature = "diagnostics")]
                crate::host::declared_by(at as usize, n as usize, 3);
                self.copies[k].gpu_writes(list, at, n);
                self.stats.back += 1;
            }
            at += n;
        }
    }

    /// Every copy is brought up to date when `list` is submitted, after what
    /// it deferred so far (once until a copy of the title's intervenes).
    fn bring_up(&self, list: &HostList) {
        let (start, len) = self.directory.1.span();
        self.directory.1.refresh(list, start, len);
        for shadow in &self.copies {
            let (start, len) = shadow.span();
            shadow.refresh(list, start, len);
        }
    }

    /// Every ten seconds or so, what was used.
    fn report(&mut self) {
        let now = std::time::Instant::now();
        let (since, frame) = *self.stats.from.get_or_insert((now, self.frame));
        if now.duration_since(since).as_secs_f64() < 10.0 {
            return;
        }
        let frames = self.frame.wrapping_sub(frame).max(1) as f64;
        let s = std::mem::take(&mut self.stats);
        tracing::info!(
            "title memory by address: {} pages ({} MB) in {} arenas, {} copies in all; per frame {:.0} dispatches that read, {:.0} that build, \
             {:.1} pages that had no copy, {:.1} pages turned down, {:.1} results brought back; {:.0} pages looked at, {:.1} KB changed, \
             {:.1} packed structures ({} there), {:.2} ms; {} windows; since the last line {} KB of packed structures came and {} of them went, \
             and pages for constants {}, views written {}, others {}, upload memory {}, faults in all {}",
            self.pages.len(),
            (self.pages.len() as u64 * PAGE) >> 20,
            self.arenas.len(),
            self.copies.len(),
            s.uses[0] as f64 / frames,
            s.uses[1] as f64 / frames,
            s.faults as f64 / frames,
            s.unknown as f64 / frames,
            s.back as f64 / frames,
            s.looked as f64 / frames,
            s.changed as f64 / 1024.0 / frames,
            s.packed as f64 / frames,
            self.packed.len(),
            s.micros as f64 / 1000.0 / frames,
            self.windows,
            s.packed_bytes >> 10,
            s.gone,
            s.came[0],
            s.came[1],
            s.came[2],
            s.came[3],
            self.faulted
        );
        self.stats.from = Some((now, self.frame));
    }
}

/// A list with this key ([`HostList::used`]) asks the builder for sizes.
const SIZES: usize = usize::MAX - (9 << 33);

/// Whether a submission that asks for the sizes of structures is waited for,
/// and landed, before the title goes on (`VICTORY_BVH_SIZES_WAIT=1`; off).
///
/// Off: it does not help. In run light-15, with it, 33 of 154 copies had the
/// slot's old size still (all of them recorded one frame after the size was
/// asked for) against 53 of 230 in light-11, 48 of 275 dynamic structures
/// were good, and the title asks in every frame, so every frame waits (109 ms
/// against 90). The title reads the slot before our submission of the asking
/// list is through, or reads it for another purpose; not known.
///
/// The title asks for a new structure's compacted size (a program of one
/// thread stores it into a small buffer), reads it a frame or two later
/// without looking whether it is there, takes memory of that size and copies
/// the structure into it. With submissions that land up to two frames late it
/// read what the slot held before, some other structure's size, and the copy
/// wrote over its neighbours: of 230 such copies of run light-11, 99 were
/// asked for two frames earlier and landed in that frame, none of those with
/// the right size.
fn sizes_wait() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_BVH_SIZES_WAIT").is_some_and(|v| v == "1"))
}

/// Whether a dispatch of the builder is its program of a structure's size:
/// one thread, the structure's address and what to store (1: the compacted
/// size) as constants, a small title buffer to store into.
fn asks_size(written: &[u64], constants: &[u32], threads: Option<[u32; 3]>) -> bool {
    threads == Some([1, 1, 1])
        && matches!(*constants, [_, hi, 1..=3, ..] if (1..0x20).contains(&hi))
        && written.first().is_some_and(|&at| crate::shadow::in_buffer(at, 1).is_some_and(|b| b.1 <= BACK))
}

/// Whether one of `lists` asks for sizes.
pub(crate) fn asked_sizes<'a>(lists: impl Iterator<Item = &'a HostList>) -> bool {
    let mut lists = lists;
    let asked = sizes_wait() && lists.any(|l| l.used(SIZES));
    if asked {
        static TIMES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = TIMES.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        if n.is_power_of_two() || n.is_multiple_of(100) {
            tracing::info!(n, frame = crate::frame::current(), "submissions that ask for structures' sizes, waited for");
        }
    }
    asked
}

/// [`fill`] of a page stores by address do not reach.
fn close(entry: *mut u32, n: usize, offset: u64) {
    fill(entry, n | MEMORY_READ_ONLY as usize, offset);
}

/// The offset first: a shader that sees the number sees it too.
fn fill(entry: *mut u32, n: usize, offset: u64) {
    // SAFETY: an entry of the directory, under the mutex.
    unsafe {
        entry.add(1).write_volatile(offset as u32);
        entry.write_volatile(n as u32 + 1);
    }
}

/// The bytes of a packed structure whose first two dwords these are: the
/// version (6) with the marks of a bottom level and of one that is packed,
/// and its size unpacked in 64-byte nodes.
fn packed_len(header: u64) -> Option<u64> {
    let (version, nodes) = (header as u32, header >> 32);
    let marks = version >> 16;
    // The header counts its nodes of 64 bytes without the last one: the
    // builder's own program of a structure's size stores one more.
    let len = (nodes + 1) * 64;
    (version & 0xFFFF == 6 && marks & 0x8100 == 0x8100 && nodes > 0 && len <= PACKED).then_some(len)
}

/// Puts what `title` holds into `ours` (what was last taken): the dwords that
/// differ. How many.
fn merge(title: &[u8; PIECE], ours: &mut [u8], never: bool) -> usize {
    if never {
        ours.copy_from_slice(title);
        return PIECE / 4;
    }
    if ours == title {
        return 0;
    }
    let mut changed = 0;
    for (t, o) in title.chunks_exact(4).zip(ours.chunks_exact_mut(4)) {
        if t != o {
            o.copy_from_slice(t);
            changed += 1;
        }
    }
    changed
}

/// Before a draw whose shader reads title memory by address: what its
/// root arguments name is made ready (`addresses`: its root views, of them
/// `written` those it writes; `constants`: its root constants, see the
/// module), every numbered copy
/// has its view in `heap`'s run and is brought up to date when `list` is
/// submitted. None if the host cannot hold it; the draw is then dropped.
pub(crate) fn ready(
    list: &HostList,
    heap: &HostHeap,
    addresses: &[u64],
    written: &[u64],
    constants: &[u32],
    geometry: &[u64],
    use_: Use,
    threads: Option<[u32; 3]>,
) -> Option<Ready> {
    let mut guard = MEMORY.lock().unwrap_or_else(|e| e.into_inner());
    if guard.is_none() {
        *guard = Memory::new();
    }
    let m = guard.as_mut()?;
    let frame = crate::frame::current();
    if m.frame != frame {
        m.frame = frame;
        m.refused.clear();
        m.take_faults();
        m.look_again();
        m.report();
        if dxr::on() {
            m.dxr_frame();
        }
        #[cfg(feature = "diagnostics")]
        m.trace.look(frame);
    }
    #[cfg(feature = "diagnostics")]
    if use_ != Use::Reads {
        m.trace.dispatch(frame, list, addresses, written, constants, threads);
    }
    let builds = use_ != Use::Reads;
    if builds && sizes_wait() && asks_size(written, constants, threads) {
        list.first_use(SIZES);
    }
    while builds && m.free.len() < SPARE && m.pages.len() + m.free.len() < MOST && m.another() {}
    m.stats.uses[usize::from(builds)] += 1;
    // Once per list and page named: the builder dispatches hundreds of times
    // a frame with the same arguments.
    const READ: usize = usize::MAX - 0x6d;
    let named = |kind: usize, at: u64| list.first_use(usize::MAX - (kind << 26) - (at / PAGE) as usize);
    // A title address in two constants (low dword, then bits 32 to 39), or
    // in one in 256-byte units (the builder's alone). A constant that only
    // looks like one names memory nothing reads; one that is not the title's
    // memory is refused.
    let held = |m: &Memory| m.pages.len() as u64;
    let before = held(m);
    for (i, &c) in constants.iter().enumerate() {
        if let Some(&high) = constants.get(i + 1)
            && (1..=0x1F).contains(&high)
            && c >= 0x1_0000
        {
            let at = u64::from(c) | u64::from(high) << 32;
            if named(1, at) {
                m.ready(at, 1);
            }
        }
        if builds && (0x0100_0000..0x2000_0000).contains(&c) {
            let at = u64::from(c) << 8;
            if named(1, at) {
                m.ready(at, 1);
            }
        }
    }
    let mut came = [held(m) - before, 0, 0, 0];
    // The title memory a builder program reads through the geometry
    // descriptions it is given (`crate::records`). LeafRehydration rebuilds a
    // static structure's triangles from the title's own index and vertex
    // buffers, once, through pointers in those records: a page they name that
    // had no copy yet read as zeros, and the triangles made of it stayed one
    // point for the structure's life (capture run 20261127-171218: 2,773,
    // 2,504 and 410 such pages in the three frames of its first 300
    // unpackings, 128 a frame made afterwards; in its dump 3,800 of 8,000
    // element nodes compared were all one point). So they are made ready
    // before it runs, as its views are.
    let mut brought = false;
    for &at in geometry {
        if list.first_use(usize::MAX - (7 << 33) - (at >> 4 & 0x7FFF_FFFF) as usize) {
            let before = held(m);
            m.ready_geometry(at);
            came[2] += held(m) - before;
            brought = true;
        }
    }
    if brought {
        m.bring_up(list);
    }
    for &at in addresses.iter().filter(|&&a| a >= 1 << 32) {
        // (Several structures to a page.)
        let mut packed = false;
        if builds
            && list.first_use(usize::MAX - (3 << 33) - (at >> 8) as usize)
            && crate::shadow::in_buffer(at, 1).is_none()
            && m.possible(at / PAGE)
        {
            // What it needs, as far as it is known now; the rest when the
            // list is submitted ([`Memory::land`]).
            if let Some((len, _)) = m.packed_at(at) {
                packed = true;
                for page in at / PAGE..=(at + len - 1) / PAGE {
                    let new = m.copy_of(page).is_none();
                    if m.map(page)
                        && new
                        && let Some(held) = m.pages.get_mut(&page)
                    {
                        held.theirs = true;
                    }
                }
                m.bring(at / PAGE, (at + len - 1) / PAGE + 1);
            }
            list.defer_host(move || {
                if let Some(m) = MEMORY.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
                    m.land(at);
                }
            });
            m.bring_up(list);
        }
        if builds && written.contains(&at) && list.first_use(usize::MAX - (4 << 33) - (at >> 2 & 0x7FFF_FFFF) as usize) {
            if let Some((start, len)) = crate::shadow::in_buffer(at, 1)
                && len <= BACK
                && use_ == (Use::Builds { back: true })
                && list.first_use(usize::MAX - (5 << 33) - (start >> 2 & 0x7FFF_FFFF) as usize)
            {
                m.small(list, start, len);
            }
            m.opens(at);
            if !packed && crate::shadow::in_buffer(at, 1).is_none() {
                #[cfg(feature = "diagnostics")]
                {
                    let built = m.dump.built.entry(at).or_insert((frame, frame, 0));
                    (built.1, built.2) = (frame, built.2 + 1);
                }
                let before = held(m);
                m.ready(at, BUILT);
                came[1] += held(m).saturating_sub(before);
            }
        }
        if packed || !named(2, at) {
            continue;
        }
        let before = held(m);
        match crate::shadow::in_buffer(at, 1) {
            Some(_) => {
                m.ready(at, 1);
                m.named_in_upload(at);
                came[3] += held(m).saturating_sub(before);
            }
            None => {
                m.ready(at, if builds { VIEW } else { PAGE });
                came[2] += held(m).saturating_sub(before);
            }
        }
    }
    for (sum, n) in m.stats.came.iter_mut().zip(came) {
        *sum += n;
    }
    let key = std::ptr::from_ref(heap) as usize;
    let (base, made) = match m.heaps.get(&key) {
        Some(&run) => run,
        None => {
            // Held for the heap's life: not given back with a list's slots.
            // (And native ray tracing's table and structures' views,
            // `victory_shader::emit::MEMORY_RUN`.)
            let run = COPIES + 3 + victory_shader::emit::DXR_STRUCTURES;
            let base = heap.dynamic(run)?;
            // Every slot of the run: a number shows no view of another kind.
            // (A structure's slot a view of no acceleration structure: a ray
            // query against it misses.)
            let structures = victory_shader::emit::MEMORY_RUN + 1..victory_shader::emit::MEMORY_RUN + 1 + victory_shader::emit::DXR_STRUCTURES;
            for n in 0..run {
                if structures.contains(&n) && dxr::enter_early().is_none() {
                    heap.null_accel_srv_at(base + n);
                } else {
                    heap.null_raw_uav_at(base + n);
                }
            }
            (base, 0)
        }
    };
    for (n, shadow) in m.copies.iter().enumerate().skip(made as usize) {
        let (_, len) = shadow.span();
        heap.raw_uav_at(base + 1 + n as u32, &shadow.buffer, 0, len);
    }
    m.heaps.insert(key, (base, m.copies.len() as u32));
    if use_ == Use::Reads && dxr::on() {
        m.dxr_views(heap, base);
    }
    // Every copy by every list that has come to by-address memory, whoever
    // numbered it: an arena another list made while this one was being
    // recorded was brought up to date by that list's submission alone, which
    // may run after this one's (structures unpacked from zeros, and then
    // written over by the arena's first upload: in frames that stream much in
    // half of them, run light-8).
    let first_read = list.first_use(READ);
    if list.first_use(usize::MAX - (6 << 33) - m.copies.len()) {
        m.bring_up(list);
    }
    if first_read {
        list.keep(&m.faults.0);
        #[cfg(feature = "diagnostics")]
        m.dump(list, addresses);
    }
    #[cfg(feature = "diagnostics")]
    if use_ == Use::Reads {
        m.dump.walked(addresses);
    }
    // After the pages are brought up, so the reads see this frame's.
    let clobbered = use_ == Use::Reads && dxr::on() && m.dxr_tops(list, addresses);
    let directory = &m.directory.1;
    Some(Ready {
        directory: directory.gpu(directory.span().0),
        faults: m.faults.2,
        base,
        clobbered,
    })
}

/// A gather of the native builder's (`crate::rtas::card_native`): `count`
/// element nodes written to title memory at `to` on the card, their
/// vertices read from the title's vertex memory in `vertices` (its copy) by
/// the offsets in `plan` (from title address `base`; the largest `most`).
pub(crate) struct Gather {
    pub(crate) to: u64,
    pub(crate) count: u32,
    pub(crate) plan: ID3D12Resource,
    pub(crate) vertices: Arc<Shadow>,
    pub(crate) base: u64,
    pub(crate) most: u32,
}

// SAFETY: a buffer of the host's (free-threaded) and a copy shared between
// threads as it is.
unsafe impl Send for Gather {}

/// Records `gather` on `list` at its place (`crate::list::work_here`);
/// whether every node's bytes went to an arena's copy.
pub(crate) fn card_elements(list: &HostList, gather: &Gather) -> bool {
    let mut guard = MEMORY.lock().unwrap_or_else(|e| e.into_inner());
    if guard.is_none() {
        *guard = Memory::new();
    }
    let Some(m) = guard.as_mut() else { return false };
    let done = m.dxr_gather(list, gather);
    m.dxr.gathered[0] += u64::from(done);
    m.dxr.gathered[1] += if done { u64::from(gather.count) } else { 0 };
    m.dxr.gathered[3] += u64::from(!done);
    done
}

/// Records on `list` at its place the copy of `bytes` of title memory from
/// `from` to `to`, both in arenas' copies on the card (a structure whose
/// elements only the card holds, copied by the title).
pub(crate) fn card_copy(list: &HostList, to: u64, from: u64, bytes: u64) -> bool {
    let mut guard = MEMORY.lock().unwrap_or_else(|e| e.into_inner());
    let Some(m) = guard.as_mut() else { return false };
    let done = m.dxr_copy(list, to, from, bytes);
    m.dxr.gathered[2] += u64::from(done);
    m.dxr.gathered[3] += u64::from(!done);
    done
}

/// A buffer in the PC's memory the card reads, with the CPU's mapping (the
/// native builder's plans).
pub(crate) fn shared_buffer(bytes: u64) -> Option<(ID3D12Resource, usize)> {
    host().and_then(|h| h.shared_buffer(bytes)).map(|(buffer, data)| (buffer, data as usize))
}

/// Title memory the layer itself wrote for the title on the CPU (the native
/// builder's structures, `crate::rtas`): its pages are taken as views'
/// are, and what was written goes up to their copies.
pub(crate) fn title_wrote(start: u64, len: u64) {
    let mut guard = MEMORY.lock().unwrap_or_else(|e| e.into_inner());
    if guard.is_none() {
        *guard = Memory::new();
    }
    if let Some(m) = guard.as_mut() {
        m.ready(start, len);
    }
}

/// Native ray tracing (`VICTORY_DXR=1`; off by default; `DXR-PLAN.md`), its
/// second stage: host (DXR) structures built from the title's own.
///
/// - The top levels the trace programs are given and the bottom levels their
///   instances name are read back from the card a few frames later (their
///   headers and the top levels' instance lists).
/// - A bottom level of the plain kind (version 6, not packed) is built as a
///   host bottom level: its elements (64 bytes, up to four triangles each)
///   are copied from title memory to a buffer of ours, a pass of ours turns
///   each into four triangles (an absent one's vertices NaN: inactive), and
///   the host builds the structure from them. Rehydrated (static) ones once
///   (again if their header changes), the others every [`dxr::AGAIN`]
///   frames; at most [`dxr::BUILDS`] a frame.
/// - A top level is built from its instances whose bottom level is built,
///   with each instance's transform the inverse of the title's world to
///   object matrix, its id and mask as the title has them.
///
/// Nothing traces them yet (that is stage 4: the trace programs' walk led
/// through them): a line every ten seconds says what is built and its cost.
/// The work goes on the list of the trace dispatch it comes before (so it
/// sees this frame's title memory), and replaces the list's compute root
/// signature and pipeline ([`Ready::clobbered`]).
mod dxr {
    use super::*;

    pub(super) fn on() -> bool {
        static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        // (The live menus switch it, `crate::live`.)
        crate::live::get(crate::live::DXR)
            .map_or_else(|| *ON.get_or_init(|| std::env::var_os("VICTORY_DXR").is_some_and(|v| v == "1")), |v| v >= 0.5)
    }

    /// Bytes read of a top level: its header and instance list (1543
    /// instances of 128 bytes from 0x8580 in the biggest seen).
    pub(super) const TOP_BYTES: u64 = 512 << 10;
    /// Bottom levels whose headers are read at a time.
    pub(super) const BOTTOMS: usize = 4096;
    /// Frames before a read is looked at.
    pub(super) const LATER: u64 = 3;
    /// Frames before a bottom level that is not static is built again.
    pub(super) const AGAIN: u64 = 60;
    /// Host bottom levels built at most a frame.
    pub(super) const BUILDS: usize = 4;
    /// The most elements of a bottom level built (bigger ones are left out;
    /// the biggest in the dumps had some 30,000).
    pub(super) const MOST_ELEMENTS: u32 = 1 << 16;
    /// The most bytes of host bottom levels kept: more are not built.
    pub(super) const MOST_KEPT: u64 = 512 << 20;
    /// No building while the card's memory in use is over this share of its
    /// budget: a run that built on with it full (8.1 of 9.6 GB) lost the
    /// device two minutes in, the submitting thread stalled 4.8 s first
    /// (with headers that claimed billions of elements built; those are not
    /// any more). At 85% the top level held 54 of 1,512 instances (run
    /// 20261126-180001).
    pub(super) const CARD_SHARE: f64 = 0.93;
    const _: () = assert!(victory_shader::emit::MEMORY_RUN == super::COPIES + 1);
    /// Frames between a structure's build being recorded and its view and
    /// table entry being set: a list recorded or still running before the
    /// build finished would trace memory that holds no structure yet (the
    /// card hangs: run 20261126-165057, the driver's event 153).
    pub(super) const PUBLISH_AFTER: u64 = 10;
    /// Frames after which a view heap no list has asked the views of has no
    /// list left recorded or running: a table entry waits for the views of
    /// the heaps asked for since ([`Memory::dxr_enter`]). The table is read
    /// by the card as it is written and a view is written when its heap is
    /// next used: an entry set with its structure showed lists already
    /// recorded or running a number whose slot held no structure yet (a new
    /// number's, a view of nothing of another kind), and a ray query against
    /// that is not defined. Six of the seven device losses of 2026-11-27
    /// came 6 to 31 seconds into the world, while the first numbers were
    /// given out, in a trace dispatch each time (DXGI_ERROR_DEVICE_HUNG).
    pub(super) const ENTER_AFTER: u64 = 60;

    /// `VICTORY_DXR_ENTER_EARLY=<frames>` (a test of the cause, off by
    /// default): table entries set with their structures as before
    /// [`ENTER_AFTER`], the structures' slots views of nothing of another
    /// kind until written, and the views written only every so many frames:
    /// the window in which a list reads a number whose slot holds no
    /// structure, made wide.
    pub(super) fn enter_early() -> Option<u64> {
        static EARLY: std::sync::OnceLock<Option<u64>> = std::sync::OnceLock::new();
        *EARLY.get_or_init(|| {
            let frames = std::env::var("VICTORY_DXR_ENTER_EARLY").ok()?.parse::<u64>().ok().filter(|f| *f > 0)?;
            tracing::warn!("VICTORY_DXR_ENTER_EARLY={frames}: the guided walk's table entries are set before their views, and the views written every {frames} frames (a test: the card may hang)");
            Some(frames)
        })
    }
    /// Frames a structure let go is kept: lists in flight and the host top
    /// levels made before it was replaced may still use it.
    pub(super) const KEEP_AFTER: u64 = 120;
    /// Frames of building after which a top level not built again is taken
    /// out of the guided walk's table (`Memory::dxr_frame`).
    pub(super) const STALE_TOP: u64 = 1800;
    /// Frames a host bottom level no top level's instances have named after
    /// which it may be let go for room ([`Memory::dxr_let_go`]).
    pub(super) const UNSEEN: u64 = 600;
    /// Marks the card can have written and not yet been looked at: the
    /// marks' slots ([`Dxr::landed`]).
    pub(super) const MARKS: u32 = 4096;
    /// Frames after which what waits for a mark that never came (its list was
    /// not run) is given up.
    pub(super) const GIVE_UP: u64 = 1800;

    pub(super) enum What {
        Top(u64),
        Bottoms(Vec<u64>),
        /// The guided walk's counters.
        Counters,
    }

    pub(super) struct Pending {
        pub(super) due: u64,
        pub(super) what: What,
        pub(super) buffer: ID3D12Resource,
        pub(super) data: usize,
        /// Bytes that came from a copy, by offset in the buffer.
        pub(super) got: Vec<(u64, u64)>,
        /// The mark the card writes once the list that copies them has run
        /// ([`Dxr::landed`]).
        pub(super) token: u32,
        /// The buffer's bytes as asked of the pool, it goes back to once
        /// looked at.
        pub(super) size: u64,
    }

    /// What is known of a bottom level.
    #[derive(Clone, Copy, Default)]
    pub(super) struct Bottom {
        pub(super) header: [u32; 16],
        pub(super) frame: u64,
    }

    /// An instance of a top level: the title's world to object matrix
    /// (dwords 0-11), its id and mask (12), flags (13), and its bottom level.
    #[derive(Clone, Copy)]
    pub(super) struct Instance {
        pub(super) words: [u32; 14],
        /// Record dwords 16-27: the world-to-structure transform the walk
        /// takes the ray into the bottom level with (dump of run
        /// 20261127-184716, 1,034 records against the title's instance
        /// descriptions): the translation, then the 3x3 rows, of
        /// S * inverse(T), T the instance's own transform (dwords 0-11, object
        /// to world, as the title gave it) and S the bottom level's node
        /// space (`crate::rtas::Space`; none for a dynamic one).
        pub(super) inverse: [u32; 12],
        pub(super) bottom: u64,
        /// Its number in the title's list (the host instance's id: the
        /// guided walk's instance node, `victory_shader` `bvh_guided`).
        pub(super) record: u32,
        /// Its root pointer (record dword 14).
        pub(super) root_pointer: u32,
    }

    /// A top level as last read: the frame, its instances and how many it
    /// says it has.
    #[derive(Default)]
    pub(super) struct Top {
        pub(super) read: u64,
        pub(super) instances: Vec<Instance>,
        pub(super) count: u32,
        /// Its header's root node pointer and instance list offset.
        pub(super) root: [u32; 2],
        /// Its host top level and the instances in it.
        pub(super) built: Option<(ID3D12Resource, usize)>,
        /// The frame its host top level was built.
        pub(super) built_frame: u64,
    }

    /// A host bottom level: the title's header it was built from, the frame,
    /// its structure and triangles.
    pub(super) struct Built {
        pub(super) header: [u32; 16],
        pub(super) frame: u64,
        pub(super) structure: ID3D12Resource,
        pub(super) bytes: u64,
        pub(super) triangles: u64,
        /// A host top level of it alone (identity, mask 0xff), for the
        /// guided walk at its root.
        pub(super) single: Option<ID3D12Resource>,
        /// The frame an instance of a top level last named it.
        pub(super) seen: u64,
    }

    /// The element pass.
    pub(super) struct Pass {
        pub(super) root: ID3D12RootSignature,
        pub(super) pso: ID3D12PipelineState,
        pub(super) device: ID3D12Device5,
    }

    #[derive(Default)]
    pub(super) struct Dxr {
        pub(super) pending: Vec<Pending>,
        pub(super) tops: HashMap<u64, Top>,
        pub(super) bottoms: HashMap<u64, Bottom>,
        pub(super) reported: Option<std::time::Instant>,
        /// Tops read, tops looked at, bottom headers read, not there.
        pub(super) stats: [u64; 4],
        /// `None` until first wanted; `Some(None)` if it cannot be made.
        pub(super) pass: Option<Option<Pass>>,
        /// The gather pass ([`GATHER_CS`]), as `pass`.
        pub(super) gather: Option<Option<Pass>>,
        /// Since the last line: gathers recorded, their elements, structure
        /// copies on the card, and those of either that could not be (no
        /// pass, vertices outside their copy, pages not in an arena).
        pub(super) gathered: [u64; 4],
        pub(super) built: HashMap<u64, Built>,
        /// Host bottom levels whose build is recorded and may not have run:
        /// the frame each may be used from ([`PUBLISH_AFTER`]) and its
        /// address. Lists are recorded on several threads and run in the
        /// order the title submitted them, so a build recorded on one list
        /// may run after a list recorded later: a host top level made on that
        /// one named a bottom level that was not built yet, and the trace
        /// program that walked it hung the card (run 20261127-151722: device
        /// removed 110 s in, while 500 bottom levels were built in 12 s; the
        /// card had stopped in a trace dispatch). Until then `built` holds
        /// the one before, if any, which is what the top levels name.
        pub(super) building: Vec<(u64, u64, Built, u32)>,
        /// Since the last line: bottom levels built, their triangles, top
        /// levels built, the time recording them took (us), and the most
        /// scratch one took.
        pub(super) period: [u64; 5],
        /// Bottom levels left out: too big, or failed.
        pub(super) left_out: u64,
        /// Frames building waited for the card's memory.
        pub(super) card_full: u64,
        /// The table of host structures the guided walk looks in
        /// (`victory_shader::emit::MEMORY_RUN`): its buffer and mapping.
        pub(super) table: Option<(ID3D12Resource, usize)>,
        /// Structure address -> its number (its view's place in the run).
        pub(super) numbers: HashMap<u64, u32>,
        /// Numbers never given out start here.
        pub(super) next_number: u32,
        /// Numbers given back (their bottom level was let go) and the frame
        /// each may be given out again: lists recorded before may still look
        /// at the view.
        pub(super) free_numbers: Vec<(u64, u32)>,
        /// Since the last line: structures not entered in the table for want
        /// of a number, and for want of a place among their key's four.
        pub(super) not_entered: [u64; 2],
        /// Host bottom levels let go for room, since the last line.
        pub(super) let_go: u64,
        /// The last frame host top levels were built: what "not named for a
        /// while" counts against ([`Memory::dxr_let_go`]). With the switch
        /// at 0 or 1 nothing is built and nothing is named, and counted in
        /// frames every bottom level looked unnamed when it went back to 2:
        /// 744 of 772 were let go within seconds (run 20261127-163117).
        pub(super) tops_built_frame: u64,
        /// The marks the card writes after the steps of a list of ours have
        /// run ([`Memory::dxr_mark`]): a buffer the CPU reads, a dword a
        /// mark. What a list builds or reads is used only once its mark is
        /// there. The table the walks look in is written by the CPU and holds
        /// at once for every walk on the card, so a structure entered in it
        /// before its build had run was walked as whatever lay in its memory:
        /// "ten frames later" was not later enough when a submission took
        /// 4.7 s to prepare (run 20261127-161146, the card lost in a trace
        /// dispatch 96 s in, with the first top levels just entered; the same
        /// at 110 and 123 s in runs 20261127-151722 and -152214). Reads of
        /// headers looked at before their copy ran read zeros, which made top
        /// levels "not a top level" for the rest of the run and bottom levels
        /// "not a version-6 header" (25 of 0x00000000 in run -155346).
        pub(super) marks: Option<(ID3D12Resource, usize)>,
        /// The mark of what is being recorded now (0 none), the last given,
        /// and whether anything was recorded under the one now.
        pub(super) token: u32,
        pub(super) last_token: u32,
        pub(super) token_used: bool,
        /// Since the last line: what was given up waiting for its mark.
        pub(super) given_up: u64,
        /// Buffers kept for builds and reads ([`Pool`]).
        pub(super) pool: Pool,
        /// Since the last line, microseconds of the building on the recording
        /// thread: making buffers (and asking their sizes), recording the
        /// reads of elements, recording the element pass and the build,
        /// making a bottom level's own top level, and the top levels.
        pub(super) cost: [u64; 5],
        /// Each number's acceleration structure and when it was set.
        pub(super) structures: Vec<Option<(ID3D12Resource, u64)>>,
        /// Bumped whenever a number's structure is set.
        pub(super) generation: u64,
        /// Per view heap: when its views were last written.
        pub(super) views: HashMap<usize, u64>,
        /// Per view heap: the frame its views were last asked for (a list
        /// that reads structures was recorded with it).
        pub(super) views_asked: HashMap<usize, u64>,
        /// Structures whose number has a new structure and whose table entry
        /// waits for the views ([`ENTER_AFTER`]): the generation that set
        /// it, their address, kind and root.
        pub(super) entering: Vec<(u64, u64, u32, [u32; 2])>,
        /// Since the last line: table entries set, the frames they waited
        /// for their views in all, and the most one waited.
        pub(super) entered: [u64; 3],
        /// The frame each generation still waiting was set in.
        pub(super) generation_frames: HashMap<u64, u64>,
        /// Structures built but not published yet: the frame they may be
        /// ([`PUBLISH_AFTER`]), their address, kind, root and offset.
        pub(super) unpublished: Vec<(u64, u64, u32, [u32; 2], ID3D12Resource, u32)>,
        /// Structures let go and the frame: kept [`KEEP_AFTER`] frames for
        /// the lists and the top levels that may still use them.
        pub(super) retired: Vec<(u64, ID3D12Resource)>,
        /// Published top levels (address) and the frame each was built: a
        /// top level names its bottom levels by address, so a bottom level let
        /// go is kept while a top level built before it went is published.
        /// With the live switch at 0 or 1 nothing is built, the top levels age,
        /// and the bottom levels they name were freed after [`KEEP_AFTER`]
        /// frames anyway: switched back to 2, the walk's first ray queries went
        /// through freed memory (run 20261126-195949: 0, then 1 and 2, and the
        /// card was lost within two seconds, NVIDIA events 13 and 153).
        /// (Top levels not built again in [`STALE_TOP`] frames of building are
        /// taken out of the table, so a top level the title no longer walks
        /// does not keep every bottom level let go after it.)
        pub(super) top_built: HashMap<u64, (u64, [u32; 2])>,
        /// Host top levels that are published no more (one newer took its place
        /// in the table, or it was taken out): the frame each was built and
        /// the frame until which it still counts as published. A dispatch
        /// recorded before may be walking it on the card for some frames yet,
        /// and the bottom levels it names were freed in the frame it went:
        /// the switch back to 2 after a minute at 0 took every stale top level
        /// out and freed their bottom levels at once, with the card in a trace
        /// dispatch (run 20261127-154107: lost three seconds after the switch,
        /// NVIDIA event 153; a switch back within seconds had held in run
        /// 20261127-152631).
        pub(super) top_gone: Vec<(u64, u64)>,
        /// The guided walk's counters on the card (queries, hits, at a top
        /// level: `victory_shader::emit::DXR_STRUCTURES`), the frame they
        /// were last read and their values then, and their growth since the
        /// last line.
        pub(super) counters: Option<ID3D12Resource>,
        /// Bottom levels published under a root pointer their instances use
        /// (dword 14, with its marks) besides their header's: (address,
        /// pointer) and the structure it was published with.
        pub(super) instance_roots: HashMap<(u64, u32), ID3D12Resource>,
        pub(super) counted: (u64, [u32; 3]),
        pub(super) counted_period: [u64; 3],
        /// The frame the card's memory was last asked about, and whether it
        /// had room.
        pub(super) room: (u64, bool),
    }

    /// A version-6 header of a bottom level of the two plain kinds the
    /// dumps have (0x8002 rehydrated, 0x8004; not 0x8102, packed), with a
    /// list of elements that fits in it. (Records past a top level's live
    /// instances name stale memory: headers that claim billions.)
    pub(super) fn usable_bottom(h: &[u32; 16]) -> bool {
        h[0] & 0xffff == 6 && matches!(h[0] >> 16, 0x8002 | 0x8004) && h[3] > 0 && h[3] <= MOST_ELEMENTS && h[13] < 1 << 24
    }

    /// Why a bottom level's header is not [`usable_bottom`], as a place in
    /// [`WHY_NOT`]: the share of a scene the host structures lack was known
    /// (a third to a half of its bottom levels, runs of 2026-11-26) and not
    /// what kept it out.
    pub(super) fn why_not(h: &[u32; 16]) -> usize {
        if h[0] & 0xffff != 6 {
            0
        } else if h[0] >> 16 & 0x8100 == 0x8100 {
            1
        } else if !matches!(h[0] >> 16, 0x8002 | 0x8004) {
            2
        } else if h[3] == 0 {
            3
        } else if h[3] > MOST_ELEMENTS {
            4
        } else {
            5
        }
    }
    pub(super) const WHY_NOT: [&str; 6] =
        ["not a version-6 header", "still packed", "other marks", "no elements", "too many elements", "elements too far in"];

    /// The kind a host bottom level is entered in the walk's table with: one
    /// the title made once holds every triangle it will have, and a ray that
    /// misses the host's has missed it (`VICTORY_DXR_WHOLE=0`: the title's own
    /// walk follows every miss, as before).
    pub(super) fn bottom_kind(h: &[u32; 16]) -> u32 {
        static WHOLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        let whole = *WHOLE.get_or_init(|| std::env::var_os("VICTORY_DXR_WHOLE").is_none_or(|v| v != "0"));
        if whole && static_bottom(h) { victory_shader::emit::DXR_BOTTOM_WHOLE } else { victory_shader::emit::DXR_BOTTOM }
    }

    /// A bottom level the title made once (rehydrated): built once.
    pub(super) fn static_bottom(h: &[u32; 16]) -> bool {
        // (Or one the layer built once itself, `crate::rtas::once`.)
        h[0] >> 16 & 2 != 0 || crate::rtas::made_once(h)
    }

    /// The buffers a build needs only until the card has run it (the
    /// elements read, the triangles, the scratch, a top level's instances)
    /// and the buffers reads come back in, kept for the next build instead of
    /// made and let go each time. Each build made seven buffers and the
    /// making took the recording thread 1 to 6 ms a build, under the lock of
    /// title memory by address that every recording thread takes (run
    /// 20261127-163117: 144 builds, 930 ms in ten seconds), and each made one
    /// was memory the card's budget had to find room for anew.
    ///
    /// A buffer is lent under the mark of the list it is recorded on and is
    /// free again once the card has written that mark ([`Dxr::landed`]); a
    /// read's buffer once the read has been looked at. Sizes are powers of two
    /// from 64 KiB; at most [`POOL_MOST`] bytes of each kind are kept free.
    #[derive(Default)]
    pub(super) struct Pool {
        /// Free buffers by (in the PC's memory, the size's power of two), with
        /// the CPU's mapping of those in the PC's memory.
        free: HashMap<(bool, u32), Vec<(ID3D12Resource, usize)>>,
        /// Bytes free of each kind: in the card's memory, in the PC's.
        free_bytes: [u64; 2],
        /// Lent under a mark: the mark, the frame, and the buffer.
        lent: Vec<(u32, u64, bool, u32, ID3D12Resource, usize)>,
        /// Since the last line: buffers made, buffers taken again.
        pub(super) made: u64,
        pub(super) reused: u64,
    }

    /// See [`Pool`].
    pub(super) const POOL_MOST: u64 = 256 << 20;
    const POOL_LEAST: u32 = 16;

    impl Pool {
        fn class(size: u64) -> u32 {
            size.max(1 << POOL_LEAST).next_power_of_two().trailing_zeros()
        }

        /// A buffer of at least `size` bytes, in the PC's memory (`shared`,
        /// with its mapping) or the card's, in the common state.
        pub(super) fn take(&mut self, device: &ID3D12Device5, shared: bool, size: u64) -> Option<(ID3D12Resource, usize)> {
            let class = Self::class(size);
            if let Some(b) = self.free.get_mut(&(shared, class)).and_then(Vec::pop) {
                self.free_bytes[usize::from(shared)] -= 1 << class;
                self.reused += 1;
                return Some(b);
            }
            self.made += 1;
            if shared {
                let (buffer, data) = host()?.shared_buffer(1 << class)?;
                Some((buffer, data as usize))
            } else {
                Some((card_buffer(device, 1 << class, D3D12_RESOURCE_STATE_COMMON)?, 0))
            }
        }

        /// [`Pool::take`], the buffer free again once the card has written
        /// mark `token`.
        pub(super) fn lend(&mut self, device: &ID3D12Device5, shared: bool, size: u64, token: u32, frame: u64) -> Option<(ID3D12Resource, usize)> {
            let (buffer, data) = self.take(device, shared, size)?;
            self.lent.push((token, frame, shared, Self::class(size), buffer.clone(), data));
            Some((buffer, data))
        }

        /// A buffer of `size` bytes taken from the pool is free again.
        pub(super) fn give_back(&mut self, shared: bool, size: u64, buffer: ID3D12Resource, data: usize) {
            let class = Self::class(size);
            let kind = usize::from(shared);
            if self.free_bytes[kind] + (1 << class) > POOL_MOST {
                return;
            }
            self.free_bytes[kind] += 1 << class;
            self.free.entry((shared, class)).or_default().push((buffer, data));
        }

        /// The buffers lent under marks the card has written, free again;
        /// those whose mark never came by [`GIVE_UP`] frames, let go.
        pub(super) fn settle(&mut self, landed: impl Fn(u32) -> bool, frame: u64) {
            let lent = std::mem::take(&mut self.lent);
            for (token, at, shared, class, buffer, data) in lent {
                if landed(token) {
                    self.give_back(shared, 1 << class, buffer, data);
                } else if at + GIVE_UP > frame {
                    self.lent.push((token, at, shared, class, buffer, data));
                }
            }
        }
    }

    impl Dxr {
        /// The mark of what is being recorded now, noted as used.
        pub(super) fn take_token(&mut self) -> u32 {
            self.token_used = self.token != 0;
            self.token
        }

        /// Whether the card wrote mark `token` (0: no mark to wait for).
        pub(super) fn landed(&self, token: u32) -> bool {
            if token == 0 {
                return true;
            }
            let Some((_, data)) = self.marks else { return false };
            // SAFETY: the marks' mapping holds `MARKS` dwords; the card writes
            // them, so read as they are now.
            unsafe { (data as *const u32).add((token % MARKS) as usize).read_volatile() == token }
        }
    }

    impl Pending {
        /// Dword `n` of the read, if it came.
        pub(super) fn word(&self, n: u64) -> Option<u32> {
            let at = 4 * n;
            if !self.got.iter().any(|&(from, len)| from <= at && at + 4 <= from + len) {
                return None;
            }
            // SAFETY: inside the read's mapping, which came frames ago.
            Some(unsafe { (self.data as *const u32).add(n as usize).read() })
        }
    }

    /// Four slots an element: its triangles' three vertices, the slot's
    /// corners by the triangle's id byte (`bvh_flat`'s tests check it
    /// against the title's own walk on dumped structures).
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

    /// The host's device, for ray tracing.
    pub(super) fn device() -> Option<ID3D12Device5> {
        windows::core::Interface::cast(&host()?.device).ok()
    }

    /// The native builder's pass for meshes whose vertices only the card
    /// holds (`crate::rtas::card_native`): element `e`'s node from its plan
    /// (32 bytes: the byte offsets of its four vertices from `base` in the
    /// title's vertex memory, all ones for none, then the node's dwords 12
    /// to 15).
    pub(super) const GATHER_CS: &str = r#"
#define RS "RootConstants(num32BitConstants=2, b0), SRV(t0), SRV(t1), UAV(u0)"
cbuffer C : register(b0) { uint base; uint count; };
ByteAddressBuffer vertices : register(t0);
ByteAddressBuffer plan : register(t1);
RWByteAddressBuffer nodes : register(u0);
[RootSignature(RS)]
[numthreads(64, 1, 1)]
void main(uint3 id : SV_DispatchThreadID) {
    uint e = id.x;
    if (e >= count) return;
    uint4 v = plan.Load4(32u * e);
    uint4 w = plan.Load4(32u * e + 16u);
    [unroll] for (uint k = 0u; k < 4u; ++k) {
        uint3 p = uint3(0x7fc00000u, 0x7fc00000u, 0x7fc00000u);
        if (v[k] != 0xffffffffu) p = vertices.Load3(base + v[k]);
        nodes.Store3(64u * e + 12u * k, p);
    }
    nodes.Store4(64u * e + 48u, w);
}
"#;

    pub(super) fn make_pass() -> Option<Pass> {
        make_pass_of(ELEMENTS_CS)
    }

    pub(super) fn make_pass_of(source: &str) -> Option<Pass> {
        let host = host()?;
        let made = (|| -> windows::core::Result<Pass> {
            let code = crate::host::compile(source, windows::core::s!("main"), windows::core::s!("cs_5_1"))?;
            let device: ID3D12Device5 = windows::core::Interface::cast(&host.device)?;
            // SAFETY: the compiled bytes, with the root signature in them.
            unsafe {
                let root: ID3D12RootSignature = host.device.CreateRootSignature(0, &code)?;
                let pso: ID3D12PipelineState = host.device.CreateComputePipelineState(&D3D12_COMPUTE_PIPELINE_STATE_DESC {
                    pRootSignature: std::mem::transmute_copy(&root),
                    CS: D3D12_SHADER_BYTECODE { pShaderBytecode: code.as_ptr().cast(), BytecodeLength: code.len() },
                    ..Default::default()
                })?;
                Ok(Pass { root, pso, device })
            }
        })();
        match made {
            Ok(pass) => Some(pass),
            Err(e) => {
                tracing::warn!(error = %e, "native ray tracing: the element pass could not be made; nothing is built");
                None
            }
        }
    }

    /// A buffer in the card's own memory, in `state`.
    pub(super) fn card_buffer(device: &ID3D12Device5, size: u64, state: D3D12_RESOURCE_STATES) -> Option<ID3D12Resource> {
        let heap = D3D12_HEAP_PROPERTIES { Type: D3D12_HEAP_TYPE_DEFAULT, ..Default::default() };
        let desc = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
            Width: size.max(256),
            Height: 1,
            DepthOrArraySize: 1,
            MipLevels: 1,
            SampleDesc: windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
            Flags: D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
            ..Default::default()
        };
        let mut buffer: Option<ID3D12Resource> = None;
        // SAFETY: plain creation.
        unsafe { device.CreateCommittedResource(&heap, D3D12_HEAP_FLAG_NONE, &desc, state, None, &mut buffer) }.ok()?;
        buffer
    }

    pub(super) fn turn(resource: &ID3D12Resource, before: D3D12_RESOURCE_STATES, after: D3D12_RESOURCE_STATES) -> D3D12_RESOURCE_BARRIER {
        D3D12_RESOURCE_BARRIER {
            Type: D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
            Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
            Anonymous: D3D12_RESOURCE_BARRIER_0 {
                Transition: std::mem::ManuallyDrop::new(D3D12_RESOURCE_TRANSITION_BARRIER {
                    // SAFETY: the same pointer, not counted; used while the
                    // resource lives (the list keeps it).
                    pResource: unsafe { std::mem::transmute_copy(resource) },
                    Subresource: D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                    StateBefore: before,
                    StateAfter: after,
                }),
            },
        }
    }

    pub(super) fn uav(resource: &ID3D12Resource) -> D3D12_RESOURCE_BARRIER {
        D3D12_RESOURCE_BARRIER {
            Type: D3D12_RESOURCE_BARRIER_TYPE_UAV,
            Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
            Anonymous: D3D12_RESOURCE_BARRIER_0 {
                UAV: std::mem::ManuallyDrop::new(D3D12_RESOURCE_UAV_BARRIER {
                    // SAFETY: as in `turn`.
                    pResource: unsafe { std::mem::transmute_copy(resource) },
                }),
            },
        }
    }

    /// A top-level record's dwords 16-27 (translation, then the 3x3 rows) as
    /// a 3x4 matrix of rows.
    pub(super) fn world_to_node(w: &[u32; 12]) -> [f32; 12] {
        let f = |k: usize| f32::from_bits(w[k]);
        [f(3), f(4), f(5), f(0), f(6), f(7), f(8), f(1), f(9), f(10), f(11), f(2)]
    }

    /// The inverse of an affine 3x4 matrix (rows), if it has one.
    pub(super) fn invert(m: &[f32; 12]) -> Option<[f32; 12]> {
        let [a, b, c, tx, d, e, f, ty, g, h, i, tz] = *m;
        let det = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g);
        if !det.is_finite() || det.abs() < 1e-20 {
            return None;
        }
        let r = 1.0 / det;
        let n = [
            (e * i - f * h) * r,
            (c * h - b * i) * r,
            (b * f - c * e) * r,
            (f * g - d * i) * r,
            (a * i - c * g) * r,
            (c * d - a * f) * r,
            (d * h - e * g) * r,
            (b * g - a * h) * r,
            (a * e - b * d) * r,
        ];
        let t = [
            -(n[0] * tx + n[1] * ty + n[2] * tz),
            -(n[3] * tx + n[4] * ty + n[5] * tz),
            -(n[6] * tx + n[7] * ty + n[8] * tz),
        ];
        let out = [n[0], n[1], n[2], t[0], n[3], n[4], n[5], t[1], n[6], n[7], n[8], t[2]];
        out.iter().all(|v| v.is_finite()).then_some(out)
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn the_inverse_undoes_the_matrix() {
            let m = [2.0, 0.0, 0.0, 5.0, 0.0, 0.0, -3.0, 1.0, 0.0, 4.0, 0.0, -2.0];
            let n = super::invert(&m).unwrap();
            let p = [1.5f32, -2.0, 7.0];
            let apply = |m: &[f32; 12], p: [f32; 3]| {
                [0, 1, 2].map(|r| m[4 * r] * p[0] + m[4 * r + 1] * p[1] + m[4 * r + 2] * p[2] + m[4 * r + 3])
            };
            let back = apply(&n, apply(&m, p));
            for (a, b) in back.iter().zip(p) {
                assert!((a - b).abs() < 1e-5, "{back:?}");
            }
        }
    }
}

impl Memory {
    /// Copies `len` bytes of title memory from `at` into `buffer` from
    /// `into` on, page by page, where an arena holds them; the pieces that
    /// came.
    fn dxr_read(&self, list: &HostList, at: u64, len: u64, buffer: &ID3D12Resource, into: u64) -> Vec<(u64, u64)> {
        let mut got = Vec::new();
        let mut done = 0;
        while done < len {
            let address = at + done;
            let page = address / PAGE;
            let n = (PAGE - address % PAGE).min(len - done);
            // (Any copy that holds the page: a structure the title keeps
            // inside one of its buffers is in that buffer's copy, not in an
            // arena. Two such bottom levels had a third to a half of a
            // scene's instances, 340 to 850, and were never read: the host's
            // top level lacked them all, runs 20261127-191255 to -204000.)
            if let Some(place) = self.copy_of(page) {
                // SAFETY: an entry of the directory.
                let start = u64::from(unsafe { self.entry(page).add(1).read() });
                let from = &self.copies[place].buffer;
                // SAFETY: an open list; live buffers; inside both (an arena
                // page is a whole 64 KiB of the arena).
                unsafe {
                    list.list.ResourceBarrier(&[dxr::turn(from, D3D12_RESOURCE_STATE_COMMON, D3D12_RESOURCE_STATE_COPY_SOURCE)]);
                    list.list.CopyBufferRegion(buffer, into + done, from, start + address % PAGE, n);
                    list.list.ResourceBarrier(&[dxr::turn(from, D3D12_RESOURCE_STATE_COPY_SOURCE, D3D12_RESOURCE_STATE_COMMON)]);
                }
                got.push((into + done, n));
            }
            done += n;
        }
        got
    }

    /// Copies `len` bytes of `buffer` from `from` on into title memory at
    /// `at`, page by page, where an arena holds it ([`Memory::dxr_read`]
    /// the other way); the bytes that went.
    fn dxr_write(&self, list: &HostList, at: u64, len: u64, buffer: &ID3D12Resource, from: u64) -> u64 {
        let (mut done, mut went) = (0, 0);
        while done < len {
            let address = at + done;
            let page = address / PAGE;
            let n = (PAGE - address % PAGE).min(len - done);
            if let Some(place) = self.copy_of(page) {
                // SAFETY: an entry of the directory.
                let start = u64::from(unsafe { self.entry(page).add(1).read() });
                let to = &self.copies[place].buffer;
                // SAFETY: an open list; live buffers; inside both.
                unsafe {
                    list.list.ResourceBarrier(&[dxr::turn(to, D3D12_RESOURCE_STATE_COMMON, D3D12_RESOURCE_STATE_COPY_DEST)]);
                    list.list.CopyBufferRegion(to, start + address % PAGE, buffer, from + done, n);
                    list.list.ResourceBarrier(&[dxr::turn(to, D3D12_RESOURCE_STATE_COPY_DEST, D3D12_RESOURCE_STATE_COMMON)]);
                }
                went += n;
            }
            done += n;
        }
        went
    }

    /// The copies brought up to date at `list`'s submission before its own
    /// commands (as for a list that reads them), and a mark begun for what
    /// the layer's own work lends on it.
    fn dxr_own_work(&mut self, list: &HostList, at: u64, len: u64) -> u32 {
        self.ready(at, len);
        if list.first_use(usize::MAX - (6 << 33) - self.copies.len()) {
            self.bring_up(list);
        }
        self.dxr.last_token = match self.dxr.last_token.wrapping_add(1) {
            0 => 1,
            t => t,
        };
        self.dxr.token = self.dxr.last_token;
        self.dxr.token_used = false;
        self.dxr.take_token()
    }

    fn dxr_own_work_done(&mut self, list: &HostList) {
        let token = std::mem::take(&mut self.dxr.token);
        if std::mem::take(&mut self.dxr.token_used) {
            self.dxr_mark(list, token);
        }
    }

    /// [`card_elements`].
    fn dxr_gather(&mut self, list: &HostList, g: &Gather) -> bool {
        if self.dxr.gather.is_none() {
            self.dxr.gather = Some(dxr::make_pass_of(dxr::GATHER_CS));
        }
        let Some(Some(pass)) = self.dxr.gather.as_ref() else { return false };
        let (root, pso, device) = (pass.root.clone(), pass.pso.clone(), pass.device.clone());
        let (start, len) = g.vertices.span();
        let bytes = 64 * u64::from(g.count);
        if g.count == 0 || g.base < start || g.base - start + u64::from(g.most) + 12 > len || g.base - start > u64::from(u32::MAX) {
            return false;
        }
        let token = self.dxr_own_work(list, g.to, bytes);
        let frame = self.frame;
        let Some((out, _)) = self.dxr.pool.lend(&device, false, bytes, token, frame) else {
            self.dxr_own_work_done(list);
            return false;
        };
        // SAFETY: an open list; buffers it keeps below; the pass's root
        // signature takes two constants, two views to read and one to
        // write; the plan's offsets were checked against the copy's span.
        unsafe {
            let l = &list.list;
            l.ResourceBarrier(&[dxr::uav(&g.vertices.buffer), dxr::turn(&out, D3D12_RESOURCE_STATE_COMMON, D3D12_RESOURCE_STATE_UNORDERED_ACCESS)]);
            l.SetComputeRootSignature(&root);
            l.SetPipelineState(&pso);
            l.SetComputeRoot32BitConstants(0, 2, [(g.base - start) as u32, g.count].as_ptr().cast(), 0);
            l.SetComputeRootShaderResourceView(1, g.vertices.buffer.GetGPUVirtualAddress());
            l.SetComputeRootShaderResourceView(2, g.plan.GetGPUVirtualAddress());
            l.SetComputeRootUnorderedAccessView(3, out.GetGPUVirtualAddress());
            l.Dispatch(g.count.div_ceil(64), 1, 1);
            l.ResourceBarrier(&[dxr::turn(&out, D3D12_RESOURCE_STATE_UNORDERED_ACCESS, D3D12_RESOURCE_STATE_COPY_SOURCE)]);
        }
        let went = self.dxr_write(list, g.to, bytes, &out, 0);
        // SAFETY: as above.
        unsafe { list.list.ResourceBarrier(&[dxr::turn(&out, D3D12_RESOURCE_STATE_COPY_SOURCE, D3D12_RESOURCE_STATE_COMMON)]) };
        list.keep(&out);
        list.keep(&g.plan);
        list.keep(&g.vertices.buffer);
        self.dxr_own_work_done(list);
        went == bytes
    }

    /// [`card_copy`].
    fn dxr_copy(&mut self, list: &HostList, to: u64, from: u64, bytes: u64) -> bool {
        let Some(device) = dxr::device() else { return false };
        let token = self.dxr_own_work(list, to, bytes);
        let frame = self.frame;
        let Some((between, _)) = self.dxr.pool.lend(&device, false, bytes, token, frame) else {
            self.dxr_own_work_done(list);
            return false;
        };
        // SAFETY: an open list; a buffer it keeps.
        unsafe { list.list.ResourceBarrier(&[dxr::turn(&between, D3D12_RESOURCE_STATE_COMMON, D3D12_RESOURCE_STATE_COPY_DEST)]) };
        let came: u64 = self.dxr_read(list, from, bytes, &between, 0).iter().map(|g| g.1).sum();
        // SAFETY: as above.
        unsafe { list.list.ResourceBarrier(&[dxr::turn(&between, D3D12_RESOURCE_STATE_COPY_DEST, D3D12_RESOURCE_STATE_COPY_SOURCE)]) };
        let went = if came == bytes { self.dxr_write(list, to, bytes, &between, 0) } else { 0 };
        // SAFETY: as above.
        unsafe { list.list.ResourceBarrier(&[dxr::turn(&between, D3D12_RESOURCE_STATE_COPY_SOURCE, D3D12_RESOURCE_STATE_COMMON)]) };
        list.keep(&between);
        self.dxr_own_work_done(list);
        went == bytes
    }

    /// Before a trace dispatch: the top levels it is given read once in a
    /// while, and host structures built. True if the list's compute root
    /// signature and pipeline were replaced.
    fn dxr_tops(&mut self, list: &HostList, addresses: &[u64]) -> bool {
        // One mark for what this list reads and builds here, written after it.
        self.dxr.last_token = match self.dxr.last_token.wrapping_add(1) {
            0 => 1,
            t => t,
        };
        self.dxr.token = self.dxr.last_token;
        self.dxr.token_used = false;
        let clobbered = self.dxr_tops_marked(list, addresses);
        let token = std::mem::take(&mut self.dxr.token);
        if std::mem::take(&mut self.dxr.token_used) {
            self.dxr_mark(list, token);
        }
        clobbered
    }

    /// The card writes `token` into its place among the marks once every
    /// step recorded on `list` before it has run.
    fn dxr_mark(&mut self, list: &HostList, token: u32) {
        if self.dxr.marks.is_none() {
            let bytes = u64::from(dxr::MARKS) * 4;
            let Some((buffer, data)) = host().and_then(|h| h.shared_buffer(bytes)) else { return };
            // SAFETY: the new mapping, `bytes` long: no mark yet.
            unsafe { std::ptr::write_bytes(data, 0, bytes as usize) };
            self.dxr.marks = Some((buffer, data as usize));
        }
        let Some((marks, _)) = self.dxr.marks.as_ref() else { return };
        let Ok(list2) = windows::core::Interface::cast::<ID3D12GraphicsCommandList2>(&list.list) else { return };
        // SAFETY: an open list; a live buffer it keeps, in the common state
        // between lists; the mark's place lies in it.
        unsafe {
            let at = marks.GetGPUVirtualAddress() + 4 * u64::from(token % dxr::MARKS);
            let write = [D3D12_WRITEBUFFERIMMEDIATE_PARAMETER { Dest: at, Value: token }];
            let modes = [D3D12_WRITEBUFFERIMMEDIATE_MODE_MARKER_OUT];
            list.list.ResourceBarrier(&[dxr::turn(marks, D3D12_RESOURCE_STATE_COMMON, D3D12_RESOURCE_STATE_COPY_DEST)]);
            list2.WriteBufferImmediate(1, write.as_ptr(), Some(modes.as_ptr()));
            list.list.ResourceBarrier(&[dxr::turn(marks, D3D12_RESOURCE_STATE_COPY_DEST, D3D12_RESOURCE_STATE_COMMON)]);
        }
        list.keep(marks);
    }

    fn dxr_tops_marked(&mut self, list: &HostList, addresses: &[u64]) -> bool {
        let Some(host) = host() else { return false };
        for &top in addresses {
            if self.dxr.tops.get(&top).is_some_and(|t| t.read + 30 > self.frame) || self.dxr.pending.len() > 64 {
                continue;
            }
            if self.dxr.pending.iter().any(|p| matches!(p.what, dxr::What::Top(a) if a == top)) {
                continue;
            }
            let Some(device) = dxr::device() else { return false };
            let Some((buffer, data)) = self.dxr.pool.take(&device, true, dxr::TOP_BYTES) else { return false };
            let got = self.dxr_read(list, top, dxr::TOP_BYTES, &buffer, 0);
            list.keep(&buffer);
            self.dxr.stats[0] += 1;
            let token = self.dxr.take_token();
            self.dxr.pending.push(dxr::Pending { due: self.frame + dxr::LATER, what: dxr::What::Top(top), buffer, data, got, token, size: dxr::TOP_BYTES });
            let entry = self.dxr.tops.entry(top).or_default();
            entry.read = self.frame;
            // The bottom levels its instances named last time, their headers.
            let mut wanted: Vec<u64> = entry.instances.iter().map(|i| i.bottom).collect();
            wanted.sort_unstable();
            wanted.dedup();
            wanted.truncate(dxr::BOTTOMS);
            if !wanted.is_empty() {
                let size = 64 * wanted.len() as u64;
                let Some(device) = dxr::device() else { return false };
                let Some((buffer, data)) = self.dxr.pool.take(&device, true, size) else { return false };
                let mut got = Vec::new();
                for (k, &b) in wanted.iter().enumerate() {
                    got.extend(self.dxr_read(list, b, 64, &buffer, 64 * k as u64));
                }
                list.keep(&buffer);
                let token = self.dxr.take_token();
                self.dxr.pending.push(dxr::Pending { due: self.frame + dxr::LATER, what: dxr::What::Bottoms(wanted), buffer, data, got, token, size });
            }
        }
        // The guided walk's counters, read back now and then.
        if let Some(counters) = self.dxr.counters.clone()
            && self.dxr.counted.0 + 60 <= self.frame
            && !self.dxr.pending.iter().any(|p| matches!(p.what, dxr::What::Counters))
            && let Some((buffer, data)) = host.shared_buffer(16)
        {
            // SAFETY: an open list; buffers it keeps.
            unsafe {
                list.list.ResourceBarrier(&[dxr::turn(&counters, D3D12_RESOURCE_STATE_COMMON, D3D12_RESOURCE_STATE_COPY_SOURCE)]);
                list.list.CopyBufferRegion(&buffer, 0, &counters, 0, 12);
                list.list.ResourceBarrier(&[dxr::turn(&counters, D3D12_RESOURCE_STATE_COPY_SOURCE, D3D12_RESOURCE_STATE_COMMON)]);
            }
            list.keep(&buffer);
            list.keep(&counters);
            self.dxr.counted.0 = self.frame;
            let token = self.dxr.take_token();
            self.dxr.pending.push(dxr::Pending { due: self.frame + dxr::LATER, what: dxr::What::Counters, buffer, data: data as usize, got: vec![(0, 12)], token, size: 0 });
        }
        // (Not while the live switch has the host's structures unused.)
        if matches!(crate::live::ray_tracing(), Some(0 | 1)) {
            return false;
        }
        let started = std::time::Instant::now();
        let clobbered = self.dxr_build(list, addresses);
        if clobbered {
            self.dxr.period[3] += started.elapsed().as_micros() as u64;
        }
        clobbered
    }

    /// Host bottom levels for the instances of `tops` that want one, then
    /// their host top levels. True if anything was recorded.
    fn dxr_build(&mut self, list: &HostList, tops: &[u64]) -> bool {
        if self.dxr.pass.is_none() {
            self.dxr.pass = Some(dxr::make_pass());
        }
        let Some(Some(pass)) = self.dxr.pass.as_ref() else { return false };
        let (pass_root, pass_pso, device) = (pass.root.clone(), pass.pso.clone(), pass.device.clone());
        let Ok(list4) = windows::core::Interface::cast::<ID3D12GraphicsCommandList4>(&list.list) else { return false };
        let frame = self.frame;
        // Room on the card, asked once a frame.
        if self.dxr.room.0 != frame {
            let room = host().and_then(|h| h.video_memory()).is_none_or(|[budget, used, ..]| (used as f64) < dxr::CARD_SHARE * budget as f64);
            self.dxr.room = (frame, room);
            if !room {
                self.dxr.card_full += 1;
            }
        }
        let kept: u64 =
            self.dxr.built.values().map(|b| b.bytes).sum::<u64>() + self.dxr.building.iter().map(|b| b.2.bytes).sum::<u64>();
        let room = self.dxr.room.1 && kept < dxr::MOST_KEPT;
        // The bottom levels to build now.
        let mut todo: Vec<(u64, [u32; 16])> = Vec::new();
        for top in tops {
            let Some(t) = self.dxr.tops.get(top) else { continue };
            for i in &t.instances {
                if todo.len() >= dxr::BUILDS {
                    break;
                }
                let Some(b) = self.dxr.bottoms.get(&i.bottom) else { continue };
                if !dxr::usable_bottom(&b.header)
                    || todo.iter().any(|t| t.0 == i.bottom)
                    || self.dxr.building.iter().any(|w| w.1 == i.bottom)
                {
                    continue;
                }
                let again = match self.dxr.built.get(&i.bottom) {
                    None => true,
                    Some(built) => {
                        built.header != b.header || (!dxr::static_bottom(&b.header) && built.frame + dxr::AGAIN <= frame)
                    }
                };
                if again && room {
                    todo.push((i.bottom, b.header));
                }
            }
        }
        let mut recorded = false;
        for (bottom, header) in todo {
            let (first, count) = (u64::from(header[13]), u64::from(header[3]));
            let mut lap = std::time::Instant::now();
            let mut took = |n: usize, cost: &mut [u64; 5]| {
                let now = std::time::Instant::now();
                cost[n] += now.duration_since(lap).as_micros() as u64;
                lap = now;
            };
            let (token, frame) = (self.dxr.take_token(), self.frame);
            let Some((elements, _)) = self.dxr.pool.lend(&device, true, 64 * count, token, frame) else { break };
            let Some((vertices, _)) = self.dxr.pool.lend(&device, false, 144 * count, token, frame) else { break };
            took(0, &mut self.dxr.cost);
            // SAFETY: an open list; buffers it keeps.
            unsafe { list.list.ResourceBarrier(&[dxr::turn(&elements, D3D12_RESOURCE_STATE_COMMON, D3D12_RESOURCE_STATE_COPY_DEST)]) };
            let got = self.dxr_read(list, bottom + first, 64 * count, &elements, 0);
            took(1, &mut self.dxr.cost);
            if got.iter().map(|g| g.1).sum::<u64>() < 64 * count {
                // (Some of it is not in an arena: next time.)
                self.dxr.left_out += 1;
                // SAFETY: as above.
                unsafe { list.list.ResourceBarrier(&[dxr::turn(&elements, D3D12_RESOURCE_STATE_COPY_DEST, D3D12_RESOURCE_STATE_COMMON)]) };
                list.keep(&elements);
                continue;
            }
            let triangles = 4 * count;
            let geometry = D3D12_RAYTRACING_GEOMETRY_DESC {
                Type: D3D12_RAYTRACING_GEOMETRY_TYPE_TRIANGLES,
                Flags: D3D12_RAYTRACING_GEOMETRY_FLAG_OPAQUE,
                Anonymous: D3D12_RAYTRACING_GEOMETRY_DESC_0 {
                    Triangles: D3D12_RAYTRACING_GEOMETRY_TRIANGLES_DESC {
                        Transform3x4: 0,
                        IndexFormat: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_UNKNOWN,
                        VertexFormat: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_R32G32B32_FLOAT,
                        IndexCount: 0,
                        VertexCount: (3 * triangles) as u32,
                        IndexBuffer: 0,
                        // SAFETY: a live buffer.
                        VertexBuffer: D3D12_GPU_VIRTUAL_ADDRESS_AND_STRIDE { StartAddress: unsafe { vertices.GetGPUVirtualAddress() }, StrideInBytes: 12 },
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
            let mut sizes = D3D12_RAYTRACING_ACCELERATION_STRUCTURE_PREBUILD_INFO::default();
            // SAFETY: the inputs live through the call.
            unsafe { device.GetRaytracingAccelerationStructurePrebuildInfo(&inputs, &mut sizes) };
            let made = (
                dxr::card_buffer(&device, sizes.ResultDataMaxSizeInBytes, D3D12_RESOURCE_STATE_RAYTRACING_ACCELERATION_STRUCTURE),
                self.dxr.pool.lend(&device, false, sizes.ScratchDataSizeInBytes, token, frame).map(|b| b.0),
            );
            took(0, &mut self.dxr.cost);
            let (Some(structure), Some(scratch)) = made else {
                self.dxr.left_out += 1;
                list.keep(&elements);
                list.keep(&vertices);
                continue;
            };
            // SAFETY: an open list; every buffer is kept by it below; the
            // pass's root signature takes two constants, a view to read and
            // one to write.
            unsafe {
                let l = &list.list;
                l.ResourceBarrier(&[
                    dxr::turn(&elements, D3D12_RESOURCE_STATE_COPY_DEST, D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE),
                    dxr::turn(&vertices, D3D12_RESOURCE_STATE_COMMON, D3D12_RESOURCE_STATE_UNORDERED_ACCESS),
                    dxr::turn(&scratch, D3D12_RESOURCE_STATE_COMMON, D3D12_RESOURCE_STATE_UNORDERED_ACCESS),
                ]);
                l.SetComputeRootSignature(&pass_root);
                l.SetPipelineState(&pass_pso);
                l.SetComputeRoot32BitConstants(0, 2, [0u32, count as u32].as_ptr().cast(), 0);
                l.SetComputeRootShaderResourceView(1, elements.GetGPUVirtualAddress());
                l.SetComputeRootUnorderedAccessView(2, vertices.GetGPUVirtualAddress());
                l.Dispatch(count.div_ceil(64) as u32, 1, 1);
                l.ResourceBarrier(&[dxr::turn(&vertices, D3D12_RESOURCE_STATE_UNORDERED_ACCESS, D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE)]);
                let build = D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_DESC {
                    DestAccelerationStructureData: structure.GetGPUVirtualAddress(),
                    Inputs: inputs,
                    SourceAccelerationStructureData: 0,
                    ScratchAccelerationStructureData: scratch.GetGPUVirtualAddress(),
                };
                list4.BuildRaytracingAccelerationStructure(&build, None);
                l.ResourceBarrier(&[
                    dxr::uav(&structure),
                    dxr::turn(&elements, D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE, D3D12_RESOURCE_STATE_COMMON),
                    dxr::turn(&vertices, D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE, D3D12_RESOURCE_STATE_COMMON),
                    dxr::turn(&scratch, D3D12_RESOURCE_STATE_UNORDERED_ACCESS, D3D12_RESOURCE_STATE_COMMON),
                ]);
            }
            for r in [&elements, &vertices, &scratch, &structure] {
                list.keep(r);
            }
            took(2, &mut self.dxr.cost);
            recorded = true;
            self.dxr.period[0] += 1;
            self.dxr.period[1] += triangles;
            self.dxr.period[4] = self.dxr.period[4].max(sizes.ScratchDataSizeInBytes);
            // It alone in a top level, for the guided walk at its root.
            // SAFETY: a live structure.
            let one = D3D12_RAYTRACING_INSTANCE_DESC {
                Transform: [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                _bitfield1: 0xff00_0000,
                _bitfield2: 0,
                AccelerationStructure: unsafe { structure.GetGPUVirtualAddress() },
            };
            let single = self.dxr_top_level(list, &list4, &device, &[one]);
            took(3, &mut self.dxr.cost);
            if let Some(single) = &single {
                let token = self.dxr.take_token();
                self.dxr.unpublished.push((frame + dxr::PUBLISH_AFTER, bottom, dxr::bottom_kind(&header), [header[12], header[13]], single.clone(), token));
            }
            // Used from the frame its view is set (`Memory::dxr_frame`).
            let new = dxr::Built { header, frame, structure, bytes: sizes.ResultDataMaxSizeInBytes, triangles, single, seen: frame };
            let token = self.dxr.take_token();
            self.dxr.building.push((frame + dxr::PUBLISH_AFTER, bottom, new, token));
        }
        // The top levels, once a top is read again (their instances move).
        let tops_began = std::time::Instant::now();
        for top in tops {
            let Some(t) = self.dxr.tops.get(top) else { continue };
            if t.instances.is_empty() || (t.built.is_some() && t.built_frame >= t.read) {
                continue;
            }
            let root = t.root;
            let mut descs: Vec<D3D12_RAYTRACING_INSTANCE_DESC> = Vec::new();
            // The walks start a bottom level at its instance's root pointer
            // (dword 14, a static one marked in bit 30), not its header's:
            // published under that one too.
            let mut roots: Vec<(u64, u32, u32, u32, ID3D12Resource)> = Vec::new();
            let mut named: Vec<u64> = Vec::new();
            for i in &t.instances {
                let Some(built) = self.dxr.built.get(&i.bottom) else { continue };
                named.push(i.bottom);
                if let Some(single) = &built.single {
                    roots.push((i.bottom, i.root_pointer, built.header[13], dxr::bottom_kind(&built.header), single.clone()));
                }
                // The host's bottom level holds the title's element nodes, in
                // the bottom level's node space: its instance goes from there
                // to the world, the inverse of the record's world-to-node
                // transform (dwords 16-27). (Inverting dwords 0-11, the
                // instance's own object-to-world transform, as before, put
                // every instance somewhere else: the host's hits were not the
                // title's.)
                let Some(n) = dxr::invert(&dxr::world_to_node(&i.inverse)) else { continue };
                descs.push(D3D12_RAYTRACING_INSTANCE_DESC {
                    Transform: n,
                    _bitfield1: (i.record & 0x00ff_ffff) | (i.words[12] & 0xff00_0000),
                    _bitfield2: 0,
                    // SAFETY: a live structure.
                    AccelerationStructure: unsafe { built.structure.GetGPUVirtualAddress() },
                });
            }
            for bottom in named {
                if let Some(built) = self.dxr.built.get_mut(&bottom) {
                    built.seen = frame;
                }
            }
            if descs.is_empty() {
                continue;
            }
            for (bottom, pointer, offset, kind, single) in roots {
                let fresh = self.dxr.instance_roots.get(&(bottom, pointer)).is_none_or(|s| s != &single);
                if fresh && pointer != 0 {
                    self.dxr.instance_roots.insert((bottom, pointer), single.clone());
                    // (Its structures' builds were marked: `built` holds only those.)
                    self.dxr.unpublished.push((frame + dxr::PUBLISH_AFTER, bottom, kind, [pointer, offset], single, 0));
                }
            }
            let Some(structure) = self.dxr_top_level(list, &list4, &device, &descs) else { continue };
            recorded = true;
            self.dxr.period[2] += 1;
            self.dxr.tops_built_frame = frame;
            let token = self.dxr.take_token();
            self.dxr.unpublished.push((frame + dxr::PUBLISH_AFTER, *top, victory_shader::emit::DXR_TOP, root, structure.clone(), token));
            if let Some(t) = self.dxr.tops.get_mut(top) {
                if let Some((old, _)) = t.built.replace((structure, descs.len())) {
                    self.dxr.retired.push((frame, old));
                }
                t.built_frame = frame;
            }
        }
        self.dxr.cost[4] += tops_began.elapsed().as_micros() as u64;
        recorded
    }

    /// A host top level of `descs`, recorded on `list`.
    fn dxr_top_level(
        &mut self,
        list: &HostList,
        list4: &ID3D12GraphicsCommandList4,
        device: &ID3D12Device5,
        descs: &[D3D12_RAYTRACING_INSTANCE_DESC],
    ) -> Option<ID3D12Resource> {
        let size = size_of_val(descs) as u64;
        let (token, frame) = (self.dxr.take_token(), self.frame);
        let (instances, data) = self.dxr.pool.lend(device, true, size, token, frame)?;
        let data = data as *mut u8;
        // SAFETY: the mapping holds `size` bytes.
        unsafe { std::ptr::copy_nonoverlapping(descs.as_ptr().cast::<u8>(), data, size as usize) };
        list.keep(&instances);
        let inputs = D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS {
            Type: D3D12_RAYTRACING_ACCELERATION_STRUCTURE_TYPE_TOP_LEVEL,
            Flags: D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BUILD_FLAG_PREFER_FAST_TRACE,
            NumDescs: descs.len() as u32,
            DescsLayout: D3D12_ELEMENTS_LAYOUT_ARRAY,
            // SAFETY: a live buffer.
            Anonymous: D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS_0 { InstanceDescs: unsafe { instances.GetGPUVirtualAddress() } },
        };
        let mut sizes = D3D12_RAYTRACING_ACCELERATION_STRUCTURE_PREBUILD_INFO::default();
        // SAFETY: the inputs live through the call.
        unsafe { device.GetRaytracingAccelerationStructurePrebuildInfo(&inputs, &mut sizes) };
        let structure = dxr::card_buffer(device, sizes.ResultDataMaxSizeInBytes, D3D12_RESOURCE_STATE_RAYTRACING_ACCELERATION_STRUCTURE)?;
        let (scratch, _) = self.dxr.pool.lend(device, false, sizes.ScratchDataSizeInBytes, token, frame)?;
        // SAFETY: an open list; buffers it keeps.
        unsafe {
            list.list.ResourceBarrier(&[dxr::turn(&scratch, D3D12_RESOURCE_STATE_COMMON, D3D12_RESOURCE_STATE_UNORDERED_ACCESS)]);
            let build = D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_DESC {
                DestAccelerationStructureData: structure.GetGPUVirtualAddress(),
                Inputs: inputs,
                SourceAccelerationStructureData: 0,
                ScratchAccelerationStructureData: scratch.GetGPUVirtualAddress(),
            };
            list4.BuildRaytracingAccelerationStructure(&build, None);
            list.list.ResourceBarrier(&[dxr::uav(&structure), dxr::turn(&scratch, D3D12_RESOURCE_STATE_UNORDERED_ACCESS, D3D12_RESOURCE_STATE_COMMON)]);
        }
        list.keep(&scratch);
        list.keep(&structure);
        Some(structure)
    }

    /// Host bottom levels no top level's instances have named for a while
    /// are let go when the room for them runs out, the longest unnamed first.
    /// They were kept for the whole run: the ones of the scenes before filled
    /// the room, and nothing was built of the scene the title was in (run
    /// 20261127-155346: "517 MB of 512 allowed" two minutes in, 395 instances
    /// of the main top level with a usable bottom level and no host one).
    /// One let go is taken out of the walk's table first and freed later, as
    /// any structure replaced ([`dxr::Dxr::retired`]); its number goes back.
    fn dxr_let_go(&mut self) {
        if !matches!(crate::live::ray_tracing(), None | Some(2 | 3)) {
            return;
        }
        let frame = self.frame;
        let kept: u64 =
            self.dxr.built.values().map(|b| b.bytes).sum::<u64>() + self.dxr.building.iter().map(|b| b.2.bytes).sum::<u64>();
        let numbers_short = self.dxr.next_number + 64 >= victory_shader::emit::DXR_STRUCTURES && self.dxr.free_numbers.is_empty();
        if kept < dxr::MOST_KEPT / 4 * 3 && self.dxr.room.1 && !numbers_short {
            return;
        }
        let mut old: Vec<(u64, u64, u64)> = self
            .dxr
            .built
            .iter()
            .filter(|(_, b)| b.seen + dxr::UNSEEN < self.dxr.tops_built_frame)
            .map(|(&address, b)| (b.seen, address, b.bytes))
            .collect();
        old.sort_unstable();
        let mut left = kept;
        // (Not many a frame: each is a few table writes.)
        for (_, address, bytes) in old.into_iter().take(64) {
            if left < dxr::MOST_KEPT / 2 && self.dxr.room.1 && !numbers_short {
                break;
            }
            let Some(gone) = self.dxr.built.remove(&address) else { continue };
            left = left.saturating_sub(bytes);
            self.dxr.unpublished.retain(|u| u.1 != address);
            self.dxr.entering.retain(|e| e.1 != address);
            self.dxr_unpublish(address, [gone.header[12], gone.header[13]]);
            let roots: Vec<u32> = self.dxr.instance_roots.keys().filter(|k| k.0 == address).map(|k| k.1).collect();
            for pointer in roots {
                self.dxr.instance_roots.remove(&(address, pointer));
                self.dxr_unpublish(address, [pointer, 0]);
            }
            if let Some(number) = self.dxr.numbers.remove(&address) {
                if let Some((structure, _)) = self.dxr.structures.get_mut(number as usize).and_then(Option::take) {
                    self.dxr.retired.push((frame, structure));
                }
                self.dxr.free_numbers.push((frame + dxr::KEEP_AFTER, number));
            }
            self.dxr.retired.push((frame, gone.structure));
            if let Some(single) = gone.single {
                self.dxr.retired.push((frame, single));
            }
            self.dxr.let_go += 1;
        }
    }

    /// The guided walk's table entry of the structure at `address` (its
    /// `kind`, root node pointer and list or element offset), and its view's
    /// structure (`victory_shader::emit::MEMORY_RUN`).
    fn dxr_publish(&mut self, address: u64, kind: u32, root: [u32; 2], structure: &ID3D12Resource) {
        use victory_shader::emit::DXR_STRUCTURES;
        if !self.dxr_table() {
            return;
        }
        let number = match self.dxr.numbers.get(&address) {
            Some(&n) => n,
            None => {
                let frame = self.frame;
                let n = if let Some(k) = self.dxr.free_numbers.iter().position(|f| f.0 <= frame) {
                    self.dxr.free_numbers.swap_remove(k).1
                } else if self.dxr.next_number < DXR_STRUCTURES {
                    self.dxr.next_number += 1;
                    self.dxr.next_number - 1
                } else {
                    self.dxr.not_entered[0] += 1;
                    return;
                };
                self.dxr.numbers.insert(address, n);
                n
            }
        };
        self.dxr.generation += 1;
        self.dxr.generation_frames.insert(self.dxr.generation, self.frame);
        if let Some((old, _)) = self.dxr.structures[number as usize].replace((structure.clone(), self.dxr.generation)) {
            self.dxr.retired.push((self.frame, old));
        }
        if dxr::enter_early().is_some() {
            self.dxr_entry(address, kind, root, number);
            return;
        }
        // (The entry once the views show the structure: `dxr_enter`.)
        self.dxr.entering.retain(|e| e.1 != address || e.3 != root);
        self.dxr.entering.push((self.dxr.generation, address, kind, root));
    }

    /// The table entries whose structures' views are written in every heap a
    /// list may still read them through ([`dxr::ENTER_AFTER`]): set.
    fn dxr_enter(&mut self) {
        let frame = self.frame;
        self.dxr.views_asked.retain(|_, asked| *asked + dxr::ENTER_AFTER > frame);
        let shown = self
            .dxr
            .views_asked
            .keys()
            .map(|heap| self.dxr.views.get(heap).copied().unwrap_or(0))
            .min()
            .unwrap_or(u64::MAX);
        let (now, later): (Vec<_>, Vec<_>) = std::mem::take(&mut self.dxr.entering).into_iter().partition(|e| e.0 <= shown);
        self.dxr.entering = later;
        for (generation, address, kind, root) in now {
            // (Let go meanwhile: no entry.)
            let Some(&number) = self.dxr.numbers.get(&address) else { continue };
            let Some(set) = self.dxr.structures.get(number as usize).and_then(|s| s.as_ref()).map(|s| s.1) else { continue };
            // (The number's structure set again since, for another root of
            // a bottom level's: once the views show that one.)
            if set > shown {
                self.dxr.entering.push((set, address, kind, root));
                continue;
            }
            let waited = self.dxr.generation_frames.get(&generation).map_or(0, |at| frame.saturating_sub(*at));
            self.dxr.entered[0] += 1;
            self.dxr.entered[1] += waited;
            self.dxr.entered[2] = self.dxr.entered[2].max(waited);
            self.dxr_entry(address, kind, root, number);
        }
        let waiting: std::collections::HashSet<u64> = self.dxr.entering.iter().map(|e| e.0).collect();
        self.dxr.generation_frames.retain(|g, _| waiting.contains(g));
    }

    /// The table entry of the structure at `address` with number `number`.
    fn dxr_entry(&mut self, address: u64, kind: u32, root: [u32; 2], number: u32) {
        use victory_shader::emit::{DXR_ENTRIES, DXR_HASH, DXR_HASH_SHIFT};
        // The root's absolute node pointer (the title's walks use the 64-bit
        // node test with a descriptor base of 0): the key and its high dword.
        let absolute = (address >> 3) + u64::from(root[0]);
        let (key, key_high) = (absolute as u32, (absolute >> 32) as u32);
        if key == 0 {
            return;
        }
        let Some((_, data)) = self.dxr.table else { return };
        let table = data as *mut u32;
        let hash = key.wrapping_mul(DXR_HASH) >> DXR_HASH_SHIFT;
        for n in 0..4 {
            let place = (hash + n) & (DXR_ENTRIES - 1);
            // SAFETY: inside the table's mapping; the card reads it as it
            // is written (an entry's key last: it is whole once found).
            unsafe {
                let entry = table.add(4 * place as usize);
                let there = entry.read_volatile();
                if there != 0 && there != key {
                    continue;
                }
                entry.add(1).write_volatile(kind << 30 | number);
                entry.add(2).write_volatile(key_high);
                entry.add(3).write_volatile(root[1]);
                std::sync::atomic::fence(std::sync::atomic::Ordering::Release);
                entry.write_volatile(key);
            }
            return;
        }
        self.dxr.not_entered[1] += 1;
    }

    /// The table entry of the structure at `address` with root `root` emptied
    /// (its key 0): the walk goes the title's way there again.
    fn dxr_unpublish(&mut self, address: u64, root: [u32; 2]) {
        use victory_shader::emit::{DXR_ENTRIES, DXR_HASH, DXR_HASH_SHIFT};
        let absolute = (address >> 3) + u64::from(root[0]);
        let key = absolute as u32;
        let Some((_, data)) = self.dxr.table else { return };
        if key == 0 {
            return;
        }
        let table = data as *mut u32;
        let hash = key.wrapping_mul(DXR_HASH) >> DXR_HASH_SHIFT;
        for n in 0..4 {
            let place = (hash + n) & (DXR_ENTRIES - 1);
            // SAFETY: inside the table's mapping.
            unsafe {
                let entry = table.add(4 * place as usize);
                if entry.read_volatile() == key {
                    entry.write_volatile(0);
                    return;
                }
            }
        }
    }

    /// The guided walk's table made, if it is not yet: [`DXR_ENTRIES`] entries
    /// and after them the live switch's dword (`crate::live::ray_tracing`).
    ///
    /// [`DXR_ENTRIES`]: victory_shader::emit::DXR_ENTRIES
    fn dxr_table(&mut self) -> bool {
        use victory_shader::emit::{DXR_ENTRIES, DXR_STRUCTURES};
        if self.dxr.table.is_some() {
            return true;
        }
        let bytes = DXR_ENTRIES as usize * 16 + 16;
        let Some((buffer, data)) = host().and_then(|h| h.shared_buffer(bytes as u64)) else { return false };
        // SAFETY: the new mapping, zeroed: every entry empty, the switch 0.
        unsafe { std::ptr::write_bytes(data, 0, bytes) };
        self.dxr.table = Some((buffer, data as usize));
        self.dxr.structures = vec![None; DXR_STRUCTURES as usize];
        self.dxr.counters = host()
            .and_then(|h| windows::core::Interface::cast::<ID3D12Device5>(&h.device).ok())
            .and_then(|device| dxr::card_buffer(&device, 256, D3D12_RESOURCE_STATE_COMMON));
        true
    }

    /// The live switch into the table's last dword: 0 as translated (the
    /// host's structures guide), 1 the title's walk only, 2 off, 3 the host's
    /// structures alone.
    fn dxr_switch(&mut self) {
        let word = match crate::live::ray_tracing() {
            Some(0) => 2,
            Some(1) => 1,
            Some(3) => 3,
            _ => 0,
        };
        if let Some((_, data)) = self.dxr.table {
            // SAFETY: the table's mapping has the switch's dword after its
            // entries.
            unsafe { (data as *mut u32).add(victory_shader::emit::DXR_ENTRIES as usize * 4).write_volatile(word) };
        }
    }

    /// The table's view and the structures' views in heap `heap`'s run from
    /// `base` (`victory_shader::emit::MEMORY_RUN`), those set since it was
    /// last written.
    fn dxr_views(&mut self, heap: &HostHeap, base: u32) {
        // (Made before any structure: the switch works from the start.)
        if !self.dxr_table() {
            return;
        }
        let Some((table, _)) = self.dxr.table.as_ref() else { return };
        let key = std::ptr::from_ref(heap) as usize;
        self.dxr.views_asked.insert(key, self.frame);
        let seen = self.dxr.views.get(&key).copied();
        if seen == Some(self.dxr.generation) {
            return;
        }
        if let (Some(frames), Some(_)) = (dxr::enter_early(), seen)
            && self.frame % frames != 0
        {
            return;
        }
        let run = victory_shader::emit::MEMORY_RUN;
        if seen.is_none() {
            heap.raw_uav_at(base + run, table, 0, u64::from(victory_shader::emit::DXR_ENTRIES) * 16 + 16);
            if let Some(counters) = &self.dxr.counters {
                heap.raw_uav_at(base + run + 1 + victory_shader::emit::DXR_STRUCTURES, counters, 0, 256);
            }
        }
        for (n, s) in self.dxr.structures.iter().enumerate() {
            if let Some((structure, set)) = s
                && seen.is_none_or(|seen| *set > seen)
            {
                heap.accel_srv_at(base + run + 1 + n as u32, structure);
            }
        }
        self.dxr.views.insert(key, self.dxr.generation);
    }

    /// Once a frame: reads that came are looked at; a line every ten
    /// seconds.
    fn dxr_frame(&mut self) {
        let frame = self.frame;
        self.dxr_switch();
        // Bottom levels whose builds are done by now: the top levels made
        // from here on name them, and the ones they replace are let go.
        let (done, later): (Vec<_>, Vec<_>) =
            std::mem::take(&mut self.dxr.building).into_iter().partition(|b| b.0 <= frame && self.dxr.landed(b.3));
        // (A build whose list never ran is given up, its structure let go.)
        let (later, lost): (Vec<_>, Vec<_>) = later.into_iter().partition(|b| b.0 + dxr::GIVE_UP > frame);
        self.dxr.building = later;
        for (_, _, gone, _) in lost {
            self.dxr.given_up += 1;
            self.dxr.retired.push((frame, gone.structure));
            if let Some(single) = gone.single {
                self.dxr.retired.push((frame, single));
            }
        }
        for (_, bottom, new, _) in done {
            if let Some(old) = self.dxr.built.insert(bottom, new) {
                self.dxr.retired.push((frame, old.structure));
                if let Some(single) = old.single {
                    self.dxr.retired.push((frame, single));
                }
            }
        }
        self.dxr_let_go();
        // Structures whose builds are done by now: published.
        let (due, later): (Vec<_>, Vec<_>) =
            std::mem::take(&mut self.dxr.unpublished).into_iter().partition(|u| u.0 <= frame && self.dxr.landed(u.5));
        let (later, lost): (Vec<_>, Vec<_>) = later.into_iter().partition(|u| u.0 + dxr::GIVE_UP > frame);
        self.dxr.unpublished = later;
        for lost in lost {
            self.dxr.given_up += 1;
            self.dxr.retired.push((frame, lost.4));
        }
        for (at, address, kind, root, structure, _) in due {
            self.dxr_publish(address, kind, root, &structure);
            if kind == victory_shader::emit::DXR_TOP
                && let Some((before, _)) = self.dxr.top_built.insert(address, (at.saturating_sub(dxr::PUBLISH_AFTER), root))
            {
                self.dxr.top_gone.push((frame + dxr::KEEP_AFTER, before));
            }
        }
        self.dxr_enter();
        if matches!(crate::live::ray_tracing(), None | Some(2 | 3)) {
            let stale: Vec<(u64, [u32; 2])> = self
                .dxr
                .top_built
                .iter()
                .filter(|(_, (built, _))| built + dxr::STALE_TOP < frame)
                .map(|(&a, &(_, root))| (a, root))
                .collect();
            for (address, root) in stale {
                self.dxr_unpublish(address, root);
                if let Some((built, _)) = self.dxr.top_built.remove(&address) {
                    self.dxr.top_gone.push((frame + dxr::KEEP_AFTER, built));
                }
            }
        }
        // (A bottom level let go at `at` may be named by any top level built
        // at or before it: kept until every published one is newer.)
        self.dxr.top_gone.retain(|&(until, _)| until > frame);
        // (And the top levels built and not entered yet: their builds name
        // the bottom levels of their frame, and they are entered once the
        // card has run them, which can be seconds later. A bottom level let
        // go for room after such a top level's build was freed before it was
        // entered: run 20261127-163117 lost the card in a trace dispatch with
        // 744 bottom levels just let go, the switch back on.)
        let oldest_top = self
            .dxr
            .top_built
            .values()
            .map(|&(b, _)| b)
            .chain(self.dxr.top_gone.iter().map(|&(_, b)| b))
            .chain(
                self.dxr
                    .unpublished
                    .iter()
                    .filter(|u| u.2 == victory_shader::emit::DXR_TOP)
                    .map(|u| u.0.saturating_sub(dxr::PUBLISH_AFTER)),
            )
            .min()
            .unwrap_or(u64::MAX);
        self.dxr.retired.retain(|(at, _)| at + dxr::KEEP_AFTER > frame || oldest_top <= *at);
        let (ready, waiting): (Vec<_>, Vec<_>) =
            std::mem::take(&mut self.dxr.pending).into_iter().partition(|p| p.due <= frame && self.dxr.landed(p.token));
        let (waiting, lost): (Vec<_>, Vec<_>) = waiting.into_iter().partition(|p| p.due + dxr::GIVE_UP > frame);
        // (The card never ran their copies: their buffers go back too.)
        self.dxr.given_up += lost.len() as u64;
        self.dxr.pending = waiting;
        for p in lost.into_iter().filter(|p| p.size > 0) {
            self.dxr.pool.give_back(true, p.size, p.buffer, p.data);
        }
        for p in ready {
            match &p.what {
                dxr::What::Top(top) => {
                    let header: Option<Vec<u32>> = (0..16).map(|n| p.word(n)).collect();
                    let top_level = header.as_ref().is_some_and(|h| h[0] & 0xffff == 6 && h[0] >> 31 == 0);
                    let (Some(h), true) = (header.clone(), top_level) else {
                        // Not a top level (another view the program is
                        // given): not read again.
                        if let (Some(t), Some(_)) = (self.dxr.tops.get_mut(top), &header) {
                            t.read = u64::MAX / 2;
                        }
                        continue;
                    };
                    let (first, count) = (u64::from(h[13]), h[3]);
                    let mut instances = Vec::new();
                    for k in 0..u64::from(count) {
                        let at = first + 128 * k;
                        if at + 128 > dxr::TOP_BYTES {
                            break;
                        }
                        let words: Option<Vec<u32>> = (0..28).map(|n| p.word(at / 4 + n)).collect();
                        let Some(w) = words else { continue };
                        // (Records past the live ones hold the largest float.)
                        if w[15] == 0 || w[14] == 0x7f7f_ffff {
                            continue;
                        }
                        let mut words = [0u32; 14];
                        words.copy_from_slice(&w[..14]);
                        let mut inverse = [0u32; 12];
                        inverse.copy_from_slice(&w[16..28]);
                        instances.push(dxr::Instance { words, inverse, bottom: u64::from(w[15]) << 8, record: k as u32, root_pointer: w[14] });
                    }
                    self.dxr.stats[1] += 1;
                    if let Some(t) = self.dxr.tops.get_mut(top) {
                        t.instances = instances;
                        t.count = count;
                        t.root = [h[12], h[13]];
                    }
                }
                dxr::What::Counters => {
                    let now: Option<Vec<u32>> = (0..3).map(|n| p.word(n)).collect();
                    if let Some(now) = now {
                        for k in 0..3 {
                            self.dxr.counted_period[k] += u64::from(now[k].wrapping_sub(self.dxr.counted.1[k]));
                            self.dxr.counted.1[k] = now[k];
                        }
                    }
                }
                dxr::What::Bottoms(list) => {
                    for (k, &b) in list.iter().enumerate() {
                        let header: Option<Vec<u32>> = (0..16).map(|n| p.word(16 * k as u64 + n)).collect();
                        if let Some(h) = header {
                            let mut words = [0u32; 16];
                            words.copy_from_slice(&h);
                            self.dxr.bottoms.insert(b, dxr::Bottom { header: words, frame });
                            self.dxr.stats[2] += 1;
                        } else {
                            self.dxr.stats[3] += 1;
                        }
                    }
                }
            }
            if p.size > 0 {
                self.dxr.pool.give_back(true, p.size, p.buffer, p.data);
            }
        }
        // Buffers lent to builds the card has run, free again.
        {
            let state = &mut self.dxr;
            let marks = state.marks.as_ref().map(|m| m.1);
            let landed = |token: u32| {
                marks.is_some_and(|data| {
                    // SAFETY: as `Dxr::landed`.
                    unsafe { (data as *const u32).add((token % dxr::MARKS) as usize).read_volatile() == token }
                })
            };
            state.pool.settle(landed, frame);
        }
        let now = std::time::Instant::now();
        let since = self.dxr.reported.map(|r| now.duration_since(r));
        if since.is_some_and(|s| s.as_secs() < 10) {
            return;
        }
        self.dxr.reported = Some(now);
        let mut line = String::new();
        for (top, t) in &self.dxr.tops {
            if t.read + 120 < frame || t.count == 0 {
                continue;
            }
            let mut bottoms: Vec<u64> = t.instances.iter().map(|i| i.bottom).collect();
            bottoms.sort_unstable();
            bottoms.dedup();
            let (mut usable, mut statics, mut elements, mut unknown, mut built) = (0, 0, 0u64, 0, 0);
            // The ones that are not usable, by why, and the marks of those
            // whose marks are not known (each with how many).
            let mut why = [0usize; dxr::WHY_NOT.len()];
            let mut marks: Vec<(u32, usize)> = Vec::new();
            for b in &bottoms {
                match self.dxr.bottoms.get(b) {
                    Some(x) if dxr::usable_bottom(&x.header) => {
                        usable += 1;
                        elements += u64::from(x.header[3]);
                        statics += usize::from(dxr::static_bottom(&x.header));
                        built += usize::from(self.dxr.built.contains_key(b));
                    }
                    Some(x) => {
                        let w = dxr::why_not(&x.header);
                        why[w] += 1;
                        if w == 2 || w == 0 {
                            match marks.iter_mut().find(|m| m.0 == x.header[0]) {
                                Some(m) => m.1 += 1,
                                None => marks.push((x.header[0], 1)),
                            }
                        }
                    }
                    None => unknown += 1,
                }
            }
            // The instances by what their bottom level is: in a host bottom
            // level (so in the host top level, if its matrix inverts), usable
            // and not built yet, not usable, not read yet.
            let mut of = [0usize; 4];
            for i in &t.instances {
                let n = match self.dxr.bottoms.get(&i.bottom) {
                    _ if self.dxr.built.contains_key(&i.bottom) => 0,
                    Some(x) if dxr::usable_bottom(&x.header) => 1,
                    Some(_) => 2,
                    None => 3,
                };
                of[n] += 1;
            }
            let placed = t.built.as_ref().map_or(0, |b| b.1);
            line += &format!(
                "; top {top:#x}: {} instances ({placed} in its host top level), {} bottom levels ({usable} usable, {statics} of them static, {built} built; {unknown} not read yet), {elements} elements",
                t.count,
                bottoms.len(),
            );
            let whys: Vec<String> =
                why.iter().zip(dxr::WHY_NOT).filter(|(n, _)| **n > 0).map(|(n, what)| format!("{n} {what}")).collect();
            marks.sort_by_key(|m| std::cmp::Reverse(m.1));
            let marks: Vec<String> = marks.iter().take(6).map(|(word, n)| format!("{n} of {word:#010x}")).collect();
            line += &format!(
                "; not usable: {}{}; of its {} instances read, {} have a host bottom level, {} one that is usable and not built yet, {} one that is not usable, {} one not read yet",
                if whys.is_empty() { "none".to_owned() } else { whys.join(", ") },
                if marks.is_empty() { String::new() } else { format!(" (first words: {})", marks.join(", ")) },
                t.instances.len(),
                of[0],
                of[1],
                of[2],
                of[3],
            );
        }
        let (bytes, triangles) = self.dxr.built.values().fold((0u64, 0u64), |a, b| (a.0 + b.bytes, a.1 + b.triangles));
        let [no_number, no_place] = std::mem::take(&mut self.dxr.not_entered);
        let entered = std::mem::take(&mut self.dxr.entered);
        let gathered = std::mem::take(&mut self.dxr.gathered);
        line += &format!(
            "; the walk's table: {} structures have a number of {} ({} given back), since the last line {no_number} not entered for want of a number and {no_place} for want of a place; {} bottom levels let go for room; waiting for the card's marks: {} reads, {} builds, {} structures to enter, {} given up; {} table entries set once their views were written, {:.1} frames after their structures (at most {}), {} waiting; the native builder's work on the card: {} gathers ({} elements), {} structure copies, {} that could not be",
            self.dxr.numbers.len(),
            victory_shader::emit::DXR_STRUCTURES,
            self.dxr.free_numbers.len(),
            std::mem::take(&mut self.dxr.let_go),
            self.dxr.pending.len(),
            self.dxr.building.len(),
            self.dxr.unpublished.len(),
            std::mem::take(&mut self.dxr.given_up),
            entered[0],
            entered[1] as f64 / entered[0].max(1) as f64,
            entered[2],
            self.dxr.entering.len(),
            gathered[0],
            gathered[1],
            gathered[2],
            gathered[3],
        );
        let cost = std::mem::take(&mut self.dxr.cost);
        let per = |us: u64| us as f64 / 1e3 / (self.dxr.period[0].max(1)) as f64;
        let (made, reused) = (std::mem::take(&mut self.dxr.pool.made), std::mem::take(&mut self.dxr.pool.reused));
        line += &format!(
            "; buffers for builds and reads since the last line: {} made, {} taken again; the recording thread per bottom level built: {:.2} ms making buffers, {:.2} reading its elements, {:.2} recording its pass and build, {:.2} its own top level; {:.1} ms on top levels since the last line",
            made,
            reused,
            per(cost[0]),
            per(cost[1]),
            per(cost[2]),
            per(cost[3]),
            cost[4] as f64 / 1e3,
        );
        let [made, made_triangles, tops, us, scratch] = std::mem::take(&mut self.dxr.period);
        let [queries, hits, top_queries] = std::mem::take(&mut self.dxr.counted_period);
        let secs = since.map_or(10.0, |s| s.as_secs_f64().max(1.0));
        let switch = match crate::live::ray_tracing() {
            Some(0) => "off",
            Some(1) => "the title's walk only",
            Some(3) => "the host's structures alone",
            _ => "as started",
        };
        tracing::info!(
            "native ray tracing, the guided walk (VICTORY_DXR_GUIDED=1; live switch: {switch}): {:.0} ray queries a second ({:.0} at top levels, {:.0} at bottom levels), {:.1}% of them hit",
            queries as f64 / secs,
            top_queries as f64 / secs,
            queries.saturating_sub(top_queries) as f64 / secs,
            if queries > 0 { 100.0 * hits as f64 / queries as f64 } else { 0.0 },
        );
        tracing::info!(
            "native ray tracing (VICTORY_DXR=1, building, nothing traced yet): {} host bottom levels kept ({} triangles at most, {} MB of {} allowed), {} left out (not all in title memory on the card), {} frames without building for the card's memory (over {}% of its budget); since the last line {made} bottom levels built ({made_triangles} triangles), {tops} top levels, {:.1} ms recording them, the most scratch {} MB; {} top levels read, {} bottom-level headers{line}",
            self.dxr.built.len(),
            triangles,
            bytes >> 20,
            dxr::MOST_KEPT >> 20,
            self.dxr.left_out,
            self.dxr.card_full,
            (dxr::CARD_SHARE * 100.0) as u32,
            us as f64 / 1000.0,
            scratch >> 20,
            self.dxr.stats[0],
            self.dxr.stats[2],
        );
    }
}

/// `VICTORY_BVH_TRACE=1`: the sizes the builder is asked for (its program of
/// one thread that stores a structure's compacted size) and when title memory
/// has them, and the copies to a place of that size.
#[cfg(feature = "diagnostics")]
mod trace {
    use super::*;

    #[derive(Default)]
    pub(super) struct Trace {
        /// Frame, where the size goes, the structure, what title memory had.
        asked: Vec<(u64, u64, u64, u64)>,
        /// By structure, what its size was when last seen to change.
        sizes: HashMap<u64, (u64, u64)>,
        said: u32,
    }

    fn on() -> bool {
        static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *ON.get_or_init(|| std::env::var_os("VICTORY_BVH_TRACE").is_some_and(|v| v == "1"))
    }

    fn value(at: u64) -> u64 {
        let mut b = [0u8; 8];
        crate::host::copy_mapped(b.as_mut_ptr(), at as *const u8, 8, false);
        u64::from_le_bytes(b)
    }

    impl Trace {
        fn say(&mut self, text: String) {
            self.said += 1;
            if self.said < 6000 {
                tracing::info!("bvh trace: {text}");
            }
        }

        pub(super) fn look(&mut self, frame: u64) {
            if !on() {
                return;
            }
            let mut gone = Vec::new();
            for i in 0..self.asked.len() {
                let (then, at, structure, was) = self.asked[i];
                let now = value(at);
                if now != was {
                    self.sizes.insert(structure, (now, frame));
                    gone.push(i);
                    self.say(format!("frame {frame}: size of {structure:#x} at {at:#x} asked in {then}: {was:#x} -> {now:#x}"));
                } else if frame > then + 60 {
                    gone.push(i);
                    self.say(format!("frame {frame}: size of {structure:#x} at {at:#x} asked in {then}: still {was:#x}"));
                }
            }
            for i in gone.into_iter().rev() {
                self.asked.swap_remove(i);
            }
        }

        pub(super) fn dispatch(
            &mut self,
            frame: u64,
            list: &HostList,
            addresses: &[u64],
            written: &[u64],
            constants: &[u32],
            threads: Option<[u32; 3]>,
        ) {
            if !on() {
                return;
            }
            let _ = addresses;
            let list = std::ptr::from_ref(list) as usize & 0xFFFF_FFFF;
            if threads == Some([1, 1, 1])
                && let [lo, hi, mode, ..] = *constants
                && (1..=3).contains(&mode)
                && hi < 0x20
                && let Some(&at) = written.first()
                && crate::shadow::in_buffer(at, 1).is_some_and(|b| b.1 <= BACK)
            {
                let structure = u64::from(lo) | u64::from(hi) << 32;
                let was = value(at);
                self.say(format!("frame {frame} list {list:x}: asks mode {mode} of {structure:#x} to {at:#x}, there {was:#x}"));
                if mode == 1 {
                    self.asked.push((frame, at, structure, was));
                }
            } else if threads == Some([32, 1, 1])
                && let [from, to] = *written
                && [from, to].iter().all(|&a| a >= 1 << 32 && crate::shadow::in_buffer(a, 1).is_none())
            {
                let known = self.sizes.get(&from).copied();
                self.say(format!("frame {frame} list {list:x}: copies {from:#x} to {to:#x} constants {:x?}, its size was {known:x?}", &constants[..constants.len().min(4)]));
            }
        }
    }
}

/// `VICTORY_MEMORY_DUMP=<folder>`: when a file `go` appears there, the arenas
/// as the card holds them are copied out and written (`arena-<n>.bin`), with
/// `pages.txt` (title page, arena, offset) and the structures the walking
/// programs were given: to look at the structures offline.
#[cfg(feature = "diagnostics")]
mod dump {
    use super::*;

    #[derive(Default)]
    pub(super) struct Dump {
        walked: std::collections::BTreeSet<u64>,
        /// Views the builder was given to write outside buffers: the first
        /// and the last frame, how often.
        pub(super) built: std::collections::BTreeMap<u64, (u64, u64, u64)>,
        /// The frame it is written at, and the copies.
        due: Option<(u64, Vec<(ID3D12Resource, usize)>)>,
        done: bool,
    }

    impl Dump {
        pub(super) fn walked(&mut self, addresses: &[u64]) {
            if !self.done && self.walked.len() < 64 {
                self.walked.extend(addresses);
            }
        }
    }

    fn folder() -> Option<&'static std::path::PathBuf> {
        static FOLDER: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();
        FOLDER.get_or_init(|| std::env::var_os("VICTORY_MEMORY_DUMP").map(Into::into)).as_ref()
    }

    fn turn(buffer: &ID3D12Resource, before: D3D12_RESOURCE_STATES, after: D3D12_RESOURCE_STATES) -> D3D12_RESOURCE_BARRIER {
        D3D12_RESOURCE_BARRIER {
            Type: D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
            Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
            Anonymous: D3D12_RESOURCE_BARRIER_0 {
                Transition: std::mem::ManuallyDrop::new(D3D12_RESOURCE_TRANSITION_BARRIER {
                    // SAFETY: the same pointer, not counted; the barrier is
                    // used while the buffer lives.
                    pResource: unsafe { std::mem::transmute_copy(buffer) },
                    Subresource: D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                    StateBefore: before,
                    StateAfter: after,
                }),
            },
        }
    }

    impl Memory {
        /// At a list's first use: the copies are recorded into it.
        pub(super) fn dump(&mut self, list: &HostList, _: &[u64]) {
            let Some(folder) = folder() else { return };
            if let Some((due, _)) = &self.dump.due {
                if self.frame < *due {
                    return;
                }
                let copies = self.dump.due.take().map(|d| d.1).unwrap_or_default();
                let mut text = String::new();
                for a in &self.dump.walked {
                    text += &format!("walked {a:#x}\n");
                }
                for (a, (first, last, n)) in &self.dump.built {
                    text += &format!("built {a:#x} {first} {last} {n}\n");
                }
                for (a, p) in &self.packed {
                    text += &format!("packed {a:#x} {} {}\n", p.len, p.frame);
                }
                text += &format!("frame {}\n", self.frame);
                for (page, held) in &self.pages {
                    let arena = self.arenas.iter().position(|a| a.0 == held.copy).unwrap_or(usize::MAX);
                    let at = held.at - self.arenas[arena].1;
                    text += &format!("{:#x} {arena} {at:#x}\n", page * PAGE);
                }
                let _ = std::fs::write(folder.join("pages.txt"), text);
                for (n, (_, data)) in copies.iter().enumerate() {
                    // SAFETY: a mapping of an arena's bytes; the copies ran
                    // frames ago.
                    let bytes = unsafe { std::slice::from_raw_parts(*data as *const u8, ARENA as usize) };
                    let _ = std::fs::write(folder.join(format!("arena-{n}.bin")), bytes);
                }
                tracing::info!(arenas = copies.len(), "title memory by address: the arenas are written out");
                return;
            }
            if self.dump.done || !folder.join("go").exists() {
                return;
            }
            self.dump.done = true;
            let Some(host) = host() else { return };
            let mut copies = Vec::new();
            for &(n, _) in &self.arenas {
                let buffer = &self.copies[n].buffer;
                let Some((to, data)) = host.shared_buffer(ARENA) else { break };
                // SAFETY: an open list; live buffers of those sizes. The list
                // has not used the arena yet.
                unsafe {
                    list.list.ResourceBarrier(&[turn(buffer, D3D12_RESOURCE_STATE_COMMON, D3D12_RESOURCE_STATE_COPY_SOURCE)]);
                    list.list.CopyBufferRegion(&to, 0, buffer, 0, ARENA);
                    list.list.ResourceBarrier(&[turn(buffer, D3D12_RESOURCE_STATE_COPY_SOURCE, D3D12_RESOURCE_STATE_COMMON)]);
                }
                list.keep(&to);
                copies.push((to, data as usize));
            }
            self.dump.due = Some((self.frame + 12, copies));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_piece_takes_what_changed() {
        let mut title = [7u8; PIECE];
        let mut ours = vec![0u8; PIECE];
        assert_eq!(merge(&title, &mut ours, true), PIECE / 4);
        assert_eq!(ours, title);
        assert_eq!(merge(&title, &mut ours, false), 0);
        // A dword of it.
        title[41] = 8;
        assert_eq!(merge(&title, &mut ours, false), 1);
        assert_eq!(ours, title);
    }

    #[test]
    fn a_top_level_record_gives_the_host_instance_its_transform() {
        // A record of dump 5 (dynamic bottom level, no node space): dwords
        // 0-11 the instance's transform, 16-27 its inverse.
        let t = [13.8575, -6.6865, 0.0, -35.1808, 6.6865, 13.8575, 0.0, -189.2898, 0.0, 0.0, 15.3864, 8.3946];
        let inverse = dxr::invert(&t).unwrap();
        let mut tail = [0u32; 12];
        let [a, b, c, x, d, e, f, y, g, h, i, z] = inverse;
        for (to, v) in tail.iter_mut().zip([x, y, z, a, b, c, d, e, f, g, h, i]) {
            *to = v.to_bits();
        }
        let back = dxr::invert(&dxr::world_to_node(&tail)).unwrap();
        for (k, (want, got)) in t.iter().zip(back).enumerate() {
            assert!((want - got).abs() < 1e-3 * want.abs().max(1.0), "{k}: {want} {got}");
        }
        // The record's own numbers (rounded as the dump shows them).
        let shown = [7.4056f32, 10.0864, -0.5456, 0.0585, 0.0282, 0.0, -0.0282, 0.0585, 0.0, 0.0, 0.0, 0.065];
        for (k, v) in shown.iter().enumerate() {
            assert!((f32::from_bits(tail[k]) - v).abs() < 2e-4, "{k}");
        }
    }

    #[test]
    fn a_packed_structure_is_known_by_its_header() {
        let header = |version: u32, nodes: u32| u64::from(version) | u64::from(nodes) << 32;
        assert_eq!(packed_len(header(0x8102_0006, 0x9a1a)), Some(0x9a1b * 64));
        // Unpacked, a top level, another version, no size, too big.
        assert_eq!(packed_len(header(0x8002_0006, 0x9a1a)), None);
        assert_eq!(packed_len(header(0x0000_0006, 5)), None);
        assert_eq!(packed_len(header(0x8102_0007, 5)), None);
        assert_eq!(packed_len(header(0x8102_0006, 0)), None);
        assert_eq!(packed_len(header(0x8102_0006, 1 << 21)), None);
    }
}

/// `VICTORY_REHYDRATE_CAPTURE=<folder>` (diagnostics): what the console's four
/// unpacking programs are given, written out for the native builder's decoder
/// (`C:\Users\Corgan\victory-mcp\DXR-PLAN.md`, plan #2). For each of their
/// dispatches, up to [`capture::DISPATCHES`] of each: its root views and root
/// constants (`dispatches.txt`); title memory as the CPU holds it (the packed
/// form: what the builder stores stays on the card) from each root view, and
/// from each address the first [`capture::SCAN`] bytes of a view hold, the
/// mesh buffers LeafRehydration reads through pointers (`windows.bin`, indexed
/// by `windows.txt`: address, offset, bytes). Taken with
/// `VICTORY_MEMORY_DUMP` in the same run: the arenas hold what the programs
/// made of it.
#[cfg(feature = "diagnostics")]
pub(crate) mod capture {
    use std::collections::HashMap;
    use std::io::Write;
    use std::sync::Mutex;

    /// The programs, by the hash the log names them by.
    const PROGRAMS: [(&str, &str); 4] = [
        ("4ddd6f9702fab2a7", "DecompressHeader"),
        ("28918b7150a7aad5", "Box32Rehydration"),
        ("caa78b8ca62bc3d6", "Box16Rehydration"),
        ("5e68767498c57eb6", "LeafRehydration"),
    ];
    /// Dispatches written of each program.
    pub(super) const DISPATCHES: u32 = 300;
    /// Bytes taken from each root view, and from each address found in them.
    const WINDOW: usize = 64 << 10;
    /// Bytes of a view looked through for addresses.
    pub(super) const SCAN: usize = 16 << 10;
    /// The most bytes written in all.
    const MOST: u64 = 1536 << 20;
    /// The most bytes taken of one span a geometry description names.
    const SPAN: u64 = 1 << 20;

    #[derive(Default)]
    struct State {
        counts: HashMap<&'static str, u32>,
        /// Window addresses taken.
        taken: std::collections::HashSet<u64>,
        written: u64,
        files: Option<(std::fs::File, std::fs::File, std::fs::File)>,
        failed: bool,
        done: bool,
    }

    static STATE: Mutex<Option<State>> = Mutex::new(None);

    fn folder() -> Option<&'static std::path::PathBuf> {
        static FOLDER: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();
        FOLDER.get_or_init(|| std::env::var_os("VICTORY_REHYDRATE_CAPTURE").map(Into::into)).as_ref()
    }

    /// Whether anything is captured in this run.
    pub(crate) fn on() -> bool {
        folder().is_some()
    }

    /// An address that can be title GPU memory (what the structures' and the
    /// meshes' addresses look like: some GB up, under 2^40).
    fn plausible(a: u64) -> bool {
        (1 << 32..1 << 40).contains(&a) && a % 4 == 0
    }

    impl State {
        /// Title memory from `address`, once per address.
        fn window(&mut self, address: u64) -> Option<Vec<u8>> {
            if !self.taken.insert(address) || self.written >= MOST {
                return None;
            }
            let mut bytes = vec![0u8; WINDOW];
            let got = crate::host::read_there(address as *const u8, &mut bytes);
            if got == 0 {
                return None;
            }
            let (_, windows, index) = self.files.as_mut()?;
            let at = windows.metadata().map(|m| m.len()).unwrap_or(0);
            let _ = windows.write_all(&bytes);
            let _ = writeln!(index, "{address:#x} {at:#x} {got:#x}");
            self.written += bytes.len() as u64;
            Some(bytes)
        }
    }

    /// A dispatch of program `hash` with these root views and constants.
    pub(crate) fn dispatch(hash: &str, frame: u64, views: &[u64], constants: &[u32]) {
        let Some(&(hash, name)) = PROGRAMS.iter().find(|p| p.0 == hash) else { return };
        let Some(folder) = folder() else { return };
        let mut guard = STATE.lock().unwrap_or_else(|e| e.into_inner());
        let state = guard.get_or_insert_with(State::default);
        if state.failed {
            return;
        }
        if state.files.is_none() {
            let _ = std::fs::create_dir_all(folder);
            let open = |n: &str| std::fs::File::create(folder.join(n));
            match (open("dispatches.txt"), open("windows.bin"), open("windows.txt")) {
                (Ok(a), Ok(b), Ok(c)) => state.files = Some((a, b, c)),
                _ => {
                    state.failed = true;
                    tracing::warn!(folder = %folder.display(), "VICTORY_REHYDRATE_CAPTURE: its files could not be made");
                    return;
                }
            }
        }
        let n = state.counts.entry(hash).or_default();
        if *n >= DISPATCHES {
            return;
        }
        *n += 1;
        let line = format!(
            "{name} {hash} frame {frame} views {} constants {}\n",
            views.iter().map(|v| format!("{v:#x}")).collect::<Vec<_>>().join(","),
            constants.iter().map(|c| format!("{c:#x}")).collect::<Vec<_>>().join(","),
        );
        if let Some((d, _, _)) = state.files.as_mut() {
            let _ = d.write_all(line.as_bytes());
        }
        for &view in views.iter().filter(|&&v| plausible(v)) {
            let Some(bytes) = state.window(view) else { continue };
            // The addresses it holds (the meshes' buffers, for LeafRehydration).
            let mut found: Vec<u64> = bytes[..SCAN]
                .chunks_exact(8)
                .map(|q| u64::from_le_bytes(q.try_into().unwrap_or_default()))
                .filter(|&a| plausible(a))
                .collect();
            found.sort_unstable();
            found.dedup();
            // (At most so many a view: some 16 MB.)
            found.truncate(256);
            for a in found {
                state.window(a);
            }
            // A view of geometry descriptions (`crate::records`): all of what
            // they name, the packed vertices' scale and offset before them
            // included (a window from the pointer alone missed it).
            let mut title = |address: u64, bytes: &mut [u8]| crate::host::read_there(address as *const u8, bytes) == bytes.len();
            for g in crate::records::read_records(view, 4096, &mut title) {
                for (start, len) in g.spans(&mut title) {
                    let len = len.min(SPAN);
                    for at in (start..start + len).step_by(WINDOW) {
                        state.window(at);
                    }
                }
            }
        }
        if state.counts.values().all(|&c| c >= DISPATCHES) || state.written >= MOST {
            if !std::mem::replace(&mut state.done, true) {
                tracing::info!(bytes = state.written, "VICTORY_REHYDRATE_CAPTURE: done");
                // The arenas now, while what these dispatches made is still
                // there (a dump 330 frames later found most of it written
                // over): `VICTORY_MEMORY_DUMP`'s go.
                if let Some(dump) = std::env::var_os("VICTORY_MEMORY_DUMP") {
                    let _ = std::fs::write(std::path::Path::new(&dump).join("go"), "go");
                }
            }
        }
    }
}

/// `VICTORY_BUILDER_CAPTURE=<folder>` (diagnostics): every dispatch the title
/// asks of the console's structure builder (`XboxBvhBuilder.dll`), whether the
/// builder runs or not (a plain start drops them all): what a native builder
/// has to do in its place (`C:\Users\Corgan\victory-mcp\DXR-PLAN.md`, plan #2).
///
/// - `programs.txt`: each program once, by hash, with what it reads (its used
///   resources: kind, root, slot, stride).
/// - `dispatches.txt`: the first [`builder_capture::EACH`] dispatches of each
///   program: frame, groups, whether direct or indirect, whether it ran, its
///   root views and constants (by slot).
/// - `frames.txt`: per frame, how many dispatches of each program.
/// - `windows.bin` / `windows.txt` (address, offset, bytes): title memory as the
///   CPU holds it, [`builder_capture::WINDOW`] bytes from each root view of
///   those dispatches, and what geometry descriptions there name
///   (`crate::records`).
#[cfg(feature = "diagnostics")]
pub(crate) mod builder_capture {
    use std::collections::{BTreeMap, HashMap, HashSet};
    use std::io::Write;
    use std::sync::Mutex;

    /// Dispatches written of each program.
    pub(super) const EACH: u32 = 60;
    /// Bytes taken from each root view.
    pub(super) const WINDOW: usize = 64 << 10;
    /// The most bytes of windows in all.
    const MOST: u64 = 1024 << 20;
    /// Lines of `frames.txt`.
    const FRAMES: u32 = 20_000;
    /// The most bytes of one span a geometry description names.
    const SPAN: u64 = 1 << 20;

    struct Files {
        programs: std::fs::File,
        dispatches: std::fs::File,
        frames: std::fs::File,
        windows: std::fs::File,
        index: std::fs::File,
    }

    #[derive(Default)]
    struct State {
        files: Option<Files>,
        failed: bool,
        programs: HashSet<String>,
        counts: HashMap<String, u32>,
        frame: u64,
        this_frame: BTreeMap<String, u32>,
        frame_lines: u32,
        taken: HashSet<u64>,
        written: u64,
    }

    static STATE: Mutex<Option<State>> = Mutex::new(None);

    fn folder() -> Option<&'static std::path::PathBuf> {
        static FOLDER: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();
        FOLDER.get_or_init(|| std::env::var_os("VICTORY_BUILDER_CAPTURE").map(Into::into)).as_ref()
    }

    /// Whether this run captures.
    pub(crate) fn on() -> bool {
        folder().is_some()
    }

    impl State {
        fn window(&mut self, address: u64) {
            if !(1 << 32..1 << 40).contains(&address) || !self.taken.insert(address) || self.written >= MOST {
                return;
            }
            let mut bytes = vec![0u8; WINDOW];
            let got = crate::host::read_there(address as *const u8, &mut bytes);
            if got == 0 {
                return;
            }
            let Some(f) = self.files.as_mut() else { return };
            let at = f.windows.metadata().map(|m| m.len()).unwrap_or(0);
            let _ = f.windows.write_all(&bytes);
            let _ = writeln!(f.index, "{address:#x} {at:#x} {got:#x}");
            self.written += bytes.len() as u64;
        }

        fn close_frame(&mut self) {
            if self.this_frame.is_empty() || self.frame_lines >= FRAMES {
                self.this_frame.clear();
                return;
            }
            self.frame_lines += 1;
            let line = format!(
                "frame {} {}\n",
                self.frame,
                self.this_frame.iter().map(|(h, n)| format!("{h}={n}")).collect::<Vec<_>>().join(" ")
            );
            self.this_frame.clear();
            if let Some(f) = self.files.as_mut() {
                let _ = f.frames.write_all(line.as_bytes());
            }
        }
    }

    /// A dispatch of a builder program: `views` and `constants` by root slot;
    /// `used` what the program reads (asked once a program).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn dispatch(
        hash: &str,
        frame: u64,
        groups: [u32; 3],
        indirect: bool,
        ran: bool,
        views: &[(usize, u64)],
        constants: &[(usize, Vec<u32>)],
        used: impl FnOnce() -> Vec<victory_shader::UsedResource>,
    ) {
        let Some(folder) = folder() else { return };
        let mut guard = STATE.lock().unwrap_or_else(|e| e.into_inner());
        let state = guard.get_or_insert_with(State::default);
        if state.failed {
            return;
        }
        if state.files.is_none() {
            let _ = std::fs::create_dir_all(folder);
            let open = |n: &str| std::fs::File::create(folder.join(n));
            match (open("programs.txt"), open("dispatches.txt"), open("frames.txt"), open("windows.bin"), open("windows.txt")) {
                (Ok(programs), Ok(dispatches), Ok(frames), Ok(windows), Ok(index)) => {
                    state.files = Some(Files { programs, dispatches, frames, windows, index });
                }
                _ => {
                    state.failed = true;
                    tracing::warn!(folder = %folder.display(), "VICTORY_BUILDER_CAPTURE: its files could not be made");
                    return;
                }
            }
            tracing::info!(folder = %folder.display(), "VICTORY_BUILDER_CAPTURE: the structure builder's dispatches are written out");
        }
        if frame != state.frame {
            state.close_frame();
            state.frame = frame;
        }
        *state.this_frame.entry(hash.to_owned()).or_default() += 1;
        let mut records = Vec::new();
        if state.programs.insert(hash.to_owned()) {
            let used = used();
            let text: Vec<String> = used
                .iter()
                .map(|u| format!("{:?}/{:?}/slot{}/stride{}/reg{}", u.kind, u.root, u.slot, u.stride, u.lower_bound))
                .collect();
            if let Some(f) = state.files.as_mut() {
                let _ = writeln!(f.programs, "{hash} first frame {frame} reads {}", text.join(" "));
            }
        }
        let n = state.counts.entry(hash.to_owned()).or_default();
        if *n >= EACH {
            return;
        }
        *n += 1;
        let line = format!(
            "{hash} frame {frame} groups {}x{}x{} {} {} views {} constants {}\n",
            groups[0],
            groups[1],
            groups[2],
            if indirect { "indirect" } else { "direct" },
            if ran { "ran" } else { "dropped" },
            views.iter().map(|(s, v)| format!("{s}={v:#x}")).collect::<Vec<_>>().join(","),
            constants
                .iter()
                .map(|(s, c)| format!("{s}=[{}]", c.iter().map(|x| format!("{x:#x}")).collect::<Vec<_>>().join(",")))
                .collect::<Vec<_>>()
                .join(","),
        );
        if let Some(f) = state.files.as_mut() {
            let _ = f.dispatches.write_all(line.as_bytes());
        }
        for &(_, view) in views {
            state.window(view);
            records.push(view);
        }
        // What geometry descriptions there name.
        let mut title = |address: u64, bytes: &mut [u8]| crate::host::read_there(address as *const u8, bytes) == bytes.len();
        for view in records {
            for g in crate::records::read_records(view, 4096, &mut title) {
                for (start, len) in g.spans(&mut title) {
                    for at in (start..start + len.min(SPAN)).step_by(WINDOW) {
                        state.window(at);
                    }
                }
            }
        }
    }
}
