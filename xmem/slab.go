package main

import (
	"bufio"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
)

const slabSysfsRoot = "/sys/kernel/slab"

// slabCache holds one /proc/slabinfo row. With SLUB cache merging, one row
// can represent several logical caches of the same size class; the name is
// merely that of the first-registered cache.
type slabCache struct {
	name         string
	activeObjs   uint64
	numObjs      uint64
	objSize      uint64 // s->size in bytes; equals bytes_alloc in kmem tracepoints
	pagesPerSlab uint64
	numSlabs     uint64
	totalBytes   uint64
	reclaim      byte // 'R' reclaimable, 'U' unreclaimable, '?' unknown
}

func readSlabinfo() ([]*slabCache, error) {
	f, err := os.Open("/proc/slabinfo")
	if err != nil {
		return nil, err
	}
	defer f.Close()
	pageSize := uint64(os.Getpagesize())
	var out []*slabCache
	sc := bufio.NewScanner(f)
	for sc.Scan() {
		line := sc.Text()
		if strings.HasPrefix(line, "slabinfo") || strings.HasPrefix(line, "#") {
			continue
		}
		fields := strings.Fields(line)
		// name active num objsize objperslab pagesperslab : tunables l b s : slabdata active_slabs num_slabs shared
		if len(fields) < 16 {
			continue
		}
		u := func(i int) uint64 { n, _ := strconv.ParseUint(fields[i], 10, 64); return n }
		c := &slabCache{
			name:         fields[0],
			activeObjs:   u(1),
			numObjs:      u(2),
			objSize:      u(3),
			pagesPerSlab: u(5),
			numSlabs:     u(14),
			reclaim:      '?',
		}
		c.totalBytes = c.numSlabs * c.pagesPerSlab * pageSize
		if b, err := os.ReadFile(filepath.Join(slabSysfsRoot, c.name, "reclaim_account")); err == nil {
			switch strings.TrimSpace(string(b)) {
			case "1":
				c.reclaim = 'R'
			case "0":
				c.reclaim = 'U'
			}
		}
		out = append(out, c)
	}
	return out, sc.Err()
}

// slabSysfs maps SLUB cache names to their merge groups. Merged caches
// appear in /sys/kernel/slab as symlinks to a shared ":0000NNN" directory.
type slabSysfs struct {
	target map[string]string   // cache name -> canonical group directory
	group  map[string][]string // group directory -> member cache names
}

func loadSlabSysfs() *slabSysfs {
	s := &slabSysfs{target: map[string]string{}, group: map[string][]string{}}
	ents, err := os.ReadDir(slabSysfsRoot)
	if err != nil {
		return s
	}
	for _, e := range ents {
		name := e.Name()
		tgt := name
		if e.Type()&fs.ModeSymlink != 0 {
			if l, err := os.Readlink(filepath.Join(slabSysfsRoot, name)); err == nil {
				tgt = filepath.Base(l)
			}
		}
		s.target[name] = tgt
		if !strings.HasPrefix(name, ":") { // group dirs themselves are not caches
			s.group[tgt] = append(s.group[tgt], name)
		}
	}
	for _, members := range s.group {
		sort.Strings(members)
	}
	return s
}

// members returns every cache name sharing storage with name (including
// itself). len > 1 means the slabinfo row is a merged size class and its
// name cannot be trusted for attribution.
func (s *slabSysfs) members(name string) []string {
	tgt, ok := s.target[name]
	if !ok {
		return []string{name}
	}
	if m := s.group[tgt]; len(m) > 0 {
		return m
	}
	return []string{name}
}

func printSlabTop(slabs []*slabCache, sysfs *slabSysfs, n int) {
	section(fmt.Sprintf("SLAB CACHES (top %d by size)", n))
	sorted := make([]*slabCache, len(slabs))
	copy(sorted, slabs)
	sort.Slice(sorted, func(i, j int) bool { return sorted[i].totalBytes > sorted[j].totalBytes })
	fmt.Printf("  %-26s %12s %6s %14s %8s %3s  %s\n",
		"NAME", "SIZE", "USE%", "OBJECTS", "OBJSIZE", "R/U", "MERGED-WITH")
	var totalR, totalU uint64
	for _, c := range slabs {
		switch c.reclaim {
		case 'R':
			totalR += c.totalBytes
		case 'U':
			totalU += c.totalBytes
		}
	}
	for i, c := range sorted {
		if i >= n {
			break
		}
		use := 0.0
		if c.numObjs > 0 {
			use = float64(c.activeObjs) / float64(c.numObjs) * 100
		}
		merged := ""
		if m := sysfs.members(c.name); len(m) > 1 {
			var others []string
			for _, x := range m {
				if x != c.name {
					others = append(others, x)
				}
			}
			merged = strings.Join(others, ",")
			if len(merged) > 60 {
				merged = merged[:57] + "..."
			}
		}
		fmt.Printf("  %-26s %12s %5.1f%% %14d %7dB   %c  %s\n",
			c.name, human(float64(c.totalBytes)), use, c.numObjs, c.objSize, c.reclaim, merged)
	}
	fmt.Println(dim(fmt.Sprintf("  by flag: reclaimable %s · unreclaimable %s",
		human(float64(totalR)), human(float64(totalU)))))
	fmt.Println()
}

