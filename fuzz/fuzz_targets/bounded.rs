#![no_main]

use libfuzzer_sys::fuzz_target;
use std::collections::VecDeque;
use vulcan::{
    bounded::{BoundedQueue, BoundedVec},
    convert::usize_from_u32,
};

// Drives the bounded collections and unbounded models with the same operations. The first byte
// picks the capacity; every following byte is an operation. A push must succeed exactly when the
// model holds fewer items than the capacity, and must hand the item back and change nothing
// otherwise. Clearing and cloning are driven too, since a clone must keep the full capacity.
fuzz_target!(|data: &[u8]| {
    let Some((&capacity_byte, operations)) = data.split_first() else { return };
    let capacity = u32::from(capacity_byte) + 1;
    let capacity_usize = usize_from_u32(capacity);
    let mut queue = BoundedQueue::with_capacity(capacity);
    let mut stack = BoundedVec::with_capacity(capacity);
    let mut queue_model = VecDeque::with_capacity(capacity_usize);
    let mut stack_model = Vec::with_capacity(capacity_usize);

    for &operation in operations {
        match operation % 8 {
            0 | 1 => {
                let has_room = queue_model.len() < capacity_usize;
                let pushed = queue.push_back(operation);
                assert_eq!(pushed.is_ok(), has_room, "a push succeeds exactly when there is room");
                if has_room {
                    queue_model.push_back(operation);
                } else {
                    assert_eq!(pushed, Err(operation), "a refused item is handed back");
                }
            }
            2 => assert_eq!(queue.pop_front(), queue_model.pop_front()),
            3 | 4 => {
                let has_room = stack_model.len() < capacity_usize;
                let pushed = stack.push(operation);
                assert_eq!(pushed.is_ok(), has_room, "a push succeeds exactly when there is room");
                if has_room {
                    stack_model.push(operation);
                } else {
                    assert_eq!(pushed, Err(operation), "a refused item is handed back");
                }
            }
            5 => assert_eq!(stack.pop(), stack_model.pop()),
            6 => {
                queue = queue.clone();
                stack = stack.clone();
            }
            _ => {
                if operation & 0x80 == 0 {
                    queue.clear();
                    queue_model.clear();
                } else {
                    stack.clear();
                    stack_model.clear();
                }
            }
        }
        assert!(queue.iter().eq(queue_model.iter()), "the queue matches the model");
        assert_eq!(stack.as_slice(), stack_model.as_slice(), "the stack matches the model");
        assert_eq!(queue.capacity(), capacity, "the queue keeps its capacity");
        assert_eq!(stack.capacity(), capacity, "the stack keeps its capacity");
        assert_eq!(queue.is_full(), queue_model.len() == capacity_usize);
        assert_eq!(stack.is_full(), stack_model.len() == capacity_usize);
        assert_eq!(queue.is_empty(), queue_model.is_empty());
        assert_eq!(stack.is_empty(), stack_model.is_empty());
    }
});
