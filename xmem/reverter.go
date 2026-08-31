package main

import (
	"fmt"
	"os"
	"strings"
	"sync"
)

// reverter records the original contents of kernel control files before the
// program modifies them, and restores everything in reverse order. It is
// wired to a defer in run(), to the signal handler, and (via defer) to
// panics, so the system is returned to its prior state even on a crash.
// SIGKILL cannot be intercepted; every other exit path restores.
type reverter struct {
	mu   sync.Mutex
	acts []revAction
	done bool
}

type revAction struct {
	path string
	orig string
}

func newReverter() *reverter { return &reverter{} }

// saveFile records the current contents of path. Must be called before the
// first write to that file. Returns an error if the original state cannot
// be read (in which case the caller must not modify the file).
func (r *reverter) saveFile(path string) error {
	b, err := os.ReadFile(path)
	if err != nil {
		return err
	}
	r.mu.Lock()
	r.acts = append(r.acts, revAction{path: path, orig: string(b)})
	r.mu.Unlock()
	return nil
}

// restoreAll writes every saved file back in reverse order. Idempotent and
// safe to call concurrently from the signal handler and a defer.
func (r *reverter) restoreAll() {
	r.mu.Lock()
	defer r.mu.Unlock()
	if r.done {
		return
	}
	r.done = true
	for i := len(r.acts) - 1; i >= 0; i-- {
		a := r.acts[i]
		v := strings.TrimSpace(a.orig)
		// Tracefs event filter files read back "none" when unset; clearing
		// one requires writing "0".
		if strings.HasSuffix(a.path, "/filter") && (v == "none" || v == "") {
			v = "0"
		}
		if err := os.WriteFile(a.path, []byte(v), 0o600); err != nil {
			fmt.Fprintf(os.Stderr, "[!] failed to restore %s to %q: %v\n", a.path, v, err)
		}
	}
}
