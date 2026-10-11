// The load client of the Kafka soak (scripts/bench-kafka-soak.sh): Kafka's own Java client (kafka-clients 4.0), the
// same program against every system. A single-file source program:
//
//   java -cp "$KAFKA_HOME/libs/*" Soak.java MODE --bootstrap HOSTS --topics T1,T2,… [--option value]…
//
//   produce  one KafkaProducer (acks=all, idempotent: the client's defaults) writing records of --size bytes round-robin
//            over the topics (no key: the sticky partitioner), for --seconds; unthrottled (--rate -1) or paced at --rate
//            records/s. Each acknowledged record is logged to --out/acked-ID.bin (topic, partition, offset, sequence).
//   consume  one KafkaConsumer in --group reading the topics until the file --stop exists, or (--expect N) until the
//            group has read N records or nothing came for --idle seconds.
//   verify   reads every partition of the topics from its start to its end and checks that each acknowledged record in
//            --out is at the offset its acknowledgement gave, and that no record appears twice.
//   report   per phase of a soak (from --events, the soak's events.log), the throughput and the exact latency percentiles
//            of every client in --dir together, from their histogram logs.
//
// `produce` and `consume` with --hlog FILE also write each second's latency histogram to FILE (HdrHistogram's log
// format, timestamps in seconds since the epoch), which `report` merges over any window.
//
// A record's value starts with a header: MAGIC, the producer's id, its sequence number and the time it was meant to be
// sent (microseconds since the epoch). Paced, that is the time of its slot, not of the send() call, so a stalled client
// or broker shows up as latency rather than as fewer records (no coordinated omission). End-to-end latency is the time a
// consumer reads it less that; producers and consumers run on one machine and share its clock.
//
// Every second a mode prints one CSV line to stdout (the columns are its header line); the last line, starting with
// "#", is the run's summary.
import java.io.*;
import java.nio.ByteBuffer;
import java.nio.file.*;
import java.time.Duration;
import java.time.Instant;
import java.util.*;
import java.util.concurrent.*;
import java.util.concurrent.atomic.*;
import java.util.concurrent.locks.LockSupport;
import org.HdrHistogram.Histogram;
import org.HdrHistogram.HistogramLogReader;
import org.HdrHistogram.HistogramLogWriter;
import org.HdrHistogram.Recorder;
import org.apache.kafka.clients.consumer.*;
import org.apache.kafka.clients.producer.*;
import org.apache.kafka.common.PartitionInfo;
import org.apache.kafka.common.TopicPartition;
import org.apache.kafka.common.serialization.*;

public class Soak {
    static final int MAGIC = 0x534f414b; // "SOAK"
    static final int HEADER = 4 + 4 + 8 + 8;
    static final int ACK_ENTRY = 2 + 4 + 8 + 8; // topic index, partition, offset, sequence

    public static void main(String[] args) throws Exception {
        if (args.length == 0) usage("no mode");
        Map<String, String> o = options(args);
        switch (args[0]) {
            case "produce" -> System.exit(produce(o));
            case "consume" -> System.exit(consume(o));
            case "verify" -> System.exit(verify(o));
            case "report" -> System.exit(report(o));
            default -> usage("unknown mode " + args[0]);
        }
    }

    // ---- produce

