//! The two live settings the native ray-tracing port reads (`crate::memory`'s
//! `dxr` code), without upstream's `VICTORY_LIVE_FILE` menus.
//!
//! Upstream victory-corgi (Corgansm, 2026-11-27) reads these from a file that
//! its F8 overlay and `live-menu.ps1` rewrite while the title runs. This tree
//! has its own F1 menu (`host/menu.rs`) instead, so here a setting is the
//! run's own value: [`get`] and [`ray_tracing`] answer `None`, which upstream's
//! code takes as "as the run was started" (`VICTORY_DXR`, `VICTORY_DXR_GUIDED`,
//! `VICTORY_BVH_BUILDER`). Hooking them to the F1 menu is a later step.

/// `VICTORY_DXR`: the host structures are built (upstream's index 8).
pub(crate) const DXR: usize = 8;

/// A setting's value as a menu set it; `None` until one does (never, here).
pub(crate) fn get(_setting: usize) -> Option<f32> {
    None
}

/// The ray-tracing switch (3 host structures alone, 2 guided, 1 the title's
/// own walk, 0 off); `None`: as the run was started. Upstream's menus move it
/// while the title runs; here `VICTORY_RAY_TRACING=<0-3>` picks it at the
/// start (unset: `None`). It only does something in a run translated with
/// `VICTORY_DXR_GUIDED=1`; it is a dword of the host's table, not part of the
/// shader cache key, so changing it needs no new warm.
pub(crate) fn ray_tracing() -> Option<u32> {
    static MODE: std::sync::OnceLock<Option<u32>> = std::sync::OnceLock::new();
    *MODE.get_or_init(|| {
        std::env::var("VICTORY_RAY_TRACING").ok().and_then(|v| v.trim().parse::<u32>().ok()).filter(|v| *v <= 3)
    })
}
