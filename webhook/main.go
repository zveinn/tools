package main

import (
	"fmt"
	"io"
	"log"
	"net/http"
)

func main() {
	// Register the handler function for the "/" route.
	http.HandleFunc("/", handleRequest)

	// Start the HTTP server on port 8888.
	log.Println("Server starting on port 8888...")
	if err := http.ListenAndServe(":8888", nil); err != nil {
		log.Fatalf("Could not start server: %s\n", err)
	}
}

func handleRequest(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodPost {
		http.Error(w, "Only POST method is allowed", http.StatusMethodNotAllowed)
		return
	}

	body, err := io.ReadAll(r.Body)
	if err != nil {
		fmt.Println(err)
		http.Error(w, "Error reading request body", http.StatusInternalServerError)
		return
	}
	fmt.Println(string(body))
	defer r.Body.Close()

	// var records []map[string]any

	// if err := json.Unmarshal(body, &records); err != nil {
	// 	http.Error(w, "Error decoding JSON", http.StatusBadRequest)
	// 	return
	// }

	// for _, record := range records {
	// 	fmt.Printf("  Record: %+v\n", record)
	// }

	w.WriteHeader(http.StatusOK)
}
