//! Data-parallel iteration that falls back to plain sequential iterators without the
//! `parallel` feature (for example on `wasm32-unknown-unknown`, which has no threads).
//!
//! Every parallel loop in dith writes disjoint rows or collects in order, so both paths produce
//! identical output.

#[cfg(feature = "parallel")]
pub(crate) use rayon::prelude;

#[cfg(not(feature = "parallel"))]
pub(crate) mod prelude {
    /// Sequential stand-ins for rayon's `par_iter`/`into_par_iter`.
    pub(crate) trait IntoParallelIterator: IntoIterator + Sized {
        fn into_par_iter(self) -> Self::IntoIter {
            self.into_iter()
        }
    }

    impl<I: IntoIterator> IntoParallelIterator for I {}

    /// Sequential stand-in for rayon's `par_iter` on borrowed collections.
    pub(crate) trait IntoParallelRefIterator<'a> {
        type Iter: Iterator;
        fn par_iter(&'a self) -> Self::Iter;
    }

    impl<'a, T: 'a> IntoParallelRefIterator<'a> for [T] {
        type Iter = std::slice::Iter<'a, T>;
        fn par_iter(&'a self) -> Self::Iter {
            self.iter()
        }
    }

    impl<'a, T: 'a> IntoParallelRefIterator<'a> for Vec<T> {
        type Iter = std::slice::Iter<'a, T>;
        fn par_iter(&'a self) -> Self::Iter {
            self.iter()
        }
    }

    /// Sequential stand-ins for rayon's `par_chunks_exact_mut` and `par_chunks_mut`.
    pub(crate) trait ParallelSliceMut<T> {
        fn par_chunks_exact_mut(&mut self, size: usize) -> std::slice::ChunksExactMut<'_, T>;
        fn par_chunks_mut(&mut self, size: usize) -> std::slice::ChunksMut<'_, T>;
    }

    impl<T> ParallelSliceMut<T> for [T] {
        fn par_chunks_exact_mut(&mut self, size: usize) -> std::slice::ChunksExactMut<'_, T> {
            self.chunks_exact_mut(size)
        }

        fn par_chunks_mut(&mut self, size: usize) -> std::slice::ChunksMut<'_, T> {
            self.chunks_mut(size)
        }
    }

    /// Sequential stand-in for rayon's `par_chunks_exact`.
    pub(crate) trait ParallelSlice<T> {
        fn par_chunks_exact(&self, size: usize) -> std::slice::ChunksExact<'_, T>;
    }

    impl<T> ParallelSlice<T> for [T] {
        fn par_chunks_exact(&self, size: usize) -> std::slice::ChunksExact<'_, T> {
            self.chunks_exact(size)
        }
    }

    /// Sequential stand-in for rayon's `with_min_len` split hint, which has no effect here.
    pub(crate) trait IndexedParallelIterator: Iterator + Sized {
        fn with_min_len(self, _min: usize) -> Self {
            self
        }
    }

    impl<I: Iterator> IndexedParallelIterator for I {}
}

/// Runs `work` on a rayon worker thread when the `parallel` feature is on, so nested parallel
/// loops avoid waking the pool from outside; runs it inline otherwise.
#[cfg(feature = "parallel")]
pub(crate) fn on_worker<R: Send>(work: impl FnOnce() -> R + Send) -> R {
    rayon::scope(|_| work())
}

/// Runs `work` inline; without the `parallel` feature there is no worker pool.
#[cfg(not(feature = "parallel"))]
pub(crate) fn on_worker<R>(work: impl FnOnce() -> R) -> R {
    work()
}
