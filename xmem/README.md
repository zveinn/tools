# memory-analysis

<img width="1249" height="683" alt="image" src="https://github.com/user-attachments/assets/da97331c-ecb6-4855-9dde-f8009ce7e7ff" />

Diagnoses the "OOM with plenty of free RAM" failure pattern: unbounded kernel
slab growth (e.g. filesystem inode caches on large-namespace object-storage
hosts), SLUB merged-cache misattribution, physical memory fragmentation, and
the blind spot where the OOM killer shoots the largest userspace process even
though the kernel itself owns the memory.

## Build & run

```bash
go build -o memory-analysis .
sudo ./memory-analysis                 # full analysis, 10s sample window
sudo ./memory-analysis -sample 30s     # longer growth/trace window
sudo ./memory-analysis -no-trace       # strictly read-only, never touches tracefs
./memory-analysis                      # non-root: partial (no slabinfo/tracing)
```

Flags: `-sample` (default `10s`, `0` disables the window), `-no-trace`,
`-top-slabs`, `-top-procs`, `-culprits` (default 3).

## Live trace mode

```bash
sudo ./memory-analysis live                # 5s refresh
sudo ./memory-analysis live -interval 2s
```

A 2x2 full-screen grid (Ctrl-C quits; tracefs and the terminal are
restored on any exit). Header shows total event rate and ring-buffer
drops (`LOST`); the per-cpu trace buffer is enlarged for the session
(and restored) so free events aren't dropped. Below the header a
**kernel alert ticker** tails `/dev/kmsg` and turns red the moment an
oom-kill, page allocation failure, hung task, or soft lockup hits the
log.

- **Top-left: slab cache growth** — per-cache object counts with size
  deltas for the current window and cumulative since start, sorted by
  total growth. A cache marching up here is your live culprit.
- **Top-right: memory / reclaim activity** — MemFree/MemAvailable/
  SReclaimable/SUnreclaim with window deltas, plus vmstat counters:
  pgscan/pgsteal (kswapd vs direct — direct reclaim and allocation
  stalls highlighted: allocations are waiting on reclaim), reclaim
  efficiency (pages freed per page scanned — red under 20%, the thrash
  signature), workingset refaults (reclaim eating the working set, not
  cold cache), compaction stall/success/fail with the failure rate (red
  at ≥50% — fragmentation is not self-healing), oom_kill (red when
  nonzero), major faults, memory PSI (some/full over 10s), and the top
  3 shrinker-held caches (xfs-buf etc.) with per-window deltas —
  falling under pressure means reclaim is working; flat while pgscan
  climbs means it is stalled.
- **Bottom-left: allocation sites with leak tracking** — kfree and
  kmem_cache_free are traced alongside the allocation events, and each
  free is matched back to its allocation site by pointer. Per site:
  allocation rate, bytes this window, and **NET = unfreed bytes since
  start** (yellow >10MB, red >100MB). High rate with NET near zero is a
  hot path; steadily climbing NET is a leak candidate. The pane footer
  shows the **rss movers** — the processes whose RSS grew most this
  window.
- **Bottom-right: fragmentation, zone Normal** — usable free blocks per
  order (excluding reserves) with per-window trend, max usable order
  (red when order-3 becomes unavailable — the OOM-with-free-memory
  precursor), and the highatomic reserve size.

## What it reports

1. **System memory overview** — `/proc/meminfo` broken down against total
   RAM, including the reclaimable/unreclaimable slab split, hugepages,
   mlocked/unevictable memory, and the untracked remainder.
2. **Slab caches** — top caches by size from `/proc/slabinfo`, each flagged
   reclaimable/unreclaimable and annotated with its SLUB merge group from
   `/sys/kernel/slab` symlinks.
3. **Lockstep detection** — caches whose object counts match within 3%
   (≥1M objects). This is the signature of companion allocations (one per
   object of a parent cache, e.g. per-inode LSM blobs and XFS attr forks
   tracking `xfs_inode`) that are freed together with the parent.
