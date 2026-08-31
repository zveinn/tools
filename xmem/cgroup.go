package main

import (
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
)

const cgroupRoot = "/sys/fs/cgroup"

// cgSlab holds per-cgroup slab charges from memory.stat. SLAB_ACCOUNT
// caches (inode, dentry, and most other named caches) are charged to the
// allocating process's cgroup at allocation time — the closest thing the
// kernel offers to "which process owns this slab memory".
type cgSlab struct {
	path      string // relative to cgroupRoot
	depth     int
	slab      uint64 // bytes
	reclaim   uint64
	unrecl    uint64
	current   uint64 // memory.current, bytes
	limit     uint64 // memory.max; 0 = unlimited ("max")
	high      uint64 // memory.high; 0 = unset ("max")
	oomKills  uint64 // memory.events oom_kill
	limitHits uint64 // memory.events max
	pids      []int
}

func readCgroupSlab() []*cgSlab {
	// cgroup v2 only: unified hierarchy with memory controller.
	if _, err := os.Stat(cgroupRoot + "/cgroup.controllers"); err != nil {
		return nil
	}
	var out []*cgSlab
	const maxDepth = 6 // k8s pods sit at depth 5-6 (kubepods.slice/.../cri-containerd-...)
	var walk func(dir string, depth int)
	walk = func(dir string, depth int) {
		if depth > maxDepth {
			return
		}
		if cg := readOneCgroup(dir, depth); cg != nil {
			out = append(out, cg)
		}
		ents, err := os.ReadDir(filepath.Join(cgroupRoot, dir))
		if err != nil {
			return
		}
		for _, e := range ents {
			if e.IsDir() {
				walk(filepath.Join(dir, e.Name()), depth+1)
			}
		}
	}
	walk("", 0)
	sort.Slice(out, func(i, j int) bool { return out[i].slab > out[j].slab })
	return out
}

func readOneCgroup(dir string, depth int) *cgSlab {
	b, err := os.ReadFile(filepath.Join(cgroupRoot, dir, "memory.stat"))
	if err != nil {
		return nil
	}
	cg := &cgSlab{path: dir, depth: depth}
	for _, line := range strings.Split(string(b), "\n") {
		fields := strings.Fields(line)
		if len(fields) != 2 {
			continue
		}
		v, err := strconv.ParseUint(fields[1], 10, 64)
		if err != nil {
			continue
		}
		switch fields[0] {
		case "slab":
			cg.slab = v
		case "slab_reclaimable":
			cg.reclaim = v
		case "slab_unreclaimable":
			cg.unrecl = v
		}
	}
	if cg.slab == 0 {
		cg.slab = cg.reclaim + cg.unrecl
	}
	// "max" parses to 0 via the error path, which is exactly the sentinel
	// used for "no limit".
	readNum := func(name string) uint64 {
		b, err := os.ReadFile(filepath.Join(cgroupRoot, dir, name))
		if err != nil {
			return 0
		}
		v, _ := strconv.ParseUint(strings.TrimSpace(string(b)), 10, 64)
		return v
	}
	cg.current = readNum("memory.current")
	cg.limit = readNum("memory.max")
	cg.high = readNum("memory.high")
	if b, err := os.ReadFile(filepath.Join(cgroupRoot, dir, "memory.events")); err == nil {
		for _, line := range strings.Split(string(b), "\n") {
			fields := strings.Fields(line)
			if len(fields) != 2 {
				continue
			}
			v, _ := strconv.ParseUint(fields[1], 10, 64)
			switch fields[0] {
			case "oom_kill":
				cg.oomKills = v
			case "max":
				cg.limitHits = v
			}
		}
	}
	if cg.slab == 0 && cg.limit == 0 && cg.high == 0 && cg.oomKills == 0 {
		return nil
	}
	if p, err := os.ReadFile(filepath.Join(cgroupRoot, dir, "cgroup.procs")); err == nil {
		for _, s := range strings.Fields(string(p)) {
			if pid, err := strconv.Atoi(s); err == nil {
				cg.pids = append(cg.pids, pid)
				if len(cg.pids) >= 3 {
					break
				}
			}
		}
	}
	return cg
}

func procComm(pid int) string {
	b, err := os.ReadFile("/proc/" + strconv.Itoa(pid) + "/comm")
	if err != nil {
		return "?"
	}
	return strings.TrimSpace(string(b))
}

