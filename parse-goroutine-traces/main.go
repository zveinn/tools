package main

import (
	"bytes"
	"fmt"
	"io"
	"io/fs"
	"os"
	"strconv"
	"strings"
)

func main() {
	minArg := os.Args[1]
	maxArg := os.Args[2]
	filter := os.Args[3]

	minCount, _ := strconv.Atoi(minArg)
	maxCount, _ := strconv.Atoi(maxArg)

	fileMap := make(map[string]bool)
	dr := os.DirFS(".")
	fs.WalkDir(dr, ".", func(path string, d fs.DirEntry, err error) error {
		if strings.Contains(path, "goroutines.txt") {
			fileMap[path] = true
			// log.Println(path)
			// log.Println(d)
			// log.Println(err)
		}
		return nil
	})
	fmt.Println("vim-go")

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

	finalOutput := make(map[string][]string)
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

	for i, v := range finalOutput {
		fmt.Println("")
		fmt.Println("")
		fmt.Println("FILE >>> ", i)
		fmt.Println("")
		for _, vv := range v {
			if strings.Contains(vv, filter) {
				fmt.Println("--------------------------------------------------------")
				fmt.Println(vv)
				fmt.Println("--------------------------------------------------------")
			} else {
				fmt.Println(vv)
			}
		}
	}
}
