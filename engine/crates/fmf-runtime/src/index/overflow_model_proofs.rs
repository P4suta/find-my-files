use super::{MAX_ENTRIES, OverflowMap};

fn initial_map() -> (OverflowMap, [Option<(u32, u64)>; MAX_ENTRIES], usize) {
    let entries: [Option<(u32, u64)>; MAX_ENTRIES] = kani::any();
    let mut count = 0;
    for index in 0..MAX_ENTRIES {
        if let Some((key, _)) = entries[index] {
            count += 1;
            for previous in &entries[..index] {
                if let Some((other, _)) = previous {
                    kani::assume(key != *other);
                }
            }
        }
    }
    (OverflowMap { entries }, entries, count)
}

fn reference(entries: &[Option<(u32, u64)>], key: u32) -> Option<u64> {
    let mut found = None;
    for slot in entries {
        if let Some((entry, value)) = slot {
            if *entry == key {
                found = Some(*value);
            }
        }
    }
    found
}

#[kani::proof]
#[kani::unwind(9)]
fn lookup_matches_every_unique_bounded_entry_set() {
    let (map, entries, count) = initial_map();
    let query: u32 = kani::any();
    let expected = reference(&entries, query);
    assert_eq!(map.get(&query).copied(), expected);
    assert_eq!(map.len(), count);
    kani::cover!(expected.is_some() && count == MAX_ENTRIES);
    kani::cover!(expected.is_none() && count == 0);
}

#[kani::proof]
#[kani::unwind(9)]
fn insertion_updates_one_key_and_preserves_every_other_mapping() {
    let (mut map, entries, count) = initial_map();
    let key: u32 = kani::any();
    let value: u64 = kani::any();
    let query: u32 = kani::any();
    let previous = reference(&entries, key);
    kani::assume(count < MAX_ENTRIES || previous.is_some());
    assert_eq!(map.insert(key, value), previous);
    let expected = if query == key {
        Some(value)
    } else {
        reference(&entries, query)
    };
    assert_eq!(map.get(&query).copied(), expected);
    assert_eq!(map.len(), count + usize::from(previous.is_none()));
    kani::cover!(previous.is_some() && value == u64::MAX);
    kani::cover!(previous.is_none() && count == MAX_ENTRIES - 1);
}

#[kani::proof]
#[kani::unwind(9)]
fn removal_deletes_only_the_selected_mapping() {
    let (mut map, entries, count) = initial_map();
    let key: u32 = kani::any();
    let query: u32 = kani::any();
    let previous = reference(&entries, key);
    assert_eq!(map.remove(&key), previous);
    let expected = if query == key {
        None
    } else {
        reference(&entries, query)
    };
    assert_eq!(map.get(&query).copied(), expected);
    assert_eq!(map.len(), count - usize::from(previous.is_some()));
    kani::cover!(previous.is_some());
    kani::cover!(previous.is_none() && count == 0);
}
