// Command memory-analysis diagnoses kernel-memory pressure of the kind that
// OOM-kills processes while plenty of RAM is "free": unbounded slab growth
// (e.g. filesystem inode caches), SLUB merged-cache misattribution, physical
// memory fragmentation, and the gap between kernel-owned memory and what the
// OOM killer can see. It only reads kernel interfaces (/proc, /sys); the one
// exception is temporarily enabling kmem tracepoints for live allocation
// attribution, and every touched file is restored on exit, crash, or signal.
package main

import (
	"flag"
	"fmt"
	"os"
	"os/signal"
	"sort"
	"strings"
	"syscall"
	"time"
)

func main() {
	if len(os.Args) > 1 && os.Args[1] == "live" {
		fs := flag.NewFlagSet("live", flag.ExitOnError)
		interval := fs.Duration("interval", 5*time.Second, "refresh window for both panes")
		_ = fs.Parse(os.Args[2:])
		os.Exit(runLive(*interval))
	}
	var (
		sample   = flag.Duration("sample", 10*time.Second, "observation window for slab growth and live tracing (0 = skip)")
		noTrace  = flag.Bool("no-trace", false, "strictly read-only: never modify tracefs (disables live attribution)")
		topSlabs = flag.Int("top-slabs", 15, "slab caches to list")
		topProcs = flag.Int("top-procs", 20, "processes to list")
		culprits = flag.Int("culprits", 3, "top slab caches to analyze in depth")
	)
	flag.Parse()
	os.Exit(run(*sample, *noTrace, *topSlabs, *topProcs, *culprits))
}

func run(sample time.Duration, noTrace bool, topSlabs, topProcs, nCulprits int) int {
	rev := newReverter()
	defer rev.restoreAll()
	sig := make(chan os.Signal, 1)
	signal.Notify(sig, os.Interrupt, syscall.SIGTERM, syscall.SIGHUP)
	go func() {
		s := <-sig
		fmt.Fprintf(os.Stderr, "\n[!] caught %v — restoring kernel settings before exit\n", s)
		rev.restoreAll()
		os.Exit(130)
	}()

	root := os.Geteuid() == 0
	if !root {
		fmt.Fprintln(os.Stderr, "[!] not running as root: /proc/slabinfo, /proc/pagetypeinfo and")
		fmt.Fprintln(os.Stderr, "    tracing are unavailable — output will be partial")
	}

	mi, err := readMeminfo()
	if err != nil {
		fmt.Fprintf(os.Stderr, "read /proc/meminfo: %v\n", err)
		return 1
	}
	printOverview(mi)

	slabs, slabErr := readSlabinfo()
	sysfs := loadSlabSysfs()
	var lockstep [][]*slabCache
	var culpritCaches []*slabCache
	if slabErr != nil {
		fmt.Printf("==== SLAB CACHES ====\n  unavailable: %v\n\n", slabErr)
	} else {
		printSlabTop(slabs, sysfs, topSlabs)
		lockstep = detectLockstep(slabs)
		printLockstep(lockstep)
		culpritCaches = pickCulprits(slabs, nCulprits)
		printCulprits(culpritCaches, sysfs)
	}

	printDentryState()

	procs := readProcs(mi["MemTotal"] + mi["SwapTotal"])
	printProcs(procs, topProcs)
	cgs := readCgroupSlab()
	printCgroupSlab(cgs, 12, mi["Slab"])
	printCgroupLimits(cgs)

	printNUMA()
	printShrinkers(mi["MemTotal"])
	printPSI()
	printVMSettings(mi)
	printSockMem(mi["MemTotal"])
	printBDI()
	printAllocinfo(mi["MemTotal"])

	zones := readFragmentation()
	printFrag(zones)

	printOOMLog(readVMStat("oom_kill")["oom_kill"])

	// Observation window: slab growth is measured read-only; live tracing
	// additionally needs root and permission to touch tracefs.
	var attr *traceResult
	growth := map[string]slabGrowth{}
	sizeLabels := map[uint64]string{}
	if sample > 0 && slabErr == nil && len(culpritCaches) > 0 {
		var sizes []uint64
		for _, c := range culpritCaches {
			sizes = append(sizes, c.objSize)
			label := fmt.Sprintf(" (reported as %q", c.name)
			if m := sysfs.members(c.name); len(m) > 1 {
				label += "; merged: " + strings.Join(m, ",")
			}
			label += ")"
			if prev, dup := sizeLabels[c.objSize]; dup {
				label = prev // first culprit wins the label
			}
			sizeLabels[c.objSize] = label
		}
		vmBefore := readVMStat(windowVMKeys...)
		switch {
		case noTrace:
			fmt.Printf("---- observing slab growth for %s (read-only, tracing disabled) ----\n\n", sample)
			time.Sleep(sample)
		case !root:
			fmt.Printf("---- observing slab growth for %s (no root, tracing skipped) ----\n\n", sample)
			time.Sleep(sample)
		default:
			fmt.Printf("---- tracing culprit-size allocations for %s ----\n\n", sample)
			var terr error
			attr, terr = traceAllocations(rev, sizes, sample)
			if terr != nil {
				fmt.Fprintf(os.Stderr, "[!] tracing unavailable: %v\n", terr)
				time.Sleep(sample)
			}
		}
		// All kernel state restored here; everything below is read-only.
		rev.restoreAll()
		if after, err := readSlabinfo(); err == nil {
			growth = diffSlabs(slabs, after)
		}
		printGrowth(culpritCaches, growth, sample)
		printVMWindow(vmBefore, readVMStat(windowVMKeys...), sample)
	}
	if attr != nil {
		printAttribution(attr, sizeLabels)
	}

	printSummary(mi, slabs, sysfs, lockstep, culpritCaches, procs, zones, growth, attr)
	printWarnings()
	return 0
}

