package main

import (
	"fmt"
	"os"
	"sort"
	"strconv"
	"strings"
)

// allocSite is one /proc/allocinfo row: exact live-byte accounting for one
// kernel allocation call site (CONFIG_MEM_ALLOC_PROFILING, kernel >= 6.10).
// Unlike slabinfo this covers raw alloc_pages() too — it attributes the
// memory that no meminfo category counts (xfs_buf data pages, driver
// buffers, dma-buf).
type allocSite struct {
	bytes uint64
	calls uint64
	site  string
}

func readAllocinfo() []allocSite {
	b, err := os.ReadFile("/proc/allocinfo")
	if err != nil {
		return nil
	}
	var out []allocSite
	for _, line := range strings.Split(string(b), "\n") {
		fields := strings.Fields(line)
		if len(fields) < 3 {
			continue
		}
		// The size column is signed: a call site can transiently report
		// negative live bytes under allocation profiling. Keep the line
		// (dropping it would understate the total) but floor it at zero.
		signed, err := strconv.ParseInt(fields[0], 10, 64)
		if err != nil {
			continue // header lines
		}
		bytes := uint64(max(signed, 0))
		calls, err := strconv.ParseUint(fields[1], 10, 64)
		if err != nil {
			continue
		}
		out = append(out, allocSite{bytes: bytes, calls: calls, site: strings.Join(fields[2:], " ")})
	}
	sort.Slice(out, func(i, j int) bool { return out[i].bytes > out[j].bytes })
	return out
}

func printAllocinfo(totalRAMKB uint64) {
	section("KERNEL ALLOCATION PROFILING (/proc/allocinfo)")
	sites := readAllocinfo()
	if len(sites) == 0 {
		fmt.Println(dim("  unavailable — needs kernel ≥6.10 with CONFIG_MEM_ALLOC_PROFILING"))
		fmt.Println(dim("  (boot with mem_profiling=1); this is the definitive per-callsite"))
		fmt.Println(dim("  accounting of ALL kernel memory, including raw page allocations"))
		if _, err := os.Stat("/sys/kernel/debug/page_owner"); err == nil {
			fmt.Println(dim("  page_owner is available on this kernel as an alternative"))
		}
		fmt.Println()
		return
	}
	var total uint64
	for _, s := range sites {
		total += s.bytes
	}
	fmt.Printf("  total attributed kernel memory: %s\n", human(float64(total)))
	fmt.Printf("  %14s %12s  %s\n", "BYTES", "CALLS", "CALL SITE")
	for i, s := range sites {
		if i >= 15 || s.bytes == 0 {
			break
		}
		site := s.site
		if len(site) > 80 {
			site = site[:77] + "..."
		}
		fmt.Printf("  %14s %12d  %s\n", human(float64(s.bytes)), s.calls, site)
	}
	fmt.Println()

	if len(sites) > 0 && totalRAMKB > 0 {
		top := sites[0]
		pct := float64(top.bytes/1024) / float64(totalRAMKB) * 100
		if pct >= 10 {
			warnf(sevWarn, "exact attribution — this call site owns the memory whatever slabtop says",
				"kernel call site %s holds %s (%.0f%% of RAM)", top.site, human(float64(top.bytes)), pct)
		}
	}
}