    static int produce(Map<String, String> o) throws Exception {
        String bootstrap = need(o, "bootstrap");
        List<String> topics = List.of(need(o, "topics").split(","));
        int id = Integer.parseInt(need(o, "id"));
        long rate = Long.parseLong(o.getOrDefault("rate", "-1"));
        long seconds = Long.parseLong(need(o, "seconds"));
        int size = Integer.parseInt(o.getOrDefault("size", "1024"));
        Path out = Path.of(need(o, "out"));
        if (size < HEADER) usage("--size below the header (" + HEADER + " bytes)");
        Files.createDirectories(out);

        Properties p = new Properties();
        p.put(ProducerConfig.BOOTSTRAP_SERVERS_CONFIG, bootstrap);
        p.put(ProducerConfig.CLIENT_ID_CONFIG, "soak-p" + id);
        p.put(ProducerConfig.ACKS_CONFIG, "all");
        p.put(ProducerConfig.KEY_SERIALIZER_CLASS_CONFIG, ByteArraySerializer.class.getName());
        p.put(ProducerConfig.VALUE_SERIALIZER_CLASS_CONFIG, ByteArraySerializer.class.getName());
        extraProps(o, p);

        byte[] filler = new byte[size - HEADER];
        new Random(id).nextBytes(filler);
        Recorder ackLatency = new Recorder(3);
        Histogram total = new Histogram(3);
        AtomicLong sent = new AtomicLong(), acked = new AtomicLong(), failed = new AtomicLong();
        Map<String, AtomicLong> errors = new ConcurrentHashMap<>();
        DataOutputStream ackLog = new DataOutputStream(new BufferedOutputStream(
            Files.newOutputStream(out.resolve("acked-" + id + ".bin")), 1 << 20));
        Map<String, Short> topicIndex = new HashMap<>();
        for (short i = 0; i < topics.size(); i++) topicIndex.put(topics.get(i), i);

        HistogramLogWriter hlog = histogramLog(o);
        System.out.println("t,sent,acked,failed,ack_p50_ms,ack_p99_ms,ack_p999_ms,ack_max_ms");
        long start = System.nanoTime();
        long wallStart = nowMicros();
        long end = start + seconds * 1_000_000_000L;
        ScheduledExecutorService ticker = Executors.newSingleThreadScheduledExecutor();
        ticker.scheduleAtFixedRate(() -> {
            Histogram h = ackLatency.getIntervalHistogram();
            synchronized (total) { total.add(h); }
            logInterval(hlog, h);
            System.out.printf("%d,%d,%d,%d,%s,%s,%s,%s%n", (System.nanoTime() - start) / 1_000_000_000L,
                sent.get(), acked.get(), failed.get(), ms(h, 50), ms(h, 99), ms(h, 99.9), ms(h, 100));
        }, 1, 1, TimeUnit.SECONDS);

        try (KafkaProducer<byte[], byte[]> producer = new KafkaProducer<>(p)) {
            long interval = rate > 0 ? 1_000_000_000L / rate : 0;
            for (long seq = 0; ; seq++) {
                long now = System.nanoTime();
                if (now >= end) break;
                long slot = now;
                if (rate > 0) {
                    slot = start + seq * interval;
                    if (slot >= end) break;
                    while ((now = System.nanoTime()) < slot) LockSupport.parkNanos(slot - now);
                }
                long meant = wallStart + (slot - start) / 1000;
                String topic = topics.get((int) (seq % topics.size()));
                ByteBuffer v = ByteBuffer.allocate(size);
                v.putInt(MAGIC).putInt(id).putLong(seq).putLong(meant).put(filler);
                final long s = seq;
                producer.send(new ProducerRecord<>(topic, null, v.array()), (md, e) -> {
                    if (e != null) {
                        failed.incrementAndGet();
                        errors.computeIfAbsent(e.getClass().getSimpleName(), k -> new AtomicLong()).incrementAndGet();
                        return;
                    }
                    ackLatency.recordValue(Math.max(0, nowMicros() - meant));
                    synchronized (ackLog) {
                        try {
                            ackLog.writeShort(topicIndex.get(md.topic()));
                            ackLog.writeInt(md.partition());
                            ackLog.writeLong(md.offset());
                            ackLog.writeLong(s);
                        } catch (IOException io) {
                            throw new UncheckedIOException(io);
                        }
                    }
                    acked.incrementAndGet();
                });
                sent.incrementAndGet();
            }
            producer.flush();
        }
        ticker.shutdown();
        ticker.awaitTermination(5, TimeUnit.SECONDS);
        synchronized (ackLog) { ackLog.close(); }
        Histogram last = ackLatency.getIntervalHistogram();
        synchronized (total) { total.add(last); }
        logInterval(hlog, last);
        if (hlog != null) hlog.close();
        double elapsed = (System.nanoTime() - start) / 1e9;
        System.out.printf("# produce id=%d sent=%d acked=%d failed=%d seconds=%.1f acked_per_s=%.0f ack_p50_ms=%s "
                + "ack_p99_ms=%s ack_p999_ms=%s ack_max_ms=%s errors=%s%n", id, sent.get(), acked.get(), failed.get(),
            elapsed, acked.get() / elapsed, ms(total, 50), ms(total, 99), ms(total, 99.9), ms(total, 100), errors);
        return 0;
    }

