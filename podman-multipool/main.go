// MinIO, Inc. CONFIDENTIAL
//
// [2014] - [2025] MinIO, Inc. All Rights Reserved.
//
// NOTICE:  All information contained herein is, and remains the property
// of MinIO, Inc and its suppliers, if any.  The intellectual and technical
// concepts contained herein are proprietary to MinIO, Inc and its suppliers
// and may be covered by U.S. and Foreign Patents, patents in process, and are
// protected by trade secret or copyright law. Dissemination of this information
// or reproduction of this material is strictly forbidden unless prior written
// permission is obtained from MinIO, Inc.

package main

import (
	"bufio"
	"context"
	"fmt"
	"net/http"
	"os"
	"os/exec"
	"os/signal"
	"path/filepath"
	"strconv"
	"strings"
	"sync"
	"syscall"
	"time"
)

const (
	// Pool and node configuration
	NumPools        = 4
	NodesPerPool    = 4
	DrivesPerNode   = 8
	TotalNodes      = NumPools * NodesPerPool
	TotalDrives     = TotalNodes * DrivesPerNode
	NetworkName     = "minio-network"
	BasePort        = 9000
	ConsoleBasePort = 9500

	// Color codes for output
	ColorRed    = "\033[0;31m"
	ColorGreen  = "\033[0;32m"
	ColorYellow = "\033[1;33m"
	ColorReset  = "\033[0m"
)

// Config holds the configuration for the MinIO cluster
type Config struct {
	MinIOImage        string
	MinIORootUser     string
	MinIORootPassword string
	BaseDataDir       string
	EOSDir            string
	UseLocalBinary    bool
}

// MinIOCluster manages the MinIO multi-pool setup
type MinIOCluster struct {
	config Config
	mu     sync.Mutex
}

// NewMinIOCluster creates a new MinIOCluster instance
func NewMinIOCluster() *MinIOCluster {
	config := Config{
		MinIOImage:        getEnv("MINIO_IMAGE", "quay.io/minio/minio:latest"),
		MinIORootUser:     getEnv("MINIO_ROOT_USER", "minioadmin"),
		MinIORootPassword: getEnv("MINIO_ROOT_PASSWORD", "minioadmin123"),
		BaseDataDir:       getEnv("BASE_DATA_DIR", "/tmp/minio-pools"),
		EOSDir:            "/home/sveinn/code/eos-fork",
		UseLocalBinary:    getEnv("USE_LOCAL_BINARY", "true") == "true",
	}
	return &MinIOCluster{config: config}
}

// Helper functions for colored output
func logInfo(msg string) {
	fmt.Printf("%s[INFO]%s %s\n", ColorGreen, ColorReset, msg)
}

func logWarn(msg string) {
	fmt.Printf("%s[WARN]%s %s\n", ColorYellow, ColorReset, msg)
}

func logError(msg string) {
	fmt.Printf("%s[ERROR]%s %s\n", ColorRed, ColorReset, msg)
}

func getEnv(key, defaultValue string) string {
	value := os.Getenv(key)
	if value == "" {
		return defaultValue
	}
	return value
}

// getContainerName returns the container name for a given pool and node
func getContainerName(pool, node int) string {
	return fmt.Sprintf("minio-pool%d-node%d", pool, node)
}

// getAPIPort returns the API port for a given pool and node
func getAPIPort(pool, node int) int {
	return BasePort + (pool-1)*100 + (node-1)*10
}

// getConsolePort returns the console port for a given pool and node
func getConsolePort(pool, node int) int {
	return ConsoleBasePort + (pool-1)*100 + (node-1)*10
}

// runCommand executes a command and returns its output
func runCommand(command string, args ...string) (string, error) {
	cmd := exec.Command(command, args...)
	output, err := cmd.CombinedOutput()
	return string(output), err
}

// runCommandSilent executes a command silently (ignoring errors)
func runCommandSilent(command string, args ...string) {
	exec.Command(command, args...).Run()
}

