package main

import (
	"strings"
	"testing"

	"github.com/gdamore/tcell/v2"
	"github.com/rivo/tview"
)

func TestFilterHistorySorting(t *testing.T) {
	// Sample history
	history := []string{
		"history command",   // prefix match for "hist"
		"git push",          // no match for "hist"
		"show history",      // substring match for "hist"
		"historical data",   // prefix match for "hist"
		"git pull",          // no match for "hist"
		"bash history file", // substring match for "hist"
		"git add .",         // no match for "hist"
	}

	// Test filtering with "hist"
	filtered := filterAndSortCommands(history, "hist")

	if len(filtered) != 4 {
		t.Errorf("Expected 4 matches for 'hist', got %d", len(filtered))
	}

	// Check that prefix matches come first
	if len(filtered) >= 2 {
		// First two should be "history command" and "historical data" (prefix matches)
		cmd1 := filtered[0]
		cmd2 := filtered[1]

		isPrefixMatch1 := cmd1 == "history command" || cmd1 == "historical data"
		isPrefixMatch2 := cmd2 == "history command" || cmd2 == "historical data"

		if !isPrefixMatch1 {
			t.Errorf("First result should be a prefix match, got: %s", cmd1)
		}
		if !isPrefixMatch2 {
			t.Errorf("Second result should be a prefix match, got: %s", cmd2)
		}
	}

	// Check that substring matches come after prefix matches
	if len(filtered) >= 4 {
		cmd3 := filtered[2]
		cmd4 := filtered[3]

		isSubstringMatch3 := cmd3 == "show history" || cmd3 == "bash history file"
		isSubstringMatch4 := cmd4 == "show history" || cmd4 == "bash history file"

		if !isSubstringMatch3 {
			t.Errorf("Third result should be a substring match, got: %s", cmd3)
		}
		if !isSubstringMatch4 {
			t.Errorf("Fourth result should be a substring match, got: %s", cmd4)
		}
	}

	t.Logf("\nFiltered results for 'hist':")
	for i, cmd := range filtered {
		t.Logf("  [%d] %s", i, cmd)
	}
}

func TestHighlightMatchesNoUnderline(t *testing.T) {
	cases := []struct {
		text, pattern string
	}{
		{"git status", "git"},
		{"git status", "gs"},
		{"docker compose up", "compose"},
		{"echo [red]alert", "red"},
		{"HISTFILE", "hist"},
	}

	for _, tc := range cases {
		got := highlightMatches(tc.text, tc.pattern)
		if strings.Contains(got, "::bu") || strings.Contains(got, "::u") || strings.Contains(got, ":u]") {
			t.Errorf("highlightMatches(%q, %q) set underline: %q", tc.text, tc.pattern, got)
		}
		if !strings.Contains(got, matchOpen) {
			t.Errorf("highlightMatches(%q, %q) missing match style: %q", tc.text, tc.pattern, got)
		}
		if strings.Count(got, matchOpen) != strings.Count(got, matchClose) {
			t.Errorf("highlightMatches(%q, %q) did not reset every highlight: %q", tc.text, tc.pattern, got)
		}
		if !strings.Contains(matchClose, ":-:-") {
			t.Error("matchClose must reset attributes, not just the foreground color")
		}
	}
}

func TestHighlightMatchesEscapesBrackets(t *testing.T) {
	got := highlightMatches("echo [red] hello", "echo")
	if strings.Contains(got, "[red]") && !strings.Contains(got, "[red[]") {
		t.Errorf("command brackets were not escaped: %q", got)
	}
}

func TestHighlightMatchesSubstringSpan(t *testing.T) {
	got := highlightMatches("git status", "git")
	want := matchOpen + "git" + matchClose + " status"
	if got != want {
		t.Errorf("contiguous highlight = %q, want %q", got, want)
	}
}

func TestHighlightDoesNotUnderlineDrawnCells(t *testing.T) {
	screen := tcell.NewSimulationScreen("UTF-8")
	if err := screen.Init(); err != nil {
		t.Fatal(err)
	}
	defer screen.Fini()
	screen.SetSize(80, 6)

	list := tview.NewList().ShowSecondaryText(false).SetHighlightFullLine(true)
	list.SetMainTextStyle(tcell.StyleDefault.Foreground(tcell.ColorWhite))
	list.AddItem(highlightMatches("git status --short", "git"), "", 0, nil)
	list.AddItem(tview.Escape("unrelated command"), "", 0, nil)
	list.AddItem(highlightMatches("echo [red]warning", "red"), "", 0, nil)
	list.SetRect(0, 0, 80, 6)
	list.Draw(screen)
	screen.Show()

	for y := 0; y < 6; y++ {
		for x := 0; x < 80; x++ {
			_, _, style, _ := screen.GetContent(x, y)
			_, _, attrs := style.Decompose()
			if attrs&tcell.AttrUnderline != 0 {
				t.Fatalf("underline attribute at (%d,%d) after drawing highlighted list", x, y)
			}
		}
	}
}

func TestFormatCount(t *testing.T) {
	if formatCount(12) != "12" {
		t.Errorf("formatCount(12) = %q", formatCount(12))
	}
	if formatCount(1284) != "1,284" {
		t.Errorf("formatCount(1284) = %q", formatCount(1284))
	}
	if formatCount(1000000) != "1,000,000" {
		t.Errorf("formatCount(1000000) = %q", formatCount(1000000))
	}
}

func TestFilterHistoryFuzzyMatches(t *testing.T) {
	history := []string{
		"git push",                // prefix match for "git"
		"git pull",                // prefix match for "git"
		"go install tools",        // fuzzy match for "git" (g-i-t)
		"gradle integration test", // fuzzy match for "git" (g-i-t)
		"git commit",              // prefix match for "git"
	}

	filtered := filterAndSortCommands(history, "git")

	if len(filtered) != 5 {
		t.Errorf("Expected 5 matches for 'git', got %d", len(filtered))
	}

	// First three should be prefix matches (git push, git pull, git commit)
	for i := 0; i < 3 && i < len(filtered); i++ {
		cmd := filtered[i]
		if len(cmd) < 3 || cmd[0:3] != "git" {
			t.Errorf("Result %d should be a prefix match, got: %s", i, cmd)
		}
	}

	// Last two should be fuzzy matches
	if len(filtered) >= 5 {
		cmd4 := filtered[3]
		cmd5 := filtered[4]

		isFuzzyMatch4 := cmd4 == "go install tools" || cmd4 == "gradle integration test"
		isFuzzyMatch5 := cmd5 == "go install tools" || cmd5 == "gradle integration test"

		if !isFuzzyMatch4 {
			t.Errorf("Result 4 should be a fuzzy match, got: %s", cmd4)
		}
		if !isFuzzyMatch5 {
			t.Errorf("Result 5 should be a fuzzy match, got: %s", cmd5)
		}
	}

	t.Logf("\nFiltered results for 'git':")
	for i, cmd := range filtered {
		t.Logf("  [%d] %s", i, cmd)
	}
}
