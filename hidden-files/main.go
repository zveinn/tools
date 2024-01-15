package main

import (
	"crypto/aes"
	"crypto/cipher"
	"crypto/rand"
	"encoding/base64"
	"encoding/binary"
	"fmt"
	"io"
	"log"
	"os"
	"strconv"
	"strings"
	"sync"
)

var (
	DISK  = "/dev/sda"
	data  [100000000]byte
	FIX   = "FILENAME"
	BLOCK = 1000000

	// META
	META_start uint64 = 0
	META_end   uint64 = 10000000
	// META_END = []byte{255, 255, 255, 0, 0, 0}
)

//   start    end      ????      NL     NAME
// 8 bytes, 8 bytes, 4 bytes,  2 bytes, .......

type META struct {
	// index // file
	Files           map[int]*FILE
	NextFileOffeset uint64
	NextMetaOffset  uint64
}
type FILE struct {
	Start      uint64
	End        uint64
	X          uint32
	Size       uint64
	Name       string
	NameLength uint16
	Data       []byte
}

var M *META

func main() {
	metaB, err := READ_META()
	if err != nil {
		log.Println(err)
	}
	PARSE_META(metaB)

	command := os.Args[1]
	if command == "ls" {
		LS()
	} else if command == "w" {
		_, _ = WRITE_META([]byte(os.Args[3]), os.Args[2])
		_, _ = WRITE([]byte(os.Args[3]))
	} else if command == "d" {
		DELETE(os.Args[2])
		err := DUMP_META()
		if err != nil {
			fmt.Println(err)
			return
		}
	} else if command == "cp" {
		f, err := os.Open(os.Args[2])
		if err != nil {
			fmt.Println(err)
			return
		}
		defer f.Close()
		fb, err := io.ReadAll(f)
		if err != nil {
			fmt.Println(err)
			return
		}

		_, _ = WRITE_META(fb, f.Name())
		_, _ = WRITE(fb)
	} else if command == "cat" {
		CAT(os.Args[2])
	} else if command == "wipe" {
		WIPE(os.Args[2])
	}
}

func LS() {
	fmt.Println("-------------------------------")
	fmt.Println("TOTAL FILES:", len(M.Files))
	fmt.Println("-------------------------------")
	for i := 0; i < len(M.Files); i++ {
		v := M.Files[i]
		fmt.Printf("%d %s \n ---- B(%d) S(%d) E(%d) M(%x)\n", i, v.Name, v.Size, v.Start, v.End, v.X)
	}
}

func TEST_WRITE() {
	key := []byte("098765432109876543210987654321XX")
	data := []byte("MY SECRET KEY!")
	var start int64 = 0
	written := WRITE_ENC(start, data, key)
	out := READ_ENC(start, written, key)
	fmt.Println(string(out))
}

func DELETE(name string) {
	for i, v := range M.Files {
		if v.Name == name {
			fmt.Println("DELETING FILE: ", name)
			w, e := OVERWRITE(int64(META_end+v.Start), int64(META_end+v.End))
			w, e = OVERWRITE(int64(META_end+v.Start), int64(META_end+v.End))
			w, e = OVERWRITE(int64(META_end+v.Start), int64(META_end+v.End))
			delete(M.Files, i)
			fmt.Println(e)
			fmt.Println("DELETED BYTES: ", w)
			return
		}
	}
}

func OVERWRITE(start, end int64) (written int, err error) {
	file, err := os.OpenFile(DISK, os.O_WRONLY, 0o644)
	if err != nil {
		return 0, err
	}
	defer file.Close()
	_, err = file.Seek(start, 0)
	if err != nil {
		return 0, err
	}
	written, err = file.Write(make([]byte, end-start))
	if err != nil {
		return 0, err
	}
	M.NextFileOffeset += uint64(written)
	return
}

func WRITE(data []byte) (written int, err error) {
	file, err := os.OpenFile(DISK, os.O_WRONLY, 0o644)
	if err != nil {
		return 0, err
	}
	defer file.Close()
	_, err = file.Seek(int64(META_end+M.NextFileOffeset), 0)
	if err != nil {
		return 0, err
	}
	written, err = file.Write(data)
	if err != nil {
		return 0, err
	}
	M.NextFileOffeset += uint64(written)
	return
}

