package main

import (
	"bytes"
	"fmt"
	"io"
	"io/fs"
	"log"
	"os"
	"strconv"
	"strings"
)

func main() {
	find()
}

func offsets() {
	file, _ := os.Open("offsets")
	fb, _ := io.ReadAll(file)
	fs := string(fb)
	fss := strings.Split(fs, "-")
	prev := 0
	for _, v := range fss {
		si, _ := strconv.Atoi(v)
		fmt.Println(si)
		if prev+32768 < si {
			os.Exit(1)
		}

		prev = si
	}
}

func find() {
	dr := os.DirFS(".")
	fs.WalkDir(dr, ".", func(path string, d fs.DirEntry, err error) error {
		// if strings.Contains(path, "goroutines.txt") {
		// log.Println(path)
		f, err := os.Open(path)
		if err != nil {
			defer f.Close()
		}
		fb, _ := io.ReadAll(f)
		index := bytes.Index(fb, []byte{0, 9, 231, 69})
		if index > -1 && strings.Contains(path, "F3") {
			// if index > -1 {
			log.Println("FOUND IT:", path)
			fmt.Println(fb[index : index+300])
		}
		// log.Println(d)
		// log.Println(err)
		// }
		return nil
	})
}
