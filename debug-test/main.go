package main

import "fmt"

func main() {
	meow1()
}

func meow1() {
	meow2()
}

func meow2() {
	xd := 1
	meow := []byte{1, 2, 3, 4, 5}
	meow2 := []byte{1, 2, 3, 4, 5}
	meow3 := []byte{1, 2, 3, 4, 5}
	meow4 := []byte{1, 2, 3, 4, 5}
	fmt.Println(meow, meow4, meow3, meow2, xd)
}
