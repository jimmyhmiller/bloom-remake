use super::*;

fn row(n: u64) -> Row {
    Arc::from([Value::u64(n % 7), Value::u64(n), Value::bytes(&[n as u8; 64])])
}

#[test]
fn indexed_old_reads_match_the_tick_snapshot_through_mixed_updates() {
    let mut store = Store::new(None, false);
    let mut counts = BTreeMap::<Row, i64>::new();
    let mut random = 43u64;
    for _ in 0..30 {
        store.clear_delta();
        let snapshot: BTreeSet<Row> = counts.keys().cloned().collect();
        for _ in 0..80 {
            random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
            let r = row((random >> 32) % 35);
            let count = counts.get(&r).copied().unwrap_or(0);
            let w = if count > 0 && random & 4 == 0 { -1 } else { 1 };
            store.add(r.clone(), w).unwrap();
            if count + w == 0 {
                counts.remove(&r);
            } else {
                counts.insert(r, count + w);
            }
            for cols in [vec![], vec![0], vec![1], vec![0, 1], vec![1, 0]] {
                let values = key(&row((random >> 40) % 35), &cols);
                let expected: BTreeSet<Row> = snapshot.iter().filter(|r| holds(r, &cols, &values)).cloned().collect();
                assert_eq!(
                    store
                        .old_rows(&cols, &values)
                        .unwrap()
                        .into_iter()
                        .collect::<BTreeSet<_>>(),
                    expected
                );
                assert_eq!(store.any(true, &cols, &values).unwrap(), !expected.is_empty());
                let range = store
                    .range_rows(
                        true,
                        &cols,
                        &values,
                        1,
                        Bound::Excluded(Value::u64(5)),
                        Bound::Included(Value::u64(25)),
                    )
                    .unwrap();
                let expected: BTreeSet<Row> = expected
                    .into_iter()
                    .filter(|r| r[1] > Value::u64(5) && r[1] <= Value::u64(25))
                    .collect();
                assert_eq!(range.into_iter().collect::<BTreeSet<_>>(), expected);
            }
        }
        store.retract_all().unwrap();
        assert_eq!(
            store.old_rows(&[], &[]).unwrap().into_iter().collect::<BTreeSet<_>>(),
            snapshot
        );
        for n in 0..35 {
            let values = [Value::u64(n)];
            assert_eq!(store.any(true, &[1], &values).unwrap(), snapshot.contains(&row(n)));
        }
        counts.clear();
    }
}

#[test]
fn replacing_delta_and_canceling_deletions_cannot_leave_stale_buckets() {
    let mut store = Store::new(None, false);
    let a = row(1);
    let b = row(8);
    store.add(a.clone(), 1).unwrap();
    store.add(b.clone(), 1).unwrap();
    store.clear_delta();
    store.add(a.clone(), -1).unwrap();
    store.add(b.clone(), -1).unwrap();
    // Column 1 is non-leading, so these reads exercise the separate deletion index.
    assert_eq!(store.old_rows(&[1], &[Value::u64(1)]).unwrap(), vec![a.clone()]);
    assert_eq!(store.old_rows(&[1], &[Value::u64(8)]).unwrap(), vec![b.clone()]);
    store.add(a.clone(), 1).unwrap();
    assert_eq!(store.old_rows(&[1], &[Value::u64(1)]).unwrap(), vec![a.clone()]);
    assert_eq!(
        store.old_rows(&[0], &[Value::u64(1)]).unwrap(),
        vec![a.clone(), b.clone()]
    );
    store.add(a.clone(), -1).unwrap();
    store.set_change(BTreeSet::new(), BTreeSet::from([b.clone()]));
    assert_eq!(store.old_rows(&[1], &[Value::u64(1)]).unwrap(), Vec::<Row>::new());
    assert_eq!(store.old_rows(&[1], &[Value::u64(8)]).unwrap(), vec![b.clone()]);
    assert_eq!(store.old_rows(&[0], &[Value::u64(1)]).unwrap(), vec![b]);
    store.clear_delta();
    assert!(!store.any(true, &[0], &[Value::u64(1)]).unwrap());
}

#[test]
fn leading_prefixes_preserve_compound_value_equality_and_row_order() {
    let make = |n: u64| -> Row {
        let prefix: &[u8] = if n < 16 {
            &[0]
        } else if n < 32 {
            &[0, 0]
        } else {
            &[255]
        };
        Arc::from([
            Value::bytes(prefix),
            Value::u64(n),
            Value::Vec((0..16).map(|i| Value::u64(n + i)).collect()),
        ])
    };
    let rows: Vec<Row> = (0..48).map(make).collect();
    let mut store = Store::new(None, false);
    for row in &rows {
        store.add(row.clone(), 1).unwrap();
    }
    store.clear_delta();
    for row in &rows {
        store.add(row.clone(), -1).unwrap();
    }
    for bytes in [vec![0], vec![0, 0], vec![0, 1], vec![255], vec![255, 0]] {
        let values = [Value::bytes(&bytes)];
        let expected: Vec<Row> = rows.iter().filter(|row| row[0] == values[0]).cloned().collect();
        assert_eq!(store.old_rows(&[0], &values).unwrap(), expected);
        assert_eq!(store.any(true, &[0], &values).unwrap(), !expected.is_empty());
        let expected: Vec<Row> = expected
            .into_iter()
            .filter(|row| row[1] >= Value::u64(8) && row[1] < Value::u64(40))
            .collect();
        assert_eq!(
            store
                .range_rows(
                    true,
                    &[0],
                    &values,
                    1,
                    Bound::Included(Value::u64(8)),
                    Bound::Excluded(Value::u64(40))
                )
                .unwrap(),
            expected
        );
    }
    for n in 0..48 {
        let independent = make(n);
        assert_eq!(
            store.old_rows(&[0, 1, 2], &independent).unwrap(),
            vec![rows[n as usize].clone()]
        );
    }
}
