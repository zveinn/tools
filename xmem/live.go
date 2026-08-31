package main

import (
	"bufio"
	"fmt"
	"os"
	"os/signal"
	"regexp"
	"sort"
	"strconv"
	"strings"
	"sync"
	"syscall"
	"time"
	"unsafe"
)

// live mode: a 2x2 full-screen grid, refreshed per interval.
//   top-left:     slab cache growth over time (per allocated object type)
//   top-right:    memory & reclaim activity (meminfo deltas, vmstat counters)
//   bottom-left:  hottest allocation call sites (kmem tracepoints)
//   bottom-right: physical fragmentation of zone Normal, per order

type attrAgg struct {
	count uint64
	bytes uint64
	comms map[string]uint64
}

// ptrInfo remembers which call site allocated an object so its free event
// (which carries only the pointer) can be attributed back.
type ptrInfo struct {
	site  string
	bytes uint64
}

type kernelAlert struct {
	when time.Time
	msg  string
}

type liveState struct {
	mu     sync.Mutex
	window map[string]*attrAgg
	events uint64
	lost   uint64
	// alloc/free matching: outstanding pointers and the per-site balance.
	// A site whose net keeps climbing is leaking; one that churns at high
	// rate but stays near zero is just hot.
	ptrs    map[string]ptrInfo
	siteNet map[string]int64 // unfreed bytes per alloc site since start
	alerts  []kernelAlert    // recent memory-related kernel log events
	nAlerts uint64
}

func newLiveState() *liveState {
	return &liveState{
		window:  map[string]*attrAgg{},
		ptrs:    map[string]ptrInfo{},
		siteNet: map[string]int64{},
	}
}

func (ls *liveState) swap() (win map[string]*attrAgg, net map[string]int64,
	alerts []kernelAlert, nAlerts, ev, lost uint64,
) {
	ls.mu.Lock()
	defer ls.mu.Unlock()
	win, ev, lost = ls.window, ls.events, ls.lost
	ls.window = map[string]*attrAgg{}
	ls.events, ls.lost = 0, 0
	net = make(map[string]int64, len(ls.siteNet))
	for k, v := range ls.siteNet {
		if v != 0 {
			net[k] = v
		}
	}
	return win, net, append([]kernelAlert(nil), ls.alerts...), ls.nAlerts, ev, lost
}

// maxTrackedPtrs bounds the pointer table; when a workload outruns it the
// table resets and net totals become approximate rather than the process
// eating the host it is diagnosing.
const maxTrackedPtrs = 1 << 20

func (ls *liveState) recAlloc(site, ptr, comm string, bytes uint64) {
	ls.mu.Lock()
	defer ls.mu.Unlock()
	a := ls.window[site]
	if a == nil {
		a = &attrAgg{comms: map[string]uint64{}}
		ls.window[site] = a
	}
	a.count++
	a.bytes += bytes
	a.comms[comm]++
	ls.events++
	if ptr == "" {
		return
	}
	if len(ls.ptrs) >= maxTrackedPtrs {
		ls.ptrs = map[string]ptrInfo{}
	}
	ls.ptrs[ptr] = ptrInfo{site: site, bytes: bytes}
	ls.siteNet[site] += int64(bytes)
}

func (ls *liveState) recFree(ptr string) {
	ls.mu.Lock()
	defer ls.mu.Unlock()
	ls.events++
	if pi, ok := ls.ptrs[ptr]; ok {
		ls.siteNet[pi.site] -= int64(pi.bytes)
		delete(ls.ptrs, ptr)
	}
}

func (ls *liveState) recAlert(msg string) {
	ls.mu.Lock()
	defer ls.mu.Unlock()
	ls.nAlerts++
	ls.alerts = append(ls.alerts, kernelAlert{when: time.Now(), msg: msg})
	if len(ls.alerts) > 3 {
		ls.alerts = ls.alerts[len(ls.alerts)-3:]
	}
}

// kmsgAlertRe matches kernel log lines that signal memory trouble the
// moment it happens.
var kmsgAlertRe = regexp.MustCompile(
	`invoked oom-killer|Out of memory|Memory cgroup out of memory|page allocation failure|blocked for more than|soft lockup`)