// createNetwork creates the podman network if it doesn't exist
func (c *MinIOCluster) createNetwork() error {
	// podman network exists returns exit code 0 if network exists, non-zero if not
	_, err := runCommand("podman", "network", "exists", NetworkName)
	if err != nil {
		// Network doesn't exist, create it
		logInfo(fmt.Sprintf("Creating Podman network: %s", NetworkName))
		if _, err := runCommand("podman", "network", "create", NetworkName); err != nil {
			logError("Failed to create network: " + err.Error())
			return fmt.Errorf("failed to create network: %w", err)
		}
	} else {
		// Network already exists
		logInfo(fmt.Sprintf("Network %s already exists", NetworkName))
	}
	return nil
}

// createDataDirectories creates all required data directories
func (c *MinIOCluster) createDataDirectories() error {
	logInfo("Creating data directories...")
	for pool := 1; pool <= NumPools; pool++ {
		for node := 1; node <= NodesPerPool; node++ {
			for drive := 1; drive <= DrivesPerNode; drive++ {
				dir := filepath.Join(c.config.BaseDataDir,
					fmt.Sprintf("pool%d", pool),
					fmt.Sprintf("node%d", node),
					fmt.Sprintf("drive%d", drive))
				if err := os.MkdirAll(dir, 0o755); err != nil {
					return fmt.Errorf("failed to create directory %s: %w", dir, err)
				}
			}
		}
	}
	logInfo("Data directories created successfully")
	return nil
}

// generateServerCommand generates the MinIO server command with all pools
func (c *MinIOCluster) generateServerCommand() string {
	var pools []string
	for pool := 1; pool <= NumPools; pool++ {
		pools = append(pools, fmt.Sprintf("http://minio-pool%d-node{1...%d}:9000/data/drive{1...%d}",
			pool, NodesPerPool, DrivesPerNode))
	}
	return strings.Join(pools, " ")
}

// startMinIONode starts a single MinIO node
func (c *MinIOCluster) startMinIONode(pool, node int) error {
	containerName := getContainerName(pool, node)
	apiPort := getAPIPort(pool, node)
	consolePort := getConsolePort(pool, node)

	logInfo(fmt.Sprintf("Starting %s (API: %d, Console: %d)", containerName, apiPort, consolePort))

	// Build volume mounts
	var volumeMounts []string
	for drive := 1; drive <= DrivesPerNode; drive++ {
		hostPath := filepath.Join(c.config.BaseDataDir,
			fmt.Sprintf("pool%d", pool),
			fmt.Sprintf("node%d", node),
			fmt.Sprintf("drive%d", drive))
		containerPath := fmt.Sprintf("/data/drive%d", drive)
		volumeMounts = append(volumeMounts, "-v", fmt.Sprintf("%s:%s:Z", hostPath, containerPath))
	}

	// Add EOS directory mount if using local binary
	if c.config.UseLocalBinary {
		volumeMounts = append(volumeMounts, "-v", fmt.Sprintf("%s:/eos:Z", c.config.EOSDir))
	}

	serverCmd := c.generateServerCommand()

	// Build podman run command
	args := []string{
		"run", "-d",
		"--name", containerName,
		"--hostname", containerName,
		"--network", NetworkName,
		"-p", fmt.Sprintf("%d:9000", apiPort),
		"-p", fmt.Sprintf("%d:9001", consolePort),
		"-e", fmt.Sprintf("MINIO_ROOT_USER=%s", c.config.MinIORootUser),
		"-e", fmt.Sprintf("MINIO_ROOT_PASSWORD=%s", c.config.MinIORootPassword),
		"-e", "MINIO_PROMETHEUS_AUTH_TYPE=public",
		"-e", "MINIO_CI_CD=on",
		"-e", "MINIO_PROMETHEUS_URL=http://prometheus:9090",
		"-e", "MINIO_ERASURE_SET_DRIVE_COUNT=8",
		"-e", "MINIO_STORAGE_CLASS_STANDARD=EC:3",
	}

	args = append(args, volumeMounts...)

	if c.config.UseLocalBinary {
		// Use alpine base image and run local binary
		args = append(args,
			"--workdir", "/eos",
			"docker.io/library/alpine:latest",
			"sh", "-c",
			fmt.Sprintf("cd /eos && ./minio server %s --console-address ':9001'", serverCmd))
	} else {
		// Use standard MinIO container image
		args = append(args,
			c.config.MinIOImage,
			"server", serverCmd, "--console-address", ":9001")
	}

	if _, err := runCommand("podman", args...); err != nil {
		logError(fmt.Sprintf("Failed to start %s", containerName))
		return err
	}

	logInfo(fmt.Sprintf("%s started successfully", containerName))
	return nil
}

