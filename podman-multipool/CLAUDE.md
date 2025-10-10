# podman-multipool - MinIO Multi-Pool Cluster Management Tool

## Overview
This is a Go-based CLI tool for managing a MinIO multi-pool distributed object storage cluster using Podman containers. It provides commands to start, stop, monitor, and manage a complex multi-pool MinIO deployment for testing and development purposes.

## Architecture

### Cluster Configuration (constants at main.go:31-46)
- **4 pools** with **4 nodes per pool** = **16 total nodes**
- **8 drives per node** = **128 total drives** across the cluster
- **Erasure coding**: EC:3 (3 parity drives per erasure set)
- **Erasure set size**: 8 drives per set
- Network: Custom Podman network named "minio-network"
- Port ranges:
  - API ports: Starting at 9000 (base + pool*100 + node*10)
  - Console ports: Starting at 9500 (base + pool*100 + node*10)

### Key Components

#### Configuration (main.go:49-76)
The `Config` struct holds:
- MinIO container image (default: `quay.io/minio/minio:latest`)
- Root credentials (default: minioadmin/minioadmin)
- Base data directory (default: `/tmp/minio-pools`)
- EOS directory path (hardcoded: `/home/sveinn/code/eos-fork`)
- Local binary flag (whether to use local MinIO binary from EOS directory)

#### MinIOCluster Type (main.go:59-76)
Main orchestration struct with mutex for thread-safe operations.

## Core Functionality

### 1. Cluster Lifecycle Management

#### Start (main.go:558-585)
- Validates local MinIO binary if `USE_LOCAL_BINARY=true`
- Creates Podman network if it doesn't exist
- Creates directory structure for all drives
- Starts all 16 nodes concurrently (goroutines)
- Waits for health checks to pass (up to 60 attempts, 2s each)
- Displays connection information

#### Stop (main.go:325-343)
- Stops all containers concurrently using goroutines and WaitGroup
- Does not remove containers or data

#### Restart (main.go:595-604)
- Stops all containers
- Waits 5 seconds
- Starts all containers
- Waits for health checks

#### Reset (main.go:587-593)
- Performs full cleanup (with user confirmation for data deletion)
- Starts fresh cluster

### 2. Container Management

#### Node Startup (main.go:174-240)
Each node is started with:
- Unique container name: `minio-pool{X}-node{Y}`
- Volume mounts for 8 drives per node with SELinux labels (`:Z`)
- Environment variables for MinIO configuration
- Two modes:
  - **Local binary mode**: Mounts EOS directory, runs Alpine with local `./minio` binary
  - **Container image mode**: Uses official MinIO container image

#### Port Allocation Logic (main.go:104-112)
- API port formula: `9000 + (pool-1)*100 + (node-1)*10`
- Console port formula: `9500 + (pool-1)*100 + (node-1)*10`
- Example: Pool 2, Node 3 → API: 9110, Console: 9610

### 3. Server Command Generation (main.go:164-172)
Generates MinIO server command with all pool endpoints:
```
http://minio-pool1-node{1...4}:9000/data/drive{1...8} \
http://minio-pool2-node{1...4}:9000/data/drive{1...8} \
http://minio-pool3-node{1...4}:9000/data/drive{1...8} \
http://minio-pool4-node{1...4}:9000/data/drive{1...8}
```

### 4. Health Monitoring (main.go:242-284)
- Polls `/minio/health/live` endpoint on all nodes
- Max 60 attempts with 2-second intervals (2-minute timeout)
- HTTP client with 2-second timeout per request
- Progress indicator (dots) while waiting

### 5. Status & Logging

#### Status Display (main.go:362-399)
Shows per-node status:
- Container existence
- Running state
- Health check status (Healthy/Unhealthy/Stopped/Not found)

#### Log Commands (main.go:401-506)
Three modes:
1. **All logs live** (main.go:417-486): Follows all nodes simultaneously with prefixes `[P{pool}N{node}]`
2. **Tail logs** (main.go:488-506): Shows last N lines from all nodes
3. **Single node** (main.go:402-414): Follows specific pool/node logs

### 6. Data Management (main.go:144-162, 286-323)
- Creates nested directory structure: `{base}/pool{X}/node{Y}/drive{Z}`
- Cleanup with user confirmation before data deletion
- All directories created with 0755 permissions

## Commands

```bash
start    # Start all pools and nodes
stop     # Stop all running containers
restart  # Restart all containers
status   # Show status of all nodes
cleanup  # Remove containers and optionally data
reset    # Full cleanup and fresh start
logs all          # Follow all nodes logs live
logs tail [N]     # Show last N lines (default: 50)
logs <pool> <node> # Follow specific node logs
```

## CLI Flags (Updated in main.go:677-828)

The tool now uses Go's `flag` package for proper CLI argument parsing. Flags can be specified before the command:

```bash
podman-multipool [flags] <command> [args]
```

### Available Flags

#### Basic Configuration
- `-image string`: MinIO container image to use (default: `quay.io/minio/minio:latest`)
- `-user string`: MinIO admin username (default: `minioadmin`)
- `-password string`: MinIO admin password (default: `minioadmin`)
- `-data-dir string`: Base directory for data storage (default: `/tmp/minio-pools`)
- `-eos-dir string`: Path to EOS/MinIO source directory (default: `/home/sveinn/code/eos-fork`)
- `-local-binary`: Use local MinIO binary from EOS directory (default: `true`)