    // ---- consume

    static int consume(Map<String, String> o) throws Exception {
        String bootstrap = need(o, "bootstrap");
        List<String> topics = List.of(need(o, "topics").split(","));
        String group = need(o, "group");
        String id = need(o, "id");
        Path stop = o.containsKey("stop") ? Path.of(o.get("stop")) : null;
        long expect = Long.parseLong(o.getOrDefault("expect", "-1"));
        long idle = Long.parseLong(o.getOrDefault("idle", "60"));
        if (stop == null && expect < 0) usage("consume needs --stop or --expect");

        Properties p = new Properties();
        p.put(ConsumerConfig.BOOTSTRAP_SERVERS_CONFIG, bootstrap);
        p.put(ConsumerConfig.GROUP_ID_CONFIG, group);
        p.put(ConsumerConfig.CLIENT_ID_CONFIG, "soak-c" + id);
        p.put(ConsumerConfig.AUTO_OFFSET_RESET_CONFIG, "earliest");
        p.put(ConsumerConfig.KEY_DESERIALIZER_CLASS_CONFIG, ByteArrayDeserializer.class.getName());
        p.put(ConsumerConfig.VALUE_DESERIALIZER_CLASS_CONFIG, ByteArrayDeserializer.class.getName());
        extraProps(o, p);

        Recorder e2e = new Recorder(3);
        Histogram total = new Histogram(3);
        long read = 0, foreign = 0, rebalances = 0;
        HistogramLogWriter hlog = histogramLog(o);
        System.out.println("t,read,e2e_p50_ms,e2e_p99_ms,e2e_p999_ms,e2e_max_ms");
        long start = System.nanoTime();
        long nextReport = start + 1_000_000_000L;
        long lastRecord = start;
        AtomicLong assigned = new AtomicLong();
        try (KafkaConsumer<byte[], byte[]> consumer = new KafkaConsumer<>(p)) {
            consumer.subscribe(topics, new ConsumerRebalanceListener() {
                public void onPartitionsRevoked(Collection<TopicPartition> ps) {}
                public void onPartitionsAssigned(Collection<TopicPartition> ps) { assigned.incrementAndGet(); }
            });
            while (true) {
                ConsumerRecords<byte[], byte[]> rs = consumer.poll(Duration.ofMillis(200));
                long now = nowMicros();
                for (ConsumerRecord<byte[], byte[]> r : rs) {
                    ByteBuffer v = ByteBuffer.wrap(r.value());
                    if (r.value().length < HEADER || v.getInt() != MAGIC) { foreign++; continue; }
                    v.getInt();
                    v.getLong();
                    e2e.recordValue(Math.max(0, now - v.getLong()));
                    read++;
                }
                long t = System.nanoTime();
                if (!rs.isEmpty()) lastRecord = t;
                if (t >= nextReport) {
                    Histogram h = e2e.getIntervalHistogram();
                    total.add(h);
                    logInterval(hlog, h);
                    System.out.printf("%d,%d,%s,%s,%s,%s%n", (t - start) / 1_000_000_000L, read, ms(h, 50), ms(h, 99),
                        ms(h, 99.9), ms(h, 100));
                    nextReport += 1_000_000_000L;
                }
                if (stop != null && Files.exists(stop)) break;
                if (expect >= 0 && (read >= expect || t - lastRecord > idle * 1_000_000_000L)) break;
            }
            consumer.commitSync(Duration.ofSeconds(60));
        }
        Histogram last = e2e.getIntervalHistogram();
        total.add(last);
        logInterval(hlog, last);
        if (hlog != null) hlog.close();
        double elapsed = (System.nanoTime() - start) / 1e9;
        double reading = (lastRecord - start) / 1e9;
        System.out.printf("# consume id=%s read=%d foreign=%d assignments=%d seconds=%.1f read_per_s=%.0f e2e_p50_ms=%s "
                + "e2e_p99_ms=%s e2e_p999_ms=%s e2e_max_ms=%s%n", id, read, foreign, assigned.get(), elapsed,
            reading > 0 ? read / reading : 0, ms(total, 50), ms(total, 99), ms(total, 99.9), ms(total, 100));
        return expect >= 0 && read < expect ? 1 : 0;
    }

