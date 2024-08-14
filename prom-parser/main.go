package main

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"log"
	"net/http"
	"os"
	"strings"
	"time"

	"github.com/prometheus/client_golang/api"
	v1 "github.com/prometheus/client_golang/api/prometheus/v1"
	"github.com/prometheus/common/model"
)

var filter = ""

func main() {
	if len(os.Args) > 1 {
		filter = os.Args[1]
	}
	getAll()
}

func getAll() {
	client, err := api.NewClient(api.Config{
		Address: "http://localhost:9090",
	})
	if err != nil {
		log.Fatal(err)
	}

	v1api := v1.NewAPI(client)

	resp, err := http.Get("http://127.0.0.1:9090/api/v1/targets/metadata")
	if err != nil {
		panic(err)
	}

	// fmt.Println(resp)
	bb, err := io.ReadAll(resp.Body)
	// fmt.Println(string(bb))

	x := make(map[string]interface{})
	err = json.Unmarshal(bb, &x)
	if err != nil {
		panic(err)
	}

	// fmt.Println(x["data"])
	for _, v := range x["data"].([]interface{}) {
		xx, ok := v.(map[string]interface{})
		if !ok {
			continue
			// fmt.Println(xx["metric"])
		}

		if !strings.Contains(xx["metric"].(string), "minio") {
			continue
		}
		if filter != "" {
			if !strings.Contains(xx["metric"].(string), filter) {
				continue
			}
		}

		ctx := context.Background()
		value, _, err := v1api.Query(ctx, xx["metric"].(string), time.Now())
		if err != nil {
			log.Fatal(err)
		}

		vector := value.(model.Vector)
		for _, sample := range vector {
			fmt.Printf("%s %s\n", xx["metric"], sample.Value)
			// fmt.Printf("%s %s\n", sample.Metric, sample.Value)
		}
	}
}