// waitForHealth waits for all nodes to become healthy
func (c *MinIOCluster) waitForHealth() error {
	logInfo("Waiting for all nodes to become healthy...")
	maxAttempts := 60
	client := &http.Client{Timeout: 2 * time.Second}

	for attempt := 0; attempt < maxAttempts; attempt++ {
		allHealthy := true

		for pool := 1; pool <= NumPools; pool++ {
			for node := 1; node <= NodesPerPool; node++ {
				apiPort := getAPIPort(pool, node)
				url := fmt.Sprintf("http://localhost:%d/minio/health/live", apiPort)

				resp, err := client.Get(url)
				if err != nil || resp.StatusCode != http.StatusOK {
					allHealthy = false
					if resp != nil {
						resp.Body.Close()
					}
					break
				}
				resp.Body.Close()
			}
			if !allHealthy {
				break
			}
		}

		if allHealthy {
			fmt.Println()
			logInfo("All nodes are healthy!")
			return nil
		}

		fmt.Print(".")
		time.Sleep(2 * time.Second)
	}

	fmt.Println()
	logError("Timeout waiting for nodes to become healthy")
	return fmt.Errorf("health check timeout")
}

// cleanup removes existing containers and optionally data
func (c *MinIOCluster) cleanup() error {
	logWarn("Cleaning up existing MinIO containers and volumes...")

	// Stop and remove containers
	for pool := 1; pool <= NumPools; pool++ {
		for node := 1; node <= NodesPerPool; node++ {
			containerName := getContainerName(pool, node)

			// Check if container exists
			if output, _ := runCommand("podman", "container", "exists", containerName); strings.TrimSpace(output) == "" {
				logInfo(fmt.Sprintf("Stopping and removing container: %s", containerName))
				runCommandSilent("podman", "stop", containerName)
				runCommandSilent("podman", "rm", "-f", containerName)
			}
		}
	}

	// Clean up data directories
	if _, err := os.Stat(c.config.BaseDataDir); err == nil {
		logWarn(fmt.Sprintf("Removing existing data directory: %s", c.config.BaseDataDir))
		fmt.Print("Are you sure you want to remove all data? (y/N) ")

		reader := bufio.NewReader(os.Stdin)
		reply, _ := reader.ReadString('\n')
		reply = strings.TrimSpace(strings.ToLower(reply))

		if reply == "y" || reply == "yes" {
			if err := os.RemoveAll(c.config.BaseDataDir); err != nil {
				return fmt.Errorf("failed to remove data directory: %w", err)
			}
		} else {
			logInfo("Keeping existing data")
		}
	}

	return nil
}

// stopAll stops all containers
func (c *MinIOCluster) stopAll() {
	logInfo("Stopping all MinIO containers...")
	for pool := 1; pool <= NumPools; pool++ {
		for node := 1; node <= NodesPerPool; node++ {
			containerName := getContainerName(pool, node)
			if output, _ := runCommand("podman", "container", "exists", containerName); strings.TrimSpace(output) == "" {
				logInfo(fmt.Sprintf("Stopping %s", containerName))
				runCommandSilent("podman", "stop", containerName)
			}
		}
	}
}

// startAll starts all existing containers
func (c *MinIOCluster) startAll() error {
	logInfo("Starting all MinIO containers...")
	for pool := 1; pool <= NumPools; pool++ {
		for node := 1; node <= NodesPerPool; node++ {
			containerName := getContainerName(pool, node)
			if output, _ := runCommand("podman", "container", "exists", containerName); strings.TrimSpace(output) == "" {
				logInfo(fmt.Sprintf("Starting %s", containerName))
				runCommandSilent("podman", "start", containerName)
			}
		}
	}
	return c.waitForHealth()
}

