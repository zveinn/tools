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
	header      *tview.TextView
	searchQuery string
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
	inputBox := tview.NewInputField().
		SetLabel("> ").
		SetFieldWidth(0).
		SetChangedFunc(func(text string) {
			ha.searchQuery = text
			ha.filterHistory(text)
		})

	// Force colors - use RGB to ensure visibility
	inputBox.SetLabelColor(tcell.NewRGBColor(0, 255, 0)). // Green label
		SetFieldTextColor(tcell.NewRGBColor(255, 255, 255)). // White text
		SetFieldBackgroundColor(tcell.NewRGBColor(0, 0, 0)) // Black background

	ha.inputField = inputBox

	// Create list with custom styling
	ha.list = tview.NewList().
		ShowSecondaryText(false).
		SetHighlightFullLine(true)

	ha.list.SetMainTextColor(tcell.ColorWhite).
		SetSelectedTextColor(tcell.ColorWhite).
		SetSelectedBackgroundColor(tcell.NewRGBColor(0, 0, 139)). // Dark blue
		SetShortcutColor(tcell.ColorGreen)

	// Initial population
	ha.updateList()

	// Handle input field keys
	ha.inputField.SetInputCapture(func(event *tcell.EventKey) *tcell.EventKey {
		switch event.Key() {
		case tcell.KeyEscape:
			ha.app.Stop()
			return nil
		case tcell.KeyDown, tcell.KeyCtrlN:
			// Move to second item when first pressing down
			if len(ha.filtered) > 1 {
				ha.list.SetCurrentItem(1)
			}
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
			// Any character, add it to input and go back
			currentText := ha.inputField.GetText()
			ha.inputField.SetText(currentText + string(event.Rune()))
			ha.app.SetFocus(ha.inputField)
			return nil
		case tcell.KeyBackspace, tcell.KeyBackspace2:
			// Remove last character from input
			currentText := ha.inputField.GetText()
			if len(currentText) > 0 {
				ha.inputField.SetText(currentText[:len(currentText)-1])
			}
			ha.app.SetFocus(ha.inputField)
			return nil
		}
		return event
	})

	// Handle list selection
	ha.list.SetSelectedFunc(func(index int, mainText string, secondaryText string, shortcut rune) {
		ha.selectCommand(index)
	})

	// Create list container with minimal border (1 char padding)
	listContainer := tview.NewFlex().
		SetDirection(tview.FlexRow).
		AddItem(ha.list, 0, 1, false)

	listFrame := tview.NewFrame(listContainer).
		SetBorders(0, 0, 1, 0, 0, 0)

	listWithBorder := tview.NewFlex().
		SetDirection(tview.FlexRow).
		AddItem(listFrame, 0, 1, false)

	// Create main layout
	mainContent := tview.NewFlex().
		SetDirection(tview.FlexRow).
		AddItem(ha.inputField, 1, 0, true).
		AddItem(listWithBorder, 0, 1, false)

	// Add padding to the sides (1 char only)
	flex := tview.NewFlex().
		AddItem(nil, 1, 0, false).
		AddItem(mainContent, 0, 1, true).
		AddItem(nil, 1, 0, false)

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
		displayCmd := cmd

		// Highlight matching characters if there's a search query
		if ha.searchQuery != "" {
			displayCmd = highlightMatches(cmd, ha.searchQuery)
		}

		// Truncate long commands
		if len(cmd) > 200 {
			displayCmd = displayCmd[:200] + "[grey]...[white]"
		}

		// Add 2 char padding to align with search input
		ha.list.AddItem("  "+displayCmd, "", 0, nil)
	}
}

func highlightMatches(text, pattern string) string {
	if pattern == "" {
		return text
	}

	lowerText := strings.ToLower(text)
	lowerPattern := strings.ToLower(pattern)

	var result strings.Builder
	patternIdx := 0

	for i := 0; i < len(text); i++ {
		if patternIdx < len(lowerPattern) && lowerText[i] == lowerPattern[patternIdx] {
			// Highlight matched character in green
			result.WriteString("[green::b]")
			result.WriteByte(text[i])
			result.WriteString("[white::-]")
			patternIdx++
		} else {
			result.WriteByte(text[i])
		}
	}

	return result.String()
}

func (ha *HistoryApp) updateStatus() {
	total := len(ha.history)
	shown := len(ha.filtered)

	var statusMsg string
	if shown == 0 {
		statusMsg = "[red]✗ No matches found"
	} else if shown == total {
		statusMsg = fmt.Sprintf("[white]%d commands", total)
	} else {
		statusMsg = fmt.Sprintf("[white]%d[grey]/[white]%d commands", shown, total)
	}

	status := fmt.Sprintf("\n[::b]%s  [grey]│  [green]↵[white] select  [grey]│  [green]↑↓[white] navigate  [grey]│  [green]Esc[white] cancel", statusMsg)
	ha.statusBar.SetText(status)
}

func (ha *HistoryApp) selectCommand(index int) {
	if index >= 0 && index < len(ha.filtered) {
		ha.app.Stop()
		fmt.Print(ha.filtered[index])
	}
}
