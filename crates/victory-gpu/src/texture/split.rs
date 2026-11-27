//! The hash of a big texture's memory cut in pieces that threads of their
//! own hash at once (`VICTORY_TEXTURE_FAST_HASH=1`; off by default).
//!
//! A refresh hashes the whole memory of a texture whose writes cannot be
//! seen (view memory, `VICTORY_TEXTURE_GUARD=0`): about 250 MB a frame in
//! the city, 16 ms of the frame for 37 ms of threads' time, since one big
//! texture is hashed on one thread whatever the others do. Here a texture
//! of [`SPLIT_FROM`] bytes or more is cut into [`PIECE`] pieces; each piece
//! takes [`super::digest`], the one for the whole before, and the pieces'
//! digests in their order, with the length, make the texture's. A change
//! of any byte changes its piece's digest as it changed the whole one
//! before, and a piece moved changes the order, so what is found changed
//! is what was found before. Smaller textures take [`super::digest`] as
//! before, bit for bit.
//!
//! The pieces go to threads kept for this alone ([`helpers`]), not to the
//! threads of `crate::shadow::wide`: a refresh hashes while it holds
//! `crate::watch::title_memory_reading`, which one thread must not take
//! twice, and a thread of `wide` waiting for its own jobs runs whatever
//! other job is queued, another texture's refresh among them. A helper
//! here only hashes bytes; the caller hashes pieces too and waits only for
//! pieces a helper has already begun.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use super::{Digest, digest};

/// Bytes of a piece.
pub(crate) const PIECE: usize = 1 << 20;

/// The least size whose hash is cut ([`texture_digest`]).
pub(crate) const SPLIT_FROM: usize = 4 * PIECE;

/// Whether the switch is on (`VICTORY_TEXTURE_FAST_HASH=1`).
pub(crate) fn on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        let on = std::env::var_os("VICTORY_TEXTURE_FAST_HASH").is_some_and(|v| v == "1");
        if on {
            tracing::info!(helpers = helper_count(), "texture hashes of 4 MiB and more cut in pieces of 1 MiB");
        }
        on
    })
}

/// How many helpers ([`helpers`]): `VICTORY_TEXTURE_HASH_THREADS`, else
/// a quarter of the logical processors, 2 to 4.
fn helper_count() -> usize {
    static N: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *N.get_or_init(|| {
        match std::env::var("VICTORY_TEXTURE_HASH_THREADS").ok().and_then(|v| v.trim().parse::<usize>().ok()) {
            Some(n) => n.clamp(1, 8),
            None => std::thread::available_parallelism().map_or(2, |n| n.get() / 4).clamp(2, 4),
        }
    })
}

/// The digest a texture's memory is told changed by: [`digest`] under
/// [`SPLIT_FROM`] bytes or with the switch off; else [`split_digest`].
pub(crate) fn texture_digest(bytes: &[u8]) -> Digest {
    if bytes.len() < SPLIT_FROM || !on() {
        return digest(bytes);
    }
    split_digest(bytes, true)
}

/// The digest of `bytes` cut into [`PIECE`] pieces, hashed by the helpers
/// and this thread (`wide`) or by this thread alone; the same either way.
pub(crate) fn split_digest(bytes: &[u8], wide: bool) -> Digest {
    let n = bytes.len().div_ceil(PIECE);
    let task = Arc::new(Task {
        at: bytes.as_ptr() as usize,
        len: bytes.len(),
        n,
        next: AtomicUsize::new(0),
        done: AtomicUsize::new(0),
        out: (0..2 * n).map(|_| AtomicU64::new(0)).collect(),
    });
    if wide && n > 1 {
        let helpers = helpers();
        let mut queue = helpers.queue.lock().unwrap_or_else(|e| e.into_inner());
        // One for each helper that could help: a helper that comes to it
        // late finds nothing left and lets it go.
        for _ in 0..helper_count().min(n - 1) {
            queue.push_back(task.clone());
        }
        drop(queue);
        helpers.more.notify_all();
    }
    task.work();
    // What is left was begun by a helper: hashing, never waiting.
    while task.done.load(Ordering::Acquire) < n {
        std::hint::spin_loop();
        std::thread::yield_now();
    }
    // The pieces' digests in order, then the length (a texture's size does
    // not change, but nothing is lost by it).
    let mut joined = Vec::with_capacity(16 * n + 8);
    for i in 0..n {
        joined.extend_from_slice(&task.out[2 * i].load(Ordering::Relaxed).to_le_bytes());
        joined.extend_from_slice(&task.out[2 * i + 1].load(Ordering::Relaxed).to_le_bytes());
    }
    joined.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
    digest(&joined)
}

/// One cut hash: the bytes, the next piece to take, the pieces done and
/// their digests (two words each).
struct Task {
    at: usize,
    len: usize,
    n: usize,
    next: AtomicUsize,
    done: AtomicUsize,
    out: Vec<AtomicU64>,
}

impl Task {
    /// Hashes pieces until none is left to take.
    fn work(&self) {
        loop {
            let i = self.next.fetch_add(1, Ordering::Relaxed);
            if i >= self.n {
                return;
            }
            let from = i * PIECE;
            let to = (from + PIECE).min(self.len);
            // SAFETY: the caller's bytes, which it borrows until every
            // piece taken is done (`split_digest` waits for `done`).
            let piece = unsafe { std::slice::from_raw_parts((self.at + from) as *const u8, to - from) };
            let Digest(d) = digest(piece);
            self.out[2 * i].store((d >> 64) as u64, Ordering::Relaxed);
            self.out[2 * i + 1].store(d as u64, Ordering::Relaxed);
            self.done.fetch_add(1, Ordering::Release);
        }
    }
}

