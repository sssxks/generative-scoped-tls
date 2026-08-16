//! Synchronous scoped thread-local storage with ergonomic bare references.
//!
//! Native TLS stores only a raw pointer to a value borrowed from an outer stack
//! frame. [`scoped!`] turns that pointer into an ordinary `&T` whose lifetime is
//! bounded by a temporary proxy created at the lookup site.
//!
//! ```rust
//! use generative_scoped_tls::{scoped, scoped_thread_local};
//!
//! struct Context { answer: u32 }
//! scoped_thread_local!(static CX: Context);
//!
//! fn deep() {
//!     let cx = scoped!(CX);
//!     assert_eq!(cx.answer, 42);
//! }
//!
//! let cx = Context { answer: 42 };
//! let body = || deep();
//! // SAFETY: this call tree is synchronous: no reference obtained through
//! // `scoped!` can survive after this `set` invocation returns.
//! unsafe { CX.set(&cx, body) };
//! ```
//!
//! # Why is `set` unsafe?
//!
//! [`scoped!`] gives the reconstructed reference a *lexical* upper bound, while
//! the raw TLS pointer is valid for a *dynamic* scope. In an ordinary synchronous
//! Rust call stack, a lookup performed during `set` finishes before `set` returns.
//! Suspension breaks that implication: a future/coroutine can retain the proxy
//! and reference across a suspension point, return control to `set`, and resume
//! later.
//!
//! Therefore installing a borrowed value is unsafe. The caller must guarantee
//! that no reference produced by [`scoped!`] from that binding remains usable
//! after the dynamic extent of the corresponding [`ScopedKey::set`] ends.

use std::cell::Cell;
use std::fmt;
use std::marker::PhantomData;
use std::ops::Deref;
use std::thread::LocalKey;

/// A scoped thread-local key.
///
/// The key owns no `T`. Each thread has one native TLS slot containing a nullable
/// raw pointer. [`ScopedKey::set`] temporarily points that slot at a caller-owned
/// `T` and restores the previous pointer on return or unwind.
///
/// `T` is deliberately invariant: the erased raw pointer is later interpreted as
/// exactly the same `T` through this key.
pub struct ScopedKey<T> {
    inner: &'static LocalKey<Cell<*const ()>>,
    _invariant: PhantomData<fn(T) -> T>,
}

// SAFETY: `ScopedKey` contains no shared instance of `T`; `inner` addresses a
// distinct `Cell<*const ()>` for each OS thread. All mutation is thread-local.
unsafe impl<T> Sync for ScopedKey<T> {}

/// A temporary lifetime anchor used by [`scoped!`].
///
/// Its only purpose is to make the returned `&T` an ordinary borrow of a local
/// temporary. The `let x = &*temporary` temporary-lifetime-extension rule then
/// keeps this proxy alive for the enclosing block when used as a `let`
/// initializer.
#[doc(hidden)]
pub struct ScopedRef<T> {
    ptr: *const T,
}

impl<T> Deref for ScopedRef<T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        // SAFETY: creating a `ScopedRef` is unsafe and requires the pointer to
        // remain valid for every reference derived through this proxy.
        unsafe { &*self.ptr }
    }
}

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
    /// Every reference produced by [`scoped!`] while this binding is active on
    /// the current thread must become unusable before this invocation of `set`
    /// returns (or unwinds past the caller-owned `value`).
    ///
    /// In particular, callers must not poll/suspend a future, coroutine, or
    /// generator that retains such a reference and then allow this `set` call to
    /// return before the suspended computation is destroyed or resumed to the
    /// point where the reference is dead.
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
    #[inline]
    pub fn with<R>(&'static self, f: impl FnOnce(&T) -> R) -> R {
        let ptr = self.current_ptr();
        // SAFETY: `ptr` is non-null and the `set` contract guarantees it points
        // at a live `T` for this synchronous access.
        unsafe { f(&*ptr) }
    }

    #[inline]
    fn current_ptr(&'static self) -> *const T {
        let ptr = self.inner.with(Cell::get);
        assert!(
            !ptr.is_null(),
            "cannot access a scoped TLS key without calling `set` first"
        );
        ptr.cast::<T>()
    }

    /// Creates the temporary proxy used by [`scoped!`].
    ///
    /// # Safety
    ///
    /// The currently installed pointer must remain valid for every reference
    /// derived from the returned proxy. Safe code should use [`scoped!`], whose
    /// proxy lifetime is bounded by an ordinary local temporary; the remaining
    /// dynamic-scope obligation is carried by the unsafe [`ScopedKey::set`]
    /// contract that installed the pointer.
    #[doc(hidden)]
    #[inline]
    pub unsafe fn __scoped_ref(&'static self) -> ScopedRef<T> {
        ScopedRef {
            ptr: self.current_ptr(),
        }
    }
}