// watchKmsg tails future kernel messages (the backlog is the batch run's
// job) and records memory-related alerts until process exit.
func watchKmsg(ls *liveState) {
	f, err := os.Open("/dev/kmsg")
	if err != nil {
		return
	}
	if _, err := f.Seek(0, 2); err != nil { // SEEK_END: new messages only
		f.Close()
		return
	}
	sc := bufio.NewScanner(f)
	sc.Buffer(make([]byte, 0, 16*1024), 1<<20)
	for sc.Scan() {
		if _, msg, ok := strings.Cut(sc.Text(), ";"); ok && kmsgAlertRe.MatchString(msg) {
			ls.recAlert(msg)
		}
	}
}

// liveSnap is one point-in-time reading of every data source the grid shows.
type liveSnap struct {
	slabs    []*slabCache
	mi       meminfo
	vm       map[string]uint64
	frag     []*zoneFrag
	psiMem   psiStats
	shr      map[string]uint64 // shrinker group -> reclaimable objects
	procRSS  map[int]uint64    // pid -> RSS KB
	procName map[int]string
}

var vmKeys = []string{
	"pgscan_kswapd", "pgscan_direct", "pgsteal_kswapd", "pgsteal_direct",
	"compact_stall", "compact_success", "compact_fail", "oom_kill", "pgmajfault",
	"workingset_refault_file", "workingset_refault_anon",
	"allocstall_dma", "allocstall_dma32", "allocstall_normal", "allocstall_movable", "allocstall_device",
}

func takeSnap() (*liveSnap, error) {
	slabs, err := readSlabinfo()
	if err != nil {
		return nil, err
	}
	mi, _ := readMeminfo()
	s := &liveSnap{
		slabs:  slabs,
		mi:     mi,
		vm:     readVMStat(vmKeys...),
		frag:   readFragmentation(),
		psiMem: readPSI("memory"),
		shr:    readShrinkerGroups(),
	}
	s.procRSS, s.procName = readProcRSSSnap()
	return s, nil
}

// readShrinkerGroups snapshots every debugfs shrinker count, aggregated by
// type ("xfs-buf", "sb-xfs", ...). Falling counts under pressure mean
// reclaim is working; flat counts while pgscan climbs mean it is stalled.
func readShrinkerGroups() map[string]uint64 {
	counts, err := readShrinkerCounts("")
	if err != nil {
		return nil
	}
	out := map[string]uint64{}
	for name, n := range counts {
		out[shrinkerGroupName(name)] += n
	}
	return out
}

// readProcRSSSnap grabs RSS for every process (one statm read each) so the
// grid can show which processes grew during the window.
func readProcRSSSnap() (map[int]uint64, map[int]string) {
	ents, err := os.ReadDir("/proc")
	if err != nil {
		return nil, nil
	}
	pageKB := uint64(os.Getpagesize()) / 1024
	rss := map[int]uint64{}
	names := map[int]string{}
	for _, e := range ents {
		pid, err := strconv.Atoi(e.Name())
		if err != nil {
			continue
		}
		b, err := os.ReadFile("/proc/" + e.Name() + "/statm")
		if err != nil {
			continue
		}
		f := strings.Fields(string(b))
		if len(f) < 2 {
			continue
		}
		pages, err := strconv.ParseUint(f[1], 10, 64)
		if err != nil || pages == 0 {
			continue
		}
		rss[pid] = pages * pageKB
		if c, err := os.ReadFile("/proc/" + e.Name() + "/comm"); err == nil {
			names[pid] = safeComm(strings.TrimSpace(string(c)))
		}
	}
	return rss, names
}

