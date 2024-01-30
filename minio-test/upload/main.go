package main

import (
	"context"
	"fmt"
	"log"
	"net/http"
	"os"

	"github.com/aws/aws-sdk-go-v2/aws"
	"github.com/aws/aws-sdk-go-v2/config"
	"github.com/aws/aws-sdk-go-v2/service/s3"
	"github.com/aws/aws-sdk-go-v2/service/s3/types"
	"github.com/minio/minio-go/v7"
	"github.com/minio/minio-go/v7/pkg/credentials"
	"github.com/minio/minio-go/v7/pkg/encrypt"
)

func main() {
	// err := makeFile("file3")
	// if err != nil {
	// 	fmt.Println(err)
	// 	return
	// }
	// fmt.Println("File created")

	// uploadFile("BIGFILE", "sveinn", "BIGFILE")
	// getattr("sveinn", "BIGFILE")
	// getattr("sveinn", "F1.txt")

	// uploadFile("file3", "sveinn", "file3")
	// getattr("test", "F2.txt")

	// uploadFileSSE("small-file", "sveinn", "small-file")
	removeFile("sveinn", "small-file")

	// getattr("minio-go-test-luxpklgrbhjrte6t", "file3")
	// awsGetAttr("unknown-bucket2", "F1.txt")
	// tagTest()
}

func removeFile(bucket, prefix string) {
	c, err := minio.New(os.Getenv("endpoint"),
		&minio.Options{
			TrailingHeaders: true,
			Creds:           credentials.NewStaticV4(os.Getenv("key"), os.Getenv("secret"), ""),
			Secure:          true,
			Transport:       createHTTPTransport(),
		})
	if err != nil {
		fmt.Println(err)
		return
	}

	// os.Exit(1)
	// sseOpt := encrypt.DefaultPBKDF([]byte("slkdjfklsd sldkfjlksd flsdfl sjdlf sldf"), []byte(bucket+"/"+prefix)) // replace key

	err = c.RemoveObject(context.Background(), bucket, prefix, minio.RemoveObjectOptions{
		// ServerSideEncryption: sseOpt,
		// DisableMultipart:     false,
		// DisableContentSha256: true,
	})
	if err != nil {
		fmt.Println(err)
		return
	}
}

func uploadFileSSE(path, bucket, prefix string) {
	c, err := minio.New(os.Getenv("endpoint"),
		&minio.Options{
			TrailingHeaders: true,
			Creds:           credentials.NewStaticV4(os.Getenv("key"), os.Getenv("secret"), ""),
			Secure:          true,
			Transport:       createHTTPTransport(),
		})
	if err != nil {
		fmt.Println(err)
		return
	}

	file, err := os.Open(path)
	if err != nil {
		fmt.Println(err)
		return
	}
	stat, err := file.Stat()
	if err != nil {
		fmt.Println(err)
		return
	}

	PR := new(ProgressReader)
	PR.F = file
	PR.S = stat
	PR.TotalSize = stat.Size()

	// os.Exit(1)
	sseOpt := encrypt.DefaultPBKDF([]byte("slkdjfklsd sldkfjlksd flsdfl sjdlf sldf"), []byte(bucket+"/"+prefix)) // replace key
	// sseOpt, err := encrypt.NewSSEKMS("cli-key", nil)
	// if err != nil {
	// 	fmt.Println(err)
	// 	return
	// }

	fmt.Println("Uploading file", stat.Size())
	_, err = c.PutObject(context.Background(), bucket, prefix, PR, stat.Size(), minio.PutObjectOptions{
		ServerSideEncryption: sseOpt,
		// DisableMultipart:     false,
		// DisableContentSha256: true,
		PartSize:       1024 * 1024 * 5,
		SendContentMd5: false,
		ContentType:    "custom/contenttype",
	})
	if err != nil {
		fmt.Println(err)
		return
	}
}

func tagTest() {
	// fmt.Println(os.Getenv("endpoint"))
	// fmt.Println(os.Getenv("key"))
	// fmt.Println(os.Getenv("secret"))
	c, err := minio.New(os.Getenv("endpoint"),
		&minio.Options{
			Creds:     credentials.NewStaticV4(os.Getenv("key"), os.Getenv("secret"), ""),
			Secure:    true,
			Transport: createHTTPTransport(),
		})
	if err != nil {
		fmt.Println(err)
		return
	}

	tags, err := c.GetObjectTagging(context.Background(), "test", "F12.txt", minio.GetObjectTaggingOptions{})
	fmt.Println(tags, err)
}

func getattr(bucket, object string) {
	c, err := minio.New(os.Getenv("endpoint"),
		&minio.Options{
			Creds:     credentials.NewStaticV4(os.Getenv("key"), os.Getenv("secret"), ""),
			Secure:    true,
			Transport: createHTTPTransport(),
		})
	if err != nil {
		fmt.Println(err)
		return
	}

	attr, err := c.GetObjectAttributes(
		context.Background(),
		bucket,
		object,
		minio.ObjectAttributesOptions{
			MaxParts:         0,
			PartNumberMarker: 0,
			// VersionID: "es.r.3C00FGv2wBeFfpB4MpnSl6H9aim",
			// VersionID: "6VEQ.R4lIzG1k.I.GYyfrLC6tcFg34gI",
		},
	)
	fmt.Println(err)
	fmt.Println(attr)
	fmt.Println(attr.ObjectParts.PartsCount)
	fmt.Println(attr.ObjectParts.PartNumberMarker)
	fmt.Println(attr.ObjectParts.NextPartNumberMarker)
	fmt.Println(attr.ObjectParts.MaxParts)
	// fmt.Println(attr.ObjectParts.Parts)
	for i, v := range attr.ObjectParts.Parts {
		fmt.Println(i, v)
	}
}

