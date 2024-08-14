package main

import (
	"encoding/json"
	"fmt"
	"io"
	"log"
	"net/http"
)

func setupHttpHandlers() {
	http.HandleFunc("/", func(w http.ResponseWriter, r *http.Request) {
		bb, err := io.ReadAll(r.Body)
		if err != nil {
			fmt.Println(err)
		}
		r.Body.Close()
		// fmt.Println(string(bb))
		var out map[string]interface{}
		err = json.Unmarshal(bb, &out)
		fmt.Println(out)

		w.WriteHeader(200)
		w.Header().Clone()
	})
}

func main() {
	setupHttpHandlers()
	log.Fatal(http.ListenAndServe("172.17.0.1:1111", nil))
}
