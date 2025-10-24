package main

import (
	"bufio"
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"github.com/gdamore/tcell/v2"
	"github.com/rivo/tview"
)

type HistoryApp struct {
	app         *tview.Application
	inputField  *tview.InputField
	list        *tview.List
	history     []string
	filtered    []string
	statusBar   *tview.TextView
}

func main() {
	historyPath := filepath.Join(os.Getenv("HOME"), ".bash_history")
	history, err := readHistory(historyPath)
	if err != nil {
		fmt.Fprintf(os.Stderr, "Error reading history: %v\n", err)
		os.Exit(1)
	}

	if len(history) == 0 {
		fmt.Fprintf(os.Stderr, "No history found\n")
		os.Exit(1)
	}

	// Deduplicate and reverse (most recent first)
	history = deduplicateHistory(history)

	ha := &HistoryApp{
		app:      tview.NewApplication(),
		history:  history,
		filtered: history,
	}

	ha.buildUI()

	if err := ha.app.Run(); err != nil {
		fmt.Fprintf(os.Stderr, "Error running app: %v\n", err)
		os.Exit(1)
	}
}

func readHistory(path string) ([]string, error) {
	file, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	defer file.Close()

	var lines []string
	scanner := bufio.NewScanner(file)
	for scanner.Scan() {
		line := strings.TrimSpace(scanner.Text())
		if line != "" {
			lines = append(lines, line)
		}
	}

	if err := scanner.Err(); err != nil {
		return nil, err
	}

	return lines, nil
}

func deduplicateHistory(history []string) []string {
	// Keep most recent occurrence of each command
	seen := make(map[string]bool)
	result := make([]string, 0)

	// Process from end to beginning (most recent first)
	for i := len(history) - 1; i >= 0; i-- {
		if !seen[history[i]] {
			seen[history[i]] = true
			result = append(result, history[i])
		}
	}

	return result
}

func (ha *HistoryApp) buildUI() {
	// Create input field
	ha.inputField = tview.NewInputField().
		SetLabel("Search: ").
		SetFieldWidth(0).
		SetChangedFunc(func(text string) {
			ha.filterHistory(text)
		})

	// Create list
	ha.list = tview.NewList().
		ShowSecondaryText(false).
		SetHighlightFullLine(true)

	// Create status bar
	ha.statusBar = tview.NewTextView().
		SetDynamicColors(true).
		SetTextAlign(tview.AlignLeft)

	// Initial population
	ha.updateList()
	ha.updateStatus()

	// Handle input field keys
	ha.inputField.SetInputCapture(func(event *tcell.EventKey) *tcell.EventKey {
		switch event.Key() {
		case tcell.KeyEscape:
			ha.app.Stop()
			return nil
		case tcell.KeyDown, tcell.KeyCtrlN:
			ha.app.SetFocus(ha.list)
			return nil
		case tcell.KeyUp, tcell.KeyCtrlP:
			ha.app.SetFocus(ha.list)
			return nil
		case tcell.KeyEnter:
			if len(ha.filtered) > 0 {
				ha.selectCommand(0)
			}
			return nil
		}
		return event
	})

	// Handle list keys
	ha.list.SetInputCapture(func(event *tcell.EventKey) *tcell.EventKey {
		switch event.Key() {
		case tcell.KeyEscape:
			ha.app.Stop()
			return nil
		case tcell.KeyRune:
			// Any character, go back to input
			ha.app.SetFocus(ha.inputField)
			return event
		case tcell.KeyBackspace, tcell.KeyBackspace2:
			ha.app.SetFocus(ha.inputField)
			return event
		}
		return event
	})

	// Handle list selection
	ha.list.SetSelectedFunc(func(index int, mainText string, secondaryText string, shortcut rune) {
		ha.selectCommand(index)
	})

	// Create layout
	flex := tview.NewFlex().
		SetDirection(tview.FlexRow).
		AddItem(ha.inputField, 1, 0, true).
		AddItem(ha.list, 0, 1, false).
		AddItem(ha.statusBar, 1, 0, false)

	ha.app.SetRoot(flex, true)
	ha.app.SetFocus(ha.inputField)
}

func (ha *HistoryApp) filterHistory(query string) {
	if query == "" {
		ha.filtered = ha.history
	} else {
		ha.filtered = make([]string, 0)
		lowerQuery := strings.ToLower(query)

		for _, cmd := range ha.history {
			if fuzzyMatch(strings.ToLower(cmd), lowerQuery) {
				ha.filtered = append(ha.filtered, cmd)
			}
		}
	}

	ha.updateList()
	ha.updateStatus()
}

func fuzzyMatch(text, pattern string) bool {
	// Simple fuzzy matching: all characters in pattern must appear in order
	patternIdx := 0
	for i := 0; i < len(text) && patternIdx < len(pattern); i++ {
		if text[i] == pattern[patternIdx] {
			patternIdx++
		}
	}
	return patternIdx == len(pattern)
}

func (ha *HistoryApp) updateList() {
	ha.list.Clear()

	maxItems := 100
	if len(ha.filtered) < maxItems {
		maxItems = len(ha.filtered)
	}

	for i := 0; i < maxItems; i++ {
		cmd := ha.filtered[i]
		// Truncate long commands
		if len(cmd) > 200 {
			cmd = cmd[:200] + "..."
		}
		ha.list.AddItem(cmd, "", 0, nil)
	}
}

func (ha *HistoryApp) updateStatus() {
	total := len(ha.history)
	shown := len(ha.filtered)
	status := fmt.Sprintf(" %d/%d commands | [yellow]Enter[white] to select | [yellow]Esc[white] to cancel", shown, total)
	ha.statusBar.SetText(status)
}

func (ha *HistoryApp) selectCommand(index int) {
	if index >= 0 && index < len(ha.filtered) {
		ha.app.Stop()
		fmt.Print(ha.filtered[index])
	}
}
