//! Synchronous scoped thread-local storage with ergonomic bare references.
//!
//! This crate is an experimental combination of two ideas:
//!
//! * the physical representation of `scoped-tls`: native TLS stores only a raw
//!   pointer to a value borrowed from an outer stack frame;
//! * `generativity`: every [`scoped!`] creates a fresh lexical lifetime brand,
//!   allowing the raw TLS pointer to be reborrowed as an ordinary `&T` whose
//!   lifetime is bounded by the surrounding lexical scope.
//!
//! The resulting use-site is deliberately not CPS:
//!
//! ```
//! use generative_scoped_tls::{scoped, scoped_thread_local};
//!
//! struct Context { answer: u32 }
//! scoped_thread_local!(static CX: Context);
//!
//! fn deep() {
//!     scoped!(let cx = CX);
//!     assert_eq!(cx.answer, 42);
//! }
//!
//! let cx = Context { answer: 42 };
//! // SAFETY: this call tree is synchronous: no reference obtained through
//! // `scoped!` can survive after this `set` invocation returns.
//! unsafe {
//!     CX.set(&cx, || deep());
//! }
//! ```
//!
//! # Why is `set` unsafe?
//!
//! `generativity` gives a *lexical* lifetime, while this crate's raw pointer is
//! valid for a *dynamic* scope. In ordinary synchronous Rust call stacks, a
//! lexical scope entered during `set` necessarily ends before `set` returns.
//! Suspension breaks that implication: an `async` future/generator can retain a
//! reference across a suspension point, return control to `set`, and resume later.
//!
//! Therefore installing a borrowed value is an unsafe operation. The caller must
//! guarantee that no reference produced by [`scoped!`] from that installed
//! value survives beyond the dynamic extent of the corresponding [`ScopedKey::set`].
//! Normal return and panic unwinding are fine; suspension/yield that lets `set`
//! return while such a reference remains live is not.
//!
//! This design intentionally puts the one `unsafe` at the outer scope boundary,
//! leaving deep call sites ergonomic and safe.

#![forbid(unsafe_op_in_unsafe_fn)]

use generativity::Guard;
use std::cell::Cell;
use std::fmt;
use std::marker::PhantomData;
use std::thread::LocalKey;

/// A scoped thread-local key.
///
/// The key itself owns no `T`. Each thread has one native TLS slot containing a
/// nullable raw pointer. [`ScopedKey::set`] temporarily points that slot at a
/// caller-owned `T` and restores the previous pointer on return or unwind.
///
/// `T` is invariant here because a stored raw pointer is later reinterpreted as
/// exactly the same `T`.
pub struct ScopedKey<T> {
    inner: &'static LocalKey<Cell<*const ()>>,
    _invariant: PhantomData<fn(T) -> T>,
}

// SAFETY: `ScopedKey` contains no shared instance of `T`; `inner` addresses a
// distinct `Cell<*const ()>` for each OS thread. All mutation is thread-local.
unsafe impl<T> Sync for ScopedKey<T> {}

impl<T> ScopedKey<T> {
    /// Internal constructor used by [`scoped_thread_local!`].
    #[doc(hidden)]
    pub const fn __new(inner: &'static LocalKey<Cell<*const ()>>) -> Self {
        Self {
            inner,
            _invariant: PhantomData,
        }
    }

    /// Installs `value` in this thread's slot while `f` executes.
    ///
    /// Nested calls are supported. On normal return or panic unwinding, the
    /// previously installed pointer is restored before this function returns.
    ///
    /// # Safety
    ///
    /// Every reference produced by [`scoped!`] while this binding is the
    /// active binding for the current thread must become unusable before this
    /// invocation of `set` returns (or unwinds past the caller-owned `value`).
    ///
    /// In particular, callers must not poll/suspend a future, coroutine, or
    /// generator that retains such a reference and then allow this `set` call to
    /// return before the suspended computation is destroyed or resumed to the
    /// point where the reference is dead.
    ///
    /// Ordinary synchronous calls and panic unwinding satisfy the intended
    /// discipline naturally.
    pub unsafe fn set<R>(&'static self, value: &T, f: impl FnOnce() -> R) -> R {
        struct Reset {
            key: &'static LocalKey<Cell<*const ()>>,
            previous: *const (),
        }

        impl Drop for Reset {
            fn drop(&mut self) {
                self.key.with(|slot| slot.set(self.previous));
            }
        }

        let previous = self.inner.with(|slot| {
            let previous = slot.get();
            slot.set(value as *const T as *const ());
            previous
        });

        let _reset = Reset {
            key: self.inner,
            previous,
        };

        f()
    }