func DUMP_META() (err error) {
	file, err := os.OpenFile(DISK, os.O_WRONLY, 0o644)
	if err != nil {
		return err
	}
	defer file.Close()
	_, err = file.Seek(0, 0)
	if err != nil {
		return err
	}

	fullMeta := make([]byte, 0)
	for _, v := range M.Files {
		fullMeta = append(fullMeta, v.Data...)
	}
	wr, err := file.Write(fullMeta)
	if err != nil {
		return err
	}
	fmt.Printf("Wrote %d bytes of META", wr)
	erase := META_end - uint64(wr)
	wr, err = file.Write(make([]byte, erase))
	if err != nil {
		return err
	}
	fmt.Printf("Erased %d bytes from the end of META", wr)

	return
}

func CREATE_FILE_META_SLICE(
	start uint64,
	end uint64,
	name string,
) (fileMeta []byte) {
	fileMeta = make([]byte, 0)
	fileMeta = binary.BigEndian.AppendUint64(
		fileMeta,
		start,
	)
	fileMeta = binary.BigEndian.AppendUint64(
		fileMeta,
		end,
	)
	fileMeta = binary.BigEndian.AppendUint32(
		fileMeta,
		0,
	)
	fileMeta = binary.BigEndian.AppendUint16(
		fileMeta,
		uint16(len(name)),
	)
	fileMeta = append(fileMeta, []byte(name)...)
	return
}

func WRITE_META(data []byte, name string) (written int, err error) {
	fileMeta := CREATE_FILE_META_SLICE(
		M.NextFileOffeset,
		M.NextFileOffeset+uint64(len(data)),
		name,
	)

	file, err := os.OpenFile(DISK, os.O_WRONLY, 0o644)
	if err != nil {
		return 0, err
	}
	defer file.Close()
	_, err = file.Seek(int64(M.NextMetaOffset), 0)
	if err != nil {
		return 0, err
	}
	written, err = file.Write(fileMeta)
	if err != nil {
		return 0, err
	}
	return
}

func CAT(name string) {
	file, err := os.OpenFile(DISK, os.O_RDONLY, 0o644)
	if err != nil {
		fmt.Println(err)
		return
	}
	defer file.Close()

	for _, v := range M.Files {
		if v.Name == name {

			_, err = file.Seek(int64(META_end+v.Start), 0)
			if err != nil {
				fmt.Println(err)
				return
			}
			buffer := make([]byte, v.End-v.Start)
			_, err = file.Read(buffer)
			if err != nil {
				fmt.Println(err)
				return
			}
			// fmt.Println(buffer)
			fmt.Println(string(buffer))
			return
		}
	}
}

func PARSE_META(data []byte) {
	currentIndex := 0
	M = new(META)
	M.Files = make(map[int]*FILE)

	index := 0
ANOTHERONE:
	M.NextMetaOffset = uint64(currentIndex)

	M.Files[index] = new(FILE)
	M.Files[index].Start = binary.BigEndian.Uint64(data[currentIndex : currentIndex+8])
	M.Files[index].End = binary.BigEndian.Uint64(data[currentIndex+8 : currentIndex+8*2])
	M.Files[index].Size = M.Files[index].End - M.Files[index].Start
	M.Files[index].X = binary.BigEndian.Uint32(data[currentIndex+8*2 : currentIndex+8*2+4])
	M.Files[index].NameLength = binary.BigEndian.Uint16(data[currentIndex+8*2+4 : currentIndex+8*2+4+2])
	currentIndex = currentIndex + 8*2 + 4 + 2
	M.Files[index].Name = string(data[currentIndex : currentIndex+int(M.Files[index].NameLength)])

	currentIndex = currentIndex + int(M.Files[index].NameLength)

	M.Files[index].Data = make([]byte, len(data[M.NextMetaOffset:currentIndex]))
	copy(M.Files[index].Data, data[M.NextMetaOffset:currentIndex])

	if M.Files[index].Size == 0 {
		delete(M.Files, index)
		return
	}

	if M.Files[index].End > M.NextFileOffeset {
		M.NextFileOffeset = M.Files[index].End
	}

	index++
	goto ANOTHERONE
}

