// crates/forge-mem/tests/no_leaks.rs

use forge_mem::ExecutableBuffer;

/// `getrusage(RUSAGE_SELF).ru_maxrss` is a HIGH-WATER MARK, not a live RSS
/// reading -- it never decreases, even when `Drop`'s `munmap` genuinely
/// frees memory. So this test can't just check "RSS went down." Instead it
/// compares the high-water mark's growth RATE across two windows of
/// allocate/free cycles: flat growth after an initial warmup means the
/// munmap-on-Drop path is working; growth that keeps scaling with
/// iteration count means a real leak.
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

    for _ in 0..10_000 {
        allocate_write_free_cycle();
    }
    let after_full_run = max_rss_kb();

    let growth = after_full_run - after_warmup;
    // A real per-buffer leak (even one page, 16KB on this platform) times
    // 10,000 iterations would be well over 100MB of growth. Allow a
    // generous fixed budget for legitimate one-time growth (allocator
    // metadata, page cache effects) without being so loose it'd miss an
    // actual leak.
    assert!(
        growth < 20_000, // 20MB
        "high-water-mark RSS grew by {growth}KB over 10,000 allocate/free cycles -- \
         this looks like a leak (Drop's munmap may not be running, or may be failing silently)"
    );
}