    /// Returns whether this key currently has an installed value on this thread.
    #[inline]
    pub fn is_set(&'static self) -> bool {
        self.inner.with(|slot| !slot.get().is_null())
    }

    /// Closure-based access with a short borrow.
    ///
    /// This is useful when CPS is acceptable, and also provides a direct baseline
    /// against which [`scoped!`] can be benchmarked.
    ///
    /// # Panics
    ///
    /// Panics if no value is currently installed for this key on this thread.
    #[inline]
    pub fn with<R>(&'static self, f: impl FnOnce(&T) -> R) -> R {
        let ptr = self.current_ptr();
        // SAFETY: `ptr` is non-null and, by the `set` safety contract, points at
        // a live `T` for the current synchronous dynamic extent. The reference
        // cannot escape the closure call through this API's lifetime.
        unsafe { f(&*ptr) }
    }

    #[inline]
    fn current_ptr(&'static self) -> *const T {
        let ptr = self.inner.with(Cell::get);
        assert!(
            !ptr.is_null(),
            "cannot access a generative scoped TLS key without calling `set` first"
        );
        ptr.cast::<T>()
    }

    /// Reborrows the currently installed raw pointer for a fresh generative
    /// lifetime. This is the unsafe core used by [`scoped!`].
    ///
    /// # Safety
    ///
    /// * `brand` must be a fresh trusted brand created *at this lookup site*;
    /// * the currently installed pointer must obey [`ScopedKey::set`]'s dynamic
    ///   scope contract for the entire branded lexical lifetime.
    ///
    /// Safe user code should call [`scoped!`] rather than this function.
    #[doc(hidden)]
    #[inline]
    pub unsafe fn __get_branded<'id>(&'static self, _brand: Guard<'id>) -> &'id T
    where
        T: 'id,
    {
        let ptr = self.current_ptr();
        // SAFETY: required by this function's contract. The generative brand
        // prevents ordinary lexical escape/extension (including to `'static`).
        unsafe { &*ptr }
    }
}

impl<T> fmt::Debug for ScopedKey<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ScopedKey").finish_non_exhaustive()
    }
}

