package main

import (
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
)

// procInfo summarizes one process's memory footprint the way the kernel's
// oom_badness() sees it: RSS + swap entries + page tables, in pages.
type procInfo struct {
	pid     int
	name    string
	rssKB   uint64
	swapKB  uint64
	pteKB   uint64
	vszKB   uint64
	oomAdj  int
	badness uint64 // pages, including oom_score_adj scaling
}

func (p *procInfo) immune() bool { return p.oomAdj <= -1000 }

// totalKB is RAM + swap: the kernel's oom_badness() scales oom_score_adj
// against totalram_pages() + total_swap_pages in the global-OOM case.
func readProcs(totalKB uint64) []*procInfo {
	ents, err := os.ReadDir("/proc")
	if err != nil {
		return nil
	}
	var out []*procInfo
	for _, e := range ents {
		pid, err := strconv.Atoi(e.Name())
		if err != nil {
			continue
		}
		p := readOneProc(pid, totalKB)
		if p != nil {
			out = append(out, p)
		}
	}
	// Kill-order sort: immune processes last, others by badness descending.
	sort.Slice(out, func(i, j int) bool {
		a, b := out[i], out[j]
		if a.immune() != b.immune() {
			return !a.immune()
		}
		return a.badness > b.badness
	})
	return out
}

func readOneProc(pid int, totalKB uint64) *procInfo {
	dir := "/proc/" + strconv.Itoa(pid)
	st, err := os.ReadFile(dir + "/status")
	if err != nil {
		return nil
	}
	p := &procInfo{pid: pid}
	haveRSS := false
	for _, line := range strings.Split(string(st), "\n") {
		fields := strings.Fields(line)
		if len(fields) < 2 {
			continue
		}
		v := func() uint64 { n, _ := strconv.ParseUint(fields[1], 10, 64); return n }
		switch fields[0] {
		case "Name:":
			p.name = safeComm(strings.Join(fields[1:], " ")) // comm may contain spaces (prctl)
		case "VmRSS:":
			p.rssKB = v()
			haveRSS = true
		case "VmSwap:":
			p.swapKB = v()
		case "VmPTE:":
			p.pteKB = v()
		case "VmSize:":
			p.vszKB = v()
		}
	}
	if !haveRSS {
		return nil // kernel thread
	}
	if b, err := os.ReadFile(dir + "/oom_score_adj"); err == nil {
		p.oomAdj, _ = strconv.Atoi(strings.TrimSpace(string(b)))
	}
	// Mirror oom_badness(): pages of RSS+swap+pagetables, then
	// oom_score_adj shifts the score by adj/1000 of RAM+swap; eligible
	// tasks never score below 1.
	pageKB := uint64(os.Getpagesize()) / 1024
	points := int64((p.rssKB + p.swapKB + p.pteKB) / pageKB)
	points += int64(p.oomAdj) * (int64(totalKB/pageKB) / 1000) // kernel truncates totalpages/1000 first
	if points < 1 {
		points = 1
	}
	p.badness = uint64(points)
	return p
}

// readSmapsRollup returns PSS and locked KB for one process. PSS divides
// shared pages among their mappers, so it sums to real usage where RSS
// double-counts; Locked is memory reclaim can never touch.
func readSmapsRollup(pid int) (pssKB, lockedKB uint64, ok bool) {
	b, err := os.ReadFile("/proc/" + strconv.Itoa(pid) + "/smaps_rollup")
	if err != nil {
		return 0, 0, false
	}
	for _, line := range strings.Split(string(b), "\n") {
		fields := strings.Fields(line)
		if len(fields) < 2 {
			continue
		}
		v, err := strconv.ParseUint(fields[1], 10, 64)
		if err != nil {
			continue
		}
		switch fields[0] {
		case "Pss:":
			pssKB = v
		case "Locked:":
			lockedKB = v
		}
	}
	return pssKB, lockedKB, true
}

// countSpecialFDs counts open BPF program/map/link and perf-event fds —
// the userspace handles behind kprobes, tracepoints, and BPF trampolines.
// Returns -1 if the fd directory is unreadable.
func countSpecialFDs(fdDir string) int {
	ents, err := os.ReadDir(fdDir)
	if err != nil {
		return -1
	}
	n := 0
	for i, e := range ents {
		if i >= 50000 {
			break
		}
		l, err := os.Readlink(filepath.Join(fdDir, e.Name()))
		if err != nil {
			continue
		}
		if strings.HasPrefix(l, "anon_inode:") &&
			(strings.Contains(l, "bpf") || strings.Contains(l, "perf_event")) {
			n++
		}
	}
	return n
}

func printProcs(procs []*procInfo, n int) {
	section(fmt.Sprintf("PROCESS ALLOCATIONS (oom_badness kill order, top %d)", n))
	fmt.Printf("  %3s %-22s %8s %10s %10s %9s %9s %8s %6s %10s %8s\n",
		"#", "NAME", "PID", "RSS", "PSS", "LOCKED", "SWAP", "PGTBL", "ADJ", "BADNESS", "BPF/PERF")
	var totalRSS, totalSwap uint64
	shown := 0
	for i, p := range procs {
		totalRSS += p.rssKB
		totalSwap += p.swapKB
		if shown >= n {
			continue
		}
		shown++
		badness := strconv.FormatUint(p.badness, 10)
		if p.immune() {
			badness = "immune"
		}
		// Both of the following readlink/walk the whole of a per-process
		// directory — do them only for displayed rows.
		bpf := ""
		switch n := countSpecialFDs("/proc/" + strconv.Itoa(p.pid) + "/fd"); {
		case n > 0:
			bpf = strconv.Itoa(n)
		case n < 0:
			bpf = "?"
		}
		pss, locked := "-", "-"
		if pssKB, lockedKB, ok := readSmapsRollup(p.pid); ok {
			pss, locked = humanKB(pssKB), humanKB(lockedKB)
		}
		fmt.Printf("  %3d %-22s %8d %10s %10s %9s %9s %8s %6d %10s %8s\n",
			i+1, p.name, p.pid, humanKB(p.rssKB), pss, locked, humanKB(p.swapKB),
			humanKB(p.pteKB), p.oomAdj, badness, bpf)
	}
	fmt.Printf("  TOTAL userspace across %d processes: RSS %s, swap %s\n",
		len(procs), humanKB(totalRSS), humanKB(totalSwap))
	fmt.Println(dim("  PSS splits shared pages between mappers (RSS double-counts them); LOCKED is unreclaimable"))
	fmt.Println(dim("  kernel slab is charged to no process — if slab dominates, rank #1 is not the real consumer"))
	fmt.Println()
}
