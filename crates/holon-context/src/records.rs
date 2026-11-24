//! Borrow records held by the caller while projecting a prepared snapshot.

use std::borrow::Borrow;

trait RecordSource<T> {
    fn len(&self) -> usize;
    fn get(&self, index: usize) -> &T;
}

impl<T, R: Borrow<T>> RecordSource<T> for &[R] {
    fn len(&self) -> usize {
        <[R]>::len(self)
    }

    fn get(&self, index: usize) -> &T {
        self[index].borrow()
    }
}

impl<T, R: Borrow<T>> RecordSource<T> for Vec<R> {
    fn len(&self) -> usize {
        self.len()
    }

    fn get(&self, index: usize) -> &T {
        self[index].borrow()
    }
}

/// A read-only view over owned or qualified records in their original order.
pub struct CapturedRecords<'a, T> {
    records: &'a dyn RecordSource<T>,
}

impl<T> Clone for CapturedRecords<'_, T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for CapturedRecords<'_, T> {}

impl<'a, T> CapturedRecords<'a, T> {
    pub fn new<R: Borrow<T>>(records: &'a Vec<R>) -> Self {
        Self { records }
    }

    pub fn from_slice<R: Borrow<T>>(records: &'a &'_ [R]) -> Self {
        Self { records }
    }

    pub fn empty() -> Self {
        Self {
            records: &(&[] as &[T]),
        }
    }

    pub fn is_empty(self) -> bool {
        self.records.len() == 0
    }

    pub fn iter(self) -> impl DoubleEndedIterator<Item = &'a T> + ExactSizeIterator {
        (0..self.records.len()).map(move |index| self.records.get(index))
    }
}
