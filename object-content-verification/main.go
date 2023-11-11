tackage main

import (
	"bytes"
	"crypto/md5"
	"encoding/binary"
	"fmt"
	"io"
	"log"
	"os"
)

var (
	OneMBSlice [200]byte
	FinalSlice []byte
)

func verify() {
	f, err := os.Open(os.Args[2])
	log.Println(err)
	fa, err := io.ReadAll(f)
	log.Println(err)
	lines := bytes.Split(fa, []byte{255, 255, 255})
	var prevNumber uint32 = 0
	prevLen := 0
	for i, v := range lines {
		number := binary.BigEndian.Uint32(v[0:4])

		if number != prevNumber+1 {
			next := binary.BigEndian.Uint32(lines[i+1][0:4])
			// fmt.Println("OOF| line(", i, ") previousLen(", prevLen, "", len(v), ">>>", prevNumber, number, next)
			fmt.Printf("OOF| line(%d) prevLen(%d) len(%d) || prevID(%d) ID(%d) nextID(%d) \n", i, prevLen, len(v), prevNumber, number, next)
			fmt.Println("____ PREV LINE ____")
			fmt.Println(lines[i-1])
		}
		prevNumber = number
		// log.Println(number, len(v))
		if i >= len(lines)-1 {
			log.Println("LAST LINE: ", i)
		}
		if len(v) != 204 {
			// log.Println("FILE CORRUPT", i, len(v))
			// fmt.Println(lines[i])
			// fmt.Println(fa[i*203-400 : i*203+400])
			// fmt.Println(lines[i])
			// fmt.Println(lines[i+1])
			// os.Exit(1)
		}
		prevLen = len(v)
	}
}

func main() {
	if os.Args[1] == "v" {
		verify()
	} else {

		FinalSlice = append(FinalSlice, OneMBSlice[:]...)
		FinalSlice = append(FinalSlice, []byte{255, 255, 255}...)
		makeFile()
	}
}

func makeFile() {
	newFile, err := os.Create("F1.txt")
	if err != nil {
		log.Println(err)
		return
	}
	md5Writer := md5.New()
	var tmp [4]byte
	for i := 1; i < 6000000; i++ {
		binary.BigEndian.PutUint32(tmp[:], uint32(i))
		fo := append(tmp[:], FinalSlice...)
		_, err := newFile.Write(fo)
		_, merr := md5Writer.Write(fo)
		if err != nil || merr != nil {
			log.Println(err)
			log.Println(merr)
			return
		}
	}
	// splitName := strings.Split(newFile.Name(), string(os.PathSeparator))
	// fileNameWithoutPath := splitName[len(splitName)-1]
	md5sum := fmt.Sprintf("%x", md5Writer.Sum(nil))
	log.Println(md5sum)
	// stats, err := newFile.Stat()
	// if err != nil {
	// 	return
	// }
	return
}