#### Cluster Topology Configuration
- `-pools int`: Number of pools in the cluster (default: `4`)
- `-nodes-per-pool int`: Number of nodes per pool (default: `4`)
- `-drives-per-node int`: Number of drives per node (default: `8`)
- `-network string`: Podman network name (default: `minio-network`)

#### Erasure Coding Configuration
- `-erasure-set-drive-count string`: MinIO erasure set drive count (default: `8`)
- `-storage-class string`: MinIO storage class standard (default: `EC:3`)

#### Other
- `-help`: Show help message

### Flag Precedence

1. Command-line flags (highest priority)
2. Environment variables (fallback)
3. Built-in defaults (lowest priority)

### Examples

```bash
# Use custom credentials
podman-multipool -user admin -password secret123 start

# Use different data directory and container image
podman-multipool -data-dir /var/lib/minio -local-binary=false start

# Create a smaller test cluster (2 pools, 2 nodes per pool, 4 drives per node)
podman-multipool -pools 2 -nodes-per-pool 2 -drives-per-node 4 start

# Custom erasure coding settings
podman-multipool -erasure-set-drive-count 4 -storage-class "EC:2" start

# Use custom network name
podman-multipool -network "my-minio-network" start

# Combine multiple flags
podman-multipool -pools 3 -nodes-per-pool 3 -drives-per-node 6 -erasure-set-drive-count 6 -storage-class "EC:2" start

# Show help
podman-multipool -help
```

## Environment Variables (Backward Compatible)

Environment variables still work as fallback values when flags are not specified:

- `MINIO_IMAGE`: Container image to use
- `MINIO_ROOT_USER`: Admin username (default: minioadmin)
- `MINIO_ROOT_PASSWORD`: Admin password (default: minioadmin)
- `BASE_DATA_DIR`: Data storage location (default: `/tmp/minio-pools`)
- `EOS_DIR`: Path to EOS directory (default: `/home/sveinn/code/eos-fork`)
- `USE_LOCAL_BINARY`: Use local MinIO binary from EOS dir (default: true)
- `NETWORK_NAME`: Podman network name (default: `minio-network`)
- `MINIO_ERASURE_SET_DRIVE_COUNT`: Erasure set drive count (default: `8`)
- `MINIO_STORAGE_CLASS_STANDARD`: Storage class standard (default: `EC:3`)

## Notable Implementation Details

### Concurrency
- Node startup uses goroutines without WaitGroup (main.go:575-577)
  - ⚠️ **Potential issue**: No wait for node startup completion before health check
- Stop operations use proper WaitGroup synchronization (main.go:328-342)
- Log following uses goroutines with context cancellation (main.go:417-486)

### Error Handling
- Silent command execution for cleanup operations (main.go:122-124)
- Container existence checks return empty string on success (odd pattern)
- Limited error propagation in some goroutines (main.go:350-357)

### MinIO-Specific Configuration
- Prometheus integration configured (main.go:210-212)
- CI/CD mode enabled
- Custom erasure set drive count: 8
- Storage class: EC:3 (3 parity, 5 data)

### Hardcoded Values
- EOS directory path: `/home/sveinn/code/eos-fork` (main.go:72)
  - ⚠️ **Note**: This is user-specific and should be configurable

### Color-Coded Output
- Uses ANSI color codes for logging (main.go:42-46)
- Green: Info messages
- Yellow: Warnings
- Red: Errors

## Recent Changes

### CLI Flag Implementation (main.go:677-828)
- **Added**: Go's `flag` package for proper CLI argument parsing
- **Changed**: `NewMinIOCluster()` now takes a `Config` parameter instead of reading from environment
- **Added**: `EOS_DIR` environment variable support (previously hardcoded)
- **Added**: `-help` flag for showing usage information
- **Improved**: Better error messages for unknown commands
- **Backward compatible**: Environment variables still work as fallback values

### Configurable Cluster Topology (main.go:50-64, 686-692)
- **Added**: Flags for cluster configuration: `-pools`, `-nodes-per-pool`, `-drives-per-node`
- **Added**: Flag for network name: `-network`
- **Added**: Flags for erasure coding: `-erasure-set-drive-count`, `-storage-class`
- **Changed**: Config struct now includes all topology and erasure coding settings
- **Changed**: All functions updated to use config values instead of hardcoded constants
- **Added**: Helper methods `getTotalNodes()` and `getTotalDrives()` for calculated values
- **Feature**: Allows creating clusters of any size dynamically at runtime

## Potential Issues & Improvements

1. **Race condition in start()**: Goroutines launch all nodes without waiting before health check
2. ~~**Hardcoded EOS path**: Should be environment variable or command-line flag~~ ✅ **FIXED**: Now configurable via `-eos-dir` flag or `EOS_DIR` environment variable
3. **Container existence check pattern**: Returns empty string on success (unintuitive)
4. **No recovery from partial failures**: If some nodes fail to start, no rollback
5. **Signal handling**: Only in showAllLogs(), not in main start operation
6. **No logs retention**: Log commands require containers to exist

## Dependencies
- Podman (container runtime)
- Go standard library only (no external dependencies)
- Network access for health checks (localhost)

## Use Cases
- Development testing of MinIO multi-pool functionality
- Testing erasure coding and data distribution
- Simulating distributed object storage cluster locally
- MinIO feature development (when using local binary mode)
