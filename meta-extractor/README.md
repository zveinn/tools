# meta-extractor

A tool for recursively walking directories and extracting file paths with compression and resume capabilities.

## Features

- Writes one file per directory (first file found in each dir)
- Delta path compression to reduce output size
- Automatic gzip compression when rotating files
- Resume capability for large directory scans
- 200MB per output file limit with automatic rotation

## Usage

### Basic scan

```bash
# Scan current directory, output to current directory
./meta-extractor --dir /path/to/scan --outDir ./output

# Limit to 5 output files
./meta-extractor --dir /path/to/scan --outDir ./output --numFiles 5
```

### Resume from previous scan

```bash
# First run (hits file limit)
./meta-extractor --dir /large/directory --outDir ./output --numFiles 10

# Resume from where we left off
./meta-extractor --dir /large/directory --outDir ./output --numFiles 10 --resume

# Continue resuming until complete
./meta-extractor --dir /large/directory --outDir ./output --numFiles 10 --resume
```

### Inflate (decompress) output files

```bash
# Expand compressed paths back to full paths
./meta-extractor --inflate ./output/out.1.log.gz --output expanded.txt

# Works with uncompressed files too
./meta-extractor --inflate ./output/out.1.log --output expanded.txt
```

## Output

Output files are named `out.1.log.gz`, `out.2.log.gz`, etc. and stored in `--outDir`.

When using `--numFiles`, a `resume.path` file is created in `--outDir` to track progress.

## Cross-compilation

```bash
# ARM 64-bit
GOARCH=arm64 go build -o meta-extractor-arm64 main.go

# x86_64
GOARCH=amd64 go build -o meta-extractor-amd64 main.go

# Linux ARM
GOOS=linux GOARCH=arm64 go build -o meta-extractor-linux-arm64 main.go
```