/// Declares a scoped thread-local key.
///
/// The native TLS storage is just `Cell<*const ()>`: one pointer-sized slot with
/// no destructor. The actual `T` remains owned by the caller of [`ScopedKey::set`].
///
/// ```
/// use generative_scoped_tls::scoped_thread_local;
/// scoped_thread_local!(static CX: u32);
/// assert!(!CX.is_set());
/// ```
#[macro_export]
macro_rules! scoped_thread_local {
    () => {};

    (
        $(#[$attr:meta])*
        $vis:vis static $name:ident : $ty:ty;
        $($rest:tt)*
    ) => {
        $(#[$attr])*
        $vis static $name: $crate::ScopedKey<$ty> = $crate::ScopedKey::__new({
            ::std::thread_local! {
                static __GENERATIVE_SCOPED_TLS_SLOT: ::std::cell::Cell<*const ()> = const {
                    ::std::cell::Cell::new(::std::ptr::null())
                };
            }
            &__GENERATIVE_SCOPED_TLS_SLOT
        });

        $crate::scoped_thread_local! { $($rest)* }
    };

    ($(#[$attr:meta])* $vis:vis static $name:ident : $ty:ty) => {
        $crate::scoped_thread_local! {
            $(#[$attr])*
            $vis static $name: $ty;
        }
    };
}

/// Binds a plain `&T` from a scoped TLS key for the remainder of the current
/// lexical scope (subject to normal non-lexical lifetime shortening).
///
/// This must be a statement-style macro rather than an expression macro: the
/// hidden generativity lifetime carrier has to live in the caller's surrounding
/// scope for as long as the returned reference may be used.
///
/// ```
/// use generative_scoped_tls::{scoped, scoped_thread_local};
/// scoped_thread_local!(static N: u32);
///
/// let n = 7;
/// unsafe {
///     N.set(&n, || {
///         scoped!(let x = N);
///         let _: &u32 = x;
///         assert_eq!(*x, 7);
///     });
/// }
/// ```
///
/// A generative borrow cannot be inflated to `'static`:
///
/// ```compile_fail
/// use generative_scoped_tls::{scoped, scoped_thread_local};
/// scoped_thread_local!(static N: u32);
///
/// fn escape() -> &'static u32 {
///     scoped!(let x = N);
///     x
/// }
/// ```
#[macro_export]
macro_rules! scoped {
    (let $name:ident = $key:expr $(;)?) => {
        $crate::__private::make_guard!(__generative_scoped_tls_guard);
        let $name = unsafe {
            // SAFETY: the guard is fresh at this exact lookup site. The remaining
            // dynamic-scope obligation was explicitly assumed by the unsafe
            // `ScopedKey::set` that installed the pointer.
            ($key).__get_branded(__generative_scoped_tls_guard)
        };
    };
}

#[doc(hidden)]
pub mod __private {
    pub use generativity::make_guard;
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use std::sync::Barrier;
    use std::thread;

    scoped_thread_local!(static NUMBER: u32);

    #[test]
    fn initially_unset() {
        assert!(!NUMBER.is_set());
    }

    #[test]
    fn plain_reference() {
        let n = 42;
        unsafe {
            NUMBER.set(&n, || {
                scoped!(let x = NUMBER);
                let _: &u32 = x;
                assert_eq!(*x, 42);
            });
        }
        assert!(!NUMBER.is_set());
    }

    #[test]
    fn repeated_gets_are_independent_brands() {
        let n = 9;
        unsafe {
            NUMBER.set(&n, || {
                scoped!(let a = NUMBER);
                scoped!(let b = NUMBER);
                assert_eq!((*a, *b), (9, 9));
            });
        }
    }

    #[test]
    fn nesting_restores_previous_binding() {
        let outer = 10;
        let inner = 20;
        unsafe {
            NUMBER.set(&outer, || {
                scoped!(let before = NUMBER);
                assert_eq!(*before, 10);

                NUMBER.set(&inner, || {
                    scoped!(let during = NUMBER);
                    assert_eq!(*during, 20);
                });

                scoped!(let after = NUMBER);
                assert_eq!(*after, 10);
                assert_eq!(*before, 10);
            });
        }
    }

    #[test]
    fn panic_unwind_restores_previous_binding() {
        let n = 3;
        let result = catch_unwind(AssertUnwindSafe(|| unsafe {
            NUMBER.set(&n, || {
                assert!(NUMBER.is_set());
                panic!("boom");
            });
        }));
        assert!(result.is_err());
        assert!(!NUMBER.is_set());
    }

    #[test]
    fn keys_are_thread_local() {
        static START: Barrier = Barrier::new(2);
        let seen = Cell::new(0u32);

        thread::scope(|scope| {
            scope.spawn(|| {
                let n = 11;
                unsafe {
                    NUMBER.set(&n, || {
                        START.wait();
                        scoped!(let x = NUMBER);
                        assert_eq!(*x, 11);
                    });
                }
            });

            let n = 22;
            unsafe {
                NUMBER.set(&n, || {
                    START.wait();
                    scoped!(let x = NUMBER);
                    seen.set(*x);
                });
            }
        });

        assert_eq!(seen.get(), 22);
    }
}
