# Safety argument

This note describes the intended invariant of `generative-scoped-tls`.

## Representation

For each declared key and each OS thread, native TLS stores exactly one `Cell<*const ()>`.

During `ScopedKey::set(&value, f)`, that pointer is set to `value as *const T`. An RAII guard restores the previous pointer on both normal return and panic unwinding.

The key does not own `T`.

## Unsafe transition 1: installing a borrowed pointer

`set` erases the lifetime of `&T` when converting it to a raw pointer. This is why `set` is unsafe.

Its caller promises that every plain reference reconstructed from that binding becomes unusable before the binding's dynamic extent ends.

## Unsafe transition 2: reconstructing `&T`

`scoped_get!` expands to `generativity::make_guard!` at the lookup site and passes that fresh `Guard<'id>` to the hidden `__get_branded` function. `__get_branded` casts the current raw pointer to `&'id T`.

The generativity brand prevents ordinary Rust lifetime extension/escape from the surrounding lexical region, including coercion to `'static`.

## Why synchronous call stacks satisfy the dynamic invariant

If lookup occurs after entering `set`, an ordinary synchronous callee must finish before its caller can continue and eventually return from `set`. Thus a lexical borrow created in that callee cannot still be executing after `set` returns.

Panic unwinding has the same stack discipline: inner frames are unwound before the outer `set` frame restores the pointer and unwinds further.

## Known excluded control flow: suspension

An async future or coroutine may preserve lexical locals in its suspended state while returning control to its caller. Therefore lexical lifetime containment does not imply dynamic-scope containment across suspension.

Example shape (do not do this):

```rust,ignore
async fn worker() {
    scoped_get!(let cx = CX);
    pending().await;
    use_it(cx);
}

unsafe {
    CX.set(&stack_context, || poll_once(&mut worker_future));
} // may return while `cx` remains stored in the suspended future
```

The `unsafe` on `set` makes this a caller contract violation rather than unsoundness reachable from entirely safe code.

## Other boundaries

- Non-scoped `thread::spawn` requires captured borrows to be `'static`, which the fresh generative lifetime is not.
- Scoped threads remain bounded by their `thread::scope` lifetime; ordinary lexical escape checks still apply.
- Process abort terminates the process, so no Rust destructor guarantee is attempted.
- FFI longjmp / forced thread cancellation / other mechanisms that bypass Rust stack discipline are outside the safe Rust execution model and must uphold the same `set` contract manually.
