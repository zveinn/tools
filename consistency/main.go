package main

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"log"
	"net/http"
	"os"
	"os/signal"
	"runtime/debug"
	"strconv"
	"strings"
	"syscall"
	"time"

	"github.com/minio/minio-go/v7"
	"github.com/minio/minio-go/v7/pkg/credentials"
)

func CatchSignal() {
	<-quit
	fmt.Println("Quit signal caught, cleaning up and exiting")
	CancelFunc()

	time.Sleep(2 * time.Second)
	if outFilePointer != nil {
		_ = outFilePointer.Sync()
		_ = outFilePointer.Close()
	}
}

// mc ls -r --versions m1 --json --no-color > test.out

var (
	runtime        string
	endpoint       string
	secret         string
	key            string
	inputFile      = "input.json"
	doneFile       = "done.json"
	outFile        = "out.json"
	secure         bool
	outFilePointer *os.File
	client         *minio.Client
	BucketInfo     []minio.BucketInfo
	GlobalContext  = context.Background()
	CancelContext  context.Context
	CancelFunc     context.CancelFunc
	watcher        = make(chan int, 10)
	startFileIndex = 0

	// pipelines
	objectChannel = make(chan minio.ObjectInfo, 500000)
)

type Object struct {
	Status         string    `json:"status"`
	Type           string    `json:"type"`
	LastModified   time.Time `json:"lastModified"`
	Size           int       `json:"size"`
	Key            string    `json:"key"`
	Etag           string    `json:"etag"`
	URL            string    `json:"url"`
	VersionID      string    `json:"versionId"`
	VersionOrdinal int       `json:"versionOrdinal"`
	StorageClass   string    `json:"storageClass"`

	// Custom
	// ...
	Parsed bool `json:"parsed"`
}

var (
	objectMap = make(map[string]*Object)
	doneMap   = make(map[string]*Object)
	quit      = make(chan os.Signal, 1)
	// objectChan = make(chan *Object, 500000)
	// bucketCount = make(map[string]int)
)

func main() {
	CancelContext, CancelFunc = context.WithCancel(GlobalContext)
	signal.Notify(quit, os.Interrupt, syscall.SIGTERM)
	go CatchSignal()

	endpoint = os.Args[1]
	secret = os.Args[2]
	key = os.Args[3]
	if len(os.Args) > 4 {
		intIndex, err := strconv.Atoi(os.Args[4])
		if err != nil {
			panic(err)
		}
		startFileIndex = intIndex
	}

	if strings.Contains(endpoint, "https") {
		secure = true
	}

	err := parseFullList(objectMap, inputFile, startFileIndex)
	if err != nil {
		fmt.Println("error parsing file:", err)
		os.Exit(1)
	}

	err = parseFullList(objectMap, doneFile, 0)
	if err != nil {
		fmt.Println("error parsing file:", err)
		os.Exit(1)
	}

	fileTimePreFix := time.Now().Format("2006-01-02-15-04-05")
	outFilePointer, err = os.OpenFile(
		fileTimePreFix+"finished.json",
		os.O_CREATE|os.O_RDWR,
		0o777,
	)

	if err != nil {
		fmt.Println("error opening or creating out file:", err)
		os.Exit(1)
	}

	err = makeClient()
	if err != nil {
		fmt.Println("error creating minio client:", err)
		os.Exit(1)
	}

	fmt.Println("_____ WILL PARSE THESE FILES ______")
	for i := range objectMap {
		if !objectMap[i].Parsed {
			fmt.Println(objectMap[i])
		}
	}

	readObjectsToCheckConsistency()
}

func listBuckets() (err error) {
	BucketInfo, err = client.ListBuckets(CancelContext)
	if err != nil {
		fmt.Println("error listing buckets", err)
	}
	fmt.Println("Found ", len(BucketInfo), " buckets ...")
	return
}

func parseFullList(fileMap map[string]*Object, path string, startingIndex int) (err error) {
	filePointer, err := os.Open(path)
	if err != nil {
		return
	}
	defer filePointer.Close()

	lineCount := 0
	scanner := bufio.NewScanner(filePointer)
	for scanner.Scan() {
		lineCount++
		if lineCount < startingIndex {
			continue
		}

		time.Sleep(1 * time.Second)
		select {
		case <-CancelContext.Done():
			fmt.Println("Stopping file list parser, was parsing: ", path, " ... stopped on line:", lineCount)
			return errors.New("context canceled")
		default:
		}

		bytes := scanner.Bytes()
		object := new(Object)
		err := json.Unmarshal(bytes, object)
		if err != nil {
			fmt.Println("could not unmarshal line:", err)
			fmt.Println("LINE: ", bytes)
			os.Exit(1)
		}
		if object.Type == "file" {
			fileMap[object.Key+object.VersionID] = object
		}
		fmt.Println(object)
	}

	err = scanner.Err()
	if err != nil {
		fmt.Println("error reading file:", err)
		return
	}
	return
}

func makeClient() (err error) {
	trans, terr := createHTTPTransport()
	if terr != nil {
		fmt.Println(terr)
		err = terr
		return
	}
	finalEnd := strings.TrimPrefix(endpoint, "https://")
	finalEnd = strings.TrimPrefix(finalEnd, "http://")
	client, err = minio.New(finalEnd,
		&minio.Options{
			Creds:     credentials.NewStaticV4(key, secret, ""),
			Secure:    secure,
			Transport: trans,
		})
	if err != nil {
		return
	}
	return
}

func createHTTPTransport() (transport *http.Transport, err error) {
	transport, err = minio.DefaultTransport(secure)
	if err != nil {
		return
	}
	transport.TLSClientConfig.InsecureSkipVerify = true
	return
}

func readObjectsToCheckConsistency() {
	defer func() {
		r := recover()
		if r != nil {
			log.Println(r, string(debug.Stack()))
		}
	}()

	for i := range objectMap {
		if !objectMap[i].Parsed {
			objectChannel <- *objectMap[i]
		}
	}



}
