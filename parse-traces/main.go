package main

import (
	"bytes"
	"flag"
	"fmt"
	"io"
	"io/fs"
	"os"
	"os/exec"
	"strconv"
	"strings"
)

var (
	filter      string
	minCount    int
	maxCount    int
	profileType string
	fileMap     = make(map[string]bool)
)

func main() {
	flag.StringVar(&filter, "filter", "", "filter lines in files")
	flag.StringVar(&profileType, "type", "", "set the profile type: goroutine,mem,cpu...")
	flag.IntVar(&minCount, "min", 0, "set min value")
	flag.IntVar(&maxCount, "max", 0, "set max value")
	flag.Parse()

	fmt.Println(profileType, minCount, maxCount, filter)

	dr := os.DirFS(".")
	fs.WalkDir(dr, ".", func(path string, d fs.DirEntry, err error) error {
		switch profileType {
		case "goroutine":
			if strings.Contains(path, "goroutines.txt") {
				fmt.Println("ADD:", path)
				fileMap[path] = true
			}
		case "cpu":
		case "mem":
			if strings.Contains(path, "mem.pprof") || strings.Contains(path, "mem-before.pprof") {
				fmt.Println("ADD:", path)
				fileMap[path] = true
			}
		}
		return nil
	})

	switch profileType {
	case "goroutine":
		parseGoroutineFiles()
	case "mem":
		parseMemFiles()
	}

	printOutput()
}

func parseMemFiles() {
	for i := range fileMap {
		cmd := exec.Command("go", "tool", "pprof", "-text", "-lines", "-compact_labels", i)
		allBytes, err := cmd.CombinedOutput()
		if err != nil {
			panic(err)
		}

		lines := bytes.Split(allBytes, []byte{10})
		startAppending := false
		appendIndex := 0
		for _, v := range lines {
			if len(v) < 10 {
				continue
			}
			if bytes.Contains(v, []byte("flat%")) {
				startAppending = true
				continue
			}
			if startAppending {
				finalOutput[i] = append(finalOutput[i], string(v))
				appendIndex++
			}
			if appendIndex > 5 {
				appendIndex = 0
				startAppending = false
			}
		}
	}
}

func parseGoroutineFiles() {
	output := make(map[string][]string)

	for i := range fileMap {
		file, _ := os.Open(i)
		allBytes, _ := io.ReadAll(file)
		lines := bytes.Split(allBytes, []byte{10})
		shouldPrint := false
		for _, v := range lines {
			if len(v) < 10 {
				continue
			}
			atIndex := bytes.Index(v, []byte(" @"))
			if atIndex > -1 {
				numberString := string(v[0:atIndex])
				numberInt, _ := strconv.Atoi(numberString)
				// log.Println(numberInt)
				if numberInt > minCount && numberInt < maxCount {
					shouldPrint = true
				} else {
					shouldPrint = false
				}
				// fmt.Println(string(v))
			}
			if shouldPrint {
				output[i] = append(output[i], string(v))
				// fmt.Println("line(", ii, ")", string(v))
			}
		}
	}

	for i, v := range output {
		startOfTrace := 0
		found := false
		for ii, vv := range v {
			if strings.Contains(vv, " @") {
				if found {
					found = false
					finalOutput[i] = append(finalOutput[i], v[startOfTrace:ii]...)
				}
				startOfTrace = ii
			}
			if strings.Contains(vv, filter) {
				found = true
			}
		}
	}
}

var finalOutput = make(map[string][]string)

func printOutput() {
	for i, v := range finalOutput {
		fmt.Println("")
		fmt.Println("")
		fmt.Println("FILE >>> ", i)
		fmt.Println("")
		for _, vv := range v {
			if strings.Contains(vv, filter) && filter != "" {
				fmt.Println("--------------------------------------------------------")
				fmt.Println(vv)
				fmt.Println("--------------------------------------------------------")
			} else {
				fmt.Println(vv)
			}
		}
	}
}
