package main

import (
	"bufio"
	"fmt"
	"os"
	"strconv"
	"strings"
	"time"
)

// meminfo holds /proc/meminfo values in KiB.
type meminfo map[string]uint64

func readMeminfo() (meminfo, error) {
	f, err := os.Open("/proc/meminfo")
	if err != nil {
		return nil, err
	}
	defer f.Close()
	mi := meminfo{}
	sc := bufio.NewScanner(f)
	for sc.Scan() {
		fields := strings.Fields(sc.Text())
		if len(fields) < 2 {
			continue
		}
		v, err := strconv.ParseUint(fields[1], 10, 64)
		if err == nil {
			mi[strings.TrimSuffix(fields[0], ":")] = v
		}
	}
	return mi, sc.Err()
}

// human renders a byte count as a short human-readable string.
func human(b float64) string {
	if b < 0 {
		return "-" + human(-b)
	}
	units := []string{"B", "KB", "MB", "GB", "TB"}
	i := 0
	for b >= 1024 && i < len(units)-1 {
		b /= 1024
		i++
	}
	return fmt.Sprintf("%.1f %s", b, units[i])
}

func humanKB(kb uint64) string { return human(float64(kb) * 1024) }

func printOverview(mi meminfo) {
	section("SYSTEM MEMORY OVERVIEW")
	total := mi["MemTotal"]
	rows := []struct{ label, key string }{
		{"Total RAM", "MemTotal"},
		{"Free", "MemFree"},
		{"Available", "MemAvailable"},
		{"Page cache", "Cached"},
		{"Buffers", "Buffers"},
		{"Anonymous", "AnonPages"},
		{"Shmem", "Shmem"},
		{"Unevictable", "Unevictable"},
		{"  mlocked", "Mlocked"},
		{"Hugetlb", "Hugetlb"},
		{"Slab total", "Slab"},
		{"  reclaimable", "SReclaimable"},
		{"  unreclaimable", "SUnreclaim"},
		{"Page tables", "PageTables"},
		{"Kernel stacks", "KernelStack"},
		{"Vmalloc used", "VmallocUsed"},
		{"Percpu", "Percpu"},
		{"Swap total", "SwapTotal"},
		{"Swap free", "SwapFree"},
	}
	for _, r := range rows {
		v, ok := mi[r.key]
		if !ok {
			continue
		}
		pct := ""
		if total > 0 && r.key != "MemTotal" && !strings.HasPrefix(r.key, "Swap") {
			pct = fmt.Sprintf("  (%.1f%% of RAM)", float64(v)/float64(total)*100)
		}
		fmt.Printf("  %-16s %12s%s\n", r.label, humanKB(v), pct)
	}
	accounted := mi["MemFree"] + mi["Cached"] + mi["Buffers"] + mi["AnonPages"] +
		mi["Slab"] + mi["PageTables"] + mi["KernelStack"] + mi["VmallocUsed"] +
		mi["Percpu"] + mi["Hugetlb"]
	if total > accounted {
		other := total - accounted
		fmt.Printf("  %-16s %12s  (drivers, alloc_pages, untracked)\n", "Other", humanKB(other))
		if other > total*15/100 {
			warnf(sevWarn, "kernel memory outside every meminfo category — see SHRINKERS (xfs-buf data pages live here) and /proc/allocinfo; GPU/dma-buf also lands here",
				"%s (%.0f%% of RAM) is in no meminfo category", humanKB(other), float64(other)/float64(total)*100)
		}
	}
	fmt.Println()

	if hc := mi["HardwareCorrupted"]; hc > 0 {
		warnf(sevWarn, "the kernel has retired pages for memory errors — check EDAC counters / mcelog / BMC event log",
			"HardwareCorrupted: %s of RAM offlined", humanKB(hc))
	}
	if total > 0 && mi["Mlocked"] > total/20 {
		warnf(sevWarn, "mlocked memory can never be reclaimed or swapped — see the LOCKED column in the process table",
			"%s is mlocked (>5%% of RAM)", humanKB(mi["Mlocked"]))
	}
	if ht, hf := mi["HugePages_Total"], mi["HugePages_Free"]; ht > 0 && hf == ht {
		warnf(sevInfo, "hugepage pool is allocated but no process uses it — vm.nr_hugepages fences this memory off from everything else",
			"%s reserved in hugepages, all unused", humanKB(mi["Hugetlb"]))
	}
}

