package main

import (
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
)

// shrinkerDebugfs is where the kernel exposes per-shrinker count/scan
// files (CONFIG_SHRINKER_DEBUG; readable by root only).
const shrinkerDebugfs = "/sys/kernel/debug/shrinker"

// readShrinkerCounts reports, per shrinker directory whose name starts
// with prefix (e.g. "xfs-buf:" — one directory per block device), how
// many objects that shrinker's count_objects() callback says are
// reclaimable right now. Reading a count file invokes the callback but
// modifies nothing. Each line is "<memcg inode> <count per NUMA node>...";
// counts are summed across memcgs and nodes. A non-nil error means the
// debugfs directory itself was unreadable (not mounted, or not root) —
// distinct from an empty map, which means no shrinker matched.
func readShrinkerCounts(prefix string) (map[string]uint64, error) {
	entries, err := os.ReadDir(shrinkerDebugfs)
	if err != nil {
		return nil, err
	}
	out := map[string]uint64{}
	for _, e := range entries {
		if !strings.HasPrefix(e.Name(), prefix) {
			continue
		}
		data, err := os.ReadFile(filepath.Join(shrinkerDebugfs, e.Name(), "count"))
		if err != nil {
			continue
		}
		var total uint64
		for _, line := range strings.Split(string(data), "\n") {
			fields := strings.Fields(line)
			if len(fields) > 1 {
				fields = fields[1:] // first column is the memcg inode
			}
			for _, f := range fields {
				if n, err := strconv.ParseUint(f, 10, 64); err == nil {
					total += n
				}
			}
		}
		out[e.Name()] = total
	}
	return out, nil
}

// shrinkerGroupName collapses a per-instance debugfs directory name to its
// shrinker type: "xfs-buf:sdb1-42" -> "xfs-buf", "sb-xfs-25" -> "sb-xfs".
// The instance part is ":<device>" and/or the trailing "-<shrinker id>".
func shrinkerGroupName(dir string) string {
	if i := strings.IndexByte(dir, ':'); i >= 0 {
		return dir[:i]
	}
	if i := strings.LastIndexByte(dir, '-'); i > 0 {
		if _, err := strconv.Atoi(dir[i+1:]); err == nil {
			return dir[:i]
		}
	}
	return dir
}

// shrinkerGroup aggregates the instances of one shrinker type across
// devices/superblocks.
type shrinkerGroup struct {
	name      string
	instances int
	objects   uint64
}

// printShrinkers lists every shrinker type by how many objects it reports
// reclaimable. This is the union of all "shrinker-visible but
// meminfo-invisible" caches: xfs-buf metadata buffers, per-superblock
// dentry/inode LRUs (sb-*), nfs, zfs, GPU driver caches.
func printShrinkers(totalRAMKB uint64) {
	section("SHRINKERS (reclaimable objects per cache type, debugfs)")
	counts, err := readShrinkerCounts("")
	if err != nil {
		fmt.Println(dim("  debugfs unreadable (need root) — check manually:"))
		fmt.Println(dim("    ls /sys/kernel/debug/shrinker/ | grep xfs-buf     # one per drive"))
		fmt.Println(dim("    cat /sys/kernel/debug/shrinker/xfs-buf:*/count    # reclaimable buffers per drive"))
		fmt.Println()
		return
	}
	agg := map[string]*shrinkerGroup{}
	for name, n := range counts {
		key := shrinkerGroupName(name)
		g := agg[key]
		if g == nil {
			g = &shrinkerGroup{name: key}
			agg[key] = g
		}
		g.instances++
		g.objects += n
	}
	var groups []shrinkerGroup
	for _, g := range agg {
		if g.objects > 0 {
			groups = append(groups, *g)
		}
	}
	if len(groups) == 0 {
		fmt.Printf("  %s all shrinkers report zero reclaimable objects\n\n", tOK())
		return
	}
	sort.Slice(groups, func(i, j int) bool { return groups[i].objects > groups[j].objects })
	fmt.Printf("  %-28s %16s %10s\n", "SHRINKER", "OBJECTS", "INSTANCES")
	for i, g := range groups {
		if i >= 12 {
			break
		}
		fmt.Printf("  %-28s %16d %10d\n", g.name, g.objects, g.instances)
	}
	fmt.Println(dim("  object sizes vary per shrinker; sb-* count dentries+inodes on superblock LRUs"))
	fmt.Println(dim("  none of this memory is in MemAvailable — it frees only via shrinker reclaim"))
	fmt.Println()

	// xfs-buf deserves a verdict: each object is a metadata buffer whose
	// 4-16KB of data pages appear in no meminfo counter.
	if g, ok := agg["xfs-buf"]; ok && g.objects > 0 && totalRAMKB > 0 {
		lowKB, highKB := g.objects*4, g.objects*16
		pct := float64(lowKB) / float64(totalRAMKB) * 100
		sev := -1
		switch {
		case pct >= 50:
			sev = sevCrit
		case pct >= 25:
			sev = sevWarn
		case highKB >= totalRAMKB/10:
			sev = sevInfo
		}
		if sev >= 0 {
			warnf(sev, "reclaimable cache, but invisible to free/MemAvailable; verify reclaim works: sync; echo 2 > /proc/sys/vm/drop_caches",
				"%d xfs-buf metadata buffers on %d drives hold ~%s-%s that no meminfo counter shows",
				g.objects, g.instances, humanKB(lowKB), humanKB(highKB))
		}
	}
}
