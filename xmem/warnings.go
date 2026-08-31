package main

import (
	"fmt"
	"sort"
)

// Severity levels for findings; higher sorts first in the WARNINGS section.
const (
	sevInfo = iota
	sevWarn
	sevCrit
)

// finding is one detected problem. Checks record findings while their
// section prints, and everything is reported together at the end so
// problems are not buried in the stats.
type finding struct {
	sev  int
	text string
	hint string // what to check or do about it; may be empty
}

var findings []finding

// warnf records a finding for the final WARNINGS section. hint may be "".
func warnf(sev int, hint, format string, args ...any) {
	findings = append(findings, finding{sev: sev, text: fmt.Sprintf(format, args...), hint: hint})
}

func sevTag(sev int) string {
	switch sev {
	case sevCrit:
		return tCrit()
	case sevWarn:
		return tWarn()
	}
	return tInfo()
}

// printWarnings reports every finding collected during the run, most
// severe first, preserving detection order within a severity.
func printWarnings() {
	section("WARNINGS")
	if len(findings) == 0 {
		fmt.Printf("  %s no problems detected\n\n", tOK())
		return
	}
	sorted := make([]finding, len(findings))
	copy(sorted, findings)
	sort.SliceStable(sorted, func(i, j int) bool { return sorted[i].sev > sorted[j].sev })
	for _, f := range sorted {
		fmt.Printf("  %s %s\n", sevTag(f.sev), f.text)
		if f.hint != "" {
			fmt.Println(dim("         → " + f.hint))
		}
	}
	fmt.Println()
}