func runLive(interval time.Duration) int {
	if os.Geteuid() != 0 {
		fmt.Fprintln(os.Stderr, "live mode needs root (/proc/slabinfo + tracing)")
		return 1
	}
	base, err := takeSnap()
	if err != nil {
		fmt.Fprintf(os.Stderr, "initial snapshot: %v\n", err)
		return 1
	}
	tr := tracefsDir()
	if tr == "" {
		fmt.Fprintln(os.Stderr, "tracefs not accessible")
		return 1
	}

	rev := newReverter()
	defer rev.restoreAll()
	sig := make(chan os.Signal, 1)
	signal.Notify(sig, os.Interrupt, syscall.SIGTERM, syscall.SIGHUP)

	// Enable the kmem allocation AND free tracepoints unfiltered (clearing
	// any pre-existing filter); originals are saved first and restored on
	// any exit path, including panic and signal. Free events carry the
	// pointer that links them back to their allocation site.
	events := []string{
		"kmem/kmalloc", "kmem/kmalloc_node",
		"kmem/kmem_cache_alloc", "kmem/kmem_cache_alloc_node",
		"kmem/kfree", "kmem/kmem_cache_free",
	}
	enabled := 0
	for _, ev := range events {
		basePath := tr + "/events/" + ev
		if _, err := os.Stat(basePath); err != nil {
			continue
		}
		if rev.saveFile(basePath+"/filter") != nil || rev.saveFile(basePath+"/enable") != nil {
			continue
		}
		_ = os.WriteFile(basePath+"/filter", []byte("0"), 0o600)
		if os.WriteFile(basePath+"/enable", []byte("1"), 0o600) != nil {
			continue
		}
		enabled++
	}
	if enabled == 0 {
		fmt.Fprintln(os.Stderr, "could not enable kmem tracepoints")
		return 1
	}
	// Bigger per-cpu ring buffer: with free events enabled the default
	// fills fast, and LOST events break alloc/free matching.
	if rev.saveFile(tr+"/buffer_size_kb") == nil {
		_ = os.WriteFile(tr+"/buffer_size_kb", []byte("4096"), 0o600)
	}

	pipe, err := os.Open(tr + "/trace_pipe")
	if err != nil {
		fmt.Fprintf(os.Stderr, "open trace_pipe: %v\n", err)
		return 1
	}
	defer pipe.Close()

	ls := newLiveState()
	go liveReader(pipe, ls, loadKallsyms())
	go watchKmsg(ls)

	// Alternate screen + hidden cursor; restored on every exit path.
	fmt.Print("\033[?1049h\033[?25l")
	defer fmt.Print("\033[?25h\033[?1049l")

	start := time.Now()
	prev := base
	renderGrid(start, interval, base, prev, prev, map[string]*attrAgg{}, nil, nil, 0, 0, 0)
	tick := time.NewTicker(interval)
	defer tick.Stop()
	for {
		select {
		case <-sig:
			return 0 // defers restore tracefs and the terminal
		case <-tick.C:
			cur, err := takeSnap()
			if err != nil {
				continue
			}
			w, net, alerts, nAlerts, ev, lost := ls.swap()
			renderGrid(start, interval, base, prev, cur, w, net, alerts, nAlerts, ev, lost)
			prev = cur
		}
	}
}

var rePtr = regexp.MustCompile(`ptr=(\S+)`)

func liveReader(f *os.File, ls *liveState, st *symtab) {
	sc := bufio.NewScanner(f)
	sc.Buffer(make([]byte, 0, 64*1024), 1<<20)
	for sc.Scan() {
		line := sc.Text()
		if strings.Contains(line, "LOST") && strings.Contains(line, "EVENTS") {
			ls.mu.Lock()
			ls.lost += parseLost(line)
			ls.mu.Unlock()
			continue
		}
		// Free events carry no size; the pointer links them back to the
		// site recorded at allocation time.
		if strings.Contains(line, " kfree:") || strings.Contains(line, " kmem_cache_free:") {
			if pm := rePtr.FindStringSubmatch(line); pm != nil {
				ls.recFree(pm[1])
			}
			continue
		}
		b, ok := allocBytes(line)
		if !ok {
			continue
		}
		site, comm := decodeAlloc(line, st)
		ptr := ""
		if pm := rePtr.FindStringSubmatch(line); pm != nil {
			ptr = pm[1]
		}
		ls.recAlloc(site, ptr, comm, b)
	}
}

