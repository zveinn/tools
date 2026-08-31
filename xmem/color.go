package main

import (
	"fmt"
	"os"
	"strings"
)

// colorOn enables ANSI colors only for terminal output; NO_COLOR disables,
// CLICOLOR_FORCE enables even when piped.
var colorOn = func() bool {
	if os.Getenv("NO_COLOR") != "" {
		return false
	}
	if os.Getenv("CLICOLOR_FORCE") != "" {
		return true
	}
	fi, err := os.Stdout.Stat()
	return err == nil && fi.Mode()&os.ModeCharDevice != 0
}()

func paint(code, s string) string {
	if !colorOn {
		return s
	}
	return "\033[" + code + "m" + s + "\033[0m"
}

func red(s string) string    { return paint("31;1", s) }
func yellow(s string) string { return paint("33;1", s) }
func green(s string) string  { return paint("32", s) }
func cyan(s string) string   { return paint("36;1", s) }
func dim(s string) string    { return paint("2", s) }
func bold(s string) string   { return paint("1", s) }

// severity tags
func tCrit() string { return red("[CRIT]") }
func tWarn() string { return yellow("[WARN]") }
func tOK() string   { return green("[ ok ]") }
func tInfo() string { return cyan("[info]") }

// safeComm strips control bytes from a task name. A process picks its own
// comm (prctl/PR_SET_NAME, or /proc/<pid>/comm) and the kernel stores it
// verbatim, so it can carry ANSI escapes that garble or spoof this tool's
// output. Sanitize on the way in, so no printer has to remember to.
func safeComm(s string) string {
	return strings.Map(func(r rune) rune {
		if r < 0x20 || r == 0x7f || r == '�' {
			return '?' // control byte, DEL, or invalid UTF-8
		}
		return r
	}, s)
}

// section prints a colored section header padded to a fixed width.
func section(title string) {
	line := "── " + title + " "
	for len([]rune(line)) < 76 {
		line += "─"
	}
	fmt.Println(cyan(line))
}
