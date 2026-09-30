// A franz-go client for the Blossom broker's gate.
//
//	franzcap <broker>                  asks for the metadata (the captures of S6 item 5)
//	franzcap produce-consume <broker>  creates a topic, produces records (idempotent, franz-go's default), and reads
//	                                   every partition back from its start, checking each record is there once, in
//	                                   order, at the offset its produce was acknowledged at
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
	if len(os.Args) == 3 && os.Args[1] == "produce-consume" {
		produceConsume(os.Args[2])
		return
	}
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

func fail(what string, err error) {
	fmt.Printf("error: %s: %v\n", what, err)
	os.Exit(1)
}

const topic = "franz"
const partitions = 3
const records = 300

func produceConsume(broker string) {
	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()

	admCl, err := kgo.NewClient(kgo.SeedBrokers(broker))
	if err != nil {
		fail("client", err)
	}
	adm := kadm.NewClient(admCl)
	res, err := adm.CreateTopics(ctx, partitions, 1, nil, topic)
	if err != nil {
		fail("create", err)
	}
	for _, r := range res {
		if r.Err != nil {
			fail("create "+r.Topic, r.Err)
		}
	}
	admCl.Close()

	// Records to explicit partitions, produced asynchronously (franz-go batches and pipelines them), each one's
	// acknowledged offset kept.
	prod, err := kgo.NewClient(kgo.SeedBrokers(broker), kgo.RecordPartitioner(kgo.ManualPartitioner()))
	if err != nil {
		fail("producer", err)
	}
	type key struct {
		p   int32
		off int64
	}
	acked := map[key]string{}
	results := make(chan *kgo.Record, records)
	for i := 0; i < records; i++ {
		r := &kgo.Record{Topic: topic, Partition: int32(i % partitions), Value: []byte(fmt.Sprintf("record %d", i))}
		prod.Produce(ctx, r, func(r *kgo.Record, err error) {
			if err != nil {
				fail("produce", err)
			}
			results <- r
		})
	}
	for i := 0; i < records; i++ {
		r := <-results
		acked[key{r.Partition, r.Offset}] = string(r.Value)
	}
	prod.Close()

	// Every partition read from its start: each acknowledged record once, at its offset, offsets consecutive.
	starts := map[string]map[int32]kgo.Offset{topic: {}}
	for p := int32(0); p < partitions; p++ {
		starts[topic][p] = kgo.NewOffset().AtStart()
	}
	cons, err := kgo.NewClient(kgo.SeedBrokers(broker), kgo.ConsumePartitions(starts))
	if err != nil {
		fail("consumer", err)
	}
	defer cons.Close()
	next := map[int32]int64{}
	seen := 0
	for seen < records {
		fetches := cons.PollFetches(ctx)
		if errs := fetches.Errors(); len(errs) > 0 {
			fail("fetch", errs[0].Err)
		}
		fetches.EachRecord(func(r *kgo.Record) {
			if r.Offset != next[r.Partition] {
				fail("order", fmt.Errorf("partition %d: offset %d after %d", r.Partition, r.Offset, next[r.Partition]))
			}
			next[r.Partition] = r.Offset + 1
			want, ok := acked[key{r.Partition, r.Offset}]
			if !ok || want != string(r.Value) {
				fail("content", fmt.Errorf("partition %d offset %d: %q, acknowledged %q", r.Partition, r.Offset, r.Value, want))
			}
			seen++
		})
	}
	fmt.Printf("ok %d records\n", seen)
}