// parseLost extracts N from "CPU:2 [LOST 12345 EVENTS]".
func parseLost(line string) uint64 {
	fields := strings.Fields(line)
	for i, f := range fields {
		if strings.Contains(f, "LOST") && i+1 < len(fields) {
			if n, err := strconv.ParseUint(fields[i+1], 10, 64); err == nil {
				return n
			}
		}
	}
	return 0
}

// ---- grid rendering ----

func renderGrid(start time.Time, interval time.Duration,
	base, prev, cur *liveSnap, w map[string]*attrAgg, net map[string]int64,
	alerts []kernelAlert, nAlerts, ev, lost uint64,
) {
	rows, cols := termSize()
	halfW := (cols - 3) / 2
	body := rows - 3 // header + alert line + spare
	topH := body / 2
	botH := body - topH

	var b strings.Builder
	b.WriteString("\033[H\033[2J")
	rate := uint64(float64(ev) / interval.Seconds())
	b.WriteString(bold(fmt.Sprintf(" memory-analysis live · %s · up %s · %d ev/s",
		time.Now().Format("15:04:05"), time.Since(start).Round(time.Second), rate)))
	if lost > 0 {
		b.WriteString(yellow(fmt.Sprintf(" · LOST %d", lost)))
	}
	b.WriteString(dim("   Ctrl-C quits, settings auto-restore\n"))
	if nAlerts > 0 && len(alerts) > 0 {
		last := alerts[len(alerts)-1]
		b.WriteString(red(fmt.Sprintf(" ⚠ kernel: %d memory event(s) · %s %s",
			nAlerts, last.when.Format("15:04:05"), clip(last.msg, max(cols-40, 20)))))
		b.WriteString("\n")
	} else {
		b.WriteString(dim(" kernel log: quiet (watching for oom-kills / allocation failures / hung tasks)\n"))
	}

	tl := paneSlab(halfW, topH, base, prev, cur)
	tr := paneVM(halfW, topH, prev, cur)
	bl := paneSites(halfW, botH, w, net, interval, prev, cur)
	br := paneFrag(halfW, botH, prev, cur)

	sep := dim("│")
	for i := 0; i < topH; i++ {
		b.WriteString(padTo(lineAt(tl, i), halfW) + " " + sep + " " + lineAt(tr, i) + "\n")
	}
	for i := 0; i < botH; i++ {
		b.WriteString(padTo(lineAt(bl, i), halfW) + " " + sep + " " + lineAt(br, i) + "\n")
	}
	os.Stdout.WriteString(b.String())
}

func lineAt(lines []string, i int) string {
	if i < len(lines) {
		return lines[i]
	}
	return ""
}

// paneSlab: cache growth, current window and cumulative since start.
func paneSlab(w, h int, base, prev, cur *liveSnap) []string {
	out := []string{cyan(rule("SLAB GROWTH (Δwin / Δtot)", w))}
	out = append(out, dim(fmt.Sprintf(" %-20s %11s %9s %9s", "CACHE", "OBJECTS", "Δwin", "Δtot")))
	type growRow struct {
		c        *slabCache
		dwB, dtB int64
	}
	prevBy := map[string]*slabCache{}
	for _, c := range prev.slabs {
		prevBy[c.name] = c
	}
	baseBy := map[string]*slabCache{}
	for _, c := range base.slabs {
		baseBy[c.name] = c
	}
	var grows []growRow
	for _, c := range cur.slabs {
		g := growRow{c: c}
		if p := prevBy[c.name]; p != nil {
			g.dwB = int64(c.totalBytes) - int64(p.totalBytes)
		}
		if p := baseBy[c.name]; p != nil {
			g.dtB = int64(c.totalBytes) - int64(p.totalBytes)
		}
		grows = append(grows, g)
	}
	sort.Slice(grows, func(i, j int) bool {
		a, z := grows[i], grows[j]
		if a.dtB != z.dtB {
			return a.dtB > z.dtB
		}
		if a.dwB != z.dwB {
			return a.dwB > z.dwB
		}
		return a.c.totalBytes > z.c.totalBytes
	})
	for _, g := range grows {
		if len(out) >= h {
			break
		}
		trend := dim("·")
		switch {
		case g.dwB > 0:
			trend = yellow("↑")
		case g.dwB < 0:
			trend = green("↓")
		}
		out = append(out, fmt.Sprintf(" %-20s %11d %9s %9s %s",
			clip(g.c.name, 20), g.c.numObjs, human(float64(g.dwB)), human(float64(g.dtB)), trend))
	}
	return out
}

