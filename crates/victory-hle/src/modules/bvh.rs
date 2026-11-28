//! `XboxBvhBuilder.dll`, the title's own ray-tracing structure builder:
//! with `VICTORY_BVH_HOOK=1` its exports are wrapped, so the layer sees each
//! call as the title made it (`victory_gpu::rtas`) before the DLL records its
//! programs. Off, the title's imports bind to the DLL as before.
//! With `VICTORY_BVH_NATIVE=static` as well, the static world's unpacking is
//! the layer's (`victory_gpu::rtas::rehydrate_native`), not the DLL's programs.
//!
//! The exports are members of the DLL's `XboxBvhBuilder` class (the object
//! first, in `rcx`), by their decorated names. The DLL's own addresses are
//! handed over by the loader ([`set_original`]) before imports are bound.

use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::{HleExport, HleModule};

const BUILD: &str = "?BuildRaytracingAccelerationStructure@XboxBvhBuilder@@QEAAXPEAUID3D12GraphicsCommandList4@@PEBUD3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_DESC@@IPEBUD3D12_RAYTRACING_ACCELERATION_STRUCTURE_POSTBUILD_INFO_DESC@@@Z";
const BATCHED: &str = "?BuildRaytracingAccelerationStructureBatched2X@XboxBvhBuilder@@QEAAXPEAUID3D12GraphicsCommandList6@@IPEBUD3D12XBOX_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_BATCHED_DESC2@@@Z";
const COPY: &str = "?CopyRaytracingAccelerationStructure@XboxBvhBuilder@@QEAAXPEAUID3D12GraphicsCommandList4@@_K1W4D3D12_RAYTRACING_ACCELERATION_STRUCTURE_COPY_MODE@@@Z";
const PREBUILD: &str = "?GetRaytracingAccelerationStructurePrebuildInfo@XboxBvhBuilder@@QEAAXPEBUD3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS@@PEBUD3D12XBOX_XDXR_STREAM_DESC@@PEAUD3D12_RAYTRACING_ACCELERATION_STRUCTURE_PREBUILD_INFO@@@Z";
const REHYDRATE_BATCHED2: &str = "?RehydrateRaytracingAccelerationStructureBatched2X@XboxBvhBuilder@@QEAAJPEAUID3D12GraphicsCommandList8@@IPEBUD3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_DESC@@PEB_K@Z";
const REHYDRATE_BATCHED: &str = "?RehydrateRaytracingAccelerationStructureBatchedX@XboxBvhBuilder@@QEAAJPEAUID3D12GraphicsCommandList8@@IPEBUD3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_DESC@@@Z";
const REHYDRATE: &str = "?RehydrateRaytracingAccelerationStructureX@XboxBvhBuilder@@QEAAJPEAUID3D12Device11@@PEBUD3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_DESC@@@Z";

/// The wrapped exports, by decorated name: the DLL's own address of each.
pub const WRAPPED: [&str; 7] = [BUILD, BATCHED, COPY, PREBUILD, REHYDRATE_BATCHED2, REHYDRATE_BATCHED, REHYDRATE];
static ORIGINAL: [AtomicUsize; 7] = [const { AtomicUsize::new(0) }; 7];

/// The DLL's own address of `name` (one of [`WRAPPED`]); the loader calls
/// this for each before the title's imports are bound.
pub fn set_original(name: &str, address: usize) {
    if let Some(n) = WRAPPED.iter().position(|w| *w == name) {
        ORIGINAL[n].store(address, Ordering::Relaxed);
    }
}

/// The DLL's own function `n` of [`WRAPPED`], as `F`.
fn original<F: Copy>(n: usize) -> Option<F> {
    let address = ORIGINAL[n].load(Ordering::Relaxed);
    if address == 0 {
        victory_gpu::rtas::missing(WRAPPED[n]);
        return None;
    }
    assert_eq!(std::mem::size_of::<F>(), std::mem::size_of::<usize>());
    // SAFETY: a function pointer type of the export's own signature (each
    // caller below names it); the address is the DLL's export.
    Some(unsafe { std::mem::transmute_copy::<usize, F>(&address) })
}

type BuildFn = extern "system" fn(*mut c_void, *mut c_void, *const u8, u32, *const u8);
extern "system" fn build(this: *mut c_void, list: *mut c_void, desc: *const u8, posts: u32, post: *const u8) {
    victory_gpu::rtas::build(list, desc, posts);
    if victory_gpu::rtas::native() == victory_gpu::rtas::Native::All {
        // (One description is a batch of one without its tail.)
        let mut one = [0u8; victory_gpu::rtas::BATCHED];
        // SAFETY: the title's description, 48 bytes.
        unsafe { std::ptr::copy_nonoverlapping(desc, one.as_mut_ptr(), victory_gpu::rtas::DESC) };
        // (Its post-build descriptions where a batched one has them.)
        one[48..52].copy_from_slice(&posts.to_le_bytes());
        one[56..64].copy_from_slice(&(post as u64).to_le_bytes());
        if victory_gpu::rtas::batched_native(list, 1, one.as_ptr()).is_empty() {
            return;
        }
    }
    if let Some(f) = original::<BuildFn>(0) {
        f(this, list, desc, posts, post);
    }
}

