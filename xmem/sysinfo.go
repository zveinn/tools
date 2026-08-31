package main

import (
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
)

// sysctlUint reads one numeric sysctl ("vm.swappiness" ->
// /proc/sys/vm/swappiness). The first field is parsed, so multi-value
// sysctls return their first number.
func sysctlUint(name string) (uint64, bool) {
	b, err := os.ReadFile("/proc/sys/" + strings.ReplaceAll(name, ".", "/"))
	if err != nil {
		return 0, false
	}
	fields := strings.Fields(string(b))
	if len(fields) == 0 {
		return 0, false
	}
	v, err := strconv.ParseUint(fields[0], 10, 64)
	return v, err == nil
}

// thpMode extracts the active transparent-hugepage mode ("[always]" style
// bracket selection).
func thpMode() string {
	b, err := os.ReadFile("/sys/kernel/mm/transparent_hugepage/enabled")
	if err != nil {
		return ""
	}
	for _, f := range strings.Fields(string(b)) {
		if strings.HasPrefix(f, "[") {
			return strings.Trim(f, "[]")
		}
	}
	return ""
}

// printVMSettings shows the sysctls that decide when reclaim starts, how
// hard it runs, and when allocations are refused — misconfiguration here
// is the root cause in a large share of OOM investigations.
func printVMSettings(mi meminfo) {
	section("KERNEL VM SETTINGS")
	totalKB := mi["MemTotal"]
	row := func(name, note string) (uint64, bool) {
		v, ok := sysctlUint(name)
		if ok {
			fmt.Printf("  %-28s %12d  %s\n", name, v, dim(note))
		}
		return v, ok
	}

	mfk, _ := row("vm.min_free_kbytes", "reclaim starts below this; too low = allocation stalls under bursts")
	row("vm.watermark_scale_factor", "distance between reclaim watermarks (units of 0.01% of RAM)")
	wbf, haveWBF := row("vm.watermark_boost_factor", "watermark boost after a fragmenting fallback (default 15000, 0 = off)")
	row("vm.swappiness", "willingness to swap anon vs drop file cache")
	vcp, haveVCP := row("vm.vfs_cache_pressure", "reclaim aggressiveness for dentry/inode caches (100 = neutral)")
	zrm, _ := row("vm.zone_reclaim_mode", "nonzero = nodes reclaim locally before using other nodes")
	ocm, _ := row("vm.overcommit_memory", "0 heuristic · 1 always · 2 strict commit limit")
	dbgRatio, _ := row("vm.dirty_background_ratio", "background writeback starts (% of dirtyable memory)")
	dRatio, _ := row("vm.dirty_ratio", "writers are throttled above this (% of dirtyable memory)")
	dbgBytes, _ := sysctlUint("vm.dirty_background_bytes")
	dBytes, _ := sysctlUint("vm.dirty_bytes")
	if dbgBytes > 0 || dBytes > 0 {
		fmt.Printf("  %-28s %12s  %s\n", "vm.dirty*_bytes",
			humanKB(dbgBytes/1024)+" / "+humanKB(dBytes/1024), dim("byte overrides (nonzero wins over ratios)"))
	}
	if thp := thpMode(); thp != "" {
		fmt.Printf("  %-28s %12s  %s\n", "transparent_hugepage", thp, dim("THP mode"))
	}

	// Commit accounting: under overcommit_memory=2 allocation failures at
	// the commit limit look exactly like memory exhaustion.
	committed, limit := mi["Committed_AS"], mi["CommitLimit"]
	if limit > 0 {
		pct := float64(committed) / float64(limit) * 100
		fmt.Printf("  %-28s %12s  %s\n", "Committed_AS / CommitLimit",
			fmt.Sprintf("%.1f%%", pct), dim(humanKB(committed)+" of "+humanKB(limit)))
		if ocm == 2 {
			switch {
			case pct >= 100:
				warnf(sevCrit, "raise vm.overcommit_ratio or add swap; malloc is returning ENOMEM now",
					"strict overcommit (vm.overcommit_memory=2) and Committed_AS is AT the commit limit (%.0f%%)", pct)
			case pct >= 90:
				warnf(sevWarn, "allocations will start failing at 100% regardless of free memory",
					"strict overcommit (vm.overcommit_memory=2) and commit usage is %.0f%%", pct)
			}
		}
	}

	// Dirty throttling status against the effective thresholds.
	dirtyable := mi["MemFree"] + mi["Active(file)"] + mi["Inactive(file)"]
	dirtyThreshKB := dirtyable * dRatio / 100
	if dBytes > 0 {
		dirtyThreshKB = dBytes / 1024
	}
	bgThreshKB := dirtyable * dbgRatio / 100
	if dbgBytes > 0 {
		bgThreshKB = dbgBytes / 1024
	}
	curDirty := mi["Dirty"] + mi["Writeback"]
	if dirtyThreshKB > 0 {
		fmt.Printf("  %-28s %12s  %s\n", "dirty now / throttle at",
			humanKB(curDirty), dim(fmt.Sprintf("throttle %s · background %s", humanKB(dirtyThreshKB), humanKB(bgThreshKB))))
		if curDirty >= dirtyThreshKB*9/10 {
			warnf(sevWarn, "writers are being throttled; reclaim of dirty pages must wait for the disks",
				"dirty+writeback %s is at %.0f%% of the throttle threshold %s",
				humanKB(curDirty), float64(curDirty)/float64(dirtyThreshKB)*100, humanKB(dirtyThreshKB))
		}
	}
	fmt.Println()

	if zrm != 0 {
		warnf(sevWarn, "set vm.zone_reclaim_mode=0 unless strictly required — it causes per-node reclaim stalls and node-local OOM on NUMA boxes",
			"vm.zone_reclaim_mode=%d", zrm)
	}
	if haveVCP && vcp == 0 {
		warnf(sevWarn, "dentries/inodes are never reclaimed at 0 — unbounded slab growth",
			"vm.vfs_cache_pressure=0")
	}
	if haveWBF && wbf == 0 {
		warnf(sevWarn, "restore the default (15000) — the boost reclaims/compacts right when kernel allocations steal movable pageblocks, the main defense against permanent Unmovable fragmentation",
			"vm.watermark_boost_factor=0 disables the anti-fragmentation watermark boost")
	}
	if totalKB >= 64<<20 && mfk > 0 && mfk < totalKB/1000 {
		warnf(sevInfo, "for large-RAM storage servers consider 1-2 GB (vm.min_free_kbytes) so reclaim starts before burst allocations fail",
			"vm.min_free_kbytes is %s on a %s box (%.2f%% of RAM)",
			humanKB(mfk), humanKB(totalKB), float64(mfk)/float64(totalKB)*100)
	}
	if mi["SwapTotal"] == 0 {
		warnf(sevInfo, "without swap, anonymous memory is unreclaimable — the kernel's only relief valve is the OOM killer",
			"no swap configured")
	}
}