func printGrowth(culprits []*slabCache, growth map[string]slabGrowth, window time.Duration) {
	if len(growth) == 0 {
		return
	}
	section(fmt.Sprintf("CULPRIT GROWTH over %s", window))
	for _, c := range culprits {
		g, ok := growth[c.name]
		if !ok {
			continue
		}
		trend := dim("flat")
		switch {
		case g.deltaBytes > 0:
			trend = yellow("GROWING")
		case g.deltaBytes < 0:
			trend = green("shrinking")
		}
		fmt.Printf("  %-26s %+12d objs  %12s  %s\n",
			c.name, g.deltaObjs, human(float64(g.deltaBytes)), trend)
		// Growth over a seconds-scale window is only alarming when it
		// extrapolates to filling RAM within hours.
		perHour := float64(g.deltaBytes) / window.Seconds() * 3600
		if perHour > 1<<30 {
			warnf(sevWarn, "if sustained this fills RAM — see live attribution for the allocating call site",
				"slab cache %s is growing at ~%s/hour", c.name, human(perHour))
		}
	}
	fmt.Println()
}

func printSummary(mi meminfo, slabs []*slabCache, sysfs *slabSysfs,
	lockstep [][]*slabCache, culprits []*slabCache, procs []*procInfo,
	zones []*zoneFrag, growth map[string]slabGrowth, attr *traceResult,
) {
	section("SUMMARY")
	total := mi["MemTotal"]
	slabKB := mi["Slab"]

	sev := func(pct float64, warn, crit float64) string {
		switch {
		case pct >= crit:
			return tCrit()
		case pct >= warn:
			return tWarn()
		}
		return tOK()
	}

	if total > 0 {
		slabPct := float64(slabKB) / float64(total) * 100
		fmt.Printf("  %s slab %s = %.1f%% of RAM  (R %s / U %s)\n",
			sev(slabPct, 25, 50), humanKB(slabKB), slabPct,
			humanKB(mi["SReclaimable"]), humanKB(mi["SUnreclaim"]))
		if slabPct >= 25 {
			s := sevWarn
			if slabPct >= 50 {
				s = sevCrit
			}
			warnf(s, "see the slab, cgroup, and culprit sections for the owner",
				"slab holds %s (%.0f%% of RAM; unreclaimable %s)",
				humanKB(slabKB), slabPct, humanKB(mi["SUnreclaim"]))
		}
		availPct := float64(mi["MemAvailable"]) / float64(total) * 100
		fmt.Printf("  %s available %s = %.1f%% of RAM\n",
			sev(100-availPct, 80, 93), humanKB(mi["MemAvailable"]), availPct)
		if availPct <= 20 {
			s := sevWarn
			if availPct <= 7 {
				s = sevCrit
			}
			warnf(s, "MemAvailable does not count shrinker-held memory (xfs-buf etc.) — the true headroom may be larger; verify with the shrinker section",
				"MemAvailable is %s (%.1f%% of RAM)", humanKB(mi["MemAvailable"]), availPct)
		}
	}

	// xfs_buf slab entries are only headers; the 4-16KB metadata blocks
	// they reference appear in no /proc/meminfo counter. The buffer
	// shrinker's count is the only figure the kernel reports for them.
	// (When debugfs is unreadable the SHRINKERS section prints the manual
	// commands.)
	if counts, err := readShrinkerCounts("xfs-buf:"); err == nil && len(counts) > 0 {
		var bufs uint64
		for _, n := range counts {
			bufs += n
		}
		lowKB, highKB := bufs*4, bufs*16 // metadata buffers are 4-16KB
		pct := float64(0)
		if total > 0 {
			pct = float64(lowKB) / float64(total) * 100
		}
		fmt.Printf("  %s xfs-buf shrinker: %d reclaimable buffers on %d drives — %s-%s of buffer pages meminfo does not count\n",
			sev(pct, 25, 50), bufs, len(counts), humanKB(lowKB), humanKB(highKB))
	}

	if len(culprits) > 0 {
		c := culprits[0]
		fmt.Printf("  %s biggest slab: %s  %s, %d objs\n",
			tInfo(), bold(c.name), human(float64(c.totalBytes)), c.numObjs)
		for _, cc := range culprits {
			if m := sysfs.members(cc.name); len(m) > 1 {
				fmt.Printf("  %s %q is a merged class — real owner one of: %s\n",
					tWarn(), cc.name, strings.Join(m, ", "))
			}
		}
	}
	for _, g := range lockstep {
		var names []string
		for _, c := range g {
			names = append(names, c.name)
		}
		fmt.Printf("  %s lockstep ~%d objs: %s %s\n",
			tInfo(), g[0].numObjs, strings.Join(names, ", "),
			dim("(per-object companions, freed together)"))
	}

	for _, z := range zones {
		if z.zone != "Normal" {
			continue
		}
		order, exact := z.maxUsableOrder()
		pageKB := uint64(os.Getpagesize()) / 1024
		switch {
		case order < 0:
			fmt.Printf("  %s zone Normal(n%s): no free blocks\n", tCrit(), z.node)
		case order < 3 && exact:
			fmt.Printf("  %s zone Normal(n%s): max usable block order %d (%s) — 32KB kernel allocs WILL fail\n",
				tCrit(), z.node, order, humanKB(pageKB<<order))
			warnf(sevCrit, "order-3+ allocations (network jumbo frames, some drivers) fail with memory free — the OOM-with-free-memory precursor; compaction cannot fix Unmovable fragmentation",
				"zone Normal node %s is fragmented: largest usable block is order %d (%s)",
				z.node, order, humanKB(pageKB<<order))
		case order < 3:
			fmt.Printf("  %s zone Normal(n%s): max free block order %d %s\n",
				tWarn(), z.node, order, dim("(approx — rerun as root)"))
		default:
			fmt.Printf("  %s zone Normal(n%s): contiguous up to order %d (%s)\n",
				tOK(), z.node, order, humanKB(pageKB<<order))
		}
		if z.highAtomicKB > 0 {
			fmt.Printf("  %s highatomic reserve fences off %s\n", tInfo(), humanKB(z.highAtomicKB))
		}
	}
	// Whether the fragmentation above can self-heal: mostly-failing
	// compaction means it cannot.
	if vs := readVMStat("compact_success", "compact_fail"); len(vs) > 0 {
		if attempts := vs["compact_success"] + vs["compact_fail"]; attempts >= 50 {
			rate := float64(vs["compact_fail"]) / float64(attempts) * 100
			if rate >= 20 {
				fmt.Printf("  %s compaction failing %.0f%% of %d attempts — fragmentation is not self-healing\n",
					sev(rate, 20, 50), rate, attempts)
			}
		}
	}

	var growing []string
	for _, c := range culprits {
		if g, ok := growth[c.name]; ok && g.deltaBytes > 0 {
			growing = append(growing, c.name+"(+"+human(float64(g.deltaBytes))+")")
		}
	}
	if len(growing) > 0 {
		fmt.Printf("  %s growing during window: %s\n", tWarn(), strings.Join(growing, " "))
	}

	if attr != nil {
		// Sorted so two runs over identical data produce diffable output.
		sizes := make([]uint64, 0, len(attr.perSize))
		for size := range attr.perSize {
			sizes = append(sizes, size)
		}
		sort.Slice(sizes, func(i, j int) bool { return sizes[i] < sizes[j] })
		for _, size := range sizes {
			var best string
			var bestN uint64
			for site, st := range attr.perSize[size] {
				// Ties broken by name: otherwise map order decides.
				if st.count > bestN || (st.count == bestN && site < best) {
					best, bestN = site, st.count
				}
			}
			if bestN > 0 {
				fmt.Printf("  %s %dB allocs ← %s (%d in window)\n", tInfo(), size, bold(best), bestN)
			}
		}
	}

	if len(procs) > 0 && !procs[0].immune() {
		p := procs[0]
		fmt.Printf("  %s next OOM victim: %s pid %d (rss %s, oom_score_adj %d)\n",
			tInfo(), bold(p.name), p.pid, humanKB(p.rssKB), p.oomAdj)
		var sumRSS uint64
		for _, pp := range procs {
			sumRSS += pp.rssKB
		}
		if slabKB > sumRSS {
			fmt.Printf("  %s slab %s > all userspace %s — killing processes cannot help\n",
				tWarn(), humanKB(slabKB), humanKB(sumRSS))
		}
	}
	fmt.Println(dim("  hints: slub_nomerge = exact slab names · slub_debug=U = historical alloc_calls"))
}