4. **Dentry state** — total/unused/negative dentries from
   `/proc/sys/fs/dentry-state`; negative dentries (cached failed lookups)
   are a classic unbounded-slab source.
5. **Process allocations** — every process ranked exactly as the kernel's
   `oom_badness()` would (RSS + swap + page tables, shifted by
   `oom_score_adj`), plus PSS and Locked from `smaps_rollup` (RSS
   double-counts shared pages; PSS doesn't) and a count of BPF/perf-event
   fds (EDR/tracing agents).
6. **Slab by cgroup** — ties slab memory to processes: `SLAB_ACCOUNT`
   caches (inode, dentry, most named caches) are charged to the allocating
   cgroup (`memory.stat`), shown as a tree with member PIDs and the
   unaccounted remainder. This is the closest the kernel offers to
   "which process owns this slab" — note objects can outlive the charger.
7. **Cgroup limits & OOM events** — every cgroup with a `memory.max`/
   `memory.high` and its usage, limit-hit and `oom_kill` counters. A memcg
   kill fires with host memory free — a completely different diagnosis
   from global exhaustion.
8. **Per-NUMA-node memory** — free/file/anon/slab per node; the OOM killer
   is invoked per node, so one exhausted node kills with memory free
   elsewhere.
9. **Shrinkers** — reclaimable-object counts per shrinker type from
   debugfs: xfs-buf metadata buffers, per-superblock dentry/inode LRUs,
   nfs/zfs/GPU caches. This is the union of "reclaimable but invisible to
   MemAvailable" memory (see below).
10. **PSI** — memory and io pressure-stall percentages, the leading
    indicator of thrash before OOM.
11. **Kernel VM settings** — the sysctls that decide when reclaim starts
    and when allocations fail (`min_free_kbytes`, `zone_reclaim_mode`,
    overcommit + commit usage, dirty throttling status against the
    effective thresholds, THP mode).
12. **Network memory** — TCP/UDP buffer pages from `/proc/net/sockstat`
    against the `tcp_mem` pressure threshold; socket buffers hide inside
    merged kmalloc slabs where nothing else names them.
13. **Writeback by device** — per-bdi dirty/writeback/bandwidth (debugfs):
    when reclaim stalls behind dirty data, this names the disk that can't
    keep up.
14. **Kernel allocation profiling** — `/proc/allocinfo` (kernel ≥6.10,
    `CONFIG_MEM_ALLOC_PROFILING`): exact per-callsite accounting of ALL
    kernel memory including raw page allocations — the definitive answer
    to "where is the missing memory", parsed automatically when present.
15. **Fragmentation** — buddy allocator state per zone; with root, the exact
    per-order availability for _normal_ allocations (excluding the
    HighAtomic/CMA reserves, via `/proc/pagetypeinfo`) plus the share of
    Unmovable pageblocks that compaction cannot fix, and the compaction
    failure rate since boot — the single number that says whether the
    fragmentation can self-heal (echoed in the summary when ≥20%).
16. **OOM kill forensics** — past OOM kills parsed from the kernel ring
    buffer (`/dev/kmsg`): victim, the triggering allocation's order and
    gfp flags, and whether the kill was global or a cgroup limit;
    cross-checked against the `/proc/vmstat` oom_kill counter, which
    survives log rotation.
17. **Culprit deep-dive + live attribution** — for the top-N caches:
    merge-group membership, growth over the sample window, and (root only)
    a live sample of kmem tracepoints filtered to the culprit size classes,
    aggregated by allocating kernel function and process (`comm:pid`,
    thread ids resolved to process ids via `/proc`). The window also diffs
    `/proc/vmstat`: page scan/steal with reclaim efficiency, direct-reclaim
    allocation stalls, workingset refaults, compaction attempts with their
    failure rate, and any OOM kill that fires mid-run.
18. **Summary + WARNINGS** — severity-flagged stats, then every problem
    detected during the run collected into one list (most severe first),
    each with a hint on what to check or change.

## Warnings

Every section doubles as a detector. Anything that crosses a threshold is
collected and reported in the final **WARNINGS** section, `[CRIT]` first:

- OOM kills since boot (and whether they were memcg-limit kills), from the
  kernel log and `/proc/vmstat`
- fragmentation kills pending (no usable order-3 blocks), and past kills
  whose failing allocation was high-order or `GFP_NOFS`-restricted
- compaction failing ≥50% of attempts (fragmentation cannot self-heal;
  CRIT when zone Normal is already below order-3)
- reclaim thrash (efficiency under 20% across the window), allocations
  stalled in direct reclaim, or an OOM kill firing during the run
- memory PSI stall levels; NUMA node exhaustion/imbalance
- slab share of RAM; slab caches growing fast enough to fill RAM in hours
- xfs-buf buffers holding meminfo-invisible memory; large "Other"
  (unaccounted) memory
- cgroups at (or already killing at) their `memory.max`
- negative-dentry bloat; TCP buffers past the `tcp_mem` pressure threshold
- writers throttled at the dirty threshold; strict-overcommit commit
  exhaustion
- misconfigured sysctls (`zone_reclaim_mode`, `vfs_cache_pressure=0`, tiny
  `min_free_kbytes` on big boxes); no swap
- hardware-retired pages (`HardwareCorrupted`); mlocked memory over 5% of
  RAM; hugepages reserved but unused

## XFS metadata buffers (invisible to meminfo)

The `xfs_buf` slab objects are only ~0.4KB headers; the 4-16KB metadata
blocks they reference are allocated with `alloc_pages()` and appear in no
`/proc/meminfo` counter — on metadata-heavy hosts (object storage with
hundreds of millions of files) this is the "missing" memory that makes
`free` look full while slabtop shows almost nothing. The buffer cache's
own shrinker is the only place the kernel reports it. The summary reads
it automatically when debugfs is accessible (root); manually:

```bash
ls /sys/kernel/debug/shrinker/ | grep xfs-buf     # one per drive
cat /sys/kernel/debug/shrinker/xfs-buf:*/count     # reclaimable buffers per drive
```

A forced reclaim (`sync; echo 2 > /proc/sys/vm/drop_caches`) should
collapse these counts. If it doesn't, the buffers are dirty or pinned and
reclaim is stalling behind metadata writeback — the pattern that turns
"just cache" into real OOM kills.

## Identifying the EXACT struct behind a slab name

`slabtop`/`slabinfo` names are unreliable: SLUB merges same-size caches and
reports the whole group under the first-registered name (e.g. 85M `xfs_ifork`
objects reported as `bio_crypt_ctx`). This tool resolves that three ways:

- **Alias groups** (always): lists every cache name sharing the group, from
  `/sys/kernel/slab` — the candidate set.
- **Live tracing** (root, default): the `call_site` of each allocation names
  the allocating kernel function (`security_inode_alloc` →
  `lsm_inode_cache`), which is exact — but only for allocations happening
  during the window.
- **`alloc_calls`** (only if booted with `slub_debug=U`): exact attribution
  of _historical_ allocations, printed automatically when available.

For permanently honest accounting on a debug host, boot with `slub_nomerge`.

## Safety

The only kernel state ever modified is tracefs: the `kmem` event `enable`
and `filter` files (four alloc events in the default run; live mode adds
the two free events and enlarges the per-cpu trace `buffer_size_kb`). Original contents are recorded
before the first write and restored in reverse order on: normal exit, error
paths, panics (via defer), and SIGINT/SIGTERM/SIGHUP. Only SIGKILL can skip
restoration. `-no-trace` guarantees zero writes anywhere.
