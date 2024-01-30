package main

import (
	"context"
	"fmt"
	"log"

	"github.com/aws/aws-sdk-go-v2/aws"
	"github.com/aws/aws-sdk-go-v2/config"
	"github.com/aws/aws-sdk-go-v2/service/s3"
	"github.com/aws/aws-sdk-go-v2/service/s3/types"
)

func main() {
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
	output, err := client.ListObjectsV2(context.TODO(), &s3.ListObjectsV2Input{
		Bucket: aws.String("sveinn-test"),
	})
	if err != nil {
		log.Fatal(err)
	}
	log.Println("first page results:")
	for _, object := range output.Contents {
		log.Printf("key=%s size=%d", aws.ToString(object.Key), object.Size)
	}
	fmt.Println("------------------------------")

	atr, e2 := client.GetObjectAttributes(context.TODO(), &s3.GetObjectAttributesInput{
		Bucket: aws.String("sveinn-test"),
		// MaxParts:         aws.Int32(10),
		// PartNumberMarker: aws.String("10"),
		// VersionId:        aws.String("null"),
		Key:              aws.String("test-object-large"),
		ObjectAttributes: []types.ObjectAttributes{"ETag", "Checksum", "ObjectSize", "ObjectParts"},
	})

	fmt.Println("ERR:", e2)
	fmt.Println("---------- FULL ------------------")
	fmt.Println(atr)
	fmt.Println("-----------------------------")
	fmt.Println(*atr.ETag)
	fmt.Println(atr.Checksum)
	fmt.Println(atr.ObjectParts)
	fmt.Println(atr.VersionId)
	fmt.Println(*atr.ObjectSize)
	fmt.Println(atr.DeleteMarker)
	fmt.Println(atr.LastModified)
	fmt.Println(atr.StorageClass)
	fmt.Println(atr.RequestCharged)
	fmt.Println(atr.ResultMetadata)
	fmt.Println(atr.ResultMetadata.Has("ObjectParts"))
	// for i, v := range atr.ObjectParts.Parts {
	// 	fmt.Println(i, v)
	// }
}