// paneVM: memory headroom and the kernel's struggle indicators — direct
// reclaim, compaction stalls, and OOM kills are the leading signals that a
// machine is heading toward allocation failure.
func paneVM(w, h int, prev, cur *liveSnap) []string {
	out := []string{cyan(rule("MEMORY / RECLAIM ACTIVITY", w))}
	dKB := func(k string) string {
		d := int64(cur.mi[k]) - int64(prev.mi[k])
		s := "Δ" + human(float64(d)*1024)
		if d > 0 {
			s = "Δ+" + human(float64(d)*1024)
		}
		return s
	}
	mem := func(label, key string, badGrow bool) string {
		d := int64(cur.mi[key]) - int64(prev.mi[key])
		line := fmt.Sprintf(" %-11s %10s  %s", label, humanKB(cur.mi[key]), dKB(key))
		if badGrow && d > 0 || !badGrow && d < 0 {
			return yellow(line)
		}
		return line
	}
	out = append(out,
		mem("MemFree", "MemFree", false),
		mem("MemAvail", "MemAvailable", false),
		mem("SReclaim", "SReclaimable", true),
		mem("SUnreclaim", "SUnreclaim", true),
		fmt.Sprintf(" %-11s %10s  Writeback %s", "Dirty", humanKB(cur.mi["Dirty"]), humanKB(cur.mi["Writeback"])),
	)
	// Counter resets (or a key missing from one snapshot) must not
	// underflow into absurd deltas.
	dv := func(k string) uint64 {
		c, p := cur.vm[k], prev.vm[k]
		if c < p {
			return 0
		}
		return c - p
	}
	stalls := dv("allocstall_dma") + dv("allocstall_dma32") + dv("allocstall_normal") +
		dv("allocstall_movable") + dv("allocstall_device")
	scan := fmt.Sprintf(" pgscan      kswapd +%d · direct +%d · stall +%d",
		dv("pgscan_kswapd"), dv("pgscan_direct"), stalls)
	if dv("pgscan_direct") > 0 || stalls > 0 {
		scan = yellow(scan) // direct reclaim = allocations are stalling
	}
	// Reclaim efficiency: pages freed per page scanned. Low efficiency
	// means the kernel is scanning frantically and finding nothing — the
	// signature of thrash right before OOM.
	steal := fmt.Sprintf(" pgsteal     kswapd +%d · direct +%d", dv("pgsteal_kswapd"), dv("pgsteal_direct"))
	if scanTot := dv("pgscan_kswapd") + dv("pgscan_direct"); scanTot > 0 {
		eff := float64(dv("pgsteal_kswapd")+dv("pgsteal_direct")) / float64(scanTot) * 100
		effS := fmt.Sprintf(" · eff %.0f%%", eff)
		switch {
		case eff < 20:
			effS = red(effS)
		case eff < 50:
			effS = yellow(effS)
		}
		steal += effS
	}
	out = append(out, scan, steal)
	// Refaults = pages reclaimed and needed again — reclaim is eating the
	// working set, not cold cache.
	refault := fmt.Sprintf(" refault     file +%d · anon +%d",
		dv("workingset_refault_file"), dv("workingset_refault_anon"))
	if dv("workingset_refault_file")+dv("workingset_refault_anon") > 10000 {
		refault = yellow(refault)
	}
	out = append(out, refault)
	comp := fmt.Sprintf(" compaction  stall +%d · ok +%d · fail +%d",
		dv("compact_stall"), dv("compact_success"), dv("compact_fail"))
	// Failure rate = "will fragmentation self-heal": mostly-failing
	// compaction means the blocks are pinned by Unmovable pages.
	if att := dv("compact_success") + dv("compact_fail"); att > 0 {
		rate := float64(dv("compact_fail")) / float64(att) * 100
		comp += fmt.Sprintf(" (%.0f%% failing)", rate)
		if rate >= 50 {
			comp = red(comp)
		} else if dv("compact_fail") > 0 {
			comp = yellow(comp)
		}
	}
	out = append(out, comp)
	oom := fmt.Sprintf(" oom_kill    +%d (total %d)", dv("oom_kill"), cur.vm["oom_kill"])
	if dv("oom_kill") > 0 {
		oom = red(oom)
	}
	out = append(out, oom,
		fmt.Sprintf(" majfault    +%d", dv("pgmajfault")))
	if cur.psiMem.ok {
		p := cur.psiMem
		psiLine := fmt.Sprintf(" psi mem     some %.2f%% · full %.2f%% (10s)", p.some.avg10, p.full.avg10)
		switch {
		case p.full.avg10 >= 10:
			psiLine = red(psiLine)
		case p.full.avg10 >= 1 || p.some.avg10 >= 10:
			psiLine = yellow(psiLine)
		}
		out = append(out, psiLine)
	}
	// Shrinker-held caches (invisible to MemAvailable): falling under
	// pressure = reclaim working; flat while pgscan climbs = stalled.
	if len(cur.shr) > 0 {
		type kv struct {
			name string
			n    uint64
		}
		var s []kv
		for k, v := range cur.shr {
			if v > 0 {
				s = append(s, kv{k, v})
			}
		}
		sort.Slice(s, func(i, j int) bool { return s[i].n > s[j].n })
		for i, e := range s {
			if i >= 3 || len(out) >= h {
				break
			}
			var d int64
			if p, ok := prev.shr[e.name]; ok {
				d = int64(e.n) - int64(p)
			}
			trend := dim("·")
			switch {
			case d > 0:
				trend = yellow("↑")
			case d < 0:
				trend = green("↓")
			}
			out = append(out, fmt.Sprintf(" shrinker    %-12s %10s  %+d %s",
				clip(e.name, 12), humanCount(e.n), d, trend))
		}
	}
	if len(out) > h {
		out = out[:h]
	}
	return out
}