// zoneFrag describes free-memory fragmentation for one memory zone.
type zoneFrag struct {
	node string
	zone string
	// buddy holds free block counts per order across all migrate types
	// (from /proc/buddyinfo, world-readable).
	buddy []uint64
	// usable holds free block counts per order excluding the
	// HighAtomic/CMA/Isolate reserves that normal GFP_KERNEL allocations
	// cannot use (from /proc/pagetypeinfo, root only).
	usable         []uint64
	highAtomicKB   uint64
	blocksByType   map[string]uint64
	totalBlocks    uint64
	havePagetype   bool
	pageBlockOrder int
	// zone accounting from /proc/zoneinfo, in pages. Free blocks are only
	// the buddy allocator's organization of FREE memory — allocated pages
	// are not blocks, so "used" is only meaningful at the zone level.
	managed      uint64
	freePages    uint64
	wmin         uint64
	wlow         uint64
	whigh        uint64
	zFile        uint64 // page cache pages in this zone
	zAnon        uint64 // anonymous pages in this zone
	haveZoneinfo bool
}

func readFragmentation() []*zoneFrag {
	var zones []*zoneFrag
	byKey := map[string]*zoneFrag{}

	if f, err := os.Open("/proc/buddyinfo"); err == nil {
		sc := bufio.NewScanner(f)
		for sc.Scan() {
			fields := strings.Fields(sc.Text())
			// Node 0, zone   Normal   203161 863902 ...
			if len(fields) < 5 || fields[0] != "Node" {
				continue
			}
			z := &zoneFrag{
				node:         strings.TrimSuffix(fields[1], ","),
				zone:         fields[3],
				blocksByType: map[string]uint64{},
			}
			for _, s := range fields[4:] {
				n, err := strconv.ParseUint(s, 10, 64)
				if err != nil {
					break
				}
				z.buddy = append(z.buddy, n)
			}
			zones = append(zones, z)
			byKey[z.node+"/"+z.zone] = z
		}
		f.Close()
	}

	parsePagetypeinfo(byKey)
	parseZoneinfo(byKey)
	return zones
}

// parseZoneinfo fills zone-level page accounting (world-readable).
func parseZoneinfo(byKey map[string]*zoneFrag) {
	f, err := os.Open("/proc/zoneinfo")
	if err != nil {
		return
	}
	defer f.Close()
	var z *zoneFrag
	sc := bufio.NewScanner(f)
	for sc.Scan() {
		fields := strings.Fields(sc.Text())
		if len(fields) == 0 {
			continue
		}
		if fields[0] == "Node" && len(fields) >= 4 {
			z = byKey[strings.TrimSuffix(fields[1], ",")+"/"+fields[3]]
			continue
		}
		if z == nil || len(fields) < 2 {
			continue
		}
		u := func(s string) uint64 { n, _ := strconv.ParseUint(s, 10, 64); return n }
		// per-cpu pageset lines use "high:"/"count:" (with colon) and do
		// not collide with the watermark keys below.
		switch fields[0] {
		case "pages":
			if len(fields) >= 3 && fields[1] == "free" {
				z.freePages = u(fields[2])
				z.haveZoneinfo = true
			}
		case "min":
			z.wmin = u(fields[1])
		case "low":
			z.wlow = u(fields[1])
		case "high":
			z.whigh = u(fields[1])
		case "managed":
			z.managed = u(fields[1])
		case "nr_zone_active_file", "nr_zone_inactive_file":
			z.zFile += u(fields[1])
		case "nr_zone_active_anon", "nr_zone_inactive_anon":
			z.zAnon += u(fields[1])
		}
	}
}