    // ---- verify

    static int verify(Map<String, String> o) throws Exception {
        String bootstrap = need(o, "bootstrap");
        List<String> topics = List.of(need(o, "topics").split(","));
        Path out = Path.of(need(o, "out"));

        Properties p = new Properties();
        p.put(ConsumerConfig.BOOTSTRAP_SERVERS_CONFIG, bootstrap);
        p.put(ConsumerConfig.CLIENT_ID_CONFIG, "soak-verify");
        p.put(ConsumerConfig.ENABLE_AUTO_COMMIT_CONFIG, "false");
        p.put(ConsumerConfig.KEY_DESERIALIZER_CLASS_CONFIG, ByteArrayDeserializer.class.getName());
        p.put(ConsumerConfig.VALUE_DESERIALIZER_CLASS_CONFIG, ByteArrayDeserializer.class.getName());
        extraProps(o, p);

        // What each partition holds: the (producer, sequence) at each offset from its start, -1 where nothing was read.
        Map<TopicPartition, long[]> held = new HashMap<>();
        Map<TopicPartition, Long> begin;
        Map<Integer, BitSet> seen = new HashMap<>();
        long records = 0, duplicates = 0, foreign = 0;
        long start = System.nanoTime();
        try (KafkaConsumer<byte[], byte[]> consumer = new KafkaConsumer<>(p)) {
            List<TopicPartition> all = new ArrayList<>();
            for (String t : topics) {
                for (PartitionInfo pi : consumer.partitionsFor(t, Duration.ofSeconds(60))) {
                    all.add(new TopicPartition(t, pi.partition()));
                }
            }
            consumer.assign(all);
            consumer.seekToBeginning(all);
            begin = consumer.beginningOffsets(all, Duration.ofSeconds(60));
            Map<TopicPartition, Long> end = consumer.endOffsets(all, Duration.ofSeconds(60));
            Set<TopicPartition> open = new HashSet<>();
            for (TopicPartition tp : all) {
                long n = end.get(tp) - begin.get(tp);
                long[] a = new long[Math.toIntExact(n)];
                Arrays.fill(a, -1);
                held.put(tp, a);
                if (n > 0) open.add(tp);
            }
            long lastProgress = System.nanoTime();
            while (!open.isEmpty()) {
                ConsumerRecords<byte[], byte[]> rs = consumer.poll(Duration.ofMillis(500));
                for (ConsumerRecord<byte[], byte[]> r : rs) {
                    TopicPartition tp = new TopicPartition(r.topic(), r.partition());
                    long at = r.offset() - begin.get(tp);
                    if (at >= held.get(tp).length) continue; // written after the end offsets were taken
                    ByteBuffer v = ByteBuffer.wrap(r.value());
                    if (r.value().length < HEADER || v.getInt() != MAGIC) { foreign++; continue; }
                    int producer = v.getInt();
                    long seq = v.getLong();
                    held.get(tp)[(int) at] = pack(producer, seq);
                    BitSet b = seen.computeIfAbsent(producer, k -> new BitSet());
                    if (b.get(Math.toIntExact(seq))) duplicates++;
                    b.set(Math.toIntExact(seq));
                    records++;
                }
                if (!rs.isEmpty()) lastProgress = System.nanoTime();
                for (Iterator<TopicPartition> it = open.iterator(); it.hasNext(); ) {
                    TopicPartition tp = it.next();
                    if (consumer.position(tp, Duration.ofSeconds(60)) >= end.get(tp)) it.remove();
                }
                if (System.nanoTime() - lastProgress > 120_000_000_000L) {
                    System.out.println("# verify FAILED: no progress for 120 s, partitions left: " + open);
                    return 1;
                }
            }
        }
        double reading = (System.nanoTime() - start) / 1e9;

        long acked = 0, lost = 0, misplaced = 0;
        List<String> examples = new ArrayList<>();
        try (DirectoryStream<Path> logs = Files.newDirectoryStream(out, "acked-*.bin")) {
            for (Path log : logs) {
                String name = log.getFileName().toString();
                int producer = Integer.parseInt(name.substring(6, name.length() - 4));
                BitSet b = seen.getOrDefault(producer, new BitSet());
                try (DataInputStream in = new DataInputStream(new BufferedInputStream(Files.newInputStream(log), 1 << 20))) {
                    while (true) {
                        short topic;
                        try { topic = in.readShort(); } catch (EOFException e) { break; }
                        int partition = in.readInt();
                        long offset = in.readLong();
                        long seq = in.readLong();
                        acked++;
                        TopicPartition tp = new TopicPartition(topics.get(topic), partition);
                        long[] a = held.get(tp);
                        long at = offset - begin.getOrDefault(tp, 0L);
                        if (a != null && at >= 0 && at < a.length && a[(int) at] == pack(producer, seq)) continue;
                        if (b.get(Math.toIntExact(seq))) misplaced++;
                        else lost++;
                        if (examples.size() < 20) {
                            examples.add(String.format("producer %d seq %d acked at %s@%d, found there: %s", producer,
                                seq, tp, offset, a == null || at < 0 || at >= a.length ? "out of range"
                                    : a[(int) at] < 0 ? "nothing" : unpack(a[(int) at])));
                        }
                    }
                }
            }
        }
        long unacknowledged = records - duplicates - (acked - lost - misplaced);
        boolean ok = lost == 0 && misplaced == 0 && duplicates == 0 && foreign == 0;
        for (String e : examples) System.out.println("# " + e);
        System.out.printf("# verify %s acked=%d lost=%d misplaced=%d duplicates=%d present_unacknowledged=%d "
                + "records=%d foreign=%d read_seconds=%.1f%n", ok ? "OK" : "FAILED", acked, lost, misplaced, duplicates,
            unacknowledged, records, foreign, reading);
        return ok ? 0 : 1;
    }

