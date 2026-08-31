package main

import (
	"bufio"
	"flag"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"unicode/utf8"

	"github.com/gdamore/tcell/v2"
	"github.com/rivo/tview"
)

// Palette stays on the terminal background. No painted chrome, no
// hairlines — tview attribute flags also must never set underline.
var (
	colorAccent     = tcell.NewRGBColor(139, 92, 246)
	colorText       = tcell.NewRGBColor(226, 232, 240)
	colorDim        = tcell.NewRGBColor(100, 116, 139)
	colorMute       = tcell.NewRGBColor(71, 85, 105)
	colorSelectedBg = tcell.NewRGBColor(40, 30, 58)
)

const (
	// matchOpen/matchClose wrap search hits. Close must reset attributes
	// ([-] only restores the foreground and would leave underline/bold on).
	accentHex     = "#8b5cf6"
	matchOpen     = "[#a78bfa::b]"
	matchClose    = "[-:-:-]"
	bookmarkMark  = "●"
	maxListItems  = 100
	maxCmdDisplay = 200
)

type HistoryApp struct {
	app              *tview.Application
	inputField       *tview.InputField
	list             *tview.List
	searchRow        *tview.Flex
	statusView       *tview.TextView
	combinedView     *tview.TextView
	footerView       *tview.TextView
	mainContent      *tview.Flex
	selectedIdx      int
	updatingList     bool
	history          []string
	bookmarks        []string
	bookmarkSet      map[string]bool
	showingBookmarks bool
	combinedCommands []string
	combinedChanged  bool
	filtered         []string
	searchQuery      string
}

