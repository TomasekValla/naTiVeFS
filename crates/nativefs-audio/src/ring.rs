//! Single-producer single-consumer ring of interleaved samples between the decoder thread and the
//! audio device thread. Lock-free, so the device thread never waits on a lock (no dropouts from a
//! decoder holding a mutex).

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

struct Shared {
    buf: Box<[UnsafeCell<i32>]>,
    mask: usize,
    /// Total samples written / read (wrapping); index = counter & mask.
    write: AtomicUsize,
    read: AtomicUsize,
}

// One writer and one reader touch disjoint parts of `buf`, separated by the atomics.
unsafe impl Sync for Shared {}

pub struct Producer(Arc<Shared>);
pub struct Consumer(Arc<Shared>);

/// A ring holding at least `capacity` samples (rounded up to a power of two).
pub fn ring(capacity: usize) -> (Producer, Consumer) {
    let cap = capacity.next_power_of_two();
    let shared = Arc::new(Shared {
        buf: (0..cap).map(|_| UnsafeCell::new(0)).collect(),
        mask: cap - 1,
        write: AtomicUsize::new(0),
        read: AtomicUsize::new(0),
    });
    (Producer(shared.clone()), Consumer(shared))
}

impl Shared {
    fn len(&self) -> usize {
        self.write.load(Ordering::Acquire).wrapping_sub(self.read.load(Ordering::Acquire))
    }
}

impl Producer {
    pub fn free(&self) -> usize {
        self.0.buf.len() - self.0.len()
    }

    /// Copies as much of `data` as fits; returns how many samples were taken.
    pub fn push(&mut self, data: &[i32]) -> usize {
        let s = &*self.0;
        let w = s.write.load(Ordering::Relaxed);
        let n = data.len().min(s.buf.len() - w.wrapping_sub(s.read.load(Ordering::Acquire)));
        for (i, v) in data[..n].iter().enumerate() {
            unsafe { *s.buf[(w.wrapping_add(i)) & s.mask].get() = *v };
        }
        s.write.store(w.wrapping_add(n), Ordering::Release);
        n
    }
}

impl Consumer {
    pub fn available(&self) -> usize {
        self.0.len()
    }

    /// Fills `out` from the ring; returns how many samples were copied.
    pub fn pop(&mut self, out: &mut [i32]) -> usize {
        let s = &*self.0;
        let r = s.read.load(Ordering::Relaxed);
        let n = out.len().min(s.write.load(Ordering::Acquire).wrapping_sub(r));
        for (i, v) in out[..n].iter_mut().enumerate() {
            *v = unsafe { *s.buf[(r.wrapping_add(i)) & s.mask].get() };
        }
        s.read.store(r.wrapping_add(n), Ordering::Release);
        n
    }

    /// Drops everything queued (after a seek; the producer must be paused meanwhile).
    pub fn clear(&mut self) {
        let s = &*self.0;
        s.read.store(s.write.load(Ordering::Acquire), Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_and_keeps_order() {
        let (mut p, mut c) = ring(8);
        let mut next = 0;
        let mut expect = 0;
        let mut out = [0; 5];
        for _ in 0..100 {
            let data: Vec<i32> = (next..next + 3).collect();
            next += p.push(&data) as i32;
            let n = c.pop(&mut out);
            for v in &out[..n] {
                assert_eq!(*v, expect);
                expect += 1;
            }
        }
        assert!(expect > 250);
    }

    #[test]
    fn threads() {
        let (mut p, mut c) = ring(1024);
        let t = std::thread::spawn(move || {
            let mut i = 0;
            while i < 1_000_000 {
                let data: Vec<i32> = (i..(i + 100).min(1_000_000)).collect();
                i += p.push(&data) as i32;
            }
        });
        let mut expect = 0;
        let mut out = [0; 77];
        while expect < 1_000_000 {
            let n = c.pop(&mut out);
            for v in &out[..n] {
                assert_eq!(*v, expect);
                expect += 1;
            }
        }
        t.join().unwrap();
    }
}
