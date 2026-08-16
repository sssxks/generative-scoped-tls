# Safety argument

This note describes the intended invariant of `generative-scoped-tls`.

## Representation

For each declared key and each OS thread, native TLS stores exactly one
`Cell<*const ()>`.

During `ScopedKey::set(&value, f)`, that pointer is set to `value as *const T`.
An RAII guard restores the previous pointer on normal return and panic unwinding.
The key does not own `T`.

`ScopedKey<T>` is invariant in `T`: the erased pointer written through a key is
later reconstructed as exactly that same `T`.

## Unsafe transition 1: installing a borrowed pointer

`set` erases the lifetime of `&T` when converting it to a raw pointer. This is
why `set` is unsafe.

Its caller promises that every reference reconstructed from that binding becomes
unusable before the binding's dynamic extent ends.

## Unsafe transition 2: creating a lexical proxy

`scoped!(KEY)` reads the current raw pointer and constructs a temporary
`ScopedRef<T>`. `ScopedRef<T>` implements `Deref<Target = T>`; dereferencing it
reconstructs `&T` from the stored pointer.

Conceptually:

```rust,ignore
let x = &*ScopedRef { ptr };
```

The lifetime of the resulting `&T` is an ordinary borrow of the `ScopedRef`
proxy. When this expression initializes a local, Rust's temporary lifetime
extension keeps the proxy alive for the surrounding block, so borrow checking
gives the reference a lexical upper bound:

```text
lifetime(&T) ⊆ lifetime(ScopedRef temporary) ⊆ lexical caller scope
```

This construction needs no fresh invariant lifetime brand. Shrinking the borrow
of the proxy only shortens the reconstructed `&T`, which is harmless; the safety
argument relies on lifetime containment, not lifetime identity.

## Why synchronous call stacks satisfy the dynamic invariant

If lookup occurs after entering `set`, an ordinary synchronous callee must finish
before its caller can continue and eventually return from `set`. Thus the local
proxy and every borrow derived from it are gone before the dynamic binding ends.

Panic unwinding has the same stack discipline: inner frames are unwound before
the outer `set` frame restores the pointer and unwinds further.

Combining the lexical argument with the unsafe caller contract gives:

```text
lifetime(&T)
    ⊆ lifetime(ScopedRef temporary)
    ⊆ lexical caller scope
    ⊆ dynamic scope(ScopedKey::set)
    ⊆ lifetime(installed T)
```

## Known excluded control flow: suspension

An async future or coroutine may preserve lexical locals in its suspended state
while returning control to its caller. In this implementation the future may
retain both the `ScopedRef` temporary and the `&T` derived from it. Therefore
lexical lifetime containment still does not imply dynamic-scope containment
across suspension.

Example shape (do not do this):

```rust,ignore
async fn worker() {
    let cx = scoped!(CX);
    pending().await;
    use_it(cx);
}

let poll_worker_once = || poll_once(&mut worker_future);
// Contract violation: this may return while `cx` remains stored in the
// suspended future.
unsafe { CX.set(&stack_context, poll_worker_once) };
```

The `unsafe` on `set` makes this a caller contract violation rather than
unsoundness reachable from entirely safe code.

An executable version of this misuse lives in
[`miri-tests/async-misuse`](miri-tests/async-misuse). It is an intentionally
failing Miri fixture: the future is suspended while retaining the reference, the
installed value is freed, and polling resumes far enough to dereference the
dangling reference.

```text
cargo +nightly miri run --locked --manifest-path miri-tests/async-misuse/Cargo.toml
```

The expected result is a nonzero exit and a Miri use-after-free diagnostic. Do
not run the fixture natively.

## Other boundaries

- Non-scoped `thread::spawn` cannot make the local proxy-derived borrow `'static`.
- Scoped threads remain bounded by their `thread::scope` lifetime.
- Process abort terminates the process, so no Rust destructor guarantee is attempted.
- FFI longjmp, forced thread cancellation, and other mechanisms that bypass Rust
  stack discipline are outside the safe Rust execution model and must uphold the
  same `set` contract manually.