    // ---- report

    /// A phase of the soak: a name and its window, in seconds since the epoch.
    record Window(String name, double from, double to) {}

    static int report(Map<String, String> o) throws Exception {
        Path dir = Path.of(need(o, "dir"));
        // events.log: "<seconds since the soak began> <seconds since the epoch> <what>".
        Map<String, Double> at = new LinkedHashMap<>();
        for (String line : Files.readAllLines(Path.of(need(o, "events")))) {
            String[] f = line.split(" ", 3);
            if (f.length < 3) continue;
            double t = Double.parseDouble(f[1]);
            for (String key : List.of("max:", "max done", "fixed:", "killed broker", "restarted broker", "fixed done")) {
                if (f[2].startsWith(key)) at.putIfAbsent(key, t);
            }
        }
        List<Window> windows = new ArrayList<>();
        if (at.containsKey("max:") && at.containsKey("max done")) {
            windows.add(new Window("max", at.get("max:"), at.get("max done")));
        }
        if (at.containsKey("fixed:") && at.containsKey("killed broker")) {
            windows.add(new Window("fixed, steady", at.get("fixed:") + 30, at.get("killed broker")));
        }
        if (at.containsKey("killed broker") && at.containsKey("restarted broker")) {
            windows.add(new Window("fixed, broker down and back (60 s after)", at.get("killed broker"),
                at.get("restarted broker") + 60));
        }
        if (at.containsKey("restarted broker") && at.containsKey("fixed done")) {
            windows.add(new Window("fixed, after recovery", at.get("restarted broker") + 60, at.get("fixed done")));
        }
        System.out.println("phase,seconds,produce_acked_per_s,ack_p50_ms,ack_p99_ms,ack_p999_ms,ack_max_ms,"
            + "consumed_per_s,e2e_p50_ms,e2e_p99_ms,e2e_p999_ms,e2e_max_ms");
        for (Window w : windows) {
            String producers = w.name().equals("max") ? "producer-max-" : "producer-fixed-";
            Histogram ack = merged(dir, producers, w);
            Histogram e2e = merged(dir, "consumer-live-", w);
            double secs = w.to() - w.from();
            System.out.printf("%s,%.0f,%.0f,%s,%s,%s,%s,%.0f,%s,%s,%s,%s%n", w.name(), secs, ack.getTotalCount() / secs,
                ms(ack, 50), ms(ack, 99), ms(ack, 99.9), ms(ack, 100), e2e.getTotalCount() / secs, ms(e2e, 50),
                ms(e2e, 99), ms(e2e, 99.9), ms(e2e, 100));
        }
        return 0;
    }

