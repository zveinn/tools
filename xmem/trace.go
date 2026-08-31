package main

import (
	"bufio"
	"errors"
	"fmt"
	"os"
	"regexp"
	"sort"
	"strconv"
	"strings"
	"time"
)

// traceResult aggregates live kmem tracepoint samples: for each slab size
// class, which kernel functions allocated objects and from which process
// context. The call site is the exact answer to "which struct is this
// really" — e.g. security_inode_alloc => lsm_inode_cache even when the
// size class is reported under a misleading merged name.
type traceResult struct {
	perSize map[uint64]map[string]*siteStat
	lines   uint64
	window  time.Duration
	notes   []string
}

type siteStat struct {
	count uint64
	comms map[string]uint64
}

func tracefsDir() string {
	for _, d := range []string{"/sys/kernel/tracing", "/sys/kernel/debug/tracing"} {
		if _, err := os.Stat(d + "/trace_pipe"); err == nil {
			return d
		}
	}
	return ""
}

// traceAllocations enables kmem allocation tracepoints filtered to the
// culprit size classes, samples trace_pipe for the window, and aggregates
// by call site. All modified tracefs files are registered with rev before
// being touched, so they are restored even if the program crashes mid-run.
func traceAllocations(rev *reverter, sizes []uint64, window time.Duration) (*traceResult, error) {
	tr := tracefsDir()
	if tr == "" {
		return nil, errors.New("tracefs not accessible (need root; is debugfs/tracefs mounted?)")
	}
	res := &traceResult{perSize: map[uint64]map[string]*siteStat{}, window: window}

	var parts []string
	sizeSet := map[uint64]bool{}
	for _, s := range sizes {
		if !sizeSet[s] {
			sizeSet[s] = true
			parts = append(parts, fmt.Sprintf("bytes_alloc == %d", s))
		}
	}
	filter := strings.Join(parts, " || ")

	// kmalloc_node/kmem_cache_alloc_node exist on <= 6.0 kernels only.
	events := []string{"kmem/kmalloc", "kmem/kmalloc_node", "kmem/kmem_cache_alloc", "kmem/kmem_cache_alloc_node"}
	enabled := 0
	for _, ev := range events {
		base := tr + "/events/" + ev
		if _, err := os.Stat(base); err != nil {
			continue
		}
		// Save original state BEFORE any modification; abort this event on
		// failure so nothing unrecorded is ever changed.
		if err := rev.saveFile(base + "/filter"); err != nil {
			continue
		}
		if err := rev.saveFile(base + "/enable"); err != nil {
			continue
		}
		if err := os.WriteFile(base+"/filter", []byte(filter), 0o600); err != nil {
			res.notes = append(res.notes, "kernel rejected filter on "+ev+"; filtering in userspace")
		}
		if err := os.WriteFile(base+"/enable", []byte("1"), 0o600); err != nil {
			continue
		}
		enabled++
	}
	if enabled == 0 {
		return nil, errors.New("could not enable any kmem tracepoints")
	}

	symtab := loadKallsyms()
	if symtab == nil {
		res.notes = append(res.notes, "kallsyms unavailable/restricted; call sites stay as raw addresses")
	}

	f, err := os.Open(tr + "/trace_pipe")
	if err != nil {
		return nil, fmt.Errorf("open trace_pipe: %w", err)
	}
	defer f.Close()

	linesCh := make(chan string, 8192)
	go func() {
		sc := bufio.NewScanner(f)
		sc.Buffer(make([]byte, 0, 64*1024), 1<<20)
		for sc.Scan() {
			linesCh <- sc.Text()
		}
		close(linesCh)
	}()

	timeout := time.After(window)
loop:
	for {
		select {
		case l, ok := <-linesCh:
			if !ok {
				break loop
			}
			res.parseLine(l, sizeSet, symtab)
		case <-timeout:
			break loop
		}
	}
	// Caller runs rev.restoreAll() immediately after to disable the events;
	// the abandoned reader goroutine dies with the process.
	return res, nil
}

var (
	reComm  = regexp.MustCompile(`^\s*(.{1,16}?)-(\d+)\s+\[`)
	reSite  = regexp.MustCompile(`call_site=(\S+)`)
	reBytes = regexp.MustCompile(`bytes_alloc=(\d+)`)
	reHex   = regexp.MustCompile(`^[0-9a-fA-F]{8,16}$`)
)

// allocBytes reports the allocation size recorded on a kmem tracepoint line,
// and whether the line is an allocation event at all.
func allocBytes(line string) (uint64, bool) {
	m := reBytes.FindStringSubmatch(line)
	if m == nil {
		return 0, false
	}
	b, err := strconv.ParseUint(m[1], 10, 64)
	return b, err == nil
}

// decodeAlloc resolves the call site and owning task of a kmem tracepoint
// line, falling back to "?" for either. Both the batch and live aggregators
// decode through here so the two cannot drift as the trace format evolves.
// It is kept separate from allocBytes because resolveTask reads /proc: the
// batch path must filter by size class before paying for it.
func decodeAlloc(line string, st *symtab) (site, comm string) {
	site, comm = "?", "?"
	if sm := reSite.FindStringSubmatch(line); sm != nil {
		site = sm[1]
		if reHex.MatchString(site) {
			if st != nil {
				if a, err := strconv.ParseUint(site, 16, 64); err == nil {
					if s := st.resolve(a); s != "" {
						site = s
					}
				}
			}
		} else if i := strings.IndexByte(site, '+'); i > 0 {
			site = site[:i] // "%pS" form: func+0x1f/0x40
		}
	}
	if cm := reComm.FindStringSubmatch(line); cm != nil {
		comm = resolveTask(safeComm(strings.TrimSpace(cm[1])), cm[2])
	}
	return site, comm
}