// printDentryState reports /proc/sys/fs/dentry-state. Negative dentries
// (cached "file not found" results) grow without bound on lookup-heavy
// workloads and are a classic source of dentry-slab bloat.
func printDentryState() {
	b, err := os.ReadFile("/proc/sys/fs/dentry-state")
	if err != nil {
		return
	}
	f := strings.Fields(string(b))
	if len(f) < 6 {
		return
	}
	u := func(i int) uint64 { n, _ := strconv.ParseUint(f[i], 10, 64); return n }
	total, unused, negative := u(0), u(1), u(4)
	section("DENTRY STATE")
	negS := fmt.Sprintf("%d", negative)
	if total > 0 {
		negS += fmt.Sprintf(" (%.0f%% of total)", float64(negative)/float64(total)*100)
	}
	fmt.Printf("  total %d · unused (LRU) %d · negative %s\n", total, unused, negS)
	fmt.Println(dim("  negative dentries cache failed lookups; they are pure overhead beyond a point"))
	fmt.Println()
	if negative > 1_000_000 && total > 0 && negative > total/3 {
		warnf(sevWarn, "something is looking up nonexistent paths at scale; echo 2 > /proc/sys/vm/drop_caches clears them",
			"%d negative dentries (%.0f%% of all dentries)", negative, float64(negative)/float64(total)*100)
	}
}