type ProgressReader struct {
	F         *os.File
	S         os.FileInfo
	ReadTotal int64
	TotalSize int64
}

func (pr *ProgressReader) Read(p []byte) (n int, err error) {
	n, err = pr.F.Read(p)
	pr.ReadTotal += int64(n)
	fmt.Printf("\r%d %d %d", pr.TotalSize/pr.ReadTotal, pr.TotalSize, pr.ReadTotal)
	return
}

func uploadFile(path, bucket, prefix string) {
	c, err := minio.New(os.Getenv("endpoint"),
		&minio.Options{
			TrailingHeaders: true,
			Creds:           credentials.NewStaticV4(os.Getenv("key"), os.Getenv("secret"), ""),
			Secure:          true,
			Transport:       createHTTPTransport(),
		})
	if err != nil {
		fmt.Println(err)
		return
	}

	file, err := os.Open(path)
	if err != nil {
		fmt.Println(err)
		return
	}
	stat, err := file.Stat()
	if err != nil {
		fmt.Println(err)
		return
	}

	PR := new(ProgressReader)
	PR.F = file
	PR.S = stat
	PR.TotalSize = stat.Size()

	// os.Exit(1)

	fmt.Println("Uploading file", stat.Size())
	_, err = c.PutObject(context.Background(), bucket, prefix, PR, stat.Size(), minio.PutObjectOptions{
		// DisableMultipart:     false,
		// DisableContentSha256: true,
		PartSize:       1024 * 1024 * 5,
		SendContentMd5: false,
		ContentType:    "custom/contenttype",
	})
	if err != nil {
		fmt.Println(err)
		return
	}
}

func createHTTPTransport() (transport *http.Transport) {
	var err error
	transport, err = minio.DefaultTransport(true)
	if err != nil {
		fmt.Println(err)
		return nil
	}

	transport.TLSClientConfig.InsecureSkipVerify = true

	return
}

var OneMBSlice [1000010]byte

func makeFile(name string) (err error) {
	newFile, err := os.Create(name)
	if err != nil {
		return err
	}
	defer newFile.Close()

	// md5Writer := md5.New()
	// var tmp [4]byte
	for i := 1; i < 70; i++ {
		// binary.BigEndian.PutUint32(tmp[:], uint32(i))
		// fo := append(tmp[:], FinalSlice...)
		_, err = newFile.Write(OneMBSlice[:])
		if err != nil {
			return err
		}
		// _, err = md5Writer.Write(fo)
		// if err != nil {
		// 	return err
		// }
	}
	// splitName := strings.Split(newFile.Name(), string(os.PathSeparator))
	// fileNameWithoutPath := splitName[len(splitName)-1]
	// md5sum := fmt.Sprintf("%x", md5Writer.Sum(nil))
	// log.Println(md5sum)
	// stats, err := newFile.Stat()
	// if err != nil {
	// 	return
	// }
	return
}

func awsGetAttr(bucket, prefix string) {
	// s.Setenv("AWS_SECRET_ACCESS_KEY", "")
	// os.Setenv("AWS_ACCESS_KEY_ID", "")
	// Load the Shared AWS Configuration (~/.aws/config)
	cfg, err := config.LoadDefaultConfig(context.TODO(), config.WithRegion("us-east-1"))
	// cfg, err := config.Lo(context.TODO())
	if err != nil {
		log.Fatal(err)
	}

	// Create an Amazon S3 service client
	client := s3.NewFromConfig(cfg)

	// Get the first page of results for ListObjectsV2 for a bucket
	// output, err := client.ListObjectsV2(context.TODO(), &s3.ListObjectsV2Input{
	// 	Bucket: aws.String("sveinn"),
	// })
	// if err != nil {
	// 	log.Fatal(err)
	// }
	// log.Println("first page results:")
	// for _, object := range output.Contents {
	// 	log.Printf("key=%s size=%d", aws.ToString(object.Key), object.Size)
	// }
	// fmt.Println("------------------------------")

	// minio-go-test-luxpklgrbhjrte6t/file2
	atr, e2 := client.GetObjectAttributes(context.TODO(), &s3.GetObjectAttributesInput{
		Bucket: aws.String(bucket),
		Key:    aws.String(prefix),
		// Bucket:           aws.String("sveinn"),
		// MaxParts:         aws.Int32(10),
		// PartNumberMarker: aws.String("10"),
		// VersionId:        aws.String("null"),
		ObjectAttributes: []types.ObjectAttributes{"ETag", "Checksum", "ObjectSize", "ObjectParts", "StorageClass"},
	})

	fmt.Println("ERR:", e2)
	fmt.Println("---------- FULL ------------------")
	fmt.Println(atr)
	fmt.Println("-----------------------------")
	fmt.Println(*atr.ETag)
	fmt.Println(atr.Checksum)
	fmt.Println(atr.ObjectParts)
	fmt.Println(atr.ObjectParts.Parts)
	fmt.Println("-----------------------------")
	fmt.Println(atr.VersionId)
	fmt.Println(*atr.ObjectSize)
	fmt.Println(atr.DeleteMarker)
	fmt.Println(atr.LastModified)
	fmt.Println(atr.StorageClass)
	fmt.Println(atr.RequestCharged)
	fmt.Println(atr.ResultMetadata)
	fmt.Println(atr.ResultMetadata.Has("ObjectParts"))
	fmt.Println("-----------------------------")
	for i, v := range atr.ObjectParts.Parts {
		fmt.Println(i, v)
	}
}
