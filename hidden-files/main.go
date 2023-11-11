package main

import (
	"fmt"
	"log"
	"os"
	"sync"
)

func main() {
	WIPE()
	// WRITE()
	// READ()
}

var DISK = "/dev/sda"

var data [1000000000]byte

func clearCurrentLine() {
	fmt.Print("\n\033[1A\033[K")
}

func WIPE() {
	// Open the device file
	// defer file.Close()
	// stat, err := os.Stat(DISK)
	// fmt.Println(stat, err)
	// os.Exit(1)

	// written := 0

	file, err := os.OpenFile(DISK, os.O_WRONLY, 0o644)
	if err != nil {
		log.Fatalf("Error opening file: %v", err)
	}
	// count := 1
	// WIPEBLOCK:
	// Seek to the position
	var wg sync.WaitGroup
	for i := 0; i < 32; i++ {
		wg.Add(1)
		go func(f os.File, i int) {
			defer wg.Done()
			fmt.Println("SEEKING:", int64(len(data)*i))
			_, err = f.Seek(int64(len(data)*i), 0)
			if err != nil {
				log.Fatalf("Error seeking file: %v", err)
			}
			// Write data
			_, err := f.Write(data[:])
			if err != nil {
				log.Fatalf("Error writing to file: %v", err)
			}
			// file.Sync()
			fmt.Println("W")
		}(*file, i)
	}
	wg.Wait()
	// if written > 1000000000*count {
	// 	fmt.Println("SYNC!")
	// 	count++
	// 	go SYNC(file)
	// 	time.Sleep(2 * time.Second)
	// }

	// goto WIPEBLOCK
}

func SYNC(f *os.File) {
	f.Sync()
}

func WRITE100M(f *os.File) {
}

func WRITE() {
	// Open the device file
	file, err := os.OpenFile(DISK, os.O_WRONLY, 0o644)
	if err != nil {
		log.Fatalf("Error opening file: %v", err)
	}
	defer file.Close()

	// Calculate the byte offset (e.g., sector 10 with 512-byte sectors)
	byteOffset := int64(10 * 512)

	// Seek to the position
	_, err = file.Seek(byteOffset, 0)
	if err != nil {
		log.Fatalf("Error seeking file: %v", err)
	}

	// Data to write
	data := []byte("NiceLand VPN DATA")

	// Write data
	_, err = file.Write(data)
	if err != nil {
		log.Fatalf("Error writing to file: %v", err)
	}
}

func READ() {
	// Open the device file
	file, err := os.OpenFile(DISK, os.O_RDONLY, 0o644)
	if err != nil {
		log.Fatalf("Error opening file: %v", err)
	}
	defer file.Close()

	// Calculate the byte offset (e.g., sector 10 with 512-byte sectors)
	byteOffset := int64(10 * 512)

	// Seek to the position
	_, err = file.Seek(byteOffset, 0)
	if err != nil {
		log.Fatalf("Error seeking file: %v", err)
	}

	// Define buffer to read data into
	buffer := make([]byte, 17) // Size of "NiceLand VPN DATA"

	// Read data
	_, err = file.Read(buffer)
	if err != nil {
		log.Fatalf("Error reading from file: %v", err)
	}

	// Print the data
	log.Printf("Read data: %s\n", buffer)
}