// humanCount renders large object counts compactly (25.4M).
func humanCount(n uint64) string {
	switch {
	case n >= 1e9:
		return fmt.Sprintf("%.1fG", float64(n)/1e9)
	case n >= 1e6:
		return fmt.Sprintf("%.1fM", float64(n)/1e6)
	case n >= 1e3:
		return fmt.Sprintf("%.1fK", float64(n)/1e3)
	}
	return strconv.FormatUint(n, 10)
}

// paneSites: hottest allocation call sites in the last window, with the
// net unfreed bytes per site since start. High rate + net near zero is a
// hot path; climbing net is a leak candidate.
func paneSites(w, h int, win map[string]*attrAgg, net map[string]int64,
	interval time.Duration, prev, cur *liveSnap,
) []string {
	out := []string{cyan(rule(fmt.Sprintf("ALLOCATION SITES (last %s · NET = unfreed since start)", interval), w))}
	out = append(out, dim(fmt.Sprintf(" %7s %8s %9s  %-26s %s", "RATE/s", "BYTES", "NET", "CALL SITE", "BY")))
	type siteRow struct {
		site string
		a    *attrAgg
	}
	var sites []siteRow
	for s, a := range win {
		sites = append(sites, siteRow{s, a})
	}
	sort.Slice(sites, func(i, j int) bool { return sites[i].a.count > sites[j].a.count })
	if len(sites) == 0 {
		out = append(out, dim(" collecting..."))
	}
	bodyH := h - 2 // reserve the bottom for rss movers
	for _, s := range sites {
		if len(out) >= bodyH {
			break
		}
		n := net[s.site]
		netS := fmt.Sprintf("%9s", human(float64(n)))
		switch {
		case n > 100<<20:
			netS = red(netS)
		case n > 10<<20:
			netS = yellow(netS)
		case n < 0:
			netS = dim(netS) // freed objects allocated before we started
		}
		out = append(out, fmt.Sprintf(" %7.0f %8s %s  %-26s %s",
			float64(s.a.count)/interval.Seconds(), human(float64(s.a.bytes)), netS,
			clip(s.site, 26), clip(topComms(s.a.comms, 1), max(w-56, 8))))
	}
	if movers := topRSSMovers(prev, cur, 3); movers != "" && h >= 4 {
		out = append(out, dim(" rss movers:"), " "+clip(movers, w-2))
	}
	return out
}