struct Helpers {
    queue: Mutex<std::collections::VecDeque<Arc<Task>>>,
    more: Condvar,
}

/// The threads that hash pieces, started at the first cut hash.
fn helpers() -> &'static Helpers {
    static HELPERS: std::sync::OnceLock<Helpers> = std::sync::OnceLock::new();
    static STARTED: std::sync::Once = std::sync::Once::new();
    let helpers = HELPERS.get_or_init(|| Helpers {
        queue: Mutex::new(std::collections::VecDeque::new()),
        more: Condvar::new(),
    });
    STARTED.call_once(|| {
        for i in 0..helper_count() {
            let _ = std::thread::Builder::new().name(format!("victory-hash-{i}")).spawn(move || {
                crate::cores::keep_helper_on_fast_cores("victory-hash");
                let helpers = HELPERS.get().expect("made above");
                loop {
                    let task = {
                        let mut queue = helpers.queue.lock().unwrap_or_else(|e| e.into_inner());
                        loop {
                            if let Some(task) = queue.pop_front() {
                                break task;
                            }
                            queue = helpers.more.wait(queue).unwrap_or_else(|e| e.into_inner());
                        }
                    };
                    task.work();
                }
            });
        }
    });
    helpers
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(len: usize, seed: u64) -> Vec<u8> {
        let mut x = seed | 1;
        (0..len)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x as u8
            })
            .collect()
    }

    #[test]
    fn small_textures_keep_the_old_digest() {
        for len in [0, 1, 63, 64, 65, 4096, PIECE, SPLIT_FROM - 1] {
            let b = bytes(len, len as u64 + 7);
            assert_eq!(texture_digest(&b), digest(&b), "{len}");
        }
    }

    #[test]
    fn the_helpers_find_what_one_thread_finds() {
        for len in [SPLIT_FROM, SPLIT_FROM + 1, 5 * PIECE + 12345, 17 * PIECE] {
            let b = bytes(len, len as u64);
            let one = split_digest(&b, false);
            for _ in 0..8 {
                assert_eq!(split_digest(&b, true), one, "{len}");
            }
        }
    }

    #[test]
    fn every_change_the_old_digest_sees_this_one_sees() {
        let len = 6 * PIECE + 100;
        let base = bytes(len, 99);
        let was = split_digest(&base, true);
        // One byte, one bit, anywhere: the first, a piece's edges, the last.
        let mut spots = vec![0, 1, PIECE - 1, PIECE, PIECE + 1, 3 * PIECE + 777, len - 1];
        spots.extend((0..64).map(|i| (i * 2_654_435_761usize) % len));
        for at in spots {
            for bit in [0u8, 7] {
                let mut b = base.clone();
                b[at] ^= 1 << bit;
                assert_ne!(digest(&b), digest(&base));
                assert_ne!(split_digest(&b, true), was, "byte {at} bit {bit}");
            }
        }
        // Two pieces swapped: same bytes, other order.
        let mut b = base.clone();
        let (a, rest) = b.split_at_mut(PIECE);
        a.swap_with_slice(&mut rest[..PIECE]);
        assert_ne!(split_digest(&b, true), was);
        // All zeros against a zero piece more (other length).
        assert_ne!(split_digest(&vec![0; SPLIT_FROM], true), split_digest(&vec![0; SPLIT_FROM + PIECE], true));
    }

    #[test]
    fn many_threads_at_once() {
        let b = Arc::new(bytes(9 * PIECE, 5));
        let want = split_digest(&b, false);
        std::thread::scope(|s| {
            for _ in 0..6 {
                let b = b.clone();
                s.spawn(move || {
                    for _ in 0..20 {
                        assert_eq!(split_digest(&b, true), want);
                    }
                });
            }
        });
    }

    /// `cargo test --release -p victory-gpu split_costs -- --ignored --nocapture`
    #[test]
    #[ignore = "a measurement: run with --release --ignored --nocapture"]
    fn split_costs() {
        // Cold: more than the 96 MB of the 7800X3D's cache, read once a round.
        let textures: Vec<Vec<u8>> = (0..12).map(|i| bytes(16 << 20, i + 1)).collect();
        let all: usize = textures.iter().map(Vec::len).sum();
        let time = |what: &str, f: &dyn Fn(&[u8])| {
            for t in &textures {
                f(t);
            }
            let began = std::time::Instant::now();
            let rounds = 5;
            for _ in 0..rounds {
                for t in &textures {
                    f(t);
                }
            }
            let s = began.elapsed().as_secs_f64() / rounds as f64;
            println!("{what}: {:.1} ms for {} MB ({:.1} GB/s); 250 MB: {:.1} ms", s * 1e3, all >> 20, all as f64 / s / 1e9, 250e6 / (all as f64 / s) * 1e3);
        };
        time("old digest, one thread", &|t| {
            std::hint::black_box(digest(t));
        });
        time("cut, one thread", &|t| {
            std::hint::black_box(split_digest(t, false));
        });
        time("cut, with the helpers", &|t| {
            std::hint::black_box(split_digest(t, true));
        });
    }
}
