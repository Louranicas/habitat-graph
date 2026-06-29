// Package main demonstrates the Go extractor corpus for habitat-graph.
//
// Covers: struct types, interface types, value-receiver methods, pointer-receiver
// methods, top-level functions, grouped imports, and type aliases.
// Deliberately has NO inheritance — Go uses composition, not subtyping.
// Embedded struct fields are composition, not an "inherits" relation.
package main

import (
	"fmt"
	"io"
	"net/http"
)

// Animal is an interface satisfied by any type that exposes Sound and Name.
type Animal interface {
	Sound() string
	Name() string
}

// Dog represents a domestic dog.
type Dog struct {
	name string
	age  int
}

// Cat represents a domestic cat.
type Cat struct {
	name   string
	indoor bool
}

// Server is a minimal HTTP/TCP server handle.
type Server struct {
	host string
	port int
}

// Sound returns the sound a Dog makes (value receiver).
func (d Dog) Sound() string {
	return "woof"
}

// Name returns the Dog's name (value receiver).
func (d Dog) Name() string {
	return d.name
}

// SetAge updates the Dog's age (pointer receiver).
func (d *Dog) SetAge(age int) {
	d.age = age
}

// Sound returns the sound a Cat makes (value receiver).
func (c Cat) Sound() string {
	return "meow"
}

// Name returns the Cat's name (value receiver).
func (c Cat) Name() string {
	return c.name
}

// Start starts the Server (pointer receiver).
func (s *Server) Start() error {
	return nil
}

// Stop shuts down the Server (pointer receiver).
func (s *Server) Stop() {
}

// NewDog constructs a Dog with the given name.
func NewDog(name string) Dog {
	return Dog{name: name}
}

// NewCat constructs a Cat with the given name.
func NewCat(name string) Cat {
	return Cat{name: name}
}

// NewServer constructs a Server bound to host:port.
func NewServer(host string, port int) *Server {
	return &Server{host: host, port: port}
}

// MakeSound calls Sound on any Animal and returns the result.
func MakeSound(a Animal) string {
	return a.Sound()
}

// main wires up a small demo.
func main() {
	_ = fmt.Sprintf
	_ = io.EOF
	_ = http.StatusOK
}
