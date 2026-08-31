package main

import (
	"fmt"
	"os"
	"regexp"
	"strconv"
	"strings"
	"syscall"
	"time"
)

// oomEvent is one OOM kill reconstructed from the kernel ring buffer. The
// kernel's report answers the questions live stats cannot: what allocation
// triggered the kill (order and gfp flags), whether the kill was global
// exhaustion or a cgroup limit, and what actually died.
type oomEvent struct {
	when       time.Time
	invoker    string // task whose allocation triggered the OOM path
	gfp        string // symbolic gfp_mask of that allocation
	order      int
	constraint string // CONSTRAINT_NONE = global, CONSTRAINT_MEMCG = cgroup limit
	memcg      string
	victim     string
	victimPID  int
	anonRSSKB  uint64
	fileRSSKB  uint64
}

type kmsgRec struct {
	usec uint64 // monotonic microseconds since boot
	msg  string
}

// readKmsgRecords drains the kernel ring buffer via /dev/kmsg. Raw
// syscalls are used deliberately: with os.File the runtime poller would
// park on EAGAIN (waiting for new messages) instead of returning at the
// end of the buffer.
func readKmsgRecords() ([]kmsgRec, error) {
	fd, err := syscall.Open("/dev/kmsg", syscall.O_RDONLY|syscall.O_NONBLOCK, 0)
	if err != nil {
		return nil, err
	}
	defer syscall.Close(fd)
	buf := make([]byte, 1<<13) // one record per read; records are <=8K
	var recs []kmsgRec
	for {
		n, err := syscall.Read(fd, buf)
		if err == syscall.EPIPE {
			continue // record overwritten mid-read; reader auto-advances
		}
		if err != nil || n <= 0 {
			return recs, nil // EAGAIN: buffer drained
		}
		// "<pri>,<seq>,<usec>,<flags>;message\n extra dict lines"
		rec := string(buf[:n])
		head, msg, ok := strings.Cut(rec, ";")
		if !ok {
			continue
		}
		var usec uint64
		if f := strings.Split(head, ","); len(f) >= 3 {
			usec, _ = strconv.ParseUint(f[2], 10, 64)
		}
		if i := strings.IndexByte(msg, '\n'); i >= 0 {
			msg = msg[:i]
		}
		recs = append(recs, kmsgRec{usec: usec, msg: msg})
	}
}

var (
	reOOMInvoked = regexp.MustCompile(`^(.+?) invoked oom-killer: gfp_mask=([^,]+), ?order=(-?\d+)`)
	reOOMKilled  = regexp.MustCompile(`Killed process (\d+) \(([^)]+)\)`)
	reOOMAnon    = regexp.MustCompile(`anon-rss:(\d+)kB`)
	reOOMFile    = regexp.MustCompile(`file-rss:(\d+)kB`)
	reOOMConstr  = regexp.MustCompile(`oom-kill:constraint=([^,]+),`)
	reOOMMemcg   = regexp.MustCompile(`(?:oom_memcg|task_memcg)=([^,]+)`)
)

// bootTime derives wall-clock boot time so kmsg's monotonic timestamps
// can be reported as dates.
func bootTime() time.Time {
	b, err := os.ReadFile("/proc/uptime")
	if err != nil {
		return time.Time{}
	}
	fields := strings.Fields(string(b))
	if len(fields) < 1 {
		return time.Time{}
	}
	up, err := strconv.ParseFloat(fields[0], 64)
	if err != nil {
		return time.Time{}
	}
	return time.Now().Add(-time.Duration(up * float64(time.Second)))
}

// parseOOMEvents assembles kill events from the three report lines the
// kernel emits per OOM (invoked -> constraint -> Killed process).
func parseOOMEvents(recs []kmsgRec) []oomEvent {
	boot := bootTime()
	var events []oomEvent
	var cur *oomEvent
	for _, r := range recs {
		if m := reOOMInvoked.FindStringSubmatch(r.msg); m != nil {
			cur = &oomEvent{invoker: m[1], gfp: m[2]}
			cur.order, _ = strconv.Atoi(m[3])
			continue
		}
		if m := reOOMConstr.FindStringSubmatch(r.msg); m != nil {
			if cur == nil {
				cur = &oomEvent{}
			}
			cur.constraint = m[1]
			if mm := reOOMMemcg.FindStringSubmatch(r.msg); mm != nil {
				cur.memcg = mm[1]
			}
			continue
		}
		if m := reOOMKilled.FindStringSubmatch(r.msg); m != nil {
			if cur == nil {
				cur = &oomEvent{}
			}
			cur.victimPID, _ = strconv.Atoi(m[1])
			cur.victim = m[2]
			if mm := reOOMAnon.FindStringSubmatch(r.msg); mm != nil {
				cur.anonRSSKB, _ = strconv.ParseUint(mm[1], 10, 64)
			}
			if mm := reOOMFile.FindStringSubmatch(r.msg); mm != nil {
				cur.fileRSSKB, _ = strconv.ParseUint(mm[1], 10, 64)
			}
			if strings.Contains(r.msg, "Memory cgroup out of memory") && cur.constraint == "" {
				cur.constraint = "CONSTRAINT_MEMCG"
			}
			if !boot.IsZero() {
				cur.when = boot.Add(time.Duration(r.usec) * time.Microsecond)
			}
			events = append(events, *cur)
			cur = nil
		}
	}
	return events
}