    /// The intervals of every histogram log in `dir` named `prefix…` that end inside `w`, added together.
    static Histogram merged(Path dir, String prefix, Window w) throws IOException {
        Histogram sum = new Histogram(3);
        try (DirectoryStream<Path> logs = Files.newDirectoryStream(dir, prefix + "*.hlog")) {
            for (Path log : logs) {
                HistogramLogReader r = new HistogramLogReader(log.toFile());
                while (r.hasNext()) {
                    Histogram h = (Histogram) r.nextIntervalHistogram();
                    if (h == null) break;
                    double end = h.getEndTimeStamp() / 1000.0;
                    if (end > w.from() && end <= w.to()) sum.add(h);
                }
            }
        }
        return sum;
    }

    // ---- helpers

    /// The histogram log --hlog names, if any: each interval with its start and end, in seconds since the epoch.
    static HistogramLogWriter histogramLog(Map<String, String> o) throws IOException {
        if (!o.containsKey("hlog")) return null;
        HistogramLogWriter w = new HistogramLogWriter(new File(o.get("hlog")));
        w.outputLogFormatVersion();
        w.outputStartTime(System.currentTimeMillis());
        w.outputLegend();
        return w;
    }

    static void logInterval(HistogramLogWriter w, Histogram h) {
        if (w == null) return;
        synchronized (w) { w.outputIntervalHistogram(h); }
    }

    static long pack(int producer, long seq) { return ((long) producer << 40) | seq; }

    static String unpack(long v) { return "producer " + (v >>> 40) + " seq " + (v & ((1L << 40) - 1)); }

    static long nowMicros() {
        Instant t = Instant.now();
        return t.getEpochSecond() * 1_000_000L + t.getNano() / 1000;
    }

    static String ms(Histogram h, double percentile) {
        if (h.getTotalCount() == 0) return "";
        long us = percentile >= 100 ? h.getMaxValue() : h.getValueAtPercentile(percentile);
        return String.format("%.2f", us / 1000.0);
    }

    // `--prop k=v` (repeatable, comma-joined by options()) passes client settings through.
    static void extraProps(Map<String, String> o, Properties p) {
        if (!o.containsKey("prop")) return;
        for (String kv : o.get("prop").split(",")) {
            int eq = kv.indexOf('=');
            if (eq <= 0) usage("--prop takes key=value, got " + kv);
            p.put(kv.substring(0, eq), kv.substring(eq + 1));
        }
    }

    static Map<String, String> options(String[] args) {
        Map<String, String> o = new HashMap<>();
        for (int i = 1; i < args.length; i += 2) {
            if (!args[i].startsWith("--") || i + 1 >= args.length) usage("bad option " + args[i]);
            o.merge(args[i].substring(2), args[i + 1], (a, b) -> a + "," + b);
        }
        return o;
    }

    static String need(Map<String, String> o, String k) {
        String v = o.get(k);
        if (v == null) usage("missing --" + k);
        return v;
    }

    static void usage(String why) {
        System.err.println("Soak: " + why + " (see the comment at the top of Soak.java)");
        System.exit(2);
    }
}