type BatchedFn = extern "system" fn(*mut c_void, *mut c_void, u32, *const u8);
extern "system" fn batched(this: *mut c_void, list: *mut c_void, count: u32, descs: *const u8) {
    victory_gpu::rtas::batched(list, count, descs);
    if victory_gpu::rtas::native() == victory_gpu::rtas::Native::All {
        // The top levels are the layer's; the rest goes on to the DLL.
        let rest = victory_gpu::rtas::batched_native(list, count, descs);
        if !rest.is_empty()
            && let Some(f) = original::<BatchedFn>(1)
        {
            f(this, list, (rest.len() / victory_gpu::rtas::BATCHED) as u32, rest.as_ptr());
        }
        return;
    }
    if let Some(f) = original::<BatchedFn>(1) {
        f(this, list, count, descs);
    }
}

type CopyFn = extern "system" fn(*mut c_void, *mut c_void, u64, u64, u32);
extern "system" fn copy(this: *mut c_void, list: *mut c_void, dest: u64, source: u64, mode: u32) {
    victory_gpu::rtas::copy(dest, source, mode);
    if victory_gpu::rtas::copy_native(list, dest, source) {
        return;
    }
    if let Some(f) = original::<CopyFn>(2) {
        f(this, list, dest, source, mode);
    }
}

type PrebuildFn = extern "system" fn(*mut c_void, *const u8, *const u8, *mut u8);
extern "system" fn prebuild(this: *mut c_void, inputs: *const u8, stream: *const u8, info: *mut u8) {
    victory_gpu::rtas::prebuild();
    if let Some(f) = original::<PrebuildFn>(3) {
        f(this, inputs, stream, info);
        victory_gpu::rtas::prebuilt(inputs, info);
    }
}

type RehydrateBatched2Fn = extern "system" fn(*mut c_void, *mut c_void, u32, *const u8, *const u64) -> i32;
extern "system" fn rehydrate_batched2(this: *mut c_void, list: *mut c_void, count: u32, descs: *const u8, extra: *const u64) -> i32 {
    victory_gpu::rtas::rehydrate(count, descs, "rehydrate (batched 2)");
    if victory_gpu::rtas::native() != victory_gpu::rtas::Native::Off {
        return victory_gpu::rtas::rehydrate_native(count, descs);
    }
    original::<RehydrateBatched2Fn>(4).map_or(E_FAIL, |f| f(this, list, count, descs, extra))
}

type RehydrateBatchedFn = extern "system" fn(*mut c_void, *mut c_void, u32, *const u8) -> i32;
extern "system" fn rehydrate_batched(this: *mut c_void, list: *mut c_void, count: u32, descs: *const u8) -> i32 {
    victory_gpu::rtas::rehydrate(count, descs, "rehydrate (batched)");
    if victory_gpu::rtas::native() != victory_gpu::rtas::Native::Off {
        return victory_gpu::rtas::rehydrate_native(count, descs);
    }
    original::<RehydrateBatchedFn>(5).map_or(E_FAIL, |f| f(this, list, count, descs))
}

type RehydrateFn = extern "system" fn(*mut c_void, *mut c_void, *const u8) -> i32;
extern "system" fn rehydrate(this: *mut c_void, device: *mut c_void, desc: *const u8) -> i32 {
    victory_gpu::rtas::rehydrate(1, desc, "rehydrate");
    if victory_gpu::rtas::native() != victory_gpu::rtas::Native::Off {
        return victory_gpu::rtas::rehydrate_native(1, desc);
    }
    original::<RehydrateFn>(6).map_or(E_FAIL, |f| f(this, device, desc))
}

const E_FAIL: i32 = 0x8000_4005_u32 as i32;

pub fn module() -> HleModule {
    let functions: [usize; 7] = [
        build as *const () as usize,
        batched as *const () as usize,
        copy as *const () as usize,
        prebuild as *const () as usize,
        rehydrate_batched2 as *const () as usize,
        rehydrate_batched as *const () as usize,
        rehydrate as *const () as usize,
    ];
    HleModule {
        name: "bvh",
        dlls: &["xboxbvhbuilder.dll"],
        // Off: none, and the imports bind to the DLL itself.
        exports: if victory_gpu::rtas::on() {
            WRAPPED.iter().zip(functions).map(|(&name, address)| HleExport { name, address }).collect()
        } else {
            Vec::new()
        },
    }
}