func READ_META() (out []byte, err error) {
	file, err := os.OpenFile(DISK, os.O_RDONLY, 0o644)
	if err != nil {
		return nil, err
	}
	defer file.Close()
	_, err = file.Seek(int64(META_start), 0)
	if err != nil {
		return nil, err
	}
	out = make([]byte, META_end) // Size of "NiceLand VPN DATA"
	_, err = file.Read(out)
	if err != nil {
		return nil, err
	}
	return
}

func WRITE_ENC(offset int64, data []byte, key []byte) (written int) {
	// Open the device file
	file, err := os.OpenFile(DISK, os.O_WRONLY, 0o644)
	if err != nil {
		log.Fatalf("Error opening file: %v", err)
	}
	defer file.Close()

	// Calculate the byte offset (e.g., sector 10 with 512-byte sectors)

	// Seek to the position
	_, err = file.Seek(offset, 0)
	if err != nil {
		log.Fatalf("Error seeking file: %v", err)
	}

	// Data to write
	enc := Encrypt(data, key)

	// Write data
	written, err = file.Write(enc)
	if err != nil {
		log.Fatalf("Error writing to file: %v", err)
	}
	return
}

func READ_ENC(offset int64, count int, key []byte) (out []byte) { // Open the device file
	file, err := os.OpenFile(DISK, os.O_RDONLY, 0o644)
	if err != nil {
		log.Fatalf("Error opening file: %v", err)
	}
	defer file.Close()

	// Seek to the position
	_, err = file.Seek(offset, 0)
	if err != nil {
		log.Fatalf("Error seeking file: %v", err)
	}

	// Define buffer to read data into
	buffer := make([]byte, count) // Size of "NiceLand VPN DATA"

	// Read data
	_, err = file.Read(buffer)
	if err != nil {
		log.Fatalf("Error reading from file: %v", err)
	}

	out = Decrypt(buffer, key)

	// Print the data
	// log.Printf("Read data: %s\n", buffer)
	return
}

func Encrypt(text, key []byte) []byte {
	block, err := aes.NewCipher(key)
	if err != nil {
		log.Println(err)
		return nil
	}
	b := base64.StdEncoding.EncodeToString(text)
	ciphertext := make([]byte, aes.BlockSize+len(b))
	iv := ciphertext[:aes.BlockSize]
	if _, err := io.ReadFull(rand.Reader, iv); err != nil {
		log.Println(err)
		return nil
	}
	cfb := cipher.NewCFBEncrypter(block, iv)
	cfb.XORKeyStream(ciphertext[aes.BlockSize:], []byte(b))
	return ciphertext
}

func Decrypt(text, key []byte) (out []byte) {
	block, err := aes.NewCipher(key)
	if err != nil {
		log.Println("ENC ERR:", err)
		return nil
	}
	if len(text) < aes.BlockSize {
		// log.Println(string(text))
		// log.Println(string(key))
		log.Println("CYPHER TOO SHORT")
		return nil
	}

	iv := text[:aes.BlockSize]
	text = text[aes.BlockSize:]
	cfb := cipher.NewCFBDecrypter(block, iv)
	out = make([]byte, len(text))
	cfb.XORKeyStream(out, text)
	data, err := base64.StdEncoding.DecodeString(string(out))
	if err != nil {
		log.Println("DATA ERROR", err)
		return nil
	}
	return data
}

func GetKey(bytes []byte, key []byte) string {
	out := Decrypt(bytes, key)
	outs := string(out)
	split := strings.Split(outs, ":")
	return split[1]
}

func StringToBytes(k string) (out []byte) {
	splitK := strings.Split(k, "-")
	for _, b := range splitK {
		bi, err := strconv.Atoi(b)
		if err != nil {
			log.Println("UNABLE TO PARSE KEY", err)
			os.Exit(1)
		}
		out = append(out, byte(bi))
	}

	return
}

func clearCurrentLine() {
	fmt.Print("\n\033[1A\033[K")
}

func WIPE(count string) {
	countInt, _ := strconv.Atoi(count)
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
	for i := 0; i < countInt; i++ {
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