// showStatus displays the status of all nodes
func (c *MinIOCluster) showStatus() {
	fmt.Println("\nMinIO Pool Status:")
	fmt.Println("==================")

	client := &http.Client{Timeout: 2 * time.Second}

	for pool := 1; pool <= NumPools; pool++ {
		fmt.Printf("Pool %d:\n", pool)
		for node := 1; node <= NodesPerPool; node++ {
			containerName := getContainerName(pool, node)
			apiPort := getAPIPort(pool, node)

			if output, _ := runCommand("podman", "container", "exists", containerName); strings.TrimSpace(output) == "" {
				// Get container status
				statusOutput, err := runCommand("podman", "ps", "--filter", fmt.Sprintf("name=%s", containerName), "--format", "{{.Status}}")
				if err == nil && strings.TrimSpace(statusOutput) != "" {
					// Check health
					url := fmt.Sprintf("http://localhost:%d/minio/health/live", apiPort)
					resp, err := client.Get(url)
					if err == nil && resp.StatusCode == http.StatusOK {
						fmt.Printf("  %s: Running and Healthy\n", containerName)
					} else {
						fmt.Printf("  %s: Running but Unhealthy\n", containerName)
					}
					if resp != nil {
						resp.Body.Close()
					}
				} else {
					fmt.Printf("  %s: Stopped\n", containerName)
				}
			} else {
				fmt.Printf("  %s: Not found\n", containerName)
			}
		}
		fmt.Println()
	}
}

// showLogs shows logs for a specific node
func (c *MinIOCluster) showLogs(pool, node int) error {
	containerName := getContainerName(pool, node)

	if output, _ := runCommand("podman", "container", "exists", containerName); strings.TrimSpace(output) == "" {
		cmd := exec.Command("podman", "logs", "-f", containerName)
		cmd.Stdout = os.Stdout
		cmd.Stderr = os.Stderr
		return cmd.Run()
	}

	logError(fmt.Sprintf("Container %s not found", containerName))
	return fmt.Errorf("container not found")
}

// showAllLogs shows logs from all nodes simultaneously
func (c *MinIOCluster) showAllLogs() error {
	logInfo("Following logs from all nodes simultaneously...")
	logInfo("Press Ctrl+C to stop")
	fmt.Println()

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	// Handle interrupt signal
	sigChan := make(chan os.Signal, 1)
	signal.Notify(sigChan, os.Interrupt, syscall.SIGTERM)
	go func() {
		<-sigChan
		cancel()
	}()

	var wg sync.WaitGroup

	for pool := 1; pool <= NumPools; pool++ {
		for node := 1; node <= NodesPerPool; node++ {
			containerName := getContainerName(pool, node)

			if output, _ := runCommand("podman", "container", "exists", containerName); strings.TrimSpace(output) != "" {
				continue
			}

			wg.Add(1)
			go func(p, n int, name string) {
				defer wg.Done()

				cmd := exec.CommandContext(ctx, "podman", "logs", "-f", name)
				stdout, err := cmd.StdoutPipe()
				if err != nil {
					return
				}
				stderr, err := cmd.StderrPipe()
				if err != nil {
					return
				}

				if err := cmd.Start(); err != nil {
					return
				}

				prefix := fmt.Sprintf("%s[P%dN%d]%s", ColorGreen, p, n, ColorReset)

				// Read stdout
				go func() {
					scanner := bufio.NewScanner(stdout)
					for scanner.Scan() {
						fmt.Printf("%s %s\n", prefix, scanner.Text())
					}
				}()

				// Read stderr
				go func() {
					scanner := bufio.NewScanner(stderr)
					for scanner.Scan() {
						fmt.Printf("%s %s\n", prefix, scanner.Text())
					}
				}()

				cmd.Wait()
			}(pool, node, containerName)
		}
	}

	wg.Wait()
	return nil
}

