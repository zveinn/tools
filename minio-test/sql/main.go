package main

import (
	"database/sql"
	"fmt"
	"sync"

	_ "github.com/mattn/go-sqlite3"
)

const create string = `
  CREATE TABLE IF NOT EXISTS files (
  id INTEGER NOT NULL PRIMARY KEY,
  time DATETIME NOT NULL,
  description TEXT
  );`

const file string = "files.db"

type Activities struct {
	mu sync.Mutex
	db *sql.DB
}

func main() {
	db, err := sql.Open("sqlite3", file)
	if err != nil {
		fmt.Println(err)
		return
	}
	if _, err := db.Exec(create); err != nil {
		fmt.Println(err)
		return
	}
	res, err := db.Exec("INSERT INTO files VALUES(NULL,?,?);", "file1", "jksjdfkljskldfjklsjdlfkjskldf")
	if err != nil {
		fmt.Println(err)
		return
	}

	var id int64
	if id, err = res.LastInsertId(); err != nil {
		fmt.Println(err)
		return
	}
	fmt.Println(id)
	// return &Activities{
	//  db: db,
	// }, nil
}