func parsePagetypeinfo(byKey map[string]*zoneFrag) {
	f, err := os.Open("/proc/pagetypeinfo")
	if err != nil {
		return
	}
	defer f.Close()

	const (
		modeNone = iota
		modeFree
		modeBlocks
	)
	mode := modeNone
	var blockTypes []string
	pageBlockOrder := 0
	pageSize := uint64(os.Getpagesize())

	sc := bufio.NewScanner(f)
	for sc.Scan() {
		line := sc.Text()
		switch {
		case strings.HasPrefix(line, "Page block order:"):
			fields := strings.Fields(line)
			pageBlockOrder, _ = strconv.Atoi(fields[len(fields)-1])
			continue
		case strings.Contains(line, "Free pages count per migrate type at order"):
			mode = modeFree
			continue
		case strings.Contains(line, "Number of blocks type"):
			fields := strings.Fields(line)
			// "Number of blocks type <T1> <T2> ..."
			if len(fields) > 4 {
				blockTypes = fields[4:]
			}
			mode = modeBlocks
			continue
		}
		fields := strings.Fields(line)
		if len(fields) == 0 || fields[0] != "Node" {
			continue
		}
		node := strings.TrimSuffix(fields[1], ",")
		zone := strings.TrimSuffix(fields[3], ",")
		z := byKey[node+"/"+zone]
		if z == nil {
			continue
		}
		z.havePagetype = true
		z.pageBlockOrder = pageBlockOrder
		switch mode {
		case modeFree:
			// Node 0, zone Normal, type Unmovable <counts per order>
			if len(fields) < 7 || fields[4] != "type" {
				continue
			}
			typ := fields[5]
			for order, s := range fields[6:] {
				n, err := strconv.ParseUint(s, 10, 64)
				if err != nil {
					break
				}
				for len(z.usable) <= order {
					z.usable = append(z.usable, 0)
				}
				switch typ {
				case "HighAtomic":
					z.highAtomicKB += (n << order) * pageSize / 1024
				case "CMA", "Isolate":
					// unusable for normal allocations
				default:
					z.usable[order] += n
				}
			}
		case modeBlocks:
			// Node 0, zone Normal <counts per block type>
			for i, s := range fields[4:] {
				if i >= len(blockTypes) {
					break
				}
				n, err := strconv.ParseUint(s, 10, 64)
				if err != nil {
					break
				}
				z.blocksByType[blockTypes[i]] += n
				z.totalBlocks += n
			}
		}
	}
}

// maxUsableOrder returns the highest order with free blocks available to
// normal allocations, or -1. Falls back to raw buddyinfo (which cannot
// exclude the HighAtomic reserve) when pagetypeinfo was unreadable.
func (z *zoneFrag) maxUsableOrder() (order int, exact bool) {
	order = -1
	if z.havePagetype {
		for o, n := range z.usable {
			if n > 0 {
				order = o
			}
		}
		return order, true
	}
	for o, n := range z.buddy {
		if n > 0 {
			order = o
		}
	}
	return order, false
}

// orderLabel renders a buddy block size compactly: "4K", "64K", "4M".
func orderLabel(kb uint64) string {
	if kb >= 1024 {
		return strconv.FormatUint(kb/1024, 10) + "M"
	}
	return strconv.FormatUint(kb, 10) + "K"
}