// printSockMem reports kernel socket-buffer memory from /proc/net/sockstat.
// TCP buffer pages hide inside generic kmalloc slabs where merged cache
// names make them invisible; sockstat names them directly.
func printSockMem(totalRAMKB uint64) {
	b, err := os.ReadFile("/proc/net/sockstat")
	if err != nil {
		return
	}
	pageKB := uint64(os.Getpagesize()) / 1024
	var tcpPages, udpPages uint64
	var tcpInuse, tcpOrphan uint64
	for _, line := range strings.Split(string(b), "\n") {
		f := strings.Fields(line)
		grab := func(key string) (uint64, bool) {
			for i := 1; i < len(f)-1; i++ {
				if f[i] == key {
					v, err := strconv.ParseUint(f[i+1], 10, 64)
					return v, err == nil
				}
			}
			return 0, false
		}
		switch {
		case strings.HasPrefix(line, "TCP:"):
			tcpPages, _ = grab("mem")
			tcpInuse, _ = grab("inuse")
			tcpOrphan, _ = grab("orphan")
		case strings.HasPrefix(line, "UDP:"):
			udpPages, _ = grab("mem")
		}
	}
	section("NETWORK MEMORY")
	fmt.Printf("  TCP buffers %s (%d sockets in use, %d orphaned) · UDP buffers %s\n",
		humanKB(tcpPages*pageKB), tcpInuse, tcpOrphan, humanKB(udpPages*pageKB))
	// tcp_mem thresholds are in pages: min, pressure, max.
	if tm, err := os.ReadFile("/proc/sys/net/ipv4/tcp_mem"); err == nil {
		f := strings.Fields(string(tm))
		if len(f) == 3 {
			pressure, _ := strconv.ParseUint(f[1], 10, 64)
			maxP, _ := strconv.ParseUint(f[2], 10, 64)
			fmt.Printf("  tcp_mem: pressure at %s · hard max %s\n",
				humanKB(pressure*pageKB), humanKB(maxP*pageKB))
			if pressure > 0 && tcpPages >= pressure {
				warnf(sevWarn, "TCP is in memory-pressure mode: socket buffers squeezed, throughput suffers",
					"TCP buffer memory %s exceeds the tcp_mem pressure threshold %s",
					humanKB(tcpPages*pageKB), humanKB(pressure*pageKB))
			}
		}
	}
	fmt.Println()
	if totalRAMKB > 0 && tcpPages*pageKB > totalRAMKB/50 {
		warnf(sevWarn, "socket buffer bloat — check for slow consumers and rmem/wmem maximums",
			"TCP buffers hold %s (>2%% of RAM)", humanKB(tcpPages*pageKB))
	}
}

// bdiStat is one backing device's writeback state from
// /sys/kernel/debug/bdi/<dev>/stats (root only).
type bdiStat struct {
	dev         string
	dirtyKB     uint64 // BdiReclaimable: dirty pages waiting for writeback
	writebackKB uint64
	bwKBps      uint64
}

// printBDI shows which disks pending writeback is queued on. When reclaim
// stalls behind dirty data (GFP_NOFS kills, Dirty stuck high), this names
// the device that cannot keep up.
func printBDI() {
	root := "/sys/kernel/debug/bdi"
	ents, err := os.ReadDir(root)
	if err != nil {
		return // non-root, or debugfs not mounted
	}
	var stats []bdiStat
	for _, e := range ents {
		b, err := os.ReadFile(filepath.Join(root, e.Name(), "stats"))
		if err != nil {
			continue
		}
		st := bdiStat{dev: blockDevName(e.Name())}
		for _, line := range strings.Split(string(b), "\n") {
			f := strings.Fields(line)
			if len(f) < 2 {
				continue
			}
			v, err := strconv.ParseUint(f[1], 10, 64)
			if err != nil {
				continue
			}
			switch f[0] {
			case "BdiReclaimable:":
				st.dirtyKB = v
			case "BdiWriteback:":
				st.writebackKB = v
			case "BdiWriteBandwidth:":
				st.bwKBps = v
			}
		}
		if st.dirtyKB > 0 || st.writebackKB > 0 {
			stats = append(stats, st)
		}
	}
	section("WRITEBACK BY DEVICE (bdi)")
	if len(stats) == 0 {
		fmt.Printf("  %s no dirty data pending on any device\n\n", tOK())
		return
	}
	sort.Slice(stats, func(i, j int) bool {
		return stats[i].dirtyKB+stats[i].writebackKB > stats[j].dirtyKB+stats[j].writebackKB
	})
	fmt.Printf("  %-16s %12s %12s %14s\n", "DEVICE", "DIRTY", "WRITEBACK", "EST BANDWIDTH")
	for i, st := range stats {
		if i >= 8 {
			break
		}
		fmt.Printf("  %-16s %12s %12s %12s/s\n",
			st.dev, humanKB(st.dirtyKB), humanKB(st.writebackKB), humanKB(st.bwKBps))
	}
	fmt.Println(dim("  reclaim of dirty pages waits on these devices; a slow one stalls the box"))
	fmt.Println()
}

// blockDevName resolves a "MAJ:MIN" bdi directory name to the block device
// name via /sys/dev/block; other names (e.g. for nfs) pass through.
func blockDevName(majmin string) string {
	l, err := os.Readlink("/sys/dev/block/" + majmin)
	if err != nil {
		return majmin
	}
	return filepath.Base(l) + " (" + majmin + ")"
}
