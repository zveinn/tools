package main

import (
	"flag"
	"fmt"
	"os"
	"path/filepath"
)

const maxFileSize = 200 * 1024 * 1024 // 200MB in bytes

type RotatingWriter struct {
	outDir      string
	currentFile *os.File
	currentSize int64
	fileNumber  int
}

func NewRotatingWriter(outDir string) (*RotatingWriter, error) {
	// Create output directory if it doesn't exist
	if err := os.MkdirAll(outDir, 0755); err != nil {
		return nil, fmt.Errorf("failed to create output directory: %w", err)
	}

	rw := &RotatingWriter{
		outDir:     outDir,
		fileNumber: 1,
	}

	// Create the first file
	if err := rw.rotate(); err != nil {
		return nil, err
	}

	return rw, nil
}

func (rw *RotatingWriter) rotate() error {
	// Close current file if open
	if rw.currentFile != nil {
		if err := rw.currentFile.Close(); err != nil {
			return fmt.Errorf("failed to close current file: %w", err)
		}
	}

	// Create new file
	filename := filepath.Join(rw.outDir, fmt.Sprintf("out.%d.log", rw.fileNumber))
	file, err := os.Create(filename)
	if err != nil {
		return fmt.Errorf("failed to create file %s: %w", filename, err)
	}

	rw.currentFile = file
	rw.currentSize = 0
	rw.fileNumber++

	return nil
}

func (rw *RotatingWriter) Write(line string) error {
	// Check if we need to rotate
	lineSize := int64(len(line) + 1) // +1 for newline
	if rw.currentSize+lineSize > maxFileSize {
		if err := rw.rotate(); err != nil {
			return err
		}
	}

	// Write the line
	n, err := fmt.Fprintf(rw.currentFile, "%s\n", line)
	if err != nil {
		return fmt.Errorf("failed to write to file: %w", err)
	}

	rw.currentSize += int64(n)
	return nil
}

func (rw *RotatingWriter) Close() error {
	if rw.currentFile != nil {
		return rw.currentFile.Close()
	}
	return nil
}

func main() {
	// Parse command-line flags
	dir := flag.String("dir", ".", "Directory to walk")
	outDir := flag.String("outDir", ".", "Output directory for log files")
	flag.Parse()

	// Validate directory exists
	if _, err := os.Stat(*dir); os.IsNotExist(err) {
		fmt.Fprintf(os.Stderr, "Error: directory '%s' does not exist\n", *dir)
		os.Exit(1)
	}

	// Create rotating writer
	writer, err := NewRotatingWriter(*outDir)
	if err != nil {
		fmt.Fprintf(os.Stderr, "Error creating rotating writer: %v\n", err)
		os.Exit(1)
	}
	defer writer.Close()

	// Walk the directory
	if err := walkDirectory(*dir, writer); err != nil {
		fmt.Fprintf(os.Stderr, "Error walking directory: %v\n", err)
		os.Exit(1)
	}
}

func walkDirectory(root string, writer *RotatingWriter) error {
	var lastDir string

	return filepath.WalkDir(root, func(path string, d os.DirEntry, err error) error {
		if err != nil {
			fmt.Fprintf(os.Stderr, "Error accessing path %s: %v\n", path, err)
			return err
		}

		if !d.IsDir() {
			dir := filepath.Dir(path)

			if dir != lastDir {
				if err := writer.Write(path); err != nil {
					return fmt.Errorf("failed to write path: %w", err)
				}
				lastDir = dir
			}
		}

		return nil
	})
}