// printOOMLog reports past OOM kills from the kernel log and cross-checks
// against the /proc/vmstat oom_kill counter (which survives log rotation).
func printOOMLog(vmstatOOMKills uint64) {
	section("OOM KILL FORENSICS (kernel log)")
	recs, err := readKmsgRecords()
	if err != nil {
		fmt.Println(dim("  /dev/kmsg unreadable (need root) — check manually:"))
		fmt.Println(dim("    dmesg -T | grep -B2 -A30 'Out of memory'"))
		if vmstatOOMKills > 0 {
			fmt.Printf("  %s /proc/vmstat reports %d OOM kill(s) since boot\n", tCrit(), vmstatOOMKills)
			warnf(sevCrit, "run as root (or dmesg -T) for the kill details",
				"%d OOM kill(s) since boot per /proc/vmstat", vmstatOOMKills)
		}
		fmt.Println()
		return
	}

	events := parseOOMEvents(recs)
	if len(events) == 0 {
		if vmstatOOMKills > 0 {
			fmt.Printf("  %s /proc/vmstat counts %d OOM kill(s) since boot, but the reports have rotated out of the ring buffer\n",
				tWarn(), vmstatOOMKills)
			warnf(sevCrit, "check journalctl -k for the rotated reports; consider persisting the journal",
				"%d OOM kill(s) since boot; details no longer in the kernel ring buffer", vmstatOOMKills)
		} else {
			fmt.Printf("  %s no OOM kills since boot\n", tOK())
		}
		fmt.Println()
		return
	}

	const show = 3
	first := 0
	if len(events) > show {
		fmt.Printf("  %d OOM kills in the log; showing the last %d\n", len(events), show)
		first = len(events) - show
	}
	for _, ev := range events[first:] {
		when := "time unknown"
		if !ev.when.IsZero() {
			when = ev.when.Format("2006-01-02 15:04:05")
		}
		fmt.Printf("  %s [%s] killed %s (pid %d)  anon-rss %s · file-rss %s\n",
			red("✗"), when, bold(ev.victim), ev.victimPID,
			humanKB(ev.anonRSSKB), humanKB(ev.fileRSSKB))
		if ev.invoker != "" {
			fmt.Printf("      triggered by %s allocating order-%d, gfp=%s\n", ev.invoker, ev.order, ev.gfp)
		}
		scope := ev.constraint
		if scope == "" {
			scope = "unknown"
		}
		line := "      constraint " + scope
		if ev.memcg != "" {
			line += " · memcg " + ev.memcg
		}
		fmt.Println(line)
	}
	fmt.Println()

	last := events[len(events)-1]
	when := ""
	if !last.when.IsZero() {
		when = " at " + last.when.Format("2006-01-02 15:04:05")
	}
	warnf(sevCrit, "details in the OOM KILL FORENSICS section",
		"OOM killer fired %d time(s) since boot — last killed %s (pid %d)%s",
		len(events), last.victim, last.victimPID, when)
	memcgKills := 0
	for _, ev := range events {
		if strings.Contains(ev.constraint, "MEMCG") {
			memcgKills++
		}
	}
	if memcgKills > 0 {
		warnf(sevCrit, "these are cgroup-limit kills, NOT global exhaustion — raise memory.max/MemoryMax or fix the workload",
			"%d of %d OOM kills were memcg-limit kills (last memcg: %s)", memcgKills, len(events), last.memcg)
	}
	if last.order >= 3 {
		warnf(sevWarn, "fragmentation kill — see the buddy allocator section; free pages existed but not contiguously",
			"the failing allocation was order-%d (%s contiguous)", last.order, humanKB(uint64(os.Getpagesize())/1024<<last.order))
	}
	if strings.Contains(last.gfp, "NOFS") || strings.Contains(last.gfp, "NOIO") {
		warnf(sevWarn, "the allocation held fs/io locks so reclaim could not write back dirty data — check dirty/writeback and per-device bdi stats",
			"the failing allocation used %s: reclaim was restricted", last.gfp)
	}
}
