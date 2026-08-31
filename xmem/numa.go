package main

import (
	"fmt"
	"os"
	"sort"
	"strconv"
	"strings"
)

// nodeMem holds the per-NUMA-node memory breakdown from
// /sys/devices/system/node/nodeN/meminfo. The OOM killer is invoked per
// node/zone: one exhausted node can kill with plenty free elsewhere,
// which global meminfo cannot show.
type nodeMem struct {
	id      int
	totalKB uint64
	freeKB  uint64
	fileKB  uint64
	anonKB  uint64
	slabKB  uint64
}

func readNUMANodes() []nodeMem {
	ents, err := os.ReadDir("/sys/devices/system/node")
	if err != nil {
		return nil
	}
	var out []nodeMem
	for _, e := range ents {
		name := e.Name()
		if !strings.HasPrefix(name, "node") {
			continue
		}
		id, err := strconv.Atoi(name[4:])
		if err != nil {
			continue
		}
		b, err := os.ReadFile("/sys/devices/system/node/" + name + "/meminfo")
		if err != nil {
			continue
		}
		n := nodeMem{id: id}
		for _, line := range strings.Split(string(b), "\n") {
			// "Node 0 MemTotal:  197760284 kB"
			fields := strings.Fields(line)
			if len(fields) < 4 {
				continue
			}
			v, err := strconv.ParseUint(fields[3], 10, 64)
			if err != nil {
				continue
			}
			switch strings.TrimSuffix(fields[2], ":") {
			case "MemTotal":
				n.totalKB = v
			case "MemFree":
				n.freeKB = v
			case "FilePages":
				n.fileKB = v
			case "AnonPages":
				n.anonKB = v
			case "Slab":
				n.slabKB = v
			}
		}
		out = append(out, n)
	}
	sort.Slice(out, func(i, j int) bool { return out[i].id < out[j].id })
	return out
}

func printNUMA() {
	nodes := readNUMANodes()
	if len(nodes) < 2 {
		return // nothing to compare on single-node boxes
	}
	section("PER-NUMA-NODE MEMORY")
	fmt.Printf("  %-6s %12s %12s %6s %12s %12s %12s\n",
		"NODE", "TOTAL", "FREE", "FREE%", "FILE", "ANON", "SLAB")
	minPct, maxPct := 100.0, 0.0
	minNode := 0
	for _, n := range nodes {
		pct := 0.0
		if n.totalKB > 0 {
			pct = float64(n.freeKB) / float64(n.totalKB) * 100
		}
		if pct < minPct {
			minPct, minNode = pct, n.id
		}
		if pct > maxPct {
			maxPct = pct
		}
		fmt.Printf("  %-6d %12s %12s %5.1f%% %12s %12s %12s\n",
			n.id, humanKB(n.totalKB), humanKB(n.freeKB), pct,
			humanKB(n.fileKB), humanKB(n.anonKB), humanKB(n.slabKB))
	}
	fmt.Println(dim("  OOM is decided per node — one exhausted node kills with memory free elsewhere"))
	fmt.Println()

	switch {
	case minPct < 2:
		warnf(sevCrit, "allocations bound to this node (numactl/mempolicy) will OOM regardless of global free memory",
			"NUMA node %d nearly exhausted: %.1f%% free", minNode, minPct)
	case minPct < 5 && maxPct > 20:
		warnf(sevWarn, "check NUMA bindings and mempolicies (a large shared_policy_node slab is the tell) and vm.zone_reclaim_mode",
			"NUMA imbalance: node %d at %.1f%% free while another node has %.1f%% free", minNode, minPct, maxPct)
	}
}
