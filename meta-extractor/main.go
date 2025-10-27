package main

import (
	"flag"
	"fmt"
	"os"
	"path/filepath"
)

func main() {
	// Parse command-line flags
	dir := flag.String("dir", ".", "Directory to walk")
	flag.Parse()

	// Validate directory exists
	if _, err := os.Stat(*dir); os.IsNotExist(err) {
		fmt.Fprintf(os.Stderr, "Error: directory '%s' does not exist\n", *dir)
		os.Exit(1)
	}

	// Walk the directory
	if err := walkDirectory(*dir); err != nil {
		fmt.Fprintf(os.Stderr, "Error walking directory: %v\n", err)
		os.Exit(1)
	}
}

func walkDirectory(root string) error {
	var lastDir string

	return filepath.WalkDir(root, func(path string, d os.DirEntry, err error) error {
		if err != nil {
			fmt.Fprintf(os.Stderr, "Error accessing path %s: %v\n", path, err)
			return err
		}

		if !d.IsDir() {
			dir := filepath.Dir(path)

			if dir != lastDir {
				fmt.Println(path)
				lastDir = dir
			}

		}

		return nil
	})
}