// showAllLogsTail shows the last N lines from all nodes
func (c *MinIOCluster) showAllLogsTail(lines int) {
	logInfo(fmt.Sprintf("Showing last %d lines from all nodes...", lines))
	fmt.Println()

	for pool := 1; pool <= NumPools; pool++ {
		fmt.Printf("================== Pool %d ==================\n", pool)
		for node := 1; node <= NodesPerPool; node++ {
			containerName := getContainerName(pool, node)

			if output, _ := runCommand("podman", "container", "exists", containerName); strings.TrimSpace(output) == "" {
				fmt.Printf("%s--- %s ---%s\n", ColorGreen, containerName, ColorReset)
				output, _ := runCommand("podman", "logs", "--tail", strconv.Itoa(lines), containerName)
				fmt.Print(output)
				fmt.Println()
			}
		}
	}
}

// displayInfo shows connection information for the cluster
func (c *MinIOCluster) displayInfo() {
	fmt.Println("\n================================================================================")
	fmt.Println("MinIO Multi-Pool Setup Complete!")
	fmt.Println("================================================================================")
	fmt.Println()
	fmt.Printf("Access Credentials:\n")
	fmt.Printf("  Username: %s\n", c.config.MinIORootUser)
	fmt.Printf("  Password: %s\n", c.config.MinIORootPassword)
	fmt.Println()
	fmt.Println("Node Access Points:")
	fmt.Println()

	for pool := 1; pool <= NumPools; pool++ {
		fmt.Printf("Pool %d:\n", pool)
		for node := 1; node <= NodesPerPool; node++ {
			apiPort := getAPIPort(pool, node)
			consolePort := getConsolePort(pool, node)
			fmt.Printf("  Node %d:\n", node)
			fmt.Printf("    API:     http://localhost:%d\n", apiPort)
			fmt.Printf("    Console: http://localhost:%d\n", consolePort)
		}
		fmt.Println()
	}

	fmt.Println("MinIO Client (mc) Configuration:")
	fmt.Printf("  mc alias set minio-pools http://localhost:9000 %s %s\n",
		c.config.MinIORootUser, c.config.MinIORootPassword)
	fmt.Println()
	fmt.Printf("Data Directory: %s\n", c.config.BaseDataDir)
	fmt.Println("================================================================================")
}

// checkLocalBinary checks if the local MinIO binary exists
func (c *MinIOCluster) checkLocalBinary() error {
	if !c.config.UseLocalBinary {
		return nil
	}

	binaryPath := filepath.Join(c.config.EOSDir, "minio")
	if _, err := os.Stat(binaryPath); os.IsNotExist(err) {
		logError(fmt.Sprintf("MinIO binary not found at %s", binaryPath))
		logInfo(fmt.Sprintf("Please build the MinIO binary first with: make all"))
		return fmt.Errorf("local binary not found")
	}

	logInfo(fmt.Sprintf("Using local MinIO binary from %s", binaryPath))
	return nil
}

// start initializes and starts all MinIO nodes
func (c *MinIOCluster) start() error {
	if err := c.checkLocalBinary(); err != nil {
		return err
	}

	if err := c.createNetwork(); err != nil {
		return err
	}

	if err := c.createDataDirectories(); err != nil {
		return err
	}

	// Start all nodes
	for pool := 1; pool <= NumPools; pool++ {
		for node := 1; node <= NodesPerPool; node++ {
			if err := c.startMinIONode(pool, node); err != nil {
				return err
			}
		}
	}

	if err := c.waitForHealth(); err != nil {
		return err
	}

	c.displayInfo()
	return nil
}

// reset performs a complete cleanup and fresh start
func (c *MinIOCluster) reset() error {
	if err := c.cleanup(); err != nil {
		return err
	}
	return c.start()
}

// restart restarts all containers
func (c *MinIOCluster) restart() error {
	c.stopAll()
	time.Sleep(5 * time.Second)
	if err := c.startAll(); err != nil {
		return err
	}
	c.displayInfo()
	return nil
}

