use std::cell::UnsafeCell;
use std::cmp::max;
use std::fmt;
use std::ptr::null;
use std::sync::atomic::Ordering;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

const CAPACITY: usize = 100000;

struct Lock {
    state: AtomicBool,
}

impl Lock {
    fn new(state: bool) -> Self {
        Self {
            state: AtomicBool::new(state),
        }
    }
    #[inline]
    fn set_true(&self) {
        while self
            .state
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            thread::yield_now();
        }
    }

    #[inline]
    fn set_false(&self) {
        while self
            .state
            .compare_exchange(true, false, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            thread::yield_now()
        }
    }

    #[inline]
    fn is_true(&self) -> bool {
        self.state
            .compare_exchange(true, false, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
    }

    #[inline]
    fn wait_for_true(&self) {
        while self
            .state
            .compare_exchange(true, false, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            thread::yield_now();
        }
    }
}

impl fmt::Debug for Lock {
    fn fmt(
        &self,
        formatter: &mut std::fmt::Formatter<'_>,
    ) -> std::result::Result<(), std::fmt::Error> {
        write!(formatter, "{}", self.state.load(Ordering::SeqCst));
        Ok({})
    }
}

struct Stack<T> {
    data: UnsafeCell<Vec<T>>,
    reserved: AtomicUsize,
    safe_to_read: Box<[Lock]>,
    safe_to_write: Box<[Lock]>,
}

impl<T: Copy + Default> Stack<T> {
    fn new() -> Self {
        let mut data = Vec::with_capacity(CAPACITY);
        let mut safe_to_read = Vec::with_capacity(CAPACITY);
        let mut safe_to_write = Vec::with_capacity(CAPACITY);
        for _ in 0..CAPACITY {
            data.push(T::default());
            safe_to_read.push(Lock::new(false));
            safe_to_write.push(Lock::new(true));
        }

        Self {
            data: UnsafeCell::new(data),
            reserved: AtomicUsize::new(0),
            safe_to_read: safe_to_read.into_boxed_slice(),
            safe_to_write: safe_to_write.into_boxed_slice(),
        }
    }

    fn push(&self, value: T) {
        // Reserve a spot.
        let position = self.reserved.fetch_add(1, Ordering::SeqCst);
        // Wait for the pop thread to have read the value.
        // -> Won't overwrite the value before the pop thread has read it.
        // println!("push before - safe_to_write: {:?}", self.safe_to_write);
        // println!("push before - safe_to_read: {:?}", self.safe_to_read);
        self.safe_to_write[position].wait_for_true();
        // Write the value.
        // SAFETY: Position is locked.
        let data = unsafe { &mut *self.data.get() };
        data[position] = value;
        // Signal to the reader thread that the position is ready to be read.
        self.safe_to_read[position].set_true();
        // println!("push after - safe_to_write: {:?}", self.safe_to_write);
        // println!("push after - safe_to_read: {:?}", self.safe_to_read);
    }

    fn pop(&self) -> Option<T> {
        // println!("pop before - safe_to_write: {:?}", self.safe_to_write);
        // println!("pop before - safe_to_read: {:?}", self.safe_to_read);
        loop {
            let current_position = self.reserved.load(Ordering::SeqCst);
            if current_position == 0 {
                return None;
            }
            let read_position = current_position - 1;
            if !self.safe_to_read[read_position].is_true() {
                /*println!(
                    "read position: {}, lock: {}",
                    read_position,
                    self.safe_to_read[read_position]
                        .state
                        .load(Ordering::SeqCst)
                );*/
                continue;
            }
            if self
                .reserved
                .compare_exchange(
                    current_position,
                    read_position,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                )
                .is_ok()
            {
                let value = Some(self.get_and_clean(read_position));
                self.safe_to_write[read_position].set_true();
                // println!("pop after - safe_to_write: {:?}", self.safe_to_write);
                // println!("pop after - safe_to_read: {:?}", self.safe_to_read);
                return value;
            } else {
                self.safe_to_read[read_position].set_false();
            };
        }
    }

    fn get_and_clean(&self, index: usize) -> T {
        let data = unsafe { &mut *self.data.get() };
        let value = data[index];
        // let reserved = self.reserved.load(Ordering::SeqCst);
        // if reserved > 0 && reserved - 1 <= index {
        //    data[index] = T::default();
        // }
        value
    }
}

unsafe impl<T> Sync for Stack<T> where T: Send {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stack() {
        let s = Stack::new();
        s.push(1);
        s.push(2);
        s.push(3);
        assert_eq!(s.pop(), Some(3));
        assert_eq!(s.pop(), Some(2));
        assert_eq!(s.pop(), Some(1));
        assert_eq!(s.pop(), None);
    }

    #[test]
    fn test_stack_threaded() {
        let s: &'static _ = Box::leak(Box::new(Stack::new()));

        let a = s.clone();
        thread::spawn(move || {
            a.push(1);
            a.push(2);
            a.push(3);
        })
        .join()
        .unwrap();
        assert_eq!(s.pop(), Some(3));
        assert_eq!(s.pop(), Some(2));
        assert_eq!(s.pop(), Some(1));
        assert_eq!(s.pop(), None);
    }

    #[test]
    fn test_stack_threaded_many() {
        let s: &'static _ = Box::leak(Box::new(Stack::new()));

        let handles: Vec<_> = (0..3)
            .map(|t| {
                let p = s.clone();
                thread::Builder::new()
                    .name(format!("thread-{i}", i = t))
                    .spawn(move || {
                        let mut r = vec![];
                        for i in 0..10 {
                            p.push(1000);
                        }
                        for i in 0..10 {
                            r.push(p.pop());
                        }
                        r
                    })
                    .unwrap()
            })
            .collect();

        for handle in handles {
            assert_eq!(vec![Some(1000); 10], handle.join().unwrap());
        }
    }
}
