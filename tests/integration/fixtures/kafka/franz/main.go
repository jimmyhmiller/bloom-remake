// A franz-go client that asks a broker for its metadata (the captures of S6 item 5).
package main

import (
	"context"
	"fmt"
	"os"
	"time"

	"github.com/twmb/franz-go/pkg/kadm"
	"github.com/twmb/franz-go/pkg/kgo"
)

func main() {
	cl, err := kgo.NewClient(kgo.SeedBrokers(os.Args[1]))
	if err != nil {
		panic(err)
	}
	defer cl.Close()
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	adm := kadm.NewClient(cl)
	m, err := adm.Metadata(ctx)
	if err != nil {
		fmt.Println("error:", err)
		os.Exit(1)
	}
	for _, b := range m.Brokers {
		fmt.Printf("broker %d at %s:%d\n", b.NodeID, b.Host, b.Port)
	}
	fmt.Printf("%d topics\n", len(m.Topics))
}