func printUsage() {
	fmt.Println("MinIO Multi-Pool Podman Management Tool")
	fmt.Println()
	fmt.Println("Usage: multi-pool-podman {start|stop|restart|status|cleanup|reset|logs}")
	fmt.Println()
	fmt.Println("Commands:")
	fmt.Println("  start    - Start all MinIO pools and nodes")
	fmt.Println("  stop     - Stop all running containers")
	fmt.Println("  restart  - Restart all containers")
	fmt.Println("  status   - Show status of all nodes")
	fmt.Println("  cleanup  - Remove all containers and optionally data")
	fmt.Println("  reset    - Complete cleanup and fresh start")
	fmt.Println("  logs     - Show logs (all nodes or specific node)")
	fmt.Println()
	fmt.Println("Logs sub-commands:")
	fmt.Println("  logs all          - Follow all nodes logs simultaneously (live)")
	fmt.Println("  logs tail [N]     - Show last N lines from all nodes (default: 50)")
	fmt.Println("  logs <pool> <node> - Follow specific node logs (e.g., logs 1 1)")
	fmt.Println()
	fmt.Println("Environment Variables:")
	fmt.Println("  MINIO_ROOT_USER     - MinIO admin username (default: minioadmin)")
	fmt.Println("  MINIO_ROOT_PASSWORD - MinIO admin password (default: minioadmin123)")
	fmt.Println("  BASE_DATA_DIR       - Base directory for data (default: /tmp/minio-pools)")
	fmt.Println("  USE_LOCAL_BINARY    - Use local MinIO binary from EOS directory (default: true)")
	fmt.Println()
	fmt.Println("Configuration:")
	fmt.Printf("  Pools: %d\n", NumPools)
	fmt.Printf("  Nodes per pool: %d\n", NodesPerPool)
	fmt.Printf("  Drives per node: %d\n", DrivesPerNode)
	fmt.Printf("  Total nodes: %d\n", TotalNodes)
	fmt.Printf("  Total drives: %d\n", TotalDrives)
}

func main() {
	if len(os.Args) < 2 {
		printUsage()
		os.Exit(1)
	}

	cluster := NewMinIOCluster()

	switch os.Args[1] {
	case "start":
		if err := cluster.start(); err != nil {
			os.Exit(1)
		}

	case "stop":
		cluster.stopAll()

	case "restart":
		if err := cluster.restart(); err != nil {
			os.Exit(1)
		}

	case "status":
		cluster.showStatus()

	case "cleanup":
		if err := cluster.cleanup(); err != nil {
			os.Exit(1)
		}

	case "reset":
		if err := cluster.reset(); err != nil {
			os.Exit(1)
		}

	case "logs":
		if len(os.Args) < 3 {
			fmt.Println("Error: logs command requires additional arguments")
			fmt.Println()
			fmt.Println("Usage:")
			fmt.Println("  logs all              - Follow all nodes logs simultaneously")
			fmt.Println("  logs tail [lines]     - Show last N lines from all nodes (default: 50)")
			fmt.Println("  logs <pool> <node>    - Follow specific node logs")
			fmt.Println()
			fmt.Println("Examples:")
			fmt.Println("  multi-pool-podman logs all")
			fmt.Println("  multi-pool-podman logs tail 100")
			fmt.Println("  multi-pool-podman logs 1 1")
			os.Exit(1)
		}

		switch os.Args[2] {
		case "all":
			if err := cluster.showAllLogs(); err != nil {
				os.Exit(1)
			}

		case "tail":
			lines := 50
			if len(os.Args) > 3 {
				if n, err := strconv.Atoi(os.Args[3]); err == nil {
					lines = n
				}
			}
			cluster.showAllLogsTail(lines)

		default:
			// Assume it's pool and node numbers
			if len(os.Args) < 4 {
				fmt.Println("Error: logs command requires both pool and node numbers")
				os.Exit(1)
			}

			pool, err1 := strconv.Atoi(os.Args[2])
			node, err2 := strconv.Atoi(os.Args[3])

			if err1 != nil || err2 != nil || pool < 1 || pool > NumPools || node < 1 || node > NodesPerPool {
				fmt.Printf("Error: Invalid pool or node number (pool: 1-%d, node: 1-%d)\n", NumPools, NodesPerPool)
				os.Exit(1)
			}

			if err := cluster.showLogs(pool, node); err != nil {
				os.Exit(1)
			}
		}

	default:
		printUsage()
		os.Exit(1)
	}
}