func printFrag(zones []*zoneFrag) {
	section("PHYSICAL FRAGMENTATION (buddy allocator)")
	if len(zones) == 0 {
		fmt.Println(dim("  /proc/buddyinfo unavailable"))
		fmt.Println()
		return
	}
	pageKB := uint64(os.Getpagesize()) / 1024
	maxOrders := 0
	exact := false
	for _, z := range zones {
		if len(z.buddy) > maxOrders {
			maxOrders = len(z.buddy)
		}
		if z.havePagetype {
			exact = true
		}
	}
	if exact {
		fmt.Println(dim("  free blocks per order, usable by normal allocations (HighAtomic/CMA excluded)"))
	} else {
		fmt.Println(dim("  free blocks per order, all migrate types (run as root to exclude reserves)"))
	}

	head := fmt.Sprintf("  %-11s", "ZONE")
	for o := 0; o < maxOrders; o++ {
		head += fmt.Sprintf(" %6s", orderLabel(pageKB<<o))
	}
	fmt.Println(dim(head + "  LARGEST"))

	for _, z := range zones {
		counts := z.buddy
		if z.havePagetype {
			counts = z.usable
		}
		row := fmt.Sprintf("  %-11s", "n"+z.node+" "+z.zone)
		for o := 0; o < maxOrders; o++ {
			var n uint64
			if o < len(counts) {
				n = counts[o]
			}
			cell := fmt.Sprintf(" %6d", n)
			if z.havePagetype && n == 0 && o >= 3 {
				cell = red(cell) // nothing usable at this order
			}
			row += cell
		}
		order, zoneExact := z.maxUsableOrder()
		largest := "none"
		if order >= 0 {
			largest = humanKB(pageKB << order)
		}
		largest = fmt.Sprintf("  %7s", largest)
		switch {
		case order < 3:
			largest = red(largest + " ✗")
		case order < 5:
			largest = yellow(largest)
		default:
			largest = green(largest)
		}
		if !zoneExact {
			largest += dim(" ~")
		}
		fmt.Println(row + largest)
	}

	// Per-zone context worth a line: fenced-off reserves and how much of
	// the zone compaction can never defragment.
	for _, z := range zones {
		var notes []string
		if z.highAtomicKB > 0 {
			notes = append(notes, fmt.Sprintf("HighAtomic reserve %s (off-limits to normal allocations)", humanKB(z.highAtomicKB)))
		}
		if z.totalBlocks > 0 {
			un := z.blocksByType["Unmovable"]
			notes = append(notes, fmt.Sprintf("pageblocks %.0f%% Unmovable (slab/kernel — compaction cannot fix)",
				float64(un)/float64(z.totalBlocks)*100))
		}
		if len(notes) > 0 {
			fmt.Printf("  %-11s %s\n", "n"+z.node+" "+z.zone, dim(strings.Join(notes, " · ")))
		}
	}
	if vs := readVMStat("compact_stall", "compact_fail", "compact_success"); len(vs) > 0 {
		line := fmt.Sprintf("  compaction since boot: stalls %d · success %d · fail %d",
			vs["compact_stall"], vs["compact_success"], vs["compact_fail"])
		// The failure rate answers "will this self-heal": compaction is the
		// kernel's only fix for fragmentation, and it fails when the blocks
		// are pinned by Unmovable (slab/kernel) pages — the pageblock share
		// printed above. A high rate means the fragmentation is permanent.
		if attempts := vs["compact_success"] + vs["compact_fail"]; attempts >= 50 {
			rate := float64(vs["compact_fail"]) / float64(attempts) * 100
			rs := fmt.Sprintf(" · %.0f%% failing", rate)
			switch {
			case rate >= 50:
				rs = red(rs)
			case rate >= 20:
				rs = yellow(rs)
			}
			line += rs
			if rate >= 50 {
				fragged := false
				for _, z := range zones {
					if o, _ := z.maxUsableOrder(); z.zone == "Normal" && o < 3 {
						fragged = true
					}
				}
				if fragged {
					warnf(sevCrit, "zone Normal is already fragmented AND compaction cannot fix it — high-order allocations keep failing until the Unmovable consumer (slab) shrinks or the host reboots",
						"%.0f%% of %d compaction attempts have failed", rate, attempts)
				} else {
					warnf(sevWarn, "fragmentation will not self-heal: the pages blocking compaction are Unmovable (slab/kernel) — see the pageblock share and slab sections for the consumer",
						"%.0f%% of %d compaction attempts have failed", rate, attempts)
				}
			}
		}
		fmt.Println(line)
	}
	fmt.Println()
}

// windowVMKeys are the kernel "struggle" counters diffed across the sample
// window. Since-boot totals blur into history; the window deltas answer
// "is the kernel fighting for memory right now".
var windowVMKeys = []string{
	"pgscan_kswapd", "pgscan_direct", "pgsteal_kswapd", "pgsteal_direct",
	"allocstall_dma", "allocstall_dma32", "allocstall_normal",
	"allocstall_movable", "allocstall_device",
	"compact_stall", "compact_success", "compact_fail",
	"workingset_refault_file", "workingset_refault_anon", "oom_kill",
}