func (res *traceResult) parseLine(line string, sizes map[uint64]bool, st *symtab) {
	b, ok := allocBytes(line)
	if !ok {
		return
	}
	res.lines++
	if !sizes[b] {
		return
	}
	site, comm := decodeAlloc(line, st)
	bySite := res.perSize[b]
	if bySite == nil {
		bySite = map[string]*siteStat{}
		res.perSize[b] = bySite
	}
	ss := bySite[site]
	if ss == nil {
		ss = &siteStat{comms: map[string]uint64{}}
		bySite[site] = ss
	}
	ss.count++
	ss.comms[comm]++
}

// taskCache memoizes tid -> "comm:pid". The pid in trace output is the
// thread id; the owning process pid (tgid) comes from /proc/<tid>/status.
var taskCache = map[string]string{}

func resolveTask(comm, tid string) string {
	// A cached entry whose comm no longer matches means the tid was
	// recycled by another process (or renamed via prctl) — re-resolve.
	if v, ok := taskCache[tid]; ok && (v == comm || strings.HasPrefix(v, comm+":")) {
		return v
	}
	if len(taskCache) > 1<<16 {
		taskCache = map[string]string{} // tid churn in long live sessions
	}
	v := comm
	if b, err := os.ReadFile("/proc/" + tid + "/status"); err == nil {
		for _, line := range strings.Split(string(b), "\n") {
			if pid, ok := strings.CutPrefix(line, "Tgid:"); ok {
				v = comm + ":" + strings.TrimSpace(pid)
				break
			}
		}
	}
	taskCache[tid] = v
	return v
}

func printAttribution(res *traceResult, sizeLabels map[uint64]string) {
	section(fmt.Sprintf("LIVE ALLOCATION ATTRIBUTION (%s sample)", res.window))
	for _, n := range res.notes {
		fmt.Printf("  note: %s\n", n)
	}
	if res.lines == 0 {
		fmt.Println("  no allocation events captured in the window")
		fmt.Println()
		return
	}
	var sizes []uint64
	for s := range res.perSize {
		sizes = append(sizes, s)
	}
	sort.Slice(sizes, func(i, j int) bool { return sizes[i] < sizes[j] })
	secs := res.window.Seconds()
	for _, size := range sizes {
		label := sizeLabels[size]
		fmt.Printf("  %s %s size class%s\n", cyan("##"), bold(fmt.Sprintf("%dB", size)), label)
		type row struct {
			site string
			st   *siteStat
		}
		var rows []row
		for site, st := range res.perSize[size] {
			rows = append(rows, row{site, st})
		}
		sort.Slice(rows, func(i, j int) bool { return rows[i].st.count > rows[j].st.count })
		for i, r := range rows {
			if i >= 10 {
				break
			}
			fmt.Printf("     %10d allocs (%8.0f/s)  %-40s  by: %s\n",
				r.st.count, float64(r.st.count)/secs, r.site, topComms(r.st.comms, 3))
		}
	}
	fmt.Println(dim("  the kernel function IS the exact owner, whatever the merged class is called"))
	fmt.Println()
}

func topComms(comms map[string]uint64, n int) string {
	type kv struct {
		k string
		v uint64
	}
	var s []kv
	for k, v := range comms {
		s = append(s, kv{k, v})
	}
	sort.Slice(s, func(i, j int) bool { return s[i].v > s[j].v })
	var out []string
	for i, e := range s {
		if i >= n {
			out = append(out, "...")
			break
		}
		out = append(out, fmt.Sprintf("%s(%d)", e.k, e.v))
	}
	return strings.Join(out, " ")
}

// symtab resolves kernel text addresses to symbol names via /proc/kallsyms.
type symtab struct {
	addrs []uint64
	names []string
}

func loadKallsyms() *symtab {
	f, err := os.Open("/proc/kallsyms")
	if err != nil {
		return nil
	}
	defer f.Close()
	st := &symtab{}
	sc := bufio.NewScanner(f)
	sc.Buffer(make([]byte, 0, 64*1024), 1<<20)
	for sc.Scan() {
		fields := strings.Fields(sc.Text())
		if len(fields) < 3 {
			continue
		}
		a, err := strconv.ParseUint(fields[0], 16, 64)
		if err != nil || a == 0 { // zeroed under kptr_restrict
			continue
		}
		name := fields[2]
		if len(fields) >= 4 {
			name += " " + fields[3] // [module]
		}
		st.addrs = append(st.addrs, a)
		st.names = append(st.names, name)
	}
	if len(st.addrs) == 0 {
		return nil
	}
	sort.Sort(st)
	return st
}

func (s *symtab) Len() int           { return len(s.addrs) }
func (s *symtab) Less(i, j int) bool { return s.addrs[i] < s.addrs[j] }
func (s *symtab) Swap(i, j int) {
	s.addrs[i], s.addrs[j] = s.addrs[j], s.addrs[i]
	s.names[i], s.names[j] = s.names[j], s.names[i]
}

func (s *symtab) resolve(a uint64) string {
	i := sort.Search(len(s.addrs), func(i int) bool { return s.addrs[i] > a }) - 1
	if i < 0 {
		return ""
	}
	return s.names[i]
}