// printCgroupSlab ties slab memory to processes via memcg charging: the
// cgroup that allocated (accounted) slab is charged for it, and its member
// PIDs are listed. Values are hierarchical — children are included in
// parents — so read it as a tree, not a sum.
func printCgroupSlab(cgs []*cgSlab, n int, totalSlabKB uint64) {
	if len(cgs) == 0 {
		return
	}
	section("SLAB BY CGROUP (who got charged)")
	fmt.Printf("  %-52s %12s %12s  %s\n", "CGROUP", "SLAB", "UNRECL", "PIDS")
	var accounted uint64
	shown := 0
	for _, cg := range cgs {
		if cg.depth == 1 {
			accounted += cg.slab
		}
		if shown >= n || cg.slab < 16<<20 { // hide <16MB entries
			continue
		}
		shown++
		name := cg.path
		if name == "" {
			name = "/"
		}
		if len(name) > 50 {
			name = "..." + name[len(name)-47:]
		}
		var pids []string
		for _, pid := range cg.pids {
			pids = append(pids, fmt.Sprintf("%s(%d)", procComm(pid), pid))
		}
		fmt.Printf("  %-52s %12s %12s  %s\n",
			strings.Repeat("  ", max(cg.depth-1, 0))+name,
			human(float64(cg.slab)), human(float64(cg.unrecl)), strings.Join(pids, " "))
	}
	if totalSlabKB > 0 {
		un := int64(totalSlabKB)*1024 - int64(accounted)
		if un > 0 {
			fmt.Printf("  %s\n", dim(fmt.Sprintf(
				"charged to cgroups: %s of %s total slab · %s unaccounted (kernel-internal, IRQ, !SLAB_ACCOUNT caches)",
				human(float64(accounted)), humanKB(totalSlabKB), human(float64(un)))))
		}
	}
	fmt.Println(dim("  values are hierarchical (children included in parents); charge goes to the"))
	fmt.Println(dim("  allocating cgroup — objects can outlive it (offline memcgs stay charged)"))
	fmt.Println()
}

// printCgroupLimits reports every cgroup that has a memory limit or has
// already OOM-killed. A memcg kill fires with any amount of memory free
// globally — a completely different diagnosis from host exhaustion, and
// invisible in meminfo.
func printCgroupLimits(cgs []*cgSlab) {
	var limited []*cgSlab
	for _, cg := range cgs {
		if cg.limit > 0 || cg.high > 0 || cg.oomKills > 0 {
			limited = append(limited, cg)
		}
	}
	if len(limited) == 0 {
		return
	}
	sort.Slice(limited, func(i, j int) bool {
		if (limited[i].oomKills > 0) != (limited[j].oomKills > 0) {
			return limited[i].oomKills > 0
		}
		return limited[i].current > limited[j].current
	})
	section("CGROUP MEMORY LIMITS & OOM EVENTS")
	fmt.Printf("  %-44s %10s %10s %6s %9s %9s  %s\n",
		"CGROUP", "CURRENT", "MAX", "USE%", "OOM-KILL", "AT-LIMIT", "PIDS")
	for i, cg := range limited {
		if i >= 12 {
			break
		}
		name := cg.path
		if name == "" {
			name = "/"
		}
		if len(name) > 42 {
			name = "..." + name[len(name)-39:]
		}
		limitS, useS := "max", ""
		if cg.limit > 0 {
			limitS = human(float64(cg.limit))
			useS = fmt.Sprintf("%.0f%%", float64(cg.current)/float64(cg.limit)*100)
		}
		var pids []string
		for _, pid := range cg.pids {
			pids = append(pids, fmt.Sprintf("%s(%d)", procComm(pid), pid))
		}
		fmt.Printf("  %-44s %10s %10s %6s %9d %9d  %s\n",
			name, human(float64(cg.current)), limitS, useS,
			cg.oomKills, cg.limitHits, strings.Join(pids, " "))
	}
	fmt.Println(dim("  a cgroup at its memory.max OOM-kills its own processes with the host's memory free"))
	fmt.Println()

	for _, cg := range limited {
		if cg.oomKills > 0 {
			warnf(sevCrit, "these kills are the cgroup limit, not host memory — raise memory.max (systemd MemoryMax) or fix the workload",
				"cgroup %s has OOM-killed %d time(s) (limit %s)",
				cg.path, cg.oomKills, human(float64(cg.limit)))
		}
	}
	for _, cg := range limited {
		if cg.oomKills == 0 && cg.limit > 0 && cg.current >= cg.limit/10*9 {
			warnf(sevWarn, "next allocation over the limit triggers a memcg OOM kill inside this cgroup",
				"cgroup %s is at %.0f%% of its %s memory.max",
				cg.path, float64(cg.current)/float64(cg.limit)*100, human(float64(cg.limit)))
		} else if cg.oomKills == 0 && cg.limitHits > 0 {
			warnf(sevInfo, "allocations paused for reclaim at the limit; latency impact likely",
				"cgroup %s hit its memory limit %d time(s) without kills", cg.path, cg.limitHits)
		}
	}
}