// printVMWindow reports reclaim/compaction activity across the observation
// window, mirroring the live-mode pane for one-shot runs.
func printVMWindow(before, after map[string]uint64, window time.Duration) {
	if len(before) == 0 || len(after) == 0 {
		return
	}
	d := func(k string) uint64 {
		if after[k] < before[k] {
			return 0 // counter reset
		}
		return after[k] - before[k]
	}
	scanned := d("pgscan_kswapd") + d("pgscan_direct")
	stolen := d("pgsteal_kswapd") + d("pgsteal_direct")
	stalls := d("allocstall_dma") + d("allocstall_dma32") + d("allocstall_normal") +
		d("allocstall_movable") + d("allocstall_device")
	refaults := d("workingset_refault_file") + d("workingset_refault_anon")
	compAtt := d("compact_success") + d("compact_fail")
	section(fmt.Sprintf("RECLAIM ACTIVITY over %s window", window))
	if scanned == 0 && stalls == 0 && compAtt == 0 && d("oom_kill") == 0 {
		fmt.Printf("  idle — no page scanning, allocation stalls, or compaction\n\n")
		return
	}
	scan := fmt.Sprintf("  pgscan     kswapd +%d · direct +%d · alloc stalls +%d",
		d("pgscan_kswapd"), d("pgscan_direct"), stalls)
	if d("pgscan_direct") > 0 || stalls > 0 {
		scan = yellow(scan) // direct reclaim = allocations are waiting on reclaim
	}
	fmt.Println(scan)
	steal := fmt.Sprintf("  pgsteal    kswapd +%d · direct +%d", d("pgsteal_kswapd"), d("pgsteal_direct"))
	if scanned > 0 {
		eff := float64(stolen) / float64(scanned) * 100
		effS := fmt.Sprintf(" · efficiency %.0f%%", eff)
		switch {
		case eff < 20:
			effS = red(effS)
		case eff < 50:
			effS = yellow(effS)
		}
		steal += effS
		// Thousands of pages scanned at low yield is the thrash signature:
		// reclaim runs flat out and frees almost nothing.
		if eff < 20 && scanned >= 10000 {
			warnf(sevWarn, "reclaim is scanning frantically and freeing little — check refaults, PSI, and the shrinker section for pinned caches",
				"reclaim efficiency %.0f%% during the window (scanned %d pages, freed %d)", eff, scanned, stolen)
		}
	}
	fmt.Println(steal)
	ref := fmt.Sprintf("  refaults   file +%d · anon +%d", d("workingset_refault_file"), d("workingset_refault_anon"))
	if refaults > 10000 {
		ref = yellow(ref + "  (reclaim is eating the working set, not cold cache)")
	}
	fmt.Println(ref)
	comp := fmt.Sprintf("  compaction stalls +%d · success +%d · fail +%d",
		d("compact_stall"), d("compact_success"), d("compact_fail"))
	if compAtt > 0 {
		rate := float64(d("compact_fail")) / float64(compAtt) * 100
		rs := fmt.Sprintf(" · %.0f%% failing", rate)
		if rate >= 50 {
			rs = red(rs)
		}
		comp += rs
	}
	fmt.Println(comp)
	if stalls > 0 {
		warnf(sevWarn, "allocations are blocking on direct reclaim — latency spikes precede OOM; see reclaim efficiency and PSI",
			"%d allocations stalled in direct reclaim during the %s window", stalls, window)
	}
	if k := d("oom_kill"); k > 0 {
		fmt.Println(red(fmt.Sprintf("  oom_kill   +%d DURING THIS RUN", k)))
		warnf(sevCrit, "the OOM killer fired while this tool was running — see the OOM forensics section for the victim",
			"%d OOM kill(s) during the %s observation window", k, window)
	}
	fmt.Println()
}

func readVMStat(keys ...string) map[string]uint64 {
	f, err := os.Open("/proc/vmstat")
	if err != nil {
		return nil
	}
	defer f.Close()
	want := map[string]bool{}
	for _, k := range keys {
		want[k] = true
	}
	out := map[string]uint64{}
	sc := bufio.NewScanner(f)
	for sc.Scan() {
		fields := strings.Fields(sc.Text())
		if len(fields) == 2 && want[fields[0]] {
			if v, err := strconv.ParseUint(fields[1], 10, 64); err == nil {
				out[fields[0]] = v
			}
		}
	}
	return out
}
