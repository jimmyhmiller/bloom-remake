// A franz-go client for the Blossom broker's gate.
//
//	franzcap <broker>                  asks for the metadata (the captures of S6 item 5)
//	franzcap produce-consume <brokers> [rf]
//	                                   creates a topic (with `rf` replicas, 1 by default), produces records
//	                                   (idempotent, franz-go's default), and reads every partition back from its
//	                                   start, checking each record is there once, in order, at the offset its produce
//	                                   was acknowledged at; `brokers` is a comma-separated list of seed brokers
//	franzcap group <brokers>           creates a topic replicated three times and produces records; two consumers in
//	                                   one group (franz-go's group consumer) read them together, rebalancing as the
//	                                   second joins, and commit; a third in the same group then gets only records
//	                                   produced after (the committed offsets hold)
package main

import (
	"context"
	"fmt"
	"os"
	"strconv"
	"strings"
	"time"

	"github.com/twmb/franz-go/pkg/kadm"
	"github.com/twmb/franz-go/pkg/kgo"
)

func main() {
	if len(os.Args) == 3 && os.Args[1] == "group" {
		group(strings.Split(os.Args[2], ","))
		return
	}
	if (len(os.Args) == 3 || len(os.Args) == 4) && os.Args[1] == "produce-consume" {
		rf := int16(1)
		if len(os.Args) == 4 {
			n, err := strconv.Atoi(os.Args[3])
			if err != nil {
				fail("replication factor", err)
			}
			rf = int16(n)
		}
		produceConsume(strings.Split(os.Args[2], ","), rf)
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

func produceConsume(brokers []string, rf int16) {
	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()

	admCl, err := kgo.NewClient(kgo.SeedBrokers(brokers...))
	if err != nil {
		fail("client", err)
	}
	adm := kadm.NewClient(admCl)
	res, err := adm.CreateTopics(ctx, partitions, rf, nil, topic)
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
	prod, err := kgo.NewClient(kgo.SeedBrokers(brokers...), kgo.RecordPartitioner(kgo.ManualPartitioner()))
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
	cons, err := kgo.NewClient(kgo.SeedBrokers(brokers...), kgo.ConsumePartitions(starts))
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

const groupTopic = "franz-group-topic"
const groupID = "franz-group"

func produceN(ctx context.Context, brokers []string, from, n int) {
	prod, err := kgo.NewClient(kgo.SeedBrokers(brokers...))
	if err != nil {
		fail("producer", err)
	}
	defer prod.Close()
	for i := from; i < from+n; i++ {
		r := &kgo.Record{Topic: groupTopic, Value: []byte(fmt.Sprintf("record %d", i))}
		if err := prod.ProduceSync(ctx, r).FirstErr(); err != nil {
			fail("produce", err)
		}
	}
}

// consume reads records into `seen` until `stop` is closed or the context ends, committing as it goes (franz-go
// autocommits) and on close.
func consume(ctx context.Context, brokers []string, name string, seen chan<- string, stop <-chan struct{}, done chan<- struct{}) {
	cl, err := kgo.NewClient(
		kgo.SeedBrokers(brokers...),
		kgo.ConsumerGroup(groupID),
		kgo.ConsumeTopics(groupTopic),
		kgo.ConsumeResetOffset(kgo.NewOffset().AtStart()),
		kgo.ClientID(name),
	)
	if err != nil {
		fail("consumer", err)
	}
	for {
		select {
		case <-stop:
			if err := cl.CommitUncommittedOffsets(ctx); err != nil {
				fail(name+" commit", err)
			}
			cl.Close()
			done <- struct{}{}
			return
		default:
		}
		pctx, cancel := context.WithTimeout(ctx, 500*time.Millisecond)
		fetches := cl.PollFetches(pctx)
		cancel()
		for _, e := range fetches.Errors() {
			if e.Err != context.DeadlineExceeded && e.Err != context.Canceled {
				fail(name+" fetch", e.Err)
			}
		}
		fetches.EachRecord(func(r *kgo.Record) { seen <- string(r.Value) })
	}
}

func group(brokers []string) {
	ctx, cancel := context.WithTimeout(context.Background(), 120*time.Second)
	defer cancel()
	admCl, err := kgo.NewClient(kgo.SeedBrokers(brokers...))
	if err != nil {
		fail("client", err)
	}
	adm := kadm.NewClient(admCl)
	res, err := adm.CreateTopics(ctx, 3, 3, nil, groupTopic)
	if err != nil {
		fail("create", err)
	}
	for _, r := range res {
		if r.Err != nil {
			fail("create "+r.Topic, r.Err)
		}
	}
	produceN(ctx, brokers, 0, 200)

	seen := make(chan string, 1000)
	stop := make(chan struct{})
	done := make(chan struct{}, 2)
	go consume(ctx, brokers, "first", seen, stop, done)
	got := map[string]bool{}
	// The second member joins once the first is reading: the group rebalances to share the partitions.
	for len(got) < 50 {
		select {
		case v := <-seen:
			got[v] = true
		case <-ctx.Done():
			fail("first reads", ctx.Err())
		}
	}
	go consume(ctx, brokers, "second", seen, stop, done)
	for len(got) < 200 {
		select {
		case v := <-seen:
			got[v] = true
		case <-ctx.Done():
			fail(fmt.Sprintf("group reads (%d of 200)", len(got)), ctx.Err())
		}
	}
	close(stop)
	<-done
	<-done
	described, err := adm.DescribeGroups(ctx, groupID)
	if err != nil {
		fail("describe", err)
	}
	admCl.Close()
	for _, g := range described {
		if g.Err != nil {
			fail("describe "+g.Group, g.Err)
		}
	}

	// A new member reads only what comes after the committed offsets.
	produceN(ctx, brokers, 200, 20)
	seen2 := make(chan string, 1000)
	stop2 := make(chan struct{})
	go consume(ctx, brokers, "third", seen2, stop2, done)
	later := map[string]bool{}
	deadline := time.After(20 * time.Second)
	for len(later) < 20 {
		select {
		case v := <-seen2:
			n := 0
			fmt.Sscanf(v, "record %d", &n)
			if n < 200 {
				fail("committed offsets", fmt.Errorf("the third member read %q, which the group had committed past", v))
			}
			later[v] = true
		case <-deadline:
			fail(fmt.Sprintf("third reads (%d of 20)", len(later)), fmt.Errorf("timed out"))
		}
	}
	close(stop2)
	<-done
	fmt.Printf("ok group %d then %d records\n", len(got), len(later))
}
