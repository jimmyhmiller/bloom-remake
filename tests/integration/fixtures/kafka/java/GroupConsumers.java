// A Java program with Kafka's own client (kafka-clients 4.0) against the Blossom brokers, run as a single-file
// source program: `java -cp <kafka libs>/* GroupConsumers.java <bootstrap>`.
//
// A KafkaProducer (idempotent, the default) writes records to a topic of three partitions replicated three times;
// two KafkaConsumers in one group (default settings: the classic protocol, range assignment) read them together,
// the second joining while the first reads, committing synchronously; every record is read, and none twice once
// both are in the group (records read before the second joined may be read again after the rebalance, as Kafka
// allows: at-least-once). A third consumer in the group then reads only records produced after the commits.
import java.time.Duration;
import java.util.*;
import java.util.concurrent.*;
import org.apache.kafka.clients.admin.*;
import org.apache.kafka.clients.consumer.*;
import org.apache.kafka.clients.producer.*;
import org.apache.kafka.common.errors.RebalanceInProgressException;
import org.apache.kafka.common.serialization.*;

public class GroupConsumers {
    static final String TOPIC = "java-group-topic";
    static final String GROUP = "java-group";

    public static void main(String[] args) throws Exception {
        String bootstrap = args[0];
        Properties ap = new Properties();
        ap.put(AdminClientConfig.BOOTSTRAP_SERVERS_CONFIG, bootstrap);
        try (Admin admin = Admin.create(ap)) {
            admin.createTopics(List.of(new NewTopic(TOPIC, 3, (short) 3))).all().get(60, TimeUnit.SECONDS);
        }
        produce(bootstrap, 0, 300);

        Set<String> seen = ConcurrentHashMap.newKeySet();
        CountDownLatch firstReading = new CountDownLatch(50);
        AtomicLatch stop = new AtomicLatch();
        ExecutorService pool = Executors.newFixedThreadPool(2);
        Future<?> a = pool.submit(() -> consume(bootstrap, "first", seen, firstReading, stop));
        if (!firstReading.await(60, TimeUnit.SECONDS)) fail("the first consumer read nothing");
        Future<?> b = pool.submit(() -> consume(bootstrap, "second", seen, new CountDownLatch(0), stop));
        long deadline = System.currentTimeMillis() + 90_000;
        while (seen.size() < 300) {
            if (System.currentTimeMillis() > deadline) fail("the group read " + seen.size() + " of 300");
            Thread.sleep(200);
        }
        stop.set();
        a.get(60, TimeUnit.SECONDS);
        b.get(60, TimeUnit.SECONDS);
        pool.shutdown();

        produce(bootstrap, 300, 20);
        Set<String> later = ConcurrentHashMap.newKeySet();
        AtomicLatch stop2 = new AtomicLatch();
        Thread third = new Thread(() -> consume(bootstrap, "third", later, new CountDownLatch(0), stop2));
        third.start();
        deadline = System.currentTimeMillis() + 60_000;
        while (later.size() < 20) {
            if (System.currentTimeMillis() > deadline) fail("the third consumer read " + later.size() + " of 20");
            Thread.sleep(200);
        }
        stop2.set();
        third.join(60_000);
        for (String v : later) {
            if (Integer.parseInt(v.substring("record ".length())) < 300) fail("the third consumer read " + v + ", which the group had committed past");
        }
        System.out.println("ok java group " + seen.size() + " then " + later.size() + " records");
    }

    static void produce(String bootstrap, int from, int n) throws Exception {
        Properties pp = new Properties();
        pp.put(ProducerConfig.BOOTSTRAP_SERVERS_CONFIG, bootstrap);
        pp.put(ProducerConfig.KEY_SERIALIZER_CLASS_CONFIG, StringSerializer.class.getName());
        pp.put(ProducerConfig.VALUE_SERIALIZER_CLASS_CONFIG, StringSerializer.class.getName());
        try (KafkaProducer<String, String> p = new KafkaProducer<>(pp)) {
            List<Future<RecordMetadata>> sent = new ArrayList<>();
            for (int i = from; i < from + n; i++) {
                sent.add(p.send(new ProducerRecord<>(TOPIC, "key " + i, "record " + i)));
            }
            for (Future<RecordMetadata> f : sent) f.get(60, TimeUnit.SECONDS);
        }
    }

    static void consume(String bootstrap, String name, Set<String> seen, CountDownLatch reading, AtomicLatch stop) {
        Properties cp = new Properties();
        cp.put(ConsumerConfig.BOOTSTRAP_SERVERS_CONFIG, bootstrap);
        cp.put(ConsumerConfig.GROUP_ID_CONFIG, GROUP);
        cp.put(ConsumerConfig.CLIENT_ID_CONFIG, name);
        cp.put(ConsumerConfig.AUTO_OFFSET_RESET_CONFIG, "earliest");
        cp.put(ConsumerConfig.ENABLE_AUTO_COMMIT_CONFIG, "false");
        cp.put(ConsumerConfig.KEY_DESERIALIZER_CLASS_CONFIG, StringDeserializer.class.getName());
        cp.put(ConsumerConfig.VALUE_DESERIALIZER_CLASS_CONFIG, StringDeserializer.class.getName());
        try (KafkaConsumer<String, String> c = new KafkaConsumer<>(cp)) {
            c.subscribe(List.of(TOPIC));
            while (!stop.isSet()) {
                ConsumerRecords<String, String> rs = c.poll(Duration.ofMillis(300));
                for (ConsumerRecord<String, String> r : rs) {
                    seen.add(r.value());
                    reading.countDown();
                }
                if (!rs.isEmpty()) {
                    try {
                        c.commitSync();
                    } catch (RebalanceInProgressException | CommitFailedException e) {
                        // The group is rebalancing: the records are read again by whoever owns them next.
                    }
                }
            }
        }
    }

    static void fail(String why) {
        System.out.println("error: " + why);
        System.exit(1);
    }

    static final class AtomicLatch {
        private volatile boolean set;
        void set() { set = true; }
        boolean isSet() { return set; }
    }
}
