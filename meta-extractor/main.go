package main

import (
	"errors"
	"flag"
	"fmt"
	"os"
	"path/filepath"
	"strconv"
	"strings"
)

const maxFileSize = 200 * 1024 * 1024 // 200MB in bytes

var ErrMaxSizeReached = errors.New("maximum size limit reached")

type RotatingWriter struct {
	outDir            string
	currentFile       *os.File
	currentSize       int64
	fileNumber        int
	totalBytesWritten int64
	maxSize           int64
}

// parseSize parses human-readable sizes like "1GB", "500MB", "2GB"
func parseSize(sizeStr string) (int64, error) {
	if sizeStr == "" {
		return 0, nil // 0 means no limit
	}

	sizeStr = strings.TrimSpace(strings.ToUpper(sizeStr))

	// Extract number and unit
	var numStr string
	var unit string

	for i, c := range sizeStr {
		if c >= '0' && c <= '9' || c == '.' {
			numStr += string(c)
		} else {
			unit = sizeStr[i:]
			break
		}
	}

	if numStr == "" {
		return 0, fmt.Errorf("invalid size format: %s", sizeStr)
	}

	num, err := strconv.ParseFloat(numStr, 64)
	if err != nil {
		return 0, fmt.Errorf("invalid number in size: %s", numStr)
	}

	var multiplier int64
	switch unit {
	case "B", "":
		multiplier = 1
	case "KB":
		multiplier = 1024
	case "MB":
		multiplier = 1024 * 1024
	case "GB":
		multiplier = 1024 * 1024 * 1024
	case "TB":
		multiplier = 1024 * 1024 * 1024 * 1024
	default:
		return 0, fmt.Errorf("invalid size unit: %s (supported: B, KB, MB, GB, TB)", unit)
	}

	return int64(num * float64(multiplier)), nil
}

func NewRotatingWriter(outDir string, maxSize int64) (*RotatingWriter, error) {
	// Create output directory if it doesn't exist
	if err := os.MkdirAll(outDir, 0755); err != nil {
		return nil, fmt.Errorf("failed to create output directory: %w", err)
	}

	rw := &RotatingWriter{
		outDir:     outDir,
		fileNumber: 1,
		maxSize:    maxSize,
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
	lineSize := int64(len(line) + 1) // +1 for newline

	// Check if we've hit the max size limit
	if rw.maxSize > 0 && rw.totalBytesWritten+lineSize > rw.maxSize {
		return ErrMaxSizeReached
	}

	// Check if we need to rotate
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
	rw.totalBytesWritten += int64(n)
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
	maxSizeStr := flag.String("maxSize", "", "Maximum total size to write (e.g., 1GB, 500MB, 2GB)")
	flag.Parse()

	// Validate directory exists
	if _, err := os.Stat(*dir); os.IsNotExist(err) {
		fmt.Fprintf(os.Stderr, "Error: directory '%s' does not exist\n", *dir)
		os.Exit(1)
	}

	// Parse max size
	maxSize, err := parseSize(*maxSizeStr)
	if err != nil {
		fmt.Fprintf(os.Stderr, "Error parsing maxSize: %v\n", err)
		os.Exit(1)
	}

	// Create rotating writer
	writer, err := NewRotatingWriter(*outDir, maxSize)
	if err != nil {
		fmt.Fprintf(os.Stderr, "Error creating rotating writer: %v\n", err)
		os.Exit(1)
	}
	defer writer.Close()

	// Walk the directory
	if err := walkDirectory(*dir, writer); err != nil {
		if errors.Is(err, ErrMaxSizeReached) {
			fmt.Fprintf(os.Stderr, "Maximum size limit reached. Total bytes written: %d\n", writer.totalBytesWritten)
			os.Exit(0)
		}
		fmt.Fprintf(os.Stderr, "Error walking directory: %v\n", err)
		os.Exit(1)
	}

	fmt.Fprintf(os.Stderr, "Completed. Total bytes written: %d\n", writer.totalBytesWritten)
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