// detectLockstep finds groups of large caches whose object counts match
// within tolerance — the signature of companion structures allocated
// one-per-object of a parent cache (e.g. per-inode LSM blobs and attr
// forks tracking xfs_inode).
func detectLockstep(slabs []*slabCache) [][]*slabCache {
	const minObjs = 1_000_000
	const tolerance = 0.03
	var big []*slabCache
	for _, c := range slabs {
		if c.numObjs >= minObjs {
			big = append(big, c)
		}
	}
	sort.Slice(big, func(i, j int) bool { return big[i].numObjs > big[j].numObjs })
	var groups [][]*slabCache
	used := make([]bool, len(big))
	for i := range big {
		if used[i] {
			continue
		}
		group := []*slabCache{big[i]}
		used[i] = true
		lead := float64(big[i].numObjs)
		for j := i + 1; j < len(big); j++ {
			if used[j] {
				continue
			}
			if (lead-float64(big[j].numObjs))/lead <= tolerance {
				group = append(group, big[j])
				used[j] = true
			}
		}
		if len(group) >= 2 {
			groups = append(groups, group)
		}
	}
	return groups
}

func printLockstep(groups [][]*slabCache) {
	if len(groups) == 0 {
		return
	}
	section("LOCKSTEP OBJECT COUNTS")
	for _, g := range groups {
		var names []string
		for _, c := range g {
			names = append(names, fmt.Sprintf("%s(%d)", c.name, c.numObjs))
		}
		fmt.Printf("  ~%d objects each: %s\n", g[0].numObjs, strings.Join(names, ", "))
	}
	fmt.Println(dim("  counts within 3% ⇒ one of each per parent object; freed together"))
	fmt.Println()
}

// pickCulprits returns the n largest caches by total size.
func pickCulprits(slabs []*slabCache, n int) []*slabCache {
	sorted := make([]*slabCache, len(slabs))
	copy(sorted, slabs)
	sort.Slice(sorted, func(i, j int) bool { return sorted[i].totalBytes > sorted[j].totalBytes })
	if len(sorted) > n {
		sorted = sorted[:n]
	}
	return sorted
}

func printCulprits(culprits []*slabCache, sysfs *slabSysfs) {
	section(fmt.Sprintf("TOP %d CULPRIT ANALYSIS", len(culprits)))
	for _, c := range culprits {
		fmt.Printf("  %s %s  %s in %d objs of %dB [%c]\n",
			cyan("##"), bold(c.name), human(float64(c.totalBytes)), c.numObjs, c.objSize, c.reclaim)
		members := sysfs.members(c.name)
		if len(members) > 1 {
			fmt.Printf("     %s merged size class — name unreliable; owner is one of: %s\n",
				tWarn(), strings.Join(members, ", "))
		} else {
			fmt.Printf("     %s not merged — struct = %s\n", tOK(), c.name)
		}
		printAllocCalls(c.name)
	}
	fmt.Println()
}

// printAllocCalls dumps historical allocation sites for a cache. Only
// populated when the kernel was booted with slub_debug=U; this is the only
// source that attributes memory allocated before this program started.
func printAllocCalls(cache string) {
	b, err := os.ReadFile(filepath.Join(slabSysfsRoot, cache, "alloc_calls"))
	if err != nil || len(strings.TrimSpace(string(b))) == 0 {
		fmt.Println(dim("     alloc_calls: n/a — boot with slub_debug=U for historical attribution"))
		return
	}
	type site struct {
		count uint64
		line  string
	}
	var sites []site
	for _, line := range strings.Split(string(b), "\n") {
		fields := strings.Fields(line)
		if len(fields) < 2 {
			continue
		}
		n, err := strconv.ParseUint(fields[0], 10, 64)
		if err != nil {
			continue
		}
		l := strings.Join(fields[1:], " ")
		if len(l) > 90 {
			l = l[:90]
		}
		sites = append(sites, site{n, l})
	}
	sort.Slice(sites, func(i, j int) bool { return sites[i].count > sites[j].count })
	fmt.Println(green("     alloc_calls (exact historical attribution):"))
	for i, s := range sites {
		if i >= 8 {
			break
		}
		fmt.Printf("       %10d  %s\n", s.count, s.line)
	}
}

// diffSlabs returns per-cache growth in bytes and objects between two
// slabinfo snapshots.
type slabGrowth struct {
	deltaBytes int64
	deltaObjs  int64
}

func diffSlabs(before, after []*slabCache) map[string]slabGrowth {
	b := map[string]*slabCache{}
	for _, c := range before {
		b[c.name] = c
	}
	out := map[string]slabGrowth{}
	for _, c := range after {
		if prev, ok := b[c.name]; ok {
			out[c.name] = slabGrowth{
				deltaBytes: int64(c.totalBytes) - int64(prev.totalBytes),
				deltaObjs:  int64(c.numObjs) - int64(prev.numObjs),
			}
		}
	}
	return out
}
