package main

import (
	"fmt"
	"os"
	"strconv"
	"strings"
)

// psiRes holds one line of a /proc/pressure file: the share of wall time
// (percent) tasks spent stalled on the resource, averaged over 10/60/300s.
// "some" = at least one task stalled; "full" = all non-idle tasks stalled.
type psiRes struct {
	avg10, avg60, avg300 float64
}

type psiStats struct {
	some, full psiRes
	ok         bool
}

func readPSI(resource string) psiStats {
	b, err := os.ReadFile("/proc/pressure/" + resource)
	if err != nil {
		return psiStats{}
	}
	st := psiStats{ok: true}
	for _, line := range strings.Split(string(b), "\n") {
		fields := strings.Fields(line)
		if len(fields) < 4 {
			continue
		}
		var r psiRes
		for _, f := range fields[1:] {
			k, v, ok := strings.Cut(f, "=")
			if !ok {
				continue
			}
			x, _ := strconv.ParseFloat(v, 64)
			switch k {
			case "avg10":
				r.avg10 = x
			case "avg60":
				r.avg60 = x
			case "avg300":
				r.avg300 = x
			}
		}
		switch fields[0] {
		case "some":
			st.some = r
		case "full":
			st.full = r
		}
	}
	return st
}

// printPSI reports pressure stall information for memory and io — the
// leading indicators that a box is thrashing toward allocation failure.
func printPSI() {
	mem, io := readPSI("memory"), readPSI("io")
	if !mem.ok && !io.ok {
		return // kernel without PSI
	}
	section("PRESSURE STALL INFORMATION (PSI)")
	fmt.Println(dim("  % of wall time stalled: some = ≥1 task · full = all non-idle tasks"))
	row := func(name string, st psiStats) {
		if !st.ok {
			return
		}
		fmt.Printf("  %-8s some %6.2f%% /%6.2f%% /%6.2f%%   full %6.2f%% /%6.2f%% /%6.2f%%   (10s/60s/300s)\n",
			name, st.some.avg10, st.some.avg60, st.some.avg300,
			st.full.avg10, st.full.avg60, st.full.avg300)
	}
	row("memory", mem)
	row("io", io)
	fmt.Println()

	switch {
	case mem.full.avg10 >= 10:
		warnf(sevCrit, "reclaim cannot keep up with allocations — OOM kills are imminent",
			"memory PSI full=%.1f%% (10s): ALL non-idle tasks are stalled on memory", mem.full.avg10)
	case mem.full.avg10 >= 1:
		warnf(sevWarn, "reclaim is stalling every task at times; watch for growth",
			"memory PSI full=%.1f%% (10s)", mem.full.avg10)
	case mem.some.avg10 >= 10:
		warnf(sevWarn, "some tasks are waiting on reclaim",
			"memory PSI some=%.1f%% (10s)", mem.some.avg10)
	}
}