impl<T> fmt::Debug for ScopedKey<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ScopedKey").finish_non_exhaustive()
    }
}

/// Declares a scoped thread-local key.
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

/// Returns a plain `&T` from the currently installed value of a scoped TLS key.
///
/// The macro expands to a borrow through a temporary [`ScopedRef`]. In a `let`
/// initializer such as `let x = scoped!(KEY);`, Rust's ordinary temporary
/// lifetime extension keeps that proxy alive for the enclosing block, which in
/// turn bounds the lifetime of `x`.
///
/// The caller-provided key expression is evaluated outside the generated unsafe
/// block.
///
/// ```
/// use generative_scoped_tls::{scoped, scoped_thread_local};
/// scoped_thread_local!(static N: u32);
///
/// let n = 7;
/// let body = || {
///     let x = scoped!(N);
///     let _: &u32 = x;
///     assert_eq!(*x, 7);
/// };
/// // SAFETY: `body` is synchronous and its scoped reference cannot escape.
/// unsafe { N.set(&n, body) };
/// ```
///
/// The local proxy prevents inflation to `'static`:
///
/// ```compile_fail
/// use generative_scoped_tls::{scoped, scoped_thread_local};
/// scoped_thread_local!(static N: u32);
///
/// fn escape() -> &'static u32 {
///     scoped!(N)
/// }
/// ```
#[macro_export]
macro_rules! scoped {
    ($key:expr $(,)?) => {{
        // `match` evaluates caller-provided syntax outside the unsafe block and
        // still composes with temporary lifetime extension of the arm result.
        match &$key {
            __generative_scoped_tls_key => unsafe {
                // SAFETY: the proxy is a fresh local temporary at this lookup
                // site. The dynamic-scope obligation is the contract of the
                // unsafe `ScopedKey::set` that installed the pointer.
                &*__generative_scoped_tls_key.__scoped_ref()
            },
        }
    }};
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
        let body = || {
            let x = scoped!(NUMBER);
            let _: &u32 = x;
            assert_eq!(*x, 42);
        };

        // SAFETY: `body` is synchronous and its scoped reference cannot escape.
        unsafe { NUMBER.set(&n, body) };
        assert!(!NUMBER.is_set());
    }

    #[test]
    fn expression_use() {
        let n = 17;
        let body = || assert_eq!(*scoped!(NUMBER), 17);
        // SAFETY: `body` is synchronous.
        unsafe { NUMBER.set(&n, body) };
    }

    #[test]
    fn repeated_gets_are_independent_borrows() {
        let n = 9;
        let body = || {
            let a = scoped!(NUMBER);
            let b = scoped!(NUMBER);
            assert_eq!((*a, *b), (9, 9));
        };

        // SAFETY: `body` is synchronous and its scoped references cannot escape.
        unsafe { NUMBER.set(&n, body) };
    }

    #[test]
    fn nesting_restores_previous_binding() {
        let outer = 10;
        let inner = 20;
        let outer_body = || {
            let before = scoped!(NUMBER);
            assert_eq!(*before, 10);

            let inner_body = || {
                let during = scoped!(NUMBER);
                assert_eq!(*during, 20);
            };
            // SAFETY: `inner_body` is synchronous and `during` cannot escape.
            unsafe { NUMBER.set(&inner, inner_body) };

            let after = scoped!(NUMBER);
            assert_eq!(*after, 10);
            assert_eq!(*before, 10);
        };

        // SAFETY: `outer_body` is synchronous and its scoped references cannot escape.
        unsafe { NUMBER.set(&outer, outer_body) };
    }

    #[test]
    fn panic_unwind_restores_previous_binding() {
        let n = 3;
        let body = || {
            assert!(NUMBER.is_set());
            panic!("boom");
        };
        let result = catch_unwind(AssertUnwindSafe(|| unsafe {
            // SAFETY: `body` is synchronous; panic unwinding drops the binding.
            NUMBER.set(&n, body);
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
                let body = || {
                    START.wait();
                    let x = scoped!(NUMBER);
                    assert_eq!(*x, 11);
                };
                // SAFETY: `body` is synchronous and `x` cannot escape.
                unsafe { NUMBER.set(&n, body) };
            });

            let n = 22;
            let body = || {
                START.wait();
                let x = scoped!(NUMBER);
                seen.set(*x);
            };
            // SAFETY: `body` is synchronous and `x` cannot escape.
            unsafe { NUMBER.set(&n, body) };
        });

        assert_eq!(seen.get(), 22);
    }
}