// topRSSMovers names the processes whose RSS grew most during the window.
func topRSSMovers(prev, cur *liveSnap, n int) string {
	if prev == nil || cur == nil || len(cur.procRSS) == 0 || len(prev.procRSS) == 0 {
		return ""
	}
	type mover struct {
		pid   int
		delta int64
	}
	var m []mover
	for pid, rss := range cur.procRSS {
		p, ok := prev.procRSS[pid]
		if !ok {
			continue // new process; its whole RSS is not window growth
		}
		if d := int64(rss) - int64(p); d > 0 {
			m = append(m, mover{pid, d})
		}
	}
	if len(m) == 0 {
		return ""
	}
	sort.Slice(m, func(i, j int) bool { return m[i].delta > m[j].delta })
	var parts []string
	for i, mv := range m {
		if i >= n {
			break
		}
		name := cur.procName[mv.pid]
		if name == "" {
			name = "?"
		}
		parts = append(parts, fmt.Sprintf("%s:%d +%s", name, mv.pid, human(float64(mv.delta)*1024)))
	}
	return strings.Join(parts, " · ")
}

// paneFrag: per-order free-block availability for zone Normal — the exact
// signal that precedes "OOM with free memory" (order-N allocation failure).
func paneFrag(w, h int, prev, cur *liveSnap) []string {
	out := []string{cyan(rule("FRAGMENTATION zone Normal (usable blocks/order)", w))}
	zc := findZone(cur.frag, "Normal")
	zp := findZone(prev.frag, "Normal")
	if zc == nil {
		return append(out, dim(" zone Normal not found"))
	}
	pageKB := uint64(os.Getpagesize()) / 1024
	order, exact := zc.maxUsableOrder()
	status := fmt.Sprintf(" max usable order: %d (%s)", order, humanKB(pageKB<<max(order, 0)))
	switch {
	case order < 0:
		status = red(" no usable free blocks  ← all allocations failing")
	case order < 3:
		status = red(status + "  ← 32KB allocs failing")
	case order < 5:
		status = yellow(status)
	default:
		status = green(status)
	}
	if !exact {
		status += dim(" approx")
	}
	out = append(out, status)
	if zc.highAtomicKB > 0 {
		out = append(out, yellow(fmt.Sprintf(" highatomic reserve: %s (off-limits)", humanKB(zc.highAtomicKB))))
	}
	// Zone-level accounting: allocated pages are not "blocks" anywhere —
	// this is where the free blocks went (usually page cache under IO).
	if zc.haveZoneinfo && zc.managed > 0 {
		pageB := float64(os.Getpagesize())
		used := zc.managed - zc.freePages
		var dFree int64
		if zp != nil && zp.haveZoneinfo {
			dFree = int64(zc.freePages) - int64(zp.freePages)
		}
		out = append(out, fmt.Sprintf(" managed %s · free %s (%.1f%%) Δ%s",
			human(float64(zc.managed)*pageB), human(float64(zc.freePages)*pageB),
			float64(zc.freePages)/float64(zc.managed)*100, human(float64(dFree)*pageB)))
		var kern uint64
		if used > zc.zFile+zc.zAnon {
			kern = used - zc.zFile - zc.zAnon
		}
		fileLine := fmt.Sprintf(" used %s = file %s + anon %s + kernel/slab %s",
			human(float64(used)*pageB), human(float64(zc.zFile)*pageB),
			human(float64(zc.zAnon)*pageB), human(float64(kern)*pageB))
		var dFile int64
		if zp != nil && zp.haveZoneinfo {
			dFile = int64(zc.zFile) - int64(zp.zFile)
		}
		if dFile != 0 {
			fileLine += fmt.Sprintf("  (file Δ%s)", human(float64(dFile)*pageB))
		}
		out = append(out, fileLine)
		wm := fmt.Sprintf(" watermarks min %s · low %s · high %s",
			human(float64(zc.wmin)*pageB), human(float64(zc.wlow)*pageB), human(float64(zc.whigh)*pageB))
		switch {
		case zc.freePages <= zc.wmin:
			wm = red(wm + "  ← below min: allocations stalling")
		case zc.freePages <= zc.wlow:
			wm = yellow(wm + "  ← below low: kswapd reclaiming")
		}
		out = append(out, wm)
	}
	// usable = allocatable by normal requests (excludes HighAtomic/CMA);
	// total = every free block in the zone (raw buddyinfo). A large gap
	// means free memory is locked away in reserves.
	counts := zc.usable
	var prevCounts []uint64
	if zp != nil {
		prevCounts = zp.usable
	}
	if !zc.havePagetype {
		counts = zc.buddy
		if zp != nil {
			prevCounts = zp.buddy
		}
	}
	out = append(out, dim(fmt.Sprintf(" %-5s %9s %11s %11s %8s", "ORDER", "SIZE", "USABLE", "TOTAL", "Δusable")))
	for o, n := range counts {
		if len(out) >= h {
			break
		}
		var total uint64
		if o < len(zc.buddy) {
			total = zc.buddy[o]
		}
		usable := fmt.Sprintf("%11d", n)
		if !zc.havePagetype {
			usable = fmt.Sprintf("%11s", "?") // pagetypeinfo unreadable
		}
		var d int64
		if o < len(prevCounts) {
			d = int64(n) - int64(prevCounts[o])
		}
		trend := dim("·")
		switch {
		case d > 0:
			trend = green("↑")
		case d < 0:
			trend = yellow("↓")
		}
		line := fmt.Sprintf(" %5d %9s %s %11d %+8d %s", o, humanKB(pageKB<<o), usable, total, d, trend)
		if zc.havePagetype && n == 0 && o >= 3 {
			mark := "✗"
			if total > 0 {
				mark = "✗ reserve-only" // free blocks exist but are fenced off
			}
			line = red(fmt.Sprintf(" %5d %9s %s %11d %+8d %s", o, humanKB(pageKB<<o), usable, total, d, mark))
		}
		out = append(out, line)
	}
	return out
}

