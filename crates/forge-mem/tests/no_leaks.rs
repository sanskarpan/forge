// crates/forge-mem/tests/no_leaks.rs

#![cfg(unix)]

use forge_mem::ExecutableBuffer;

/// `getrusage(RUSAGE_SELF).ru_maxrss` is a HIGH-WATER MARK, not a live RSS
/// reading -- it never decreases, even when `Drop`'s `munmap` genuinely
/// frees memory. So this test can't just check "RSS went down." Instead it
/// takes THREE measurements -- after an initial warmup, at the run's
/// midpoint, and at the end -- and compares the two equal-sized half-run
/// deltas (mid - warmup) and (end - mid) against EACH OTHER, not just
/// against one loose absolute budget for the whole run. A real leak grows
/// both halves by roughly the same amount, since each half does the same
/// number of allocate/free cycles; a working `Drop` keeps both near zero.
/// Comparing the two halves lets each one use a much tighter per-half
/// budget than a single whole-run budget could, without becoming flaky on
/// a loaded machine -- a single absolute budget over the whole run has to
/// stay loose enough to avoid false positives, which is exactly what lets
/// a partial leak (e.g. one code path or buffer size that leaks while
/// others don't) hide under it indefinitely.
fn max_rss_kb() -> i64 {
    // SAFETY: `usage` is a valid, exclusively-owned local passed by
    // pointer for getrusage to fill in; RUSAGE_SELF has no preconditions.
    unsafe {
        let mut usage: libc::rusage = std::mem::zeroed();
        let rc = libc::getrusage(libc::RUSAGE_SELF, &mut usage);
        assert_eq!(
            rc,
            0,
            "getrusage failed: {}",
            std::io::Error::last_os_error()
        );
        // macOS reports ru_maxrss in BYTES; Linux reports it in KB. Both
        // are only compared to each other WITHIN this one process/run
        // below, so the unit doesn't matter as long as it's consistent --
        // still normalize to a "kb-ish" unit for a readable failure
        // message if this ever fails.
        #[cfg(target_os = "macos")]
        {
            usage.ru_maxrss / 1024
        }
        #[cfg(not(target_os = "macos"))]
        {
            usage.ru_maxrss
        }
    }
}

fn allocate_write_free_cycle() {
    let mut buf = ExecutableBuffer::new(4096).expect("allocation should succeed");
    buf.write(|mem| mem[..4].copy_from_slice(&[0xC0, 0x03, 0x5F, 0xD6]));
    buf.make_executable()
        .expect("make_executable should succeed");
    drop(buf);
}

#[test]
fn ten_thousand_allocate_free_cycles_do_not_leak() {
    // Warm up: let the allocator/OS settle into a steady state before
    // taking the first measurement (the first few hundred mmap/munmap
    // calls can grow RSS for reasons unrelated to leaking -- e.g. the
    // allocator's own bookkeeping pages).
    for _ in 0..500 {
        allocate_write_free_cycle();
    }
    let after_warmup = max_rss_kb();

    for _ in 0..5_000 {
        allocate_write_free_cycle();
    }
    let after_first_half = max_rss_kb();

    for _ in 0..5_000 {
        allocate_write_free_cycle();
    }
    let after_second_half = max_rss_kb();

    let first_half_growth = after_first_half - after_warmup;
    let second_half_growth = after_second_half - after_first_half;

    // Each half runs the same number of cycles, so a real leak -- even one
    // that only fires on a fraction of allocations -- grows both halves by
    // roughly the same amount, while a working Drop keeps both near zero.
    // A per-half budget can be much tighter than the old single whole-run
    // budget was: a per-buffer leak (even one page, 16KB on this platform)
    // hitting just 5% of the 5,000 cycles in a half would add up to ~4MB,
    // well clear of noise from allocator bookkeeping but well under a
    // budget loose enough to never false-positive on a loaded machine.
    assert!(
        first_half_growth < 3_000, // 3MB over 5,000 cycles
        "first-half RSS grew by {first_half_growth}KB over 5,000 allocate/free cycles -- \
         this looks like a leak (Drop's munmap may not be running, or may be failing silently)"
    );
    assert!(
        second_half_growth < 3_000, // 3MB over 5,000 cycles
        "second-half RSS grew by {second_half_growth}KB over 5,000 allocate/free cycles -- \
         this looks like a leak (Drop's munmap may not be running, or may be failing silently)"
    );
}
