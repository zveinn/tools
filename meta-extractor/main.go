package main

import (
	"bufio"
	"compress/gzip"
	"errors"
	"flag"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strconv"
	"strings"
)

const maxFileSize = 200 * 1024 * 1024 // 200MB in bytes

var ErrMaxFilesReached = errors.New("maximum number of files reached")

type MaxFilesError struct {
	LastPath string
}

func (e *MaxFilesError) Error() string {
	return fmt.Sprintf("maximum number of files reached, last path: %s", e.LastPath)
}

func (e *MaxFilesError) Is(target error) bool {
	return target == ErrMaxFilesReached
}

type RotatingWriter struct {
	outDir            string
	currentFile       *os.File
	currentFilePath   string
	currentSize       int64
	fileNumber        int
	totalBytesWritten int64
	maxFiles          int
	lastWrittenPath   string
}

// compressFile compresses a file using gzip and removes the original
func compressFile(filePath string) error {
	// Open source file
	srcFile, err := os.Open(filePath)
	if err != nil {
		return fmt.Errorf("failed to open file for compression: %w", err)
	}
	defer srcFile.Close()

	// Create compressed file
	gzPath := filePath + ".gz"
	gzFile, err := os.Create(gzPath)
	if err != nil {
		return fmt.Errorf("failed to create compressed file: %w", err)
	}
	defer gzFile.Close()

	// Create gzip writer
	gzWriter := gzip.NewWriter(gzFile)
	defer gzWriter.Close()

	// Copy data
	if _, err := io.Copy(gzWriter, srcFile); err != nil {
		return fmt.Errorf("failed to compress data: %w", err)
	}

	// Close gzip writer to flush
	if err := gzWriter.Close(); err != nil {
		return fmt.Errorf("failed to close gzip writer: %w", err)
	}

	// Close files before removing
	srcFile.Close()
	gzFile.Close()

	// Remove original file
	if err := os.Remove(filePath); err != nil {
		return fmt.Errorf("failed to remove original file: %w", err)
	}

	return nil
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

// reconstructPath takes a delta path and the last full path, and reconstructs the full path
func reconstructPath(lastPath, deltaPath string) (string, error) {
	// If this is the first path or a relative path without delta marker
	if lastPath == "" || !strings.HasPrefix(deltaPath, "-") {
		// Check if it's a relative path from last directory
		if lastPath != "" && !strings.HasPrefix(deltaPath, "-") {
			lastDir := filepath.Dir(lastPath)
			return filepath.Join(lastDir, deltaPath), nil
		}
		// It's a full path
		return deltaPath, nil
	}

	// Parse delta format: -N:suffix
	colonIdx := strings.Index(deltaPath, ":")
	if colonIdx == -1 {
		return "", fmt.Errorf("invalid delta format: %s", deltaPath)
	}

	levelsUpStr := deltaPath[1:colonIdx] // Skip the '-'
	suffix := deltaPath[colonIdx+1:]

	levelsUp, err := strconv.Atoi(levelsUpStr)
	if err != nil {
		return "", fmt.Errorf("invalid levels in delta: %s", levelsUpStr)
	}

	// Start from last path's directory
	lastDir := filepath.Dir(lastPath)
	parts := strings.Split(lastDir, string(filepath.Separator))

	// Go up N levels
	if levelsUp > len(parts) {
		return "", fmt.Errorf("cannot go up %d levels from %s", levelsUp, lastDir)
	}

	// Remove the last N parts
	parts = parts[:len(parts)-levelsUp]

	// Join with the new suffix
	if len(parts) == 0 {
		return suffix, nil
	}

	newPath := strings.Join(parts, string(filepath.Separator))
	if suffix != "" {
		newPath = filepath.Join(newPath, suffix)
	}

	return newPath, nil
}

// calculateDeltaPath computes a compressed path representation relative to the last path
// Returns the full path for the first file, or a delta like "-1:file2" for subsequent files
func calculateDeltaPath(lastPath, currentPath string) string {
	if lastPath == "" {
		// First path, write it fully
		return currentPath
	}

	// Get directory parts
	lastDir := filepath.Dir(lastPath)
	currentDir := filepath.Dir(currentPath)
	currentFile := filepath.Base(currentPath)

	// Split into parts
	lastParts := strings.Split(lastDir, string(filepath.Separator))
	currentParts := strings.Split(currentDir, string(filepath.Separator))

	// Find common prefix length
	commonLen := 0
	minLen := len(lastParts)
	if len(currentParts) < minLen {
		minLen = len(currentParts)
	}

	for i := 0; i < minLen; i++ {
		if lastParts[i] == currentParts[i] {
			commonLen++
		} else {
			break
		}
	}

	// Calculate how many directories to go up from lastDir
	levelsUp := len(lastParts) - commonLen

	// Build the new suffix (the parts after the common prefix + filename)
	newParts := append([]string{}, currentParts[commonLen:]...)
	newParts = append(newParts, currentFile)
	newSuffix := strings.Join(newParts, string(filepath.Separator))

	if levelsUp == 0 {
		// Going deeper or staying at same level, just write the suffix
		return newSuffix
	}

	// Going up directories, use -N:suffix format
	return fmt.Sprintf("-%d:%s", levelsUp, newSuffix)
}

func NewRotatingWriter(outDir string, maxFiles int, startFileNum int) (*RotatingWriter, error) {
	// Create output directory if it doesn't exist
	if err := os.MkdirAll(outDir, 0755); err != nil {
		return nil, fmt.Errorf("failed to create output directory: %w", err)
	}

	rw := &RotatingWriter{
		outDir:     outDir,
		fileNumber: startFileNum,
		maxFiles:   maxFiles,
	}

	// Create the first file
	if err := rw.rotate(); err != nil {
		return nil, err
	}

	return rw, nil
}

// findLastFileNumber scans outDir for existing out.N.log or out.N.log.gz files
// and returns the highest N found, or 0 if none exist
func findLastFileNumber(outDir string) (int, error) {
	entries, err := os.ReadDir(outDir)
	if err != nil {
		// If directory doesn't exist, start from 1
		if os.IsNotExist(err) {
			return 0, nil
		}
		return 0, err
	}

	maxNum := 0
	for _, entry := range entries {
		name := entry.Name()
		// Match out.N.log or out.N.log.gz
		if strings.HasPrefix(name, "out.") && (strings.HasSuffix(name, ".log") || strings.HasSuffix(name, ".log.gz")) {
			// Extract number
			var num int
			if strings.HasSuffix(name, ".log.gz") {
				_, err := fmt.Sscanf(name, "out.%d.log.gz", &num)
				if err == nil && num > maxNum {
					maxNum = num
				}
			} else if strings.HasSuffix(name, ".log") {
				_, err := fmt.Sscanf(name, "out.%d.log", &num)
				if err == nil && num > maxNum {
					maxNum = num
				}
			}
		}
	}

	return maxNum, nil
}

func (rw *RotatingWriter) rotate() error {
	// Check if we've reached the maximum number of files
	if rw.maxFiles > 0 && rw.fileNumber > rw.maxFiles {
		return ErrMaxFilesReached
	}

	// Close and compress current file if open
	if rw.currentFile != nil {
		if err := rw.currentFile.Close(); err != nil {
			return fmt.Errorf("failed to close current file: %w", err)
		}

		// Compress the file we just closed
		if rw.currentFilePath != "" {
			fmt.Fprintf(os.Stderr, "Compressing %s...\n", rw.currentFilePath)
			if err := compressFile(rw.currentFilePath); err != nil {
				return fmt.Errorf("failed to compress file: %w", err)
			}
			fmt.Fprintf(os.Stderr, "Compressed to %s.gz\n", rw.currentFilePath)
		}
	}

	// Create new file
	filename := filepath.Join(rw.outDir, fmt.Sprintf("out.%d.log", rw.fileNumber))
	file, err := os.Create(filename)
	if err != nil {
		return fmt.Errorf("failed to create file %s: %w", filename, err)
	}

	rw.currentFile = file
	rw.currentFilePath = filename
	rw.currentSize = 0
	rw.fileNumber++

	return nil
}

func (rw *RotatingWriter) WritePath(fullPath string) error {
	// Calculate delta path relative to last written path
	deltaPath := calculateDeltaPath(rw.lastWrittenPath, fullPath)
	lineSize := int64(len(deltaPath) + 1) // +1 for newline

	// Check if we need to rotate
	if rw.currentSize+lineSize > maxFileSize {
		if err := rw.rotate(); err != nil {
			return err
		}
	}

	// Write the delta path
	n, err := fmt.Fprintf(rw.currentFile, "%s\n", deltaPath)
	if err != nil {
		return fmt.Errorf("failed to write to file: %w", err)
	}

	rw.currentSize += int64(n)
	rw.totalBytesWritten += int64(n)
	rw.lastWrittenPath = fullPath // Update last written path
	return nil
}

func (rw *RotatingWriter) Close() error {
	if rw.currentFile != nil {
		// Close the file
		if err := rw.currentFile.Close(); err != nil {
			return err
		}

		// Compress the final file
		if rw.currentFilePath != "" {
			fmt.Fprintf(os.Stderr, "Compressing final file %s...\n", rw.currentFilePath)
			if err := compressFile(rw.currentFilePath); err != nil {
				return fmt.Errorf("failed to compress final file: %w", err)
			}
			fmt.Fprintf(os.Stderr, "Compressed to %s.gz\n", rw.currentFilePath)
		}
	}
	return nil
}

// inflateFile reads a compressed log file and writes expanded paths to output
// Automatically handles both .gz compressed and uncompressed files
func inflateFile(inputPath, outputPath string) error {
	// Open input file
	inputFile, err := os.Open(inputPath)
	if err != nil {
		return fmt.Errorf("failed to open input file: %w", err)
	}
	defer inputFile.Close()

	// Create a reader that handles both gzip and plain files
	var reader io.Reader = inputFile

	// Check if file is gzipped by extension
	if strings.HasSuffix(inputPath, ".gz") {
		gzReader, err := gzip.NewReader(inputFile)
		if err != nil {
			return fmt.Errorf("failed to create gzip reader: %w", err)
		}
		defer gzReader.Close()
		reader = gzReader
	}

	// Create output file
	outputFile, err := os.Create(outputPath)
	if err != nil {
		return fmt.Errorf("failed to create output file: %w", err)
	}
	defer outputFile.Close()

	scanner := bufio.NewScanner(reader)
	writer := bufio.NewWriter(outputFile)
	defer writer.Flush()

	var lastPath string
	lineNum := 0

	for scanner.Scan() {
		lineNum++
		deltaPath := scanner.Text()

		// Reconstruct the full path
		fullPath, err := reconstructPath(lastPath, deltaPath)
		if err != nil {
			return fmt.Errorf("error at line %d: %w", lineNum, err)
		}

		// Write the full path
		if _, err := fmt.Fprintf(writer, "%s\n", fullPath); err != nil {
			return fmt.Errorf("failed to write to output: %w", err)
		}

		lastPath = fullPath
	}

	if err := scanner.Err(); err != nil {
		return fmt.Errorf("error reading input file: %w", err)
	}

	return nil
}

func main() {
	// Parse command-line flags
	dir := flag.String("dir", ".", "Directory to walk")
	outDir := flag.String("outDir", ".", "Output directory for log files")
	numFiles := flag.Int("numFiles", 0, "Maximum number of files to write (0 = unlimited)")
	resume := flag.Bool("resume", false, "Resume from last directory in resume.path")
	inflateInput := flag.String("inflate", "", "Inflate mode: input compressed log file to expand")
	inflateOutput := flag.String("output", "", "Inflate mode: output file for expanded paths")
	flag.Parse()

	// Check if we're in inflate mode
	if *inflateInput != "" {
		if *inflateOutput == "" {
			fmt.Fprintf(os.Stderr, "Error: --output is required when using --inflate\n")
			os.Exit(1)
		}

		fmt.Fprintf(os.Stderr, "Inflating %s to %s...\n", *inflateInput, *inflateOutput)
		if err := inflateFile(*inflateInput, *inflateOutput); err != nil {
			fmt.Fprintf(os.Stderr, "Error inflating file: %v\n", err)
			os.Exit(1)
		}
		fmt.Fprintf(os.Stderr, "Inflation completed successfully.\n")
		return
	}

	// Normal mode: walk directory and create compressed logs
	// Validate directory exists
	if _, err := os.Stat(*dir); os.IsNotExist(err) {
		fmt.Fprintf(os.Stderr, "Error: directory '%s' does not exist\n", *dir)
		os.Exit(1)
	}

	// Check if we're resuming
	var resumePath string
	var startFileNum int = 1
	resumeFilePath := filepath.Join(*outDir, "resume.path")

	if *resume {
		data, err := os.ReadFile(resumeFilePath)
		if err != nil {
			fmt.Fprintf(os.Stderr, "Error reading resume.path: %v\n", err)
			os.Exit(1)
		}
		resumePath = strings.TrimSpace(string(data))
		fmt.Fprintf(os.Stderr, "Resuming from path: %s\n", resumePath)

		// Find the last file number to continue from
		lastNum, err := findLastFileNumber(*outDir)
		if err != nil {
			fmt.Fprintf(os.Stderr, "Error finding last file number: %v\n", err)
			os.Exit(1)
		}
		startFileNum = lastNum + 1
		fmt.Fprintf(os.Stderr, "Continuing from file number: %d\n", startFileNum)
	}

	// Create rotating writer
	writer, err := NewRotatingWriter(*outDir, *numFiles, startFileNum)
	if err != nil {
		fmt.Fprintf(os.Stderr, "Error creating rotating writer: %v\n", err)
		os.Exit(1)
	}
	defer writer.Close()

	// Walk the directory
	if err := walkDirectory(*dir, writer, resumePath); err != nil {
		if errors.Is(err, ErrMaxFilesReached) {
			// Extract the last path from the error
			var maxFilesErr *MaxFilesError
			if errors.As(err, &maxFilesErr) {
				// Write the last path to resume.path
				if writeErr := os.WriteFile(resumeFilePath, []byte(maxFilesErr.LastPath), 0644); writeErr != nil {
					fmt.Fprintf(os.Stderr, "Error writing resume.path: %v\n", writeErr)
				} else {
					fmt.Fprintf(os.Stderr, "Saved resume point to: %s\n", resumeFilePath)
				}
			}
			fmt.Fprintf(os.Stderr, "Maximum file limit reached (%d files). Total bytes written: %d\n", *numFiles, writer.totalBytesWritten)
			os.Exit(0)
		}
		fmt.Fprintf(os.Stderr, "Error walking directory: %v\n", err)
		os.Exit(1)
	}

	fmt.Fprintf(os.Stderr, "Completed. Total bytes written: %d\n", writer.totalBytesWritten)
}

func walkDirectory(root string, writer *RotatingWriter, resumePath string) error {
	var lastDir string
	var currentPath string
	resumeReached := resumePath == "" // If no resume path, start immediately

	err := filepath.WalkDir(root, func(path string, d os.DirEntry, err error) error {
		if err != nil {
			fmt.Fprintf(os.Stderr, "Error accessing path %s: %v\n", path, err)
			return err
		}

		// Track current path for resume
		currentPath = path

		// Check if we've reached the resume point
		if !resumeReached {
			if path == resumePath {
				resumeReached = true
				fmt.Fprintf(os.Stderr, "Resume point reached, continuing from: %s\n", path)
			}
			return nil // Skip until we reach the resume point
		}

		if !d.IsDir() {
			dir := filepath.Dir(path)

			if dir != lastDir {
				if err := writer.WritePath(path); err != nil {
					// If we hit max files, wrap error with current path
					if errors.Is(err, ErrMaxFilesReached) {
						return &MaxFilesError{LastPath: currentPath}
					}
					return fmt.Errorf("failed to write path: %w", err)
				}
				lastDir = dir
			}
		}

		return nil
	})

	// Wrap any ErrMaxFilesReached with the last path we were at
	if err != nil && errors.Is(err, ErrMaxFilesReached) {
		return &MaxFilesError{LastPath: currentPath}
	}

	return err
}