func findZone(zones []*zoneFrag, name string) *zoneFrag {
	for _, z := range zones {
		if z.zone == name && z.node == "0" {
			return z
		}
	}
	for _, z := range zones {
		if z.zone == name {
			return z
		}
	}
	return nil
}

// ---- terminal helpers ----

var ansiRe = regexp.MustCompile("\x1b\\[[0-9;]*m")

func visLen(s string) int { return len([]rune(ansiRe.ReplaceAllString(s, ""))) }

// padTo pads (or plain-text clips) s to exactly w visible columns.
func padTo(s string, w int) string {
	v := visLen(s)
	switch {
	case v < w:
		return s + strings.Repeat(" ", w-v)
	case v > w:
		r := []rune(ansiRe.ReplaceAllString(s, ""))
		return string(r[:w])
	}
	return s
}

func rule(title string, width int) string {
	line := "── " + title + " "
	for len([]rune(line)) < width {
		line += "─"
	}
	return string([]rune(line)[:width])
}

func clip(s string, n int) string {
	r := []rune(s)
	if len(r) <= n {
		return s
	}
	return string(r[:n])
}

// termSize returns the terminal dimensions, defaulting to 40x120.
func termSize() (rows, cols int) {
	var ws struct{ rows, cols, x, y uint16 }
	_, _, errno := syscall.Syscall(syscall.SYS_IOCTL,
		uintptr(syscall.Stdout), uintptr(syscall.TIOCGWINSZ), uintptr(unsafe.Pointer(&ws)))
	if errno != 0 || ws.rows == 0 || ws.cols == 0 {
		return 40, 120
	}
	return int(ws.rows), int(ws.cols)
}