func main() {
	bookmarksFlag := flag.Bool("bookmarks", false, "start directly in the bookmarks list")
	flag.Parse()

	tview.Styles.PrimitiveBackgroundColor = tcell.ColorDefault
	tview.Styles.ContrastBackgroundColor = tcell.ColorDefault
	tview.Styles.PrimaryTextColor = colorText
	tview.Styles.BorderColor = tcell.ColorDefault
	tview.Styles.TitleColor = colorAccent
	tview.Styles.GraphicsColor = colorMute

	commands, err := loadCommands()
	if err != nil {
		fmt.Fprintf(os.Stderr, "Error loading commands: %v\n", err)
		os.Exit(1)
	}

	if len(commands) == 0 {
		fmt.Fprintf(os.Stderr, "No commands found\n")
		os.Exit(1)
	}

	bookmarks, err := loadBookmarks()
	if err != nil {
		bookmarks = []string{}
	}

	ha := &HistoryApp{
		app:         tview.NewApplication(),
		history:     commands,
		bookmarks:   bookmarks,
		bookmarkSet: make(map[string]bool, len(bookmarks)),
		filtered:    commands,
	}

	for _, b := range bookmarks {
		ha.bookmarkSet[b] = true
	}

	if *bookmarksFlag {
		ha.showingBookmarks = true
		ha.filtered = append([]string(nil), ha.bookmarks...)
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

func readCache(path string) ([]string, error) {
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

func writeCache(path string, commands []string) error {
	file, err := os.Create(path)
	if err != nil {
		return err
	}
	defer file.Close()

	writer := bufio.NewWriter(file)
	for _, cmd := range commands {
		if _, err := fmt.Fprintln(writer, cmd); err != nil {
			return err
		}
	}
	return writer.Flush()
}

func loadCommands() ([]string, error) {
	home := os.Getenv("HOME")
	historyPath := filepath.Join(home, ".bash_history")
	cacheDir := filepath.Join(home, ".gistory")
	cachePath := filepath.Join(cacheDir, "commands")

	// Ensure the cache directory exists
	if err := os.MkdirAll(cacheDir, 0755); err != nil {
		return nil, fmt.Errorf("creating cache directory: %w", err)
	}

	bashHistory, err := readHistory(historyPath)
	if err != nil && !os.IsNotExist(err) {
		return nil, fmt.Errorf("reading bash history: %w", err)
	}

	cacheCommands, err := readCache(cachePath)
	if err != nil && !os.IsNotExist(err) {
		return nil, fmt.Errorf("reading cache: %w", err)
	}

	// Load both into a map to remove duplicates.
	// Prefer most recent commands from bash history.
	seen := make(map[string]bool)
	result := make([]string, 0, len(bashHistory)+len(cacheCommands))

	// Add commands from bash history (most recent first)
	for i := len(bashHistory) - 1; i >= 0; i-- {
		cmd := bashHistory[i]
		if !seen[cmd] {
			seen[cmd] = true
			result = append(result, cmd)
		}
	}

	// Add any unique commands from the cache
	for _, cmd := range cacheCommands {
		if !seen[cmd] {
			seen[cmd] = true
			result = append(result, cmd)
		}
	}

	// Rewrite the cache with the new de-duped list
	if err := writeCache(cachePath, result); err != nil {
		return nil, fmt.Errorf("writing cache: %w", err)
	}

	return result, nil
}

func loadBookmarks() ([]string, error) {
	home := os.Getenv("HOME")
	path := filepath.Join(home, ".gistory", "bookmarks")
	bookmarks, err := readCache(path)
	if err != nil {
		if os.IsNotExist(err) {
			return []string{}, nil
		}
		return nil, err
	}
	return bookmarks, nil
}

func saveBookmarks(bookmarks []string) error {
	home := os.Getenv("HOME")
	path := filepath.Join(home, ".gistory", "bookmarks")
	return writeCache(path, bookmarks)
}

func (ha *HistoryApp) buildUI() {
	inputBox := tview.NewInputField().
		SetLabel("  › ").
		SetFieldWidth(0).
		SetPlaceholder("search history").
		SetChangedFunc(func(text string) {
			ha.searchQuery = text
			ha.filterHistory(text)
		})

	transparent := tcell.StyleDefault.
		Foreground(colorText).
		Background(tcell.ColorDefault)
	inputBox.SetLabelStyle(tcell.StyleDefault.
		Foreground(colorAccent).
		Background(tcell.ColorDefault).
		Bold(true)).
		SetFieldStyle(transparent).
		SetPlaceholderStyle(tcell.StyleDefault.
			Foreground(colorDim).
			Background(tcell.ColorDefault))
	inputBox.SetBackgroundColor(tcell.ColorDefault)

	ha.inputField = inputBox

	ha.list = tview.NewList().
		ShowSecondaryText(false).
		SetHighlightFullLine(true).
		SetWrapAround(true)

	ha.list.SetMainTextStyle(transparent)
	ha.list.SetSelectedStyle(tcell.StyleDefault.
		Foreground(colorText).
		Background(colorSelectedBg))
	ha.list.SetBackgroundColor(tcell.ColorDefault)
	ha.list.SetShortcutStyle(tcell.StyleDefault.Foreground(colorAccent).Background(tcell.ColorDefault))

	ha.inputField.SetInputCapture(func(event *tcell.EventKey) *tcell.EventKey {
		switch event.Key() {
		case tcell.KeyEscape:
			ha.app.Stop()
			return nil
		case tcell.KeyDown, tcell.KeyCtrlN:
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
		case tcell.KeyCtrlB:
			ha.toggleBookmarksView()
			return nil
		case tcell.KeyPgUp, tcell.KeyPgDn:
			return nil
		case tcell.KeyBackspace, tcell.KeyBackspace2:
			return event
		}
		return event
	})

	ha.list.SetInputCapture(func(event *tcell.EventKey) *tcell.EventKey {
		switch event.Key() {
		case tcell.KeyEscape:
			ha.app.Stop()
			return nil
		case tcell.KeyEnter:
			if len(ha.combinedCommands) > 0 {
				combined := strings.Join(ha.combinedCommands, " ; ")
				ha.app.Stop()
				fmt.Print(combined)
				return nil
			}
		case tcell.KeyLeft:
			if idx := ha.list.GetCurrentItem(); idx >= 0 && idx < len(ha.filtered) {
				cmd := ha.filtered[idx]
				ha.combinedCommands = append(ha.combinedCommands, cmd)
				ha.combinedChanged = true
				// ForceDraw is explicitly safe to call during direct event handling
				// (per tview docs). It runs BeforeDraw (the safe hook for structural
				// layout changes like conditionally adding the combined bar) then the
				// actual draw. This prevents freezing/reentrancy.
				ha.app.ForceDraw()
			}
			return nil
		case tcell.KeyRight:
			idx := ha.list.GetCurrentItem()
			if idx >= 0 && idx < len(ha.filtered) {
				cmd := ha.filtered[idx]
				ha.toggleBookmark(cmd)
				ha.filterHistory(ha.searchQuery)

				// Try to keep the cursor on the same command (or same position if it was removed)
				if len(ha.filtered) > 0 {
					newIdx := idx
					for i, c := range ha.filtered {
						if c == cmd {
							newIdx = i
							break
						}
					}
					if newIdx >= len(ha.filtered) {
						newIdx = len(ha.filtered) - 1
					}
					ha.list.SetCurrentItem(newIdx)
				}
			}
			return nil
		case tcell.KeyCtrlB:
			ha.toggleBookmarksView()
			return nil
		case tcell.KeyPgUp, tcell.KeyPgDn:
			return nil
		case tcell.KeyRune:
			currentText := ha.inputField.GetText()
			ha.inputField.SetText(currentText + string(event.Rune()))
			ha.app.SetFocus(ha.inputField)
			return nil
		case tcell.KeyBackspace, tcell.KeyBackspace2:
			currentText := ha.inputField.GetText()
			if len(currentText) > 0 {
				ha.inputField.SetText(currentText[:len(currentText)-1])
			}
			ha.app.SetFocus(ha.inputField)
			return nil
		}
		return event
	})

	ha.list.SetSelectedFunc(func(index int, mainText string, secondaryText string, shortcut rune) {
		ha.selectCommand(index)
	})

	ha.list.SetChangedFunc(func(index int, _ string, _ string, _ rune) {
		if ha.updatingList || len(ha.filtered) == 0 {
			return
		}
		if ha.selectedIdx != index {
			ha.setItemDisplay(ha.selectedIdx, false)
		}
		ha.selectedIdx = index
		ha.setItemDisplay(index, true)
	})

	statusView := tview.NewTextView().
		SetDynamicColors(true).
		SetTextAlign(tview.AlignRight)
	statusView.SetBackgroundColor(tcell.ColorDefault)
	ha.statusView = statusView

	searchRow := tview.NewFlex().SetDirection(tview.FlexColumn)
	searchRow.SetBackgroundColor(tcell.ColorDefault)
	searchRow.AddItem(ha.inputField, 0, 1, true)
	searchRow.AddItem(statusView, 24, 0, false)
	ha.searchRow = searchRow

	ha.combinedView = tview.NewTextView().SetDynamicColors(true)
	ha.combinedView.SetBackgroundColor(tcell.ColorDefault)
	ha.combinedView.SetTextColor(colorText)

	ha.footerView = tview.NewTextView().SetDynamicColors(true)
	ha.footerView.SetBackgroundColor(tcell.ColorDefault)
	ha.footerView.SetText("  [#475569]↵ select   ← combine   → bookmark   ^B bookmarks   esc[-:-:-]")

	ha.mainContent = tview.NewFlex().SetDirection(tview.FlexRow)
	ha.rebuildMainContent()

	ha.updateList()
	ha.syncSearchPrompt()

	root := tview.NewFlex().
		AddItem(nil, 0, 0, false).
		AddItem(ha.mainContent, 0, 1, true).
		AddItem(nil, 0, 0, false)

	ha.app.SetRoot(root, true)
	ha.app.SetFocus(ha.inputField)

	// Global input capture to handle Ctrl+Backspace for removing combined commands,
	// even when the input search bar has focus. This prevents the search bar from
	// consuming the key for editing the query text.
	ha.app.SetInputCapture(func(event *tcell.EventKey) *tcell.EventKey {
		if event.Key() == tcell.KeyCtrlH ||
			((event.Key() == tcell.KeyBackspace || event.Key() == tcell.KeyBackspace2) &&
				event.Modifiers()&tcell.ModCtrl != 0) {
			if len(ha.combinedCommands) > 0 {
				ha.combinedCommands = ha.combinedCommands[:len(ha.combinedCommands)-1]
				ha.combinedChanged = true
				ha.app.ForceDraw()
				return nil
			}
		}
		return event
	})

	// Use BeforeDraw to safely perform structural layout changes (e.g. adding/removing
	// the combined bar) right before rendering. This avoids reentrancy issues when
	// triggered from InputCapture handlers.
	ha.app.SetBeforeDrawFunc(func(screen tcell.Screen) bool {
		if ha.combinedChanged {
			ha.rebuildMainContent()
			ha.combinedChanged = false
		}
		return false
	})
}

func (ha *HistoryApp) toggleBookmarksView() {
	ha.showingBookmarks = !ha.showingBookmarks
	ha.filterHistory(ha.searchQuery)
	ha.syncSearchPrompt()
}

func (ha *HistoryApp) syncSearchPrompt() {
	if ha.inputField == nil {
		return
	}
	if ha.showingBookmarks {
		ha.inputField.SetPlaceholder("search bookmarks")
	} else {
		ha.inputField.SetPlaceholder("search history")
	}
}

func (ha *HistoryApp) filterHistory(query string) {
	source := ha.history
	if ha.showingBookmarks {
		source = ha.bookmarks
	}
	ha.filtered = filterAndSortCommands(source, query)
	ha.updateList()
}

func (ha *HistoryApp) rebuildMainContent() {
	if ha.mainContent == nil {
		return
	}

	ha.mainContent.Clear()
	ha.mainContent.AddItem(ha.searchRow, 1, 0, false)

	if len(ha.combinedCommands) > 0 {
		parts := make([]string, len(ha.combinedCommands))
		for i, cmd := range ha.combinedCommands {
			parts[i] = tview.Escape(cmd)
		}
		ha.combinedView.SetText("  " + strings.Join(parts, " [#64748b];[-:-:-] "))
		ha.mainContent.AddItem(ha.combinedView, 1, 0, false)
	}

	ha.mainContent.AddItem(ha.list, 0, 1, false)
	ha.mainContent.AddItem(ha.footerView, 1, 0, false)
}

func (ha *HistoryApp) toggleBookmark(cmd string) {
	if ha.bookmarkSet == nil {
		ha.bookmarkSet = make(map[string]bool)
	}

	if ha.bookmarkSet[cmd] {
		delete(ha.bookmarkSet, cmd)
		// remove from slice while preserving order
		newBookmarks := make([]string, 0, len(ha.bookmarks))
		for _, b := range ha.bookmarks {
			if b != cmd {
				newBookmarks = append(newBookmarks, b)
			}
		}
		ha.bookmarks = newBookmarks
	} else {
		ha.bookmarkSet[cmd] = true
		ha.bookmarks = append(ha.bookmarks, cmd)
	}

	_ = saveBookmarks(ha.bookmarks)
}

func filterAndSortCommands(history []string, query string) []string {
	if query == "" {
		return history
	}

	prefixMatches := make([]string, 0)
	substringMatches := make([]string, 0)
	fuzzyMatches := make([]string, 0)

	lowerQuery := strings.ToLower(query)

	for _, cmd := range history {
		lowerCmd := strings.ToLower(cmd)

		if strings.HasPrefix(lowerCmd, lowerQuery) {
			prefixMatches = append(prefixMatches, cmd)
		} else if strings.Contains(lowerCmd, lowerQuery) {
			substringMatches = append(substringMatches, cmd)
		} else if fuzzyMatch(lowerCmd, lowerQuery) {
			fuzzyMatches = append(fuzzyMatches, cmd)
		}
	}

	result := make([]string, 0, len(prefixMatches)+len(substringMatches)+len(fuzzyMatches))
	result = append(result, prefixMatches...)
	result = append(result, substringMatches...)
	result = append(result, fuzzyMatches...)

	return result
}

func fuzzyMatch(text, pattern string) bool {
	patternIdx := 0
	for i := 0; i < len(text) && patternIdx < len(pattern); i++ {
		if text[i] == pattern[patternIdx] {
			patternIdx++
		}
	}
	return patternIdx == len(pattern)
}

func (ha *HistoryApp) displayBody(cmd string) string {
	displaySrc, truncated := truncateDisplay(cmd, maxCmdDisplay)
	var displayCmd string
	if ha.searchQuery != "" {
		displayCmd = highlightMatches(displaySrc, ha.searchQuery)
	} else {
		displayCmd = tview.Escape(displaySrc)
	}
	if truncated {
		displayCmd += "[#64748b]…[-:-:-]"
	}
	return displayCmd
}

func (ha *HistoryApp) itemPrefix(index int, selected bool) string {
	if index < 0 || index >= len(ha.filtered) {
		return "  "
	}
	starred := ha.bookmarkSet != nil && ha.bookmarkSet[ha.filtered[index]]
	switch {
	case starred:
		return "[" + accentHex + "]" + bookmarkMark + "[-:-:-] "
	case selected:
		return "[" + accentHex + "]›[-:-:-] "
	default:
		return "  "
	}
}

func (ha *HistoryApp) setItemDisplay(index int, selected bool) {
	if ha.list == nil || index < 0 || index >= ha.list.GetItemCount() || index >= len(ha.filtered) {
		return
	}
	ha.list.SetItemText(index, ha.itemPrefix(index, selected)+ha.displayBody(ha.filtered[index]), "")
}

func (ha *HistoryApp) updateList() {
	if ha.list == nil {
		return
	}

	ha.updatingList = true
	defer func() { ha.updatingList = false }()

	ha.list.Clear()
	ha.selectedIdx = 0

	if len(ha.filtered) == 0 {
		empty := "  [#475569]no matching commands[-:-:-]"
		if ha.showingBookmarks && ha.searchQuery == "" {
			empty = "  [#475569]no bookmarks yet · → to bookmark a command[-:-:-]"
		}
		ha.list.AddItem(empty, "", 0, nil)
		ha.updateStatus()
		return
	}

	maxItems := min(len(ha.filtered), maxListItems)
	for i := range maxItems {
		ha.list.AddItem(ha.itemPrefix(i, i == 0)+ha.displayBody(ha.filtered[i]), "", 0, nil)
	}

	ha.updateStatus()
}

func (ha *HistoryApp) updateStatus() {
	if ha.statusView == nil {
		return
	}

	count := formatCount(len(ha.filtered))
	if ha.showingBookmarks {
		ha.statusView.SetText(fmt.Sprintf("[#475569]bookmarks  %s[-:-:-]  ", count))
		return
	}
	ha.statusView.SetText(fmt.Sprintf("[#475569]%s[-:-:-]  ", count))
}

func formatCount(n int) string {
	if n < 1000 {
		return fmt.Sprintf("%d", n)
	}
	s := fmt.Sprintf("%d", n)
	var b strings.Builder
	lead := len(s) % 3
	if lead == 0 {
		lead = 3
	}
	b.WriteString(s[:lead])
	for i := lead; i < len(s); i += 3 {
		b.WriteByte(',')
		b.WriteString(s[i : i+3])
	}
	return b.String()
}

func truncateDisplay(s string, maxRunes int) (string, bool) {
	if utf8.RuneCountInString(s) <= maxRunes {
		return s, false
	}
	runes := []rune(s)
	return string(runes[:maxRunes]), true
}

func highlightMatches(text, pattern string) string {
	if pattern == "" {
		return tview.Escape(text)
	}

	lowerText := strings.ToLower(text)
	lowerPattern := strings.ToLower(pattern)
	// ToLower can change byte length on some UTF-8 folds (e.g. İ).
	// Skip highlighting rather than slicing mid-sequence.
	if lowerPattern == "" || len(lowerText) != len(text) {
		return tview.Escape(text)
	}

	if strings.Contains(lowerText, lowerPattern) {
		return highlightSubstrings(text, lowerText, lowerPattern)
	}
	return highlightFuzzy(text, lowerText, lowerPattern)
}

func highlightSubstrings(text, lowerText, lowerPattern string) string {
	var b strings.Builder
	patLen := len(lowerPattern)
	start := 0
	for start <= len(lowerText)-patLen {
		idx := strings.Index(lowerText[start:], lowerPattern)
		if idx < 0 {
			break
		}
		idx += start
		b.WriteString(tview.Escape(text[start:idx]))
		b.WriteString(matchOpen)
		b.WriteString(tview.Escape(text[idx : idx+patLen]))
		b.WriteString(matchClose)
		start = idx + patLen
	}
	b.WriteString(tview.Escape(text[start:]))
	return b.String()
}

func highlightFuzzy(text, lowerText, lowerPattern string) string {
	var b strings.Builder
	patternIdx := 0
	i := 0
	for i < len(text) {
		if patternIdx < len(lowerPattern) && lowerText[i] == lowerPattern[patternIdx] {
			start := i
			for i < len(text) && patternIdx < len(lowerPattern) && lowerText[i] == lowerPattern[patternIdx] {
				i++
				patternIdx++
			}
			b.WriteString(matchOpen)
			b.WriteString(tview.Escape(text[start:i]))
			b.WriteString(matchClose)
			continue
		}
		j := i + 1
		for j < len(text) && !(patternIdx < len(lowerPattern) && lowerText[j] == lowerPattern[patternIdx]) {
			j++
		}
		b.WriteString(tview.Escape(text[i:j]))
		i = j
	}
	return b.String()
}

func (ha *HistoryApp) selectCommand(index int) {
	if len(ha.combinedCommands) > 0 {
		combined := strings.Join(ha.combinedCommands, " ; ")
		ha.app.Stop()
		fmt.Print(combined)
		return
	}
	if index >= 0 && index < len(ha.filtered) {
		ha.app.Stop()
		fmt.Print(ha.filtered[index])
	}
}
